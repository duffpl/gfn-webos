//! A game's detail screen, opened from the library grid, and its full-screen screenshot viewer.

use std::time::{SystemTime, UNIX_EPOCH};

use super::library::{button, title_case};
use super::*;
use crate::gfx::Fade;

const TITLE_Y: f32 = 120.0;
const BADGES_Y: f32 = 220.0;
const BADGE_H: f32 = 44.0;
const BADGE_SIZE: f32 = 22.0;
const BUTTONS_Y: f32 = 296.0;
const BUTTON_H: f32 = 60.0;
const DESCRIPTION_Y: f32 = 404.0;
const DESCRIPTION_SIZE: f32 = 30.0;
const DESCRIPTION_LINES: usize = 4;
const TEXT_W: f32 = 940.0;
const FACTS_Y: f32 = 604.0;
const FACT_W: f32 = 470.0;
const FACT_H: f32 = 80.0;
const THUMB_W: f32 = 320.0;
const THUMB_H: f32 = 180.0;
const THUMB_GAP: f32 = 24.0;
const THUMB_RADIUS: f32 = 10.0;
const STRIP_Y: f32 = 812.0;
const STRIP_SHOWN: usize = 5;
const VIEWER_THUMB_W: f32 = 192.0;
const VIEWER_THUMB_H: f32 = 108.0;
const VIEWER_THUMBS: usize = 7;
const VIEWER_STRIP_Y: f32 = 884.0;
/// Laid over the picture under the text, so the text reads whatever the picture is.
const SHADE: Rgba = [14, 15, 18, 235];
pub(super) const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    /// The store chip, which picks the store version to launch.
    Store,
    Play,
    Screenshots,
    Strip(usize),
}

pub(super) struct Detail {
    /// Position in `Library::games`.
    pub(super) game: usize,
    focus: Focus,
    /// The first screenshot shown in the strip.
    strip_first: usize,
    /// The viewer is open at this screenshot.
    viewer: Option<usize>,
    /// Artwork this screen asked for, freed when it closes.
    art: HashSet<String>,
    /// Of those, the full-size screenshots: about 8 MB each at 1080p, so only the viewer's
    /// current one and its two neighbours are kept.
    full: HashSet<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Step {
    Stay,
    Play,
    Close,
}

impl Detail {
    pub(super) fn new(game: usize) -> Self {
        Self { game, focus: Focus::Play, strip_first: 0, viewer: None, art: HashSet::new(), full: HashSet::new() }
    }

    fn nav(&mut self, nav: Nav, game: &mut Game) -> Step {
        let shots = game.screenshots.len();
        if let Some(index) = self.viewer {
            match nav {
                Nav::Left => self.viewer = Some(step(index, shots, -1)),
                Nav::Right => self.viewer = Some(step(index, shots, 1)),
                Nav::Back => {
                    self.viewer = None;
                    self.focus_strip(index);
                }
                _ => {}
            }
            return Step::Stay;
        }
        let picker = game.owned_variants().len() > 1;
        match (self.focus, nav) {
            (_, Nav::Back) => return Step::Close,
            (Focus::Store, Nav::Left) => cycle_store(game, -1),
            (Focus::Store, Nav::Right | Nav::Select) => cycle_store(game, 1),
            (Focus::Store, Nav::Down) => self.focus = Focus::Play,
            (Focus::Play, Nav::Select) => return Step::Play,
            (Focus::Play, Nav::Right) if shots > 0 => self.focus = Focus::Screenshots,
            (Focus::Screenshots, Nav::Left) => self.focus = Focus::Play,
            (Focus::Screenshots, Nav::Select) => self.viewer = Some(0),
            (Focus::Play | Focus::Screenshots, Nav::Up) if picker => self.focus = Focus::Store,
            (Focus::Play | Focus::Screenshots, Nav::Down) if shots > 0 => self.focus = Focus::Strip(self.strip_first),
            (Focus::Strip(i), Nav::Left) => self.focus_strip(i.saturating_sub(1)),
            (Focus::Strip(i), Nav::Right) => self.focus_strip((i + 1).min(shots - 1)),
            (Focus::Strip(_), Nav::Up) => self.focus = Focus::Play,
            (Focus::Strip(i), Nav::Select) => self.viewer = Some(i),
            _ => {}
        }
        Step::Stay
    }

