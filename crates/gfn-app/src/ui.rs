//! The app's screens on the UI thread: what is shown, how remote, keyboard, controller and mouse
//! move through it, and when a frame needs drawing at all (the TV's graphics plane sits over the
//! video, so an idle stream must not repaint it).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use gfn_core_client::CoreClient;
use gfn_input::InputCapture;
use gfn_sdl::{InputEvent, Texture, Window};
use gfn_sink::MediaRuntime;
use serde_json::Value;

use crate::account::{self, Game};
use crate::art;
use crate::bluetooth::{self, Command, Status};
use crate::gfx::{Align, CANVAS_H, CANVAS_W, Painter, Rect, Rgba};
use crate::session::{self, Profile, Stats, Takeover};
use crate::settings::{self, StreamSettings};
use crate::{TV, Update, Updates};

const BACKGROUND: Rgba = [14, 15, 18, 255];
const PANEL: Rgba = [30, 32, 38, 255];
const TILE: Rgba = [38, 40, 48, 255];
const BUTTON: Rgba = [52, 55, 64, 255];
const TEXT: Rgba = [240, 241, 243, 255];
const DIM: Rgba = [150, 155, 166, 255];
const ACCENT: Rgba = [118, 185, 0, 255];
const ON_ACCENT: Rgba = [12, 14, 10, 255];
const WARN: Rgba = [255, 120, 100, 255];
const SCRIM: Rgba = [0, 0, 0, 170];
const CLEAR: Rgba = [0, 0, 0, 0];

const MARGIN: f32 = 96.0;
const COLUMNS: usize = 6;
const TILE_W: f32 = 240.0;
const TILE_H: f32 = 320.0;
const TILE_RADIUS: f32 = 12.0;
const ROW_H: f32 = TILE_H + 84.0;
const GRID_TOP: f32 = 180.0;
const GRID_BOTTOM: f32 = 990.0;
/// Artwork textures kept before those far from the viewport are dropped (about 300 KB each at
/// 1080p); dropped ones reload from the core's disk cache when scrolled back into view.
const ART_KEPT: usize = 96;
/// Rows either side of the visible ones whose artwork survives eviction.
const ART_MARGIN_ROWS: usize = 3;

const HINT_FOR: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
    Left,
    Right,
    Select,
    Back,
}

const SCANCODE_RETURN: i32 = 40;
const SCANCODE_ESCAPE: i32 = 41;
const SCANCODE_BACKSPACE: i32 = 42;
const SCANCODE_SPACE: i32 = 44;
const SCANCODE_RIGHT: i32 = 79;
const SCANCODE_LEFT: i32 = 80;
const SCANCODE_DOWN: i32 = 81;
const SCANCODE_UP: i32 = 82;
const SCANCODE_KP_ENTER: i32 = 88;
const SCANCODE_F3: i32 = 60;
const SCANCODE_TAB: i32 = 43;
const SCANCODE_DELETE: i32 = 76;
/// `SDLK_AC_BACK`, and the raw codes LG remotes send for BACK (482 on the Magic Remote, 461 on
/// others), as plx-native observed them.
const KEYCODES_REMOTE_BACK: [i32; 3] = [0x4000_010E, 482, 461];
/// The remote's colour buttons, as plx-native measured them on a webOS 4.5 set
/// (docs/remote-keys.md §9); the standard KEY_RED/KEY_BLUE codes never arrive.
const KEYCODE_RED: i32 = 486;
const KEYCODE_GREEN: i32 = 487;
const KEYCODE_BLUE: i32 = 489;

/// How long the TV gets to raise its keyboard before we say it didn't.
const KEYBOARD_APPEARS_WITHIN: Duration = Duration::from_secs(3);
const NOTICE_FOR: Duration = Duration::from_secs(5);

const STICK_THRESHOLD: i16 = 20_000;

fn is_remote_back(scancode: i32, keycode: i32) -> bool {
    KEYCODES_REMOTE_BACK.contains(&scancode) || KEYCODES_REMOTE_BACK.contains(&keycode)
}

/// Keys the on-screen keyboard sends for editing rather than as text.
fn is_editing_key(scancode: i32) -> bool {
    matches!(scancode, SCANCODE_BACKSPACE | SCANCODE_RETURN | SCANCODE_KP_ENTER | SCANCODE_TAB | SCANCODE_DELETE)
}

/// LG's SDL fork shifts the key event's fields by four bytes against the standard layout
/// (plx-native docs/remote-keys.md §1), so a remote code may land in either field.
fn is_remote_key(scancode: i32, keycode: i32, code: i32) -> bool {
    scancode == code || keycode == code
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    CloseMenu,
    ToggleStats,
    OpenKeyboard,
    EndSession,
    Takeover(Takeover),
    Bluetooth(Command),
    Exit,
}

struct Menu {
    title: String,
    message: String,
    items: Vec<(String, Action)>,
    focus: usize,
    /// What Back does.
    back: Action,
}

impl Menu {
    fn new(title: impl Into<String>, message: impl Into<String>, items: Vec<(String, Action)>, back: Action) -> Self {
        Self { title: title.into(), message: message.into(), items, focus: 0, back }
    }
}

enum Screen {
    Starting(String),
    SignIn { code: String, uri: String, qr: Option<(u32, Vec<u8>)>, texture: Option<Texture> },
    Library,
    Controllers,
    Settings,
    Launching { title: String, status: String, queue: Option<i64> },
    Streaming { first_frame: bool, hint_until: Instant },
    Ending(String),
    Fatal,
}

enum Art {
    Requested,
    Ready(Texture),
    Missing,
}

struct Library {
    games: Vec<Game>,
    focus: usize,
    /// A button above the grid has focus instead of a tile; `header_focus` says which.
    header: bool,
    header_focus: usize,
    scroll: f32,
    art: HashMap<String, Art>,
    pending: HashMap<String, art::Pixels>,
    /// The artwork last reported to the artwork thread as on screen.
    on_screen: HashSet<String>,
}

pub struct App {
    painter: Painter,
    screen: Screen,
    menu: Option<Menu>,
    library: Library,
    core: Option<Arc<CoreClient>>,
    scope: Value,
    user: String,
    runtime: MediaRuntime,
    updates: Updates,
    art: Sender<art::Message>,
    session: Option<session::Handle>,
    bluetooth: bluetooth::Handle,
    controllers: bluetooth::Snapshot,
    controllers_focus: usize,
    stream_settings: StreamSettings,
    settings_path: PathBuf,
    settings_focus: usize,
    /// The conflict question the session thread is waiting on.
    takeover: Option<Sender<Takeover>>,
    /// The stream statistics panel; kept across sessions.
    stats_visible: bool,
    profile: Option<Profile>,
    stats: Option<Stats>,
    keyboard: Option<Keyboard>,
    /// A short message over the stream, until the instant.
    notice: Option<(String, Instant)>,
    last_render: Instant,
    dirty: bool,
    quitting: bool,
    exit: bool,
    video_reset: bool,
    mouse: (f32, f32),
    stick: (i8, i8),
    clock: Instant,
    /// Developer aid: save the frame drawn at this time to this file, then exit.
    screenshot: Option<(Instant, PathBuf)>,
}

impl App {
    pub fn new(
        painter: Painter,
        runtime: MediaRuntime,
        updates: Updates,
        art: Sender<art::Message>,
        bluetooth: bluetooth::Handle,
        (stream_settings, settings_path): (StreamSettings, PathBuf),
    ) -> Self {
        Self {
            painter,
            screen: Screen::Starting("Starting".into()),
            menu: None,
            library: Library { games: Vec::new(), focus: 0, header: false, header_focus: 0, scroll: 0.0, art: HashMap::new(), pending: HashMap::new(), on_screen: HashSet::new() },
            core: None,
            scope: Value::Null,
            user: String::new(),
            runtime,
            updates,
            art,
            session: None,
            bluetooth,
            controllers: bluetooth::Snapshot::default(),
            controllers_focus: 0,
            stream_settings,
            settings_path,
            settings_focus: 0,
            takeover: None,
            stats_visible: false,
            profile: None,
            stats: None,
            keyboard: None,
            notice: None,
            last_render: Instant::now(),
            dirty: true,
            quitting: false,
            exit: false,
            video_reset: false,
            mouse: (-1.0, -1.0),
            stick: (0, 0),
            clock: Instant::now(),
            screenshot: None,
        }
    }

