//! The home screen: the game grid, and the search, genre filter and sort order over it.

use serde_json::json;

use super::detail::Detail;
use super::*;

const COLUMNS: usize = 6;
pub(super) const TILE_W: f32 = 240.0;
pub(super) const TILE_H: f32 = 320.0;
const TILE_RADIUS: f32 = 12.0;
const ROW_H: f32 = TILE_H + 84.0;
const TOOLBAR_Y: f32 = 150.0;
const TOOLBAR_H: f32 = 60.0;
const SEARCH_W: f32 = 520.0;
const GRID_TOP: f32 = 260.0;
const GRID_BOTTOM: f32 = 990.0;
const GENRE_MENU_W: f32 = 520.0;
const GENRE_ROW_H: f32 = 64.0;
const GENRE_ROWS_SHOWN: usize = 10;
/// Artwork textures kept before those far from the viewport are dropped (about 300 KB each at
/// 1080p); dropped ones reload from the core's disk cache when scrolled back into view.
const ART_KEPT: usize = 96;
/// Rows either side of the visible ones whose artwork survives eviction.
const ART_MARGIN_ROWS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Header {
    Settings,
    Controllers,
}

const HEADER_BUTTONS: [Header; 2] = [Header::Settings, Header::Controllers];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Search,
    Genre,
    Sort,
}

const TOOLS: [Tool; 3] = [Tool::Search, Tool::Genre, Tool::Sort];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Header(usize),
    Toolbar(usize),
    Grid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sort {
    /// The server's order: most recently played first.
    Recent,
    Title,
}

/// What a press on the library asks the app to do.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Nothing,
    /// The game at this position in `Library::games`.
    Details(usize),
    Open(Header),
    Type,
    Exit,
}

pub(super) enum Art {
    Requested,
    Ready(Texture),
    Missing,
}

pub(super) struct Library {
    pub(super) games: Vec<Game>,
    /// Positions in `games` that the search and genre leave, in the chosen order.
    view: Vec<usize>,
    query: String,
    genre: Option<String>,
    sort: Sort,
    genre_names: HashMap<String, String>,
    focus: Focus,
    /// The selected tile, as a position in `view`.
    selected: usize,
    /// The genre menu is open, with this row focused.
    genre_menu: Option<usize>,
    scroll: f32,
    pub(super) art: HashMap<String, Art>,
    pub(super) pending: HashMap<String, art::Pixels>,
    /// The artwork last reported to the artwork thread as on screen.
    on_screen: HashSet<String>,
    /// Where the sort order is kept between launches.
    prefs: PathBuf,
    /// The game detail screen, over the grid.
    pub(super) detail: Option<Detail>,
}

impl Library {
    pub(super) fn new(prefs: PathBuf) -> Self {
        let stored = std::fs::read(&prefs).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        let sort = if stored.is_some_and(|v| v["sort"] == "title") { Sort::Title } else { Sort::Recent };
        Self {
            games: Vec::new(),
            view: Vec::new(),
            query: String::new(),
            genre: None,
            sort,
            genre_names: HashMap::new(),
            focus: Focus::Grid,
            selected: 0,
            genre_menu: None,
            scroll: 0.0,
            art: HashMap::new(),
            pending: HashMap::new(),
            on_screen: HashSet::new(),
            prefs,
            detail: None,
        }
    }

    pub(super) fn set_games(&mut self, games: Vec<Game>) {
        self.games = games;
        self.detail = None;
        self.view.clear();
        self.refresh();
    }

    pub(super) fn set_genre_names(&mut self, names: HashMap<String, String>) {
        self.genre_names = names;
    }

    pub(super) fn type_text(&mut self, text: &str) {
        self.query.push_str(text);
        self.refresh();
    }

    pub(super) fn erase(&mut self) {
        self.query.pop();
        self.refresh();
    }

    /// Rebuilds the view, keeping the selected game selected if it is still in it.
    fn refresh(&mut self) {
        let current = self.view.get(self.selected).copied();
        self.view = view(&self.games, &self.query, self.genre.as_deref(), self.sort);
        self.selected = current.and_then(|game| self.view.iter().position(|&i| i == game)).unwrap_or(0);
        if self.view.is_empty() && self.focus == Focus::Grid {
            self.focus = Focus::Toolbar(0);
        }
    }

    pub(super) fn genre_label(&self, code: &str) -> String {
        self.genre_names.get(code).cloned().unwrap_or_else(|| title_case(code))
    }

    /// "All genres" first, then each genre in the library with its number of games, most first.
    /// Counted over the whole library so they stay put while a search is typed.
    fn genre_rows(&self) -> Vec<(Option<String>, usize)> {
        let mut rows = vec![(None, self.games.len())];
        rows.extend(genre_counts(&self.games).into_iter().map(|(genre, count)| (Some(genre), count)));
        rows
    }

