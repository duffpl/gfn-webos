//! SDL scancodes (USB HID usage ids) to Windows virtual keys, which is what NVST key reports carry.
//! Mirrors OpenNOW's desktop `sdl_virtual_key`/`sdl_modifiers` (MIT, opennow-streamer-platform).

pub fn virtual_key(scancode: i32) -> Option<u16> {
    Some(match scancode {
        4..=29 => 0x41 + (scancode - 4) as u16,  // A..Z
        30..=38 => 0x31 + (scancode - 30) as u16, // 1..9
        39 => 0x30,
        40 | 88 => 0x0d, // Return, keypad Enter
        41 => 0x1b,
        42 => 0x08,
        43 => 0x09,
        44 => 0x20,
        45 => 0xbd,
        46 | 103 => 0xbb, // Equals, keypad Equals
        47 => 0xdb,
        48 => 0xdd,
        49 => 0xdc,
        51 => 0xba,
        52 => 0xde,
        53 => 0xc0,
        54 => 0xbc,
        55 => 0xbe,
        56 => 0xbf,
        57 => 0x14,
        58..=69 => 0x70 + (scancode - 58) as u16,   // F1..F12
        104..=115 => 0x7c + (scancode - 104) as u16, // F13..F24
        70 => 0x2a,
        71 => 0x91,
        72 => 0x13,
        73 => 0x2d,
        74 => 0x24,
        75 => 0x21,
        76 => 0x2e,
        77 => 0x23,
        78 => 0x22,
        79 => 0x27,
        80 => 0x25,
        81 => 0x28,
        82 => 0x26,
        83 => 0x90,
        84 => 0x6f,
        85 => 0x6a,
        86 => 0x6d,
        87 => 0x6b,
        89..=97 => 0x61 + (scancode - 89) as u16, // keypad 1..9
        98 => 0x60,
        99 => 0x6e,
        100 => 0xe2,
        101 => 0x5d,
        224 => 0xa2,
        225 => 0xa0,
        226 => 0xa4,
        227 => 0x5b,
        228 => 0xa3,
        229 => 0xa1,
        230 => 0xa5,
        231 => 0x5c,
        _ => return None,
    })
}

const KMOD_SHIFT: u16 = 0x0003;
const KMOD_CTRL: u16 = 0x00c0;
const KMOD_ALT: u16 = 0x0300;
const KMOD_GUI: u16 = 0x0c00;

pub fn is_ctrl(scancode: i32) -> bool {
    matches!(scancode, 224 | 228)
}

pub fn is_alt(scancode: i32) -> bool {
    matches!(scancode, 226 | 230)
}

/// NVST modifier flags (1 shift, 2 ctrl, 4 alt, 8 meta). A modifier key's own report leaves
/// its flag out, as the official client does.
pub fn modifiers(scancode: i32, keymod: u16) -> u16 {
    let mut flags = 0;
    if !matches!(scancode, 225 | 229) && keymod & KMOD_SHIFT != 0 {
        flags |= 0x01;
    }
    if !is_ctrl(scancode) && keymod & KMOD_CTRL != 0 {
        flags |= 0x02;
    }
    if !is_alt(scancode) && keymod & KMOD_ALT != 0 {
        flags |= 0x04;
    }
    if !matches!(scancode, 227 | 231) && keymod & KMOD_GUI != 0 {
        flags |= 0x08;
    }
    flags
}

pub fn has_ctrl_and_alt(keymod: u16) -> bool {
    keymod & KMOD_CTRL != 0 && keymod & KMOD_ALT != 0
}

/// A character typed on a US layout: its virtual key and whether it needs Shift. For servers that
/// take no Unicode text; anything else has no key and is dropped.
pub fn us_key(c: char) -> Option<(u16, bool)> {
    const SHIFTED_DIGITS: &str = ")!@#$%^&*(";
    Some(match c {
        'a'..='z' => (0x41 + (c as u16 - 'a' as u16), false),
        'A'..='Z' => (0x41 + (c as u16 - 'A' as u16), true),
        '0'..='9' => (0x30 + (c as u16 - '0' as u16), false),
        ' ' => (0x20, false),
        '-' => (0xbd, false),
        '_' => (0xbd, true),
        '=' => (0xbb, false),
        '+' => (0xbb, true),
        '[' => (0xdb, false),
        '{' => (0xdb, true),
        ']' => (0xdd, false),
        '}' => (0xdd, true),
        '\\' => (0xdc, false),
        '|' => (0xdc, true),
        ';' => (0xba, false),
        ':' => (0xba, true),
        '\'' => (0xde, false),
        '"' => (0xde, true),
        ',' => (0xbc, false),
        '<' => (0xbc, true),
        '.' => (0xbe, false),
        '>' => (0xbe, true),
        '/' => (0xbf, false),
        '?' => (0xbf, true),
        '`' => (0xc0, false),
        '~' => (0xc0, true),
        _ => (0x30 + SHIFTED_DIGITS.find(c)? as u16, true),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_desktop_table() {
        assert_eq!(virtual_key(4), Some(0x41)); // A
        assert_eq!(virtual_key(26), Some(0x57)); // W
        assert_eq!(virtual_key(29), Some(0x5a)); // Z
        assert_eq!(virtual_key(30), Some(0x31)); // 1
        assert_eq!(virtual_key(39), Some(0x30)); // 0
        assert_eq!(virtual_key(41), Some(0x1b)); // Escape
        assert_eq!(virtual_key(69), Some(0x7b)); // F12
        assert_eq!(virtual_key(115), Some(0x87)); // F24
        assert_eq!(virtual_key(97), Some(0x69)); // keypad 9
        assert_eq!(virtual_key(98), Some(0x60)); // keypad 0
        assert_eq!(virtual_key(224), Some(0xa2)); // LCtrl
        assert_eq!(virtual_key(50), None); // non-US hash has no desktop mapping
    }

    #[test]
    fn us_layout_characters() {
        assert_eq!(us_key('q'), Some((0x51, false)));
        assert_eq!(us_key('Q'), Some((0x51, true)));
        assert_eq!(us_key('@'), Some((0x32, true)));
        assert_eq!(us_key(')'), Some((0x30, true)));
        assert_eq!(us_key('?'), Some((0xbf, true)));
        assert_eq!(us_key('ł'), None);
    }

    #[test]
    fn modifier_keys_omit_their_own_flag() {
        assert_eq!(modifiers(225, 0x0001), 0); // LShift down reports no shift
        assert_eq!(modifiers(4, 0x0001 | 0x0040), 0x03);
        assert_eq!(modifiers(224, 0x0040 | 0x0100), 0x04);
    }
}
