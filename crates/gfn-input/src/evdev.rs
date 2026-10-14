//! Controllers read straight from `/dev/input/event*`, for the TV: its SDL does not report
//! Bluetooth controllers, but once paired and connected (gfn-app's Bluetooth screen) they are
//! ordinary evdev devices, readable because native apps run in the `compositor` group.
//!
//! Events come out in SDL's `GameController` numbering so the rest of the input path is shared
//! with the desktop. Which evdev code is which button depends on the kernel driver; the layouts
//! below cover hid-generic (Xbox, 8BitDo) and both generations of hid-sony.

use std::sync::mpsc::{Receiver, channel};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PadEvent {
    Added { id: i32, name: String },
    Removed { id: i32 },
    /// `SDL_GameControllerButton` numbering.
    Button { id: i32, button: u8, pressed: bool },
    /// `SDL_GameControllerAxis` numbering.
    Axis { id: i32, axis: u8, value: i16 },
}

/// Background reader of every connected controller. Inert off Linux.
pub struct Pads {
    events: Receiver<PadEvent>,
}

impl Pads {
    /// `wake` is called from the reader thread after each batch of events, to rouse a main loop
    /// that sleeps waiting for input.
    pub fn spawn(wake: impl Fn() + Send + 'static) -> Self {
        let (sender, events) = channel();
        #[cfg(target_os = "linux")]
        std::thread::Builder::new()
            .name("evdev-pads".into())
            .spawn(move || linux::run(&sender, &wake))
            .expect("spawning the controller reader");
        #[cfg(not(target_os = "linux"))]
        let _ = (sender, wake);
        Self { events }
    }

    pub fn try_recv(&self) -> Option<PadEvent> {
        self.events.try_recv().ok()
    }
}

#[cfg(any(target_os = "linux", test))]
mod layout {
    //! Evdev codes to SDL controller buttons and axes, chosen from the device's capabilities.

    use std::collections::HashMap;

    pub const BTN_TRIGGER: u16 = 0x120;
    pub const BTN_SOUTH: u16 = 0x130;
    const BTN_C: u16 = 0x132;
    pub const ABS_X: u16 = 0x00;
    pub const ABS_Y: u16 = 0x01;
    const KEY_A: u16 = 0x1e;
    const ABS_Z: u16 = 0x02;
    const ABS_RX: u16 = 0x03;
    const ABS_RY: u16 = 0x04;
    const ABS_RZ: u16 = 0x05;
    const ABS_GAS: u16 = 0x09;
    const ABS_BRAKE: u16 = 0x0a;
    pub const ABS_HAT0X: u16 = 0x10;
    pub const ABS_HAT0Y: u16 = 0x11;
    const VENDOR_SONY: u16 = 0x054c;