    /// The game a Play press starts: the one whose details are open, or the selected tile while
    /// the grid has focus.
    fn play(&self) -> Option<usize> {
        if let Some(detail) = &self.detail {
            return Some(detail.game);
        }
        if self.focus != Focus::Grid || self.genre_menu.is_some() {
            return None;
        }
        self.view.get(self.selected).copied()
    }

    fn toggle_sort(&mut self) {
        self.sort = match self.sort {
            Sort::Recent => Sort::Title,
            Sort::Title => Sort::Recent,
        };
        self.refresh();
        let stored = json!({ "sort": if self.sort == Sort::Title { "title" } else { "recent" } });
        if let Err(error) = std::fs::write(&self.prefs, stored.to_string()) {
            eprintln!("saving the library order: {error}");
        }
    }

    fn nav(&mut self, nav: Nav) -> Outcome {
        if let Some(row) = self.genre_menu {
            let rows = self.genre_rows();
            match nav {
                Nav::Up => self.genre_menu = Some(row.saturating_sub(1)),
                Nav::Down => self.genre_menu = Some((row + 1).min(rows.len() - 1)),
                Nav::Select => {
                    self.genre = rows.into_iter().nth(row).and_then(|(genre, _)| genre);
                    self.genre_menu = None;
                    self.refresh();
                }
                Nav::Back => self.genre_menu = None,
                Nav::Left | Nav::Right => {}
            }
            return Outcome::Nothing;
        }
        let count = self.view.len();
        match self.focus {
            Focus::Header(index) => match nav {
                Nav::Left => self.focus = Focus::Header(index.saturating_sub(1)),
                Nav::Right => self.focus = Focus::Header((index + 1).min(HEADER_BUTTONS.len() - 1)),
                Nav::Down => self.focus = Focus::Toolbar(0),
                Nav::Select => return Outcome::Open(HEADER_BUTTONS[index]),
                Nav::Back => return Outcome::Exit,
                Nav::Up => {}
            },
            Focus::Toolbar(index) => match nav {
                Nav::Left => self.focus = Focus::Toolbar(index.saturating_sub(1)),
                Nav::Right => self.focus = Focus::Toolbar((index + 1).min(TOOLS.len() - 1)),
                Nav::Up => self.focus = Focus::Header(0),
                Nav::Down if count > 0 => self.focus = Focus::Grid,
                Nav::Down => {}
                Nav::Select => match TOOLS[index] {
                    Tool::Search => return Outcome::Type,
                    Tool::Genre => {
                        let current = self.genre_rows().iter().position(|(genre, _)| *genre == self.genre);
                        self.genre_menu = Some(current.unwrap_or(0));
                    }
                    Tool::Sort => self.toggle_sort(),
                },
                Nav::Back if TOOLS[index] == Tool::Search && !self.query.is_empty() => {
                    self.query.clear();
                    self.refresh();
                }
                Nav::Back => return Outcome::Exit,
            },
            Focus::Grid => {
                let selected = &mut self.selected;
                match nav {
                    Nav::Up if *selected < COLUMNS => self.focus = Focus::Toolbar(0),
                    Nav::Up => *selected -= COLUMNS,
                    Nav::Left if *selected % COLUMNS > 0 => *selected -= 1,
                    Nav::Right if *selected % COLUMNS + 1 < COLUMNS && *selected + 1 < count => *selected += 1,
                    Nav::Down if *selected + COLUMNS < count => *selected += COLUMNS,
                    // The last row may be short: moving down from above it lands on its last tile.
                    Nav::Down if *selected / COLUMNS + 1 < count.div_ceil(COLUMNS) => *selected = count - 1,
                    Nav::Select => return self.play().map_or(Outcome::Nothing, Outcome::Details),
                    Nav::Back => return Outcome::Exit,
                    _ => {}
                }
            }
        }
        Outcome::Nothing
    }

