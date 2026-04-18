//! Raw FFI bindings to the libghostty C embedding API (ghostty.h).
//!
//! Only the subset needed for Phase 1 is declared here.

#![allow(non_camel_case_types, dead_code)]

use std::ffi::c_char;
use std::os::raw::{c_int, c_void};

// ── Opaque handles ──────────────────────────────────────────────────

pub type ghostty_app_t = *mut c_void;
pub type ghostty_config_t = *mut c_void;
pub type ghostty_surface_t = *mut c_void;

// ── Platform ────────────────────────────────────────────────────────

pub const GHOSTTY_PLATFORM_INVALID: c_int = 0;
pub const GHOSTTY_PLATFORM_MACOS: c_int = 1;
pub const GHOSTTY_PLATFORM_IOS: c_int = 2;
pub const GHOSTTY_PLATFORM_LINUX: c_int = 3;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ghostty_platform_linux_s {
    pub _reserved: *mut c_void,
}

#[repr(C)]
pub union ghostty_platform_u {
    pub linux: ghostty_platform_linux_s,
    // macOS/iOS variants omitted — not needed on Linux
    _pad: [u8; 8], // ensure union is large enough
}

// ── Text reading ───────────────────────────────────────────────────

pub type ghostty_point_tag_e = c_int;
pub const GHOSTTY_POINT_ACTIVE: ghostty_point_tag_e = 0;
pub const GHOSTTY_POINT_VIEWPORT: ghostty_point_tag_e = 1;
pub const GHOSTTY_POINT_SCREEN: ghostty_point_tag_e = 2;
pub const GHOSTTY_POINT_SURFACE: ghostty_point_tag_e = 3;

