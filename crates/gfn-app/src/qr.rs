use qrcodegen::{QrCode, QrCodeEcc};

/// Quiet zone the QR spec requires around the symbol, in modules.
const BORDER: i32 = 4;

/// `text` as an RGBA image with one pixel per module (drawn scaled up with nearest filtering):
/// black modules on white, quiet zone included. Returns the side length and the pixels.
pub fn pixels(text: &str) -> Option<(u32, Vec<u8>)> {
    let code = QrCode::encode_text(text, QrCodeEcc::Medium).ok()?;
    let side = code.size() + 2 * BORDER;
    let mut rgba = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let dark = code.get_module(x - BORDER, y - BORDER);
            let value = if dark { 0 } else { 255 };
            rgba.extend_from_slice(&[value, value, value, 255]);
        }
    }
    Some((side as u32, rgba))
}

#[cfg(test)]
mod tests {
    #[test]
    fn has_a_white_quiet_zone_and_dark_finder() {
        let (side, rgba) = super::pixels("https://example.com/activate?code=ABCD").unwrap();
        let at = |x: u32, y: u32| rgba[((y * side + x) * 4) as usize];
        assert_eq!(at(0, 0), 255);
        assert_eq!(at(4, 4), 0);
    }
}
