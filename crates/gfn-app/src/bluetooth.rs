//! Pairs and connects Bluetooth controllers through the TV's own Bluetooth service,
//! `com.webos.service.bluetooth2`, with the call sequence the webos-gamepad project found works for
//! apps: discovery, `adapter/pair` (a subscription that ends in `endPairing`), then `hid/connect`.
//! A connected controller then appears as a `/dev/input/event*` device, which gfn-input reads.
//!
//! A worker thread owns the bus and sends the UI a full [`Snapshot`] after every change. The
//! simulator drives the same worker with a scripted stand-in for the service.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use gfn_luna::Reply;
use serde_json::{Value, json};

use crate::{Update, Updates};

const SERVICE: &str = "luna://com.webos.service.bluetooth2";
/// Long enough for a controller to show up, short enough not to keep the radio busy (it shares
/// the antenna with Wi-Fi on the TV).
const SEARCH_FOR: Duration = Duration::from_secs(60);
const GIVE_UP_AFTER: Duration = Duration::from_secs(30);
/// Paired controllers that are off are retried this often while no stream runs: a controller
/// switched on later may wait for the TV to connect to it rather than connect itself.
const RECONNECT_EVERY: Duration = Duration::from_secs(15);
/// Housekeeping cadence while something is in flight (a search, a pairing, a scripted reply).
const TICK: Duration = Duration::from_millis(50);
/// Otherwise nothing changes between replies but the reconnect timer, so the worker mostly sleeps.
const IDLE_TICK: Duration = Duration::from_secs(1);
/// A reply on no call: sent after each command to wake the worker out of its idle wait.
const WAKE: u64 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// The controllers screen opened: look for new devices.
    Open,
    Close,
    Search,
    Pair(String),
    Connect(String),
    Disconnect(String),
    Forget(String),
    /// No background reconnect attempts while a stream runs: paging competes with Wi-Fi.
    Streaming(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Connected,
    Paired,
    Nearby,
    Pairing,
    Connecting,
    Disconnecting,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub address: String,
    pub name: String,
    pub controller: bool,
    pub status: Status,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// Why Bluetooth cannot be used at all.
    pub unavailable: Option<String>,
    pub searching: bool,
    /// Paired controllers, connected ones first.
    pub paired: Vec<Device>,
    /// Unpaired devices in range, controllers first.
    pub nearby: Vec<Device>,
}

impl Snapshot {
    pub fn connected(&self) -> usize {
        self.paired.iter().filter(|d| d.status == Status::Connected).count()
    }
}

pub struct Handle {
    commands: Sender<Command>,
    wake: Sender<Reply>,
}

impl Handle {
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
        let _ = self.wake.send(Reply { call: WAKE, payload: Value::Null });
    }
}

pub fn start(updates: Updates) -> Handle {
    let (commands_tx, commands) = channel();
    let (replies_tx, replies) = channel();
    let bus = open_bus(replies_tx.clone());
    std::thread::Builder::new()
        .name("bluetooth".into())
        .spawn(move || run(bus, &replies, &commands, &updates))
        .expect("spawning the bluetooth thread");
    Handle { commands: commands_tx, wake: replies_tx }
}

fn open_bus(replies: Sender<Reply>) -> Result<Box<dyn Bus>, String> {
    #[cfg(feature = "sim")]
    {
        Ok(Box::new(fake::Fake::new(replies, Duration::from_millis(1))))
    }
    #[cfg(not(feature = "sim"))]
    {
        gfn_luna::Bus::open(replies).map(|bus| Box::new(Luna(bus)) as Box<dyn Bus>)
    }
}