    /// Focuses a thumbnail, scrolling the strip to it.
    fn focus_strip(&mut self, index: usize) {
        self.focus = Focus::Strip(index);
        if index < self.strip_first {
            self.strip_first = index;
        } else if index >= self.strip_first + STRIP_SHOWN {
            self.strip_first = index + 1 - STRIP_SHOWN;
        }
    }

    fn hint(&self) -> &'static str {
        if self.viewer.is_some() {
            return "Left/Right  Browse     Back  Close";
        }
        match self.focus {
            Focus::Store => "Left/Right  Change the store     Back  Library",
            Focus::Play => "OK  Play     Back  Library",
            Focus::Screenshots => "OK  Screenshots     Green  Play     Back  Library",
            Focus::Strip(_) => "OK  View     Green  Play     Back  Library",
        }
    }
}

/// `index` moved by `by` through `count` items, wrapping around at either end.
fn step(index: usize, count: usize, by: isize) -> usize {
    if count == 0 {
        return 0;
    }
    (index as isize + by).rem_euclid(count as isize) as usize
}

fn cycle_store(game: &mut Game, by: isize) {
    let owned = game.owned_variants();
    let current = game.launch_index().and_then(|index| owned.iter().position(|&o| o == index)).unwrap_or(0);
    if let Some(&next) = owned.get(step(current, owned.len(), by)) {
        game.select_variant(next);
    }
}

/// A store's own name for GFN's code of it.
fn store_name(code: &str) -> String {
    match code.to_ascii_uppercase().replace([' ', '-'], "_").as_str() {
        "STEAM" => "Steam".into(),
        "EPIC" | "EGS" | "EPIC_GAMES" | "EPIC_GAMES_STORE" => "Epic Games Store".into(),
        "UBISOFT" | "UPLAY" | "UBISOFT_CONNECT" => "Ubisoft Connect".into(),
        "EA" | "EA_APP" | "ORIGIN" => "EA app".into(),
        "XBOX" | "MICROSOFT" | "MICROSOFT_STORE" => "Xbox".into(),
        "GOG" => "GOG".into(),
        "BATTLENET" | "BATTLE_NET" => "Battle.net".into(),
        "" => "Unknown store".into(),
        _ => title_case(code),
    }
}

/// The tags after the store chip, for the store version that would launch.
fn badges(game: &Game) -> Vec<String> {
    let variant = game.launch_variant();
    let mut badges = Vec::new();
    for (on, name) in [(variant.is_some_and(|v| v.hdr), "HDR"), (variant.is_some_and(|v| v.rtx), "RTX"), (variant.is_some_and(|v| v.reflex), "Reflex")] {
        if on {
            badges.push(name.to_owned());
        }
    }
    badges.extend(game.rating.clone());
    badges
}

/// Days since 1970-01-01 of the date an ISO 8601 text starts with.
fn date_days(iso: &str) -> Option<i64> {
    let (year, month, day) = date_parts(iso)?;
    // Howard Hinnant's days_from_civil.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

pub(super) fn date_parts(iso: &str) -> Option<(i64, u32, u32)> {
    let date = iso.get(..10)?;
    let mut parts = date.split('-');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok().filter(|m| (1..=12).contains(m))?;
    let day = parts.next()?.parse().ok().filter(|d| (1..=31).contains(d))?;
    Some((year, month, day))
}

fn today() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| (since.as_secs() / 86_400) as i64)
}

