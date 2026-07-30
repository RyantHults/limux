//! Settings UI — a standalone GTK4 window with tabbed pages.
//!
//! Pages: General, Keyboard Shortcuts, Appearance, About.
//! Opened via Ctrl+, or the gear button in the sidebar.

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::rc::Rc;

use gdk4;
use gtk4::prelude::*;
use gtk4::{self, glib};

use crate::app::GhosttyApp;
use crate::ghostty_sys::{ghostty_config_color_s, ghostty_config_get};
use crate::settings::{self, Settings, ShortcutDef, SHORTCUT_DEFAULTS};

/// Show the settings window (creates a new one or presents existing).
pub fn show(parent: &gtk4::ApplicationWindow) {
    let window = gtk4::Window::builder()
        .title("Settings")
        .default_width(700)
        .default_height(500)
        .transient_for(parent)
        .modal(false)
        .build();

    let stack = gtk4::Stack::new();
    stack.set_transition_type(gtk4::StackTransitionType::SlideLeftRight);

    let sidebar = gtk4::StackSidebar::new();
    sidebar.set_stack(&stack);
    sidebar.set_width_request(150);

    // Build pages
    let settings = settings::get();

    let general_page = build_general_page(&settings);
    stack.add_titled(&general_page, Some("general"), "General");

    let shortcuts_page = build_shortcuts_page(&settings);
    stack.add_titled(&shortcuts_page, Some("shortcuts"), "Keyboard Shortcuts");

    let appearance_page = build_appearance_page(&settings);
    stack.add_titled(&appearance_page, Some("appearance"), "Appearance");

    let about_page = build_about_page();
    stack.add_titled(&about_page, Some("about"), "About");

    // Layout: sidebar | stack
    let paned = gtk4::Paned::new(gtk4::Orientation::Horizontal);
    paned.set_start_child(Some(&sidebar));
    paned.set_end_child(Some(&stack));
    paned.set_position(150);
    paned.set_resize_start_child(false);
    paned.set_resize_end_child(true);
    paned.set_shrink_start_child(false);
    paned.set_shrink_end_child(false);

    window.set_child(Some(&paned));
    window.present();
}

// ── General page ───────────────────────────────────────────────────

fn build_general_page(settings: &Settings) -> gtk4::Box {
    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    page.set_margin_top(16);
    page.set_margin_bottom(16);
    page.set_margin_start(16);
    page.set_margin_end(16);

    // Default shell
    page.append(&section_label("Shell"));
    let shell_entry = gtk4::Entry::new();
    shell_entry.set_placeholder_text(Some("/bin/bash"));
    if let Some(ref shell) = settings.general.default_shell {
        shell_entry.set_text(shell);
    }
    shell_entry.connect_changed(|entry| {
        let text = entry.text().to_string();
        settings::update(|s| {
            s.general.default_shell = if text.is_empty() { None } else { Some(text) };
        });
    });
    page.append(&form_row("Default shell", &shell_entry));

    // Working directory
    let dir_entry = gtk4::Entry::new();
    dir_entry.set_placeholder_text(Some("~ (home directory)"));
    if let Some(ref dir) = settings.general.working_directory {
        dir_entry.set_text(dir);
    }
    dir_entry.connect_changed(|entry| {
        let text = entry.text().to_string();
        settings::update(|s| {
            s.general.working_directory = if text.is_empty() { None } else { Some(text) };
        });
    });
    page.append(&form_row("Working directory", &dir_entry));

    // Session restore
    page.append(&section_label("Behavior"));
    let session_switch = gtk4::Switch::new();
    session_switch.set_active(settings.session_restore());
    session_switch.connect_active_notify(|sw| {
        let active = sw.is_active();
        settings::update(|s| {
            s.general.session_restore = Some(active);
        });
    });
    page.append(&form_row("Restore session on startup", &session_switch));

    // Notifications
    let notif_switch = gtk4::Switch::new();
    notif_switch.set_active(settings.notifications_enabled());
    notif_switch.connect_active_notify(|sw| {
        let active = sw.is_active();
        settings::update(|s| {
            s.general.notifications_enabled = Some(active);
        });
        if active {
            crate::notify::enable();
        } else {
            crate::notify::disable();
        }
        crate::tray::update_notifications_enabled(active);
    });
    page.append(&form_row("Desktop notifications", &notif_switch));

    // Desktop integration
    append_integrate_section(&page);

    page
}

