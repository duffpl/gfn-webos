//! Live elementary-stream video output on LG webOS through StarfishMediaAPIs (BUFFERSTREAM).
//!
//! The C seam (`native/starfish.c`, from PlxNative) owns the C++ ABI. This crate drives it in the
//! order PlxNative verified on hardware: exported window (webOS 5+) -> Load -> loadCompleted ->
//! Play -> ACB bind (webOS 4.x) -> feed -> setMediaVideoData + display window once frames decode.
//!
//! Every Starfish/ACB control call for one session must come from the thread that opened it, so
//! [`StarfishVideo`] is `!Send`: create it on the thread that will feed it.

mod payload;
mod sys;

use std::borrow::Cow;
use std::ffi::{CStr, CString};
use std::marker::PhantomData;
use std::os::raw::{c_char, c_int, c_long, c_uint};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::{Duration, Instant};

pub use payload::{LoadPayload, VideoCodec};

const LOAD_BUDGET: Duration = Duration::from_secs(20);
const UNLOAD_BUDGET: Duration = Duration::from_secs(5);
const PLAYER_TYPE_MSE: c_int = 10;
const ES_VIDEO: c_int = 1;
/// libpf callback types (webOS 4 numbering; see PlxNative player/CLAUDE.md).
const CB_FRAME_READY: c_int = 0;
const CB_ERROR: c_int = 18;
const CB_UNLOAD_COMPLETED: c_int = 23;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoPlane {
    None,
    /// webOS 2.2.3 – 4.10: libAcbAPI binds the decoded sink to the plane.
    Acb,
    /// webOS 5+: SDL exported window id travels in the Load payload.
    Exported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedResult {
    Accepted,
    /// The pipeline's source buffer is full; the caller decides whether to retry or drop.
    BufferFull,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    FrameReady { pts: i64 },
    Error { code: i64, message: String },
    /// Everything else; the message is logged where it arrives, so it is not carried.
    Other { kind: c_int, num: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const FULL_HD: Rect = Rect { x: 0, y: 0, w: 1920, h: 1080 };
}

#[derive(Debug, Clone)]
pub struct OpenConfig {
    pub app_id: String,
    pub payload: LoadPayload,
    pub display: Rect,
}

#[derive(Default)]
struct SessionState {
    epoch: u32,
    load_completed: bool,
    unload_completed: bool,
    media_id: Option<CString>,
    source_info: Option<CString>,
    frames: u64,
    events: Option<SyncSender<Event>>,
}

static SESSION: Mutex<Option<SessionState>> = Mutex::new(None);
static NEXT_EPOCH: AtomicU32 = AtomicU32::new(1);
/// Pipeline events held for `decode`: a few seconds of frame-ready and quality reports.
const EVENT_BACKLOG: usize = 512;
/// A quarantined object blocks every later Load in the native layer for the life of the process;
/// remembered so the refusal can say that only a restart helps.
static QUARANTINED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Playing,
    Bound,
    Streaming,
}

pub struct StarfishVideo {
    epoch: u32,
    plane: VideoPlane,
    display: Rect,
    source: (i32, i32),
    stage: Stage,
    acb: bool,
    events: Receiver<Event>,
    _not_send: PhantomData<*const ()>,
}

/// Opens the event log `native/starfish.c` writes its diagnostics to (stderr when `None`).
pub fn open_event_log(path: Option<&str>) -> bool {
    let path = path.and_then(|p| CString::new(p).ok());
    unsafe { sys::gfn_elog_open(path.as_ref().map_or(std::ptr::null(), |p| p.as_ptr())) != 0 }
}

pub fn video_plane() -> VideoPlane {
    match unsafe { sys::vp_mode() } {
        1 => VideoPlane::Acb,
        2 => VideoPlane::Exported,
        _ => VideoPlane::None,
    }
}

impl StarfishVideo {
    /// Loads the pipeline and starts playback. Blocks until `loadCompleted` (bounded).
    pub fn open(config: OpenConfig) -> Result<Self, String> {
        let plane = video_plane();
        if plane == VideoPlane::None {
            return Err("no video plane binding (neither libAcbAPI nor SDL exported window)".into());
        }
        let window_id = if plane == VideoPlane::Exported {
            let id = unsafe { sys::vp_create_window() };
            if id.is_null() {
                return Err("SDL_webOSCreateExportedWindow failed".into());
            }
            Some(unsafe { CStr::from_ptr(id) }.to_string_lossy().into_owned())
        } else {
            None
        };

        let app_id = CString::new(config.app_id.clone()).map_err(|_| "app id contains NUL")?;
        let acb = plane == VideoPlane::Acb && unsafe { sys::acb_create(app_id.as_ptr(), PLAYER_TYPE_MSE) } != 0;
        if plane == VideoPlane::Acb && !acb {
            unsafe { sys::vp_destroy_window() };
            return Err("AcbAPI_create/initialize failed".into());
        }

        let epoch = NEXT_EPOCH.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = sync_channel(EVENT_BACKLOG);
        *lock_session() = Some(SessionState { epoch, events: Some(tx), ..Default::default() });

        let json = config.payload.to_json(&config.app_id, window_id.as_deref());
        eprintln!("starfish: load epoch={epoch} {json}");
        let json = CString::new(json).map_err(|_| "payload contains NUL")?;
        let mut video = StarfishVideo {
            epoch,
            plane,
            display: config.display,
            source: (config.payload.width as i32, config.payload.height as i32),
            stage: Stage::Playing,
            acb,
            events: rx,
            _not_send: PhantomData,
        };
        if unsafe { sys::sf_load(json.as_ptr(), epoch) } == 0 {
            if QUARANTINED.load(Ordering::Relaxed) {
                return Err("The TV's video player could not be released after the last stream. Restart the app to play again.".into());
            }
            return Err("StarfishMediaAPIs::Load refused".into());
        }
        eprintln!("starfish: load returned");
        video.wait_load_completed()?;
        eprintln!("starfish: load completed");
        if plane == VideoPlane::Exported {
            video.place(video.display);
            video.stage = Stage::Streaming;
        }
        if unsafe { sys::sf_play() } == 0 {
            return Err("StarfishMediaAPIs::Play refused".into());
        }
        Ok(video)
    }

    fn wait_load_completed(&self) -> Result<(), String> {
        let deadline = Instant::now() + LOAD_BUDGET;
        loop {
            {
                let session = lock_session();
                let state = session.as_ref().filter(|s| s.epoch == self.epoch);
                if state.is_some_and(|s| s.load_completed) {
                    return Ok(());
                }
            }
            // webOS 4.x reports load completion as a bare numeric callback, not a "loadCompleted"
            // string, so the object's own state is the reliable signal (as PlxNative's pump polls).
            if unsafe { sys::sf_is_load_completed() } != 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("loadCompleted did not arrive in time".into());
            }
            match self.events.recv_timeout(Duration::from_millis(20)) {
                Ok(Event::Error { code, message }) => {
                    return Err(format!("pipeline refused Load: {code} {message}"));
                }
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err("event channel closed".into()),
            }
        }
    }

    pub fn plane(&self) -> VideoPlane {
        self.plane
    }

    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    /// Feeds one Annex-B access unit. `pts_ns` must be monotonic within the session.
    pub fn feed(&mut self, access_unit: &[u8], pts_ns: i64) -> FeedResult {
        let Ok(size) = c_uint::try_from(access_unit.len()) else {
            return FeedResult::Error;
        };
        let verdict = unsafe { sys::sf_feed(access_unit.as_ptr(), size, pts_ns, ES_VIDEO) };
        self.advance_binding();
        match verdict as u8 {
            b'O' => FeedResult::Accepted,
            b'B' => FeedResult::BufferFull,
            _ => FeedResult::Error,
        }
    }

    /// ACB needs the pipeline's own sourceInfo envelope, which only exists once frames decode.
    fn advance_binding(&mut self) {
        if !self.acb || self.stage == Stage::Streaming {
            return;
        }
        let (media_id, source_info, frames) = {
            let session = lock_session();
            let Some(state) = session.as_ref().filter(|s| s.epoch == self.epoch) else {
                return;
            };
            (state.media_id.clone(), state.source_info.clone(), state.frames)
        };
        if self.stage == Stage::Playing {
            if let Some(id) = media_id {
                unsafe { sys::acb_bind(id.as_ptr()) };
                self.stage = Stage::Bound;
            }
        }
        if self.stage == Stage::Bound && frames >= 2 {
            if let Some(info) = source_info {
                if unsafe { sys::acb_send_video_data(info.as_ptr()) } != -1 {
                    let r = self.display;
                    unsafe { sys::acb_start(r.x as c_long, r.y as c_long, r.w as c_long, r.h as c_long) };
                    self.stage = Stage::Streaming;
                }
            }
        }
    }

    pub fn place(&mut self, display: Rect) {
        self.display = display;
        if self.plane == VideoPlane::Exported {
            let (w, h) = self.source;
            unsafe { sys::vp_place(w, h, display.x, display.y, display.w, display.h) };
        }
    }

    /// Drops everything queued in the pipeline, e.g. before feeding a recovery keyframe.
    pub fn flush(&mut self) -> bool {
        unsafe { sys::sf_flush() != 0 }
    }

    pub fn frames_presented(&self) -> u64 {
        lock_session().as_ref().filter(|s| s.epoch == self.epoch).map_or(0, |s| s.frames)
    }
}

