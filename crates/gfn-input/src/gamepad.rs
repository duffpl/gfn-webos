//! Up to four controllers in NVST slots 0-3, reported as full XInput-style state snapshots.

use opennow_streamer_platform_webos::CapturedInput;

const SLOTS: usize = 4;
const BUTTON_GUIDE: u8 = 5;
const DEADZONE: f32 = 0.15;

#[derive(Debug, Clone, Copy, Default)]
struct State {
    instance_id: Option<i32>,
    buttons: u16,
    left_trigger: u8,
    right_trigger: u8,
    raw: [i16; 4],
}

#[derive(Default)]
pub struct Gamepads {
    slots: [State; SLOTS],
}

impl Gamepads {
    pub fn connect(&mut self, instance_id: i32) -> Option<CapturedInput> {
        if self.slot(instance_id).is_some() {
            return None;
        }
        let slot = self.slots.iter().position(|s| s.instance_id.is_none())?;
        self.slots[slot] = State { instance_id: Some(instance_id), ..State::default() };
        Some(self.report(slot))
    }

    /// The neutral report for the freed slot also carries the bitmap without it.
    pub fn disconnect(&mut self, instance_id: i32) -> Option<CapturedInput> {
        let slot = self.slot(instance_id)?;
        self.slots[slot] = State::default();
        Some(self.report(slot))
    }

    /// `SDL_GameControllerButton` numbering. Guide opens the stream menu instead of reaching the game.
    pub fn button(&mut self, instance_id: i32, button: u8, pressed: bool) -> Option<CapturedInput> {
        let slot = self.slot(instance_id)?;
        if button == BUTTON_GUIDE {
            return pressed.then_some(CapturedInput::Guide);
        }
        let mask = button_mask(button)?;
        let state = &mut self.slots[slot];
        if pressed {
            state.buttons |= mask;
        } else {
            state.buttons &= !mask;
        }
        Some(self.report(slot))
    }

    /// `SDL_GameControllerAxis` numbering: 0-3 sticks (LX, LY, RX, RY), 4-5 triggers.
    pub fn axis(&mut self, instance_id: i32, axis: u8, value: i16) -> Option<CapturedInput> {
        let slot = self.slot(instance_id)?;
        let state = &mut self.slots[slot];
        match axis {
            0..=3 => state.raw[usize::from(axis)] = value,
            4 => state.left_trigger = trigger(value),
            5 => state.right_trigger = trigger(value),
            _ => return None,
        }
        Some(self.report(slot))
    }

    /// Every connected controller back to rest, as the reports that tell the server so.
    pub fn neutral(&mut self) -> Vec<CapturedInput> {
        for state in self.slots.iter_mut().filter(|s| s.instance_id.is_some()) {
            *state = State { instance_id: state.instance_id, ..State::default() };
        }
        self.keepalive()
    }

    pub fn keepalive(&self) -> Vec<CapturedInput> {
        (0..SLOTS).filter(|&slot| self.slots[slot].instance_id.is_some()).map(|slot| self.report(slot)).collect()
    }

    fn slot(&self, instance_id: i32) -> Option<usize> {
        self.slots.iter().position(|s| s.instance_id == Some(instance_id))
    }

    /// Bit n marks slot n connected; bit n+8 is set alongside it, as the official client sends.
    fn bitmap(&self) -> u16 {
        (0..SLOTS)
            .filter(|&slot| self.slots[slot].instance_id.is_some())
            .fold(0, |bitmap, slot| bitmap | (1 << slot) | (1 << (slot + 8)))
    }

    fn report(&self, slot: usize) -> CapturedInput {
        let state = self.slots[slot];
        let (left_x, left_y) = radial_deadzone(state.raw[0], state.raw[1]);
        let (right_x, right_y) = radial_deadzone(state.raw[2], state.raw[3]);
        CapturedInput::Gamepad {
            controller_id: slot as u8,
            bitmap: self.bitmap(),
            buttons: state.buttons,
            left_trigger: state.left_trigger,
            right_trigger: state.right_trigger,
            left_stick_x: left_x,
            // SDL's Y axis points down; XInput's points up.
            left_stick_y: left_y.saturating_neg(),
            right_stick_x: right_x,
            right_stick_y: right_y.saturating_neg(),
        }
    }
}

fn button_mask(button: u8) -> Option<u16> {
    Some(match button {
        0 => 0x1000,  // A
        1 => 0x2000,  // B
        2 => 0x4000,  // X
        3 => 0x8000,  // Y
        4 => 0x0020,  // Back
        6 => 0x0010,  // Start
        7 => 0x0040,  // left stick
        8 => 0x0080,  // right stick
        9 => 0x0100,  // left shoulder
        10 => 0x0200, // right shoulder
        11 => 0x0001, // d-pad up
        12 => 0x0002, // down
        13 => 0x0004, // left
        14 => 0x0008, // right
        _ => return None,
    })
}

fn radial_deadzone(x: i16, y: i16) -> (i16, i16) {
    let (nx, ny) = (f32::from(x) / 32767.0, f32::from(y) / 32767.0);
    let magnitude = nx.hypot(ny);
    if magnitude < DEADZONE {
        return (0, 0);
    }
    let factor = ((magnitude - DEADZONE) / (1.0 - DEADZONE)).clamp(0.0, 1.0) / magnitude;
    ((nx * factor * 32767.0).round() as i16, (ny * factor * 32767.0).round() as i16)
}

fn trigger(value: i16) -> u8 {
    ((f32::from(value) / 32767.0).clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadzone_matches_the_desktop_client() {
        assert_eq!(radial_deadzone(2_000, 2_000), (0, 0));
        let diagonal = radial_deadzone(16_384, 16_384);
        assert_eq!(diagonal.0, diagonal.1);
        assert!(diagonal.0 > 10_000);
        assert_eq!(radial_deadzone(i16::MAX, 0), (i16::MAX, 0));
    }

    #[test]
    fn slots_bitmap_and_inverted_y() {
        let mut pads = Gamepads::default();
        pads.connect(10);
        let Some(CapturedInput::Gamepad { controller_id, bitmap, .. }) = pads.connect(11) else { panic!() };
        assert_eq!((controller_id, bitmap), (1, 0x0303));
        let Some(CapturedInput::Gamepad { left_stick_y, buttons, .. }) = pads.axis(10, 1, i16::MAX) else { panic!() };
        assert_eq!((left_stick_y, buttons), (-i16::MAX, 0));
        let Some(CapturedInput::Gamepad { buttons, .. }) = pads.button(10, 0, true) else { panic!() };
        assert_eq!(buttons, 0x1000);
        assert_eq!(pads.button(10, BUTTON_GUIDE, true), Some(CapturedInput::Guide));
        let Some(CapturedInput::Gamepad { controller_id, bitmap, buttons, .. }) = pads.disconnect(10) else { panic!() };
        assert_eq!((controller_id, bitmap, buttons), (0, 0x0202, 0));
        assert_eq!(pads.keepalive().len(), 1);
        assert_eq!(pads.button(99, 0, true), None);
    }
}