    fn hint(&self, typing: bool) -> &'static str {
        if typing {
            return if TV { "Type to search     Back  Done" } else { "Type to search     Enter  Done" };
        }
        if self.genre_menu.is_some() {
            return "Up/Down  Move     OK  Apply     Back  Close";
        }
        match self.focus {
            Focus::Grid => "OK  Details     Green  Play     Back  Exit",
            Focus::Header(_) => "OK  Open     Back  Exit",
            Focus::Toolbar(index) => match TOOLS[index] {
                Tool::Search if self.query.is_empty() => "OK  Search     Back  Exit",
                Tool::Search => "OK  Edit the search     Back  Clear it",
                Tool::Genre => "OK  Choose a genre     Back  Exit",
                Tool::Sort => "OK  Change the order     Back  Exit",
            },
        }
    }

    /// Scrolls towards the selected row; true while still moving.
    fn follow_selection(&mut self) -> bool {
        if self.view.is_empty() {
            self.scroll = 0.0;
            return false;
        }
        let row = (self.selected / COLUMNS) as f32;
        let visible = GRID_BOTTOM - GRID_TOP;
        let (top, bottom) = (row * ROW_H, row * ROW_H + ROW_H);
        let mut target = self.scroll;
        if top < target {
            target = top;
        } else if bottom > target + visible {
            target = bottom - visible;
        }
        let delta = target - self.scroll;
        if delta.abs() > 0.5 {
            self.scroll += delta * 0.25;
            true
        } else {
            self.scroll = target;
            false
        }
    }
}

/// Every word of `query` appears somewhere in `search_text` (already lower-case).
fn matches(search_text: &str, query: &str) -> bool {
    query.split_whitespace().all(|word| search_text.contains(&word.to_lowercase()))
}

fn view(games: &[Game], query: &str, genre: Option<&str>, sort: Sort) -> Vec<usize> {
    let mut view: Vec<usize> = games
        .iter()
        .enumerate()
        .filter(|(_, game)| genre.is_none_or(|genre| game.genres.iter().any(|g| g == genre)))
        .filter(|(_, game)| matches(&game.search_text, query))
        .map(|(index, _)| index)
        .collect();
    if sort == Sort::Title {
        view.sort_by_cached_key(|&index| games[index].title.to_lowercase());
    }
    view
}

/// Genres with their number of games, most games first.
fn genre_counts(games: &[Game]) -> Vec<(String, usize)> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for game in games {
        let unique: HashSet<&str> = game.genres.iter().map(String::as_str).collect();
        for genre in unique {
            *counts.entry(genre).or_default() += 1;
        }
    }
    let mut counts: Vec<(String, usize)> = counts.into_iter().map(|(genre, count)| (genre.to_owned(), count)).collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
}

/// A readable name for a genre code NVIDIA gave no label for: `FIGHTING_GAME` reads "Fighting Game".
pub(super) fn title_case(code: &str) -> String {
    code.split(['_', ' '])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars.flat_map(char::to_lowercase)).collect())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

struct GenreRow {
    rect: Rect,
    /// Position in `Library::genre_rows`.
    index: usize,
    label: String,
    count: usize,
    active: bool,
}

impl App {
    pub(super) fn library_nav(&mut self, nav: Nav) {
        if self.library.detail.is_some() {
            self.detail_nav(nav);
            return;
        }
        match self.library.nav(nav) {
            Outcome::Nothing => {}
            Outcome::Details(index) => self.open_detail(index),
            Outcome::Open(button) => self.open_header(button),
            Outcome::Type => self.open_keyboard(),
            Outcome::Exit => self.menu = Some(exit_menu()),
        }
    }

    /// Green on the remote, or X / Square on a controller.
    pub(super) fn library_play(&mut self) {
        if let Some(index) = self.library.play() {
            self.launch_library_game(index);
        }
        self.dirty = true;
    }

    pub(super) fn launch_library_game(&mut self, index: usize) {
        // A stream needs the memory more than a detail screen that reloads from the disk cache.
        self.release_detail_art();
        let game = self.library.games[index].clone();
        self.launch(&game);
    }

    /// Input while the TV's keyboard is up for the search field.
    pub(super) fn library_typing(&mut self, event: InputEvent) {
        match event {
            InputEvent::Text(text) => self.library.type_text(text.as_str()),
            InputEvent::Key { scancode: SCANCODE_BACKSPACE, pressed: true, .. } => self.library.erase(),
            InputEvent::Key { scancode, keycode, pressed: true, repeat: false, .. }
                if is_remote_back(scancode, keycode)
                    || is_remote_key(scancode, keycode, KEYCODE_GREEN)
                    || matches!(scancode, SCANCODE_ESCAPE | SCANCODE_RETURN | SCANCODE_KP_ENTER) =>
            {
                self.close_keyboard();
            }
            InputEvent::ControllerButton { button: 0 | 1, pressed: true, .. } | InputEvent::MouseButton { button: 1, pressed: true } => {
                self.close_keyboard();
            }
            _ => return,
        }
        self.dirty = true;
    }