    /// Developer aid: shows one screen with sample data, without the core.
    pub fn preview(&mut self, name: &str) -> Result<(), String> {
        match name {
            "signin" => self.account(account::Progress::SignIn {
                code: "KX7P-Q2MD".into(),
                uri: "nvidia.com/activate".into(),
                uri_complete: "https://www.nvidia.com/activate?code=KX7P-Q2MD".into(),
            }),
            "queue" => {
                self.screen = Screen::Launching { title: "Against the Storm".into(), status: String::new(), queue: Some(14) };
            }
            "launching" => {
                self.screen =
                    Screen::Launching { title: "Against the Storm".into(), status: "Requesting a cloud seat".into(), queue: None };
            }
            "conflict" => {
                self.screen = Screen::Launching { title: "Teardown".into(), status: "Requesting a cloud seat".into(), queue: None };
                self.menu = Some(conflict_menu("Diablo® IV", "Teardown"));
            }
            "menu" => {
                self.screen = Screen::Streaming { first_frame: true, hint_until: Instant::now() };
                self.menu = Some(self.stream_menu());
            }
            "keyboard" => {
                self.screen = Screen::Streaming { first_frame: true, hint_until: Instant::now() };
                self.keyboard = Some(Keyboard { opened: Instant::now(), seen: true });
            }
            "stats" => {
                self.screen = Screen::Streaming { first_frame: true, hint_until: Instant::now() };
                self.stats_visible = true;
                self.profile = Some(Profile {
                    resolution: "1920x1080".into(),
                    fps: Some(60),
                    codec: "H265".into(),
                    max_bitrate_mbps: Some(20.0),
                    server: "eu-netherlands-north".into(),
                    gpu: "L40G".into(),
                    hdr: true,
                });
                self.stats = Some(Stats {
                    fps: Some(59.8),
                    bitrate_mbps: Some(18.4),
                    ping_ms: Some(14.0),
                    jitter_ms: Some(1.2),
                    loss_percent: Some(0.0),
                    decode_ms: Some(0.4),
                    display_lag_ms: Some(48),
                });
            }
            "error" => self.account(account::Progress::Failed("catalog.library.list: request timed out".into())),
            // The simulator's scripted Bluetooth service fills the list over the first seconds.
            "controllers" => self.open_controllers(),
            "settings" => self.screen = Screen::Settings,
            other => return Err(format!("--preview {other}: one of signin, queue, launching, conflict, menu, stats, keyboard, error, controllers, settings")),
        }
        Ok(())
    }

    pub fn screenshot_after(&mut self, delay: Duration, path: PathBuf) {
        self.screenshot = Some((Instant::now() + delay, path));
    }

    pub fn should_exit(&self) -> bool {
        self.exit
    }

    pub fn is_streaming(&self) -> bool {
        matches!(self.screen, Screen::Streaming { .. })
    }

    /// Whether game input goes to the stream (no menu over it).
    pub fn stream_has_input(&self) -> bool {
        self.is_streaming() && self.menu.is_none()
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// True once after a session ends: the last picture must not linger into the next stream.
    pub fn take_video_reset(&mut self) -> bool {
        std::mem::take(&mut self.video_reset)
    }

    /// Window close or Ctrl+C: release the seat first if there is one.
    pub fn request_quit(&mut self) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        if let Some(reply) = self.takeover.take() {
            let _ = reply.send(Takeover::Cancel);
        }
        match &self.session {
            Some(handle) => {
                handle.end();
                self.menu = None;
                self.screen = Screen::Ending("Ending your session".into());
                self.dirty = true;
            }
            None => self.exit = true,
        }
    }

    pub fn update(&mut self, update: Update, input: &mut InputCapture) {
        if let Update::Session(session::Progress::Stats(stats)) = update {
            // Once a second: repainting the plane over the video only pays when the panel shows.
            self.stats = Some(stats);
            self.dirty |= self.stats_visible && self.is_streaming();
            return;
        }
        self.dirty = true;
        match update {
            Update::Account(progress) => self.account(progress),
            Update::Art { url, pixels } => match pixels {
                Some(pixels) => {
                    self.library.pending.insert(url, pixels);
                }
                None => {
                    self.library.art.insert(url, Art::Missing);
                }
            },
            Update::Session(progress) => self.session_progress(progress, input),
            Update::Bluetooth(snapshot) => self.controllers = snapshot,
        }
    }

    fn account(&mut self, progress: account::Progress) {
        match progress {
            account::Progress::Starting(status) => self.screen = Screen::Starting(status),
            account::Progress::SignIn { code, uri, uri_complete } => {
                let qr = crate::qr::pixels(if uri_complete.is_empty() { &uri } else { &uri_complete });
                self.screen = Screen::SignIn { code, uri, qr, texture: None };
            }
            account::Progress::SignedIn { core, user, scope } => {
                self.core = Some(core);
                self.user = user;
                self.scope = scope;
                self.screen = Screen::Starting("Loading your library".into());
            }
            account::Progress::Library(games) => {
                self.library.games = games;
                self.library.focus = 0;
                self.screen = Screen::Library;
            }
            account::Progress::Failed(message) => {
                if !self.quitting {
                    self.screen = Screen::Fatal;
                    self.menu = Some(Menu::new("Something went wrong", message, vec![("Exit".into(), Action::Exit)], Action::Exit));
                } else {
                    self.exit = true;
                }
            }
        }
    }

    fn session_progress(&mut self, progress: session::Progress, input: &mut InputCapture) {
        use session::Progress;
        match progress {
            Progress::Status(text) => {
                if let Screen::Launching { status, queue, .. } = &mut self.screen {
                    *status = text;
                    *queue = None;
                }
            }
            Progress::Queue(position) => {
                if let Screen::Launching { queue, .. } = &mut self.screen {
                    *queue = Some(position);
                }
            }
            Progress::Conflict { app_id, reply } => {
                if self.quitting {
                    let _ = reply.send(Takeover::Cancel);
                    return;
                }
                let running = self
                    .library
                    .games
                    .iter()
                    .find(|g| g.has_id(&app_id))
                    .map_or_else(|| "Another game".to_owned(), |g| g.title.clone());
                let wanted = match &self.screen {
                    Screen::Launching { title, .. } => title.clone(),
                    _ => "this game".into(),
                };
                self.takeover = Some(reply);
                self.menu = Some(conflict_menu(&running, &wanted));
            }
            Progress::Profile(profile) => {
                if let Some((width, height)) = profile.resolution.split_once('x') {
                    if let (Ok(width), Ok(height)) = (width.parse(), height.parse()) {
                        input.set_stream_size(width, height);
                    }
                }
                self.profile = Some(profile);
            }
            // update() takes stats first, so they do not repaint the screen.
            Progress::Stats(_) => {}
            Progress::Started => {
                self.bluetooth.send(Command::Streaming(true));
                self.screen = Screen::Streaming { first_frame: false, hint_until: Instant::now() + HINT_FOR };
            }
            Progress::FirstFrame => {
                if let Screen::Streaming { first_frame, hint_until } = &mut self.screen {
                    *first_frame = true;
                    *hint_until = Instant::now() + HINT_FOR;
                }
            }
            Progress::MenuRequested => {
                if self.stream_has_input() {
                    self.open_stream_menu(input);
                }
            }
            Progress::Ended(error) => {
                self.bluetooth.send(Command::Streaming(false));
                self.session = None;
                self.takeover = None;
                self.profile = None;
                self.stats = None;
                self.close_keyboard();
                self.video_reset = true;
                input.release();
                if self.quitting {
                    self.exit = true;
                    return;
                }
                self.screen = Screen::Library;
                self.menu = error.map(|message| {
                    Menu::new("The session ended", message, vec![("OK".into(), Action::CloseMenu)], Action::CloseMenu)
                });
            }
        }
    }

    fn open_stream_menu(&mut self, input: &mut InputCapture) {
        self.close_keyboard();
        input.release();
        self.menu = Some(self.stream_menu());
        self.dirty = true;
    }

    fn stream_menu(&self) -> Menu {
        let stats = if self.stats_visible { "Hide statistics" } else { "Show statistics" };
        Menu::new(
            "",
            "",
            vec![
                ("Back to the game".into(), Action::CloseMenu),
                ("Keyboard".into(), Action::OpenKeyboard),
                (stats.into(), Action::ToggleStats),
                ("End session".into(), Action::EndSession),
            ],
            Action::CloseMenu,
        )
    }

