use std::ffi::CStr;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use gtk4::glib;

use crate::clipboard;
use crate::ghostty_sys::*;

/// Global app state, accessible from C callbacks.
/// Initialized once in `GhosttyApp::new()`.
static APP: OnceLock<AppInner> = OnceLock::new();

struct AppInner {
    ghostty_app: ghostty_app_t,
    config: ghostty_config_t,
    tick_pending: AtomicBool,
}

// SAFETY: The ghostty opaque pointers are only accessed from the main thread
// (via glib::idle_add). The AtomicBool is inherently Send+Sync.
unsafe impl Send for AppInner {}
unsafe impl Sync for AppInner {}

/// Owns the Ghostty app lifecycle.
pub struct GhosttyApp;

impl GhosttyApp {
    /// Initialize the Ghostty backend. Must be called once from the main thread.
    pub fn init() -> Result<(), &'static str> {
        // Initialize Ghostty global state (allocator, etc.)
        let args: Vec<std::ffi::CString> = std::env::args()
            .map(|a| std::ffi::CString::new(a).unwrap())
            .collect();
        let argv: Vec<*const std::ffi::c_char> = args.iter().map(|a| a.as_ptr()).collect();
        let ret = unsafe { ghostty_init(argv.len(), argv.as_ptr()) };
        if ret != 0 {
            return Err("ghostty_init failed");
        }

        // Load config
        let config = unsafe { ghostty_config_new() };
        if config.is_null() {
            return Err("ghostty_config_new returned null");
        }
        unsafe {
            ghostty_config_load_default_files(config);
            ghostty_config_finalize(config);
        }

        // Build runtime callbacks
        let runtime = ghostty_runtime_config_s {
            userdata: std::ptr::null_mut(),
            supports_selection_clipboard: true,
            wakeup_cb: Some(wakeup_cb),
            action_cb: Some(action_cb),
            read_clipboard_cb: Some(clipboard::read_clipboard_cb),
            confirm_read_clipboard_cb: Some(clipboard::confirm_read_clipboard_cb),
            write_clipboard_cb: Some(clipboard::write_clipboard_cb),
            close_surface_cb: Some(close_surface_cb),
        };

        let ghostty_app = unsafe { ghostty_app_new(&runtime, config) };
        if ghostty_app.is_null() {
            unsafe { ghostty_config_free(config) };
            return Err("ghostty_app_new returned null");
        }

        APP.set(AppInner {
            ghostty_app,
            config,
            tick_pending: AtomicBool::new(false),
        })
        .map_err(|_| "GhosttyApp already initialized")?;

        Ok(())
    }

    /// Get the raw ghostty_app_t handle.
    pub fn handle() -> ghostty_app_t {
        APP.get().expect("GhosttyApp not initialized").ghostty_app
    }

    /// Clean up. Called on app shutdown.
    pub fn destroy() {
        if let Some(inner) = APP.get() {
            unsafe {
                ghostty_app_free(inner.ghostty_app);
                ghostty_config_free(inner.config);
            }
        }
    }
}

// ── C callback trampolines ──────────────────────────────────────────

unsafe extern "C" fn wakeup_cb(_userdata: *mut c_void) {
    let Some(inner) = APP.get() else { return };

    // Coalesce: only schedule one idle callback at a time
    if inner
        .tick_pending
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        glib::idle_add_once(tick);
    }
}

fn tick() {
    let Some(inner) = APP.get() else { return };
    inner.tick_pending.store(false, Ordering::Release);

    unsafe {
        ghostty_app_tick(inner.ghostty_app);
    }

    // Request a redraw of the surface after ticking
    crate::surface::queue_render();
}

unsafe extern "C" fn action_cb(
    _app: ghostty_app_t,
    _target: ghostty_target_s,
    action: ghostty_action_s,
) -> bool {
    match action.tag {
        GHOSTTY_ACTION_TAG_RENDER => {
            crate::surface::queue_render();
            true
        }
        GHOSTTY_ACTION_TAG_SET_TITLE => {
            let title_ptr = unsafe { action.action.set_title.title };
            if !title_ptr.is_null() {
                let title = unsafe { CStr::from_ptr(title_ptr) }
                    .to_str()
                    .unwrap_or("limux");
                // Update the tab title for the surface that sent this action
                let surface = unsafe { _target.target.surface };
                if !surface.is_null() {
                    crate::window::set_surface_title(surface, title);
                }
                crate::set_window_title(title);
            }
            true
        }
        GHOSTTY_ACTION_TAG_NEW_TAB => {
            crate::window::new_workspace();
            true
        }
        GHOSTTY_ACTION_TAG_CLOSE_ALL_WINDOWS => {
            crate::close_window();
            true
        }
        GHOSTTY_ACTION_TAG_NEW_SPLIT => {
            let dir = unsafe { action.action.new_split };
            let orientation = match dir {
                GHOSTTY_SPLIT_DIRECTION_RIGHT | GHOSTTY_SPLIT_DIRECTION_LEFT => {
                    crate::split::Orientation::Horizontal
                }
                _ => crate::split::Orientation::Vertical,
            };
            crate::window::split_focused(orientation);
            true
        }
        GHOSTTY_ACTION_TAG_GOTO_SPLIT => {
            let goto = unsafe { action.action.goto_split };
            let forward = goto != GHOSTTY_GOTO_SPLIT_PREVIOUS;
            crate::window::navigate_split(forward);
            true
        }
        GHOSTTY_ACTION_TAG_GOTO_TAB => {
            let tab = unsafe { action.action.goto_tab };
            crate::window::goto_tab(tab);
            true
        }
        GHOSTTY_ACTION_TAG_RING_BELL => {
            let surface = unsafe { _target.target.surface };
            if !surface.is_null() {
                crate::window::handle_bell(surface);
            }
            true
        }
        _ => false,
    }
}

unsafe extern "C" fn close_surface_cb(userdata: *mut c_void, _needs_confirm: bool) {
    // userdata is a Box<SurfaceId> leaked in surface::create().
    // Recover the surface ID to know which surface to close.
    if userdata.is_null() {
        return;
    }
    let surface_id = unsafe { *(userdata as *const crate::split::SurfaceId) };

    // Defer to next main loop iteration — this callback fires during
    // ghostty_app_tick(), so we can't modify surfaces/UI here.
    glib::idle_add_local_once(move || {
        crate::window::close_surface(surface_id);
    });
}
