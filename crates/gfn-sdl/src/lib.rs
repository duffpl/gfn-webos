//! The SDL2 surface the app needs on the TV and in the desktop simulator.
//!
//! Hand-written declarations rather than a bindings crate: the TV runs LG's SDL 2.0.x fork, and
//! naming only stock SDL 2.0.4-era entry points keeps the binary loadable on every firmware.
//! Window and renderer calls belong to the main thread (macOS enforces it); [`AudioOut`] may be
//! fed from any thread.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::rc::Rc;

pub const INIT_VIDEO: u32 = 0x20;
pub const INIT_AUDIO: u32 = 0x10;
pub const INIT_GAMECONTROLLER: u32 = 0x2000;

const WINDOW_FULLSCREEN: u32 = 0x1;
const WINDOW_RESIZABLE: u32 = 0x20;
const WINDOWPOS_CENTERED: c_int = 0x2FFF_0000;
const RENDERER_ACCELERATED: u32 = 0x2;
const RENDERER_PRESENTVSYNC: u32 = 0x4;
const PIXELFORMAT_IYUV: u32 = 0x5655_5949;
/// `SDL_PIXELFORMAT_ABGR8888`: R, G, B, A bytes in memory order on little-endian targets.
const PIXELFORMAT_RGBA32: u32 = 0x1676_2004;
const TEXTUREACCESS_STATIC: c_int = 0;
const BLENDMODE_BLEND: c_int = 1;
const TEXTUREACCESS_STREAMING: c_int = 1;
const AUDIO_S16LSB: u16 = 0x8010;

const EVENT_QUIT: u32 = 0x100;
const EVENT_WINDOW: u32 = 0x200;
/// SDL_APP_WILLENTERBACKGROUND .. SDL_APP_DIDENTERFOREGROUND. webOS sends them when another app
/// (Home, an input switch) takes the screen and when ours gets it back; window focus events alone
/// also fire for system overlays that leave the app running.
const EVENT_APP_WILL_BACKGROUND: u32 = 0x103;
const EVENT_APP_DID_BACKGROUND: u32 = 0x104;
const EVENT_APP_WILL_FOREGROUND: u32 = 0x105;
const EVENT_APP_DID_FOREGROUND: u32 = 0x106;
const EVENT_KEYDOWN: u32 = 0x300;
const EVENT_KEYUP: u32 = 0x301;
const EVENT_TEXTINPUT: u32 = 0x303;
const EVENT_MOUSEMOTION: u32 = 0x400;
const EVENT_MOUSEBUTTONDOWN: u32 = 0x401;
const EVENT_MOUSEBUTTONUP: u32 = 0x402;
const EVENT_MOUSEWHEEL: u32 = 0x403;
const EVENT_CONTROLLERAXISMOTION: u32 = 0x650;
const EVENT_CONTROLLERBUTTONDOWN: u32 = 0x651;
const EVENT_CONTROLLERBUTTONUP: u32 = 0x652;
const EVENT_CONTROLLERDEVICEADDED: u32 = 0x653;
const EVENT_CONTROLLERDEVICEREMOVED: u32 = 0x654;
const WINDOWEVENT_FOCUS_GAINED: u8 = 12;
const WINDOWEVENT_FOCUS_LOST: u8 = 13;
/// Mouse events SDL synthesises from touch input carry this device id.
const TOUCH_MOUSEID: u32 = u32::MAX;
const WINDOW_INPUT_FOCUS: u32 = 0x200;
const LOG_CATEGORY_INPUT: c_int = 3;
const LOG_PRIORITY_DEBUG: c_int = 2;

/// LG's SDL fork inserts a `Uint32 inputSource` at +12 in keyboard and text events, so everything
/// after it sits four bytes later than the stock headers say (plx-native docs/remote-keys.md §1,
/// device-verified on webOS 4.5).
const LG_FORK: bool = cfg!(all(target_os = "linux", target_arch = "arm"));
const MOUSEWHEEL_FLIPPED: u32 = 1;

#[repr(C)]
struct AudioSpec {
    freq: c_int,
    format: u16,
    channels: u8,
    silence: u8,
    samples: u16,
    padding: u16,
    size: u32,
    callback: Option<extern "C" fn(*mut c_void, *mut u8, c_int)>,
    userdata: *mut c_void,
}

