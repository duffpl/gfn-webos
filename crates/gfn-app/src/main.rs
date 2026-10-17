//! GeForce NOW for LG webOS.
//!
//!     gfn-app [--core PATH] [--data-dir DIR] [--codec C] [--max-bitrate MBPS] [--verbose]
//!             [--screenshot FILE.png [--screenshot-after SECONDS]] [--preview SCREEN]
//!
//! `--screenshot` and `--preview` are developer aids: save one drawn frame and exit; show one
//! screen with sample data and no core.
//!
//! On the TV the picture goes to the Starfish video plane beneath a transparent fullscreen
//! window and the UI is drawn on that window; in the desktop simulator (`--features sim`) the
//! picture is decoded on the host and drawn into the window under the UI.
//!
//! The main thread owns SDL: it pumps events, routes them to the UI or the stream, and draws only
//! when something changed. Core calls and the engine block, so they run on worker threads that
//! report back through [`Update`]s.

mod account;
mod art;
mod bluetooth;
mod crash;
mod gfx;
mod qr;
mod screensaver;
mod session;
mod settings;
mod ui;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SendError, Sender, channel};

use gfn_input::evdev::PadEvent;
use gfn_input::{InputCapture, Pointer};
use gfn_sdl::{GameController, InputEvent, SystemCursors};
use gfn_sink::{AudioOut, SinkBackend, create_runtime};

pub const TV: bool = cfg!(all(target_os = "linux", target_arch = "arm"));

/// The main loop's sleep during a TV stream: the controller keepalive interval.
const STREAM_WAIT_MS: u32 = 100;
/// The Magic Remote's pointer hides after this long without motion; LG's SDL defaults to five
/// minutes, which leaves it over the library while browsing with the arrow keys or a pad.
const CURSOR_SLEEP_MS: &str = "3000";

pub enum Update {
    Account(account::Progress),
    Art { url: String, pixels: Option<art::Pixels> },
    Bluetooth(bluetooth::Snapshot),
    Session(session::Progress),
}

/// The UI thread's inbox. Each send also wakes the main loop, which can then sleep for long
/// stretches during a stream instead of polling the channel.
#[derive(Clone)]
pub struct Updates {
    tx: Sender<Update>,
    waker: Option<gfn_sdl::Waker>,
}

impl Updates {
    pub fn new(tx: Sender<Update>, waker: Option<gfn_sdl::Waker>) -> Self {
        Self { tx, waker }
    }

    pub fn send(&self, update: Update) -> Result<(), SendError<Update>> {
        self.tx.send(update)?;
        if let Some(waker) = self.waker {
            waker.wake();
        }
        Ok(())
    }
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn config(args: &[String]) -> Result<account::Config, String> {
    let core = match flag(args, "--core") {
        Some(path) => PathBuf::from(path),
        None => std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("opennow-core"),
    };
    if !core.is_file() {
        return Err(format!("{}: no opennow-core there (use --core)", core.display()));
    }
    let data_dir = match flag(args, "--data-dir") {
        Some(dir) => PathBuf::from(dir),
        None => default_data_dir()?,
    };
    let verbose = args.iter().any(|a| a == "--verbose");
    Ok(account::Config { core, data_dir, verbose })
}

/// The saved streaming settings, with `--codec` and `--max-bitrate` overriding them for this run.
fn stream_settings(args: &[String], path: &std::path::Path) -> Result<settings::StreamSettings, String> {
    let mut stream = settings::StreamSettings::load(path);
    if let Some(codec) = flag(args, "--codec") {
        stream.codec = codec.to_owned();
    }
    if let Some(value) = flag(args, "--max-bitrate") {
        stream.max_bitrate_mbps = value.parse().map_err(|_| format!("--max-bitrate {value}: not a whole number"))?;
    }
    Ok(stream)
}

/// The installed package's id: ACB binds the video plane to it, and it names the data directory.
/// webOS runs a native app from `.../usr/palm/applications/<id>/`.
fn app_id() -> String {
    std::env::var("APPID").ok().filter(|id| !id.is_empty()).unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.parent()?.file_name()?.to_str()?.to_owned()))
            .filter(|_| TV)
            .unwrap_or_else(|| "io.github.gfnwebos".into())
    })
}

