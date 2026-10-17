//! Drawing on a fixed 1920×1080 layout canvas, scaled and centred into whatever the window or
//! panel really is. Text is rasterised at the output size (not scaled), so it stays sharp in a
//! small simulator window and on a 4K panel alike.

use std::collections::HashMap;

use gfn_sdl::{Area, Texture, Window};

pub const CANVAS_W: f32 = 1920.0;
pub const CANVAS_H: f32 = 1080.0;

pub type Rgba = [u8; 4];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }

    pub fn inset(&self, by: f32) -> Self {
        Self::new(self.x + by, self.y + by, self.w - 2.0 * by, self.h - 2.0 * by)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

pub struct Fonts {
    regular: fontdue::Font,
    bold: fontdue::Font,
}

impl Fonts {
    pub fn load() -> Result<Self, String> {
        let load = |bytes: &[u8]| {
            fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).map_err(|e| format!("font: {e}"))
        };
        Ok(Self {
            regular: load(include_bytes!("../../../assets/fonts/Inter-Regular.ttf"))?,
            bold: load(include_bytes!("../../../assets/fonts/Inter-Bold.ttf"))?,
        })
    }

    fn font(&self, bold: bool) -> &fontdue::Font {
        if bold { &self.bold } else { &self.regular }
    }

    /// Advance width of `text` at `px` pixels.
    fn width(&self, text: &str, px: f32, bold: bool) -> f32 {
        let font = self.font(bold);
        let mut width = 0.0;
        let mut previous = None;
        for c in text.chars() {
            if let Some(p) = previous {
                width += font.horizontal_kern(p, c, px).unwrap_or(0.0);
            }
            width += font.metrics(c, px).advance_width;
            previous = Some(c);
        }
        width
    }

    /// White coverage mask of one line of `text`; `None` for empty or invisible text.
    fn rasterize(&self, text: &str, px: f32, bold: bool) -> Option<(u32, u32, Vec<u8>)> {
        let font = self.font(bold);
        let line = font.horizontal_line_metrics(px)?;
        let width = self.width(text, px, bold).ceil() as usize + 2;
        let height = (line.ascent - line.descent).ceil() as usize + 1;
        if width <= 2 || height == 0 {
            return None;
        }
        let mut pixels = vec![0u8; width * height * 4];
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[..3].copy_from_slice(&[255, 255, 255]);
        }
        let mut pen = 1.0f32;
        let mut previous = None;
        for c in text.chars() {
            if let Some(p) = previous {
                pen += font.horizontal_kern(p, c, px).unwrap_or(0.0);
            }
            let (metrics, coverage) = font.rasterize(c, px);
            let left = (pen + metrics.xmin as f32).round() as i64;
            let top = (line.ascent - metrics.height as f32 - metrics.ymin as f32).round() as i64;
            for row in 0..metrics.height {
                for col in 0..metrics.width {
                    let (x, y) = (left + col as i64, top + row as i64);
                    if x < 0 || y < 0 || x as usize >= width || y as usize >= height {
                        continue;
                    }
                    let alpha = &mut pixels[(y as usize * width + x as usize) * 4 + 3];
                    *alpha = (*alpha).max(coverage[row * metrics.width + col]);
                }
            }
            pen += metrics.advance_width;
            previous = Some(c);
        }
        Some((width as u32, height as u32, pixels))
    }
}

struct Cached {
    texture: Option<Texture>,
    used: u64,
}

impl Cached {
    fn bytes(&self) -> usize {
        self.texture.as_ref().map_or(0, |t| t.width as usize * t.height as usize * 4)
    }
}

/// Rendered lines are kept while this many bytes of texture or fewer; one wide title line at 4K
/// is about 1 MB, so this holds every line of several screens.
const TEXT_BUDGET_BYTES: usize = 24 * 1024 * 1024;

/// Window-bound drawing state: the scale, rendered text lines and anti-aliased corner masks.
pub struct Painter {
    fonts: Fonts,
    scale: f32,
    offset: (f32, f32),
    frame: u64,
    /// By (pixel size, bold), then by the line itself, so a hit is looked up without allocating.
    text: HashMap<(u32, bool), HashMap<String, Cached>>,
    text_bytes: usize,
    /// Anti-aliased discs by pixel radius; rounded rectangles draw their quadrants as corners.
    discs: HashMap<u32, Texture>,
    fades: HashMap<Fade, Texture>,
}