    /// Everything but controller hot-plug and quit, which the main loop owns.
    pub fn event(&mut self, event: InputEvent, input: &mut InputCapture, window: &Window) {
        if self.stream_has_input() && self.keyboard.is_some() {
            match event {
                InputEvent::Text(text) => input.type_text(text.as_str()),
                InputEvent::Key { scancode, keycode, pressed: true, repeat, .. } => {
                    if is_remote_back(scancode, keycode) || scancode == SCANCODE_ESCAPE {
                        self.close_keyboard();
                    } else if is_editing_key(scancode) {
                        input.type_key(scancode);
                    } else if is_remote_key(scancode, keycode, KEYCODE_GREEN) && !repeat {
                        self.close_keyboard();
                    }
                }
                // Typed characters arrive as text; their key presses would type them twice.
                InputEvent::Key { .. } => {}
                _ => input.handle(event, window.size()),
            }
            return;
        }
        if self.stream_has_input() {
            if let InputEvent::Key { scancode, keycode, pressed, repeat, .. } = event {
                if is_remote_key(scancode, keycode, KEYCODE_GREEN) {
                    if pressed && !repeat {
                        self.act(Action::OpenKeyboard);
                    }
                    return;
                }
                if is_remote_key(scancode, keycode, KEYCODE_BLUE) {
                    if !repeat {
                        input.handle(InputEvent::MouseButton { button: 3, pressed }, window.size());
                    }
                    return;
                }
                if is_remote_key(scancode, keycode, KEYCODE_RED) || scancode == SCANCODE_F3 {
                    if pressed && !repeat {
                        self.act(Action::ToggleStats);
                    }
                    return;
                }
            }
            let wants_menu = match event {
                InputEvent::Key { scancode, keycode, pressed: true, .. } if is_remote_back(scancode, keycode) => true,
                InputEvent::Key { scancode: SCANCODE_ESCAPE, pressed: true, .. } => !TV && !input.is_captured(),
                _ => false,
            };
            if wants_menu {
                self.open_stream_menu(input);
            } else {
                let captured = input.is_captured();
                input.handle(event, window.size());
                if input.is_captured() != captured {
                    self.dirty = true;
                }
            }
            return;
        }
        match event {
            InputEvent::MouseMotion { x, y, .. } => {
                self.mouse = self.painter.to_canvas(window, x, y);
                self.hover();
            }
            InputEvent::MouseButton { button: 1, pressed: true } => self.click(),
            InputEvent::MouseWheel { y, flipped, .. } if y != 0 => {
                let y = if flipped { -y } else { y };
                self.nav(if y > 0 { Nav::Up } else { Nav::Down });
            }
            InputEvent::FocusLost | InputEvent::FocusGained => self.dirty = true,
            _ => {
                if let Some(nav) = self.nav_of(event) {
                    self.nav(nav);
                }
            }
        }
    }

