//! The notes of the releases newer than this build, opened from the header's update button. The
//! app does not install anything: the new package goes on the TV the way this one did.

use super::detail::{MONTHS, date_parts};
use super::*;
use crate::release::{self, Line, Release};

const BOX: Rect = Rect { x: 240.0, y: 110.0, w: 1440.0, h: 860.0 };
const PAD: f32 = 56.0;
const QR_SIDE: f32 = 300.0;
const NOTES_TOP: f32 = BOX.y + PAD + 150.0;
const NOTES_W: f32 = BOX.w - 2.0 * PAD - QR_SIDE - 64.0;
const NOTES_BOTTOM: f32 = BOX.y + BOX.h - PAD - 68.0 - 32.0;
const SIZE: f32 = 28.0;
const LINE_H: f32 = SIZE * 1.4;
const BULLET_INDENT: f32 = 36.0;
/// Lines moved by one press: a line at a time is slow going on a remote.
const STEP: usize = 3;
const CLOSE: Rect = Rect { x: BOX.x + PAD, y: BOX.y + BOX.h - PAD - 68.0, w: 240.0, h: 68.0 };

pub(super) struct Notes {
    /// The first line shown.
    first: usize,
    qr: Option<(u32, Vec<u8>)>,
    texture: Option<Texture>,
}

/// One drawn line of the notes.
struct Row {
    text: String,
    indent: f32,
    bold: bool,
    color: Rgba,
    bullet: bool,
}

/// "17 Oct 2026" from an ISO 8601 date.
pub(super) fn day_text(iso: &str) -> Option<String> {
    let (year, month, day) = date_parts(iso)?;
    Some(format!("{day} {} {year}", MONTHS[month as usize - 1]))
}

/// "Version 0.1.0 · fc8dd23 · 17 Oct 2026", leaving out what the build does not know.
pub(super) fn version_text() -> String {
    let date = day_text(release::COMMIT_DATE);
    let parts = [Some(format!("Version {}", release::VERSION)), Some(release::COMMIT.to_owned()).filter(|c| !c.is_empty()), date];
    parts.into_iter().flatten().collect::<Vec<_>>().join("  ·  ")
}

impl App {
    pub(super) fn open_notes(&mut self) {
        let Some(newest) = self.releases.first() else { return };
        let url = if newest.url.is_empty() { release::releases_page() } else { newest.url.clone() };
        self.notes = Some(Notes { first: 0, qr: crate::qr::pixels(&url), texture: None });
        self.dirty = true;
    }

    pub(super) fn notes_nav(&mut self, nav: Nav) {
        let last = self.notes_rows().len().saturating_sub(visible_rows());
        let Some(notes) = &mut self.notes else { return };
        match nav {
            Nav::Up => notes.first = notes.first.saturating_sub(STEP),
            Nav::Down => notes.first = (notes.first + STEP).min(last),
            Nav::Select | Nav::Back => self.notes = None,
            Nav::Left | Nav::Right => {}
        }
        self.dirty = true;
    }

    pub(super) fn notes_click(&mut self) {
        let (x, y) = self.mouse;
        if CLOSE.contains(x, y) || !BOX.contains(x, y) {
            self.notes = None;
            self.dirty = true;
        }
    }

    /// Every release newer than this one, newest first, wrapped to the notes column.
    fn notes_rows(&self) -> Vec<Row> {
        let p = &self.painter;
        let mut rows = Vec::new();
        let body = [214, 216, 221, 255];
        for (index, release) in self.releases.iter().enumerate() {
            if index > 0 {
                rows.push(Row { text: String::new(), indent: 0.0, bold: false, color: body, bullet: false });
            }
            let heading = match day_text(&release.date) {
                Some(day) => format!("Version {}  ·  {day}", release.version),
                None => format!("Version {}", release.version),
            };
            rows.push(Row { text: heading, indent: 0.0, bold: true, color: ACCENT, bullet: false });
            let lines = release::notes_lines(&release.notes);
            if lines.is_empty() {
                rows.push(Row { text: "No notes for this release.".into(), indent: 0.0, bold: false, color: DIM, bullet: false });
            }
            for line in lines {
                let (text, indent, bold, color, bullet) = match line {
                    Line::Heading(text) => (text, 0.0, true, TEXT, false),
                    Line::Bullet(text) => (text, BULLET_INDENT, false, body, true),
                    Line::Text(text) => (text, 0.0, false, body, false),
                    Line::Gap => (String::new(), 0.0, false, body, false),
                };
                let width = NOTES_W - indent;
                // A long link is one word to the wrapper; cut it rather than run past the column.
                for (i, text) in p.wrap(&text, SIZE, width).into_iter().enumerate() {
                    let text = p.fit(&text, SIZE, bold, width);
                    rows.push(Row { text, indent, bold, color, bullet: bullet && i == 0 });
                }
            }
        }
        rows
    }