/// The way a [`Painter::fade`] runs out: towards the right, the top or the bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fade {
    Right,
    Up,
    Down,
}

impl Painter {
    pub fn new(fonts: Fonts) -> Self {
        Self { fonts, scale: 1.0, offset: (0.0, 0.0), frame: 0, text: HashMap::new(), text_bytes: 0, discs: HashMap::new(), fades: HashMap::new() }
    }

    /// Call before drawing a frame: picks up the current output size and evicts stale text.
    pub fn begin(&mut self, window: &Window) {
        let (w, h) = window.output_size();
        self.scale = (w as f32 / CANVAS_W).min(h as f32 / CANVAS_H).max(0.01);
        self.offset = ((w as f32 - CANVAS_W * self.scale) / 2.0, (h as f32 - CANVAS_H * self.scale) / 2.0);
        self.frame += 1;
        if self.text_bytes > TEXT_BUDGET_BYTES {
            self.evict_text();
        }
    }

    /// Drops the least recently drawn lines until the cache is back to half its budget; lines
    /// drawn in the last frame stay.
    fn evict_text(&mut self) {
        let mut by_age: Vec<(u64, usize)> =
            self.text.values().flat_map(|lines| lines.values().map(|c| (c.used, c.bytes()))).collect();
        by_age.sort_unstable();
        let (mut bytes, mut cutoff) = (self.text_bytes, 0);
        for (used, size) in by_age {
            if bytes <= TEXT_BUDGET_BYTES / 2 || used + 1 >= self.frame {
                break;
            }
            bytes -= size;
            cutoff = used + 1;
        }
        for lines in self.text.values_mut() {
            lines.retain(|_, cached| cached.used >= cutoff);
        }
        self.text.retain(|_, lines| !lines.is_empty());
        self.text_bytes = self.text.values().flat_map(|lines| lines.values().map(Cached::bytes)).sum();
    }

    /// Output pixels per canvas unit.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Maps a window-coordinate point (mouse events) onto the canvas.
    pub fn to_canvas(&self, window: &Window, x: i32, y: i32) -> (f32, f32) {
        let (win_w, _) = window.size();
        let (out_w, _) = window.output_size();
        let density = if win_w == 0 { 1.0 } else { out_w as f32 / win_w as f32 };
        ((x as f32 * density - self.offset.0) / self.scale, (y as f32 * density - self.offset.1) / self.scale)
    }

    pub fn area(&self, r: Rect) -> Area {
        let x0 = (self.offset.0 + r.x * self.scale).round() as i32;
        let y0 = (self.offset.1 + r.y * self.scale).round() as i32;
        let x1 = (self.offset.0 + (r.x + r.w) * self.scale).round() as i32;
        let y1 = (self.offset.1 + (r.y + r.h) * self.scale).round() as i32;
        Area::new(x0, y0, x1 - x0, y1 - y0)
    }

    pub fn fill(&mut self, window: &mut Window, r: Rect, color: Rgba) {
        window.fill(self.area(r), color);
    }

    /// A rectangle with corners of `radius` canvas units; translucent colours do not overlap.
    pub fn round_rect(&mut self, window: &mut Window, r: Rect, radius: f32, color: Rgba) {
        let a = self.area(r);
        let radius = ((radius * self.scale).round() as i32).min(a.w / 2).min(a.h / 2);
        if radius < 1 {
            window.fill(a, color);
            return;
        }
        let Some(disc) = self.disc(window, radius as u32) else {
            window.fill(a, color);
            return;
        };
        let d = radius;
        let corners = [
            (Area::new(0, 0, d, d), Area::new(a.x, a.y, d, d)),
            (Area::new(d, 0, d, d), Area::new(a.x + a.w - d, a.y, d, d)),
            (Area::new(0, d, d, d), Area::new(a.x, a.y + a.h - d, d, d)),
            (Area::new(d, d, d, d), Area::new(a.x + a.w - d, a.y + a.h - d, d, d)),
        ];
        for (source, dest) in corners {
            window.draw(disc, Some(source), dest, color);
        }
        window.fill(Area::new(a.x + d, a.y, a.w - 2 * d, a.h), color);
        window.fill(Area::new(a.x, a.y + d, d, a.h - 2 * d), color);
        window.fill(Area::new(a.x + a.w - d, a.y + d, d, a.h - 2 * d), color);
    }