    fn tile_rect(&self, position: usize) -> Rect {
        let gap = (CANVAS_W - 2.0 * MARGIN - COLUMNS as f32 * TILE_W) / (COLUMNS as f32 - 1.0);
        let (row, col) = (position / COLUMNS, position % COLUMNS);
        Rect::new(MARGIN + col as f32 * (TILE_W + gap), GRID_TOP + row as f32 * ROW_H - self.library.scroll, TILE_W, TILE_H)
    }

    /// Left to right, ending beside the user's name.
    fn header_buttons(&self) -> Vec<(Rect, String, Header)> {
        let mut right = CANVAS_W - MARGIN - self.painter.text_width(&self.user, 30.0, false) - 40.0;
        let mut buttons: Vec<_> = HEADER_BUTTONS
            .iter()
            .rev()
            .map(|&button| {
                let label = match button {
                    Header::Settings => "Settings".to_owned(),
                    Header::Controllers => match self.controllers.connected() {
                        0 => "Controllers".into(),
                        n => format!("Controllers · {n} connected"),
                    },
                };
                let w = self.painter.text_width(&label, 28.0, true) + 64.0;
                right -= w;
                let rect = Rect::new(right, 58.0, w, 60.0);
                right -= 20.0;
                (rect, label, button)
            })
            .collect();
        buttons.reverse();
        buttons
    }

    /// In `TOOLS` order.
    fn toolbar_buttons(&self) -> Vec<(Rect, String)> {
        let library = &self.library;
        let search = if library.query.is_empty() { "Search games".to_owned() } else { library.query.clone() };
        let genre = library.genre.as_deref().map_or_else(|| "All".to_owned(), |genre| library.genre_label(genre));
        let sort = match library.sort {
            Sort::Recent => "Recently played",
            Sort::Title => "A–Z",
        };
        let mut buttons = vec![(Rect::new(MARGIN, TOOLBAR_Y, SEARCH_W, TOOLBAR_H), search)];
        let mut x = MARGIN + SEARCH_W + 20.0;
        for label in [format!("Genre: {genre}  ⌄"), format!("Sort: {sort}")] {
            let w = self.painter.text_width(&label, 28.0, true) + 64.0;
            buttons.push((Rect::new(x, TOOLBAR_Y, w, TOOLBAR_H), label));
            x += w + 20.0;
        }
        buttons
    }

    /// The open genre menu's rows on screen, scrolled to keep the focused one in view.
    fn genre_menu_rows(&self) -> Vec<GenreRow> {
        let Some(focus) = self.library.genre_menu else { return Vec::new() };
        let anchor = self.toolbar_buttons()[1].0;
        let first = (focus + 1).saturating_sub(GENRE_ROWS_SHOWN);
        let top = anchor.y + anchor.h + 24.0;
        self.library
            .genre_rows()
            .into_iter()
            .enumerate()
            .skip(first)
            .take(GENRE_ROWS_SHOWN)
            .enumerate()
            .map(|(slot, (index, (genre, count)))| GenreRow {
                rect: Rect::new(anchor.x, top + slot as f32 * GENRE_ROW_H, GENRE_MENU_W, GENRE_ROW_H - 8.0),
                index,
                label: genre.as_deref().map_or_else(|| "All genres".to_owned(), |genre| self.library.genre_label(genre)),
                count,
                active: genre == self.library.genre,
            })
            .collect()
    }

    pub(super) fn open_header(&mut self, button: Header) {
        match button {
            Header::Settings => {
                self.settings_focus = 0;
                self.screen = Screen::Settings;
            }
            Header::Controllers => self.open_controllers(),
        }
        self.dirty = true;
    }

    pub(super) fn library_hover(&mut self) {
        if self.library.detail.is_some() {
            self.detail_hover();
            return;
        }
        let (x, y) = self.mouse;
        let before = (self.library.focus, self.library.selected, self.library.genre_menu);
        if self.library.genre_menu.is_some() {
            if let Some(row) = self.genre_menu_rows().into_iter().find(|row| row.rect.contains(x, y)) {
                self.library.genre_menu = Some(row.index);
            }
        } else if let Some(index) = self.header_buttons().iter().position(|(rect, _, _)| rect.contains(x, y)) {
            self.library.focus = Focus::Header(index);
        } else if let Some(index) = self.toolbar_buttons().iter().position(|(rect, _)| rect.contains(x, y)) {
            self.library.focus = Focus::Toolbar(index);
        } else if (GRID_TOP - 20.0..GRID_BOTTOM).contains(&y) {
            if let Some(position) = (0..self.library.view.len()).find(|&i| self.tile_rect(i).contains(x, y)) {
                (self.library.focus, self.library.selected) = (Focus::Grid, position);
            }
        }
        self.dirty |= before != (self.library.focus, self.library.selected, self.library.genre_menu);
    }