#[repr(C)]
struct Rect {
    x: c_int,
    y: c_int,
    w: c_int,
    h: c_int,
}

unsafe extern "C" {
    fn SDL_SetMainReady();
    fn SDL_Init(flags: u32) -> c_int;
    fn SDL_Quit();
    fn SDL_GetError() -> *const c_char;
    fn SDL_CreateWindow(title: *const c_char, x: c_int, y: c_int, w: c_int, h: c_int, flags: u32) -> *mut c_void;
    fn SDL_DestroyWindow(window: *mut c_void);
    fn SDL_CreateRenderer(window: *mut c_void, index: c_int, flags: u32) -> *mut c_void;
    fn SDL_DestroyRenderer(renderer: *mut c_void);
    fn SDL_GetRendererOutputSize(renderer: *mut c_void, w: *mut c_int, h: *mut c_int) -> c_int;
    fn SDL_SetRenderDrawColor(renderer: *mut c_void, r: u8, g: u8, b: u8, a: u8) -> c_int;
    fn SDL_RenderClear(renderer: *mut c_void) -> c_int;
    fn SDL_RenderCopy(renderer: *mut c_void, texture: *mut c_void, src: *const Rect, dst: *const Rect) -> c_int;
    fn SDL_RenderPresent(renderer: *mut c_void);
    fn SDL_RenderFillRect(renderer: *mut c_void, rect: *const Rect) -> c_int;
    fn SDL_SetRenderDrawBlendMode(renderer: *mut c_void, mode: c_int) -> c_int;
    fn SDL_SetTextureBlendMode(texture: *mut c_void, mode: c_int) -> c_int;
    fn SDL_SetTextureColorMod(texture: *mut c_void, r: u8, g: u8, b: u8) -> c_int;
    fn SDL_SetTextureAlphaMod(texture: *mut c_void, a: u8) -> c_int;
    fn SDL_UpdateTexture(texture: *mut c_void, rect: *const Rect, pixels: *const c_void, pitch: c_int) -> c_int;
    fn SDL_CreateTexture(renderer: *mut c_void, format: u32, access: c_int, w: c_int, h: c_int) -> *mut c_void;
    fn SDL_DestroyTexture(texture: *mut c_void);
    fn SDL_UpdateYUVTexture(
        texture: *mut c_void,
        rect: *const Rect,
        y: *const u8,
        y_pitch: c_int,
        u: *const u8,
        u_pitch: c_int,
        v: *const u8,
        v_pitch: c_int,
    ) -> c_int;
    fn SDL_PollEvent(event: *mut Event) -> c_int;
    fn SDL_PushEvent(event: *mut Event) -> c_int;
    fn SDL_RegisterEvents(count: c_int) -> u32;
    fn SDL_WaitEventTimeout(event: *mut Event, timeout: c_int) -> c_int;
    fn SDL_RenderReadPixels(renderer: *mut c_void, rect: *const Rect, format: u32, pixels: *mut c_void, pitch: c_int) -> c_int;
    fn SDL_RenderSetClipRect(renderer: *mut c_void, rect: *const Rect) -> c_int;
    fn SDL_GetWindowSize(window: *mut c_void, w: *mut c_int, h: *mut c_int);
    fn SDL_SetWindowGrab(window: *mut c_void, grabbed: c_int);
    fn SDL_SetRelativeMouseMode(enabled: c_int) -> c_int;
    fn SDL_ShowCursor(toggle: c_int) -> c_int;
    fn SDL_StartTextInput();
    fn SDL_StopTextInput();
    fn SDL_HasScreenKeyboardSupport() -> c_int;
    fn SDL_IsScreenKeyboardShown(window: *mut c_void) -> c_int;
    fn SDL_GetWindowFlags(window: *mut c_void) -> u32;
    fn SDL_LogSetPriority(category: c_int, priority: c_int);
    fn SDL_CreateSystemCursor(id: c_int) -> *mut c_void;
    fn SDL_SetCursor(cursor: *mut c_void);
    fn SDL_FreeCursor(cursor: *mut c_void);
    fn SDL_IsGameController(joystick_index: c_int) -> c_int;
    fn SDL_GameControllerOpen(joystick_index: c_int) -> *mut c_void;
    fn SDL_GameControllerClose(controller: *mut c_void);
    fn SDL_GameControllerGetJoystick(controller: *mut c_void) -> *mut c_void;
    fn SDL_JoystickInstanceID(joystick: *mut c_void) -> i32;
    fn SDL_OpenAudioDevice(
        device: *const c_char,
        iscapture: c_int,
        desired: *const AudioSpec,
        obtained: *mut AudioSpec,
        allowed_changes: c_int,
    ) -> u32;
    fn SDL_CloseAudioDevice(device: u32);
    fn SDL_PauseAudioDevice(device: u32, pause_on: c_int);
    fn SDL_QueueAudio(device: u32, data: *const c_void, len: u32) -> c_int;
    fn SDL_GetQueuedAudioSize(device: u32) -> u32;
    fn SDL_ClearQueuedAudio(device: u32);
}