fn run(bus: Result<Box<dyn Bus>, String>, replies: &Receiver<Reply>, commands: &Receiver<Command>, updates: &Updates) {
    let mut worker = match bus {
        Ok(bus) => Worker::new(bus),
        Err(error) => {
            eprintln!("bluetooth: unavailable: {error}");
            let _ = updates.send(Update::Bluetooth(Snapshot { unavailable: Some(error), ..Snapshot::default() }));
            // Keep the commands channel alive so the UI can keep sending into it.
            while commands.recv().is_ok() {}
            return;
        }
    };
    let mut last = None;
    loop {
        match replies.recv_timeout(if worker.busy() { TICK } else { IDLE_TICK }) {
            Ok(reply) => worker.reply(reply),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        loop {
            match commands.try_recv() {
                Ok(command) => worker.command(command),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    worker.command(Command::Close);
                    return;
                }
            }
        }
        worker.housekeep(Instant::now());
        let snapshot = worker.snapshot();
        if last.as_ref() != Some(&snapshot) {
            let _ = updates.send(Update::Bluetooth(snapshot.clone()));
            last = Some(snapshot);
        }
    }
}

trait Bus: Send {
    fn call(&mut self, uri: &str, payload: Value) -> u64;
    fn cancel(&mut self, call: u64);
    /// Lets a scripted bus deliver what is due.
    fn tick(&mut self) {}
    /// Whether a scripted bus has replies waiting for `tick`.
    fn scripted_pending(&self) -> bool {
        false
    }
}

#[cfg(not(feature = "sim"))]
struct Luna(gfn_luna::Bus);

#[cfg(not(feature = "sim"))]
impl Bus for Luna {
    fn call(&mut self, uri: &str, payload: Value) -> u64 {
        self.0.call(uri, &payload)
    }

    fn cancel(&mut self, call: u64) {
        self.0.cancel(call);
    }
}

/// What the service last said about a device.
#[derive(Debug, Clone)]
struct Known {
    address: String,
    name: String,
    paired: bool,
    connected: bool,
    class: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpKind {
    Pairing,
    Connecting,
    Disconnecting,
}

#[derive(Debug, Clone)]
struct Op {
    kind: OpKind,
    call: u64,
    since: Instant,
    /// A background reconnect: its failure is not worth showing.
    quiet: bool,
}

#[derive(Debug, Clone)]
enum Purpose {
    Status,
    Pair(String),
    Connect(String),
    /// One reply, then retire the call; a failure is shown on `address` when there is one.
    Once { address: Option<String>, what: &'static str },
}

struct Worker {
    bus: Box<dyn Bus>,
    calls: HashMap<u64, Purpose>,
    known: Vec<Known>,
    ops: HashMap<String, Op>,
    failures: HashMap<String, String>,
    /// Paired from this screen: listed even when they do not look like controllers.
    paired_here: HashSet<String>,
    unavailable: Option<String>,
    searching: Option<Instant>,
    streaming: bool,
    last_reconnect: Instant,
}

impl Worker {
    fn busy(&self) -> bool {
        self.searching.is_some() || !self.ops.is_empty() || self.bus.scripted_pending()
    }

    fn new(mut bus: Box<dyn Bus>) -> Self {
        let status = bus.call(&format!("{SERVICE}/device/getStatus"), json!({ "subscribe": true }));
        Self {
            bus,
            calls: HashMap::from([(status, Purpose::Status)]),
            known: Vec::new(),
            ops: HashMap::new(),
            failures: HashMap::new(),
            paired_here: HashSet::new(),
            unavailable: None,
            searching: None,
            streaming: false,
            last_reconnect: Instant::now(),
        }
    }

    fn call(&mut self, method: &str, payload: Value, purpose: Purpose) -> u64 {
        let call = self.bus.call(&format!("{SERVICE}/{method}"), payload);
        self.calls.insert(call, purpose);
        call
    }

    fn once(&mut self, method: &str, address: Option<&str>, what: &'static str) {
        let payload = address.map_or_else(|| json!({}), |a| json!({ "address": a }));
        self.call(method, payload, Purpose::Once { address: address.map(str::to_owned), what });
    }