    pub(super) fn library_click(&mut self) {
        if self.library.detail.is_some() {
            self.detail_click();
            return;
        }
        let (x, y) = self.mouse;
        if self.library.genre_menu.is_some() {
            match self.genre_menu_rows().into_iter().find(|row| row.rect.contains(x, y)) {
                Some(row) => {
                    self.library.genre_menu = Some(row.index);
                    self.library_nav(Nav::Select);
                }
                None => self.library.genre_menu = None,
            }
            return;
        }
        if let Some((_, _, button)) = self.header_buttons().into_iter().find(|(rect, _, _)| rect.contains(x, y)) {
            self.open_header(button);
        } else if let Some(index) = self.toolbar_buttons().iter().position(|(rect, _)| rect.contains(x, y)) {
            self.library.focus = Focus::Toolbar(index);
            self.library_nav(Nav::Select);
        } else if (GRID_TOP - 20.0..GRID_BOTTOM).contains(&y) {
            if let Some(position) = (0..self.library.view.len()).find(|&i| self.tile_rect(i).contains(x, y)) {
                (self.library.focus, self.library.selected) = (Focus::Grid, position);
                self.library_nav(Nav::Select);
            }
        }
    }

    pub(super) fn library_moving(&mut self) -> bool {
        self.library.follow_selection()
    }

    /// Asks the artwork thread for `url` drawn at `w` by `h` canvas units, unless it is loaded or
    /// on its way.
    pub(super) fn want_art(&mut self, url: &str, w: f32, h: f32, radius: f32) {
        if self.library.art.contains_key(url) {
            return;
        }
        self.library.art.insert(url.to_owned(), Art::Requested);
        let scale = self.painter.scale();
        let px = |units: f32| (units * scale).round() as u32;
        let _ = self.art.send(art::Message::Want(art::Job { url: url.to_owned(), width: px(w), height: px(h), radius: px(radius) }));
    }

    /// Tells the artwork thread what is on screen now, so work for anything else is dropped.
    pub(super) fn show_art(&mut self, on_screen: HashSet<String>) {
        if on_screen == self.library.on_screen {
            return;
        }
        // Requests for artwork no longer shown are dropped by the artwork thread; forgetting them
        // here makes them ask again if they come back into view.
        self.library.art.retain(|url, art| !matches!(art, Art::Requested) || on_screen.contains(url));
        let _ = self.art.send(art::Message::Visible(on_screen.clone()));
        self.library.on_screen = on_screen;
    }

    /// Textures can only be made on this thread. Artwork no longer asked for (a closed detail
    /// screen, a scrolled-past tile) is not kept.
    fn upload_art(&mut self, window: &mut Window) {
        for (url, pixels) in std::mem::take(&mut self.library.pending) {
            if !matches!(self.library.art.get(&url), Some(Art::Requested)) {
                continue;
            }
            let state = match window.texture(pixels.width, pixels.height, &pixels.rgba) {
                Ok(texture) => Art::Ready(texture),
                Err(_) => Art::Missing,
            };
            self.library.art.insert(url, state);
        }
    }

