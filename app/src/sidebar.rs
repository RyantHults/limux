//! Sidebar widget — vertical workspace list on the left side of the window.

#![allow(dead_code)]

use gdk4;
use gtk4::prelude::*;
use gtk4::{self, gio};

use crate::workspace::{WorkspaceColor, WorkspaceId};

/// Build the sidebar widget. Returns the sidebar container and the list box.
pub fn build() -> (gtk4::Box, gtk4::ListBox) {
    let sidebar_box = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    sidebar_box.set_width_request(180);
    sidebar_box.add_css_class("sidebar");

    let list_box = gtk4::ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::Single);
    list_box.set_vexpand(true);
    list_box.add_css_class("workspace-list");

    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    scrolled.set_hscrollbar_policy(gtk4::PolicyType::Never);
    scrolled.set_child(Some(&list_box));
    sidebar_box.append(&scrolled);

    // Bottom bar: "+ New" button and gear button
    let bottom_bar = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    bottom_bar.set_margin_start(4);
    bottom_bar.set_margin_end(4);

    let new_btn = gtk4::Button::with_label("+ New");
    new_btn.add_css_class("flat");
    new_btn.set_hexpand(true);
    new_btn.connect_clicked(|_| {
        crate::window::new_workspace();
    });
    bottom_bar.append(&new_btn);

    let gear_btn = gtk4::Button::from_icon_name("emblem-system-symbolic");
    gear_btn.add_css_class("flat");
    gear_btn.set_tooltip_text(Some("Settings"));
    gear_btn.set_action_name(Some("win.open-settings"));
    bottom_bar.append(&gear_btn);

    sidebar_box.append(&bottom_bar);

    (sidebar_box, list_box)
}

/// Create a sidebar row for a workspace.
pub fn make_row(workspace_id: WorkspaceId, title: &str) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    row.set_selectable(true);

    let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    hbox.set_margin_start(8);
    hbox.set_margin_end(4);
    hbox.set_margin_top(4);
    hbox.set_margin_bottom(4);

    // Color indicator (4px wide left border, hidden until a color is set)
    let color_indicator = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    color_indicator.set_width_request(4);
    color_indicator.set_visible(false);
    color_indicator.set_widget_name("workspace-color");
    hbox.append(&color_indicator);

    // Pin icon (hidden until pinned)
    let pin_icon = gtk4::Image::from_icon_name("view-pin-symbolic");
    pin_icon.set_pixel_size(12);
    pin_icon.set_visible(false);
    pin_icon.set_widget_name("workspace-pin");
    hbox.append(&pin_icon);

    let label = gtk4::Label::new(Some(title));
    label.set_hexpand(true);
    label.set_xalign(0.0);
    label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    label.set_widget_name("workspace-title");
    hbox.append(&label);

    // Git branch label (hidden initially)
    let branch_label = gtk4::Label::new(None);
    branch_label.add_css_class("dim-label");
    branch_label.set_visible(false);
    branch_label.set_widget_name("workspace-branch");
    hbox.append(&branch_label);

    // Remote connection indicator (hidden for local workspaces)
    let remote_icon = gtk4::Image::from_icon_name("network-idle-symbolic");
    remote_icon.set_pixel_size(12);
    remote_icon.set_visible(false);
    remote_icon.set_widget_name("workspace-remote");
    hbox.append(&remote_icon);

    // Bell indicator (hidden until a bell fires in this workspace)
    let bell_indicator = gtk4::Label::new(Some("●"));
    bell_indicator.add_css_class("ws-bell");
    bell_indicator.set_visible(false);
    bell_indicator.set_widget_name("workspace-bell");
    hbox.append(&bell_indicator);

    // Close button
    let close_btn = gtk4::Button::from_icon_name("window-close-symbolic");
    close_btn.add_css_class("flat");
    close_btn.add_css_class("circular");
    close_btn.set_tooltip_text(Some("Close workspace"));
    close_btn.set_widget_name("workspace-close");
    close_btn.connect_clicked(move |_| {
        crate::window::close_workspace(workspace_id);
    });
    hbox.append(&close_btn);

    // Detail box for metadata (status, log, progress) — hidden when empty
    let detail_box = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    detail_box.set_visible(false);
    detail_box.set_widget_name("workspace-detail");
    detail_box.set_margin_start(16);
    detail_box.set_margin_end(4);
    detail_box.add_css_class("workspace-detail");

    // Wrap header + detail in a vertical box
    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    vbox.append(&hbox);
    vbox.append(&detail_box);
    row.set_child(Some(&vbox));

    // Right-click context menu
    attach_context_menu(&row, workspace_id);

    // Drag-and-drop reordering
    attach_drag_and_drop(&row, workspace_id);

    // Store workspace ID on the row for lookup
    unsafe { row.set_data("workspace-id", workspace_id) };

    row
}