    pub const SDL_UP: u8 = 11;
    pub const SDL_DOWN: u8 = 12;
    pub const SDL_LEFT: u8 = 13;
    pub const SDL_RIGHT: u8 = 14;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Target {
        /// SDL axes 0-3.
        Stick(u8),
        /// SDL axes 4-5.
        Trigger(u8),
        HatX,
        HatY,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Layout {
        pub name: &'static str,
        pub buttons: HashMap<u16, u8>,
        pub axes: HashMap<u16, Target>,
        /// Digital trigger buttons, for controllers without analogue triggers.
        pub trigger_buttons: HashMap<u16, u8>,
    }

    /// `keys` and `abs` are the codes the device reports.
    pub fn choose(vendor: u16, keys: &[u16], abs: &[u16]) -> Layout {
        let has_key = |code| keys.contains(&code);
        let has_abs = |code| abs.contains(&code);
        // hid-sony before Linux 4.10 left the DualShock to hid-generic: buttons in HID order,
        // square first. Newer hid-sony follows the gamepad spec (no BTN_C).
        let sony_generic = vendor == VENDOR_SONY && has_key(BTN_C);
        let (name, buttons): (&'static str, &[(u16, u8)]) = if sony_generic {
            ("sony-generic", &[(0x130, 2), (0x131, 0), (0x132, 1), (0x133, 3), (0x134, 9), (0x135, 10), (0x138, 4), (0x139, 6), (0x13a, 7), (0x13b, 8), (0x13c, 5)])
        } else if vendor == VENDOR_SONY {
            ("sony", &[(0x130, 0), (0x131, 1), (0x133, 3), (0x134, 2), (0x136, 9), (0x137, 10), (0x13a, 4), (0x13b, 6), (0x13c, 5), (0x13d, 7), (0x13e, 8)])
        } else {
            // Xbox controllers over Bluetooth on older firmware send View and Xbox as consumer
            // keys (KEY_BACK, KEY_HOMEPAGE).
            ("xbox", &[(0x130, 0), (0x131, 1), (0x133, 2), (0x134, 3), (0x136, 9), (0x137, 10), (0x13a, 4), (0x13b, 6), (0x13c, 5), (0x13d, 7), (0x13e, 8), (0x9e, 4), (0xac, 5)])
        };
        let mut buttons: HashMap<u16, u8> = buttons.iter().copied().collect();
        buttons.extend([(0x220, SDL_UP), (0x221, SDL_DOWN), (0x222, SDL_LEFT), (0x223, SDL_RIGHT)]);

        let mut axes = HashMap::from([(ABS_X, Target::Stick(0)), (ABS_Y, Target::Stick(1)), (ABS_HAT0X, Target::HatX), (ABS_HAT0Y, Target::HatY)]);
        let (right, triggers) = if has_abs(ABS_GAS) || has_abs(ABS_BRAKE) {
            ((ABS_Z, ABS_RZ), Some((ABS_BRAKE, ABS_GAS)))
        } else if sony_generic {
            ((ABS_Z, ABS_RZ), Some((ABS_RX, ABS_RY)))
        } else if has_abs(ABS_RX) && has_abs(ABS_RY) {
            ((ABS_RX, ABS_RY), Some((ABS_Z, ABS_RZ)))
        } else {
            ((ABS_Z, ABS_RZ), None)
        };
        axes.insert(right.0, Target::Stick(2));
        axes.insert(right.1, Target::Stick(3));
        let mut trigger_buttons = HashMap::new();
        match triggers {
            Some((left, right)) if has_abs(left) || has_abs(right) => {
                axes.insert(left, Target::Trigger(4));
                axes.insert(right, Target::Trigger(5));
            }
            _ => {
                let (left, right) = if sony_generic { (0x136, 0x137) } else { (0x138, 0x139) };
                trigger_buttons.extend([(left, 4), (right, 5)]);
            }
        }
        axes.retain(|code, _| has_abs(*code));
        Layout { name, buttons, axes, trigger_buttons }
    }

    /// Gamepads and joysticks: a stick and a fire button. LG's remote and its virtual input
    /// devices declare every key code including the gamepad ones, but no stick and also letters.
    pub fn is_controller(keys: &[u16], abs: &[u16]) -> bool {
        let has_stick = abs.contains(&ABS_X) && abs.contains(&ABS_Y);
        let has_fire = keys.contains(&BTN_SOUTH) || keys.contains(&BTN_TRIGGER);
        has_stick && has_fire && !keys.contains(&KEY_A)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const XBOX_KEYS: &[u16] = &[0x130, 0x131, 0x132, 0x133, 0x134, 0x135, 0x136, 0x137, 0x13a, 0x13b, 0x13c, 0x13d, 0x13e, 0x9e];
        const XBOX_ABS: &[u16] = &[0x00, 0x01, 0x02, 0x05, 0x09, 0x0a, 0x10, 0x11];

        #[test]
        fn xbox_over_bluetooth() {
            let layout = choose(0x045e, XBOX_KEYS, XBOX_ABS);
            assert_eq!(layout.name, "xbox");
            assert_eq!((layout.buttons[&0x133], layout.buttons[&0x134], layout.buttons[&0x9e]), (2, 3, 4));
            assert_eq!(layout.axes[&ABS_RZ], Target::Stick(3));
            assert_eq!(layout.axes[&ABS_BRAKE], Target::Trigger(4));
            assert!(layout.trigger_buttons.is_empty());
        }

        #[test]
        fn dualshock_on_old_and_new_kernels() {
            let old = choose(VENDOR_SONY, &[0x130, 0x131, 0x132, 0x133, 0x134, 0x135, 0x136, 0x137, 0x138, 0x139, 0x13a, 0x13b, 0x13c, 0x13d], &[0, 1, 2, 3, 4, 5, 0x10, 0x11]);
            assert_eq!((old.name, old.buttons[&0x131], old.buttons[&0x130]), ("sony-generic", 0, 2));
            assert_eq!((old.axes[&ABS_Z], old.axes[&ABS_RX]), (Target::Stick(2), Target::Trigger(4)));
            let new = choose(VENDOR_SONY, &[0x130, 0x131, 0x133, 0x134, 0x136, 0x137, 0x138, 0x139, 0x13a, 0x13b, 0x13c, 0x13d, 0x13e], &[0, 1, 2, 3, 4, 5, 0x10, 0x11]);
            assert_eq!((new.name, new.buttons[&0x134], new.buttons[&0x133]), ("sony", 2, 3));
            assert_eq!((new.axes[&ABS_RX], new.axes[&ABS_Z]), (Target::Stick(2), Target::Trigger(4)));
        }

        #[test]
        fn digital_triggers_without_analogue_ones() {
            let layout = choose(0x2dc8, &[0x130, 0x131, 0x133, 0x134, 0x138, 0x139], &[0, 1, 2, 5]);
            assert_eq!(layout.trigger_buttons[&0x138], 4);
            assert_eq!(layout.axes.get(&ABS_GAS), None);
        }

        #[test]
        fn only_controllers() {
            assert!(is_controller(XBOX_KEYS, XBOX_ABS));
            assert!(!is_controller(&[0x110, 0x111, 0x1c], &[]));
            assert!(!is_controller(&[0x14a], &[0x00, 0x01]));
            let everything: Vec<u16> = (1..=0x2ff).collect();
            assert!(!is_controller(&everything, &[]), "LG remote");
            assert!(!is_controller(&everything, &[0x00, 0x01]));
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::{HashMap, HashSet};
    use std::ffi::CString;
    use std::sync::mpsc::Sender;
    use std::time::{Duration, Instant};

    use super::PadEvent;
    use super::layout::{self, Layout, Target};

    const EV_SYN: u16 = 0x00;
    const EV_KEY: u16 = 0x01;
    const EV_ABS: u16 = 0x03;
    const SYN_REPORT: u16 = 0;
    const KEY_MAX: usize = 0x2ff;
    const ABS_MAX: usize = 0x3f;
    const RESCAN_EVERY: Duration = Duration::from_secs(1);
    const FIRST_ID: i32 = 1_000;

    const fn ioc_read(nr: u32, size: usize) -> u32 {
        (2 << 30) | ((size as u32) << 16) | ((b'E' as u32) << 8) | nr
    }
    const EVIOCGID: u32 = ioc_read(0x02, 8);
    const fn eviocgname(len: usize) -> u32 {
        ioc_read(0x06, len)
    }
    const fn eviocgbit(ev: u16, len: usize) -> u32 {
        ioc_read(0x20 + ev as u32, len)
    }
    const fn eviocgabs(abs: u16) -> u32 {
        ioc_read(0x40 + abs as u32, std::mem::size_of::<libc::input_absinfo>())
    }
    /// `_IOW('E', 0x90, int)`
    const EVIOCGRAB: u32 = (1 << 30) | (4 << 16) | ((b'E' as u32) << 8) | 0x90;

    struct Pad {
        id: i32,
        fd: i32,
        node: String,
        layout: Layout,
        /// (min, max) per mapped axis.
        ranges: HashMap<u16, (i32, i32)>,
        hat: (i32, i32),
    }

    impl Drop for Pad {
        fn drop(&mut self) {
            unsafe { libc::close(self.fd) };
        }
    }

    pub fn run(sender: &Sender<PadEvent>, wake: &dyn Fn()) {
        let mut pads: Vec<Pad> = Vec::new();
        // Nodes looked at and found not to be controllers (or unreadable), until they go away.
        let mut skipped: HashSet<String> = HashSet::new();
        let mut next_id = FIRST_ID;
        let mut last_scan: Option<Instant> = None;
        loop {
            if last_scan.is_none_or(|at| at.elapsed() >= RESCAN_EVERY) {
                last_scan = Some(Instant::now());
                let nodes = event_nodes();
                skipped.retain(|node| nodes.contains(node));
                for node in nodes {
                    if skipped.contains(&node) || pads.iter().any(|p| p.node == node) {
                        continue;
                    }
                    match open(&node, next_id) {
                        Some(pad) => {
                            let _ = sender.send(PadEvent::Added { id: pad.id, name: device_name(pad.fd) });
                            next_id += 1;
                            pads.push(pad);
                            wake();
                        }
                        None => {
                            skipped.insert(node);
                        }
                    }
                }
            }
            if pads.is_empty() {
                std::thread::sleep(RESCAN_EVERY);
                continue;
            }
            let mut polled: Vec<libc::pollfd> = pads.iter().map(|p| libc::pollfd { fd: p.fd, events: libc::POLLIN, revents: 0 }).collect();
            let ready = unsafe { libc::poll(polled.as_mut_ptr(), polled.len() as libc::nfds_t, RESCAN_EVERY.as_millis() as i32) };
            if ready <= 0 {
                continue;
            }
            let mut gone = Vec::new();
            let mut woke = false;
            for (index, polled) in polled.iter().enumerate() {
                if polled.revents == 0 {
                    continue;
                }
                let pad = &mut pads[index];
                if polled.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 || !drain(pad, sender) {
                    gone.push(index);
                }
                woke = true;
            }
            for index in gone.into_iter().rev() {
                let pad = pads.remove(index);
                eprintln!("evdev: {} removed", pad.node);
                let _ = sender.send(PadEvent::Removed { id: pad.id });
            }
            if woke {
                wake();
            }
        }
    }

    /// Nodes with a device behind them. The TV's `/dev/input` is static, so listing it would
    /// also give nodes that only answer ENXIO until something connects.
    fn event_nodes() -> HashSet<String> {
        std::fs::read_to_string("/proc/bus/input/devices")
            .map(|devices| {
                devices
                    .lines()
                    .filter_map(|line| line.strip_prefix("H: Handlers="))
                    .flat_map(str::split_whitespace)
                    .filter(|handler| handler.starts_with("event"))
                    .map(|handler| format!("/dev/input/{handler}"))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn bits(fd: i32, ev: u16, max: usize) -> Vec<u16> {
        let mut buffer = vec![0_u8; max / 8 + 1];
        let length = unsafe { libc::ioctl(fd, eviocgbit(ev, buffer.len()) as _, buffer.as_mut_ptr()) };
        if length < 0 {
            return Vec::new();
        }
        (0..buffer.len() * 8).filter(|&bit| buffer[bit / 8] & (1 << (bit % 8)) != 0).map(|bit| bit as u16).collect()
    }

    fn device_name(fd: i32) -> String {
        let mut buffer = [0_u8; 256];
        let length = unsafe { libc::ioctl(fd, eviocgname(buffer.len()) as _, buffer.as_mut_ptr()) };
        if length <= 0 {
            return "Controller".into();
        }
        let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
        String::from_utf8_lossy(&buffer[..end]).into_owned()
    }

    fn open(node: &str, id: i32) -> Option<Pad> {
        let path = CString::new(node).ok()?;
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if fd < 0 {
            eprintln!("evdev: {node}: {}", std::io::Error::last_os_error());
            return None;
        }
        let keys = bits(fd, EV_KEY, KEY_MAX);
        let abs = bits(fd, EV_ABS, ABS_MAX);
        let name = device_name(fd);
        if !layout::is_controller(&keys, &abs) {
            unsafe { libc::close(fd) };
            return None;
        }
        let mut ids = [0_u16; 4];
        unsafe { libc::ioctl(fd, EVIOCGID as _, ids.as_mut_ptr()) };
        let (vendor, product) = (ids[1], ids[2]);
        let layout = layout::choose(vendor, &keys, &abs);
        let mut ranges = HashMap::new();
        for &code in layout.axes.keys() {
            let mut info: libc::input_absinfo = unsafe { std::mem::zeroed() };
            if unsafe { libc::ioctl(fd, eviocgabs(code) as _, &mut info) } >= 0 {
                ranges.insert(code, (info.minimum, info.maximum));
            }
        }
        // Without the grab the TV's own input handling may also act on the controller (as
        // arrow keys and OK), which would reach a game twice.
        let grabbed = unsafe { libc::ioctl(fd, EVIOCGRAB as _, 1 as libc::c_int) } == 0;
        eprintln!(
            "evdev: {node} \"{name}\" {vendor:04x}:{product:04x} layout={} grabbed={grabbed} keys={:x?} abs={:x?} ranges={:?}",
            layout.name, keys, abs, ranges
        );
        Some(Pad { id, fd, node: node.to_owned(), layout, ranges, hat: (0, 0) })
    }

    /// Reads what is queued; false once the device is gone.
    fn drain(pad: &mut Pad, sender: &Sender<PadEvent>) -> bool {
        const SIZE: usize = std::mem::size_of::<libc::input_event>();
        let mut events: [libc::input_event; 64] = unsafe { std::mem::zeroed() };
        loop {
            let read = unsafe { libc::read(pad.fd, events.as_mut_ptr().cast(), SIZE * events.len()) };
            if read < 0 {
                return std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock;
            }
            if read == 0 {
                return false;
            }
            for event in &events[..read as usize / SIZE] {
                translate(pad, event.type_, event.code, event.value, sender);
            }
        }
    }

    fn translate(pad: &mut Pad, kind: u16, code: u16, value: i32, sender: &Sender<PadEvent>) {
        let id = pad.id;
        match kind {
            EV_SYN if code == SYN_REPORT => return,
            EV_KEY => {
                if value == 2 {
                    return;
                }
                if let Some(&button) = pad.layout.buttons.get(&code) {
                    let _ = sender.send(PadEvent::Button { id, button, pressed: value != 0 });
                } else if let Some(&axis) = pad.layout.trigger_buttons.get(&code) {
                    let _ = sender.send(PadEvent::Axis { id, axis, value: if value != 0 { i16::MAX } else { 0 } });
                }
            }
            EV_ABS => {
                let Some(&target) = pad.layout.axes.get(&code) else { return };
                let (min, max) = pad.ranges.get(&code).copied().unwrap_or((-32768, 32767));
                match target {
                    Target::Stick(axis) => {
                        let _ = sender.send(PadEvent::Axis { id, axis, value: stick(value, min, max) });
                    }
                    Target::Trigger(axis) => {
                        let _ = sender.send(PadEvent::Axis { id, axis, value: trigger(value, min, max) });
                    }
                    Target::HatX => {
                        let old = std::mem::replace(&mut pad.hat.0, value.signum());
                        hat(id, old, value.signum(), layout::SDL_LEFT, layout::SDL_RIGHT, sender);
                    }
                    Target::HatY => {
                        let old = std::mem::replace(&mut pad.hat.1, value.signum());
                        hat(id, old, value.signum(), layout::SDL_UP, layout::SDL_DOWN, sender);
                    }
                }
            }
            _ => {}
        }
    }

    fn hat(id: i32, old: i32, new: i32, negative: u8, positive: u8, sender: &Sender<PadEvent>) {
        if old == new {
            return;
        }
        let button = |direction| if direction < 0 { negative } else { positive };
        if old != 0 {
            let _ = sender.send(PadEvent::Button { id, button: button(old), pressed: false });
        }
        if new != 0 {
            let _ = sender.send(PadEvent::Button { id, button: button(new), pressed: true });
        }
    }

    pub(super) fn stick(value: i32, min: i32, max: i32) -> i16 {
        if max <= min {
            return 0;
        }
        let (min, max, value) = (min as f32, max as f32, value as f32);
        let centre = (min + max) / 2.0;
        (((value - centre) / ((max - min) / 2.0)).clamp(-1.0, 1.0) * 32767.0).round() as i16
    }

    pub(super) fn trigger(value: i32, min: i32, max: i32) -> i16 {
        if max <= min {
            return 0;
        }
        (((value - min) as f32 / (max - min) as f32).clamp(0.0, 1.0) * 32767.0).round() as i16
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn ranges_are_normalised() {
            assert_eq!(stick(0, 0, 65535), -32767);
            assert_eq!(stick(65535, 0, 65535), 32767);
            assert_eq!(stick(128, 0, 255).abs() < 200, true);
            assert_eq!(trigger(1023, 0, 1023), 32767);
            assert_eq!(trigger(0, 0, 255), 0);
        }
    }
}
