//! Tab strip UI — rendering, drag-and-drop, context menus, rename.
//!
//! Extracted from window.rs. Functions here build and manage the per-pane
//! tab strip widget but do not directly access the WINDOW thread-local.

use std::cell::RefCell;
use std::rc::Rc;

use gdk4;
use gtk4::prelude::*;
use gtk4::{self, glib};

use crate::split::Orientation;
use crate::workspace::{self, PaneId, PanelKind};
use crate::window::PanelOps;


// ── Drop preview types ──────────────────────────────────────────────

/// Which edge of a pane the cursor is closest to during drag-and-drop.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DropZone {
    Left,
    Right,
    Top,
    Bottom,
}

impl DropZone {
    /// Convert to split orientation and insertion order.
    pub(crate) fn to_split_params(self) -> (Orientation, bool) {
        match self {
            DropZone::Left => (Orientation::Horizontal, true),
            DropZone::Right => (Orientation::Horizontal, false),
            DropZone::Top => (Orientation::Vertical, true),
            DropZone::Bottom => (Orientation::Vertical, false),
        }
    }
}

/// Detect which edge zone the cursor is in using diagonal quadrants.
pub(crate) fn detect_drop_zone(x: f64, y: f64, width: f64, height: f64) -> DropZone {
    if width <= 0.0 || height <= 0.0 {
        return DropZone::Right;
    }
    let nx = x / width;
    let ny = y / height;
    // Compare position relative to the two diagonals
    let above_main = ny < nx;       // above top-left → bottom-right diagonal
    let above_anti = ny < 1.0 - nx; // above top-right → bottom-left diagonal
    match (above_main, above_anti) {
        (true, true) => DropZone::Top,
        (false, false) => DropZone::Bottom,
        (false, true) => DropZone::Left,
        (true, false) => DropZone::Right,
    }
}

/// Rectangle for the drop preview overlay, in overlay-relative coordinates.
#[derive(Clone, Copy)]
pub(crate) struct PreviewRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

// ── PaneWidget ──────────────────────────────────────────────────────

/// Widget state for a single pane within a workspace's split layout.
pub(crate) struct PaneWidget {
    /// Container: vertical GtkBox with optional tab strip + GtkStack of GLAreas
    pub container: gtk4::Box,
    /// Stack holding one GLArea per tab in this pane
    pub stack: gtk4::Stack,
    /// Tab strip (hidden when only one tab)
    pub tab_strip: gtk4::Box,
}

// ── Tab strip rendering ──────────────────────────────────────────────

