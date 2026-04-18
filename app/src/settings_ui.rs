//! Settings UI — a standalone GTK4 window with tabbed pages.
//!
//! Pages: General, Keyboard Shortcuts, Appearance (stub), About.
//! Opened via Ctrl+, or the gear button in the sidebar.

use std::cell::RefCell;
use std::rc::Rc;

use gdk4;
use gtk4::prelude::*;
use gtk4::{self, glib};

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

    page
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

// ── Appearance page (STUB) ─────────────────────────────────────────

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

    // Ghostty config section (STUB — edit button only)
    page.append(&section_label("Terminal Appearance"));

    let info_label = gtk4::Label::new(Some(
        "Terminal font, colors, cursor, and scrollback are configured in\n\
         Ghostty's config file. Changes take effect on restart."
    ));
    info_label.set_xalign(0.0);
    info_label.set_wrap(true);
    info_label.add_css_class("dim-label");
    page.append(&info_label);

    let config_path = settings::ghostty_config_path();
    let path_label = gtk4::Label::new(Some(&config_path.display().to_string()));
    path_label.set_xalign(0.0);
    path_label.set_selectable(true);
    path_label.add_css_class("monospace");
    page.append(&path_label);

    let edit_btn = gtk4::Button::with_label("Edit Ghostty Config");
    edit_btn.set_halign(gtk4::Align::Start);
    edit_btn.set_margin_top(8);
    edit_btn.connect_clicked(move |_| {
        let path = settings::ghostty_config_path();
        // Ensure the config file exists
        if !path.exists() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, "# Ghostty configuration\n# See: https://ghostty.org/docs/config\n\n");
        }
        // Open in the user's editor
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "xdg-open".to_string());
        let _ = std::process::Command::new(&editor)
            .arg(&path)
            .spawn();
    });
    page.append(&edit_btn);

    page
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

    let version = gtk4::Label::new(Some("Version 0.1.0"));
    version.add_css_class("dim-label");
    page.append(&version);

    let desc = gtk4::Label::new(Some("A Linux terminal workspace manager\npowered by Ghostty"));
    desc.set_justify(gtk4::Justification::Center);
    page.append(&desc);

    page
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
