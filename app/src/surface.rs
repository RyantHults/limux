//! Terminal surface creation — creates a GtkGLArea backed by a Ghostty surface.

use std::cell::Cell;
use std::ffi::CString;

use gtk4::prelude::*;
use gtk4::{self, glib};

use crate::ghostty_sys::*;
use crate::input;
use crate::split::SurfaceId;
use crate::surfaces::{self, SurfaceEntry};

/// Create a new terminal surface widget. Returns (GLArea, SurfaceId).
/// The SurfaceId is assigned once the GLArea is realized.
/// If `forced_id` is Some, use that ID instead of allocating a new one.
pub fn create(
    working_directory: Option<&str>,
    command: Option<&str>,
) -> (gtk4::GLArea, std::rc::Rc<Cell<Option<SurfaceId>>>) {
    create_with_id(working_directory, command, None)
}

pub fn create_with_id(
    working_directory: Option<&str>,
    command: Option<&str>,
    forced_id: Option<SurfaceId>,
) -> (gtk4::GLArea, std::rc::Rc<Cell<Option<SurfaceId>>>) {
    let gl_area = gtk4::GLArea::new();
    gl_area.set_auto_render(false);
    gl_area.set_has_depth_buffer(false);
    gl_area.set_has_stencil_buffer(false);
    gl_area.set_allowed_apis(gdk4::GLAPI::GL);
    gl_area.set_focusable(true);
    gl_area.set_focus_on_click(true);
    gl_area.set_hexpand(true);
    gl_area.set_vexpand(true);

    let wd = working_directory.map(|s| CString::new(s).unwrap());
    let cmd = command.map(|s| CString::new(s).unwrap());

    let surface_id: std::rc::Rc<Cell<Option<SurfaceId>>> = std::rc::Rc::new(Cell::new(None));
    let sid_realize = surface_id.clone();

    // ── unrealize: GL context is about to be destroyed (e.g. during reparenting).
    // Deinitialize GPU resources but preserve terminal state (PTY, scrollback).
    gl_area.connect_unrealize(|gl_area| {
        let id: Option<&SurfaceId> = unsafe { gl_area.data("limux-surface-id") }
            .map(|d| unsafe { d.as_ref() });
        if let Some(&id) = id {
            eprintln!("[unrealize] surface {id} — deinitializing GPU resources");
            gl_area.make_current();
            if let Some(handle) = surfaces::get_handle(id) {
                unsafe { ghostty_surface_display_unrealized(handle) };
            }
        }
    });

    // ── realize: create Ghostty surface ──
    gl_area.connect_realize(move |gl_area| {
        // If already realized (re-realize after reparenting), reinitialize GPU resources
        let existing: Option<&SurfaceId> = unsafe { gl_area.data("limux-surface-id") }
            .map(|d| unsafe { d.as_ref() });
        if let Some(&id) = existing {
            eprintln!("[realize] re-realize of surface {id} — reinitializing GPU resources");
            gl_area.make_current();
            if let Some(handle) = surfaces::get_handle(id) {
                unsafe { ghostty_surface_display_realized(handle) };
            }
            gl_area.queue_render();
            return;
        }

        gl_area.make_current();
        if let Some(err) = gl_area.error() {
            eprintln!("GLArea realize error: {err}");
            return;
        }

        let mut cfg = unsafe { ghostty_surface_config_new() };
        cfg.platform_tag = GHOSTTY_PLATFORM_LINUX;
        cfg.platform = ghostty_platform_u {
            linux: ghostty_platform_linux_s {
                _reserved: std::ptr::null_mut(),
            },
        };

        let id = forced_id.unwrap_or_else(|| surfaces::pre_allocate_id());
        let id_box = Box::new(id);
        let id_ptr = Box::into_raw(id_box) as *mut std::os::raw::c_void;

        cfg.userdata = id_ptr;
        cfg.scale_factor = gl_area.scale_factor() as f64;
        cfg.font_size = 0.0;
        cfg.working_directory = wd
            .as_ref()
            .map_or(std::ptr::null(), |s| s.as_ptr());
        cfg.command = cmd
            .as_ref()
            .map_or(std::ptr::null(), |s| s.as_ptr());

        let surface = unsafe { ghostty_surface_new(crate::app::GhosttyApp::handle(), &cfg) };
        if surface.is_null() {
            unsafe { drop(Box::from_raw(id_ptr as *mut SurfaceId)) };
            return;
        }

        let scale = gl_area.scale_factor();
        let w = gl_area.width();
        let h = gl_area.height();
        if w > 0 && h > 0 {
            unsafe {
                ghostty_surface_set_size(surface, (w * scale) as u32, (h * scale) as u32);
                ghostty_surface_set_content_scale(surface, scale as f64, scale as f64);
            }
        }

        let im_context = gtk4::IMMulticontext::new();

        surfaces::register_with_id(id, SurfaceEntry {
            surface,
            gl_area: gl_area.clone(),
            im_context: im_context.clone(),
        });
        sid_realize.set(Some(id));

        unsafe { gl_area.set_data("limux-surface-id", id) };

        input::attach(gl_area, surface, &im_context);

        // Queue initial render
        gl_area.queue_render();
    });

    // ── render ──
    gl_area.connect_render(|gl_area, _ctx| {
        // Explicitly make context current — after reparenting, the GL
        // state may need refreshing even though GTK should have done this.
        gl_area.make_current();

        let id: Option<&SurfaceId> = unsafe { gl_area.data("limux-surface-id") }
            .map(|d| unsafe { d.as_ref() });
        if let Some(&id) = id {
            if let Some(handle) = surfaces::get_handle(id) {
                let scale = gl_area.scale_factor();
                let w = gl_area.width();
                let h = gl_area.height();

                if w > 0 && h > 0 {
                    unsafe {
                        ghostty_surface_set_size(
                            handle,
                            (w * scale) as u32,
                            (h * scale) as u32,
                        );
                    }
                }

                unsafe { ghostty_surface_draw(handle) };
            }
        }
        glib::Propagation::Stop
    });

    // ── resize ──
    gl_area.connect_resize(|gl_area, width, height| {
        let id: Option<&SurfaceId> = unsafe { gl_area.data("limux-surface-id") }
            .map(|d| unsafe { d.as_ref() });
        if let Some(&id) = id {
            if let Some(handle) = surfaces::get_handle(id) {
                let scale = gl_area.scale_factor();
                unsafe {
                    ghostty_surface_set_size(handle, (width * scale) as u32, (height * scale) as u32);
                    ghostty_surface_set_content_scale(handle, scale as f64, scale as f64);
                }
            }
        }
    });

    (gl_area, surface_id)
}

/// Explicitly free a Ghostty surface by ID.
pub fn close_surface(id: SurfaceId) {
    if let Some(entry) = surfaces::unregister(id) {
        unsafe { ghostty_surface_free(entry.surface) };
    }
}

/// Queue render on all registered surfaces.
pub fn queue_render() {
    surfaces::queue_render_all();
}

/// Get any surface handle (for clipboard callbacks).
pub fn get_surface_handle() -> Option<ghostty_surface_t> {
    surfaces::any_handle()
}