/// "Today", "Yesterday", "5 days ago", then the date. Dates are UTC, as NVIDIA sends them.
fn last_played_text(iso: &str, today: i64) -> Option<String> {
    let days = today - date_days(iso)?;
    Some(match days {
        ..=0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=13 => format!("{days} days ago"),
        _ => {
            let (year, month, day) = date_parts(iso)?;
            format!("{day} {} {year}", MONTHS[month as usize - 1])
        }
    })
}

fn facts(game: &Game, today: i64) -> Vec<(&'static str, String)> {
    let released = game.released.as_deref().and_then(date_parts).map(|(year, _, _)| year.to_string());
    let last_played = game.last_played.as_deref().and_then(|iso| last_played_text(iso, today));
    [("Publisher", game.publisher.clone()), ("Developer", game.developer.clone()), ("Released", released), ("Last played", last_played)]
        .into_iter()
        .filter_map(|(label, value)| Some((label, value?)))
        .collect()
}

/// The first `max` of `lines`, the last of them cut short with "…" when more follow.
fn cut_lines(mut lines: Vec<String>, max: usize, fit: impl Fn(&str) -> String) -> Vec<String> {
    if lines.len() <= max || max == 0 {
        return lines;
    }
    let joined = format!("{} {}", lines[max - 1], lines[max]);
    lines.truncate(max);
    lines[max - 1] = fit(&joined);
    if !lines[max - 1].ends_with('…') {
        lines[max - 1].push('…');
    }
    lines
}

/// Where the viewer's thumbnails go: up to `VIEWER_THUMBS` around `current`, centred.
fn viewer_thumbs(current: usize, count: usize) -> Vec<(usize, Rect)> {
    let shown = count.min(VIEWER_THUMBS);
    let first = current.saturating_sub(VIEWER_THUMBS / 2).min(count - shown);
    let row = shown as f32 * (VIEWER_THUMB_W + THUMB_GAP) - THUMB_GAP;
    let left = (CANVAS_W - row) / 2.0;
    (first..first + shown)
        .map(|index| {
            let x = left + (index - first) as f32 * (VIEWER_THUMB_W + THUMB_GAP);
            (index, Rect::new(x, VIEWER_STRIP_Y, VIEWER_THUMB_W, VIEWER_THUMB_H))
        })
        .collect()
}

struct Layout {
    store: Rect,
    store_label: String,
    /// More than one owned store version, so the chip picks between them.
    picker: bool,
    play: Rect,
    screenshots: Option<Rect>,
    strip: Vec<(usize, Rect)>,
}

impl Layout {
    fn targets(&self) -> Vec<(Rect, Focus)> {
        let mut targets = vec![(self.play, Focus::Play)];
        if self.picker {
            targets.push((self.store, Focus::Store));
        }
        targets.extend(self.screenshots.map(|rect| (rect, Focus::Screenshots)));
        targets.extend(self.strip.iter().map(|&(index, rect)| (rect, Focus::Strip(index))));
        targets
    }
}

fn chip(p: &mut Painter, window: &mut Window, rect: Rect, label: &str, focused: bool) {
    if focused {
        p.halo(window, rect, 12.0, 4.0, ACCENT);
        p.round_rect(window, rect, 12.0, ACCENT);
    } else {
        p.halo(window, rect, 12.0, 2.0, DIM);
        p.round_rect(window, rect, 12.0, BACKGROUND);
    }
    let color = if focused { ON_ACCENT } else { TEXT };
    p.text(window, label, rect.x + rect.w / 2.0, rect.y + 10.0, BADGE_SIZE, color, true, Align::Center);
}

impl App {
    pub(super) fn open_detail(&mut self, game: usize) {
        self.library.detail = Some(Detail::new(game));
        self.dirty = true;
    }

    /// Back to the grid, on the same tile.
    fn close_detail(&mut self) {
        self.release_detail_art();
        self.library.detail = None;
    }

