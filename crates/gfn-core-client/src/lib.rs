//! Client for `opennow-core`, the process that owns NVIDIA auth, CloudMatch sessions and catalog.
//!
//! Wire format (OpenNOW docs/core-protocol.md): newline-delimited JSON on the child's stdin/stdout,
//! stderr reserved for redacted diagnostics. This mirrors the Qt shell's `CoreClient`: handshake
//! first, bounded per-request deadlines, `busy` retried with the same id and payload (100 ms
//! doubling to 1 s, never past the original deadline), and an `ack` for every accepted
//! `session.create` so the core does not delete the freshly allocated seat.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub const PROTOCOL_VERSION: u64 = 5;
const MAX_LINE_BYTES: usize = 1024 * 1024;
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(5);
const MIN_DEADLINE: Duration = Duration::from_millis(100);
const MAX_DEADLINE: Duration = Duration::from_secs(300);
const REQUIRED_CAPABILITIES: [&str; 1] = ["queue.servers.v1"];

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The core answered with `ok: false`.
    Rpc { code: String, message: String },
    Timeout,
    Disconnected,
    Protocol(String),
    Io(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rpc { code, message } => write!(f, "{code}: {message}"),
            Self::Timeout => f.write_str("request timed out"),
            Self::Disconnected => f.write_str("core process disconnected"),
            Self::Protocol(message) => write!(f, "protocol error: {message}"),
            Self::Io(message) => write!(f, "io error: {message}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, PartialEq)]
pub struct CoreEvent {
    pub name: String,
    pub payload: Value,
}

type Reply = Result<Value, Error>;

struct Shared {
    stdin: Mutex<Option<ChildStdin>>,
    pending: Mutex<HashMap<String, Sender<Reply>>>,
}

impl Shared {
    fn write(&self, message: &Value) -> Result<(), Error> {
        let mut line = serde_json::to_vec(message).map_err(|e| Error::Protocol(e.to_string()))?;
        line.push(b'\n');
        let mut stdin = self.stdin.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let stdin = stdin.as_mut().ok_or(Error::Disconnected)?;
        stdin.write_all(&line).and_then(|()| stdin.flush()).map_err(|e| Error::Io(e.to_string()))
    }

    fn fail_all(&self) {
        let pending = std::mem::take(&mut *self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        for (_, reply) in pending {
            let _ = reply.send(Err(Error::Disconnected));
        }
    }
}

pub struct CoreClient {
    child: Child,
    shared: Arc<Shared>,
    next_id: AtomicU64,
    capabilities: Vec<String>,
}

pub struct SpawnOptions<'a> {
    pub executable: &'a Path,
    pub data_dir: Option<&'a Path>,
    pub shell_version: &'a str,
    /// Where the core's stderr diagnostics go; `None` discards them.
    pub diagnostics: Option<Box<dyn FnMut(&str) + Send>>,
}

impl CoreClient {
    /// Starts the core process and completes `core.hello`. Events arrive on the returned receiver.
    pub fn spawn(options: SpawnOptions<'_>) -> Result<(Self, Receiver<CoreEvent>), Error> {
        let mut command = Command::new(options.executable);
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some(dir) = options.data_dir {
            // The core also writes under $HOME (the media library's Pictures folder). A webOS app
            // jail sets HOME to the read-only app directory, so keep everything in the data dir.
            command.env("OPENNOW_DATA_DIR", dir).env("HOME", dir).env("OPENNOW_PICTURES_DIR", dir.join("media"));
        }
        let mut child = command.spawn().map_err(|e| Error::Io(format!("{}: {e}", options.executable.display())))?;
        let stdin = child.stdin.take().ok_or(Error::Disconnected)?;
        let stdout = child.stdout.take().ok_or(Error::Disconnected)?;
        let stderr = child.stderr.take().ok_or(Error::Disconnected)?;

        let shared = Arc::new(Shared { stdin: Mutex::new(Some(stdin)), pending: Mutex::new(HashMap::new()) });
        let (events_tx, events_rx) = mpsc::channel();
        let reader_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("core-stdout".into())
            .spawn(move || read_messages(BufReader::new(stdout), &reader_shared, &events_tx))
            .map_err(|e| Error::Io(e.to_string()))?;
        let mut diagnostics = options.diagnostics;
        thread::Builder::new()
            .name("core-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if let Some(sink) = diagnostics.as_mut() {
                        sink(&line);
                    }
                }
            })
            .map_err(|e| Error::Io(e.to_string()))?;

        let mut client = Self { child, shared, next_id: AtomicU64::new(1), capabilities: Vec::new() };
        let hello = client.request(
            "core.hello",
            json!({"protocolVersion": PROTOCOL_VERSION, "shell": "webos", "shellVersion": options.shell_version}),
            HANDSHAKE_DEADLINE,
        )?;
        if hello.get("protocolVersion").and_then(Value::as_u64) != Some(PROTOCOL_VERSION) {
            return Err(Error::Protocol(format!("core protocol mismatch: {hello}")));
        }
        client.capabilities = hello
            .get("capabilities")
            .and_then(Value::as_array)
            .map(|caps| caps.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default();
        if let Some(missing) = REQUIRED_CAPABILITIES.iter().find(|c| !client.has_capability(c)) {
            return Err(Error::Protocol(format!("core lacks required capability {missing}")));
        }
        Ok((client, events_rx))
    }

    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }

    pub fn request(&self, method: &str, params: Value, deadline: Duration) -> Reply {
        let deadline_at = Instant::now() + deadline.clamp(MIN_DEADLINE, MAX_DEADLINE);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let message = json!({"type": "request", "id": id, "method": method, "params": params});
        let mut retry_delay = Duration::from_millis(100);
        loop {
            let (tx, rx) = mpsc::channel();
            self.lock_pending().insert(id.clone(), tx);
            if let Err(error) = self.shared.write(&message) {
                self.lock_pending().remove(&id);
                return Err(error);
            }
            let remaining = deadline_at.saturating_duration_since(Instant::now());
            let reply = match rx.recv_timeout(remaining) {
                Ok(reply) => reply,
                Err(RecvTimeoutError::Timeout) => {
                    self.lock_pending().remove(&id);
                    let _ = self.shared.write(&json!({"type": "cancel", "id": id}));
                    return Err(Error::Timeout);
                }
                Err(RecvTimeoutError::Disconnected) => return Err(Error::Disconnected),
            };
            match reply {
                Err(Error::Rpc { code, .. }) if code == "busy" && Instant::now() + retry_delay < deadline_at => {
                    thread::sleep(retry_delay);
                    retry_delay = (retry_delay * 2).min(Duration::from_secs(1));
                }
                Ok(result) => {
                    if method == "session.create" {
                        self.shared.write(&json!({"type": "ack", "id": id}))?;
                    }
                    return Ok(result);
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn lock_pending(&self) -> std::sync::MutexGuard<'_, HashMap<String, Sender<Reply>>> {
        self.shared.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for CoreClient {
    fn drop(&mut self) {
        // stdin EOF ends the core's read loop; give it a moment to persist state before killing it.
        self.shared.stdin.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_messages(stdout: impl BufRead, shared: &Shared, events: &Sender<CoreEvent>) {
    for line in stdout.split(b'\n') {
        let Ok(line) = line else { break };
        if line.len() > MAX_LINE_BYTES {
            break;
        }
        let Ok(message) = serde_json::from_slice::<Value>(&line) else { break };
        if !dispatch(&message, shared, events) {
            break;
        }
    }
    shared.fail_all();
}

/// Returns false when the message violates the protocol; the connection is then abandoned, as the
/// spec requires, rather than left ambiguous.
fn dispatch(message: &Value, shared: &Shared, events: &Sender<CoreEvent>) -> bool {
    match message.get("type").and_then(Value::as_str) {
        Some("response") => {
            let Some(id) = message.get("id").and_then(Value::as_str) else { return false };
            let reply = if message.get("ok").and_then(Value::as_bool) == Some(true) {
                Ok(message.get("result").cloned().unwrap_or(Value::Null))
            } else {
                let error = message.get("error");
                let text = |key| error.and_then(|e| e.get(key)).and_then(Value::as_str).unwrap_or_default().to_owned();
                Err(Error::Rpc { code: text("code"), message: text("message") })
            };
            let waiter = shared.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(id);
            if let Some(waiter) = waiter {
                let _ = waiter.send(reply);
            }
            true
        }
        Some("event") => {
            let Some(name) = message.get("name").and_then(Value::as_str) else { return false };
            let payload = message.get("payload").cloned().unwrap_or(Value::Null);
            let _ = events.send(CoreEvent { name: name.to_owned(), payload });
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared_with_waiter(id: &str) -> (Arc<Shared>, Receiver<Reply>, Child) {
        let mut child = Command::new("cat").stdin(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
        let shared = Arc::new(Shared {
            stdin: Mutex::new(child.stdin.take()),
            pending: Mutex::new(HashMap::new()),
        });
        let (tx, rx) = mpsc::channel();
        shared.pending.lock().unwrap().insert(id.to_owned(), tx);
        (shared, rx, child)
    }

    #[test]
    fn routes_responses_errors_and_events() {
        let (shared, rx, mut child) = shared_with_waiter("7");
        let (events_tx, events_rx) = mpsc::channel();
        let input = concat!(
            r#"{"type":"event","name":"settings.changed","payload":{"key":"fps"}}"#, "\n",
            r#"{"type":"response","id":"7","ok":false,"error":{"code":"busy","message":"later"}}"#, "\n",
        );
        read_messages(input.as_bytes(), &shared, &events_tx);
        assert_eq!(rx.recv().unwrap(), Err(Error::Rpc { code: "busy".into(), message: "later".into() }));
        assert_eq!(events_rx.recv().unwrap().name, "settings.changed");
        let _ = child.kill();
    }

    #[test]
    fn malformed_message_fails_every_pending_request() {
        let (shared, rx, mut child) = shared_with_waiter("1");
        let (events_tx, _events_rx) = mpsc::channel();
        read_messages("{\"type\":\"bogus\"}\n".as_bytes(), &shared, &events_tx);
        assert_eq!(rx.recv().unwrap(), Err(Error::Disconnected));
        let _ = child.kill();
    }
}