pub fn error() -> String {
    unsafe { CStr::from_ptr(SDL_GetError()) }.to_string_lossy().into_owned()
}

/// Owns `SDL_Init`/`SDL_Quit`. Create one on the main thread before any window or audio device.
pub struct Sdl(());

impl Sdl {
    pub fn init(flags: u32) -> Result<Self, String> {
        unsafe {
            SDL_SetMainReady();
            if SDL_Init(flags) != 0 {
                return Err(format!("SDL_Init: {}", error()));
            }
        }
        Ok(Self(()))
    }
}

impl Drop for Sdl {
    fn drop(&mut self) {
        unsafe { SDL_Quit() };
    }
}

/// One decoded I420 picture with tightly or loosely packed planes.
pub struct YuvFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub y_stride: usize,
    pub uv_stride: usize,
}

/// Window and renderer share one owner so textures can keep the renderer alive: SDL frees a
/// renderer's textures with it, and the renderer must go before its window.
struct Surface {
    window: *mut c_void,
    renderer: *mut c_void,
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SDL_DestroyRenderer(self.renderer);
            SDL_DestroyWindow(self.window);
        }
    }
}

/// A rectangle in output pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Area {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    fn raw(self) -> Rect {
        Rect { x: self.x, y: self.y, w: self.w, h: self.h }
    }
}

/// An RGBA texture drawn with a colour and alpha tint (white glyph masks take any text colour).
pub struct Texture {
    /// Held only to keep the renderer alive until this texture is destroyed.
    _surface: Rc<Surface>,
    texture: *mut c_void,
    pub width: u32,
    pub height: u32,
}

impl Drop for Texture {
    fn drop(&mut self) {
        unsafe { SDL_DestroyTexture(self.texture) };
    }
}

pub struct Window {
    surface: Rc<Surface>,
    video: *mut c_void,
    video_size: (u32, u32),
}

impl Window {
    /// `vsync` makes each present wait for the display refresh: even frame pacing for video the
    /// window draws itself, at the cost of up to one refresh of latency.
    pub fn open(_sdl: &Sdl, title: &str, width: u32, height: u32, fullscreen: bool, vsync: bool) -> Result<Self, String> {
        let title = CString::new(title).map_err(|_| "title contains NUL")?;
        let flags = if fullscreen { WINDOW_FULLSCREEN } else { WINDOW_RESIZABLE };
        unsafe {
            let window = SDL_CreateWindow(
                title.as_ptr(),
                WINDOWPOS_CENTERED,
                WINDOWPOS_CENTERED,
                width as c_int,
                height as c_int,
                flags,
            );
            if window.is_null() {
                return Err(format!("SDL_CreateWindow: {}", error()));
            }
            let flags = if vsync { RENDERER_ACCELERATED | RENDERER_PRESENTVSYNC } else { RENDERER_ACCELERATED };
            let renderer = SDL_CreateRenderer(window, -1, flags);
            if renderer.is_null() {
                SDL_DestroyWindow(window);
                return Err(format!("SDL_CreateRenderer: {}", error()));
            }
            SDL_SetRenderDrawBlendMode(renderer, BLENDMODE_BLEND);
            Ok(Self { surface: Rc::new(Surface { window, renderer }), video: std::ptr::null_mut(), video_size: (0, 0) })
        }
    }