    /// Frees the detail screen's textures; drawing it again asks for them anew.
    pub(super) fn release_detail_art(&mut self) {
        let Some(detail) = &mut self.library.detail else { return };
        let urls = std::mem::take(&mut detail.art);
        detail.full.clear();
        detail.viewer = None;
        self.library.art.retain(|url, _| !urls.contains(url));
    }

    /// Frees the full-size screenshots other than `keep`.
    fn keep_full(&mut self, keep: &HashSet<String>) {
        let Some(detail) = &mut self.library.detail else { return };
        let dropped: Vec<String> = detail.full.iter().filter(|url| !keep.contains(*url)).cloned().collect();
        for url in dropped {
            detail.full.remove(&url);
            detail.art.remove(&url);
            self.library.art.remove(&url);
        }
    }

    fn want_detail_art(&mut self, url: &str, w: f32, h: f32, radius: f32, full: bool) {
        if let Some(detail) = &mut self.library.detail {
            detail.art.insert(url.to_owned());
            if full {
                detail.full.insert(url.to_owned());
            }
        }
        self.want_art(url, w, h, radius);
    }

    fn px(&self, units: f32) -> u32 {
        (units * self.painter.scale()).round() as u32
    }

    fn thumb_url(&self, shot: &str) -> String {
        art::sized(shot, self.px(THUMB_W))
    }

    fn full_url(&self, shot: &str) -> String {
        art::sized(shot, self.px(CANVAS_W))
    }

    pub(super) fn detail_nav(&mut self, nav: Nav) {
        let Some(detail) = self.library.detail.as_mut() else { return };
        let index = detail.game;
        match detail.nav(nav, &mut self.library.games[index]) {
            Step::Stay => {}
            Step::Play => self.launch_library_game(index),
            Step::Close => self.close_detail(),
        }
        self.dirty = true;
    }

    fn detail_layout(&self) -> Option<Layout> {
        let detail = self.library.detail.as_ref()?;
        let game = &self.library.games[detail.game];
        let picker = game.owned_variants().len() > 1;
        let store = store_name(game.launch_variant().map_or("", |v| v.store.as_str()));
        let store_label = if picker { format!("‹  {store}  ›") } else { store };
        let store = Rect::new(MARGIN, BADGES_Y, self.painter.text_width(&store_label, BADGE_SIZE, true) + 40.0, BADGE_H);
        let play = Rect::new(MARGIN, BUTTONS_Y, 240.0, BUTTON_H);
        let screenshots = (!game.screenshots.is_empty()).then(|| {
            Rect::new(play.x + play.w + 20.0, BUTTONS_Y, self.painter.text_width("Screenshots", 28.0, true) + 64.0, BUTTON_H)
        });
        let last = (detail.strip_first + STRIP_SHOWN).min(game.screenshots.len());
        let strip = (detail.strip_first..last)
            .map(|index| {
                let x = MARGIN + (index - detail.strip_first) as f32 * (THUMB_W + THUMB_GAP);
                (index, Rect::new(x, STRIP_Y, THUMB_W, THUMB_H))
            })
            .collect();
        Some(Layout { store, store_label, picker, play, screenshots, strip })
    }

    pub(super) fn detail_hover(&mut self) {
        let (x, y) = self.mouse;
        let Some(layout) = self.detail_layout() else { return };
        let Some(detail) = &mut self.library.detail else { return };
        if detail.viewer.is_some() {
            return;
        }
        if let Some((_, focus)) = layout.targets().into_iter().find(|(rect, _)| rect.contains(x, y)) {
            if detail.focus != focus {
                detail.focus = focus;
                self.dirty = true;
            }
        }
    }

