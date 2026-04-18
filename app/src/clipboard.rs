use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};

use gdk4::prelude::*;

use crate::ghostty_sys::*;
use crate::surfaces;

fn get_clipboard(location: ghostty_clipboard_e) -> Option<gdk4::Clipboard> {
    let display = gdk4::Display::default()?;
    Some(match location {
        GHOSTTY_CLIPBOARD_SELECTION => display.primary_clipboard(),
        _ => display.clipboard(),
    })
}

/// Extract the surface handle from the userdata pointer.
/// Ghostty passes the surface's userdata (a leaked Box<SurfaceId>) in callbacks.
fn surface_from_userdata(userdata: *mut c_void) -> Option<ghostty_surface_t> {
    if userdata.is_null() {
        return None;
    }
    let surface_id = unsafe { *(userdata as *const crate::split::SurfaceId) };
    surfaces::get_handle(surface_id)
}

/// Runtime callback: Ghostty requests clipboard content.
pub unsafe extern "C" fn read_clipboard_cb(
    userdata: *mut c_void,
    location: ghostty_clipboard_e,
    state: *mut c_void,
) {
    let Some(clipboard) = get_clipboard(location) else {
        return;
    };
    let Some(surface) = surface_from_userdata(userdata) else {
        return;
    };

    // state is an opaque pointer we must pass back to Ghostty
    let state_ptr = state as usize;

    clipboard.read_text_async(
        gtk4::gio::Cancellable::NONE,
        move |result| {
            let text = result
                .ok()
                .flatten()
                .unwrap_or_default();
            let cstr = CString::new(text.as_str()).unwrap_or_default();
            unsafe {
                ghostty_surface_complete_clipboard_request(
                    surface,
                    cstr.as_ptr(),
                    state_ptr as *mut c_void,
                    true,
                );
            }
        },
    );
}

/// Runtime callback: Ghostty confirms clipboard read (auto-confirm for Phase 1).
pub unsafe extern "C" fn confirm_read_clipboard_cb(
    userdata: *mut c_void,
    content: *const c_char,
    state: *mut c_void,
    _request_type: ghostty_clipboard_request_e,
) {
    let Some(surface) = surface_from_userdata(userdata) else {
        return;
    };

    unsafe {
        ghostty_surface_complete_clipboard_request(
            surface,
            if content.is_null() {
                c"".as_ptr()
            } else {
                content
            },
            state,
            true,
        );
    }
}

/// Runtime callback: Ghostty writes to clipboard.
pub unsafe extern "C" fn write_clipboard_cb(
    _userdata: *mut c_void,
    location: ghostty_clipboard_e,
    content: *const ghostty_clipboard_content_s,
    content_count: usize,
    _should_confirm: bool,
) {
    let Some(clipboard) = get_clipboard(location) else {
        return;
    };

    if content.is_null() || content_count == 0 {
        return;
    }

    // Find text content
    let items = unsafe { std::slice::from_raw_parts(content, content_count) };
    for item in items {
        if item.data.is_null() {
            continue;
        }
        // Accept text/plain or null mime (default)
        let is_text = item.mime.is_null() || {
            let mime = unsafe { CStr::from_ptr(item.mime) };
            mime.to_bytes() == b"text/plain"
        };
        if is_text {
            let text = unsafe { CStr::from_ptr(item.data) }
                .to_str()
                .unwrap_or("");
            clipboard.set_text(text);
            return;
        }
    }

    // Fallback: use first item
    if !items[0].data.is_null() {
        let text = unsafe { CStr::from_ptr(items[0].data) }
            .to_str()
            .unwrap_or("");
        clipboard.set_text(text);
    }
}
