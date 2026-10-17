//! One cloud session on its own thread: core `session.create` (or taking over the account's
//! running seat), queue polling, `streamer.prepare`, then the in-process engine until the user
//! ends it. Core calls block for up to a minute and the engine's `start` blocks on RTSP, so none
//! of this may run on the UI thread.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gfn_core_client::CoreClient;
use gfn_sink::MediaRuntime;
use opennow_streamer_core::Engine;
use opennow_streamer_protocol::Command;
use serde_json::{Value, json};

use crate::{Update, Updates};
use crate::settings::StreamSettings;

const POLL_INTERVAL: Duration = Duration::from_millis(1500);
const ENGINE_PROTOCOL_VERSION: u64 = 7;

#[derive(Debug)]
pub enum Progress {
    Status(String),
    Queue(i64),
    /// The account already holds a seat (`app_id` of what runs there); answer on `reply`.
    Conflict { app_id: String, reply: Sender<Takeover> },
    /// What the server agreed to stream and where from; sent once, just before `Started`.
    Profile(Profile),
    Started,
    FirstFrame,
    /// The engine's once-a-second measurements.
    Stats(Stats),
    /// The controller's Guide button: the user wants the stream menu.
    MenuRequested,
    Ended(Option<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Takeover {
    Resume,
    Replace,
    Cancel,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Profile {
    pub resolution: String,
    pub fps: Option<u64>,
    pub codec: String,
    pub max_bitrate_mbps: Option<f64>,
    pub server: String,
    pub gpu: String,
    pub hdr: bool,
}

impl Profile {
    /// From `streamer.prepare`'s context: the negotiated profile wins over the requested settings.
    fn from_context(context: &Value) -> Self {
        let session = &context["session"];
        let negotiated = &session["negotiatedStreamProfile"];
        let settings = &context["settings"];
        let pick = |key: &str| if negotiated[key].is_null() { &settings[key] } else { &negotiated[key] };
        let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
        // `zone` is a host such as eu-netherlands-north.cloudmatchbeta.nvidiagrid.net.
        let zone = session["zone"].as_str().unwrap_or_default().split('.').next().unwrap_or_default();
        let server = match &session["serverLocation"] {
            Value::String(location) if !location.is_empty() => location.clone(),
            _ => zone.to_owned(),
        };
        Self {
            resolution: text(pick("resolution")),
            fps: pick("fps").as_u64(),
            codec: text(pick("codec")).to_uppercase(),
            max_bitrate_mbps: settings["maxBitrateMbps"].as_f64(),
            server,
            gpu: text(&session["gpuType"]),
            hdr: negotiated["enableHdr"].as_bool().unwrap_or(false),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stats {
    pub fps: Option<f64>,
    pub bitrate_mbps: Option<f64>,
    pub ping_ms: Option<f64>,
    pub jitter_ms: Option<f64>,
    pub loss_percent: Option<f64>,
    pub decode_ms: Option<f64>,
    /// Frame handed to the TV's decoder until it is on screen.
    pub display_lag_ms: Option<i64>,
}

impl Stats {
    fn from_telemetry(event: &Value) -> Self {
        let number = |key: &str| event[key].as_f64();
        Self {
            fps: number("framesPerSecond"),
            bitrate_mbps: number("bitrateMbps"),
            ping_ms: number("pingMs"),
            jitter_ms: number("jitterMs"),
            loss_percent: number("packetLossPercent"),
            decode_ms: number("decodeTimeMs"),
            display_lag_ms: gfn_sink::display_lag_ms(),
        }
    }
}

pub struct Launch {
    pub variant: String,
    pub catalog_app_id: Value,
    pub title: String,
    pub scope: Value,
    pub settings: StreamSettings,
}

/// Ends the session it was handed out for; the thread then tears down and reports `Ended`.
pub struct Handle {
    quit: Arc<AtomicBool>,
}

impl Handle {
    pub fn end(&self) {
        self.quit.store(true, Ordering::Relaxed);
    }
}

pub fn start(core: Arc<CoreClient>, runtime: MediaRuntime, launch: Launch, updates: Updates) -> Handle {
    let quit = Arc::new(AtomicBool::new(false));
    let thread_quit = Arc::clone(&quit);
    let send = move |progress| {
        let _ = updates.send(Update::Session(progress));
    };
    let spawned = std::thread::Builder::new().name("gfn-session".into()).spawn({
        let send = send.clone();
        move || {
            let outcome = run(&core, runtime, &launch, &thread_quit, &send);
            send(Progress::Ended(outcome.err().filter(|error| error != CANCELLED)));
        }
    });
    if let Err(error) = spawned {
        send(Progress::Ended(Some(format!("session thread: {error}"))));
    }
    Handle { quit }
}

type Report<'a> = dyn Fn(Progress) + 'a;

/// The error a request returns when the user ended the session while it was in flight; not shown.
const CANCELLED: &str = "cancelled";

fn run(core: &CoreClient, runtime: MediaRuntime, launch: &Launch, quit: &AtomicBool, send: &Report<'_>) -> Result<(), String> {
    // Requests give up when the user ends the session, so Back never waits out a slow request;
    // releasing the seat must not, so it gets its own.
    let call = |method: &str, params: Value, secs: u64| {
        core.request_unless(method, params, Duration::from_secs(secs), quit).map_err(|e| match e {
            gfn_core_client::Error::Cancelled => CANCELLED.to_owned(),
            e => format!("{method}: {e}"),
        })
    };
    let release = |method: &str, params: Value, secs: u64| {
        core.request(method, params, Duration::from_secs(secs)).map_err(|e| format!("{method}: {e}"))
    };
    let (events_tx, engine_events) = channel();
    let mut engine = Engine::with_media_runtime(events_tx, runtime);
    let ready = engine_call(&mut engine, json!({"type": "hello", "protocolVersion": ENGINE_PROTOCOL_VERSION}))?;
    let mut capabilities = ready["capabilities"].clone();
    capabilities["nativeHdrSupported"] = json!(launch.settings.hdr);
    gfn_sink::set_use_ndl(launch.settings.player == "ndl");

    for (key, value) in launch.settings.core_settings() {
        call("settings.set", json!({"key": key, "value": value}), 10)?;
    }
    send(Progress::Status("Requesting a cloud seat".into()));
    let request = json!({
        "appId": launch.variant, "variantId": launch.variant, "catalogAppId": launch.catalog_app_id,
        "scope": launch.scope, "title": launch.title, "accountLinked": true,
        "supportsInGameSettingsPersistence": false, "appLaunchMode": "default",
        "runtimeCapabilities": capabilities, "maxEntitledFps": 0,
    });
    let created = match core.request_unless("session.create", request.clone(), Duration::from_secs(60), quit) {
        Ok(created) => created,
        Err(gfn_core_client::Error::Rpc { code, .. }) if code == "session_conflict" => {
            match take_over_existing(&call, request, send)? {
                Some(created) => created,
                None => return Ok(()),
            }
        }
        // The core still holds a session locally, typically one whose stream died with the
        // network before `session.stop` could reach the service.
        Err(gfn_core_client::Error::Rpc { code, .. }) if code == "session_update_busy" => {
            match take_over_local(&call, request, send)? {
                Some(created) => created,
                None => return Ok(()),
            }
        }
        Err(error) => return Err(describe(&error)),
    };
    let mut seat = Seat { call: &call, release: &release, session: created["session"].clone(), stopped: false };

    let Some(ready_session) = seat.wait_until_ready(quit, send)? else { return seat.stop() };
    send(Progress::Status("Connecting".into()));
    let prepared = call("streamer.prepare", json!({"session": ready_session, "runtimeCapabilities": capabilities}), 15)?;
    let context = &prepared["context"];
    send(Progress::Profile(Profile::from_context(context)));
    engine_call(&mut engine, json!({"type": "start", "context": prepared["context"], "microphoneEnabled": false}))?;
    send(Progress::Started);

    let outcome = pump_engine(&engine_events, quit, send);
    let _ = engine_call(&mut engine, json!({"type": "stop", "reason": "user"}));
    let stopped = seat.stop();
    let _ = engine.handle(command(json!({"type": "shutdown"}))?);
    outcome.and(stopped)
}

fn describe(error: &gfn_core_client::Error) -> String {
    match error {
        gfn_core_client::Error::Cancelled => CANCELLED.to_owned(),
        gfn_core_client::Error::Rpc { message, .. } => message.clone(),
        other => other.to_string(),
    }
}

/// The account's seat is taken: ask the user whether to resume it or replace it with `request`.
fn take_over_existing(call: &Call<'_>, request: Value, send: &Report<'_>) -> Result<Option<Value>, String> {
    let listed = call("session.remote.list", json!({}), 30)?;
    let existing = listed["sessions"].as_array().and_then(|s| s.first()).cloned().ok_or(
        "GeForce NOW reports a running session but none was found. Wait a minute and try again.",
    )?;
    let id = existing["sessionId"].as_str().unwrap_or("?").to_owned();
    let app_id = match &existing["appId"] {
        Value::String(id) => id.clone(),
        other => other.to_string(),
    };
    let (reply, answer) = channel();
    send(Progress::Conflict { app_id, reply });
    match answer.recv().unwrap_or(Takeover::Cancel) {
        Takeover::Cancel => Ok(None),
        Takeover::Resume => {
            send(Progress::Status("Resuming your session".into()));
            call("session.claim", json!({"sessionId": id}), 60).map(Some)
        }
        Takeover::Replace => {
            send(Progress::Status("Ending the running session".into()));
            call("session.stop", json!({"sessionId": id, "streamingBaseUrl": existing["streamingBaseUrl"]}), 35)?;
            send(Progress::Status("Requesting a cloud seat".into()));
            call("session.create", request, 60).map(Some)
        }
    }
}

fn take_over_local(call: &Call<'_>, request: Value, send: &Report<'_>) -> Result<Option<Value>, String> {
    let active = call("session.active.get", json!({}), 15)?;
    let session = &active["session"];
    if session.is_null() {
        return call("session.create", request, 60).map(Some);
    }
    let id = session["sessionId"].as_str().unwrap_or("?").to_owned();
    let base = session["streamingBaseUrl"].clone();
    let app_id = match &session["appId"] {
        Value::String(id) => id.clone(),
        other => other.to_string(),
    };
    let (reply, answer) = channel();
    send(Progress::Conflict { app_id, reply });
    match answer.recv().unwrap_or(Takeover::Cancel) {
        Takeover::Cancel => Ok(None),
        Takeover::Resume => {
            send(Progress::Status("Resuming your session".into()));
            call("session.poll", json!({"sessionId": id, "streamingBaseUrl": base}), 30).map(Some)
        }
        Takeover::Replace => {
            send(Progress::Status("Ending the running session".into()));
            call("session.stop", json!({"sessionId": id, "streamingBaseUrl": base}), 35)?;
            send(Progress::Status("Requesting a cloud seat".into()));
            call("session.create", request, 60).map(Some)
        }
    }
}

type Call<'a> = dyn Fn(&str, Value, u64) -> Result<Value, String> + 'a;

/// A created GFN session holds a paid seat until stopped, so every exit path after
/// `session.create` must release it; `Drop` is the backstop for early returns.
struct Seat<'a> {
    call: &'a Call<'a>,
    release: &'a Call<'a>,
    session: Value,
    stopped: bool,
}

impl Seat<'_> {
    fn id(&self) -> &str {
        self.session["sessionId"].as_str().unwrap_or("?")
    }

    fn wait_until_ready(&mut self, quit: &AtomicBool, send: &Report<'_>) -> Result<Option<Value>, String> {
        let mut last_queue = None;
        loop {
            let status = self.session["status"].as_i64().unwrap_or(0);
            let queue = self.session["queuePosition"].as_i64().unwrap_or(0);
            if queue > 0 && last_queue != Some(queue) {
                send(Progress::Queue(queue));
                last_queue = Some(queue);
            }
            // A claimed seat keeps its old status until a poll confirms the resume.
            let resuming = self.session["resumePending"] == true;
            match status {
                2 | 3 if queue <= 0 && !resuming => return Ok(Some(self.session.clone())),
                7.. => return Err(format!("The session ended (status {status}): {}", self.session["error"])),
                _ if queue <= 0 && last_queue.is_some() => {
                    send(Progress::Status("Preparing your game".into()));
                    last_queue = None;
                }
                _ => {}
            }
            if sleep_unless_quit(POLL_INTERVAL, quit) {
                return Ok(None);
            }
            let polled = (self.call)(
                "session.poll",
                json!({"sessionId": self.id(), "streamingBaseUrl": self.session["streamingBaseUrl"]}),
                30,
            )?;
            self.session = polled["session"].clone();
        }
    }

    fn stop(&mut self) -> Result<(), String> {
        if std::mem::replace(&mut self.stopped, true) {
            return Ok(());
        }
        let params = json!({"sessionId": self.id(), "streamingBaseUrl": self.session["streamingBaseUrl"]});
        (self.release)("session.stop", params, 35).map(|_| ())
    }
}

impl Drop for Seat<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("releasing the seat failed: {error}");
        }
    }
}

fn pump_engine(events: &Receiver<Value>, quit: &AtomicBool, send: &Report<'_>) -> Result<(), String> {
    loop {
        if quit.load(Ordering::Relaxed) {
            return Ok(());
        }
        let event = match events.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Err("The stream engine stopped unexpectedly".into()),
        };
        match event["type"].as_str().unwrap_or("") {
            "status" => {
                if event["event"] == "first-frame" {
                    send(Progress::FirstFrame);
                }
                if event["status"] == "stopped" {
                    return Ok(());
                }
            }
            "overlay-request" => send(Progress::MenuRequested),
            "telemetry" => send(Progress::Stats(Stats::from_telemetry(&event))),
            "error" => return Err(format!("{}", event["message"].as_str().unwrap_or("stream error"))),
            _ => {}
        }
    }
}