    pub(super) fn detail_click(&mut self) {
        let (x, y) = self.mouse;
        let Some(detail) = &mut self.library.detail else { return };
        if let Some(current) = detail.viewer {
            let count = self.library.games[detail.game].screenshots.len();
            match viewer_thumbs(current, count).into_iter().find(|(_, rect)| rect.contains(x, y)) {
                Some((index, _)) => detail.viewer = Some(index),
                None => self.detail_nav(if x < CANVAS_W / 3.0 { Nav::Left } else { Nav::Right }),
            }
            self.dirty = true;
            return;
        }
        let Some(layout) = self.detail_layout() else { return };
        if let Some((_, focus)) = layout.targets().into_iter().find(|(rect, _)| rect.contains(x, y)) {
            if let Some(detail) = &mut self.library.detail {
                detail.focus = focus;
            }
            self.detail_nav(Nav::Select);
        }
    }

    pub(super) fn draw_detail(&mut self, window: &mut Window) {
        let Some(detail) = &self.library.detail else { return };
        if detail.viewer.is_some() {
            self.draw_viewer(window);
            return;
        }
        let (focus, hint) = (detail.focus, detail.hint());
        let game = self.library.games[detail.game].clone();
        self.keep_full(&HashSet::new());
        let Some(layout) = self.detail_layout() else { return };

        let hero = game.hero.as_deref().or(game.screenshots.first().map(String::as_str)).map(|url| self.full_url(url));
        let thumbs: Vec<(Rect, String, bool)> =
            layout.strip.iter().map(|&(index, rect)| (rect, self.thumb_url(&game.screenshots[index]), focus == Focus::Strip(index))).collect();
        let mut on_screen = HashSet::new();
        if let Some(url) = &hero {
            self.want_detail_art(url, CANVAS_W, CANVAS_H, 0.0, false);
            on_screen.insert(url.clone());
        }
        for (_, url, _) in &thumbs {
            self.want_detail_art(url, THUMB_W, THUMB_H, THUMB_RADIUS, false);
            on_screen.insert(url.clone());
        }
        self.show_art(on_screen);

        let genres = game.genres.iter().map(|genre| self.library.genre_label(genre)).collect::<Vec<_>>().join("  ·  ");
        let facts = facts(&game, today());
        let p = &mut self.painter;
        let art = &self.library.art;
        let ready = |url: &str| match art.get(url) {
            Some(Art::Ready(texture)) => Some(texture),
            _ => None,
        };
        if let Some(texture) = hero.as_deref().and_then(ready) {
            p.image(window, texture, Rect::new(0.0, 0.0, CANVAS_W, CANVAS_H), 255);
        }
        p.fill(window, Rect::new(0.0, 0.0, 720.0, CANVAS_H), SHADE);
        p.fade(window, Rect::new(720.0, 0.0, 900.0, CANVAS_H), SHADE, Fade::Right);
        p.fade(window, Rect::new(0.0, 640.0, CANVAS_W, CANVAS_H - 640.0), SHADE, Fade::Up);

        let title = p.fit(&game.title, 64.0, true, CANVAS_W - 2.0 * MARGIN);
        p.text(window, &title, MARGIN, TITLE_Y, 64.0, TEXT, true, Align::Left);

        chip(p, window, layout.store, &layout.store_label, focus == Focus::Store);
        let mut x = layout.store.x + layout.store.w + 16.0;
        for badge in badges(&game) {
            let rect = Rect::new(x, BADGES_Y, p.text_width(&badge, BADGE_SIZE, true) + 32.0, BADGE_H);
            chip(p, window, rect, &badge, false);
            x += rect.w + 12.0;
        }
        let genres = p.fit(&genres, 26.0, false, CANVAS_W - MARGIN - x - 8.0);
        p.text(window, &genres, x + 8.0, BADGES_Y + 8.0, 26.0, DIM, false, Align::Left);

        button(p, window, layout.play, "Play", focus == Focus::Play);
        if let Some(rect) = layout.screenshots {
            button(p, window, rect, "Screenshots", focus == Focus::Screenshots);
        }

        if let Some(description) = &game.description {
            let lines = cut_lines(p.wrap(description, DESCRIPTION_SIZE, TEXT_W), DESCRIPTION_LINES, |line| {
                p.fit(line, DESCRIPTION_SIZE, false, TEXT_W)
            });
            for (i, line) in lines.iter().enumerate() {
                let y = DESCRIPTION_Y + i as f32 * DESCRIPTION_SIZE * 1.4;
                p.text(window, line, MARGIN, y, DESCRIPTION_SIZE, [214, 216, 221, 255], false, Align::Left);
            }
        }
        for (i, (label, value)) in facts.iter().enumerate() {
            let (x, y) = (MARGIN + (i % 2) as f32 * FACT_W, FACTS_Y + (i / 2) as f32 * FACT_H);
            p.text(window, label, x, y, 24.0, DIM, false, Align::Left);
            let value = p.fit(value, 28.0, false, FACT_W - 32.0);
            p.text(window, &value, x, y + 32.0, 28.0, TEXT, false, Align::Left);
        }

        if let (Some((first, _)), Some((last, _))) = (layout.strip.first(), layout.strip.last()) {
            let count = game.screenshots.len();
            let label = if count > STRIP_SHOWN { format!("Screenshots  ·  {}–{} of {count}", first + 1, last + 1) } else { "Screenshots".into() };
            p.text(window, &label, MARGIN, STRIP_Y - 44.0, 24.0, DIM, false, Align::Left);
        }
        for (rect, url, focused) in &thumbs {
            if *focused {
                p.halo(window, *rect, THUMB_RADIUS, 5.0, ACCENT);
            }
            p.round_rect(window, *rect, THUMB_RADIUS, TILE);
            if let Some(texture) = ready(url) {
                p.image(window, texture, *rect, 255);
            }
        }
        footer(p, window, hint);
    }