    /// A `width`-unit ring around `r`, for something opaque with `radius` corners drawn over `r`.
    pub fn halo(&mut self, window: &mut Window, r: Rect, radius: f32, width: f32, color: Rgba) {
        let outer = Rect::new(r.x - width, r.y - width, r.w + 2.0 * width, r.h + 2.0 * width);
        self.round_rect(window, outer, radius + width, color);
    }

    fn disc(&mut self, window: &mut Window, radius: u32) -> Option<&Texture> {
        if !self.discs.contains_key(&radius) {
            let texture = window.texture(radius * 2, radius * 2, &disc_pixels(radius)).ok()?;
            self.discs.insert(radius, texture);
        }
        self.discs.get(&radius)
    }

    /// `color` over `r`, at full strength on one side and gone on the side `fade` names.
    pub fn fade(&mut self, window: &mut Window, r: Rect, color: Rgba, fade: Fade) {
        if !self.fades.contains_key(&fade) {
            let Ok(texture) = (match fade {
                Fade::Right => window.texture(FADE_STEPS, 1, &fade_pixels(false)),
                Fade::Up => window.texture(1, FADE_STEPS, &fade_pixels(true)),
                Fade::Down => window.texture(1, FADE_STEPS, &fade_pixels(false)),
            }) else {
                return;
            };
            self.fades.insert(fade, texture);
        }
        let area = self.area(r);
        window.draw(&self.fades[&fade], None, area, color);
    }

    pub fn image(&mut self, window: &mut Window, texture: &Texture, r: Rect, alpha: u8) {
        window.draw(texture, None, self.area(r), [255, 255, 255, alpha]);
    }

    /// Width of `text` in canvas units.
    pub fn text_width(&self, text: &str, size: f32, bold: bool) -> f32 {
        self.fonts.width(text, size * self.scale, bold) / self.scale
    }

    /// `text` shortened with an ellipsis to fit `max_width` canvas units.
    pub fn fit(&self, text: &str, size: f32, bold: bool, max_width: f32) -> String {
        if self.text_width(text, size, bold) <= max_width {
            return text.to_owned();
        }
        let shortened = |chars: usize| {
            let end = text.char_indices().nth(chars).map_or(text.len(), |(i, _)| i);
            text[..end].trim_end().to_owned() + "…"
        };
        // Widths grow with the prefix, so the longest prefix that fits is found by bisection
        // (this runs for every tile on every frame of a scroll).
        let (mut fits, mut too_long) = (0, text.chars().count());
        while too_long - fits > 1 {
            let mid = (fits + too_long) / 2;
            if self.text_width(&shortened(mid), size, bold) <= max_width {
                fits = mid;
            } else {
                too_long = mid;
            }
        }
        let best = shortened(fits);
        if fits == 0 && self.text_width(&best, size, bold) > max_width { String::new() } else { best }
    }

    /// One line of text whose box (ascent to descent) starts at canvas `y`.
    pub fn text(&mut self, window: &mut Window, text: &str, x: f32, y: f32, size: f32, color: Rgba, bold: bool, align: Align) {
        if text.is_empty() {
            return;
        }
        let px = (size * self.scale).round().max(1.0) as u32;
        let frame = self.frame;
        let (scale, offset) = (self.scale, self.offset);
        let lines = self.text.entry((px, bold)).or_default();
        if !lines.contains_key(text) {
            let texture =
                self.fonts.rasterize(text, px as f32, bold).and_then(|(w, h, pixels)| window.texture(w, h, &pixels).ok());
            let cached = Cached { texture, used: frame };
            self.text_bytes += cached.bytes();
            lines.insert(text.to_owned(), cached);
        }
        let cached = lines.get_mut(text).expect("inserted above");
        cached.used = frame;
        let Some(texture) = cached.texture.as_ref() else { return };
        let width = texture.width as f32 / scale;
        let left = match align {
            Align::Left => x,
            Align::Center => x - width / 2.0,
            Align::Right => x - width,
        };
        let a = Area::new((offset.0 + left * scale).round() as i32, (offset.1 + y * scale).round() as i32, 0, 0);
        window.draw(texture, None, Area::new(a.x, a.y, texture.width as i32, texture.height as i32), color);
    }

