//! Turns SDL input into the engine's captured input, following OpenNOW's desktop capture
//! (MIT, opennow-streamer-platform `SdlInputCapture`): Windows virtual keys, ×120 wheel steps,
//! XInput button masks, a radial stick deadzone and a 100 ms controller keepalive.
//!
//! The mouse mode follows the game's cursor: when the server hides it the pointer is locked and
//! reports relative motion; when it is visible the pointer stays free and positions are sent as
//! absolute coordinates inside the letterboxed picture.

pub mod evdev;
mod gamepad;
mod keymap;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gfn_sdl::InputEvent;
use opennow_streamer_platform_webos::{CapturedInput, CapturedInputQueue, RemoteCursor, RemoteCursorState};
use opennow_streamer_protocol::text_input::TextInputError;

use gamepad::Gamepads;

const KEEPALIVE: Duration = Duration::from_millis(100);
const VK_LSHIFT: u16 = 0xa0;
const NVST_SHIFT: u16 = 0x01;

/// On-screen keyboard output waiting to go out in order. The core carries one Unicode text at a
/// time, so text queues here while the previous one is still being sent, and the editing keys
/// typed after it wait behind it.
#[derive(Debug)]
enum Typed {
    Text(String),
    Key(u16),
}
const WHEEL_STEP: i32 = 120;

/// What the embedder should do with the local pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pointer {
    /// Input is not captured: normal desktop pointer.
    Free,
    /// Locked and hidden, raw motion only.
    Relative,
    /// Visible with this `SDL_SystemCursor` shape; positions go to the game as absolute.
    Absolute { shape: u8 },
}

pub struct InputCapture {
    queue: Arc<CapturedInputQueue>,
    cursor: Arc<RemoteCursor>,
    /// Whether the user has handed input to the stream. Always true on the TV, where there is
    /// no desktop to give the pointer back to.
    captured: bool,
    always_captured: bool,
    focused: bool,
    stream_size: (u32, u32),
    pressed_keys: HashMap<i32, u16>,
    pressed_buttons: u8,
    /// A Ctrl+Alt release swallows the matching key-ups so the game never sees half a chord.
    swallowed: Vec<i32>,
    gamepads: Gamepads,
    last_keepalive: Instant,
    typing: VecDeque<Typed>,
}

impl InputCapture {
    pub fn new(queue: Arc<CapturedInputQueue>, cursor: Arc<RemoteCursor>, always_captured: bool) -> Self {
        Self {
            queue,
            cursor,
            captured: always_captured,
            always_captured,
            focused: always_captured,
            stream_size: (1920, 1080),
            pressed_keys: HashMap::new(),
            pressed_buttons: 0,
            swallowed: Vec::new(),
            gamepads: Gamepads::default(),
            last_keepalive: Instant::now(),
            typing: VecDeque::new(),
        }
    }

    pub fn set_stream_size(&mut self, width: u32, height: u32) {
        self.stream_size = (width.max(1), height.max(1));
    }

    pub fn is_captured(&self) -> bool {
        self.captured && self.focused
    }

    pub fn pointer(&self) -> Pointer {
        if !self.is_captured() {
            return Pointer::Free;
        }
        match self.cursor.state() {
            RemoteCursorState::Hidden => Pointer::Relative,
            RemoteCursorState::Predefined(id) => Pointer::Absolute { shape: system_cursor(id) },
            RemoteCursorState::Unknown | RemoteCursorState::Custom(_) => {
                Pointer::Absolute { shape: gfn_sdl::SystemCursors::ARROW }
            }
        }
    }

