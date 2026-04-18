use std::ffi::CString;

use gdk4::prelude::*;
use gtk4::prelude::*;
use gtk4::{self, glib};

use crate::ghostty_sys::*;

/// Translate GDK modifier state to Ghostty mods.
fn translate_mods(state: gdk4::ModifierType) -> ghostty_input_mods_e {
    let mut mods: ghostty_input_mods_e = GHOSTTY_MODS_NONE;
    if state.contains(gdk4::ModifierType::SHIFT_MASK) {
        mods |= GHOSTTY_MODS_SHIFT;
    }
    if state.contains(gdk4::ModifierType::CONTROL_MASK) {
        mods |= GHOSTTY_MODS_CTRL;
    }
    if state.contains(gdk4::ModifierType::ALT_MASK) {
        mods |= GHOSTTY_MODS_ALT;
    }
    if state.contains(gdk4::ModifierType::SUPER_MASK) {
        mods |= GHOSTTY_MODS_SUPER;
    }
    if state.contains(gdk4::ModifierType::LOCK_MASK) {
        mods |= GHOSTTY_MODS_CAPS;
    }
    mods
}

/// Translate a GDK mouse button number to Ghostty.
fn translate_mouse_button(button: u32) -> ghostty_input_mouse_button_e {
    match button {
        1 => GHOSTTY_MOUSE_LEFT,
        2 => GHOSTTY_MOUSE_MIDDLE,
        3 => GHOSTTY_MOUSE_RIGHT,
        4 => GHOSTTY_MOUSE_FOUR,
        5 => GHOSTTY_MOUSE_FIVE,
        6 => GHOSTTY_MOUSE_SIX,
        7 => GHOSTTY_MOUSE_SEVEN,
        8 => GHOSTTY_MOUSE_EIGHT,
        _ => GHOSTTY_MOUSE_UNKNOWN,
    }
}

/// Attach all input event controllers to the GLArea.
pub fn attach(
    gl_area: &gtk4::GLArea,
    surface: ghostty_surface_t,
    im_context: &gtk4::IMMulticontext,
) {
    attach_keyboard(gl_area, surface, im_context);
    attach_mouse(gl_area, surface);
    attach_scroll(gl_area, surface);
    attach_focus(gl_area, surface);
    attach_ime(gl_area, surface, im_context);
}

fn attach_keyboard(
    gl_area: &gtk4::GLArea,
    surface: ghostty_surface_t,
    im_context: &gtk4::IMMulticontext,
) {
    let key_ctrl = gtk4::EventControllerKey::new();
    key_ctrl.set_im_context(Some(im_context));

    let surf = surface;
    key_ctrl.connect_key_pressed(move |_ctrl, keyval, keycode, state| {
        let mods = translate_mods(state);

        // Get text from keyval (Key has .to_unicode() and .to_lower() methods)
        let uc: Option<char> = keyval.to_unicode();
        let text_cstring: Option<CString> = uc
            .filter(|c| !c.is_control())
            .and_then(|c| {
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                CString::new(s.as_bytes()).ok()
            });

        let text_ptr: *const std::ffi::c_char = text_cstring
            .as_ref()
            .map_or(std::ptr::null(), |cs| cs.as_ptr());

        // Unshifted codepoint
        let unshifted_keyval = keyval.to_lower();
        let unshifted_cp: u32 = unshifted_keyval
            .to_unicode()
            .map_or(0, |c| c as u32);

        let key_event = ghostty_input_key_s {
            action: GHOSTTY_ACTION_PRESS,
            keycode,
            mods,
            consumed_mods: GHOSTTY_MODS_NONE,
            text: text_ptr,
            unshifted_codepoint: unshifted_cp,
            composing: false,
        };

        let handled = unsafe { ghostty_surface_key(surf, key_event) };
        glib::Propagation::from(handled)
    });

    let surf = surface;
    key_ctrl.connect_key_released(move |_ctrl, _keyval, keycode, state| {
        let key_event = ghostty_input_key_s {
            action: GHOSTTY_ACTION_RELEASE,
            keycode,
            mods: translate_mods(state),
            consumed_mods: GHOSTTY_MODS_NONE,
            text: std::ptr::null(),
            unshifted_codepoint: 0,
            composing: false,
        };
        unsafe { ghostty_surface_key(surf, key_event) };
    });

    gl_area.add_controller(key_ctrl);
}