impl Drop for StarfishVideo {
    /// Unload -> callback gate drained -> D1, or quarantine when any proof is missing. Leaking one
    /// object beats letting a late firmware callback touch freed memory (PlxNative engine.rs).
    fn drop(&mut self) {
        if unsafe { sys::sf_ready() } != 0 {
            unsafe { sys::sf_unload() };
            let gate_proven = unsafe { sys::sf_callback_gate_retire() } != 0;
            let deadline = Instant::now() + UNLOAD_BUDGET;
            let unload_completed = loop {
                let done = lock_session()
                    .as_ref()
                    .filter(|s| s.epoch == self.epoch)
                    .is_some_and(|s| s.unload_completed);
                if done || Instant::now() >= deadline {
                    break done;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            if self.acb {
                unsafe { sys::acb_unload() };
            }
            if !(gate_proven && unload_completed) || unsafe { sys::sf_destroy() } == 0 {
                unsafe { sys::sf_quarantine() };
                QUARANTINED.store(true, Ordering::Relaxed);
                eprintln!("starfish: quarantined (gate proven={gate_proven}, unload completed={unload_completed})");
            }
        }
        unsafe { sys::vp_destroy_window() };
        let mut session = lock_session();
        if session.as_ref().is_some_and(|s| s.epoch == self.epoch) {
            *session = None;
        }
    }
}

fn lock_session() -> std::sync::MutexGuard<'static, Option<SessionState>> {
    SESSION.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn between<'a>(haystack: &'a str, open: &str, close: char) -> Option<&'a str> {
    let start = haystack.find(open)? + open.len();
    let len = haystack[start..].find(close)?;
    Some(&haystack[start..start + len])
}