    /// Text from the on-screen keyboard, for the game to receive as typed characters.
    pub fn type_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.typing.back_mut() {
            Some(Typed::Text(pending)) => pending.push_str(text),
            _ => self.typing.push_back(Typed::Text(text.to_owned())),
        }
        self.flush_typing();
    }

    /// An editing key (Backspace, Enter, ...) pressed on the on-screen keyboard, kept in order
    /// with the text around it.
    pub fn type_key(&mut self, scancode: i32) {
        if let Some(virtual_key) = keymap::virtual_key(scancode) {
            self.typing.push_back(Typed::Key(virtual_key));
            self.flush_typing();
        }
    }

    fn flush_typing(&mut self) {
        while let Some(next) = self.typing.front() {
            match next {
                Typed::Text(text) => match self.queue.submit_text(text.as_bytes()) {
                    Err(TextInputError::Busy) => return,
                    Err(TextInputError::Unavailable) => {
                        let text = text.clone();
                        self.type_as_keys(&text);
                    }
                    Ok(()) | Err(TextInputError::Invalid | TextInputError::TooLarge) => {}
                },
                Typed::Key(virtual_key) => self.tap(*virtual_key, 0),
            }
            self.typing.pop_front();
        }
    }

    /// For a server that takes no Unicode text: what a US keyboard can type, as key presses.
    fn type_as_keys(&self, text: &str) {
        for (virtual_key, shift) in text.chars().filter_map(keymap::us_key) {
            if shift {
                self.queue.push(CapturedInput::Key { virtual_key: VK_LSHIFT, modifiers: 0, pressed: true });
            }
            self.tap(virtual_key, if shift { NVST_SHIFT } else { 0 });
            if shift {
                self.queue.push(CapturedInput::Key { virtual_key: VK_LSHIFT, modifiers: 0, pressed: false });
            }
        }
    }

    fn tap(&self, virtual_key: u16, modifiers: u16) {
        self.queue.push(CapturedInput::Key { virtual_key, modifiers, pressed: true });
        self.queue.push(CapturedInput::Key { virtual_key, modifiers, pressed: false });
    }

    /// Call once per main-loop iteration: re-sends controller state the server times out without.
    pub fn tick(&mut self) {
        self.flush_typing();
        if self.last_keepalive.elapsed() >= KEEPALIVE {
            self.last_keepalive = Instant::now();
            for input in self.gamepads.keepalive() {
                self.queue.push(input);
            }
        }
    }

    pub fn controller_connected(&mut self, instance_id: i32) {
        if let Some(input) = self.gamepads.connect(instance_id) {
            self.queue.push(input);
        }
    }

    pub fn controller_disconnected(&mut self, instance_id: i32) {
        if let Some(input) = self.gamepads.disconnect(instance_id) {
            self.queue.push(input);
        }
    }

    /// Lets go of everything the game holds (keys, buttons, controllers back to neutral) and, off
    /// the TV, gives the pointer back; for when the app takes input over, e.g. to show a menu.
    pub fn release(&mut self) {
        self.release_all();
        self.typing.clear();
        self.queue.discard_text();
        for input in self.gamepads.neutral() {
            self.queue.push(input);
        }
        if !self.always_captured {
            self.captured = false;
        }
    }

    /// `window_size` is in window coordinates (what mouse events use).
    pub fn handle(&mut self, event: InputEvent, window_size: (u32, u32)) {
        match event {
            InputEvent::FocusGained => self.focused = true,
            InputEvent::FocusLost => {
                self.release_all();
                self.focused = false;
                self.captured = false;
            }
            InputEvent::Key { scancode, modifiers, pressed, repeat, .. } => self.key(scancode, modifiers, pressed, repeat),
            InputEvent::MouseMotion { x, y, xrel, yrel } => self.motion(x, y, xrel, yrel, window_size),
            InputEvent::MouseButton { button, pressed } => self.button(button, pressed),
            InputEvent::MouseWheel { x, y, flipped } if self.is_captured() && (x != 0 || y != 0) => {
                let sign = if flipped { -1 } else { 1 };
                self.queue.push(CapturedInput::MouseWheel {
                    delta_x: clamp_i16(x.saturating_mul(WHEEL_STEP).saturating_mul(sign)),
                    delta_y: clamp_i16(y.saturating_mul(WHEEL_STEP).saturating_mul(sign)),
                });
            }
            InputEvent::ControllerButton { instance_id, button, pressed } => {
                if let Some(input) = self.gamepads.button(instance_id, button, pressed) {
                    self.queue.push(input);
                }
            }
            InputEvent::ControllerAxis { instance_id, axis, value } => {
                if let Some(input) = self.gamepads.axis(instance_id, axis, value) {
                    self.queue.push(input);
                }
            }
            _ => {}
        }
    }

    fn key(&mut self, scancode: i32, keymod: u16, pressed: bool, repeat: bool) {
        if !pressed {
            if let Some(index) = self.swallowed.iter().position(|&s| s == scancode) {
                self.swallowed.swap_remove(index);
                return;
            }
            if let Some(virtual_key) = self.pressed_keys.remove(&scancode) {
                let modifiers = keymap::modifiers(scancode, keymod);
                self.queue.push(CapturedInput::Key { virtual_key, modifiers, pressed: false });
            }
            return;
        }
        if repeat || !self.is_captured() {
            return;
        }
        if (keymap::is_ctrl(scancode) || keymap::is_alt(scancode)) && keymap::has_ctrl_and_alt(keymod) {
            self.swallowed.push(scancode);
            self.release_all();
            self.captured = false;
            return;
        }
        let Some(virtual_key) = keymap::virtual_key(scancode) else { return };
        if self.pressed_keys.insert(scancode, virtual_key).is_none() {
            let modifiers = keymap::modifiers(scancode, keymod);
            self.queue.push(CapturedInput::Key { virtual_key, modifiers, pressed: true });
        }
    }

    fn motion(&mut self, x: i32, y: i32, xrel: i32, yrel: i32, window_size: (u32, u32)) {
        match self.pointer() {
            Pointer::Free => {}
            Pointer::Relative if xrel != 0 || yrel != 0 => {
                self.queue.push(CapturedInput::MouseMove { delta_x: clamp_i16(xrel), delta_y: clamp_i16(yrel) });
            }
            Pointer::Relative => {}
            Pointer::Absolute { .. } => {
                let (stream_w, stream_h) = self.stream_size;
                let (left, top, width, height) = gfn_sdl::letterbox_rect(stream_w, stream_h, window_size.0, window_size.1);
                let (width, height) = (width.max(1), height.max(1));
                self.queue.push(CapturedInput::MouseAbsolute {
                    x: (x - left).clamp(0, width as i32 - 1) as u16,
                    y: (y - top).clamp(0, height as i32 - 1) as u16,
                    width: width.min(u32::from(u16::MAX)) as u16,
                    height: height.min(u32::from(u16::MAX)) as u16,
                });
            }
        }
    }

    fn button(&mut self, button: u8, pressed: bool) {
        if !(1..=5).contains(&button) {
            return;
        }
        let mask = 1 << button;
        if pressed && !self.captured {
            // The click that hands input to the stream is not the game's click.
            self.captured = true;
            self.focused = true;
            return;
        }
        if pressed && self.is_captured() && self.pressed_buttons & mask == 0 {
            self.pressed_buttons |= mask;
            self.queue.push(CapturedInput::MouseButton { button, pressed: true });
        } else if !pressed && self.pressed_buttons & mask != 0 {
            self.pressed_buttons &= !mask;
            self.queue.push(CapturedInput::MouseButton { button, pressed: false });
        }
    }

    /// Leaving capture must not strand a held key or button in the game.
    fn release_all(&mut self) {
        for (_, virtual_key) in self.pressed_keys.drain() {
            self.queue.push(CapturedInput::Key { virtual_key, modifiers: 0, pressed: false });
        }
        for button in 1..=5u8 {
            if self.pressed_buttons & (1 << button) != 0 {
                self.queue.push(CapturedInput::MouseButton { button, pressed: false });
            }
        }
        self.pressed_buttons = 0;
    }
}