/// Attach a right-click context menu to a sidebar row.
fn attach_context_menu(row: &gtk4::ListBoxRow, workspace_id: WorkspaceId) {
    // Build the menu model
    let menu = gio::Menu::new();

    // Color submenu
    let color_submenu = gio::Menu::new();
    for color in WorkspaceColor::ALL {
        let item = gio::MenuItem::new(
            Some(&capitalize(&color.to_string())),
            Some(&format!("ws.set-color-{}", color)),
        );
        color_submenu.append_item(&item);
    }
    color_submenu.append(Some("None"), Some("ws.clear-color"));
    menu.append_submenu(Some("Set Color"), &color_submenu);

    // Rename
    menu.append(Some("Rename"), Some("ws.rename"));

    // Pin/unpin
    menu.append(Some("Pin / Unpin"), Some("ws.toggle-pin"));

    let popover = gtk4::PopoverMenu::from_model(Some(&menu));
    popover.set_parent(row);
    popover.set_has_arrow(false);

    // Register actions on the row
    let action_group = gio::SimpleActionGroup::new();

    for color in WorkspaceColor::ALL {
        let color_val = *color;
        let a = gio::SimpleAction::new(&format!("set-color-{}", color), None);
        let ws_id = workspace_id;
        a.connect_activate(move |_, _| {
            crate::window::set_workspace_color(ws_id, Some(color_val));
        });
        action_group.add_action(&a);
    }

    let a = gio::SimpleAction::new("clear-color", None);
    let ws_id = workspace_id;
    a.connect_activate(move |_, _| {
        crate::window::set_workspace_color(ws_id, None);
    });
    action_group.add_action(&a);

    let a = gio::SimpleAction::new("toggle-pin", None);
    let ws_id = workspace_id;
    a.connect_activate(move |_, _| {
        crate::window::toggle_workspace_pinned(ws_id);
    });
    action_group.add_action(&a);

    let a = gio::SimpleAction::new("rename", None);
    let ws_id = workspace_id;
    let row_weak = row.downgrade();
    a.connect_activate(move |_, _| {
        // Delay until after the popover finishes closing
        let row_weak = row_weak.clone();
        gtk4::glib::idle_add_local_once(move || {
            if let Some(row) = row_weak.upgrade() {
                start_inline_rename(&row, ws_id);
            }
        });
    });
    action_group.add_action(&a);

    row.insert_action_group("ws", Some(&action_group));

    // Right-click gesture
    let gesture = gtk4::GestureClick::new();
    gesture.set_button(3); // right-click
    let popover_clone = popover.clone();
    gesture.connect_released(move |gesture, _, x, y| {
        gesture.set_state(gtk4::EventSequenceState::Claimed);
        popover_clone.set_pointing_to(Some(&gdk4::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover_clone.popup();
    });
    row.add_controller(gesture);
}

/// Attach drag source and drop target for sidebar row reordering.
fn attach_drag_and_drop(row: &gtk4::ListBoxRow, workspace_id: WorkspaceId) {
    // Drag source: carry workspace ID as a string
    let drag_source = gtk4::DragSource::new();
    drag_source.set_actions(gdk4::DragAction::MOVE);

    let ws_id = workspace_id;
    drag_source.connect_prepare(move |_source, _x, _y| {
        let value = gtk4::glib::Value::from(ws_id.to_string());
        Some(gdk4::ContentProvider::for_value(&value))
    });

    let row_weak = row.downgrade();
    drag_source.connect_drag_begin(move |_source, _drag| {
        if let Some(row) = row_weak.upgrade() {
            row.add_css_class("drag-active");
        }
    });

    let row_weak = row.downgrade();
    drag_source.connect_drag_end(move |_source, _drag, _delete| {
        if let Some(row) = row_weak.upgrade() {
            row.remove_css_class("drag-active");
        }
    });

    row.add_controller(drag_source);

    // Drop target: accept workspace ID strings
    let drop_target = gtk4::DropTarget::new(gtk4::glib::Type::STRING, gdk4::DragAction::MOVE);

    let row_weak = row.downgrade();
    drop_target.connect_motion(move |_target, _x, y| {
        if let Some(row) = row_weak.upgrade() {
            let height = row.height() as f64;
            row.remove_css_class("drop-above");
            row.remove_css_class("drop-below");
            if y < height / 2.0 {
                row.add_css_class("drop-above");
            } else {
                row.add_css_class("drop-below");
            }
        }
        gdk4::DragAction::MOVE
    });

    let row_weak = row.downgrade();
    drop_target.connect_leave(move |_target| {
        if let Some(row) = row_weak.upgrade() {
            row.remove_css_class("drop-above");
            row.remove_css_class("drop-below");
        }
    });

    let target_ws_id = workspace_id;
    let row_weak = row.downgrade();
    drop_target.connect_drop(move |_target, value, _x, y| {
        // Clean up drop indicators
        if let Some(row) = row_weak.upgrade() {
            row.remove_css_class("drop-above");
            row.remove_css_class("drop-below");
        }

        let Ok(source_str) = value.get::<String>() else { return false };
        let Ok(source_id) = source_str.parse::<WorkspaceId>() else { return false };

        if source_id == target_ws_id {
            return false; // Can't drop on self
        }

        let row_height = row_weak.upgrade().map(|r| r.height() as f64).unwrap_or(40.0);
        let before = y < row_height / 2.0;
        crate::window::move_workspace(source_id, target_ws_id, before);
        true
    });

    row.add_controller(drop_target);
}

/// Show a rename dialog for a workspace.
fn start_inline_rename(row: &gtk4::ListBoxRow, workspace_id: WorkspaceId) {
    // Get current title
    let current_title = find_child_by_name(row, "workspace-title")
        .and_then(|w| w.downcast::<gtk4::Label>().ok())
        .map(|l| l.label().to_string())
        .unwrap_or_default();

    // Find the toplevel window
    let Some(window) = row.root().and_then(|r| r.downcast::<gtk4::Window>().ok()) else { return };

    // Build a popover with an entry anchored to the row
    let entry = gtk4::Entry::new();
    entry.set_text(&current_title);
    entry.set_width_chars(20);

    let popover = gtk4::Popover::new();
    popover.set_child(Some(&entry));
    popover.set_parent(row);
    popover.set_autohide(true);

    // Commit on Enter
    let popover_enter = popover.clone();
    let ws_id = workspace_id;
    entry.connect_activate(move |entry| {
        let new_name = entry.text().to_string();
        if !new_name.is_empty() {
            crate::window::rename_workspace(ws_id, &new_name);
        }
        popover_enter.popdown();
    });

    // Cancel on Escape
    let key_ctrl = gtk4::EventControllerKey::new();
    let popover_esc = popover.clone();
    key_ctrl.connect_key_pressed(move |_, key, _, _| {
        if key == gdk4::Key::Escape {
            popover_esc.popdown();
            return gtk4::glib::Propagation::Stop;
        }
        gtk4::glib::Propagation::Proceed
    });
    entry.add_controller(key_ctrl);

    // Clean up popover parent on close
    let popover_closed = popover.clone();
    popover.connect_closed(move |_| {
        popover_closed.unparent();
    });

    popover.popup();
    entry.grab_focus();
    entry.select_region(0, -1);
}

/// Update the title label of a sidebar row.
pub fn update_row_title(row: &gtk4::ListBoxRow, title: &str) {
    if let Some(widget) = find_child_by_name(row, "workspace-title") {
        if let Some(label) = widget.downcast_ref::<gtk4::Label>() {
            label.set_label(title);
        }
    }
}

/// Update the remote connection indicator on a sidebar row.
pub fn update_row_remote_state(
    row: &gtk4::ListBoxRow,
    state: &crate::remote::RemoteConnectionState,
    detail: Option<&str>,
) {
    use crate::remote::RemoteConnectionState;

    if let Some(widget) = crate::util::find_widget_by_name(row.upcast_ref(), "workspace-remote") {
        if let Some(image) = widget.downcast_ref::<gtk4::Image>() {
            match state {
                RemoteConnectionState::Disconnected => {
                    image.set_visible(false);
                    image.remove_css_class("remote-connecting");
                    image.remove_css_class("remote-connected");
                    image.remove_css_class("remote-error");
                }
                RemoteConnectionState::Connecting => {
                    image.set_icon_name(Some("network-transmit-receive-symbolic"));
                    image.set_visible(true);
                    image.remove_css_class("remote-connected");
                    image.remove_css_class("remote-error");
                    image.add_css_class("remote-connecting");
                    image.set_tooltip_text(Some(detail.unwrap_or("Connecting...")));
                }
                RemoteConnectionState::Connected => {
                    image.set_icon_name(Some("network-idle-symbolic"));
                    image.set_visible(true);
                    image.remove_css_class("remote-connecting");
                    image.remove_css_class("remote-error");
                    image.add_css_class("remote-connected");
                    image.set_tooltip_text(Some(detail.unwrap_or("Connected")));
                }
                RemoteConnectionState::Error => {
                    image.set_icon_name(Some("network-error-symbolic"));
                    image.set_visible(true);
                    image.remove_css_class("remote-connecting");
                    image.remove_css_class("remote-connected");
                    image.add_css_class("remote-error");
                    image.set_tooltip_text(Some(detail.unwrap_or("Connection error")));
                }
            }
        }
    }
}

/// Update the git branch label of a sidebar row.
pub fn update_row_branch(row: &gtk4::ListBoxRow, branch: Option<&str>) {
    if let Some(hbox) = row.child().and_downcast::<gtk4::Box>() {
        // Find the branch label by iterating children
        let mut child = hbox.first_child();
        while let Some(widget) = child {
            if let Some(label) = widget.downcast_ref::<gtk4::Label>() {
                if label.widget_name() == "workspace-branch" {
                    match branch {
                        Some(b) => {
                            label.set_label(b);
                            label.set_visible(true);
                        }
                        None => {
                            label.set_visible(false);
                        }
                    }
                    return;
                }
            }
            child = widget.next_sibling();
        }
    }
}

/// Update the color indicator on a sidebar row.
pub fn update_row_color(row: &gtk4::ListBoxRow, color: Option<WorkspaceColor>) {
    let Some(indicator) = find_child_by_name(row, "workspace-color") else { return };

    // Remove all existing color classes
    for c in WorkspaceColor::ALL {
        indicator.remove_css_class(c.css_class());
    }

    match color {
        Some(c) => {
            indicator.add_css_class(c.css_class());
            indicator.set_visible(true);
        }
        None => {
            indicator.set_visible(false);
        }
    }
}

/// Update the pin icon visibility on a sidebar row.
pub fn update_row_pinned(row: &gtk4::ListBoxRow, pinned: bool) {
    if let Some(pin) = find_child_by_name(row, "workspace-pin") {
        pin.set_visible(pinned);
    }
    // Hide close button when pinned
    if let Some(close) = find_child_by_name(row, "workspace-close") {
        close.set_visible(!pinned);
    }
}

/// Update the bell indicator on a sidebar row.
pub fn update_row_bell(row: &gtk4::ListBoxRow, has_bell: bool) {
    if let Some(bell) = find_child_by_name(row, "workspace-bell") {
        bell.set_visible(has_bell);
    }
}

/// Get the workspace ID stored on a row.
pub fn row_workspace_id(row: &gtk4::ListBoxRow) -> Option<WorkspaceId> {
    unsafe { row.data::<WorkspaceId>("workspace-id").map(|d| *d.as_ref()) }
}

/// Update status entries in the detail box.
pub fn update_row_status(
    row: &gtk4::ListBoxRow,
    entries: &std::collections::HashMap<String, crate::workspace::SidebarStatusEntry>,
) {
    let Some(detail) = find_child_by_name(row, "workspace-detail") else { return };
    let detail_box = detail.downcast_ref::<gtk4::Box>().unwrap();

    // Remove old status widgets
    remove_children_by_class(detail_box, "status-entry");

    // Add sorted entries
    let mut sorted: Vec<_> = entries.values().collect();
    sorted.sort_by(|a, b| b.priority.cmp(&a.priority));

    for entry in sorted {
        let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        hbox.add_css_class("status-entry");

        if let Some(ref icon) = entry.icon {
            let img = gtk4::Image::from_icon_name(icon);
            img.set_pixel_size(12);
            hbox.append(&img);
        }

        let text = format!("{}: {}", entry.key, entry.value);
        let label = gtk4::Label::new(Some(&text));
        label.set_xalign(0.0);
        label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        if let Some(ref color_class) = entry.color {
            label.add_css_class(&format!("status-color-{color_class}"));
        }
        hbox.append(&label);

        // Prepend status entries before log/progress
        detail_box.prepend(&hbox);
    }

    update_detail_visibility(detail_box);
}

/// Update the latest log entry in the detail box.
pub fn update_row_log(row: &gtk4::ListBoxRow, entry: Option<&crate::workspace::SidebarLogEntry>) {
    let Some(detail) = find_child_by_name(row, "workspace-detail") else { return };
    let detail_box = detail.downcast_ref::<gtk4::Box>().unwrap();

    // Remove old log widget
    remove_children_by_class(detail_box, "log-entry");

    if let Some(entry) = entry {
        let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        hbox.add_css_class("log-entry");
        hbox.add_css_class(&format!("log-{}", entry.level.as_str()));

        let level_icon = match entry.level {
            crate::workspace::LogLevel::Info => "dialog-information-symbolic",
            crate::workspace::LogLevel::Progress => "emblem-synchronizing-symbolic",
            crate::workspace::LogLevel::Success => "emblem-ok-symbolic",
            crate::workspace::LogLevel::Warning => "dialog-warning-symbolic",
            crate::workspace::LogLevel::Error => "dialog-error-symbolic",
        };
        let img = gtk4::Image::from_icon_name(level_icon);
        img.set_pixel_size(12);
        hbox.append(&img);

        let label = gtk4::Label::new(Some(&entry.message));
        label.set_xalign(0.0);
        label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        label.add_css_class("dim-label");
        hbox.append(&label);

        detail_box.append(&hbox);
    }

    update_detail_visibility(detail_box);
}

/// Update the progress bar in the detail box.
pub fn update_row_progress(
    row: &gtk4::ListBoxRow,
    state: Option<&crate::workspace::SidebarProgressState>,
) {
    let Some(detail) = find_child_by_name(row, "workspace-detail") else { return };
    let detail_box = detail.downcast_ref::<gtk4::Box>().unwrap();

    // Remove old progress widget
    remove_children_by_class(detail_box, "progress-entry");

    if let Some(state) = state {
        let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 1);
        vbox.add_css_class("progress-entry");

        let bar = gtk4::ProgressBar::new();
        bar.set_fraction(state.value.clamp(0.0, 1.0));
        vbox.append(&bar);

        if let Some(ref label_text) = state.label {
            let label = gtk4::Label::new(Some(label_text));
            label.set_xalign(0.0);
            label.add_css_class("dim-label");
            label.add_css_class("caption");
            vbox.append(&label);
        }

        detail_box.append(&vbox);
    }

    update_detail_visibility(detail_box);
}

/// Remove all children of a box that have a specific CSS class.
fn remove_children_by_class(container: &gtk4::Box, class: &str) {
    let mut child = container.first_child();
    while let Some(widget) = child {
        let next = widget.next_sibling();
        if widget.has_css_class(class) {
            container.remove(&widget);
        }
        child = next;
    }
}

/// Show the detail box if it has any children, hide otherwise.
fn update_detail_visibility(detail_box: &gtk4::Box) {
    detail_box.set_visible(detail_box.first_child().is_some());
}

/// Find a child widget by widget name within a sidebar row (recursive).
fn find_child_by_name(row: &gtk4::ListBoxRow, name: &str) -> Option<gtk4::Widget> {
    let root = row.child()?;
    crate::util::find_widget_by_name(&root, name)
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

thread_local! {
    static CSS_PROVIDER: std::cell::RefCell<Option<gtk4::CssProvider>> =
        const { std::cell::RefCell::new(None) };
}

/// Apply CSS styling for the sidebar (initial load).
pub fn load_css() {
    let dark = crate::appearance::is_dark();
    apply_css(dark);
}

/// Reload CSS with a new color scheme (called by appearance.rs on theme change).
pub fn reload_css(dark: bool) {
    apply_css(dark);
}

/// Generate and apply the dynamic CSS for the given color scheme.
fn apply_css(dark: bool) {
    let display = gdk4::Display::default().unwrap();

    // Remove old provider if present
    CSS_PROVIDER.with(|cell| {
        if let Some(old) = cell.borrow().as_ref() {
            gtk4::style_context_remove_provider_for_display(&display, old);
        }
    });

    let bell_hex = WorkspaceColor::bell_hex(dark);
    let css = generate_css(dark, bell_hex);

    let provider = gtk4::CssProvider::new();
    provider.load_from_string(&css);
    gtk4::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    CSS_PROVIDER.with(|cell| {
        *cell.borrow_mut() = Some(provider);
    });
}

/// Build the full CSS string with palette-appropriate colors.
fn generate_css(dark: bool, bell_hex: &str) -> String {
    let mut css = String::with_capacity(2048);

    // Static rules using GTK theme variables (auto-adapt to light/dark)
    css.push_str(r#"
        .sidebar {
            background-color: @theme_bg_color;
            border-right: 1px solid @borders;
        }
        .workspace-list row {
            padding: 2px 0;
        }
        .workspace-list row:selected {
            background-color: alpha(@theme_selected_bg_color, 0.3);
        }
        .drag-active { opacity: 0.5; }
        .drop-above { border-top: 2px solid @theme_selected_bg_color; }
        .drop-below { border-bottom: 2px solid @theme_selected_bg_color; }
        .pane-tab-strip {
            min-height: 24px;
            padding: 2px 4px;
            border-bottom: 1px solid @borders;
        }
        .pane-tab {
            margin: 0 2px;
        }
        .pane-tab-strip.pane-active .pane-tab:checked {
            box-shadow: inset 0 2px 0 0 @theme_selected_bg_color;
        }
        .pane-tab-close {
            opacity: 0.5;
        }
        .pane-tab-close:hover {
            opacity: 1.0;
        }
        .workspace-detail { font-size: 0.85em; margin-top: 2px; margin-bottom: 2px; }
        .status-entry { margin-top: 1px; }
        .progress-entry { margin-top: 2px; }
        .progress-entry progressbar { min-height: 4px; }
        .caption { font-size: 0.8em; }
        paned > separator {
            min-width: 1px;
            min-height: 1px;
            padding: 0;
            margin: 0;
        }
    "#);

    // Dynamic color rules — workspace indicators
    for color in WorkspaceColor::ALL {
        let hex = color.hex(dark);
        css.push_str(&format!(
            ".{} {{ background-color: {}; border-radius: 2px; }}\n",
            color.css_class(), hex
        ));
    }

    // Bell indicators
    css.push_str(&format!(
        ".ws-bell {{ color: {bell_hex}; font-size: 10px; margin-start: 4px; }}\n"
    ));
    css.push_str(&format!(
        ".pane-bell {{ border: 2px solid {bell_hex}; }}\n"
    ));

    // Remote connection state indicators
    let remote_muted = if dark { "#888888" } else { "#999999" };
    let remote_ok = if dark { "#73d216" } else { "#4e9a06" };
    let remote_err = if dark { "#ff6b6b" } else { "#cc0000" };
    css.push_str(&format!(".remote-connecting {{ color: {}; }}\n", remote_muted));
    css.push_str(&format!(".remote-connected {{ color: {}; }}\n", remote_ok));
    css.push_str(&format!(".remote-error {{ color: {}; }}\n", remote_err));

    // Status colors
    for color in &["red", "orange", "yellow", "green", "blue", "purple"] {
        let wc = WorkspaceColor::from_name(color).unwrap();
        css.push_str(&format!(
            ".status-color-{color} {{ color: {}; }}\n",
            wc.hex(dark)
        ));
    }

    // Log level colors
    let blue = WorkspaceColor::Blue.hex(dark);
    let green = WorkspaceColor::Green.hex(dark);
    let orange = WorkspaceColor::Orange.hex(dark);
    let red = WorkspaceColor::Red.hex(dark);
    css.push_str(&format!(".log-info     {{ }}\n"));
    css.push_str(&format!(".log-progress {{ color: {blue}; }}\n"));
    css.push_str(&format!(".log-success  {{ color: {green}; }}\n"));
    css.push_str(&format!(".log-warning  {{ color: {orange}; }}\n"));
    css.push_str(&format!(".log-error    {{ color: {red}; }}\n"));

    css
}