fn on_pipeline_event(epoch: c_uint, kind: c_int, num: i64, message: &str) {
    if kind != CB_FRAME_READY && kind != 46 && kind != 47 {
        eprintln!("starfish: event epoch={epoch} type={kind} num={num} {message}");
    }
    let mut session = lock_session();
    let Some(state) = session.as_mut().filter(|s| s.epoch == epoch) else {
        return;
    };
    if kind == CB_FRAME_READY {
        state.frames += 1;
    }
    if kind == CB_UNLOAD_COMPLETED {
        state.unload_completed = true;
    }
    if message.contains("loadCompleted") || message.contains("\"loaded\"") {
        state.load_completed = true;
    }
    if state.media_id.is_none() {
        let id = between(message, "\"context\":\"", '"').or_else(|| between(message, "\"mediaId\":\"", '"'));
        state.media_id = id.and_then(|id| CString::new(id).ok());
    }
    // ACB wants the pipeline's sourceInfo envelope verbatim, never a re-encoded copy.
    if state.source_info.is_none() && message.contains("\"video\":") && message.contains("\"context\":") {
        state.source_info = CString::new(message).ok();
    }
    let event = match kind {
        CB_FRAME_READY => Event::FrameReady { pts: num },
        CB_ERROR => Event::Error { code: num, message: message.to_owned() },
        _ => Event::Other { kind, num },
    };
    if let Some(events) = &state.events {
        // Only `decode` drains this; while the stream is paused nothing does, and the oldest
        // undrained events are worth least, so the newest are what is dropped once it is full.
        let _ = events.try_send(event);
    }
}

/// Called by `native/starfish.c` on a libplayerAPIs thread.
#[unsafe(no_mangle)]
pub extern "C" fn sf_on_event(epoch: c_uint, kind: c_int, num: i64, message: *const c_char) {
    let _ = std::panic::catch_unwind(|| {
        let message = if message.is_null() { Cow::Borrowed("") } else { unsafe { CStr::from_ptr(message) }.to_string_lossy() };
        on_pipeline_event(epoch, kind, num, &message);
    });
}

/// Called by `native/starfish.c` on a libAcbAPI thread. ACB replies are diagnostics only.
#[unsafe(no_mangle)]
pub extern "C" fn acb_on_event(event: c_long, reply: *const c_char) {
    let reply = if reply.is_null() { String::new() } else { unsafe { CStr::from_ptr(reply) }.to_string_lossy().into_owned() };
    eprintln!("starfish: acb event={event} {reply}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests share the process-wide session.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn events_for_a_retired_epoch_are_ignored() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (tx, rx) = sync_channel(EVENT_BACKLOG);
        *lock_session() = Some(SessionState { epoch: 7, events: Some(tx), ..Default::default() });
        on_pipeline_event(6, CB_FRAME_READY, 1, "");
        on_pipeline_event(7, CB_FRAME_READY, 2, "");
        assert_eq!(rx.try_recv(), Ok(Event::FrameReady { pts: 2 }));
        assert!(rx.try_recv().is_err());
        assert_eq!(lock_session().as_ref().unwrap().frames, 1);
    }

    #[test]
    fn captures_media_id_and_source_info_verbatim() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (tx, _rx) = sync_channel(EVENT_BACKLOG);
        *lock_session() = Some(SessionState { epoch: 9, events: Some(tx), ..Default::default() });
        let info = r#"{"sourceInfo":{"context":"_abc123","video":{"width":1920}}}"#;
        on_pipeline_event(9, 5, 0, info);
        let session = lock_session();
        let state = session.as_ref().unwrap();
        assert_eq!(state.media_id.as_deref(), Some(c"_abc123"));
        assert_eq!(state.source_info.as_ref().unwrap().to_str().unwrap(), info);
    }
}