    fn nav_of(&mut self, event: InputEvent) -> Option<Nav> {
        match event {
            InputEvent::Key { scancode, keycode, pressed: true, repeat, .. } => match scancode {
                SCANCODE_UP => Some(Nav::Up),
                SCANCODE_DOWN => Some(Nav::Down),
                SCANCODE_LEFT => Some(Nav::Left),
                SCANCODE_RIGHT => Some(Nav::Right),
                SCANCODE_RETURN | SCANCODE_KP_ENTER | SCANCODE_SPACE if !repeat => Some(Nav::Select),
                SCANCODE_ESCAPE | SCANCODE_BACKSPACE if !repeat => Some(Nav::Back),
                _ if is_remote_back(scancode, keycode) && !repeat => Some(Nav::Back),
                _ => None,
            },
            InputEvent::ControllerButton { button, pressed: true, .. } => match button {
                0 => Some(Nav::Select),
                1 => Some(Nav::Back),
                11 => Some(Nav::Up),
                12 => Some(Nav::Down),
                13 => Some(Nav::Left),
                14 => Some(Nav::Right),
                _ => None,
            },
            InputEvent::ControllerAxis { axis: axis @ (0 | 1), value, .. } => {
                let direction = if value > STICK_THRESHOLD {
                    1
                } else if value < -STICK_THRESHOLD {
                    -1
                } else {
                    0
                };
                let held = if axis == 0 { &mut self.stick.0 } else { &mut self.stick.1 };
                if *held == direction {
                    return None;
                }
                *held = direction;
                match (axis, direction) {
                    (0, 1) => Some(Nav::Right),
                    (0, -1) => Some(Nav::Left),
                    (1, 1) => Some(Nav::Down),
                    (1, -1) => Some(Nav::Up),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn nav(&mut self, nav: Nav) {
        self.dirty = true;
        if let Some(menu) = &mut self.menu {
            match nav {
                Nav::Up => menu.focus = menu.focus.saturating_sub(1),
                Nav::Down => menu.focus = (menu.focus + 1).min(menu.items.len().saturating_sub(1)),
                Nav::Select => {
                    let action = menu.items.get(menu.focus).map(|(_, a)| a.clone());
                    if let Some(action) = action {
                        self.act(action);
                    }
                }
                Nav::Back => {
                    let back = menu.back.clone();
                    self.act(back);
                }
                Nav::Left | Nav::Right => {}
            }
            return;
        }
        match &self.screen {
            Screen::Library => self.library_nav(nav),
            Screen::Controllers => self.controllers_nav(nav),
            Screen::Settings => self.settings_nav(nav),
            Screen::Launching { .. } if nav == Nav::Back => self.act(Action::EndSession),
            Screen::SignIn { .. } | Screen::Starting(_) if nav == Nav::Back => self.menu = Some(exit_menu()),
            _ => {}
        }
    }

    fn library_nav(&mut self, nav: Nav) {
        let count = self.library.games.len();
        if count == 0 {
            self.library.header = true;
        }
        if self.library.header {
            let last = HEADER_BUTTONS.len() - 1;
            match nav {
                Nav::Down if count > 0 => self.library.header = false,
                Nav::Left => self.library.header_focus = self.library.header_focus.saturating_sub(1),
                Nav::Right => self.library.header_focus = (self.library.header_focus + 1).min(last),
                Nav::Select => self.open_header(HEADER_BUTTONS[self.library.header_focus.min(last)]),
                Nav::Back => self.menu = Some(exit_menu()),
                _ => {}
            }
            return;
        }
        let focus = &mut self.library.focus;
        match nav {
            Nav::Up if *focus < COLUMNS => self.library.header = true,
            Nav::Left if *focus % COLUMNS > 0 => *focus -= 1,
            Nav::Right if *focus % COLUMNS + 1 < COLUMNS && *focus + 1 < count => *focus += 1,
            Nav::Up if *focus >= COLUMNS => *focus -= COLUMNS,
            Nav::Down if *focus + COLUMNS < count => *focus += COLUMNS,
            // The last row may be short: moving down from above it lands on its last tile.
            Nav::Down if *focus / COLUMNS + 1 < count.div_ceil(COLUMNS) => *focus = count - 1,
            Nav::Select if count > 0 => {
                let game = self.library.games[*focus].clone();
                self.launch(&game);
            }
            Nav::Back => self.menu = Some(exit_menu()),
            _ => {}
        }
    }

    fn act(&mut self, action: Action) {
        self.dirty = true;
        self.menu = None;
        match action {
            Action::CloseMenu => {}
            Action::ToggleStats => self.stats_visible = !self.stats_visible,
            Action::OpenKeyboard => self.open_keyboard(),
            Action::EndSession => {
                if let Some(handle) = &self.session {
                    handle.end();
                    self.screen = Screen::Ending("Ending your session".into());
                }
                if let Some(reply) = self.takeover.take() {
                    let _ = reply.send(Takeover::Cancel);
                }
            }
            Action::Takeover(choice) => {
                if let Some(reply) = self.takeover.take() {
                    let _ = reply.send(choice);
                }
                if choice == Takeover::Cancel {
                    self.screen = Screen::Ending("Cancelling".into());
                }
            }
            Action::Bluetooth(command) => self.bluetooth.send(command),
            Action::Exit => self.request_quit(),
        }
    }

    fn launch(&mut self, game: &Game) {
        let (Some(core), Some(variant)) = (self.core.clone(), game.launch_variant()) else {
            self.menu = Some(Menu::new(
                "Can't start this game",
                "GeForce NOW lists no playable store version of it.",
                vec![("OK".into(), Action::CloseMenu)],
                Action::CloseMenu,
            ));
            return;
        };
        let launch = session::Launch {
            variant: variant.id.clone(),
            catalog_app_id: game.app_id.clone(),
            title: game.title.clone(),
            scope: self.scope.clone(),
            settings: self.stream_settings.clone(),
        };
        println!("launching {} ({} variant {})", game.title, variant.store, variant.id);
        self.session = Some(session::start(core, self.runtime.clone(), launch, self.updates.clone()));
        self.screen = Screen::Launching { title: game.title.clone(), status: "Starting".into(), queue: None };
    }

    fn tile_rect(&self, index: usize) -> Rect {
        let gap = (CANVAS_W - 2.0 * MARGIN - COLUMNS as f32 * TILE_W) / (COLUMNS as f32 - 1.0);
        let (row, col) = (index / COLUMNS, index % COLUMNS);
        Rect::new(MARGIN + col as f32 * (TILE_W + gap), GRID_TOP + row as f32 * ROW_H - self.library.scroll, TILE_W, TILE_H)
    }

    fn menu_buttons(&self) -> Vec<Rect> {
        let Some(menu) = &self.menu else { return Vec::new() };
        let layout = self.menu_layout(menu);
        (0..menu.items.len()).map(|i| Rect::new(layout.x + 48.0, layout.buttons_y + i as f32 * 84.0, layout.w - 96.0, 68.0)).collect()
    }

    fn hover(&mut self) {
        let (x, y) = self.mouse;
        if self.menu.is_some() {
            if let Some(index) = self.menu_buttons().iter().position(|r| r.contains(x, y)) {
                let menu = self.menu.as_mut().expect("checked");
                if menu.focus != index {
                    menu.focus = index;
                    self.dirty = true;
                }
            }
            return;
        }
        if matches!(self.screen, Screen::Library) {
            let hit = self.header_buttons().iter().position(|(rect, _, _)| rect.contains(x, y));
            if let Some(index) = hit {
                if !self.library.header || self.library.header_focus != index {
                    (self.library.header, self.library.header_focus) = (true, index);
                    self.dirty = true;
                }
            } else if self.library.header && (GRID_TOP - 20.0..GRID_BOTTOM).contains(&y) {
                self.library.header = false;
                self.dirty = true;
            }
        }
        if matches!(self.screen, Screen::Library) && (GRID_TOP - 20.0..GRID_BOTTOM).contains(&y) {
            let hit = (0..self.library.games.len()).find(|&i| self.tile_rect(i).contains(x, y));
            if let Some(index) = hit.filter(|&i| i != self.library.focus || self.library.header) {
                self.library.focus = index;
                self.library.header = false;
                self.dirty = true;
            }
        }
        if matches!(self.screen, Screen::Settings) {
            let hit = (0..SettingRow::ALL.len()).find(|&i| settings_row_rect(i).contains(x, y));
            if let Some(index) = hit.filter(|&i| i != self.settings_focus) {
                self.settings_focus = index;
                self.dirty = true;
            }
        }
        if matches!(self.screen, Screen::Controllers) {
            let hit = self.controller_rows().into_iter().position(|row| row.rect.contains(x, y));
            if let Some(index) = hit.filter(|&i| i != self.controllers_focus) {
                self.controllers_focus = index;
                self.dirty = true;
            }
        }
    }

    fn click(&mut self) {
        self.dirty = true;
        let (x, y) = self.mouse;
        if let Some(menu) = &self.menu {
            if let Some(index) = self.menu_buttons().iter().position(|r| r.contains(x, y)) {
                let action = menu.items[index].1.clone();
                self.act(action);
            }
            return;
        }
        if matches!(self.screen, Screen::Library) {
            if let Some((_, _, button)) = self.header_buttons().into_iter().find(|(rect, _, _)| rect.contains(x, y)) {
                self.open_header(button);
                return;
            }
        }
        if matches!(self.screen, Screen::Settings) {
            if let Some(index) = (0..SettingRow::ALL.len()).find(|&i| settings_row_rect(i).contains(x, y)) {
                self.settings_focus = index;
                let (left, right) = settings_arrow_rects(index);
                self.settings_nav(if left.contains(x, y) {
                    Nav::Left
                } else if right.contains(x, y) {
                    Nav::Right
                } else {
                    Nav::Select
                });
            }
            return;
        }
        if matches!(self.screen, Screen::Controllers) {
            if let Some(index) = self.controller_rows().into_iter().position(|row| row.rect.contains(x, y)) {
                self.controllers_focus = index;
                self.controllers_nav(Nav::Select);
            }
            return;
        }
        if matches!(self.screen, Screen::Library) && (GRID_TOP - 20.0..GRID_BOTTOM).contains(&y) {
            if let Some(index) = (0..self.library.games.len()).find(|&i| self.tile_rect(i).contains(x, y)) {
                self.library.focus = index;
                let game = self.library.games[index].clone();
                self.launch(&game);
            }
        }
    }

    /// Advances animations; true while something on screen is moving.
    fn animate(&mut self) -> bool {
        let mut moving = matches!(self.screen, Screen::Starting(_) | Screen::Launching { .. } | Screen::Ending(_));
        moving |= matches!(self.screen, Screen::Controllers) && self.controllers_busy();
        if let Screen::Streaming { hint_until, .. } = self.screen {
            // The hint and notice are static: one repaint to show them (via dirty) and one when
            // they expire, rather than recompositing the plane over the video every frame.
            let now = Instant::now();
            let expired_unseen = |until: Instant| now >= until && self.last_render < until;
            moving |= expired_unseen(hint_until);
            moving |= self.notice.as_ref().is_some_and(|(_, until)| expired_unseen(*until));
        }
        if matches!(self.screen, Screen::Library) && !self.library.games.is_empty() {
            let row = (self.library.focus / COLUMNS) as f32;
            let visible = GRID_BOTTOM - GRID_TOP;
            let (top, bottom) = (row * ROW_H, row * ROW_H + ROW_H);
            let mut target = self.library.scroll;
            if top < target {
                target = top;
            } else if bottom > target + visible {
                target = bottom - visible;
            }
            let delta = target - self.library.scroll;
            if delta.abs() > 0.5 {
                self.library.scroll += delta * 0.25;
                moving = true;
            } else {
                self.library.scroll = target;
            }
        }
        moving
    }

    /// Draws a frame when anything changed. `video` is whether the simulator has a picture to
    /// show under the UI; `captured` whether the stream currently owns keyboard and mouse.
    pub fn render(&mut self, window: &mut Window, video: bool, captured: bool) -> bool {
        let moving = self.animate();
        let shoot = self.screenshot.as_ref().is_some_and(|(at, _)| Instant::now() >= *at);
        if !self.dirty && !moving && !shoot {
            return false;
        }
        self.dirty = false;
        self.last_render = Instant::now();
        self.painter.begin(window);
        match self.screen {
            Screen::Streaming { .. } if video => window.draw_video(),
            Screen::Streaming { .. } => window.clear(CLEAR),
            _ => window.clear(BACKGROUND),
        }
        self.draw_screen(window, captured);
        if self.stats_visible && self.is_streaming() {
            self.draw_stats(window);
        }
        self.draw_menu(window);
        if shoot {
            self.save_screenshot(window);
        }
        window.present();
        true
    }

    fn save_screenshot(&mut self, window: &mut Window) {
        let Some((_, path)) = self.screenshot.take() else { return };
        match window.read_pixels() {
            Ok((w, h, pixels)) => match image::save_buffer(&path, &pixels, w, h, image::ColorType::Rgba8) {
                Ok(()) => println!("screenshot: {}", path.display()),
                Err(error) => eprintln!("screenshot {}: {error}", path.display()),
            },
            Err(error) => eprintln!("screenshot: {error}"),
        }
        self.request_quit();
    }

    fn draw_screen(&mut self, window: &mut Window, captured: bool) {
        let p = &mut self.painter;
        let t = self.clock.elapsed().as_secs_f32();
        match &mut self.screen {
            Screen::Starting(status) | Screen::Ending(status) => {
                let status = status.clone();
                brand(p, window);
                spinner(p, window, CANVAS_W / 2.0, 500.0, t);
                p.text(window, &status, CANVAS_W / 2.0, 580.0, 34.0, DIM, false, Align::Center);
            }
            Screen::Fatal => brand(p, window),
            Screen::SignIn { code, uri, qr, texture } => {
                if texture.is_none() {
                    if let Some((side, pixels)) = qr.as_ref() {
                        *texture = window.texture(*side, *side, pixels).ok();
                    }
                }
                let (code, uri) = (code.clone(), uri.clone());
                brand(p, window);
                p.text(window, "Sign in to GeForce NOW", 240.0, 260.0, 56.0, TEXT, true, Align::Left);
                p.text(window, "1. On your phone or computer, go to", 240.0, 380.0, 32.0, DIM, false, Align::Left);
                p.text(window, &uri, 240.0, 430.0, 40.0, TEXT, true, Align::Left);
                p.text(window, "2. Enter this code", 240.0, 530.0, 32.0, DIM, false, Align::Left);
                let box_w = p.text_width(&code, 72.0, true) + 96.0;
                p.round_rect(window, Rect::new(240.0, 590.0, box_w, 130.0), 16.0, PANEL);
                p.text(window, &code, 240.0 + box_w / 2.0, 610.0, 72.0, ACCENT, true, Align::Center);
                p.text(window, "Or scan the code with your phone.", 240.0, 790.0, 28.0, DIM, false, Align::Left);
                let qr_rect = Rect::new(1220.0, 260.0, 460.0, 460.0);
                p.round_rect(window, qr_rect.inset(-16.0), 20.0, [255, 255, 255, 255]);
                if let Some(texture) = texture.as_ref() {
                    p.image(window, texture, qr_rect, 255);
                }
                spinner(p, window, 1450.0, 830.0, t);
                p.text(window, "Waiting for you to sign in", 1450.0, 880.0, 26.0, DIM, false, Align::Center);
            }
            Screen::Library => self.draw_library(window),
            Screen::Controllers => self.draw_controllers(window),
            Screen::Settings => self.draw_settings(window),
            Screen::Launching { title, status, queue } => {
                let (title, status, queue) = (title.clone(), status.clone(), *queue);
                brand(p, window);
                p.text(window, &p.fit(&title, 60.0, true, 1500.0), CANVAS_W / 2.0, 330.0, 60.0, TEXT, true, Align::Center);
                spinner(p, window, CANVAS_W / 2.0, 520.0, t);
                match queue {
                    Some(position) => {
                        p.text(window, &format!("You're number {position} in the queue"), CANVAS_W / 2.0, 610.0, 40.0, ACCENT, true, Align::Center);
                        p.text(window, "Your game starts automatically when a seat is free.", CANVAS_W / 2.0, 670.0, 28.0, DIM, false, Align::Center);
                    }
                    None => p.text(window, &status, CANVAS_W / 2.0, 610.0, 34.0, DIM, false, Align::Center),
                }
                footer(p, window, "Back  Cancel");
            }
            Screen::Streaming { first_frame, hint_until } => {
                let (first_frame, hint) = (*first_frame, Instant::now() < *hint_until);
                if !first_frame {
                    p.text(window, "Starting the stream", CANVAS_W / 2.0, 600.0, 34.0, DIM, false, Align::Center);
                }
                let notice = self.notice.as_ref().filter(|(_, until)| Instant::now() < *until).map(|(text, _)| text.as_str());
                let keyboard = self.keyboard.is_some().then_some(if TV {
                    "Typing goes to the game  ·  Back closes the keyboard"
                } else {
                    "Typing goes to the game  ·  Esc to stop"
                });
                let hint = if let Some(text) = notice.or(keyboard) {
                    Some(text)
                } else if !TV && !captured && self.menu.is_none() {
                    Some("Click the picture to play  ·  Esc for the menu")
                } else if hint && self.menu.is_none() {
                    Some(if TV {
                        "Back: menu  ·  Red: statistics  ·  Green: keyboard  ·  Blue: right click"
                    } else {
                        "Ctrl+Alt gives you the mouse back  ·  F3 for statistics"
                    })
                } else {
                    None
                };
                if let Some(hint) = hint {
                    let w = p.text_width(hint, 28.0, false) + 64.0;
                    p.round_rect(window, Rect::new((CANVAS_W - w) / 2.0, 40.0, w, 64.0), 32.0, [20, 22, 26, 210]);
                    p.text(window, hint, CANVAS_W / 2.0, 54.0, 28.0, TEXT, false, Align::Center);
                }
            }
        }
    }

    fn draw_library(&mut self, window: &mut Window) {
        let scale = self.painter.scale();
        let p = &mut self.painter;
        p.text(window, "GeForce NOW", MARGIN, 64.0, 48.0, TEXT, true, Align::Left);
        p.text(window, &self.user, CANVAS_W - MARGIN, 76.0, 30.0, DIM, false, Align::Right);
        footer(p, window, "OK  Play     Back  Exit");
        for (index, (rect, label, _)) in self.header_buttons().into_iter().enumerate() {
            let focused = self.library.header && index == self.library.header_focus;
            let p = &mut self.painter;
            if focused {
                p.halo(window, rect, 30.0, 4.0, ACCENT);
            }
            p.round_rect(window, rect, 30.0, if focused { ACCENT } else { BUTTON });
            let color = if focused { ON_ACCENT } else { TEXT };
            p.text(window, &label, rect.x + rect.w / 2.0, rect.y + 14.0, 28.0, color, true, Align::Center);
        }
        let p = &mut self.painter;
        if self.library.games.is_empty() {
            p.text(window, "Your library is empty", CANVAS_W / 2.0, 440.0, 44.0, TEXT, true, Align::Center);
            p.text(window, "Add games at play.geforcenow.com and they will show up here.", CANVAS_W / 2.0, 510.0, 30.0, DIM, false, Align::Center);
            return;
        }

        // Upload finished artwork (textures can only be made on this thread).
        for (url, pixels) in std::mem::take(&mut self.library.pending) {
            let state = match window.texture(pixels.width, pixels.height, &pixels.rgba) {
                Ok(texture) => Art::Ready(texture),
                Err(_) => Art::Missing,
            };
            self.library.art.insert(url, state);
        }

        let first_row = (self.library.scroll / ROW_H).floor().max(0.0) as usize;
        let rows = ((GRID_BOTTOM - GRID_TOP) / ROW_H).ceil() as usize + 2;
        let visible = first_row * COLUMNS..((first_row + rows) * COLUMNS).min(self.library.games.len());
        let on_screen: HashSet<String> =
            self.library.games[visible.start.min(visible.end)..visible.end].iter().filter_map(|g| g.art.clone()).collect();
        if on_screen != self.library.on_screen {
            // Requests for tiles scrolled past are dropped by the artwork thread; forgetting them
            // here makes them ask again if they come back into view.
            self.library.art.retain(|url, art| !matches!(art, Art::Requested) || on_screen.contains(url));
            let _ = self.art.send(art::Message::Visible(on_screen.clone()));
            self.library.on_screen = on_screen;
        }
        let clip = self.painter.area(Rect::new(0.0, GRID_TOP - 24.0, CANVAS_W, GRID_BOTTOM - GRID_TOP + 24.0));
        window.clip(Some(clip));
        for index in visible {
            let rect = self.tile_rect(index);
            let focused = index == self.library.focus && !self.library.header;
            let game = &self.library.games[index];
            if let Some(url) = &game.art {
                if !self.library.art.contains_key(url) {
                    self.library.art.insert(url.clone(), Art::Requested);
                    let job = art::Job {
                        url: url.clone(),
                        width: (TILE_W * scale).round() as u32,
                        height: (TILE_H * scale).round() as u32,
                        radius: (TILE_RADIUS * scale).round() as u32,
                    };
                    let _ = self.art.send(art::Message::Want(job));
                }
            }
            let p = &mut self.painter;
            if focused {
                p.halo(window, rect, TILE_RADIUS, 6.0, ACCENT);
            }
            match game.art.as_ref().and_then(|url| self.library.art.get(url)) {
                Some(Art::Ready(texture)) => {
                    p.round_rect(window, rect, TILE_RADIUS, TILE);
                    p.image(window, texture, rect, 255);
                }
                _ => {
                    p.round_rect(window, rect, TILE_RADIUS, TILE);
                    p.paragraph(window, &game.title, Rect::new(rect.x + 20.0, rect.y + 130.0, rect.w - 40.0, 200.0), 26.0, DIM, Align::Center);
                }
            }
            let title = p.fit(&game.title, 26.0, focused, TILE_W);
            p.text(window, &title, rect.x, rect.y + TILE_H + 18.0, 26.0, if focused { TEXT } else { DIM }, focused, Align::Left);
        }
        window.clip(None);
        self.evict_far_art(first_row, rows);
    }

    fn evict_far_art(&mut self, first_row: usize, rows: usize) {
        let ready = self.library.art.values().filter(|art| matches!(art, Art::Ready(_))).count();
        if ready <= ART_KEPT {
            return;
        }
        let start = first_row.saturating_sub(ART_MARGIN_ROWS) * COLUMNS;
        let end = ((first_row + rows + ART_MARGIN_ROWS) * COLUMNS).min(self.library.games.len());
        let near: HashSet<&str> = self.library.games[start.min(end)..end].iter().filter_map(|g| g.art.as_deref()).collect();
        self.library.art.retain(|url, art| !matches!(art, Art::Ready(_)) || near.contains(url.as_str()));
    }

    fn draw_stats(&mut self, window: &mut Window) {
        let rows = stats_rows(self.profile.as_ref(), self.stats.as_ref());
        // Below the start hint, which is centred along the top.
        let (x, y, w) = (40.0, 130.0, 620.0);
        let (line, size) = (40.0, 26.0);
        let p = &mut self.painter;
        p.round_rect(window, Rect::new(x, y, w, 32.0 + rows.len() as f32 * line), 16.0, [20, 22, 26, 210]);
        for (i, (label, value, warn)) in rows.iter().enumerate() {
            let row_y = y + 16.0 + i as f32 * line;
            p.text(window, label, x + 24.0, row_y, size, DIM, false, Align::Left);
            let value = p.fit(value, size, true, w - 220.0);
            p.text(window, &value, x + w - 24.0, row_y, size, if *warn { WARN } else { TEXT }, true, Align::Right);
        }
    }

    fn menu_layout(&self, menu: &Menu) -> MenuLayout {
        let w = 820.0;
        let mut h = 48.0;
        if !menu.title.is_empty() {
            h += 70.0;
        }
        if !menu.message.is_empty() {
            h += self.painter.paragraph_height(&menu.message, 30.0, w - 96.0) + 24.0;
        }
        let buttons_rel = h;
        h += menu.items.len() as f32 * 84.0 + 32.0;
        let (x, y) = ((CANVAS_W - w) / 2.0, (CANVAS_H - h) / 2.0);
        MenuLayout { x, y, w, h, buttons_y: y + buttons_rel }
    }

    fn draw_menu(&mut self, window: &mut Window) {
        let Some(menu) = &self.menu else { return };
        let layout = self.menu_layout(menu);
        let buttons = self.menu_buttons();
        let p = &mut self.painter;
        p.fill(window, Rect::new(-CANVAS_W, -CANVAS_H, CANVAS_W * 3.0, CANVAS_H * 3.0), SCRIM);
        p.round_rect(window, Rect::new(layout.x, layout.y, layout.w, layout.h), 24.0, PANEL);
        let mut y = layout.y + 48.0;
        if !menu.title.is_empty() {
            p.text(window, &menu.title, layout.x + 48.0, y, 40.0, TEXT, true, Align::Left);
            y += 70.0;
        }
        if !menu.message.is_empty() {
            p.paragraph(window, &menu.message, Rect::new(layout.x + 48.0, y, layout.w - 96.0, 400.0), 30.0, DIM, Align::Left);
        }
        for (i, ((label, _), rect)) in menu.items.iter().zip(buttons).enumerate() {
            let focused = i == menu.focus;
            p.round_rect(window, rect, 14.0, if focused { ACCENT } else { BUTTON });
            let label = p.fit(label, 30.0, true, rect.w - 48.0);
            p.text(window, &label, rect.x + rect.w / 2.0, rect.y + 15.0, 30.0, if focused { ON_ACCENT } else { TEXT }, true, Align::Center);
        }
    }
}

/// The TV's on-screen keyboard, while we have it up.
struct Keyboard {
    opened: Instant,
    /// The TV has reported the panel on screen at least once.
    seen: bool,
}

impl App {
    fn open_keyboard(&mut self) {
        if self.keyboard.is_some() || !self.is_streaming() {
            return;
        }
        println!("keyboard: open (screen keyboard support={})", gfn_sdl::has_screen_keyboard());
        gfn_sdl::start_text_input();
        self.keyboard = Some(Keyboard { opened: Instant::now(), seen: false });
        self.dirty = true;
    }

    fn close_keyboard(&mut self) {
        if self.keyboard.take().is_some() {
            gfn_sdl::stop_text_input();
            self.dirty = true;
        }
    }

    /// Notices when the TV takes its keyboard down by itself, or never puts it up. Once per
    /// main-loop iteration.
    pub fn poll_keyboard(&mut self, window: &Window) {
        if !TV {
            return;
        }
        let Some(keyboard) = &mut self.keyboard else { return };
        let shown = window.screen_keyboard_shown();
        if shown && !keyboard.seen {
            keyboard.seen = true;
            println!("keyboard: shown");
        } else if !shown && keyboard.seen {
            println!("keyboard: closed by the TV");
            self.close_keyboard();
        } else if !shown && keyboard.opened.elapsed() > KEYBOARD_APPEARS_WITHIN {
            println!("keyboard: did not appear (window focus={})", window.has_input_focus());
            self.close_keyboard();
            self.notice = Some(("The TV's keyboard didn't open".into(), Instant::now() + NOTICE_FOR));
        }
    }
}

const LIST_X: f32 = 800.0;
const LIST_W: f32 = CANVAS_W - MARGIN - LIST_X;
const LIST_TOP: f32 = 180.0;
const LIST_BOTTOM: f32 = 990.0;
const ROW_STEP: f32 = 100.0;
const ROW_INNER: f32 = 84.0;

#[derive(Debug, Clone)]
enum Item {
    Device(bluetooth::Device),
    Search,
}

struct Row {
    rect: Rect,
    item: Item,
}

/// Section headings and placeholders between the rows.
struct Label {
    text: &'static str,
    y: f32,
    heading: bool,
}

impl App {
    fn open_controllers(&mut self) {
        self.bluetooth.send(Command::Open);
        self.controllers_focus = 0;
        self.screen = Screen::Controllers;
        self.dirty = true;
    }

    fn controllers_nav(&mut self, nav: Nav) {
        let rows = self.controller_rows();
        let last = rows.len().saturating_sub(1);
        match nav {
            Nav::Up => self.controllers_focus = self.controllers_focus.saturating_sub(1),
            Nav::Down => self.controllers_focus = (self.controllers_focus + 1).min(last),
            Nav::Back => {
                self.bluetooth.send(Command::Close);
                self.screen = Screen::Library;
            }
            Nav::Select => match rows.into_iter().nth(self.controllers_focus).map(|row| row.item) {
                Some(Item::Search) => self.bluetooth.send(Command::Search),
                Some(Item::Device(device)) => self.select_device(device),
                None => {}
            },
            Nav::Left | Nav::Right => {}
        }
    }

    fn select_device(&mut self, device: bluetooth::Device) {
        let address = device.address.clone();
        let paired = self.controllers.paired.iter().any(|d| d.address == address);
        if !paired {
            if matches!(device.status, Status::Nearby | Status::Failed(_)) {
                self.bluetooth.send(Command::Pair(address));
            }
            return;
        }
        let first = match device.status {
            Status::Connected => ("Disconnect".to_owned(), Action::Bluetooth(Command::Disconnect(address.clone()))),
            Status::Paired | Status::Failed(_) => ("Connect".to_owned(), Action::Bluetooth(Command::Connect(address.clone()))),
            _ => return,
        };
        self.menu = Some(Menu::new(
            device.name,
            "",
            vec![first, ("Forget".into(), Action::Bluetooth(Command::Forget(address))), ("Cancel".into(), Action::CloseMenu)],
            Action::CloseMenu,
        ));
    }

    fn controller_layout(&self) -> (Vec<Row>, Vec<Label>) {
        let snapshot = &self.controllers;
        let mut rows = Vec::new();
        let mut labels = Vec::new();
        let mut y = LIST_TOP;
        let section = |rows: &mut Vec<Row>, labels: &mut Vec<Label>, y: &mut f32, title, devices: &[bluetooth::Device], empty| {
            labels.push(Label { text: title, y: *y, heading: true });
            *y += 56.0;
            if devices.is_empty() {
                labels.push(Label { text: empty, y: *y, heading: false });
                *y += 64.0;
            }
            for device in devices {
                rows.push(Row { rect: Rect::new(LIST_X, *y, LIST_W, ROW_INNER), item: Item::Device(device.clone()) });
                *y += ROW_STEP;
            }
            *y += 24.0;
        };
        section(&mut rows, &mut labels, &mut y, "Your controllers", &snapshot.paired, "None paired yet");
        let empty = if snapshot.searching { "Looking for devices in pairing mode…" } else { "Nothing found" };
        section(&mut rows, &mut labels, &mut y, "Nearby", &snapshot.nearby, empty);
        if !snapshot.searching && snapshot.unavailable.is_none() {
            rows.push(Row { rect: Rect::new(LIST_X, y, 360.0, 68.0), item: Item::Search });
        }
        // Keep the focused row on screen.
        let focus = self.controllers_focus.min(rows.len().saturating_sub(1));
        let scroll = rows.get(focus).map_or(0.0, |row| (row.rect.y + row.rect.h - LIST_BOTTOM).max(0.0));
        for row in &mut rows {
            row.rect.y -= scroll;
        }
        for label in &mut labels {
            label.y -= scroll;
        }
        (rows, labels)
    }

    fn controller_rows(&self) -> Vec<Row> {
        self.controller_layout().0
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

    fn open_header(&mut self, button: Header) {
        match button {
            Header::Settings => {
                self.settings_focus = 0;
                self.screen = Screen::Settings;
            }
            Header::Controllers => self.open_controllers(),
        }
        self.dirty = true;
    }

    fn draw_controllers(&mut self, window: &mut Window) {
        let t = self.clock.elapsed().as_secs_f32();
        let (rows, labels) = self.controller_layout();
        let focus = self.controllers_focus.min(rows.len().saturating_sub(1));
        let snapshot = self.controllers.clone();
        let p = &mut self.painter;
        p.text(window, "Controllers", MARGIN, 64.0, 48.0, TEXT, true, Align::Left);
        footer(p, window, "OK  Select     Back  Done");

        let mut y = LIST_TOP;
        p.text(window, "Pair a controller", MARGIN, y, 36.0, TEXT, true, Align::Left);
        y += 70.0;
        for step in [
            "1. Put the controller in pairing mode.",
            "Xbox: hold the pair button on top until the logo flashes fast.",
            "PlayStation: hold Share (Create) and PS until the light flashes.",
            "8BitDo, Switch Pro: hold the pair button.",
            "2. Pick it under Nearby when it shows up.",
        ] {
            let indent = if step.as_bytes().get(1) == Some(&b'.') { 0.0 } else { 32.0 };
            let color = if indent == 0.0 { TEXT } else { DIM };
            y += p.paragraph(window, step, Rect::new(MARGIN + indent, y, 600.0 - indent, 200.0), 28.0, color, Align::Left) + 14.0;
        }
        if let Some(reason) = &snapshot.unavailable {
            y += 30.0;
            p.paragraph(window, &format!("Bluetooth isn't available: {reason}"), Rect::new(MARGIN, y, 600.0, 300.0), 28.0, WARN, Align::Left);
        } else if snapshot.searching {
            y += 40.0;
            spinner(p, window, MARGIN + 30.0, y + 16.0, t);
            p.text(window, "Searching", MARGIN + 80.0, y, 30.0, DIM, false, Align::Left);
        }

        let clip = p.area(Rect::new(LIST_X - 16.0, LIST_TOP - 16.0, LIST_W + 32.0, LIST_BOTTOM - LIST_TOP + 32.0));
        window.clip(Some(clip));
        for label in &labels {
            let (size, color) = if label.heading { (30.0, TEXT) } else { (28.0, DIM) };
            p.text(window, label.text, LIST_X, label.y, size, color, label.heading, Align::Left);
        }
        for (index, row) in rows.iter().enumerate() {
            let focused = index == focus;
            if focused {
                p.halo(window, row.rect, 14.0, 4.0, ACCENT);
            }
            match &row.item {
                Item::Search => {
                    p.round_rect(window, row.rect, 14.0, if focused { ACCENT } else { BUTTON });
                    let color = if focused { ON_ACCENT } else { TEXT };
                    p.text(window, "Search again", row.rect.x + row.rect.w / 2.0, row.rect.y + 15.0, 30.0, color, true, Align::Center);
                }
                Item::Device(device) => {
                    p.round_rect(window, row.rect, 14.0, if focused { BUTTON } else { PANEL });
                    let name = p.fit(&device.name, 30.0, true, row.rect.w - 64.0);
                    let name_color = if device.controller { TEXT } else { DIM };
                    p.text(window, &name, row.rect.x + 32.0, row.rect.y + 10.0, 30.0, name_color, true, Align::Left);
                    let (status, color) = match &device.status {
                        Status::Connected => ("Connected".to_owned(), ACCENT),
                        Status::Paired => ("Not connected. Turn it on, or select to connect.".to_owned(), DIM),
                        Status::Nearby => ("Select to pair".to_owned(), DIM),
                        Status::Pairing => ("Pairing…".to_owned(), TEXT),
                        Status::Connecting => ("Connecting…".to_owned(), TEXT),
                        Status::Disconnecting => ("Disconnecting…".to_owned(), TEXT),
                        Status::Failed(message) => (message.clone(), WARN),
                    };
                    let status = p.fit(&status, 24.0, false, row.rect.w - 64.0);
                    p.text(window, &status, row.rect.x + 32.0, row.rect.y + 50.0, 24.0, color, false, Align::Left);
                }
            }
        }
        window.clip(None);
    }

    /// The controllers screen animates while it searches or something is in progress.
    fn controllers_busy(&self) -> bool {
        self.controllers.searching
            || self.controllers.paired.iter().chain(&self.controllers.nearby).any(|d| {
                matches!(d.status, Status::Pairing | Status::Connecting | Status::Disconnecting)
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Header {
    Settings,
    Controllers,
}

const HEADER_BUTTONS: [Header; 2] = [Header::Settings, Header::Controllers];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingRow {
    Resolution,
    FrameRate,
    Bitrate,
    Codec,
    Hdr,
    Player,
}

impl SettingRow {
    const ALL: [Self; 6] = [Self::Resolution, Self::FrameRate, Self::Bitrate, Self::Codec, Self::Hdr, Self::Player];

    fn label(self) -> &'static str {
        match self {
            Self::Resolution => "Resolution",
            Self::FrameRate => "Frame rate",
            Self::Bitrate => "Maximum bitrate",
            Self::Codec => "Video codec",
            Self::Hdr => "HDR",
            Self::Player => "Video player",
        }
    }

    fn help(self) -> &'static str {
        match self {
            Self::Resolution => "The picture size the server streams. 1440p and 4K need a membership that includes them; otherwise the server picks the closest size it allows.",
            Self::FrameRate => "Frames per second. 120 fps depends on your membership and the game; the server falls back to what it allows.",
            Self::Bitrate => "The most data the stream may use. Higher is sharper but needs a steadier network; on Wi-Fi, 20 Mbps or less avoids stutter.",
            Self::Codec => "H.265 gives a sharper picture at the same bitrate. H.264 may respond a few milliseconds faster.",
            Self::Hdr => "Streams HDR10 when the game and your membership support it, and needs H.265 (turning it on selects it). Experimental on this TV.",
            Self::Player => "Starfish is the TV's media player. NDL feeds the decoder directly and may respond faster, but plays only H.264 without HDR (choosing it selects H.264). Experimental.",
        }
    }
}

const SETTINGS_TOP: f32 = 200.0;
const SETTINGS_W: f32 = 1100.0;

fn settings_row_rect(index: usize) -> Rect {
    Rect::new(MARGIN, SETTINGS_TOP + index as f32 * 100.0, SETTINGS_W, 84.0)
}

fn settings_value_x(row: Rect) -> f32 {
    row.x + row.w - 200.0
}

/// Click targets for the ‹ and › drawn either side of the value, wider than the glyphs.
fn settings_arrow_rects(index: usize) -> (Rect, Rect) {
    let row = settings_row_rect(index);
    let value_x = settings_value_x(row);
    let target = |centre: f32| Rect::new(centre - 50.0, row.y, 100.0, row.h);
    (target(value_x - 150.0), target(value_x + 150.0))
}

impl App {
    fn setting_value(&self, row: SettingRow) -> String {
        let s = &self.stream_settings;
        match row {
            SettingRow::Resolution => settings::RESOLUTIONS
                .iter()
                .find(|(value, _)| *value == s.resolution)
                .map_or_else(|| s.resolution.clone(), |(_, label)| (*label).to_owned()),
            SettingRow::FrameRate => format!("{} fps", s.fps),
            SettingRow::Bitrate => format!("{} Mbps", s.max_bitrate_mbps),
            SettingRow::Codec => settings::CODECS
                .iter()
                .find(|(value, _)| *value == s.codec)
                .map_or_else(|| s.codec.clone(), |(_, label)| (*label).to_owned()),
            SettingRow::Hdr => (if s.hdr { "On" } else { "Off" }).to_owned(),
            SettingRow::Player => settings::PLAYERS
                .iter()
                .find(|(value, _)| *value == s.player)
                .map_or_else(|| s.player.clone(), |(_, label)| (*label).to_owned()),
        }
    }

    /// Steps the row's value; `wrap` lets OK and clicks cycle past the last choice.
    fn change_setting(&mut self, row: SettingRow, forward: bool, wrap: bool) {
        fn next<T: PartialEq + Copy>(choices: &[T], current: T, forward: bool, wrap: bool) -> T {
            let stepped = settings::step(choices, current, forward);
            if wrap && stepped == current { choices[0] } else { stepped }
        }
        let s = &mut self.stream_settings;
        match row {
            SettingRow::Resolution => {
                let values: Vec<&str> = settings::RESOLUTIONS.iter().map(|(v, _)| *v).collect();
                s.resolution = next(&values, s.resolution.as_str(), forward, wrap).to_owned();
            }
            SettingRow::FrameRate => s.fps = next(&settings::FRAME_RATES, s.fps, forward, wrap),
            SettingRow::Bitrate => s.max_bitrate_mbps = next(&settings::BITRATES, s.max_bitrate_mbps, forward, wrap),
            SettingRow::Codec => {
                let values: Vec<&str> = settings::CODECS.iter().map(|(v, _)| *v).collect();
                s.codec = next(&values, s.codec.as_str(), forward, wrap).to_owned();
                s.hdr &= s.codec == "h265";
                if s.codec != "h264" {
                    s.player = "starfish".into();
                }
            }
            SettingRow::Hdr => {
                s.hdr = next(&[false, true], s.hdr, forward, wrap);
                if s.hdr {
                    s.codec = "h265".into();
                    s.player = "starfish".into();
                }
            }
            SettingRow::Player => {
                let values: Vec<&str> = settings::PLAYERS.iter().map(|(v, _)| *v).collect();
                s.player = next(&values, s.player.as_str(), forward, wrap).to_owned();
                if s.player == "ndl" {
                    s.codec = "h264".into();
                    s.hdr = false;
                }
            }
        }
        if let Err(error) = self.stream_settings.save(&self.settings_path) {
            eprintln!("saving settings: {error}");
        }
    }

    fn settings_nav(&mut self, nav: Nav) {
        let row = SettingRow::ALL[self.settings_focus.min(SettingRow::ALL.len() - 1)];
        match nav {
            Nav::Up => self.settings_focus = self.settings_focus.saturating_sub(1),
            Nav::Down => self.settings_focus = (self.settings_focus + 1).min(SettingRow::ALL.len() - 1),
            Nav::Left => self.change_setting(row, false, false),
            Nav::Right => self.change_setting(row, true, false),
            Nav::Select => self.change_setting(row, true, true),
            Nav::Back => self.screen = Screen::Library,
        }
    }

    fn draw_settings(&mut self, window: &mut Window) {
        let focus = self.settings_focus.min(SettingRow::ALL.len() - 1);
        let values: Vec<String> = SettingRow::ALL.iter().map(|&row| self.setting_value(row)).collect();
        let p = &mut self.painter;
        p.text(window, "Streaming settings", MARGIN, 64.0, 48.0, TEXT, true, Align::Left);
        footer(p, window, "Left/Right  Change     Back  Done");
        for (index, (row, value)) in SettingRow::ALL.iter().zip(&values).enumerate() {
            let rect = settings_row_rect(index);
            let focused = index == focus;
            if focused {
                p.halo(window, rect, 14.0, 4.0, ACCENT);
            }
            p.round_rect(window, rect, 14.0, if focused { BUTTON } else { PANEL });
            p.text(window, row.label(), rect.x + 32.0, rect.y + 24.0, 30.0, TEXT, true, Align::Left);
            let value_x = settings_value_x(rect);
            if focused {
                p.text(window, "‹", value_x - 150.0, rect.y + 20.0, 34.0, ACCENT, true, Align::Center);
                p.text(window, "›", value_x + 150.0, rect.y + 20.0, 34.0, ACCENT, true, Align::Center);
            }
            p.text(window, value, value_x, rect.y + 24.0, 30.0, if focused { TEXT } else { DIM }, true, Align::Center);
        }
        let help_y = settings_row_rect(SettingRow::ALL.len()).y + 20.0;
        let used = p.paragraph(window, SettingRow::ALL[focus].help(), Rect::new(MARGIN, help_y, SETTINGS_W, 200.0), 28.0, DIM, Align::Left);
        p.text(window, "Changes apply to the next game you start.", MARGIN, help_y + used + 30.0, 28.0, TEXT, false, Align::Left);
    }
}

struct MenuLayout {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    buttons_y: f32,
}

fn conflict_menu(running: &str, wanted: &str) -> Menu {
    Menu::new(
        "A session is already running",
        format!("{running} is still running on your account. Pick up where you left off, or end it and play {wanted}."),
        vec![
            (format!("Resume {running}"), Action::Takeover(Takeover::Resume)),
            (format!("End it and play {wanted}"), Action::Takeover(Takeover::Replace)),
            ("Cancel".into(), Action::Takeover(Takeover::Cancel)),
        ],
        Action::Takeover(Takeover::Cancel),
    )
}

fn exit_menu() -> Menu {
    Menu::new(
        "Exit GeForce NOW?",
        "",
        vec![("Exit".into(), Action::Exit), ("Cancel".into(), Action::CloseMenu)],
        Action::CloseMenu,
    )
}

/// The statistics panel's rows: label, value, and whether the value is worth a warning colour.
fn stats_rows(profile: Option<&Profile>, stats: Option<&Stats>) -> Vec<(&'static str, String, bool)> {
    fn number(value: Option<f64>, decimals: usize, unit: &str) -> String {
        value.map_or_else(|| "–".to_owned(), |v| format!("{v:.decimals$} {unit}"))
    }
    let stats = stats.copied().unwrap_or_default();
    let mut rows = Vec::new();
    if let Some(profile) = profile {
        let mut stream = profile.resolution.replace('x', "×");
        if let Some(fps) = profile.fps {
            stream.push_str(&format!(" · {fps} fps"));
        }
        if !profile.codec.is_empty() {
            stream.push_str(&format!(" · {}", profile.codec));
        }
        if profile.hdr {
            stream.push_str(" · HDR");
        }
        rows.push(("Stream", stream, false));
        let server = [profile.server.as_str(), profile.gpu.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>();
        if !server.is_empty() {
            rows.push(("Server", server.join(" · "), false));
        }
    }
    rows.push(("Frame rate", number(stats.fps, 1, "fps"), false));
    let mut bitrate = number(stats.bitrate_mbps, 1, "Mbps");
    if let (Some(_), Some(max)) = (stats.bitrate_mbps, profile.and_then(|p| p.max_bitrate_mbps)) {
        bitrate = format!("{} / {max:.0} Mbps", number(stats.bitrate_mbps, 1, "").trim_end());
    }
    rows.push(("Bitrate", bitrate, false));
    rows.push(("Ping", number(stats.ping_ms, 0, "ms"), stats.ping_ms.is_some_and(|ms| ms > 50.0)));
    rows.push(("Jitter", number(stats.jitter_ms, 1, "ms"), stats.jitter_ms.is_some_and(|ms| ms > 10.0)));
    rows.push(("Packet loss", number(stats.loss_percent, 1, "%"), stats.loss_percent.is_some_and(|p| p >= 1.0)));
    rows.push(("Decode", number(stats.decode_ms, 1, "ms"), false));
    if TV || stats.display_lag_ms.is_some() {
        let lag = stats.display_lag_ms.map(|ms| ms as f64);
        rows.push(("Display lag", number(lag, 0, "ms"), lag.is_some_and(|ms| ms > 100.0)));
    }
    rows
}

fn brand(p: &mut Painter, window: &mut Window) {
    p.text(window, "GeForce NOW", MARGIN, 64.0, 48.0, TEXT, true, Align::Left);
}

fn footer(p: &mut Painter, window: &mut Window, text: &str) {
    p.text(window, text, CANVAS_W - MARGIN, 1020.0, 26.0, DIM, false, Align::Right);
}

/// Eight dots chasing round a circle.
fn spinner(p: &mut Painter, window: &mut Window, cx: f32, cy: f32, t: f32) {
    let head = (t * 10.0) as usize;
    for i in 0..8usize {
        let angle = i as f32 / 8.0 * std::f32::consts::TAU;
        let age = (head + 8 - i % 8) % 8;
        let alpha = (255 - age * 28) as u8;
        let (x, y) = (cx + 30.0 * angle.cos(), cy + 30.0 * angle.sin());
        p.round_rect(window, Rect::new(x - 6.0, y - 6.0, 12.0, 12.0), 6.0, [ACCENT[0], ACCENT[1], ACCENT[2], alpha]);
    }
}