fn append_integrate_section(parent: &gtk4::Box) {
    use crate::{install, settings};
    if !install::is_appimage() {
        return;
    }

    if install::is_integrated() {
        // Already integrated — show an insensitive switch.
        let sw = gtk4::Switch::new();
        sw.set_active(true);
        sw.set_sensitive(false);
        parent.append(&form_row("Desktop integration", &sw));
        let sub = gtk4::Label::new(Some("limux is already integrated with your desktop."));
        sub.set_xalign(0.0);
        sub.add_css_class("dim-label");
        parent.append(&sub);
        return;
    }

    // Not yet integrated — show an "Integrate" button.
    let btn = gtk4::Button::with_label("Integrate");
    let row = form_row("Desktop integration", &btn);
    parent.append(&row);

    let parent_weak = parent.downgrade();
    let row_weak = row.downgrade();
    btn.connect_clicked(move |_| {
        match install::perform_integration() {
            Ok(()) => {
                settings::update(|s| s.general.desktop_integrated = Some(true));
                if let (Some(parent), Some(row)) = (parent_weak.upgrade(), row_weak.upgrade()) {
                    parent.remove(&row);
                    // Rebuild as "already integrated".
                    let sw = gtk4::Switch::new();
                    sw.set_active(true);
                    sw.set_sensitive(false);
                    parent.append(&form_row("Desktop integration", &sw));
                    let new_sub =
                        gtk4::Label::new(Some("limux is already integrated with your desktop."));
                    new_sub.set_xalign(0.0);
                    new_sub.add_css_class("dim-label");
                    parent.append(&new_sub);
                }
            }
            Err(e) => eprintln!("integration failed: {}", e),
        }
    });
}

// ── Keyboard Shortcuts page ────────────────────────────────────────

fn build_shortcuts_page(settings: &Settings) -> gtk4::Box {
    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    page.set_margin_top(16);
    page.set_margin_bottom(16);
    page.set_margin_start(16);
    page.set_margin_end(16);

    page.append(&section_label("Keyboard Shortcuts"));

    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    scrolled.set_hscrollbar_policy(gtk4::PolicyType::Never);

    let list_box = gtk4::ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);
    list_box.add_css_class("boxed-list");

    for def in SHORTCUT_DEFAULTS {
        let accel = settings
            .shortcuts
            .get(def.action)
            .cloned()
            .unwrap_or_else(|| def.default_accel.to_string());

        let row = build_shortcut_row(def, &accel);
        list_box.append(&row);
    }

    scrolled.set_child(Some(&list_box));
    page.append(&scrolled);

    // Reset to defaults button
    let reset_btn = gtk4::Button::with_label("Reset All to Defaults");
    reset_btn.set_halign(gtk4::Align::Start);
    reset_btn.set_margin_top(8);
    let list_box_weak = list_box.downgrade();
    reset_btn.connect_clicked(move |_| {
        settings::update(|s| {
            s.shortcuts.clear();
        });
        crate::window::rebuild_shortcuts();
        // Refresh the list
        if let Some(list_box) = list_box_weak.upgrade() {
            // Remove all rows and rebuild
            while let Some(child) = list_box.first_child() {
                list_box.remove(&child);
            }
            for def in SHORTCUT_DEFAULTS {
                let row = build_shortcut_row(def, def.default_accel);
                list_box.append(&row);
            }
        }
    });
    page.append(&reset_btn);

    page
}