/// Server predefined cursor ids to `SDL_SystemCursor`, as the desktop client maps them.
fn system_cursor(id: u8) -> u8 {
    use gfn_sdl::SystemCursors as C;
    match id {
        2 => C::IBEAM,
        3 => C::WAIT,
        4 => C::CROSSHAIR,
        5 => C::WAITARROW,
        6 => C::SIZENWSE,
        7 => C::SIZENESW,
        8 => C::SIZEWE,
        9 => C::SIZENS,
        10 => C::SIZEALL,
        12 => C::HAND,
        _ => C::ARROW,
    }
}

fn clamp_i16(value: i32) -> i16 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(always: bool) -> (InputCapture, Arc<CapturedInputQueue>, Arc<RemoteCursor>) {
        let queue = Arc::new(CapturedInputQueue::default());
        let cursor = Arc::new(RemoteCursor::default());
        (InputCapture::new(Arc::clone(&queue), Arc::clone(&cursor), always), queue, cursor)
    }

    fn drain(queue: &CapturedInputQueue) -> Vec<CapturedInput> {
        std::iter::from_fn(|| queue.take()).collect()
    }

    fn key(scancode: i32, modifiers: u16, pressed: bool) -> InputEvent {
        InputEvent::Key { scancode, keycode: 0, modifiers, pressed, repeat: false }
    }

    const WINDOW: (u32, u32) = (1280, 720);

    #[test]
    fn first_click_captures_without_reaching_the_game() {
        let (mut input, queue, _) = capture(false);
        input.handle(key(4, 0, true), WINDOW);
        input.handle(InputEvent::MouseButton { button: 1, pressed: true }, WINDOW);
        input.handle(InputEvent::MouseButton { button: 1, pressed: false }, WINDOW);
        assert!(drain(&queue).is_empty());
        input.handle(key(4, 0, true), WINDOW);
        assert_eq!(drain(&queue), vec![CapturedInput::Key { virtual_key: 0x41, modifiers: 0, pressed: true }]);
    }

    #[test]
    fn ctrl_alt_releases_held_input_and_swallows_the_chord() {
        let (mut input, queue, _) = capture(true);
        input.handle(key(26, 0, true), WINDOW); // W held
        input.handle(InputEvent::MouseButton { button: 3, pressed: true }, WINDOW);
        input.handle(key(224, 0x0040, true), WINDOW); // LCtrl
        input.handle(key(226, 0x0040 | 0x0100, true), WINDOW); // LAlt -> release
        input.handle(key(226, 0x0040, false), WINDOW);
        input.handle(key(224, 0, false), WINDOW);
        input.handle(key(26, 0, false), WINDOW);
        let events = drain(&queue);
        assert!(!input.is_captured());
        assert!(events.contains(&CapturedInput::Key { virtual_key: 0x57, modifiers: 0, pressed: false }));
        assert!(events.contains(&CapturedInput::Key { virtual_key: 0xa2, modifiers: 0, pressed: false }));
        assert!(events.contains(&CapturedInput::MouseButton { button: 3, pressed: false }));
        assert!(!events.iter().any(|e| matches!(e, CapturedInput::Key { virtual_key: 0xa4, .. })));
        let key_ups = events.iter().filter(|e| matches!(e, CapturedInput::Key { pressed: false, .. })).count();
        assert_eq!(key_ups, 2, "{events:?}");
    }

    #[test]
    fn release_lets_go_of_keys_and_controllers() {
        let (mut input, queue, _) = capture(false);
        input.handle(InputEvent::MouseButton { button: 1, pressed: true }, WINDOW); // captures
        input.controller_connected(7);
        input.handle(key(26, 0, true), WINDOW);
        input.handle(InputEvent::ControllerButton { instance_id: 7, button: 0, pressed: true }, WINDOW);
        drain(&queue);
        input.release();
        let events = drain(&queue);
        assert!(!input.is_captured());
        assert!(events.contains(&CapturedInput::Key { virtual_key: 0x57, modifiers: 0, pressed: false }));
        assert!(events.iter().any(|e| matches!(e, CapturedInput::Gamepad { buttons: 0, .. })), "{events:?}");
    }

    #[test]
    fn pointer_mode_follows_the_game_cursor() {
        let (mut input, queue, cursor) = capture(true);
        input.handle(InputEvent::MouseMotion { x: 640, y: 360, xrel: 3, yrel: -2 }, WINDOW);
        assert_eq!(
            drain(&queue),
            vec![CapturedInput::MouseAbsolute { x: 640, y: 360, width: 1280, height: 720 }]
        );
        cursor.apply(&[0, 0]);
        assert_eq!(input.pointer(), Pointer::Relative);
        input.handle(InputEvent::MouseMotion { x: 640, y: 360, xrel: 3, yrel: -2 }, WINDOW);
        assert_eq!(drain(&queue), vec![CapturedInput::MouseMove { delta_x: 3, delta_y: -2 }]);
    }

    #[test]
    fn absolute_positions_are_relative_to_the_letterboxed_picture() {
        let (mut input, queue, _) = capture(true);
        input.handle(InputEvent::MouseMotion { x: 500, y: 0, xrel: 0, yrel: 0 }, (1000, 1000));
        assert_eq!(drain(&queue), vec![CapturedInput::MouseAbsolute { x: 500, y: 0, width: 1000, height: 562 }]);
    }

    fn text_of(input: &CapturedInput) -> Option<&str> {
        match input {
            CapturedInput::Text(text) => Some(text.as_str()),
            _ => None,
        }
    }

    #[test]
    fn typed_text_and_editing_keys_keep_their_order() {
        let (mut input, queue, _) = capture(true);
        queue.set_text_ready(1, true);
        input.type_text("ab");
        // Still being sent: the slot stays busy while the transport holds the text.
        let in_flight = drain(&queue);
        assert_eq!(in_flight.len(), 1);
        assert_eq!(text_of(&in_flight[0]), Some("ab"));
        input.type_text("c");
        input.type_key(42); // Backspace
        input.type_text("d");
        assert!(drain(&queue).is_empty());
        drop(in_flight);
        input.tick();
        // The key follows its text in the same queue, so it may go out at once; the next text
        // waits for the slot.
        let sent = drain(&queue);
        assert_eq!(text_of(&sent[0]), Some("c"));
        assert_eq!(sent[1..], [
            CapturedInput::Key { virtual_key: 0x08, modifiers: 0, pressed: true },
            CapturedInput::Key { virtual_key: 0x08, modifiers: 0, pressed: false },
        ]);
        drop(sent);
        input.tick();
        let sent = drain(&queue);
        assert_eq!(sent.len(), 1);
        assert_eq!(text_of(&sent[0]), Some("d"));
    }

    #[test]
    fn without_unicode_text_characters_become_us_key_presses() {
        let (mut input, queue, _) = capture(true);
        input.type_text("A1ł");
        assert_eq!(
            drain(&queue),
            vec![
                CapturedInput::Key { virtual_key: 0xa0, modifiers: 0, pressed: true },
                CapturedInput::Key { virtual_key: 0x41, modifiers: 1, pressed: true },
                CapturedInput::Key { virtual_key: 0x41, modifiers: 1, pressed: false },
                CapturedInput::Key { virtual_key: 0xa0, modifiers: 0, pressed: false },
                CapturedInput::Key { virtual_key: 0x31, modifiers: 0, pressed: true },
                CapturedInput::Key { virtual_key: 0x31, modifiers: 0, pressed: false },
            ]
        );
    }

    #[test]
    fn wheel_uses_windows_detents() {
        let (mut input, queue, _) = capture(true);
        input.handle(InputEvent::MouseWheel { x: 0, y: -1, flipped: false }, WINDOW);
        assert_eq!(drain(&queue), vec![CapturedInput::MouseWheel { delta_x: 0, delta_y: -120 }]);
    }
}