/// Rebuild the tab strip buttons for a pane.
pub(crate) fn refresh_pane_tab_strip(
    pane_widget: &PaneWidget,
    pane: &workspace::Pane,
    drop_preview: &gtk4::DrawingArea,
    drop_preview_rect: &Rc<RefCell<Option<PreviewRect>>>,
) {
    // Clear existing strip buttons
    while let Some(child) = pane_widget.tab_strip.first_child() {
        pane_widget.tab_strip.remove(&child);
    }

    let pane_id = pane.id;

    for (idx, tab) in pane.tabs.iter().enumerate() {
        let label_text = if tab.title.is_empty() {
            if tab.is_browser() { "Browser".to_string() } else { "Terminal".to_string() }
        } else {
            tab.title.clone()
        };

        // Single toggle button containing [label | close_icon]
        // The close icon uses a capture-phase gesture to intercept clicks
        // before the toggle button handles them.
        let btn_content = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        let label = gtk4::Label::new(Some(&label_text));
        btn_content.append(&label);

        let close_icon = gtk4::Image::from_icon_name("window-close-symbolic");
        close_icon.set_pixel_size(12);
        close_icon.add_css_class("pane-tab-close");
        close_icon.set_tooltip_text(Some("Close tab"));
        close_icon.set_can_target(true);
        btn_content.append(&close_icon);

        let btn = gtk4::ToggleButton::new();
        btn.set_child(Some(&btn_content));
        btn.set_active(idx == pane.selected_tab);
        btn.add_css_class("flat");
        btn.add_css_class("pane-tab");

        // Capture-phase click on the button — check if click is on the close icon region
        let panel_close2 = tab.panel.clone();
        let close_icon_weak = close_icon.downgrade();
        let close_click = gtk4::GestureClick::new();
        close_click.set_button(1);
        close_click.set_propagation_phase(gtk4::PropagationPhase::Capture);
        close_click.connect_pressed(move |gesture, _, x, _y| {
            if let Some(icon) = close_icon_weak.upgrade() {
                // Check if click x is within the close icon's allocated area
                let Some(btn_widget) = gesture.widget() else { return };
                if let Some(origin) = icon.compute_point(&btn_widget, &gtk4::graphene::Point::new(0.0, 0.0)) {
                    let icon_start = origin.x() as f64;
                    let icon_end = icon_start + icon.width() as f64;
                    if x >= icon_start && x <= icon_end {
                        gesture.set_state(gtk4::EventSequenceState::Claimed);
                        crate::window::close_tab_by_panel(pane_id, &panel_close2);
                    }
                }
            }
        });
        btn.add_controller(close_click);

        let panel = tab.panel.clone();
        btn.connect_clicked(move |_| {
            crate::window::select_pane_tab_generic(pane_id, idx, &panel);
        });

        // Middle-click on the toggle button closes the tab
        let panel_close = tab.panel.clone();
        let middle_click = gtk4::GestureClick::new();
        middle_click.set_button(2);
        middle_click.connect_released(move |gesture, _, _, _| {
            gesture.set_state(gtk4::EventSequenceState::Claimed);
            crate::window::close_tab_by_panel(pane_id, &panel_close);
        });
        btn.add_controller(middle_click);

        let tab_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        tab_box.append(&btn);

        // Right-click context menu
        attach_tab_context_menu(&btn, pane_id, idx);

        // Drag source on the tab box
        attach_pane_tab_drag_source(&btn, pane_id, idx);

        pane_widget.tab_strip.append(&tab_box);
    }

    // Remove stale drop targets before adding new ones
    remove_drop_targets(pane_widget.tab_strip.upcast_ref::<gtk4::Widget>());
    remove_drop_targets(pane_widget.stack.upcast_ref::<gtk4::Widget>());

    // Drop target on the tab strip — merges dragged tab into this pane
    attach_tab_strip_drop_target(&pane_widget.tab_strip, pane_id, drop_preview, drop_preview_rect);

    // Drop target on the terminal area (stack) — splits the pane.
    // Browser panes need capture phase because WebKitWebView consumes the
    // pointer-release event, preventing the drop signal from firing in bubble phase.
    let has_browser = pane.tabs.iter().any(|t| t.is_browser());
    attach_pane_drop_target(&pane_widget.stack, pane_id, has_browser, drop_preview, drop_preview_rect);
}