fn build_shortcut_row(def: &ShortcutDef, current_accel: &str) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    hbox.set_margin_start(8);
    hbox.set_margin_end(8);
    hbox.set_margin_top(6);
    hbox.set_margin_bottom(6);

    let label = gtk4::Label::new(Some(def.label));
    label.set_hexpand(true);
    label.set_xalign(0.0);
    hbox.append(&label);

    // Shortcut display label
    let accel_label = gtk4::ShortcutLabel::new(current_accel);
    accel_label.set_disabled_text("Not set");
    hbox.append(&accel_label);

    // "Set" button to capture a new key combo
    let set_btn = gtk4::Button::with_label("Set");
    set_btn.add_css_class("flat");
    let action_name = def.action.to_string();
    let accel_label_weak = accel_label.downgrade();
    set_btn.connect_clicked(move |btn| {
        start_key_capture(btn, &action_name, accel_label_weak.clone());
    });
    hbox.append(&set_btn);

    row.set_child(Some(&hbox));
    row
}

/// Enter key capture mode: show a popover that captures the next keypress.
fn start_key_capture(
    anchor: &gtk4::Button,
    action: &str,
    accel_label: glib::WeakRef<gtk4::ShortcutLabel>,
) {
    let popover = gtk4::Popover::new();
    popover.set_parent(anchor);
    popover.set_autohide(true);

    let label = gtk4::Label::new(Some("Press a key combination..."));
    label.set_margin_top(12);
    label.set_margin_bottom(12);
    label.set_margin_start(16);
    label.set_margin_end(16);
    popover.set_child(Some(&label));

    let key_ctrl = gtk4::EventControllerKey::new();
    let action_owned = action.to_string();
    let popover_weak = popover.downgrade();
    key_ctrl.connect_key_pressed(move |_, keyval, _keycode, state| {
        // Ignore bare modifier presses
        if matches!(
            keyval,
            gdk4::Key::Shift_L
                | gdk4::Key::Shift_R
                | gdk4::Key::Control_L
                | gdk4::Key::Control_R
                | gdk4::Key::Alt_L
                | gdk4::Key::Alt_R
                | gdk4::Key::Super_L
                | gdk4::Key::Super_R
        ) {
            return glib::Propagation::Proceed;
        }

        // Escape cancels
        if keyval == gdk4::Key::Escape {
            if let Some(p) = popover_weak.upgrade() {
                p.popdown();
            }
            return glib::Propagation::Stop;
        }

        // Build accelerator string
        let accel = gtk4::accelerator_name(keyval, state);
        let accel_str = accel.to_string();

        // Check for conflicts
        let conflict = check_shortcut_conflict(&action_owned, &accel_str);
        if let Some(conflict_action) = conflict {
            // Find the label for the conflicting action
            let conflict_label = SHORTCUT_DEFAULTS
                .iter()
                .find(|d| d.action == conflict_action)
                .map(|d| d.label)
                .unwrap_or(&conflict_action);
            label.set_text(&format!("Conflicts with: {conflict_label}"));
            return glib::Propagation::Stop;
        }

        // Apply the new shortcut
        settings::update(|s| {
            s.shortcuts.insert(action_owned.clone(), accel_str.clone());
        });

        // Rebuild the shortcut controller so the new binding takes effect
        crate::window::rebuild_shortcuts();

        // Update display
        if let Some(al) = accel_label.upgrade() {
            al.set_accelerator(&accel_str);
        }

        if let Some(p) = popover_weak.upgrade() {
            p.popdown();
        }

        glib::Propagation::Stop
    });

    popover.add_controller(key_ctrl);

    let popover_close = popover.clone();
    popover.connect_closed(move |_| {
        popover_close.unparent();
    });

    popover.popup();
}

/// Check if an accelerator conflicts with an existing shortcut.
/// Returns the conflicting action name if found, None otherwise.
fn check_shortcut_conflict(action: &str, accel: &str) -> Option<String> {
    let current = settings::get();
    let merged = settings::merged_shortcuts(&current);
    for (other_action, other_accel) in &merged {
        if other_action != action && other_accel == accel {
            return Some(other_action.clone());
        }
    }
    None
}