    fn retire(&mut self, call: u64) {
        self.calls.remove(&call);
        self.bus.cancel(call);
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Open | Command::Search => self.search(),
            Command::Close => self.stop_searching(),
            Command::Pair(address) => {
                // Discovery and pairing share the radio; inquiry scans slow pairing down.
                self.stop_searching();
                self.failures.remove(&address);
                let call = self.call("adapter/pair", json!({ "address": address, "subscribe": true }), Purpose::Pair(address.clone()));
                self.ops.insert(address, Op { kind: OpKind::Pairing, call, since: Instant::now(), quiet: false });
            }
            Command::Connect(address) => self.connect(address, false),
            Command::Disconnect(address) => {
                self.failures.remove(&address);
                self.once("hid/disconnect", Some(&address), "disconnect");
                self.ops.insert(address, Op { kind: OpKind::Disconnecting, call: 0, since: Instant::now(), quiet: false });
            }
            Command::Forget(address) => {
                self.failures.remove(&address);
                self.paired_here.remove(&address);
                if let Some(op) = self.ops.remove(&address) {
                    self.retire(op.call);
                }
                self.once("adapter/unpair", Some(&address), "forget");
            }
            Command::Streaming(streaming) => {
                self.streaming = streaming;
                self.last_reconnect = Instant::now();
            }
        }
    }

    fn search(&mut self) {
        self.once("adapter/startDiscovery", None, "search");
        self.searching = Some(Instant::now());
    }

    fn stop_searching(&mut self) {
        if self.searching.take().is_some() {
            self.once("adapter/cancelDiscovery", None, "stop searching");
        }
    }

    fn connect(&mut self, address: String, quiet: bool) {
        if !quiet {
            self.failures.remove(&address);
        }
        // Kept, never cancelled: dropping the subscription may count as letting go of the device.
        let call = self.call("hid/connect", json!({ "address": address, "subscribe": true }), Purpose::Connect(address.clone()));
        self.ops.insert(address, Op { kind: OpKind::Connecting, call, since: Instant::now(), quiet });
    }

    fn fail(&mut self, address: &str, message: String) {
        let quiet = self.ops.remove(address).is_some_and(|op| op.quiet);
        eprintln!("bluetooth: {address}: {message}");
        if !quiet {
            self.failures.insert(address.to_owned(), message);
        }
    }

    fn reply(&mut self, reply: Reply) {
        let Some(purpose) = self.calls.get(&reply.call).cloned() else { return };
        let payload = &reply.payload;
        let ok = payload["returnValue"].as_bool().unwrap_or(true);
        match purpose {
            Purpose::Status if !ok => {
                self.unavailable = Some(error_text(payload));
                self.retire(reply.call);
            }
            Purpose::Status => {
                if let Some(devices) = payload["devices"].as_array() {
                    self.known = devices.iter().filter_map(known).collect();
                    self.settle();
                }
            }
            Purpose::Pair(address) => self.pair_reply(reply.call, &address, payload),
            Purpose::Connect(address) => {
                if !ok && self.ops.get(&address).is_some_and(|op| op.call == reply.call) {
                    self.retire(reply.call);
                    self.fail(&address, error_text(payload));
                } else if !ok {
                    self.retire(reply.call);
                }
            }
            Purpose::Once { address, what } => {
                self.retire(reply.call);
                if ok {
                    return;
                }
                let message = format!("Couldn't {what}: {}", error_text(payload));
                match address {
                    Some(address) => self.fail(&address, message),
                    None => {
                        eprintln!("bluetooth: {message}");
                        if what == "search" {
                            self.searching = None;
                        }
                    }
                }
            }
        }
    }