/// Sign-in and caches must outlive reinstalls, so on the TV they live beside the app install,
/// not inside it (appinstalld replaces the app directory wholesale, and `$HOME` is that directory).
/// The app jail mounts `/media/developer` read-write; the first candidate that takes a write wins.
fn default_data_dir() -> Result<PathBuf, String> {
    if TV {
        let id = app_id();
        let candidates = [
            PathBuf::from(format!("/media/developer/{id}-data")),
            PathBuf::from(format!("/media/developer/temp/{id}-data")),
            PathBuf::from(format!("/tmp/{id}-data")),
        ];
        return candidates
            .iter()
            .find(|dir| writable(dir))
            .cloned()
            .ok_or_else(|| format!("no writable data directory among {candidates:?}"));
    }
    let home = std::env::var("HOME").map_err(|_| "HOME is not set; pass --data-dir")?;
    Ok(PathBuf::from(home).join(".local/share/gfn-webos"))
}

fn writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(".write-test");
    let ok = std::fs::create_dir_all(dir).is_ok() && std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(probe);
    ok
}

/// A launched TV app's stdout and stderr go nowhere readable; the jail's `/tmp` is the host's.
fn redirect_output_on_tv() {
    if !TV {
        return;
    }
    use std::os::fd::AsRawFd;
    unsafe extern "C" {
        fn dup2(old: i32, new: i32) -> i32;
    }
    if let Ok(log) = std::fs::File::create(format!("/tmp/{}.log", app_id())) {
        unsafe {
            dup2(log.as_raw_fd(), 1);
            dup2(log.as_raw_fd(), 2);
        }
    }
    // native/starfish.c's pipeline, callback and video-plane diagnostics, into the same log.
    gfn_starfish::open_event_log(None);
}