// ── Appearance page ────────────────────────────────────────────────

fn build_appearance_page(settings: &Settings) -> gtk4::Box {
    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    page.set_margin_top(16);
    page.set_margin_bottom(16);
    page.set_margin_start(16);
    page.set_margin_end(16);

    // Sidebar settings (functional)
    page.append(&section_label("Sidebar"));

    let width_spin = gtk4::SpinButton::with_range(100.0, 500.0, 10.0);
    width_spin.set_value(settings.sidebar_width() as f64);
    width_spin.connect_value_changed(|spin| {
        let val = spin.value() as i32;
        settings::update(|s| {
            s.sidebar.width = Some(val);
        });
    });
    page.append(&form_row("Default sidebar width", &width_spin));

    let visible_switch = gtk4::Switch::new();
    visible_switch.set_active(settings.sidebar_visible());
    visible_switch.connect_active_notify(|sw| {
        let active = sw.is_active();
        settings::update(|s| {
            s.sidebar.visible = Some(active);
        });
    });
    page.append(&form_row("Sidebar visible on startup", &visible_switch));

    page.append(&build_terminal_appearance_section());

    page
}

fn build_terminal_appearance_section() -> gtk4::Box {
    let section = gtk4::Box::new(gtk4::Orientation::Vertical, 6);

    section.append(&section_label("Terminal Appearance"));

    let info = gtk4::Label::new(Some(
        "Settings are written to Ghostty's config file and applied immediately.",
    ));
    info.set_xalign(0.0);
    info.set_wrap(true);
    info.add_css_class("dim-label");
    section.append(&info);

    // ── Font ──────────────────────────────────────────────────────────
    section.append(&subsection_label("Font"));

    // font-family: RepeatableString — read from file, written to file
    let family_entry = gtk4::Entry::new();
    family_entry.set_hexpand(true);
    family_entry.set_placeholder_text(Some("system default"));
    if let Some(v) = parse_ghostty_config_key("font-family") {
        family_entry.set_text(&v);
    }
    let e = family_entry.clone();
    family_entry.connect_activate(move |_| {
        apply_config_change("font-family", &e.text());
    });
    let e = family_entry.clone();
    let fc = gtk4::EventControllerFocus::new();
    fc.connect_leave(move |_| apply_config_change("font-family", &e.text()));
    family_entry.add_controller(fc);
    section.append(&form_row("Family", &family_entry));

    // font-size: f32 — read via FFI, written as integer string
    let font_size = config_get_f32("font-size").unwrap_or(12.0);
    let size_spin = gtk4::SpinButton::with_range(6.0, 72.0, 1.0);
    size_spin.set_value(font_size as f64);
    size_spin.connect_value_changed(|spin| {
        apply_config_change("font-size", &format!("{}", spin.value() as u32));
    });
    section.append(&form_row("Size", &size_spin));

    // ── Colors ────────────────────────────────────────────────────────
    section.append(&subsection_label("Colors"));

    // background/foreground: optional Color — read via FFI (None if not set in config)
    let bg_entry = gtk4::Entry::new();
    bg_entry.set_hexpand(true);
    bg_entry.set_placeholder_text(Some("terminal default"));
    if let Some(c) = config_get_color("background") {
        bg_entry.set_text(&color_to_hex(c));
    }
    let e = bg_entry.clone();
    bg_entry.connect_activate(move |_| apply_config_change("background", &e.text()));
    let e = bg_entry.clone();
    let fc = gtk4::EventControllerFocus::new();
    fc.connect_leave(move |_| apply_config_change("background", &e.text()));
    bg_entry.add_controller(fc);
    section.append(&form_row("Background", &bg_entry));

    let fg_entry = gtk4::Entry::new();
    fg_entry.set_hexpand(true);
    fg_entry.set_placeholder_text(Some("terminal default"));
    if let Some(c) = config_get_color("foreground") {
        fg_entry.set_text(&color_to_hex(c));
    }
    let e = fg_entry.clone();
    fg_entry.connect_activate(move |_| apply_config_change("foreground", &e.text()));
    let e = fg_entry.clone();
    let fc = gtk4::EventControllerFocus::new();
    fc.connect_leave(move |_| apply_config_change("foreground", &e.text()));
    fg_entry.add_controller(fc);
    section.append(&form_row("Foreground", &fg_entry));

    // ── Cursor ────────────────────────────────────────────────────────
    section.append(&subsection_label("Cursor"));

    // cursor-style: enum — read via FFI, written as tag name string
    const CURSOR_STYLES: &[&str] = &["block", "bar", "underline", "block_hollow"];
    let cursor_combo = gtk4::ComboBoxText::new();
    for s in CURSOR_STYLES {
        cursor_combo.append_text(s);
    }
    let current_cursor = config_get_ptr_str("cursor-style")
        .unwrap_or_else(|| "block".to_string());
    let cursor_idx = CURSOR_STYLES
        .iter()
        .position(|&s| s == current_cursor)
        .unwrap_or(0) as u32;
    cursor_combo.set_active(Some(cursor_idx));
    cursor_combo.connect_changed(|combo| {
        if let Some(v) = combo.active_text() {
            apply_config_change("cursor-style", v.as_str());
        }
    });
    section.append(&form_row("Style", &cursor_combo));

    // ── Scrollback ────────────────────────────────────────────────────
    section.append(&subsection_label("Scrollback"));

    // scrollback-limit: usize — read from file, written as integer string
    let scrollback_entry = gtk4::Entry::new();
    scrollback_entry.set_hexpand(true);
    scrollback_entry.set_placeholder_text(Some("10000000"));
    if let Some(v) = parse_ghostty_config_key("scrollback-limit") {
        scrollback_entry.set_text(&v);
    }
    let e = scrollback_entry.clone();
    scrollback_entry.connect_activate(move |_| {
        let s = e.text();
        // Only write if it parses as a valid integer
        if s.trim().parse::<u64>().is_ok() {
            apply_config_change("scrollback-limit", s.trim());
        }
    });
    let e = scrollback_entry.clone();
    let fc = gtk4::EventControllerFocus::new();
    fc.connect_leave(move |_| {
        let s = e.text();
        if s.trim().parse::<u64>().is_ok() {
            apply_config_change("scrollback-limit", s.trim());
        }
    });
    scrollback_entry.add_controller(fc);
    section.append(&form_row("Limit (bytes)", &scrollback_entry));

    // ── Edit button ───────────────────────────────────────────────────
    let edit_btn = gtk4::Button::with_label("Edit Ghostty Config");
    edit_btn.set_halign(gtk4::Align::Start);
    edit_btn.set_margin_top(8);
    edit_btn.connect_clicked(move |_| {
        let path = settings::ghostty_config_path();
        if !path.exists() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(
                &path,
                "# Ghostty configuration\n# See: https://ghostty.org/docs/config\n\n",
            );
        }
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "xdg-open".to_string());
        let _ = std::process::Command::new(&editor).arg(&path).spawn();
    });
    section.append(&edit_btn);

    section
}