    fn renderer(&self) -> *mut c_void {
        self.surface.renderer
    }

    /// The TV composites its hardware video plane beneath the graphics plane, so anything not
    /// drawn must stay fully transparent for the picture to show through.
    pub fn present_transparent(&mut self) {
        self.clear([0, 0, 0, 0]);
        self.present();
    }

    /// The TV's keyboard rises only while this window has keyboard focus, and fails silently
    /// otherwise.
    pub fn has_input_focus(&self) -> bool {
        unsafe { SDL_GetWindowFlags(self.surface.window) & WINDOW_INPUT_FOCUS != 0 }
    }

    pub fn screen_keyboard_shown(&self) -> bool {
        unsafe { SDL_IsScreenKeyboardShown(self.surface.window) != 0 }
    }

    /// Size in window coordinates, the space mouse positions are reported in (not HiDPI pixels).
    pub fn size(&self) -> (u32, u32) {
        let (mut w, mut h) = (0, 0);
        unsafe { SDL_GetWindowSize(self.surface.window, &mut w, &mut h) };
        (w.max(0) as u32, h.max(0) as u32)
    }

    /// Size in drawing pixels (larger than [`Window::size`] on HiDPI displays).
    pub fn output_size(&self) -> (u32, u32) {
        let (mut w, mut h) = (0, 0);
        unsafe { SDL_GetRendererOutputSize(self.renderer(), &mut w, &mut h) };
        (w.max(0) as u32, h.max(0) as u32)
    }

    /// Relative mode hides the pointer and reports raw motion; the grab keeps it in the window.
    pub fn set_relative_mouse(&mut self, enabled: bool) -> Result<(), String> {
        unsafe {
            SDL_SetWindowGrab(self.surface.window, c_int::from(enabled));
            if SDL_SetRelativeMouseMode(c_int::from(enabled)) != 0 {
                return Err(format!("SDL_SetRelativeMouseMode: {}", error()));
            }
        }
        Ok(())
    }

    /// Replaces the whole target, alpha included (no blending).
    pub fn clear(&mut self, rgba: [u8; 4]) {
        unsafe {
            SDL_SetRenderDrawColor(self.renderer(), rgba[0], rgba[1], rgba[2], rgba[3]);
            SDL_RenderClear(self.renderer());
        }
    }

    pub fn fill(&mut self, area: Area, rgba: [u8; 4]) {
        unsafe {
            SDL_SetRenderDrawColor(self.renderer(), rgba[0], rgba[1], rgba[2], rgba[3]);
            SDL_RenderFillRect(self.renderer(), &area.raw());
        }
    }

    /// `pixels` is tightly packed RGBA, `width * height * 4` bytes, straight (not premultiplied) alpha.
    pub fn texture(&mut self, width: u32, height: u32, pixels: &[u8]) -> Result<Texture, String> {
        if pixels.len() != width as usize * height as usize * 4 || width == 0 || height == 0 {
            return Err(format!("texture: {} bytes for {width}x{height}", pixels.len()));
        }
        unsafe {
            let texture =
                SDL_CreateTexture(self.renderer(), PIXELFORMAT_RGBA32, TEXTUREACCESS_STATIC, width as c_int, height as c_int);
            if texture.is_null() {
                return Err(format!("SDL_CreateTexture: {}", error()));
            }
            let texture = Texture { _surface: Rc::clone(&self.surface), texture, width, height };
            if SDL_UpdateTexture(texture.texture, std::ptr::null(), pixels.as_ptr().cast(), (width * 4) as c_int) != 0 {
                return Err(format!("SDL_UpdateTexture: {}", error()));
            }
            SDL_SetTextureBlendMode(texture.texture, BLENDMODE_BLEND);
            Ok(texture)
        }
    }

    /// Draws `source` (texture pixels, `None` = all) of `texture` into `dest`, multiplied by `tint`.
    pub fn draw(&mut self, texture: &Texture, source: Option<Area>, dest: Area, tint: [u8; 4]) {
        let source = source.map(Area::raw);
        unsafe {
            SDL_SetTextureColorMod(texture.texture, tint[0], tint[1], tint[2]);
            SDL_SetTextureAlphaMod(texture.texture, tint[3]);
            SDL_RenderCopy(
                self.renderer(),
                texture.texture,
                source.as_ref().map_or(std::ptr::null(), |r| r as *const Rect),
                &dest.raw(),
            );
        }
    }