fn main() {
    if TV {
        // Without it webOS keeps BACK for itself and shows the launcher bar. Read when SDL starts.
        // SAFETY: no other thread exists yet.
        unsafe { std::env::set_var("SDL_WEBOS_ACCESS_POLICY_KEYS_BACK", "true") };
    }
    redirect_output_on_tv();
    crash::install();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = run(&args) {
        eprintln!("gfn-app: {error}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let config = config(args)?;
    println!("data directory: {}", config.data_dir.display());
    let settings_path = settings::StreamSettings::path(&config.data_dir);
    let stream_settings = stream_settings(args, &settings_path)?;
    let sdl = gfn_sdl::Sdl::init(gfn_sdl::INIT_VIDEO | gfn_sdl::INIT_AUDIO | gfn_sdl::INIT_GAMECONTROLLER)?;
    let mut window = if TV {
        gfn_sdl::Window::open(&sdl, "GeForce NOW", 1920, 1080, true, true)?
    } else {
        gfn_sdl::Window::open(&sdl, "GeForce NOW (simulator)", 1280, 720, false, true)?
    };
    if TV && !gfn_sdl::set_hint("SDL_WEBOS_CURSOR_SLEEP_TIME", CURSOR_SLEEP_MS) {
        eprintln!("cursor sleep time not taken: {}", gfn_sdl::error());
    }
    let controller_use = screensaver::start(TV, app_id());
    let audio = match AudioOut::open(&sdl, 48_000, 2, 480) {
        Ok(audio) => Some(Arc::new(audio)),
        Err(error) => {
            eprintln!("audio output unavailable: {error}");
            None
        }
    };
    let (backend, frames) = backend(audio);
    let runtime = create_runtime(Arc::new(backend));
    let mut input = InputCapture::new(runtime.captured_input(), runtime.remote_cursor(), TV);
    let mut controllers: Vec<GameController> = Vec::new();
    // The TV's SDL does not see Bluetooth controllers; there they are read from evdev instead,
    // and SDL's controller events are ignored so a pad it does see is not counted twice.
    let pads = TV.then(|| {
        let waker = gfn_sdl::Waker::new(&sdl);
        gfn_input::evdev::Pads::spawn(move || waker.wake())
    });
    let mut cursors = SystemCursors::new(&sdl);
    let mut pointer = Pointer::Free;

    let quit = Arc::new(AtomicBool::new(false));
    let interrupted = Arc::clone(&quit);
    // Ctrl+C must still release the cloud seat; a second one gives up on that.
    ctrlc::set_handler(move || {
        if interrupted.swap(true, Ordering::Relaxed) {
            std::process::exit(130);
        }
        println!("interrupted; ending the session (Ctrl+C again to exit at once)");
    })
    .map_err(|e| format!("installing the Ctrl+C handler: {e}"))?;

    let (updates_tx, updates) = channel();
    let updates_tx = Updates::new(updates_tx, Some(gfn_sdl::Waker::new(&sdl)));
    let (art_tx, art_rx) = channel();
    let preview = flag(args, "--preview");
    match preview {
        None => account::start(config, art_rx, art_tx.clone(), updates_tx.clone(), Arc::clone(&quit)),
        Some(_) => art::spawn_files(art_rx, updates_tx.clone()),
    }
    let bluetooth = bluetooth::start(updates_tx.clone());
    let mut app = ui::App::new(
        gfx::Painter::new(gfx::Fonts::load()?),
        runtime,
        updates_tx,
        art_tx,
        bluetooth,
        (stream_settings, settings_path),
    );
    if let Some(screen) = preview {
        app.preview(screen)?;
    }
    if let Some(path) = flag(args, "--screenshot") {
        let delay = flag(args, "--screenshot-after").and_then(|s| s.parse().ok()).unwrap_or(10.0);
        app.screenshot_after(std::time::Duration::from_secs_f64(delay), PathBuf::from(path));
    }
    let mut video = false;

    while !app.should_exit() {
        if quit.load(Ordering::Relaxed) {
            app.request_quit();
        }
        // Idle screens sleep in SDL until input arrives; a live simulator stream wakes for each
        // decoded picture instead (below), so it only peeks at the queue here. A TV stream draws
        // nothing per frame and every other thread wakes this one, so it sleeps until the next
        // controller keepalive, leaving the CPU to the video, audio and network threads.
        let idle_wait = match (app.is_streaming(), frames.is_some()) {
            (true, true) => 0,
            (true, false) if TV => STREAM_WAIT_MS,
            _ => 8,
        };
        let mut next = if idle_wait == 0 { gfn_sdl::poll_event() } else { gfn_sdl::wait_event(idle_wait) };
        while let Some(event) = next {
            match event {
                Some(InputEvent::Quit) => {
                    quit.store(true, Ordering::Relaxed);
                    app.request_quit();
                }
                Some(InputEvent::Background) => {
                    gfn_sink::set_backgrounded(true);
                    app.event(InputEvent::FocusLost, &mut input, &window);
                }
                Some(InputEvent::Foreground) => {
                    gfn_sink::set_backgrounded(false);
                    app.event(InputEvent::FocusGained, &mut input, &window);
                    // The compositor may have dropped the pointer mode and our last frame.
                    apply_pointer(&mut window, &mut cursors, &mut input, pointer);
                    app.mark_dirty();
                }
                Some(InputEvent::ControllerAdded { .. }) if TV => {}
                Some(InputEvent::ControllerAdded { device_index }) => {
                    if let Some(controller) = GameController::open(&sdl, device_index) {
                        input.controller_connected(controller.instance_id());
                        controllers.push(controller);
                    }
                }
                Some(InputEvent::ControllerRemoved { instance_id }) => {
                    input.controller_disconnected(instance_id);
                    controllers.retain(|c| c.instance_id() != instance_id);
                }
                Some(event) => {
                    match event {
                        InputEvent::ControllerButton { pressed, .. } => controller_use.button(pressed),
                        InputEvent::ControllerAxis { value, .. } => controller_use.axis(value),
                        _ => {}
                    }
                    app.event(event, &mut input, &window);
                }
                None => {}
            }
            next = gfn_sdl::poll_event();
        }
        while let Some(event) = pads.as_ref().and_then(|pads| pads.try_recv()) {
            match event {
                PadEvent::Added { id, name } => {
                    println!("controller connected: {name}");
                    input.controller_connected(id);
                }
                PadEvent::Removed { id } => input.controller_disconnected(id),
                PadEvent::Button { id, button, pressed } => {
                    controller_use.button(pressed);
                    app.event(InputEvent::ControllerButton { instance_id: id, button, pressed }, &mut input, &window);
                }
                PadEvent::Axis { id, axis, value } => {
                    controller_use.axis(value);
                    app.event(InputEvent::ControllerAxis { instance_id: id, axis, value }, &mut input, &window);
                }
            }
        }
        while let Ok(update) = updates.try_recv() {
            app.update(update, &mut input);
        }
        controller_use.streaming(app.is_streaming());
        if app.stream_has_input() {
            input.tick();
        }
        app.poll_keyboard(&window);
        let wanted = if app.stream_has_input() { input.pointer() } else { Pointer::Free };
        if wanted != pointer {
            apply_pointer(&mut window, &mut cursors, &mut input, wanted);
            pointer = wanted;
        }
        if app.take_video_reset() {
            window.drop_video();
            video = false;
        }
        if app.is_streaming() && frames.as_ref().is_some_and(|slot| upload_next_frame(slot, &mut window)) {
            video = true;
            app.mark_dirty();
        }
        app.render(&mut window, video, input.is_captured());
    }
    Ok(())
}

fn apply_pointer(window: &mut gfn_sdl::Window, cursors: &mut SystemCursors, input: &mut InputCapture, pointer: Pointer) {
    let mut relative = pointer == Pointer::Relative && input.relative_available();
    if let Err(error) = window.set_relative_mouse(relative) {
        eprintln!("{error}");
        if relative {
            input.set_relative_unavailable();
            let _ = window.set_relative_mouse(false);
            relative = false;
        }
    }
    if !relative {
        gfn_sdl::show_cursor(true);
        cursors.set(match pointer {
            Pointer::Absolute { shape } => shape,
            _ => SystemCursors::ARROW,
        });
    }
}

#[cfg(feature = "sim")]
type Frames = Arc<gfn_sink::sim::FrameSlot>;
#[cfg(not(feature = "sim"))]
type Frames = ();

/// Waits briefly for the next decoded picture and uploads it; short enough that input stays
/// responsive between pictures.
#[cfg(feature = "sim")]
fn upload_next_frame(slot: &Frames, window: &mut gfn_sdl::Window) -> bool {
    let Some(frame) = slot.wait_take(std::time::Duration::from_millis(4)) else { return false };
    window.upload_video(&frame.as_frame()).map_err(|error| eprintln!("video: {error}")).is_ok()
}

/// The TV's picture is on its own hardware plane; nothing to upload.
#[cfg(not(feature = "sim"))]
fn upload_next_frame(_: &Frames, _: &mut gfn_sdl::Window) -> bool {
    false
}

#[cfg(all(feature = "sim", target_os = "macos"))]
fn backend(audio: Option<Arc<AudioOut>>) -> (SinkBackend, Option<Frames>) {
    let slot = Arc::new(gfn_sink::sim::FrameSlot::default());
    let decoder = gfn_sink::vt::decoder_factory(Arc::clone(&slot));
    (SinkBackend::new("sim-videotoolbox", "macos", gfn_sink::vt::hardware_codecs(), decoder, audio), Some(slot))
}

#[cfg(all(feature = "sim", not(target_os = "macos")))]
fn backend(audio: Option<Arc<AudioOut>>) -> (SinkBackend, Option<Frames>) {
    let slot = Arc::new(gfn_sink::sim::FrameSlot::default());
    let decoder = gfn_sink::sim::decoder_factory(Arc::clone(&slot));
    (SinkBackend::new("sim-openh264", "desktop", vec![gfn_sink::MediaVideoCodec::H264], decoder, audio), Some(slot))
}

#[cfg(all(not(feature = "sim"), target_os = "linux", target_arch = "arm"))]
fn backend(audio: Option<Arc<AudioOut>>) -> (SinkBackend, Option<Frames>) {
    let starfish = gfn_sink::starfish::decoder_factory(app_id(), gfn_starfish::Rect::FULL_HD);
    let app = app_id();
    // NDL is an experiment; if it will not open, the stream still plays through Starfish.
    let decoder: Arc<gfn_sink::DecoderFactory> = Arc::new(move |config: &gfn_sink::MediaStreamConfig| {
        if gfn_sink::use_ndl() {
            match gfn_sink::ndl::NdlDecoder::open(&app, config) {
                Ok(decoder) => return Ok(Box::new(decoder) as Box<dyn gfn_sink::VideoDecoder>),
                Err(error) => eprintln!("ndl: {error}; using Starfish"),
            }
        }
        starfish(config)
    });
    // AV1 is claimed on every TV, as the Settings screen warns: older decoders refuse it at Load.
    let codecs = vec![gfn_sink::MediaVideoCodec::H264, gfn_sink::MediaVideoCodec::H265, gfn_sink::MediaVideoCodec::Av1];
    // The C9 panel is HDR10; the player takes HDR from the stream's own VUI and SEI.
    let backend = SinkBackend::new("starfish", "webos", codecs, decoder, audio).with_hdr(vec![gfn_sink::MediaVideoCodec::H265]);
    (backend, None)
}

#[cfg(all(not(feature = "sim"), not(all(target_os = "linux", target_arch = "arm"))))]
fn backend(_: Option<Arc<AudioOut>>) -> (SinkBackend, Option<Frames>) {
    compile_error!("off the TV, build gfn-app with --features sim");
}
