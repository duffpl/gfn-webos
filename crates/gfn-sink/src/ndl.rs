//! The TV's NDL DirectMedia video path (webOS 4 API v1): access units go straight to the
//! hardware decoder with no media pipeline in between, which is why moonlight-tv prefers it over
//! Starfish on webOS 3.5–4.x (ss4s `ndl-webos4`, weight 50 vs SMP's 10). The v1 API knows only
//! H.264 and has no HDR.
//!
//! The library is dlopen'd so a TV without it only loses this option.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use opennow_streamer_platform_webos::{EncodedFrame, MediaStreamConfig, MediaVideoCodec};

use crate::{Decoded, VideoDecoder};

const LIBRARY: &CStr = c"libNDL_directmedia.so.1";

#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(file: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}

const RTLD_NOW: c_int = 2;

/// `NDL_DIRECTVIDEO_DATA_INFO_T`. `source` stays NONE as in ss4s, the only proven caller.
#[repr(C)]
struct VideoInfo {
    width: c_int,
    height: c_int,
    source: c_int,
}

type PlayCallback = extern "C" fn(u64);

struct Api {
    get_error: extern "C" fn() -> *const c_char,
    video_open: extern "C" fn(*mut VideoInfo) -> c_int,
    video_close: extern "C" fn() -> c_int,
    video_set_area: extern "C" fn(c_int, c_int, c_int, c_int) -> c_int,
    video_set_callback: extern "C" fn(PlayCallback) -> c_int,
    video_play: extern "C" fn(*const c_void, u32, u64) -> c_int,
}

impl Api {
    fn error(&self) -> String {
        let message = (self.get_error)();
        if message.is_null() { "unknown error".into() } else { unsafe { CStr::from_ptr(message) }.to_string_lossy().into_owned() }
    }
}

/// Loaded and initialised once per process: ss4s also initialises at startup and only quits on
/// exit, and a second `NDL_DirectMediaInit` after `Quit` has never been exercised.
static API: OnceLock<Result<Api, String>> = OnceLock::new();

fn api(app_id: &str) -> Result<&'static Api, String> {
    API.get_or_init(|| load(app_id)).as_ref().map_err(Clone::clone)
}

fn load(app_id: &str) -> Result<Api, String> {
    unsafe {
        let lib = dlopen(LIBRARY.as_ptr(), RTLD_NOW);
        if lib.is_null() {
            let reason = dlerror();
            let reason = if reason.is_null() { "not found".into() } else { CStr::from_ptr(reason).to_string_lossy() };
            return Err(format!("{}: {reason}", LIBRARY.to_string_lossy()));
        }
        let symbol = |name: &CStr| {
            let address = dlsym(lib, name.as_ptr());
            if address.is_null() { Err(format!("{} lacks {}", LIBRARY.to_string_lossy(), name.to_string_lossy())) } else { Ok(address) }
        };
        let init: extern "C" fn(*const c_char, *const c_void) -> c_int =
            std::mem::transmute(symbol(c"NDL_DirectMediaInit")?);
        let api = Api {
            get_error: std::mem::transmute(symbol(c"NDL_DirectMediaGetError")?),
            video_open: std::mem::transmute(symbol(c"NDL_DirectVideoOpen")?),
            video_close: std::mem::transmute(symbol(c"NDL_DirectVideoClose")?),
            video_set_area: std::mem::transmute(symbol(c"NDL_DirectVideoSetArea")?),
            video_set_callback: std::mem::transmute(symbol(c"NDL_DirectVideoSetCallback")?),
            video_play: std::mem::transmute(symbol(c"NDL_DirectVideoPlayWithCallback")?),
        };
        let app_id = CString::new(app_id).map_err(|e| e.to_string())?;
        if init(app_id.as_ptr(), std::ptr::null()) != 0 {
            return Err(format!("NDL_DirectMediaInit: {}", api.error()));
        }
        Ok(api)
    }
}

/// Feed-to-callback times, written by NDL's callback thread. The callback's userdata is the feed
/// time in microseconds since `EPOCH`.
static EPOCH: OnceLock<Instant> = OnceLock::new();
static DONE: AtomicU64 = AtomicU64::new(0);
static LAG_SUM_US: AtomicI64 = AtomicI64::new(0);
static LAG_COUNT: AtomicI64 = AtomicI64::new(0);
static LAG_MIN_US: AtomicI64 = AtomicI64::new(i64::MAX);
static LAG_MAX_US: AtomicI64 = AtomicI64::new(0);

fn micros_now() -> u64 {
    u64::try_from(EPOCH.get_or_init(Instant::now).elapsed().as_micros()).unwrap_or(u64::MAX)
}