// ── About page ─────────────────────────────────────────────────────

fn build_about_page() -> gtk4::Box {
    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    page.set_margin_top(32);
    page.set_margin_start(16);
    page.set_margin_end(16);
    page.set_valign(gtk4::Align::Start);

    let title = gtk4::Label::new(Some("limux"));
    title.add_css_class("title-1");
    page.append(&title);

    let version = gtk4::Label::new(Some(&format!("Version {}", env!("CARGO_PKG_VERSION"))));
    version.add_css_class("dim-label");
    page.append(&version);

    let desc = gtk4::Label::new(Some("A Linux terminal workspace manager\npowered by Ghostty"));
    desc.set_justify(gtk4::Justification::Center);
    page.append(&desc);

    page
}

// ── Appearance helpers ─────────────────────────────────────────────

/// Write key=value to the Ghostty config file then reload the live config.
fn apply_config_change(key: &str, value: &str) {
    set_ghostty_config_key(key, value.trim());
    GhosttyApp::reload_config();
}

/// Update (or append) a single key in the Ghostty config file.
/// An empty value writes `key =` which resets the field to its default.
fn set_ghostty_config_key(key: &str, value: &str) {
    let path = settings::ghostty_config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let new_line = if value.is_empty() {
        format!("{} =", key)
    } else {
        format!("{} = {}", key, value)
    };

    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    let mut found = false;
    for line in &mut lines {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        // Strip the key prefix; verify the next non-whitespace char is '='
        // so "font-size" doesn't accidentally match "font-size-adjust".
        if let Some(rest) = trimmed.strip_prefix(key) {
            if rest.trim_start().starts_with('=') || rest.trim_start().is_empty() {
                *line = new_line.clone();
                found = true;
                break;
            }
        }
    }
    if !found {
        lines.push(new_line);
    }

    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    let _ = std::fs::write(&path, out);
}