    pub(super) fn draw_notes(&mut self, window: &mut Window) {
        if self.notes.is_none() {
            return;
        }
        let rows = self.notes_rows();
        let Some(newest) = self.releases.first() else { return };
        let title = format!("Version {} is available", newest.version);
        let notes = self.notes.as_mut().expect("checked");
        if notes.texture.is_none() {
            if let Some((side, pixels)) = notes.qr.as_ref() {
                notes.texture = window.texture(*side, *side, pixels).ok();
            }
        }
        let first = notes.first.min(rows.len().saturating_sub(visible_rows()));
        let p = &mut self.painter;
        p.fill(window, Rect::new(-CANVAS_W, -CANVAS_H, CANVAS_W * 3.0, CANVAS_H * 3.0), SCRIM);
        p.round_rect(window, BOX, 24.0, super::PANEL);
        let (x, y) = (BOX.x + PAD, BOX.y + PAD);
        p.text(window, &title, x, y, 44.0, TEXT, true, Align::Left);
        let installed = format!("You have {}. Install the new package the way you installed this one.", release::VERSION);
        p.text(window, &p.fit(&installed, 28.0, false, NOTES_W), x, y + 70.0, 28.0, DIM, false, Align::Left);

        let clip = p.area(Rect::new(x, NOTES_TOP, NOTES_W, NOTES_BOTTOM - NOTES_TOP));
        window.clip(Some(clip));
        for (slot, row) in rows.iter().skip(first).take(visible_rows()).enumerate() {
            let row_y = NOTES_TOP + slot as f32 * LINE_H;
            if row.bullet {
                p.text(window, "•", x + 8.0, row_y, SIZE, DIM, false, Align::Left);
            }
            p.text(window, &row.text, x + row.indent, row_y, SIZE, row.color, row.bold, Align::Left);
        }
        window.clip(None);
        if rows.len() > visible_rows() {
            let track = Rect::new(x + NOTES_W + 24.0, NOTES_TOP, 6.0, NOTES_BOTTOM - NOTES_TOP);
            let shown = visible_rows() as f32 / rows.len() as f32;
            let at = first as f32 / rows.len() as f32;
            p.round_rect(window, track, 3.0, BUTTON);
            p.round_rect(window, Rect::new(track.x, track.y + at * track.h, track.w, (shown * track.h).max(24.0)), 3.0, DIM);
        }

        let qr = Rect::new(BOX.x + BOX.w - PAD - QR_SIDE, NOTES_TOP, QR_SIDE, QR_SIDE);
        p.round_rect(window, qr.inset(-12.0), 16.0, [255, 255, 255, 255]);
        if let Some(texture) = self.notes.as_ref().and_then(|notes| notes.texture.as_ref()) {
            p.image(window, texture, qr, 255);
        }
        let caption = Rect::new(qr.x - 20.0, qr.y + qr.h + 36.0, qr.w + 40.0, 200.0);
        p.paragraph(window, "Scan for the release page and its download", caption, 24.0, DIM, Align::Center);

        p.round_rect(window, CLOSE, 14.0, ACCENT);
        p.text(window, "Close", CLOSE.x + CLOSE.w / 2.0, CLOSE.y + 15.0, 30.0, ON_ACCENT, true, Align::Center);
        let hint = if rows.len() > visible_rows() { "Up/Down  Scroll     OK  Close" } else { "OK  Close" };
        p.text(window, hint, BOX.x + BOX.w - PAD, CLOSE.y + 20.0, 26.0, DIM, false, Align::Right);
    }

    /// Developer aid: two releases newer than this build, with the notes open when `open`.
    pub(super) fn preview_update(&mut self, open: bool) {
        self.preview_library(false);
        let notes = "## What's Changed\n\
                     * Sign out from the account menu by @duffpl in https://github.com/duffpl/gfn-webos/pull/12\n\
                     * A **longer** entry, to show how a line that does not fit the column wraps onto the next one and keeps its indent\n\
                     * Faster artwork on Wi-Fi\n\n\
                     ## Fixes\n\
                     - The keyboard no longer stays open after a search\n\
                     - `HDR` badge placement on short titles\n\n\
                     **Full Changelog**: https://github.com/duffpl/gfn-webos/compare/v0.1.0...v0.3.0";
        self.releases = vec![
            Release { version: "0.3.0".into(), date: "2026-11-20".into(), notes: notes.into(), url: release::releases_page() },
            Release { version: "0.2.0".into(), date: "2026-11-02".into(), notes: String::new(), url: String::new() },
        ];
        self.library.set_update(true);
        if open {
            self.open_notes();
        }
    }
}

fn visible_rows() -> usize {
    ((NOTES_BOTTOM - NOTES_TOP) / LINE_H).floor() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_read_as_day_month_year() {
        assert_eq!(day_text("2026-10-17").as_deref(), Some("17 Oct 2026"));
        assert_eq!(day_text("2026-01-05T10:00:00Z").as_deref(), Some("5 Jan 2026"));
        assert_eq!(day_text(""), None);
    }
}
