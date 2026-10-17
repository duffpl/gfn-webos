//! Calls to webOS system services over the luna-service2 bus (`native/luna.c`).
//!
//! Only the TV has a bus; elsewhere [`Bus::open`] fails and callers fall back or go without.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

/// One reply to the call that [`Bus::call`] numbered `call`. Subscriptions reply many times.
#[derive(Debug, Clone)]
pub struct Reply {
    pub call: u64,
    pub payload: Value,
}

/// One user of the process's single bus connection, with its own reply channel.
pub struct Bus {
    sink: u64,
    next: AtomicU32,
}

/// Call ids carry their bus's sink in the top bits: native/luna.c passes ids through a 32-bit
/// pointer, which leaves 2^28 calls per user.
const SINK_SHIFT: u32 = 28;
const MAX_SINKS: usize = 1 << (32 - SINK_SHIFT);

static CONNECTED: OnceLock<Result<(), String>> = OnceLock::new();
static SINKS: Mutex<Vec<Sender<Reply>>> = Mutex::new(Vec::new());

impl Bus {
    /// Connects anonymously on first use; replies to this bus's calls go to `replies`.
    pub fn open(replies: Sender<Reply>) -> Result<Self, String> {
        CONNECTED.get_or_init(sys::open).clone()?;
        let mut sinks = SINKS.lock().unwrap_or_else(|e| e.into_inner());
        if sinks.len() == MAX_SINKS {
            return Err("too many bus users".into());
        }
        sinks.push(replies);
        Ok(Self { sink: (sinks.len() - 1) as u64, next: AtomicU32::new(1) })
    }

    /// `uri` is `luna://service/method`; add `"subscribe": true` to `payload` for a subscription.
    /// Replies keep coming until [`Bus::cancel`].
    pub fn call(&self, uri: &str, payload: &Value) -> u64 {
        let call = self.sink << SINK_SHIFT | u64::from(self.next.fetch_add(1, Ordering::Relaxed));
        sys::call(call, uri, &payload.to_string());
        call
    }

    pub fn cancel(&self, call: u64) {
        sys::cancel(call);
    }
}

#[cfg_attr(not(all(target_os = "linux", target_arch = "arm")), allow(dead_code))]
fn deliver(call: u64, payload: &str) {
    let payload = serde_json::from_str(payload)
        .unwrap_or_else(|_| serde_json::json!({ "returnValue": false, "errorText": format!("unparsable reply: {payload}") }));
    let sinks = SINKS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(replies) = sinks.get((call >> SINK_SHIFT) as usize) {
        let _ = replies.send(Reply { call, payload });
    }
}

#[cfg(all(target_os = "linux", target_arch = "arm"))]
mod sys {
    use std::ffi::{CStr, CString, c_char};

    unsafe extern "C" {
        fn gfn_luna_open(callback: extern "C" fn(u64, *const c_char), err: *mut c_char, err_len: usize) -> i32;
        fn gfn_luna_call(id: u64, uri: *const c_char, payload: *const c_char);
        fn gfn_luna_cancel(id: u64);
    }

    extern "C" fn on_reply(id: u64, payload: *const c_char) {
        let payload = unsafe { CStr::from_ptr(payload) }.to_string_lossy();
        super::deliver(id, &payload);
    }

    pub fn open() -> Result<(), String> {
        let mut err = [0 as c_char; 256];
        if unsafe { gfn_luna_open(on_reply, err.as_mut_ptr(), err.len()) } == 0 {
            return Ok(());
        }
        Err(unsafe { CStr::from_ptr(err.as_ptr()) }.to_string_lossy().into_owned())
    }

    pub fn call(id: u64, uri: &str, payload: &str) {
        let (Ok(uri), Ok(payload)) = (CString::new(uri), CString::new(payload)) else {
            super::deliver(id, r#"{"returnValue":false,"errorText":"NUL byte in the request"}"#);
            return;
        };
        unsafe { gfn_luna_call(id, uri.as_ptr(), payload.as_ptr()) };
    }

    pub fn cancel(id: u64) {
        unsafe { gfn_luna_cancel(id) };
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "arm")))]
mod sys {
    pub fn open() -> Result<(), String> {
        Err("there is no luna bus off the TV".into())
    }

    pub fn call(_: u64, _: &str, _: &str) {}

    pub fn cancel(_: u64) {}
}