    /// Uploads `frame` as the current video picture (simulator video path).
    pub fn upload_video(&mut self, frame: &YuvFrame<'_>) -> Result<(), String> {
        unsafe {
            if self.video.is_null() || self.video_size != (frame.width, frame.height) {
                if !self.video.is_null() {
                    SDL_DestroyTexture(self.video);
                }
                self.video = SDL_CreateTexture(
                    self.renderer(),
                    PIXELFORMAT_IYUV,
                    TEXTUREACCESS_STREAMING,
                    frame.width as c_int,
                    frame.height as c_int,
                );
                if self.video.is_null() {
                    self.video_size = (0, 0);
                    return Err(format!("SDL_CreateTexture: {}", error()));
                }
                self.video_size = (frame.width, frame.height);
            }
            if SDL_UpdateYUVTexture(
                self.video,
                std::ptr::null(),
                frame.y.as_ptr(),
                frame.y_stride as c_int,
                frame.u.as_ptr(),
                frame.uv_stride as c_int,
                frame.v.as_ptr(),
                frame.uv_stride as c_int,
            ) != 0
            {
                return Err(format!("SDL_UpdateYUVTexture: {}", error()));
            }
        }
        Ok(())
    }

    /// Clears to black and letterboxes the last uploaded video picture, if any.
    pub fn draw_video(&mut self) {
        self.clear([0, 0, 0, 255]);
        if self.video.is_null() {
            return;
        }
        let (out_w, out_h) = self.output_size();
        let dst = letterbox(self.video_size.0, self.video_size.1, out_w as c_int, out_h as c_int);
        unsafe { SDL_RenderCopy(self.renderer(), self.video, std::ptr::null(), &dst) };
    }

    /// Forgets the last video picture, so a new stream never flashes the previous one.
    pub fn drop_video(&mut self) {
        if !self.video.is_null() {
            unsafe { SDL_DestroyTexture(self.video) };
            self.video = std::ptr::null_mut();
            self.video_size = (0, 0);
        }
    }

    /// The frame drawn so far (call before [`Window::present`]) as tightly packed RGBA.
    pub fn read_pixels(&mut self) -> Result<(u32, u32, Vec<u8>), String> {
        let (w, h) = self.output_size();
        let mut pixels = vec![0u8; w as usize * h as usize * 4];
        let status = unsafe {
            SDL_RenderReadPixels(self.renderer(), std::ptr::null(), PIXELFORMAT_RGBA32, pixels.as_mut_ptr().cast(), (w * 4) as c_int)
        };
        if status != 0 {
            return Err(format!("SDL_RenderReadPixels: {}", error()));
        }
        Ok((w, h, pixels))
    }

    /// Restricts drawing to `area` (output pixels); `None` lifts it.
    pub fn clip(&mut self, area: Option<Area>) {
        let rect = area.map(Area::raw);
        unsafe { SDL_RenderSetClipRect(self.renderer(), rect.as_ref().map_or(std::ptr::null(), |r| r as *const Rect)) };
    }

    pub fn present(&mut self) {
        unsafe { SDL_RenderPresent(self.renderer()) };
    }

    /// Letterboxes `frame` into the window and presents it.
    pub fn present_yuv(&mut self, frame: &YuvFrame<'_>) -> Result<(), String> {
        self.upload_video(frame)?;
        self.draw_video();
        self.present();
        Ok(())
    }
}

/// The rectangle `frame` occupies inside an `out_w`×`out_h` area.
pub fn letterbox_rect(src_w: u32, src_h: u32, out_w: u32, out_h: u32) -> (i32, i32, u32, u32) {
    let r = letterbox(src_w, src_h, out_w as c_int, out_h as c_int);
    (r.x, r.y, r.w.max(0) as u32, r.h.max(0) as u32)
}