fn attach_mouse(gl_area: &gtk4::GLArea, surface: ghostty_surface_t) {
    // Click
    let click = gtk4::GestureClick::new();
    click.set_button(0); // any button

    let surf = surface;
    let gl_weak = gl_area.downgrade();
    click.connect_pressed(move |gesture, _n_press, x, y| {
        let button = gesture.current_button();
        let state = gesture.current_event_state();
        let mods = translate_mods(state);

        unsafe {
            ghostty_surface_mouse_pos(surf, x, y, mods);
            ghostty_surface_mouse_button(
                surf,
                GHOSTTY_MOUSE_PRESS,
                translate_mouse_button(button),
                mods,
            );
        }

        if let Some(gl_area) = gl_weak.upgrade() {
            gl_area.grab_focus();
        }
    });

    let surf = surface;
    click.connect_released(move |gesture, _n_press, x, y| {
        let button = gesture.current_button();
        let state = gesture.current_event_state();
        let mods = translate_mods(state);

        unsafe {
            ghostty_surface_mouse_pos(surf, x, y, mods);
            ghostty_surface_mouse_button(
                surf,
                GHOSTTY_MOUSE_RELEASE,
                translate_mouse_button(button),
                mods,
            );
        }
    });

    gl_area.add_controller(click);

    // Motion
    let motion = gtk4::EventControllerMotion::new();
    let surf = surface;
    motion.connect_motion(move |ctrl, x, y| {
        let state = ctrl.current_event_state();
        unsafe {
            ghostty_surface_mouse_pos(surf, x, y, translate_mods(state));
        }
    });
    gl_area.add_controller(motion);
}

fn attach_scroll(gl_area: &gtk4::GLArea, surface: ghostty_surface_t) {
    let scroll = gtk4::EventControllerScroll::new(
        gtk4::EventControllerScrollFlags::BOTH_AXES
            | gtk4::EventControllerScrollFlags::DISCRETE,
    );

    let surf = surface;
    scroll.connect_scroll(move |ctrl, dx, dy| {
        let state = ctrl.current_event_state();
        let scroll_mods = translate_mods(state) as ghostty_input_scroll_mods_t;

        unsafe {
            ghostty_surface_mouse_scroll(surf, dx, dy, scroll_mods);
        }
        glib::Propagation::Stop
    });

    gl_area.add_controller(scroll);
}

fn attach_focus(gl_area: &gtk4::GLArea, surface: ghostty_surface_t) {
    let focus_ctrl = gtk4::EventControllerFocus::new();

    let surf = surface;
    let gl_weak = gl_area.downgrade();
    focus_ctrl.connect_enter(move |_ctrl| {
        unsafe { ghostty_surface_set_focus(surf, true) };
        // Update the tab's focused surface tracking so tab-switch
        // restores focus to the correct pane
        if let Some(gl_area) = gl_weak.upgrade() {
            let id: Option<&crate::split::SurfaceId> =
                unsafe { gl_area.data("limux-surface-id") }
                    .map(|d| unsafe { d.as_ref() });
            if let Some(&id) = id {
                crate::window::set_focused_surface(id);
            }
        }
    });

    let surf = surface;
    focus_ctrl.connect_leave(move |_ctrl| {
        unsafe { ghostty_surface_set_focus(surf, false) };
    });

    gl_area.add_controller(focus_ctrl);
}

fn attach_ime(
    _gl_area: &gtk4::GLArea,
    surface: ghostty_surface_t,
    im_context: &gtk4::IMMulticontext,
) {
    let surf = surface;
    im_context.connect_commit(move |_ctx, text| {
        if let Ok(cstr) = CString::new(text) {
            unsafe {
                ghostty_surface_text(surf, cstr.as_ptr(), text.len());
            }
        }
    });

    let surf = surface;
    im_context.connect_preedit_changed(move |ctx| {
        let (preedit, _, _) = ctx.preedit_string();
        if let Ok(cstr) = CString::new(preedit.as_str()) {
            unsafe {
                ghostty_surface_preedit(surf, cstr.as_ptr(), preedit.len());
            }
        }
    });
}