    /// Word-wrapped text; returns the height used in canvas units.
    pub fn paragraph(&mut self, window: &mut Window, text: &str, r: Rect, size: f32, color: Rgba, align: Align) -> f32 {
        let line_height = size * 1.4;
        let mut y = r.y;
        for line in self.wrap(text, size, r.w) {
            let x = match align {
                Align::Left => r.x,
                Align::Center => r.x + r.w / 2.0,
                Align::Right => r.x + r.w,
            };
            self.text(window, &line, x, y, size, color, false, align);
            y += line_height;
        }
        y - r.y
    }

    /// Height [`Painter::paragraph`] needs for `text` in `width` canvas units.
    pub fn paragraph_height(&self, text: &str, size: f32, width: f32) -> f32 {
        self.wrap(text, size, width).len() as f32 * size * 1.4
    }

    /// `text` broken into the lines [`Painter::paragraph`] would draw.
    pub fn wrap(&self, text: &str, size: f32, width: f32) -> Vec<String> {
        let mut lines = Vec::new();
        for paragraph in text.split('\n') {
            let mut line = String::new();
            for word in paragraph.split_whitespace() {
                let candidate = if line.is_empty() { word.to_owned() } else { format!("{line} {word}") };
                if !line.is_empty() && self.text_width(&candidate, size, false) > width {
                    lines.push(std::mem::replace(&mut line, word.to_owned()));
                } else {
                    line = candidate;
                }
            }
            lines.push(line);
        }
        lines
    }
}

const FADE_STEPS: u32 = 256;

/// A white strip from opaque to clear, or clear to opaque when `rising`.
fn fade_pixels(rising: bool) -> Vec<u8> {
    (0..FADE_STEPS)
        .flat_map(|i| {
            let alpha = (255 * i / (FADE_STEPS - 1)) as u8;
            [255, 255, 255, if rising { alpha } else { 255 - alpha }]
        })
        .collect()
}

/// White disc of `radius` pixels with a one-pixel anti-aliased edge.
fn disc_pixels(radius: u32) -> Vec<u8> {
    let size = radius * 2;
    let r = radius as f32;
    let mut pixels = vec![255u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 + 0.5 - r, y as f32 + 0.5 - r);
            let coverage = (r - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
            pixels[((y * size + x) * 4 + 3) as usize] = (coverage * 255.0).round() as u8;
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disc_is_opaque_inside_and_clear_outside() {
        let pixels = disc_pixels(8);
        let alpha = |x: u32, y: u32| pixels[((y * 16 + x) * 4 + 3) as usize];
        assert_eq!(alpha(8, 8), 255);
        assert_eq!(alpha(0, 0), 0);
    }

    #[test]
    fn fit_keeps_the_longest_prefix_that_fits() {
        let painter = Painter::new(Fonts::load().unwrap());
        assert_eq!(painter.fit("Short", 26.0, false, 240.0), "Short");
        let title = "The Elder Scrolls V: Skyrim Special Edition";
        let fitted = painter.fit(title, 26.0, true, 240.0);
        assert!(fitted.ends_with('…') && painter.text_width(&fitted, 26.0, true) <= 240.0, "{fitted}");
        let kept = fitted.trim_end_matches('…').chars().count();
        let next: String = title.chars().take(kept + 1).collect::<String>().trim_end().to_owned() + "…";
        assert!(painter.text_width(&next, 26.0, true) > 240.0 || next == fitted, "{next}");
        assert_eq!(painter.fit(title, 26.0, false, 1.0), "");
    }

    #[test]
    fn text_rasterizes_inside_its_box() {
        let fonts = Fonts::load().unwrap();
        let (w, h, pixels) = fonts.rasterize("Hag", 32.0, false).unwrap();
        assert!(w > 40 && h >= 32);
        assert!(pixels.chunks_exact(4).any(|p| p[3] == 255));
        assert!(fonts.rasterize("", 32.0, false).is_none());
    }
}