fn letterbox(src_w: u32, src_h: u32, out_w: c_int, out_h: c_int) -> Rect {
    if src_w == 0 || src_h == 0 || out_w <= 0 || out_h <= 0 {
        return Rect { x: 0, y: 0, w: out_w, h: out_h };
    }
    let scale = (out_w as f64 / src_w as f64).min(out_h as f64 / src_h as f64);
    let (w, h) = ((src_w as f64 * scale) as c_int, (src_h as f64 * scale) as c_int);
    Rect { x: (out_w - w) / 2, y: (out_h - h) / 2, w, h }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.drop_video();
    }
}

/// `SDL_Event` is a 56-byte union; only the fields read below are interpreted.
#[repr(C, align(8))]
pub struct Event([u8; 56]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    Quit,
    FocusGained,
    FocusLost,
    /// Another app has the screen; both the will- and did- edges arrive, in no promised order.
    Background,
    Foreground,
    Key { scancode: i32, keycode: i32, modifiers: u16, pressed: bool, repeat: bool },
    /// Committed text: the TV's on-screen keyboard, or a desktop keyboard in the simulator.
    Text(TextChunk),
    MouseMotion { x: i32, y: i32, xrel: i32, yrel: i32 },
    /// SDL numbering: 1 left, 2 middle, 3 right, 4 X1, 5 X2.
    MouseButton { button: u8, pressed: bool },
    MouseWheel { x: i32, y: i32, flipped: bool },
    /// `device_index` is only valid for [`GameController::open`]; later events carry the instance id.
    ControllerAdded { device_index: i32 },
    ControllerRemoved { instance_id: i32 },
    /// `SDL_GameControllerButton` numbering.
    ControllerButton { instance_id: i32, button: u8, pressed: bool },
    /// `SDL_GameControllerAxis` numbering.
    ControllerAxis { instance_id: i32, axis: u8, value: i16 },
}

/// `SDL_TextInputEvent`'s inline UTF-8, at most 31 bytes per event.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TextChunk {
    bytes: [u8; 32],
    len: u8,
}

impl TextChunk {
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or_default()
    }
}

impl std::fmt::Debug for TextChunk {
    // Typed text can be a password.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TextChunk({} bytes)", self.len)
    }
}

/// The next queued event: `None` when the queue is empty, `Some(None)` for one the app ignores.
pub fn poll_event() -> Option<Option<InputEvent>> {
    let mut raw = Event([0; 56]);
    if unsafe { SDL_PollEvent(&mut raw) } == 0 {
        return None;
    }
    Some(decode(&raw))
}

/// Like [`poll_event`], but sleeps up to `timeout_ms` for an event to arrive.
pub fn wait_event(timeout_ms: u32) -> Option<Option<InputEvent>> {
    let mut raw = Event([0; 56]);
    if unsafe { SDL_WaitEventTimeout(&mut raw, timeout_ms.min(i32::MAX as u32) as c_int) } == 0 {
        return None;
    }
    Some(decode(&raw))
}

/// Wakes [`wait_event`] from another thread; the event it posts decodes as one the app ignores.
#[derive(Clone, Copy)]
pub struct Waker(u32);

impl Waker {
    pub fn new(_sdl: &Sdl) -> Self {
        Self(unsafe { SDL_RegisterEvents(1) })
    }

    pub fn wake(&self) {
        let mut raw = Event([0; 56]);
        raw.0[..4].copy_from_slice(&self.0.to_ne_bytes());
        unsafe { SDL_PushEvent(&mut raw) };
    }
}