    fn draw_viewer(&mut self, window: &mut Window) {
        let Some(detail) = &self.library.detail else { return };
        let Some(current) = detail.viewer else { return };
        let hint = detail.hint();
        let game = &self.library.games[detail.game];
        let (title, shots) = (game.title.clone(), game.screenshots.clone());
        let count = shots.len();
        if count == 0 {
            return;
        }
        // The current one first: the artwork thread works in order.
        let mut full: Vec<String> = Vec::new();
        for index in [current, step(current, count, 1), step(current, count, -1)] {
            let url = self.full_url(&shots[index]);
            if !full.contains(&url) {
                full.push(url);
            }
        }
        self.keep_full(&full.iter().cloned().collect());
        for url in &full {
            self.want_detail_art(url, CANVAS_W, CANVAS_H, 0.0, true);
        }
        let thumbs: Vec<(Rect, String, bool)> =
            viewer_thumbs(current, count).into_iter().map(|(index, rect)| (rect, self.thumb_url(&shots[index]), index == current)).collect();
        for (_, url, _) in &thumbs {
            self.want_detail_art(url, THUMB_W, THUMB_H, THUMB_RADIUS, false);
        }
        self.show_art(full.iter().chain(thumbs.iter().map(|(_, url, _)| url)).cloned().collect());

        let p = &mut self.painter;
        let art = &self.library.art;
        let ready = |url: &str| match art.get(url) {
            Some(Art::Ready(texture)) => Some(texture),
            _ => None,
        };
        match ready(&full[0]) {
            Some(texture) => p.image(window, texture, Rect::new(0.0, 0.0, CANVAS_W, CANVAS_H), 255),
            None => p.text(window, "Loading…", CANVAS_W / 2.0, 500.0, 34.0, DIM, false, Align::Center),
        }
        p.fade(window, Rect::new(0.0, 0.0, CANVAS_W, 200.0), [0, 0, 0, 200], Fade::Down);
        p.fade(window, Rect::new(0.0, 760.0, CANVAS_W, CANVAS_H - 760.0), [0, 0, 0, 220], Fade::Up);
        let title = p.fit(&title, 40.0, true, CANVAS_W - 2.0 * MARGIN - 200.0);
        p.text(window, &title, MARGIN, 52.0, 40.0, TEXT, true, Align::Left);
        p.text(window, &format!("{} / {count}", current + 1), CANVAS_W - MARGIN, 58.0, 32.0, TEXT, false, Align::Right);
        for (rect, url, focused) in &thumbs {
            if *focused {
                p.halo(window, *rect, 8.0, 4.0, ACCENT);
            }
            p.round_rect(window, *rect, 8.0, TILE);
            if let Some(texture) = ready(url) {
                p.image(window, texture, *rect, if *focused { 255 } else { 170 });
            }
        }
        footer(p, window, hint);
    }

