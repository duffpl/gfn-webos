//! Keeps the TV awake while a controller is in use.
//!
//! Before its screensaver starts, webOS asks every app registered with
//! `tvpower/registerScreenSaverRequest`, and LG's SDL answers "not now" when one of its own
//! joysticks was used recently. Our controllers are read from evdev, which SDL never sees, so the
//! TV went to sleep while someone browsed with a pad; this answers the same requests from our own
//! record of controller input.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const SERVICE: &str = "luna://com.webos.service.tvpower/power";
/// Without controller input for this long the TV may sleep again, so a pad left on the sofa does
/// not keep it on all night.
const IDLE_LIMIT: Duration = Duration::from_secs(10 * 60);
/// A resting stick still reports small movements; only a deliberate push counts as use.
const AXIS_ACTIVE: i16 = 8_000;

#[derive(Clone)]
pub struct Activity(Arc<Shared>);

struct Shared {
    start: Instant,
    /// Milliseconds since `start` of the last controller input, plus one; zero for none yet.
    last_input: AtomicU64,
    streaming: AtomicBool,
}

impl Activity {
    fn new() -> Self {
        Self(Arc::new(Shared { start: Instant::now(), last_input: AtomicU64::new(0), streaming: AtomicBool::new(false) }))
    }

    pub fn button(&self, pressed: bool) {
        if pressed {
            self.touch();
        }
    }

    pub fn axis(&self, value: i16) {
        if value.unsigned_abs() >= AXIS_ACTIVE.unsigned_abs() {
            self.touch();
        }
    }

    pub fn streaming(&self, streaming: bool) {
        self.0.streaming.store(streaming, Ordering::Relaxed);
    }

    fn touch(&self) {
        let now = self.0.start.elapsed().as_millis() as u64 + 1;
        self.0.last_input.store(now, Ordering::Relaxed);
    }

    fn idle(&self) -> Option<Duration> {
        match self.0.last_input.load(Ordering::Relaxed) {
            0 => None,
            last => Some(self.0.start.elapsed().saturating_sub(Duration::from_millis(last - 1))),
        }
    }

    fn keep_awake(&self) -> bool {
        keep_awake(self.idle(), self.0.streaming.load(Ordering::Relaxed))
    }
}

fn keep_awake(idle: Option<Duration>, streaming: bool) -> bool {
    streaming || idle.is_some_and(|idle| idle < IDLE_LIMIT)
}

/// Registers with the TV's power service; off the TV it only records activity.
pub fn start(tv: bool, app_id: String) -> Activity {
    let activity = Activity::new();
    if tv {
        let watched = activity.clone();
        std::thread::Builder::new()
            .name("screensaver".into())
            .spawn(move || run(&watched, &app_id))
            .expect("spawning the screensaver thread");
    }
    activity
}

fn run(activity: &Activity, app_id: &str) {
    let (replies_tx, replies) = channel();
    let bus = match gfn_luna::Bus::open(replies_tx) {
        Ok(bus) => bus,
        Err(error) => {
            eprintln!("screensaver: no bus: {error}");
            return;
        }
    };
    let registration = bus.call(&format!("{SERVICE}/registerScreenSaverRequest"), &json!({ "clientName": app_id, "subscribe": true }));
    while let Ok(reply) = replies.recv() {
        if reply.payload["returnValue"] == false {
            eprintln!("screensaver: {}", reply.payload);
            if reply.call == registration {
                return;
            }
            continue;
        }
        if reply.call != registration {
            continue;
        }
        if let Some(response) = response(&reply.payload, app_id, activity.keep_awake()) {
            println!("screensaver: asked; {}", if response["ack"] == true { "allowed" } else { "held off" });
            bus.call(&format!("{SERVICE}/responseScreenSaverRequest"), &response);
        }
    }
}

/// The answer to one screensaver request, which carries the timestamp it is to be answered with;
/// the registration's first reply has none and needs no answer.
fn response(request: &Value, app_id: &str, keep_awake: bool) -> Option<Value> {
    let timestamp = match request.get("timestamp")? {
        Value::String(timestamp) => timestamp.clone(),
        other => other.to_string(),
    };
    Some(json!({ "clientName": app_id, "timestamp": timestamp, "ack": !keep_awake }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_controller_use_or_a_stream_holds_the_screensaver_off() {
        assert!(!keep_awake(None, false));
        assert!(keep_awake(Some(Duration::from_secs(30)), false));
        assert!(!keep_awake(Some(IDLE_LIMIT), false));
        assert!(keep_awake(None, true));
    }

    #[test]
    fn small_stick_movements_do_not_count_as_use() {
        let activity = Activity::new();
        activity.axis(AXIS_ACTIVE - 1);
        activity.axis(-(AXIS_ACTIVE - 1));
        activity.button(false);
        assert!(activity.idle().is_none());
        activity.axis(-AXIS_ACTIVE);
        assert!(activity.keep_awake());
    }

    #[test]
    fn requests_are_answered_with_their_timestamp() {
        assert_eq!(response(&json!({ "returnValue": true, "subscribed": true }), "app", true), None);
        let request = json!({ "returnValue": true, "timestamp": "1234", "state": "Active" });
        assert_eq!(response(&request, "app", true), Some(json!({ "clientName": "app", "timestamp": "1234", "ack": false })));
        assert_eq!(response(&json!({ "timestamp": 99 }), "app", false), Some(json!({ "clientName": "app", "timestamp": "99", "ack": true })));
    }
}