    fn pair_reply(&mut self, call: u64, address: &str, payload: &Value) {
        let ok = payload["returnValue"].as_bool().unwrap_or(true);
        match payload["request"].as_str() {
            Some("endPairing") => {
                self.retire(call);
                if ok {
                    self.paired_here.insert(address.to_owned());
                    self.connect(address.to_owned(), false);
                } else {
                    self.fail(address, error_text(payload));
                }
            }
            Some(request @ ("displayPinCode" | "displayPasskey" | "confirmPasskey" | "confirmPassKey" | "enterPinCode" | "enterPasskey")) => {
                eprintln!("bluetooth: {address} asked for {request}");
                self.retire(call);
                self.once("adapter/cancelPairing", Some(address), "cancel pairing");
                self.fail(address, "It asks for a code, which this app can't enter. Pair it from the TV's settings instead.".into());
            }
            _ if !ok => {
                self.retire(call);
                self.fail(address, error_text(payload));
            }
            _ => {}
        }
    }

    /// Ends operations the device list shows are done.
    fn settle(&mut self) {
        let done: Vec<String> = self
            .ops
            .iter()
            .filter(|(address, op)| {
                let known = self.known.iter().find(|k| &k.address == *address);
                match op.kind {
                    OpKind::Pairing => false,
                    OpKind::Connecting => known.is_some_and(|k| k.connected),
                    OpKind::Disconnecting => known.is_none_or(|k| !k.connected),
                }
            })
            .map(|(address, _)| address.clone())
            .collect();
        for address in done {
            self.ops.remove(&address);
        }
        // A quiet reconnect can succeed after an earlier attempt failed.
        let known = &self.known;
        self.failures.retain(|address, _| !known.iter().any(|k| &k.address == address && k.connected));
    }

    fn housekeep(&mut self, now: Instant) {
        self.bus.tick();
        if self.searching.is_some_and(|since| now - since > SEARCH_FOR) {
            self.stop_searching();
        }
        let stale: Vec<(String, Op)> = self
            .ops
            .iter()
            .filter(|(_, op)| now - op.since > GIVE_UP_AFTER)
            .map(|(address, op)| (address.clone(), op.clone()))
            .collect();
        for (address, op) in stale {
            match op.kind {
                OpKind::Pairing => {
                    self.retire(op.call);
                    self.once("adapter/cancelPairing", Some(&address), "cancel pairing");
                    self.fail(&address, "Pairing timed out. Put the controller back in pairing mode and try again.".into());
                }
                OpKind::Connecting => {
                    self.retire(op.call);
                    self.fail(&address, "It didn't connect. Make sure it's on and in range.".into());
                }
                OpKind::Disconnecting => {
                    self.ops.remove(&address);
                }
            }
        }
        if !self.streaming && now - self.last_reconnect > RECONNECT_EVERY {
            self.last_reconnect = now;
            let idle: Vec<String> = self
                .known
                .iter()
                .filter(|k| k.paired && !k.connected && self.is_controller(k) && !self.ops.contains_key(&k.address))
                .map(|k| k.address.clone())
                .collect();
            for address in idle {
                self.connect(address, true);
            }
        }
    }

    fn is_controller(&self, known: &Known) -> bool {
        looks_like_controller(known.class, &known.name) || self.paired_here.contains(&known.address)
    }