pub type ghostty_point_coord_e = c_int;
pub const GHOSTTY_POINT_COORD_EXACT: ghostty_point_coord_e = 0;
pub const GHOSTTY_POINT_COORD_TOP_LEFT: ghostty_point_coord_e = 1;
pub const GHOSTTY_POINT_COORD_BOTTOM_RIGHT: ghostty_point_coord_e = 2;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ghostty_point_s {
    pub tag: ghostty_point_tag_e,
    pub coord: ghostty_point_coord_e,
    pub x: u32,
    pub y: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ghostty_selection_s {
    pub top_left: ghostty_point_s,
    pub bottom_right: ghostty_point_s,
    pub rectangle: bool,
}

#[repr(C)]
pub struct ghostty_text_s {
    pub tl_px_x: f64,
    pub tl_px_y: f64,
    pub offset_start: u32,
    pub offset_len: u32,
    pub text: *const c_char,
    pub text_len: usize,
}

// ── Clipboard ───────────────────────────────────────────────────────

pub const GHOSTTY_CLIPBOARD_STANDARD: c_int = 0;
pub const GHOSTTY_CLIPBOARD_SELECTION: c_int = 1;
pub type ghostty_clipboard_e = c_int;

pub const GHOSTTY_CLIPBOARD_REQUEST_PASTE: c_int = 0;
pub const GHOSTTY_CLIPBOARD_REQUEST_OSC_52_READ: c_int = 1;
pub const GHOSTTY_CLIPBOARD_REQUEST_OSC_52_WRITE: c_int = 2;
pub type ghostty_clipboard_request_e = c_int;

#[repr(C)]
pub struct ghostty_clipboard_content_s {
    pub mime: *const c_char,
    pub data: *const c_char,
}

// ── Input ───────────────────────────────────────────────────────────

pub type ghostty_input_mods_e = c_int;

pub const GHOSTTY_MODS_NONE: ghostty_input_mods_e = 0;
pub const GHOSTTY_MODS_SHIFT: ghostty_input_mods_e = 1 << 0;
pub const GHOSTTY_MODS_CTRL: ghostty_input_mods_e = 1 << 1;
pub const GHOSTTY_MODS_ALT: ghostty_input_mods_e = 1 << 2;
pub const GHOSTTY_MODS_SUPER: ghostty_input_mods_e = 1 << 3;
pub const GHOSTTY_MODS_CAPS: ghostty_input_mods_e = 1 << 4;

pub type ghostty_input_action_e = c_int;
pub const GHOSTTY_ACTION_RELEASE: ghostty_input_action_e = 0;
pub const GHOSTTY_ACTION_PRESS: ghostty_input_action_e = 1;
pub const GHOSTTY_ACTION_REPEAT: ghostty_input_action_e = 2;

pub type ghostty_input_mouse_state_e = c_int;
pub const GHOSTTY_MOUSE_RELEASE: ghostty_input_mouse_state_e = 0;
pub const GHOSTTY_MOUSE_PRESS: ghostty_input_mouse_state_e = 1;

pub type ghostty_input_mouse_button_e = c_int;
pub const GHOSTTY_MOUSE_UNKNOWN: ghostty_input_mouse_button_e = 0;
pub const GHOSTTY_MOUSE_LEFT: ghostty_input_mouse_button_e = 1;
pub const GHOSTTY_MOUSE_RIGHT: ghostty_input_mouse_button_e = 2;
pub const GHOSTTY_MOUSE_MIDDLE: ghostty_input_mouse_button_e = 3;
pub const GHOSTTY_MOUSE_FOUR: ghostty_input_mouse_button_e = 4;
pub const GHOSTTY_MOUSE_FIVE: ghostty_input_mouse_button_e = 5;
pub const GHOSTTY_MOUSE_SIX: ghostty_input_mouse_button_e = 6;
pub const GHOSTTY_MOUSE_SEVEN: ghostty_input_mouse_button_e = 7;
pub const GHOSTTY_MOUSE_EIGHT: ghostty_input_mouse_button_e = 8;

pub type ghostty_input_scroll_mods_t = c_int;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ghostty_input_key_s {
    pub action: ghostty_input_action_e,
    pub mods: ghostty_input_mods_e,
    pub consumed_mods: ghostty_input_mods_e,
    pub keycode: u32,
    pub text: *const c_char,
    pub unshifted_codepoint: u32,
    pub composing: bool,
}

impl Default for ghostty_input_key_s {
    fn default() -> Self {
        Self {
            action: GHOSTTY_ACTION_PRESS,
            mods: GHOSTTY_MODS_NONE,
            consumed_mods: GHOSTTY_MODS_NONE,
            keycode: 0,
            text: std::ptr::null(),
            unshifted_codepoint: 0,
            composing: false,
        }
    }
}

// ── Surface config ──────────────────────────────────────────────────

pub const GHOSTTY_SURFACE_CONTEXT_WINDOW: c_int = 0;

#[repr(C)]
pub struct ghostty_env_var_s {
    pub key: *const c_char,
    pub value: *const c_char,
}

#[repr(C)]
pub struct ghostty_surface_config_s {
    pub platform_tag: c_int,
    pub platform: ghostty_platform_u,
    pub userdata: *mut c_void,
    pub scale_factor: f64,
    pub font_size: f32,
    pub working_directory: *const c_char,
    pub command: *const c_char,
    pub env_vars: *mut ghostty_env_var_s,
    pub env_var_count: usize,
    pub initial_input: *const c_char,
    pub wait_after_command: bool,
    pub context: c_int,
}

// ── Action types (subset for Phase 1) ───────────────────────────────

pub type ghostty_action_tag_e = c_int;
pub const GHOSTTY_ACTION_TAG_NEW_TAB: ghostty_action_tag_e = 2;
pub const GHOSTTY_ACTION_TAG_CLOSE_ALL_WINDOWS: ghostty_action_tag_e = 5;
pub const GHOSTTY_ACTION_TAG_NEW_SPLIT: ghostty_action_tag_e = 4;
pub const GHOSTTY_ACTION_TAG_GOTO_TAB: ghostty_action_tag_e = 15;
pub const GHOSTTY_ACTION_TAG_GOTO_SPLIT: ghostty_action_tag_e = 16;
pub const GHOSTTY_ACTION_TAG_RENDER: ghostty_action_tag_e = 28;
pub const GHOSTTY_ACTION_TAG_SET_TITLE: ghostty_action_tag_e = 33;
pub const GHOSTTY_ACTION_TAG_RING_BELL: ghostty_action_tag_e = 49;

// Split directions
pub type ghostty_action_split_direction_e = c_int;
pub const GHOSTTY_SPLIT_DIRECTION_RIGHT: ghostty_action_split_direction_e = 0;
pub const GHOSTTY_SPLIT_DIRECTION_DOWN: ghostty_action_split_direction_e = 1;
pub const GHOSTTY_SPLIT_DIRECTION_LEFT: ghostty_action_split_direction_e = 2;
pub const GHOSTTY_SPLIT_DIRECTION_UP: ghostty_action_split_direction_e = 3;

// Goto split
pub type ghostty_action_goto_split_e = c_int;
pub const GHOSTTY_GOTO_SPLIT_PREVIOUS: ghostty_action_goto_split_e = 0;
pub const GHOSTTY_GOTO_SPLIT_NEXT: ghostty_action_goto_split_e = 1;

// Goto tab (i32)
pub type ghostty_action_goto_tab_e = c_int;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ghostty_action_set_title_s {
    pub title: *const c_char,
}

// The action union is large and complex. We define fields we need and
// use a byte array to ensure correct total size.
#[repr(C)]
#[derive(Copy, Clone)]
pub union ghostty_action_u {
    pub new_split: ghostty_action_split_direction_e,
    pub goto_tab: ghostty_action_goto_tab_e,
    pub goto_split: ghostty_action_goto_split_e,
    pub set_title: ghostty_action_set_title_s,
    _pad: [u8; 64], // oversized to ensure we don't truncate
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ghostty_action_s {
    pub tag: ghostty_action_tag_e,
    pub action: ghostty_action_u,
}

pub type ghostty_target_tag_e = c_int;

#[repr(C)]
#[derive(Copy, Clone)]
pub union ghostty_target_u {
    pub surface: ghostty_surface_t,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ghostty_target_s {
    pub tag: ghostty_target_tag_e,
    pub target: ghostty_target_u,
}

// ── Runtime config (callbacks) ──────────────────────────────────────

pub type ghostty_runtime_wakeup_cb =
    Option<unsafe extern "C" fn(userdata: *mut c_void)>;

pub type ghostty_runtime_action_cb = Option<
    unsafe extern "C" fn(
        app: ghostty_app_t,
        target: ghostty_target_s,
        action: ghostty_action_s,
    ) -> bool,
>;

pub type ghostty_runtime_read_clipboard_cb = Option<
    unsafe extern "C" fn(
        userdata: *mut c_void,
        location: ghostty_clipboard_e,
        state: *mut c_void,
    ),
>;

pub type ghostty_runtime_confirm_read_clipboard_cb = Option<
    unsafe extern "C" fn(
        userdata: *mut c_void,
        content: *const c_char,
        state: *mut c_void,
        request_type: ghostty_clipboard_request_e,
    ),
>;

pub type ghostty_runtime_write_clipboard_cb = Option<
    unsafe extern "C" fn(
        userdata: *mut c_void,
        location: ghostty_clipboard_e,
        content: *const ghostty_clipboard_content_s,
        content_count: usize,
        should_confirm: bool,
    ),
>;

pub type ghostty_runtime_close_surface_cb =
    Option<unsafe extern "C" fn(userdata: *mut c_void, needs_confirm: bool)>;

#[repr(C)]
pub struct ghostty_runtime_config_s {
    pub userdata: *mut c_void,
    pub supports_selection_clipboard: bool,
    pub wakeup_cb: ghostty_runtime_wakeup_cb,
    pub action_cb: ghostty_runtime_action_cb,
    pub read_clipboard_cb: ghostty_runtime_read_clipboard_cb,
    pub confirm_read_clipboard_cb: ghostty_runtime_confirm_read_clipboard_cb,
    pub write_clipboard_cb: ghostty_runtime_write_clipboard_cb,
    pub close_surface_cb: ghostty_runtime_close_surface_cb,
}

// ── Extern functions ────────────────────────────────────────────────

unsafe extern "C" {
    // Global init — MUST be called before any other ghostty function
    pub fn ghostty_init(argc: usize, argv: *const *const c_char) -> c_int;

    // Config
    pub fn ghostty_config_new() -> ghostty_config_t;
    pub fn ghostty_config_free(config: ghostty_config_t);
    pub fn ghostty_config_load_default_files(config: ghostty_config_t);
    pub fn ghostty_config_finalize(config: ghostty_config_t);

    // Surface config
    pub fn ghostty_surface_config_new() -> ghostty_surface_config_s;

    // App
    pub fn ghostty_app_new(
        runtime: *const ghostty_runtime_config_s,
        config: ghostty_config_t,
    ) -> ghostty_app_t;
    pub fn ghostty_app_free(app: ghostty_app_t);
    pub fn ghostty_app_tick(app: ghostty_app_t);

    // Surface
    pub fn ghostty_surface_new(
        app: ghostty_app_t,
        config: *const ghostty_surface_config_s,
    ) -> ghostty_surface_t;
    pub fn ghostty_surface_free(surface: ghostty_surface_t);
    pub fn ghostty_surface_draw(surface: ghostty_surface_t);
    pub fn ghostty_surface_set_size(surface: ghostty_surface_t, w: u32, h: u32);
    pub fn ghostty_surface_set_content_scale(
        surface: ghostty_surface_t,
        x: f64,
        y: f64,
    );
    pub fn ghostty_surface_set_focus(surface: ghostty_surface_t, focused: bool);
    pub fn ghostty_surface_set_occlusion(surface: ghostty_surface_t, occluded: bool);
    pub fn ghostty_surface_refresh(surface: ghostty_surface_t);
    pub fn ghostty_surface_display_unrealized(surface: ghostty_surface_t);
    pub fn ghostty_surface_display_realized(surface: ghostty_surface_t);
    pub fn ghostty_surface_request_close(surface: ghostty_surface_t);

    // Input
    pub fn ghostty_surface_key(
        surface: ghostty_surface_t,
        event: ghostty_input_key_s,
    ) -> bool;
    pub fn ghostty_surface_text(
        surface: ghostty_surface_t,
        text: *const c_char,
        len: usize,
    );
    pub fn ghostty_surface_preedit(
        surface: ghostty_surface_t,
        text: *const c_char,
        len: usize,
    );
    pub fn ghostty_surface_mouse_button(
        surface: ghostty_surface_t,
        state: ghostty_input_mouse_state_e,
        button: ghostty_input_mouse_button_e,
        mods: ghostty_input_mods_e,
    ) -> bool;
    pub fn ghostty_surface_mouse_pos(
        surface: ghostty_surface_t,
        x: f64,
        y: f64,
        mods: ghostty_input_mods_e,
    );
    pub fn ghostty_surface_mouse_scroll(
        surface: ghostty_surface_t,
        dx: f64,
        dy: f64,
        mods: ghostty_input_scroll_mods_t,
    );

    // Text reading
    pub fn ghostty_surface_read_text(
        surface: ghostty_surface_t,
        selection: ghostty_selection_s,
        result: *mut ghostty_text_s,
    ) -> bool;
    pub fn ghostty_surface_free_text(
        surface: ghostty_surface_t,
        text: *mut ghostty_text_s,
    );

    // Clipboard
    pub fn ghostty_surface_complete_clipboard_request(
        surface: ghostty_surface_t,
        content: *const c_char,
        state: *mut c_void,
        confirmed: bool,
    );
}