fn subsection_label(text: &str) -> gtk4::Label {
    let label = gtk4::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_margin_top(6);
    label.set_margin_start(4);
    label.add_css_class("dim-label");
    label
}

fn config_get_f32(key: &str) -> Option<f32> {
    let config = GhosttyApp::config();
    if config.is_null() {
        return None;
    }
    let key_c = CString::new(key).ok()?;
    let mut out: f32 = 0.0;
    let ok = unsafe {
        ghostty_config_get(
            config,
            &mut out as *mut f32 as *mut std::os::raw::c_void,
            key_c.as_ptr(),
            key.len(),
        )
    };
    if ok { Some(out) } else { None }
}

fn config_get_color(key: &str) -> Option<ghostty_config_color_s> {
    let config = GhosttyApp::config();
    if config.is_null() {
        return None;
    }
    let key_c = CString::new(key).ok()?;
    let mut out = ghostty_config_color_s::default();
    let ok = unsafe {
        ghostty_config_get(
            config,
            &mut out as *mut ghostty_config_color_s as *mut std::os::raw::c_void,
            key_c.as_ptr(),
            key.len(),
        )
    };
    if ok { Some(out) } else { None }
}

// Works for enum fields (Ghostty writes @tagName static C string)
// and ?[:0]const u8 optional-string fields (writes pointer-or-null).
fn config_get_ptr_str(key: &str) -> Option<String> {
    let config = GhosttyApp::config();
    if config.is_null() {
        return None;
    }
    let key_c = CString::new(key).ok()?;
    let mut out: *const c_char = std::ptr::null();
    let ok = unsafe {
        ghostty_config_get(
            config,
            &mut out as *mut *const c_char as *mut std::os::raw::c_void,
            key_c.as_ptr(),
            key.len(),
        )
    };
    if ok && !out.is_null() {
        Some(unsafe { CStr::from_ptr(out) }.to_string_lossy().into_owned())
    } else {
        None
    }
}

// File-based fallback for config fields the C API doesn't support
// (RepeatableString types like font-family, and usize like scrollback-limit).
fn parse_ghostty_config_key(key: &str) -> Option<String> {
    let content = std::fs::read_to_string(settings::ghostty_config_path()).ok()?;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix(key) {
            let rest = rest.trim_start();
            if let Some(value) = rest.strip_prefix('=') {
                let v = value.trim().to_string();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}

fn color_to_hex(c: ghostty_config_color_s) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

// ── Helpers ────────────────────────────────────────────────────────

fn section_label(text: &str) -> gtk4::Label {
    let label = gtk4::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_margin_top(8);
    label.add_css_class("heading");
    label
}

fn form_row(label_text: &str, widget: &impl IsA<gtk4::Widget>) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
    row.set_margin_start(4);

    let label = gtk4::Label::new(Some(label_text));
    label.set_hexpand(true);
    label.set_xalign(0.0);
    row.append(&label);

    row.append(widget.as_ref());
    row
}
