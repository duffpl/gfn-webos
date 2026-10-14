//! `native/starfish.c` verbs. Off-target builds get inert stand-ins so the crate's logic stays
//! host-testable; they report "no video plane", which `StarfishVideo::open` turns into an error.

#[cfg(all(target_os = "linux", target_arch = "arm"))]
unsafe extern "C" {
    pub fn gfn_elog_open(path: *const c_char) -> c_int;
    pub fn sf_load(payload: *const c_char, epoch: c_uint) -> c_int;
    pub fn sf_ready() -> c_int;
    pub fn sf_is_load_completed() -> c_int;
    pub fn sf_play() -> c_int;
    pub fn sf_flush() -> c_int;
    pub fn sf_feed(p: *const u8, size: c_uint, pts: i64, es_data: c_int) -> c_char;
    pub fn sf_unload();
    pub fn sf_callback_gate_retire() -> c_int;
    pub fn sf_destroy() -> c_int;
    pub fn sf_quarantine();
    pub fn vp_mode() -> c_int;
    pub fn vp_create_window() -> *const c_char;
    pub fn vp_place(src_w: c_int, src_h: c_int, dst_x: c_int, dst_y: c_int, dst_w: c_int, dst_h: c_int) -> c_int;
    pub fn vp_destroy_window();
    pub fn acb_create(app_id: *const c_char, player_type: c_int) -> c_long;
    pub fn acb_bind(media_id: *const c_char);
    pub fn acb_send_video_data(source_info: *const c_char) -> c_int;
    pub fn acb_start(x: c_long, y: c_long, w: c_long, h: c_long);
    pub fn acb_unload();
}

#[cfg(not(all(target_os = "linux", target_arch = "arm")))]
mod host {
    use super::*;
    pub unsafe fn gfn_elog_open(_: *const c_char) -> c_int { 1 }
    pub unsafe fn sf_load(_: *const c_char, _: c_uint) -> c_int { 0 }
    pub unsafe fn sf_ready() -> c_int { 0 }
    pub unsafe fn sf_is_load_completed() -> c_int { 0 }
    pub unsafe fn sf_play() -> c_int { 0 }
    pub unsafe fn sf_flush() -> c_int { 0 }
    pub unsafe fn sf_feed(_: *const u8, _: c_uint, _: i64, _: c_int) -> c_char { b'e' as c_char }
    pub unsafe fn sf_unload() {}
    pub unsafe fn sf_callback_gate_retire() -> c_int { 0 }
    pub unsafe fn sf_destroy() -> c_int { 0 }
    pub unsafe fn sf_quarantine() {}
    pub unsafe fn vp_mode() -> c_int { 0 }
    pub unsafe fn vp_create_window() -> *const c_char { std::ptr::null() }
    pub unsafe fn vp_place(_: c_int, _: c_int, _: c_int, _: c_int, _: c_int, _: c_int) -> c_int { 0 }
    pub unsafe fn vp_destroy_window() {}
    pub unsafe fn acb_create(_: *const c_char, _: c_int) -> c_long { 0 }
    pub unsafe fn acb_bind(_: *const c_char) {}
    pub unsafe fn acb_send_video_data(_: *const c_char) -> c_int { -1 }
    pub unsafe fn acb_start(_: c_long, _: c_long, _: c_long, _: c_long) {}
    pub unsafe fn acb_unload() {}
}

#[cfg(not(all(target_os = "linux", target_arch = "arm")))]
pub use host::*;

use std::os::raw::{c_char, c_int, c_long, c_uint};
