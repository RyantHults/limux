//! Small cross-cutting utilities shared across modules.

use std::ffi::CStr;
use std::os::raw::c_char;

/// Define an atomic u32 ID allocator: a static `AtomicU32` and a public
/// function that returns the next ID.
#[macro_export]
macro_rules! make_id_allocator {
    ($fn_name:ident, $static_name:ident) => {
        static $static_name: std::sync::atomic::AtomicU32 =
            std::sync::atomic::AtomicU32::new(0);

        pub fn $fn_name() -> u32 {
            $static_name.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    };
}

use gtk4::glib;
use gtk4::prelude::*;

/// Convert a C string pointer to an owned `String`, returning `None` if null.
///
/// # Safety
/// `ptr` must be null or point to a valid NUL-terminated C string.
pub unsafe fn cstr_to_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        None
    } else {
        Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
    }
}

/// Recursively search a widget tree for a child with the given widget name.
pub fn find_widget_by_name(widget: &gtk4::Widget, name: &str) -> Option<gtk4::Widget> {
    if widget.widget_name() == name {
        return Some(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        if let Some(found) = find_widget_by_name(&c, name) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

/// Defer a `grab_focus()` call to the next idle iteration of the GLib main loop.
/// Useful after widget reparenting or visibility changes where immediate focus
/// would be ignored by GTK.
pub fn defer_focus(widget: &impl IsA<gtk4::Widget>) {
    let weak = widget.downgrade();
    glib::idle_add_local_once(move || {
        if let Some(w) = weak.upgrade() {
            w.grab_focus();
        }
    });
}