    pub(super) fn draw_library(&mut self, window: &mut Window) {
        self.upload_art(window);
        if self.library.detail.is_some() {
            self.draw_detail(window);
            return;
        }
        let typing = self.keyboard.is_some();
        let header = self.header_buttons();
        let tools = self.toolbar_buttons();
        let menu_open = self.library.genre_menu.is_some();
        let p = &mut self.painter;
        p.text(window, "GeForce NOW", MARGIN, 64.0, 48.0, TEXT, true, Align::Left);
        p.text(window, &self.user, CANVAS_W - MARGIN, 76.0, 30.0, DIM, false, Align::Right);
        footer(p, window, self.library.hint(typing));
        for (index, (rect, label, _)) in header.into_iter().enumerate() {
            button(p, window, rect, &label, self.library.focus == Focus::Header(index) && !menu_open);
        }
        for (index, (rect, label)) in tools.into_iter().enumerate() {
            let focused = self.library.focus == Focus::Toolbar(index) && !menu_open;
            if TOOLS[index] != Tool::Search {
                button(p, window, rect, &label, focused);
                continue;
            }
            if focused {
                p.halo(window, rect, 30.0, 4.0, ACCENT);
            }
            p.round_rect(window, rect, 30.0, if focused || typing { BUTTON } else { PANEL });
            let text = if typing { format!("{}|", self.library.query) } else { label };
            let text = p.fit(&text, 28.0, false, rect.w - 64.0);
            let color = if self.library.query.is_empty() && !typing { DIM } else { TEXT };
            p.text(window, &text, rect.x + 32.0, rect.y + 14.0, 28.0, color, false, Align::Left);
        }
        let (shown, total) = (self.library.view.len(), self.library.games.len());
        if total > 0 {
            let count = if shown == total { format!("{total} games") } else { format!("{shown} of {total} games") };
            p.text(window, &count, CANVAS_W - MARGIN, TOOLBAR_Y + 16.0, 26.0, DIM, false, Align::Right);
        }
        if total == 0 {
            p.text(window, "Your library is empty", CANVAS_W / 2.0, 440.0, 44.0, TEXT, true, Align::Center);
            p.text(window, "Add games at play.geforcenow.com and they will show up here.", CANVAS_W / 2.0, 510.0, 30.0, DIM, false, Align::Center);
            return;
        }
        if shown == 0 {
            p.text(window, "No games match", CANVAS_W / 2.0, 480.0, 44.0, TEXT, true, Align::Center);
            p.text(window, "Clear the search or choose All genres.", CANVAS_W / 2.0, 550.0, 30.0, DIM, false, Align::Center);
            self.draw_genre_menu(window);
            return;
        }

        let first_row = (self.library.scroll / ROW_H).floor().max(0.0) as usize;
        let rows = ((GRID_BOTTOM - GRID_TOP) / ROW_H).ceil() as usize + 2;
        let visible = first_row * COLUMNS..((first_row + rows) * COLUMNS).min(shown);
        let visible = visible.start.min(visible.end)..visible.end;
        let on_screen: HashSet<String> =
            self.library.view[visible.clone()].iter().filter_map(|&index| self.library.games[index].art.clone()).collect();
        self.show_art(on_screen);
        let clip = self.painter.area(Rect::new(0.0, GRID_TOP - 24.0, CANVAS_W, GRID_BOTTOM - GRID_TOP + 24.0));
        window.clip(Some(clip));
        for position in visible {
            let rect = self.tile_rect(position);
            let focused = position == self.library.selected && self.library.focus == Focus::Grid && !menu_open;
            if let Some(url) = self.library.games[self.library.view[position]].art.clone() {
                self.want_art(&url, TILE_W, TILE_H, TILE_RADIUS);
            }
            let game = &self.library.games[self.library.view[position]];
            let p = &mut self.painter;
            if focused {
                p.halo(window, rect, TILE_RADIUS, 6.0, ACCENT);
            }
            p.round_rect(window, rect, TILE_RADIUS, TILE);
            match game.art.as_ref().and_then(|url| self.library.art.get(url)) {
                Some(Art::Ready(texture)) => p.image(window, texture, rect, 255),
                _ => {
                    p.paragraph(window, &game.title, Rect::new(rect.x + 20.0, rect.y + 130.0, rect.w - 40.0, 200.0), 26.0, DIM, Align::Center);
                }
            }
            if game.hdr() {
                hdr_badge(p, window, rect);
            }
            let title = p.fit(&game.title, 26.0, focused, TILE_W);
            p.text(window, &title, rect.x, rect.y + TILE_H + 18.0, 26.0, if focused { TEXT } else { DIM }, focused, Align::Left);
        }
        window.clip(None);
        self.evict_far_art(first_row, rows);
        self.draw_genre_menu(window);
    }

    fn draw_genre_menu(&mut self, window: &mut Window) {
        let rows = self.genre_menu_rows();
        let (Some(first), Some(last)) = (rows.first(), rows.last()) else { return };
        let focus = self.library.genre_menu;
        let panel = Rect::new(first.rect.x - 12.0, first.rect.y - 12.0, GENRE_MENU_W + 24.0, last.rect.y + last.rect.h - first.rect.y + 24.0);
        let p = &mut self.painter;
        p.halo(window, panel, 18.0, 2.0, BUTTON);
        p.round_rect(window, panel, 18.0, PANEL);
        for row in &rows {
            let focused = Some(row.index) == focus;
            if focused {
                p.round_rect(window, row.rect, 12.0, ACCENT);
            }
            let (color, dim) = if focused { (ON_ACCENT, ON_ACCENT) } else { (TEXT, DIM) };
            if row.active {
                p.text(window, "✓", row.rect.x + 20.0, row.rect.y + 12.0, 28.0, if focused { ON_ACCENT } else { ACCENT }, true, Align::Left);
            }
            let label = p.fit(&row.label, 28.0, row.active, row.rect.w - 160.0);
            p.text(window, &label, row.rect.x + 60.0, row.rect.y + 12.0, 28.0, color, row.active, Align::Left);
            p.text(window, &row.count.to_string(), row.rect.x + row.rect.w - 24.0, row.rect.y + 12.0, 28.0, dim, false, Align::Right);
        }
    }