    fn snapshot(&self) -> Snapshot {
        let mut paired = Vec::new();
        let mut nearby = Vec::new();
        for known in &self.known {
            let op = self.ops.get(&known.address).filter(|op| !op.quiet);
            let status = match (self.failures.get(&known.address), op.map(|op| op.kind)) {
                (_, Some(OpKind::Pairing)) => Status::Pairing,
                (_, Some(OpKind::Connecting)) => Status::Connecting,
                (_, Some(OpKind::Disconnecting)) => Status::Disconnecting,
                (Some(failure), None) => Status::Failed(failure.clone()),
                (None, None) if known.connected => Status::Connected,
                (None, None) if known.paired => Status::Paired,
                (None, None) => Status::Nearby,
            };
            let controller = self.is_controller(known);
            let device = Device { address: known.address.clone(), name: known.name.clone(), controller, status };
            if known.paired && controller {
                paired.push(device);
            } else if !known.paired && !known.name.is_empty() {
                nearby.push(device);
            }
        }
        paired.sort_by(|a, b| (b.status == Status::Connected).cmp(&(a.status == Status::Connected)).then_with(|| a.name.cmp(&b.name)));
        nearby.sort_by(|a, b| b.controller.cmp(&a.controller).then_with(|| a.name.cmp(&b.name)));
        Snapshot { unavailable: self.unavailable.clone(), searching: self.searching.is_some(), paired, nearby }
    }
}

fn known(device: &Value) -> Option<Known> {
    let address = device["address"].as_str()?.to_owned();
    let connected = device["connectedProfiles"].as_array().is_some_and(|p| p.iter().any(|p| p == "hid"));
    Some(Known {
        name: device["name"].as_str().unwrap_or_default().to_owned(),
        paired: device["paired"].as_bool().unwrap_or(false),
        connected,
        class: device["classOfDevice"].as_u64().unwrap_or(0) as u32,
        address,
    })
}

fn error_text(payload: &Value) -> String {
    match (payload["errorText"].as_str(), payload["errorCode"].as_i64()) {
        (Some(text), _) => text.to_owned(),
        (None, Some(code)) => format!("error {code}"),
        (None, None) => "the Bluetooth service refused".into(),
    }
}

/// Classic Bluetooth announces gamepads in the class of device (major class peripheral, minor
/// joystick or gamepad); BLE controllers often send no class, so the name decides for them.
fn looks_like_controller(class: u32, name: &str) -> bool {
    let major = (class >> 8) & 0x1f;
    let minor = (class >> 2) & 0x0f;
    if major == 0x05 && matches!(minor, 1 | 2) {
        return true;
    }
    let name = name.to_lowercase();
    ["controller", "gamepad", "xbox", "8bitdo", "dualsense", "dualshock", "joy-con", "stadia"]
        .iter()
        .any(|word| name.contains(word))
}

#[cfg(any(test, feature = "sim"))]
mod fake {
    //! Plays `com.webos.service.bluetooth2` for the simulator and the tests: a remote and a
    //! soundbar already paired (never listed), and devices that turn up once a search starts.

    use super::*;

    struct Device {
        address: &'static str,
        name: &'static str,
        class: u32,
        paired: bool,
        connected: bool,
        visible: bool,
        /// Pairing fails with this.
        refuses: Option<&'static str>,
        /// Switched off: connecting times out.
        off: bool,
    }

    enum Event {
        Found(usize),
        Paired { call: u64, device: usize },
        Connected { call: u64, device: usize },
        Disconnected(usize),
    }

    pub struct Fake {
        replies: Sender<Reply>,
        unit: Duration,
        next: u64,
        status: Option<u64>,
        devices: Vec<Device>,
        due: Vec<(Instant, Event)>,
    }

    impl Fake {
        /// Delays in the script are counted in `unit`s of a millisecond's worth at 1 ms.
        pub fn new(replies: Sender<Reply>, unit: Duration) -> Self {
            let device = |address, name, class, paired, connected, visible| Device {
                address,
                name,
                class,
                paired,
                connected,
                visible,
                refuses: None,
                off: false,
            };
            let mut devices = vec![
                device("AA:00:00:00:00:01", "LG Magic Remote", 0x00_250c, true, true, true),
                device("AA:00:00:00:00:02", "LG SK6F", 0x24_0414, true, false, true),
                device("AA:00:00:00:00:03", "8BitDo Pro 2", 0x00_2508, true, false, true),
                device("AA:00:00:00:00:04", "Xbox Wireless Controller", 0x00_2508, false, false, false),
                device("AA:00:00:00:00:05", "DualSense Wireless Controller", 0x00_2508, false, false, false),
                device("AA:00:00:00:00:06", "Pixel 8", 0x5a_020c, false, false, false),
            ];
            devices[2].off = true;
            devices[5].refuses = Some("Authentication Rejected");
            Self { replies, unit, next: 1, status: None, devices, due: Vec::new() }
        }