    /// Developer aid: Cyberpunk 2077's detail screen, with its catalog text and the artwork downloaded
    /// into `.claude/design/images` (not committed); without it the screen shows no pictures.
    pub(super) fn preview_detail(&mut self, viewer: bool) {
        self.preview_library(false);
        let images = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.claude/design/images/cyberpunk-2077");
        let game = &mut self.library.games[0];
        game.hero = Some(format!("{images}/marquee_hero_image.jpg"));
        game.screenshots = (1..=12).map(|i| format!("{images}/screenshots_{i:02}.jpg")).collect();
        game.description = Some(
            "Cyberpunk 2077 is an open-world, action-adventure RPG set in the dark future of Night City — a dangerous \
             megalopolis obsessed with power, glamor, and ceaseless body modification."
                .into(),
        );
        game.rating = Some("PEGI 18".into());
        game.released = Some("2020-12-10T00:00:00.000000+0000".into());
        game.last_played = Some("2026-10-16T19:40:00.000Z".into());
        game.publisher = Some("CD PROJEKT RED".into());
        game.developer = Some("CD PROJEKT RED".into());
        let variant = |id: &str, store: &str, selected: bool, in_library: bool| account::Variant {
            id: id.into(),
            store: store.into(),
            selected,
            in_library,
            hdr: true,
            rtx: true,
            reflex: true,
        };
        game.variants = vec![variant("1", "STEAM", true, true), variant("2", "GOG", false, true), variant("3", "EPIC", false, false)];
        let mut detail = Detail::new(0);
        if viewer {
            detail.viewer = Some(3);
        }
        self.library.detail = Some(detail);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(shots: usize) -> Game {
        Game {
            title: "T".into(),
            screenshots: (0..shots).map(|i| format!("shot {i}")).collect(),
            variants: vec![
                account::Variant { id: "1".into(), store: "STEAM".into(), selected: true, in_library: true, ..Default::default() },
                account::Variant { id: "2".into(), store: "EPIC".into(), in_library: false, ..Default::default() },
                account::Variant { id: "3".into(), store: "GOG".into(), in_library: true, ..Default::default() },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn last_played_reads_as_days_then_a_date() {
        let today = date_days("2026-10-17").unwrap();
        assert_eq!(last_played_text("2026-10-17T08:00:00Z", today).as_deref(), Some("Today"));
        assert_eq!(last_played_text("2026-10-18T00:30:00Z", today).as_deref(), Some("Today"));
        assert_eq!(last_played_text("2026-10-16T23:59:00Z", today).as_deref(), Some("Yesterday"));
        assert_eq!(last_played_text("2026-10-10", today).as_deref(), Some("7 days ago"));
        assert_eq!(last_played_text("2026-02-28T10:00:00Z", today).as_deref(), Some("28 Feb 2026"));
        assert_eq!(last_played_text("soon", today), None);
        assert_eq!(date_days("1970-01-01"), Some(0));
        assert_eq!(date_days("2000-03-01"), Some(11_017));
    }

    #[test]
    fn badges_follow_the_launching_store_and_the_rating() {
        let mut game = game(0);
        game.variants[0].hdr = true;
        game.variants[2].rtx = true;
        game.rating = Some("PEGI 16".into());
        assert_eq!(badges(&game), ["HDR", "PEGI 16"]);
        game.select_variant(2);
        assert_eq!(badges(&game), ["RTX", "PEGI 16"]);
        assert_eq!(store_name("EPIC"), "Epic Games Store");
        assert_eq!(store_name("NEW_STORE"), "New Store");
    }

    #[test]
    fn facts_skip_what_is_unknown() {
        let game = Game { publisher: Some("Valve".into()), released: Some("2011-04-18T00:00:00Z".into()), ..Default::default() };
        assert_eq!(facts(&game, 0), [("Publisher", "Valve".to_owned()), ("Released", "2011".to_owned())]);
    }

    #[test]
    fn a_long_description_is_cut_at_the_last_line() {
        let lines = |n: usize| (0..n).map(|i| format!("line {i}")).collect::<Vec<_>>();
        assert_eq!(cut_lines(lines(3), 4, |s| s.to_owned()), lines(3));
        let cut = cut_lines(lines(6), 4, |s| format!("{}…", &s[..8]));
        assert_eq!(cut, ["line 0", "line 1", "line 2", "line 3 l…"]);
        assert_eq!(cut_lines(lines(5), 4, |s| s.to_owned())[3], "line 3 line 4…");
    }

    #[test]
    fn the_viewer_wraps_around_and_back_returns_to_its_thumbnail() {
        let mut game = game(8);
        let mut detail = Detail::new(0);
        assert_eq!(detail.nav(Nav::Down, &mut game), Step::Stay);
        assert_eq!(detail.focus, Focus::Strip(0));
        assert_eq!(detail.nav(Nav::Select, &mut game), Step::Stay);
        detail.nav(Nav::Left, &mut game);
        assert_eq!(detail.viewer, Some(7));
        detail.nav(Nav::Right, &mut game);
        detail.nav(Nav::Right, &mut game);
        assert_eq!(detail.viewer, Some(1));
        detail.viewer = Some(6);
        assert_eq!(detail.nav(Nav::Back, &mut game), Step::Stay);
        assert_eq!((detail.viewer, detail.focus, detail.strip_first), (None, Focus::Strip(6), 2));
        assert_eq!(detail.nav(Nav::Back, &mut game), Step::Close);
    }

    #[test]
    fn the_store_chip_cycles_owned_stores_and_play_launches() {
        let mut game = game(0);
        let mut detail = Detail::new(0);
        assert_eq!(detail.nav(Nav::Right, &mut game), Step::Stay);
        assert_eq!(detail.focus, Focus::Play);
        detail.nav(Nav::Up, &mut game);
        assert_eq!(detail.focus, Focus::Store);
        detail.nav(Nav::Right, &mut game);
        assert_eq!(game.launch_variant().map(|v| v.id.as_str()), Some("3"));
        detail.nav(Nav::Right, &mut game);
        assert_eq!(game.launch_variant().map(|v| v.id.as_str()), Some("1"));
        detail.nav(Nav::Left, &mut game);
        assert_eq!(game.launch_variant().map(|v| v.id.as_str()), Some("3"));
        detail.nav(Nav::Down, &mut game);
        assert_eq!(detail.nav(Nav::Select, &mut game), Step::Play);
    }

    #[test]
    fn viewer_thumbnails_stay_centred_and_on_screen() {
        let indices = |current, count| viewer_thumbs(current, count).into_iter().map(|(i, _)| i).collect::<Vec<_>>();
        assert_eq!(indices(0, 12), [0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(indices(6, 12), [3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(indices(11, 12), [5, 6, 7, 8, 9, 10, 11]);
        assert_eq!(indices(1, 2), [0, 1]);
        let row = viewer_thumbs(0, 2);
        assert!((row[0].1.x + row[1].1.x + VIEWER_THUMB_W - CANVAS_W).abs() < 0.01);
    }
}