    fn evict_far_art(&mut self, first_row: usize, rows: usize) {
        let ready = self.library.art.values().filter(|art| matches!(art, Art::Ready(_))).count();
        if ready <= ART_KEPT {
            return;
        }
        let start = first_row.saturating_sub(ART_MARGIN_ROWS) * COLUMNS;
        let end = ((first_row + rows + ART_MARGIN_ROWS) * COLUMNS).min(self.library.view.len());
        let near: HashSet<&str> =
            self.library.view[start.min(end)..end].iter().filter_map(|&index| self.library.games[index].art.as_deref()).collect();
        self.library.art.retain(|url, art| !matches!(art, Art::Ready(_)) || near.contains(url.as_str()));
    }

    /// Developer aid: the library with sample games and no core. Two covers come from
    /// `.claude/design/images` (not committed); the rest show their titles.
    pub(super) fn preview_library(&mut self, genre_menu: bool) {
        let sample = |title: &str, genres: &[&str], hdr: bool| Game {
            app_id: Value::from(title),
            title: title.to_owned(),
            art: None,
            genres: genres.iter().map(|g| (*g).to_owned()).collect(),
            search_text: format!("{} {}", title, genres.join(" ")).to_lowercase(),
            variants: vec![account::Variant { id: "1".into(), store: "STEAM".into(), selected: true, in_library: true, hdr, ..Default::default() }],
            ..Default::default()
        };
        self.user = "Player One".into();
        self.library.set_games(vec![
            sample("Cyberpunk 2077", &["ACTION", "RPG"], true),
            sample("Fortnite", &["ACTION", "SHOOTER"], false),
            sample("Baldur's Gate 3", &["RPG", "STRATEGY"], false),
            sample("Forza Horizon 5", &["RACING"], true),
            sample("Hades II", &["ACTION", "INDIE"], false),
            sample("Against the Storm", &["STRATEGY", "INDIE"], false),
            sample("Diablo® IV", &["ACTION", "RPG"], true),
            sample("Teardown", &["ACTION", "INDIE"], false),
            sample("Alan Wake 2", &["ACTION", "ADVENTURE"], true),
            sample("Stardew Valley", &["SIMULATION", "INDIE"], false),
            sample("Counter-Strike 2", &["SHOOTER"], false),
            sample("Hollow Knight", &["ADVENTURE", "INDIE"], false),
        ]);
        let images = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.claude/design/images");
        self.library.games[0].art = Some(format!("{images}/cyberpunk-2077/game_box_art.jpg"));
        self.library.games[1].art = Some(format!("{images}/fortnite/game_box_art.jpg"));
        self.library.set_genre_names(HashMap::from([("RPG".to_owned(), "Role-Playing".to_owned())]));
        if genre_menu {
            self.library.focus = Focus::Toolbar(1);
            self.library.genre_menu = Some(0);
        }
        self.screen = Screen::Library;
    }
}

pub(super) fn button(p: &mut Painter, window: &mut Window, rect: Rect, label: &str, focused: bool) {
    if focused {
        p.halo(window, rect, 30.0, 4.0, ACCENT);
    }
    p.round_rect(window, rect, 30.0, if focused { ACCENT } else { BUTTON });
    let color = if focused { ON_ACCENT } else { TEXT };
    p.text(window, label, rect.x + rect.w / 2.0, rect.y + 14.0, 28.0, color, true, Align::Center);
}