        fn send(&self, call: u64, payload: Value) {
            let _ = self.replies.send(Reply { call, payload });
        }

        fn after(&mut self, units: u32, event: Event) {
            self.due.push((Instant::now() + self.unit * units, event));
        }

        fn push_status(&self) {
            let Some(call) = self.status else { return };
            let devices: Vec<Value> = self
                .devices
                .iter()
                .filter(|d| d.visible)
                .map(|d| {
                    json!({
                        "address": d.address,
                        "name": d.name,
                        "classOfDevice": d.class,
                        "paired": d.paired,
                        "connectedProfiles": if d.connected { vec!["hid"] } else { vec![] },
                    })
                })
                .collect();
            self.send(call, json!({ "returnValue": true, "devices": devices }));
        }

        fn index(&self, payload: &Value) -> Option<usize> {
            let address = payload["address"].as_str()?;
            self.devices.iter().position(|d| d.address == address)
        }
    }

    impl Bus for Fake {
        fn call(&mut self, uri: &str, payload: Value) -> u64 {
            let call = self.next;
            self.next += 1;
            let method = uri.strip_prefix(&format!("{SERVICE}/")).unwrap_or(uri);
            let device = self.index(&payload);
            match (method, device) {
                ("device/getStatus", _) => {
                    self.status = Some(call);
                    self.push_status();
                }
                ("adapter/startDiscovery", _) => {
                    self.send(call, json!({ "returnValue": true }));
                    for (units, index) in [(1500, 3), (3000, 4), (4000, 5)] {
                        self.after(units, Event::Found(index));
                    }
                }
                ("adapter/pair", Some(index)) => {
                    self.send(call, json!({ "returnValue": true, "subscribed": true }));
                    self.after(2000, Event::Paired { call, device: index });
                }
                ("hid/connect", Some(index)) => {
                    self.send(call, json!({ "returnValue": true, "subscribed": true }));
                    if !self.devices[index].off {
                        self.after(1000, Event::Connected { call, device: index });
                    }
                }
                ("hid/disconnect", Some(index)) => {
                    self.send(call, json!({ "returnValue": true }));
                    self.after(500, Event::Disconnected(index));
                }
                ("adapter/unpair", Some(index)) => {
                    self.devices[index].paired = false;
                    self.devices[index].connected = false;
                    self.send(call, json!({ "returnValue": true }));
                    self.push_status();
                }
                _ => self.send(call, json!({ "returnValue": true })),
            }
            call
        }

        fn cancel(&mut self, _: u64) {}

        fn scripted_pending(&self) -> bool {
            !self.due.is_empty()
        }