fn decode(raw: &Event) -> Option<InputEvent> {
    let b = &raw.0;
    let kind = u32::from_ne_bytes([b[0], b[1], b[2], b[3]]);
    let i32_at = |o: usize| i32::from_ne_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let u32_at = |o: usize| u32::from_ne_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let i16_at = |o: usize| i16::from_ne_bytes([b[o], b[o + 1]]);
    match kind {
        EVENT_QUIT => Some(InputEvent::Quit),
        EVENT_APP_WILL_BACKGROUND | EVENT_APP_DID_BACKGROUND => Some(InputEvent::Background),
        EVENT_APP_WILL_FOREGROUND | EVENT_APP_DID_FOREGROUND => Some(InputEvent::Foreground),
        EVENT_WINDOW => match b[12] {
            WINDOWEVENT_FOCUS_GAINED => Some(InputEvent::FocusGained),
            WINDOWEVENT_FOCUS_LOST => Some(InputEvent::FocusLost),
            _ => None,
        },
        EVENT_MOUSEMOTION if u32_at(12) != TOUCH_MOUSEID => {
            Some(InputEvent::MouseMotion { x: i32_at(20), y: i32_at(24), xrel: i32_at(28), yrel: i32_at(32) })
        }
        EVENT_MOUSEBUTTONDOWN | EVENT_MOUSEBUTTONUP if u32_at(12) != TOUCH_MOUSEID => {
            Some(InputEvent::MouseButton { button: b[16], pressed: kind == EVENT_MOUSEBUTTONDOWN })
        }
        EVENT_MOUSEWHEEL if u32_at(12) != TOUCH_MOUSEID => {
            Some(InputEvent::MouseWheel { x: i32_at(16), y: i32_at(20), flipped: u32_at(24) == MOUSEWHEEL_FLIPPED })
        }
        EVENT_CONTROLLERDEVICEADDED => Some(InputEvent::ControllerAdded { device_index: i32_at(8) }),
        EVENT_CONTROLLERDEVICEREMOVED => Some(InputEvent::ControllerRemoved { instance_id: i32_at(8) }),
        EVENT_CONTROLLERBUTTONDOWN | EVENT_CONTROLLERBUTTONUP => Some(InputEvent::ControllerButton {
            instance_id: i32_at(8),
            button: b[12],
            pressed: kind == EVENT_CONTROLLERBUTTONDOWN,
        }),
        EVENT_CONTROLLERAXISMOTION => {
            Some(InputEvent::ControllerAxis { instance_id: i32_at(8), axis: b[12], value: i16_at(16) })
        }
        EVENT_KEYDOWN | EVENT_KEYUP if LG_FORK => Some(InputEvent::Key {
            // The fork packs state and repeat into the u32 at +16: low byte pressed, bit 0x100 repeat.
            scancode: i32_at(20),
            keycode: i32_at(24),
            modifiers: u16::from_ne_bytes([b[28], b[29]]),
            pressed: kind == EVENT_KEYDOWN,
            repeat: b[17] & 1 != 0,
        }),
        EVENT_KEYDOWN | EVENT_KEYUP => Some(InputEvent::Key {
            scancode: i32_at(16),
            keycode: i32_at(20),
            modifiers: u16::from_ne_bytes([b[24], b[25]]),
            pressed: kind == EVENT_KEYDOWN,
            repeat: b[13] != 0,
        }),
        EVENT_TEXTINPUT => {
            let start = if LG_FORK { 16 } else { 12 };
            let mut bytes = [0u8; 32];
            let field = &b[start..(start + 32).min(b.len())];
            let len = field.iter().position(|&c| c == 0).unwrap_or(field.len());
            bytes[..len].copy_from_slice(&field[..len]);
            Some(InputEvent::Text(TextChunk { bytes, len: len as u8 }))
        }
        _ => None,
    }
}

/// Raises the TV's own on-screen keyboard; typed text then arrives as [`InputEvent::Text`]. On a
/// desktop it only turns text events on.
pub fn start_text_input() {
    unsafe {
        // LG's driver narrates the keyboard's lifecycle at INPUT/DEBUG, which is below the default.
        SDL_LogSetPriority(LOG_CATEGORY_INPUT, LOG_PRIORITY_DEBUG);
        SDL_StartTextInput();
    }
}

pub fn stop_text_input() {
    unsafe { SDL_StopTextInput() };
}

pub fn has_screen_keyboard() -> bool {
    unsafe { SDL_HasScreenKeyboardSupport() != 0 }
}

pub fn show_cursor(visible: bool) {
    unsafe { SDL_ShowCursor(c_int::from(visible)) };
}

/// `SDL_SystemCursor` shapes, created on first use and kept for the process lifetime.
pub struct SystemCursors {
    cursors: [*mut c_void; 12],
}

impl SystemCursors {
    pub const ARROW: u8 = 0;
    pub const IBEAM: u8 = 1;
    pub const WAIT: u8 = 2;
    pub const CROSSHAIR: u8 = 3;
    pub const WAITARROW: u8 = 4;
    pub const SIZENWSE: u8 = 5;
    pub const SIZENESW: u8 = 6;
    pub const SIZEWE: u8 = 7;
    pub const SIZENS: u8 = 8;
    pub const SIZEALL: u8 = 9;
    pub const HAND: u8 = 11;