/// An outlined "HDR" tag in the tile's top-right corner, over the artwork.
fn hdr_badge(p: &mut Painter, window: &mut Window, tile: Rect) {
    let badge = Rect::new(tile.x + tile.w - 76.0, tile.y + 14.0, 62.0, 32.0);
    p.halo(window, badge, 8.0, 2.0, TEXT);
    p.round_rect(window, badge, 8.0, [14, 15, 18, 255]);
    p.text(window, "HDR", badge.x + badge.w / 2.0, badge.y + 6.0, 19.0, TEXT, true, Align::Center);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(title: &str, genres: &[&str]) -> Game {
        Game {
            app_id: Value::from(title),
            title: title.to_owned(),
            art: None,
            genres: genres.iter().map(|g| (*g).to_owned()).collect(),
            search_text: format!("{title} valve steam").to_lowercase(),
            ..Default::default()
        }
    }

    fn library(games: Vec<Game>) -> Library {
        let mut library = Library::new(std::env::temp_dir().join(format!("gfn-library-view-test-{}.json", std::process::id())));
        library.sort = Sort::Recent;
        library.set_games(games);
        library
    }

    #[test]
    fn search_needs_every_word_and_combines_with_the_genre() {
        let games = vec![game("Portal 2", &["PUZZLE"]), game("Half-Life 2", &["SHOOTER"]), game("Portal", &["PUZZLE", "SHOOTER"])];
        assert_eq!(view(&games, "portal", None, Sort::Recent), [0, 2]);
        assert_eq!(view(&games, "  PORTAL valve ", None, Sort::Recent), [0, 2]);
        assert_eq!(view(&games, "portal epic", None, Sort::Recent), Vec::<usize>::new());
        assert_eq!(view(&games, "", Some("SHOOTER"), Sort::Recent), [1, 2]);
        assert_eq!(view(&games, "portal", Some("SHOOTER"), Sort::Recent), [2]);
    }

    #[test]
    fn title_order_ignores_case_and_keeps_equal_titles_in_played_order() {
        let games = vec![game("b", &[]), game("A", &[]), game("a", &[]), game("C", &[])];
        assert_eq!(view(&games, "", None, Sort::Title), [1, 2, 0, 3]);
    }

    #[test]
    fn genres_are_counted_over_the_library_most_games_first() {
        let games = vec![game("1", &["RPG", "ACTION"]), game("2", &["ACTION", "ACTION"]), game("3", &["INDIE"])];
        assert_eq!(genre_counts(&games), [("ACTION".to_owned(), 2), ("INDIE".to_owned(), 1), ("RPG".to_owned(), 1)]);
        let mut library = library(games);
        library.type_text("3");
        assert_eq!(library.genre_rows()[0], (None, 3));
    }

    #[test]
    fn unnamed_genre_codes_read_as_words() {
        assert_eq!(title_case("FIGHTING_GAME"), "Fighting Game");
        assert_eq!(title_case("Racing"), "Racing");
    }

    #[test]
    fn the_selected_game_stays_selected_while_the_view_changes() {
        let mut library = library(vec![game("Portal 2", &[]), game("Half-Life 2", &[]), game("Portal", &[])]);
        library.selected = 2;
        library.type_text("portal");
        assert_eq!(library.view, [0, 2]);
        assert_eq!(library.play(), Some(2));
        library.type_text(" 2");
        assert_eq!(library.play(), Some(0));
    }

    #[test]
    fn up_from_the_top_row_reaches_the_toolbar_and_back_clears_the_search() {
        let mut library = library((0..8).map(|i| game(&format!("Game {i}"), &[])).collect());
        library.selected = 7;
        library.nav(Nav::Up);
        assert_eq!((library.focus, library.selected), (Focus::Grid, 1));
        library.nav(Nav::Up);
        assert_eq!(library.focus, Focus::Toolbar(0));
        assert_eq!(library.nav(Nav::Select), Outcome::Type);
        library.type_text("game 1");
        assert_eq!(library.nav(Nav::Back), Outcome::Nothing);
        assert_eq!(library.view.len(), 8);
        assert_eq!(library.nav(Nav::Back), Outcome::Exit);
        library.nav(Nav::Down);
        assert_eq!(library.nav(Nav::Select), Outcome::Details(1));
    }

    #[test]
    fn the_genre_menu_applies_the_focused_row() {
        let mut library = library(vec![game("1", &["RPG"]), game("2", &["ACTION"]), game("3", &["ACTION"])]);
        library.focus = Focus::Toolbar(1);
        library.nav(Nav::Select);
        assert_eq!(library.genre_menu, Some(0));
        library.nav(Nav::Down);
        library.nav(Nav::Select);
        assert_eq!((library.genre.as_deref(), library.view.clone()), (Some("ACTION"), vec![1, 2]));
        assert_eq!(library.play(), None);
        library.nav(Nav::Select);
        assert_eq!(library.genre_menu, Some(1));
    }

    #[test]
    fn nothing_matching_moves_focus_off_the_grid() {
        let mut library = library(vec![game("Portal", &[])]);
        library.type_text("zzz");
        assert_eq!(library.focus, Focus::Toolbar(0));
        assert_eq!(library.nav(Nav::Down), Outcome::Nothing);
        assert_eq!(library.focus, Focus::Toolbar(0));
    }

    #[test]
    fn the_sort_order_is_remembered() {
        let prefs = std::env::temp_dir().join(format!("gfn-library-sort-test-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&prefs);
        let mut library = Library::new(prefs.clone());
        library.set_games(vec![game("b", &[]), game("a", &[])]);
        library.focus = Focus::Toolbar(2);
        library.nav(Nav::Select);
        assert_eq!(library.view, [1, 0]);
        assert_eq!(Library::new(prefs.clone()).sort, Sort::Title);
        let _ = std::fs::remove_file(prefs);
    }
}