        fn tick(&mut self) {
            let now = Instant::now();
            let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.due).into_iter().partition(|(at, _)| *at <= now);
            self.due = later;
            for (_, event) in due {
                match event {
                    Event::Found(index) => self.devices[index].visible = true,
                    Event::Paired { call, device } => match self.devices[device].refuses {
                        Some(error) => {
                            self.send(call, json!({ "request": "endPairing", "returnValue": false, "errorText": error }));
                        }
                        None => {
                            self.devices[device].paired = true;
                            self.send(call, json!({ "request": "endPairing", "returnValue": true }));
                        }
                    },
                    Event::Connected { call, device } => {
                        self.devices[device].connected = true;
                        self.send(call, json!({ "returnValue": true, "connected": true }));
                    }
                    Event::Disconnected(index) => self.devices[index].connected = false,
                }
                self.push_status();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker() -> (Worker, Receiver<Reply>) {
        let (tx, rx) = channel();
        (Worker::new(Box::new(fake::Fake::new(tx, Duration::from_micros(1)))), rx)
    }

    /// Runs the worker until `done` holds or a second passes.
    fn until(worker: &mut Worker, replies: &Receiver<Reply>, done: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            while let Ok(reply) = replies.try_recv() {
                worker.reply(reply);
            }
            worker.housekeep(Instant::now());
            let snapshot = worker.snapshot();
            if done(&snapshot) || Instant::now() > deadline {
                return snapshot;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn status_of(snapshot: &Snapshot, name: &str) -> Option<Status> {
        snapshot.paired.iter().chain(&snapshot.nearby).find(|d| d.name == name).map(|d| d.status.clone())
    }

    #[test]
    fn lists_paired_controllers_only_and_finds_new_ones_by_searching() {
        let (mut worker, replies) = worker();
        let snapshot = until(&mut worker, &replies, |s| !s.paired.is_empty());
        let names: Vec<&str> = snapshot.paired.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["8BitDo Pro 2"]);
        worker.command(Command::Open);
        let snapshot = until(&mut worker, &replies, |s| s.nearby.len() == 3);
        assert!(snapshot.searching);
        let names: Vec<&str> = snapshot.nearby.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["DualSense Wireless Controller", "Xbox Wireless Controller", "Pixel 8"]);
    }

    #[test]
    fn pairing_connects_and_a_refusal_is_shown() {
        let (mut worker, replies) = worker();
        worker.command(Command::Open);
        until(&mut worker, &replies, |s| s.nearby.len() == 3);
        worker.command(Command::Pair("AA:00:00:00:00:04".into()));
        assert_eq!(status_of(&worker.snapshot(), "Xbox Wireless Controller"), Some(Status::Pairing));
        assert!(!worker.snapshot().searching, "pairing stops the search");
        let snapshot = until(&mut worker, &replies, |s| s.connected() == 1);
        assert_eq!(status_of(&snapshot, "Xbox Wireless Controller"), Some(Status::Connected));

        worker.command(Command::Pair("AA:00:00:00:00:06".into()));
        let snapshot = until(&mut worker, &replies, |s| matches!(status_of(s, "Pixel 8"), Some(Status::Failed(_))));
        assert_eq!(status_of(&snapshot, "Pixel 8"), Some(Status::Failed("Authentication Rejected".into())));
    }

    #[test]
    fn disconnect_and_forget() {
        let (mut worker, replies) = worker();
        worker.command(Command::Open);
        until(&mut worker, &replies, |s| s.nearby.len() == 3);
        worker.command(Command::Pair("AA:00:00:00:00:05".into()));
        until(&mut worker, &replies, |s| s.connected() == 1);
        worker.command(Command::Disconnect("AA:00:00:00:00:05".into()));
        let snapshot = until(&mut worker, &replies, |s| s.connected() == 0 && !matches!(status_of(s, "DualSense Wireless Controller"), Some(Status::Disconnecting)));
        assert_eq!(status_of(&snapshot, "DualSense Wireless Controller"), Some(Status::Paired));
        worker.command(Command::Forget("AA:00:00:00:00:05".into()));
        let snapshot = until(&mut worker, &replies, |s| s.paired.len() == 1);
        assert_eq!(status_of(&snapshot, "DualSense Wireless Controller"), Some(Status::Nearby));
    }

    #[test]
    fn background_reconnects_fail_silently() {
        let (mut worker, replies) = worker();
        until(&mut worker, &replies, |s| !s.paired.is_empty());
        worker.housekeep(Instant::now() + RECONNECT_EVERY * 2);
        assert_eq!(status_of(&worker.snapshot(), "8BitDo Pro 2"), Some(Status::Paired));
        worker.housekeep(Instant::now() + RECONNECT_EVERY * 2 + GIVE_UP_AFTER * 2);
        assert_eq!(status_of(&worker.snapshot(), "8BitDo Pro 2"), Some(Status::Paired));
    }

    #[test]
    fn controller_detection() {
        assert!(looks_like_controller(0x00_2508, "Whatever"));
        assert!(looks_like_controller(0, "Xbox Wireless Controller"));
        assert!(!looks_like_controller(0x00_250c, "LG Magic Remote"));
        assert!(!looks_like_controller(0x24_0414, "LG SK6F"));
    }
}