extern "C" fn on_frame_done(fed_at_us: u64) {
    let lag = i64::try_from(micros_now().saturating_sub(fed_at_us)).unwrap_or(i64::MAX);
    DONE.fetch_add(1, Ordering::Relaxed);
    LAG_SUM_US.fetch_add(lag, Ordering::Relaxed);
    LAG_COUNT.fetch_add(1, Ordering::Relaxed);
    LAG_MIN_US.fetch_min(lag, Ordering::Relaxed);
    LAG_MAX_US.fetch_max(lag, Ordering::Relaxed);
}

/// Only one NDL video can be open per process; the video thread opens at most one at a time.
pub struct NdlDecoder {
    api: &'static Api,
    fed: u64,
    rejected: u64,
    done_at_open: u64,
    since: Instant,
    lag_window: (Instant, i64, i64),
}

impl NdlDecoder {
    pub fn open(app_id: &str, config: &MediaStreamConfig) -> Result<Self, String> {
        if config.codec != MediaVideoCodec::H264 {
            return Err(format!("NDL plays H.264 only, not {}", config.codec.label()));
        }
        let api = api(app_id)?;
        let width = c_int::try_from(config.width).map_err(|e| e.to_string())?;
        let height = c_int::try_from(config.height).map_err(|e| e.to_string())?;
        let mut info = VideoInfo { width, height, source: 0 };
        if (api.video_open)(&raw mut info) != 0 {
            return Err(format!("NDL_DirectVideoOpen {width}x{height}: {}", api.error()));
        }
        (api.video_set_callback)(on_frame_done);
        let (x, y, w, h) = fit_full_hd(width, height);
        (api.video_set_area)(x, y, w, h);
        eprintln!("ndl: open {width}x{height}@{} area {x},{y} {w}x{h}", config.fps);
        Ok(Self {
            api,
            fed: 0,
            rejected: 0,
            done_at_open: DONE.load(Ordering::Relaxed),
            since: Instant::now(),
            lag_window: (Instant::now(), 0, 0),
        })
    }

    fn report(&mut self) {
        let window = &mut self.lag_window;
        if window.0.elapsed() >= Duration::from_secs(1) {
            let (sum, count) = (LAG_SUM_US.swap(0, Ordering::Relaxed), LAG_COUNT.swap(0, Ordering::Relaxed));
            window.1 += sum;
            window.2 += count;
            if count > 0 {
                crate::set_display_lag_ms(Some(sum / count / 1000));
            }
            window.0 = Instant::now();
        }
        if self.since.elapsed() >= Duration::from_secs(5) {
            let (sum, count) = (self.lag_window.1, self.lag_window.2);
            eprintln!(
                "ndl: fed={} done={} rejected={} callback-lag avg={}ms min={}ms max={}ms",
                self.fed,
                self.presented_frames(),
                self.rejected,
                sum / count.max(1) / 1000,
                if count > 0 { LAG_MIN_US.swap(i64::MAX, Ordering::Relaxed) / 1000 } else { 0 },
                LAG_MAX_US.swap(0, Ordering::Relaxed) / 1000,
            );
            (self.lag_window.1, self.lag_window.2) = (0, 0);
            self.since = Instant::now();
        }
    }
}

/// NDL places the plane in 1920x1080 UI coordinates whatever the stream size; letterbox to fit.
fn fit_full_hd(width: c_int, height: c_int) -> (c_int, c_int, c_int, c_int) {
    let scaled_height = 1920 * height / width;
    if scaled_height <= 1080 {
        (0, (1080 - scaled_height) / 2, 1920, scaled_height)
    } else {
        let scaled_width = 1920 * 1080 / scaled_height;
        ((1920 - scaled_width) / 2, 0, scaled_width, 1080)
    }
}

impl VideoDecoder for NdlDecoder {
    fn decode(&mut self, frame: &EncodedFrame) -> Result<Decoded, String> {
        let size = u32::try_from(frame.data.len()).map_err(|e| e.to_string())?;
        let rc = (self.api.video_play)(frame.data.as_ptr().cast(), size, micros_now());
        self.fed += 1;
        self.report();
        if rc == 0 {
            Ok(Decoded::Accepted)
        } else {
            // A refused access unit has never been seen to mean a dead decoder; the pipeline
            // requests a keyframe for the broken chain.
            self.rejected += 1;
            Ok(Decoded::Skipped)
        }
    }

    fn presented_frames(&self) -> u64 {
        DONE.load(Ordering::Relaxed) - self.done_at_open
    }
}

impl Drop for NdlDecoder {
    fn drop(&mut self) {
        (self.api.video_close)();
        crate::set_display_lag_ms(None);
        eprintln!("ndl: closed");
    }
}

#[cfg(test)]
mod tests {
    use super::fit_full_hd;

    #[test]
    fn letterboxes_into_full_hd() {
        assert_eq!(fit_full_hd(2560, 1440), (0, 0, 1920, 1080));
        assert_eq!(fit_full_hd(1920, 800), (0, 140, 1920, 800));
        assert_eq!(fit_full_hd(1440, 1080), (240, 0, 1440, 1080));
    }
}