fn command(mut value: Value) -> Result<Command, String> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    value["id"] = json!(format!("app-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
    serde_json::from_value(value).map_err(|e| format!("engine command: {e}"))
}

/// Runs one engine command and returns its response (the reply that carries the command's id).
fn engine_call(engine: &mut Engine, value: Value) -> Result<Value, String> {
    let kind = value["type"].as_str().unwrap_or("?").to_owned();
    let command = command(value)?;
    let id = command.id.clone();
    let (replies, _) = engine.handle(command);
    let reply = replies.into_iter().find(|r| r["id"] == id.as_str()).ok_or(format!("engine {kind}: no reply"))?;
    if reply["type"] == "error" {
        return Err(format!("engine {kind}: {} {}", reply["code"], reply["message"]));
    }
    Ok(reply)
}

/// Returns true when quit was requested during the wait.
pub fn sleep_unless_quit(duration: Duration, quit: &AtomicBool) -> bool {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        if quit.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    quit.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_prefers_what_the_server_negotiated() {
        let context = json!({
            "session": {
                "zone": "eu-netherlands-north.cloudmatchbeta.nvidiagrid.net",
                "serverLocation": null,
                "gpuType": "L40G",
                "negotiatedStreamProfile": {"resolution": "1920x1080", "fps": 60, "codec": "h265"},
            },
            "settings": {"resolution": "3840x2160", "fps": 120, "codec": "auto", "maxBitrateMbps": 20.0},
        });
        let profile = Profile::from_context(&context);
        assert_eq!(
            profile,
            Profile {
                resolution: "1920x1080".into(),
                fps: Some(60),
                codec: "H265".into(),
                max_bitrate_mbps: Some(20.0),
                server: "eu-netherlands-north".into(),
                gpu: "L40G".into(),
                hdr: false,
            }
        );
    }

    #[test]
    fn telemetry_keeps_missing_measurements_missing() {
        let stats = Stats::from_telemetry(&json!({"type": "telemetry", "framesPerSecond": 59.5, "pingMs": null}));
        assert_eq!(stats.fps, Some(59.5));
        assert_eq!(stats.ping_ms, None);
    }
}