/// Attach a right-click context menu to a pane tab button.
fn attach_tab_context_menu(btn: &gtk4::ToggleButton, pane_id: PaneId, tab_idx: usize) {
    let menu = gtk4::gio::Menu::new();
    menu.append(Some("Rename"), Some("tab.rename"));

    let popover = gtk4::PopoverMenu::from_model(Some(&menu));
    popover.set_parent(btn);
    popover.set_has_arrow(false);

    // Unparent popover when button is destroyed to avoid finalization warning
    let popover_cleanup = popover.clone();
    btn.connect_destroy(move |_| {
        popover_cleanup.unparent();
    });

    let action_group = gtk4::gio::SimpleActionGroup::new();

    let rename_action = gtk4::gio::SimpleAction::new("rename", None);
    let btn_weak = btn.downgrade();
    rename_action.connect_activate(move |_, _| {
        let btn_weak = btn_weak.clone();
        glib::idle_add_local_once(move || {
            if let Some(btn) = btn_weak.upgrade() {
                start_tab_rename(&btn, pane_id, tab_idx);
            }
        });
    });
    action_group.add_action(&rename_action);

    btn.insert_action_group("tab", Some(&action_group));

    let gesture = gtk4::GestureClick::new();
    gesture.set_button(3); // right-click
    let popover_clone = popover.clone();
    gesture.connect_released(move |gesture, _, x, y| {
        gesture.set_state(gtk4::EventSequenceState::Claimed);
        popover_clone.set_pointing_to(Some(&gdk4::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover_clone.popup();
    });
    btn.add_controller(gesture);
}

/// Show a rename popover for a tab.
fn start_tab_rename(btn: &gtk4::ToggleButton, pane_id: PaneId, tab_idx: usize) {
    // Button child is a Box containing [Label, Image]
    let current_title = btn.child()
        .and_then(|c| c.first_child())
        .and_then(|w| w.downcast::<gtk4::Label>().ok())
        .map(|l| l.label().to_string())
        .unwrap_or_default();

    let entry = gtk4::Entry::new();
    entry.set_text(&current_title);
    entry.set_width_chars(20);

    let popover = gtk4::Popover::new();
    popover.set_child(Some(&entry));
    popover.set_parent(btn);
    popover.set_autohide(true);

    let popover_enter = popover.clone();
    entry.connect_activate(move |entry| {
        let new_name = entry.text().to_string();
        if !new_name.is_empty() {
            crate::window::rename_tab(pane_id, tab_idx, &new_name);
        }
        popover_enter.popdown();
    });

    let key_ctrl = gtk4::EventControllerKey::new();
    let popover_esc = popover.clone();
    key_ctrl.connect_key_pressed(move |_, key, _, _| {
        if key == gdk4::Key::Escape {
            popover_esc.popdown();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    entry.add_controller(key_ctrl);

    let popover_closed = popover.clone();
    popover.connect_closed(move |_| {
        popover_closed.unparent();
    });

    popover.popup();
    entry.grab_focus();
    entry.select_region(0, -1);
}

/// Attach a drag source to a pane tab button for pane reordering.
fn attach_pane_tab_drag_source(btn: &gtk4::ToggleButton, pane_id: PaneId, tab_idx: usize) {
    let drag_source = gtk4::DragSource::new();
    drag_source.set_actions(gdk4::DragAction::MOVE);

    drag_source.connect_prepare(move |_source, _x, _y| {
        let value = glib::Value::from(format!("pane:{}:{}", pane_id, tab_idx));
        Some(gdk4::ContentProvider::for_value(&value))
    });

    btn.add_controller(drag_source);
}

/// Remove all DropTarget controllers from a widget.
pub(crate) fn remove_drop_targets(widget: &gtk4::Widget) {
    let mut to_remove = Vec::new();
    let controllers = widget.observe_controllers();
    for i in 0..controllers.n_items() {
        if let Some(obj) = controllers.item(i) {
            if obj.downcast_ref::<gtk4::DropTarget>().is_some() {
                to_remove.push(obj.downcast::<gtk4::EventController>().unwrap());
            }
        }
    }
    for ctrl in to_remove {
        widget.remove_controller(&ctrl);
    }
}

/// Attach a drop target to the tab strip — dropping here merges the tab into this pane.
fn attach_tab_strip_drop_target(
    tab_strip: &gtk4::Box,
    target_pane_id: PaneId,
    drop_preview: &gtk4::DrawingArea,
    preview_rect: &Rc<RefCell<Option<PreviewRect>>>,
) {
    let drop_target = gtk4::DropTarget::new(glib::Type::STRING, gdk4::DragAction::MOVE);

    // Reject external file drops.
    drop_target.connect_accept(|_target, drop| {
        let formats = drop.formats();
        if formats.contains_type(gdk4::FileList::static_type()) {
            return false;
        }
        true
    });

    let inset = 6.0_f64;

    // --- motion: show full-pane overlay ---
    let strip_weak = tab_strip.downgrade();
    let preview_weak = drop_preview.downgrade();
    let rect_motion = preview_rect.clone();
    drop_target.connect_motion(move |_target, _x, _y| {
        if let (Some(strip), Some(preview)) = (strip_weak.upgrade(), preview_weak.upgrade()) {
            // The pane container is the tab strip's parent
            if let Some(container) = strip.parent() {
                let overlay = preview.parent().unwrap();
                let w = container.width() as f64;
                let h = container.height() as f64;
                if let Some(origin) = container.compute_point(&overlay, &gtk4::graphene::Point::new(0.0, 0.0)) {
                    let ox = origin.x() as f64;
                    let oy = origin.y() as f64;
                    *rect_motion.borrow_mut() = Some(PreviewRect {
                        x: ox + inset, y: oy + inset,
                        width: w - inset * 2.0, height: h - inset * 2.0,
                    });
                    preview.queue_draw();
                }
            }
        }
        gdk4::DragAction::MOVE
    });

    // --- leave: clear preview ---
    let preview_weak = drop_preview.downgrade();
    let rect_leave = preview_rect.clone();
    drop_target.connect_leave(move |_target| {
        *rect_leave.borrow_mut() = None;
        if let Some(preview) = preview_weak.upgrade() {
            preview.queue_draw();
        }
    });

    // --- drop: move tab into this pane ---
    let preview_weak = drop_preview.downgrade();
    let rect_drop = preview_rect.clone();
    drop_target.connect_drop(move |_target, value, _x, _y| {
        *rect_drop.borrow_mut() = None;
        if let Some(preview) = preview_weak.upgrade() {
            preview.queue_draw();
        }

        let Ok(payload) = value.get::<String>() else { return false };
        let Some(rest) = payload.strip_prefix("pane:") else { return false };
        let Some((pid, tidx)) = rest.split_once(':') else { return false };
        let Ok(source_pane_id) = pid.parse::<PaneId>() else { return false };
        let Ok(tab_idx) = tidx.parse::<usize>() else { return false };

        if source_pane_id == target_pane_id {
            return false; // already in this pane
        }

        // Defer to idle so GTK finishes drag cleanup before we modify widgets
        glib::idle_add_local_once(move || {
            crate::window::move_tab_to_pane(source_pane_id, tab_idx, target_pane_id);
        });
        true
    });

    tab_strip.add_controller(drop_target);
}

/// Attach a drop target to the pane's content area (stack) for split drops.
fn attach_pane_drop_target(
    container: &gtk4::Stack,
    target_pane_id: PaneId,
    has_browser: bool,
    drop_preview: &gtk4::DrawingArea,
    preview_rect: &Rc<RefCell<Option<PreviewRect>>>,
) {
    let drop_target = gtk4::DropTarget::new(glib::Type::STRING, gdk4::DragAction::MOVE);
    // WebKitWebView consumes pointer-release events, preventing the drop signal
    // from firing in bubble phase. Use capture phase for browser panes so the
    // drop target intercepts events before the WebView sees them.
    if has_browser {
        drop_target.set_propagation_phase(gtk4::PropagationPhase::Capture);
    }

    // Reject external file drops — those are handled by the GLArea's file drop target.
    drop_target.connect_accept(|_target, drop| {
        let formats = drop.formats();
        // If the drag offers GdkFileList, it's a file drop from a file manager.
        if formats.contains_type(gdk4::FileList::static_type()) {
            return false;
        }
        true
    });

    let inset = 6.0_f64;

    // --- motion: compute overlay preview rect ---
    let container_weak = container.downgrade();
    let preview_weak = drop_preview.downgrade();
    let rect_motion = preview_rect.clone();
    drop_target.connect_motion(move |_target, x, y| {

        if let (Some(container), Some(preview)) =
            (container_weak.upgrade(), preview_weak.upgrade())
        {
            let w = container.width() as f64;
            let h = container.height() as f64;
            let zone = detect_drop_zone(x, y, w, h);

            // Translate container origin to overlay coordinates
            let overlay = preview.parent().unwrap();
            if let Some(origin) = container.compute_point(&overlay, &gtk4::graphene::Point::new(0.0, 0.0)) {
                let ox = origin.x() as f64;
                let oy = origin.y() as f64;

                let rect = match zone {
                    DropZone::Left => PreviewRect {
                        x: ox + inset, y: oy + inset,
                        width: w / 2.0 - inset * 2.0, height: h - inset * 2.0,
                    },
                    DropZone::Right => PreviewRect {
                        x: ox + w / 2.0 + inset, y: oy + inset,
                        width: w / 2.0 - inset * 2.0, height: h - inset * 2.0,
                    },
                    DropZone::Top => PreviewRect {
                        x: ox + inset, y: oy + inset,
                        width: w - inset * 2.0, height: h / 2.0 - inset * 2.0,
                    },
                    DropZone::Bottom => PreviewRect {
                        x: ox + inset, y: oy + h / 2.0 + inset,
                        width: w - inset * 2.0, height: h / 2.0 - inset * 2.0,
                    },
                };

                *rect_motion.borrow_mut() = Some(rect);
                preview.queue_draw();
            }
        }
        gdk4::DragAction::MOVE
    });

    // --- leave: hide preview ---
    let preview_weak = drop_preview.downgrade();
    let rect_leave = preview_rect.clone();
    drop_target.connect_leave(move |_target| {

        *rect_leave.borrow_mut() = None;
        if let Some(preview) = preview_weak.upgrade() {
            preview.queue_draw();
        }
    });

    // --- drop: execute move, hide preview ---
    let container_weak = container.downgrade();
    let preview_weak = drop_preview.downgrade();
    let rect_drop = preview_rect.clone();
    drop_target.connect_drop(move |_target, value, x, y| {

        // Clear preview
        *rect_drop.borrow_mut() = None;
        if let Some(preview) = preview_weak.upgrade() {
            preview.queue_draw();
        }

        let Ok(payload) = value.get::<String>() else { return false };
        let Some(rest) = payload.strip_prefix("pane:") else { return false };

        // Parse "pane:{pane_id}:{tab_idx}" or legacy "pane:{pane_id}"
        let (source_pane_id, source_tab_idx) = if let Some((pid, tidx)) = rest.split_once(':') {
            let Ok(pid) = pid.parse::<PaneId>() else { return false };
            let Ok(tidx) = tidx.parse::<usize>() else { return false };
            (pid, Some(tidx))
        } else {
            let Ok(pid) = rest.parse::<PaneId>() else { return false };
            (pid, None)
        };

        let (w, h) = container_weak.upgrade()
            .map(|c| (c.width() as f64, c.height() as f64))
            .unwrap_or((100.0, 100.0));
        let zone = detect_drop_zone(x, y, w, h);
        let (orientation, before) = zone.to_split_params();

        // Defer to idle so GTK finishes drag cleanup before we modify widgets
        glib::idle_add_local_once(move || {
            if source_pane_id == target_pane_id {
                // Same pane: extract tab into a new split
                if let Some(tab_idx) = source_tab_idx {
                    crate::window::split_tab_to_pane(source_pane_id, tab_idx, orientation, before);
                }
            } else if let Some(tab_idx) = source_tab_idx {
                // Different pane with a specific tab: check if source has multiple tabs
                let source_tab_count = crate::window::pane_tab_count(source_pane_id);
                if source_tab_count > 1 {
                    // Extract just this tab, then move it adjacent to target
                    crate::window::split_tab_to_pane_target(source_pane_id, tab_idx, target_pane_id, orientation, before);
                } else {
                    crate::window::move_pane(source_pane_id, target_pane_id, before, orientation);
                }
            } else {
                crate::window::move_pane(source_pane_id, target_pane_id, before, orientation);
            }
        });
        true
    });

    container.add_controller(drop_target);
}