    pub fn new(_sdl: &Sdl) -> Self {
        Self { cursors: [std::ptr::null_mut(); 12] }
    }

    pub fn set(&mut self, shape: u8) {
        let Some(slot) = self.cursors.get_mut(usize::from(shape)) else { return };
        unsafe {
            if slot.is_null() {
                *slot = SDL_CreateSystemCursor(c_int::from(shape));
            }
            if !slot.is_null() {
                SDL_SetCursor(*slot);
            }
        }
    }
}

impl Drop for SystemCursors {
    fn drop(&mut self) {
        for cursor in self.cursors.into_iter().filter(|c| !c.is_null()) {
            unsafe { SDL_FreeCursor(cursor) };
        }
    }
}

pub struct GameController {
    controller: *mut c_void,
    instance_id: i32,
}

impl GameController {
    /// `None` when the joystick has no game-controller mapping or cannot be opened.
    pub fn open(_sdl: &Sdl, device_index: i32) -> Option<Self> {
        unsafe {
            if SDL_IsGameController(device_index) == 0 {
                return None;
            }
            let controller = SDL_GameControllerOpen(device_index);
            if controller.is_null() {
                return None;
            }
            let instance_id = SDL_JoystickInstanceID(SDL_GameControllerGetJoystick(controller));
            Some(Self { controller, instance_id })
        }
    }

    pub fn instance_id(&self) -> i32 {
        self.instance_id
    }
}

impl Drop for GameController {
    fn drop(&mut self) {
        unsafe { SDL_GameControllerClose(self.controller) };
    }
}

/// Push-mode audio output: interleaved S16 PCM queued from any thread.
pub struct AudioOut {
    device: u32,
    pub sample_rate: u32,
    pub channels: u8,
}

// SDL_QueueAudio/SDL_GetQueuedAudioSize lock the device internally.
unsafe impl Send for AudioOut {}
unsafe impl Sync for AudioOut {}

impl AudioOut {
    pub fn open(_sdl: &Sdl, sample_rate: u32, channels: u8, period_samples: u16) -> Result<Self, String> {
        let desired = AudioSpec {
            freq: sample_rate as c_int,
            format: AUDIO_S16LSB,
            channels,
            silence: 0,
            samples: period_samples,
            padding: 0,
            size: 0,
            callback: None,
            userdata: std::ptr::null_mut(),
        };
        let mut obtained = AudioSpec { ..desired };
        let device = unsafe { SDL_OpenAudioDevice(std::ptr::null(), 0, &desired, &mut obtained, 0) };
        if device == 0 {
            return Err(format!("SDL_OpenAudioDevice: {}", error()));
        }
        unsafe { SDL_PauseAudioDevice(device, 0) };
        Ok(Self { device, sample_rate, channels })
    }

    pub fn queue(&self, samples: &[i16]) -> Result<(), String> {
        let bytes = std::mem::size_of_val(samples) as u32;
        if unsafe { SDL_QueueAudio(self.device, samples.as_ptr().cast(), bytes) } != 0 {
            return Err(format!("SDL_QueueAudio: {}", error()));
        }
        Ok(())
    }

    pub fn queued_ms(&self) -> u32 {
        let bytes = unsafe { SDL_GetQueuedAudioSize(self.device) };
        let frame_bytes = 2 * u32::from(self.channels.max(1));
        bytes / frame_bytes * 1000 / self.sample_rate.max(1)
    }

    pub fn clear(&self) {
        unsafe { SDL_ClearQueuedAudio(self.device) };
    }
}

impl Drop for AudioOut {
    fn drop(&mut self) {
        unsafe { SDL_CloseAudioDevice(self.device) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letterbox_centers_and_preserves_aspect() {
        let r = letterbox(1920, 1080, 1000, 1000);
        assert_eq!((r.x, r.y, r.w, r.h), (0, 219, 1000, 562));
    }

    #[test]
    fn event_buffer_matches_sdl_union_size() {
        assert_eq!(std::mem::size_of::<Event>(), 56);
    }
}
