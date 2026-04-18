//! Window management — sidebar-driven workspace switching with split panes.
//!
//! Layout: GtkPaned(horizontal) → [Sidebar | GtkNotebook(hidden tabs)]
//! Each workspace is a notebook page containing a GtkPaned for split panes.
//! Each pane can contain multiple tabs (surfaces) with a tab strip.

use std::cell::RefCell;
use std::rc::Rc;

use gdk4;
use gio;
use gtk4::prelude::*;
use gtk4::{self, glib};

use crate::browser;
use crate::sidebar;
use crate::split::{Direction, Node, NodeId, Orientation, SplitTree};
use crate::surface;
use crate::surfaces;
use crate::workspace::{self, Pane, PaneId, PanelKind, Workspace, WorkspaceId};

// ── PanelOps trait ─────────────────────────────────────────────────

/// GTK-aware operations on `PanelKind`. Keeps `workspace.rs` free of UI deps.
pub(crate) trait PanelOps {
    /// GtkStack child name for this panel (e.g. `"surface-3"` or `"browser-7"`).
    fn stack_child_name(&self) -> String;

    /// Grab GTK focus and notify Ghostty of the focus change.
    fn focus_gtk(&self);

    /// Request graceful close of the underlying resource.
    fn request_close(&self);
}

impl PanelOps for PanelKind {
    fn stack_child_name(&self) -> String {
        match self {
            PanelKind::Terminal { surface_id } => format!("surface-{surface_id}"),
            PanelKind::Browser { browser_id, .. } => format!("browser-{browser_id}"),
        }
    }

    fn focus_gtk(&self) {
        match self {
            PanelKind::Terminal { surface_id } => {
                if let Some(gl_area) = surfaces::get_gl_area(*surface_id) {
                    gl_area.grab_focus();
                    if let Some(handle) = surfaces::get_handle(*surface_id) {
                        unsafe { crate::ghostty_sys::ghostty_surface_set_focus(handle, true) };
                    }
                }
            }
            PanelKind::Browser { browser_id, .. } => {
                browser::focus_webview(*browser_id);
            }
        }
    }

    fn request_close(&self) {
        match self {
            PanelKind::Terminal { surface_id } => {
                if let Some(handle) = surfaces::get_handle(*surface_id) {
                    unsafe { crate::ghostty_sys::ghostty_surface_request_close(handle) };
                }
            }
            PanelKind::Browser { browser_id, .. } => {
                close_browser(*browser_id);
            }
        }
    }
}

use crate::tab_strip::{self, PaneWidget, PreviewRect};

/// Application window state.
pub struct AppWindow {
    notebook: gtk4::Notebook,
    sidebar_list: gtk4::ListBox,
    main_paned: gtk4::Paned,
    sidebar_box: gtk4::Box,
    sidebar_visible: bool,
    sidebar_last_width: i32,
    workspaces: Vec<Workspace>,
    /// Maps WorkspaceId → (GtkPaned, GtkListBoxRow, notebook page index)
    workspace_widgets: std::collections::HashMap<WorkspaceId, WorkspaceWidgets>,
    working_directory: Option<String>,
    command: Option<String>,
}

struct WorkspaceWidgets {
    /// Root paned for this workspace's split layout.
    root_paned: gtk4::Paned,
    /// Overlay wrapping root_paned — this is the notebook page widget.
    overlay: gtk4::Overlay,
    /// Semi-transparent drawing area for drop preview.
    drop_preview: gtk4::DrawingArea,
    /// Shared state for the drop preview rectangle.
    drop_preview_rect: Rc<RefCell<Option<PreviewRect>>>,
    sidebar_row: gtk4::ListBoxRow,
    /// Maps PaneId → the GtkBox containing the pane tab strip + GLArea stack.
    pane_widgets: std::collections::HashMap<PaneId, PaneWidget>,
    /// Maps SplitTree NodeId → GtkPaned for each split node.
    split_paneds: std::collections::HashMap<NodeId, gtk4::Paned>,
}

impl AppWindow {
    /// Find which workspace contains a given surface, returning (ws_idx, ws_id, pane_id).
    fn find_workspace_with_surface(&self, surface_id: crate::split::SurfaceId) -> Option<(usize, WorkspaceId, PaneId)> {
        for (i, ws) in self.workspaces.iter().enumerate() {
            if let Some(pane_id) = ws.pane_for_surface(surface_id) {
                return Some((i, ws.id, pane_id));
            }
        }
        None
    }

    /// Find which workspace contains a given browser, returning (ws_idx, ws_id, pane_id).
    fn find_workspace_with_browser(&self, browser_id: crate::browser::BrowserPanelId) -> Option<(usize, WorkspaceId, PaneId)> {
        for (i, ws) in self.workspaces.iter().enumerate() {
            if let Some(pane_id) = ws.pane_for_browser(browser_id) {
                return Some((i, ws.id, pane_id));
            }
        }
        None
    }
}

thread_local! {
    static WINDOW: RefCell<Option<Rc<RefCell<AppWindow>>>> = const { RefCell::new(None) };
    static GTK_WINDOW: RefCell<Option<gtk4::ApplicationWindow>> = const { RefCell::new(None) };
    static SHORTCUT_CTRL: RefCell<Option<gtk4::ShortcutController>> = const { RefCell::new(None) };
}

/// Immutable access to the AppWindow. Returns the closure's return value,
/// or the default if WINDOW is unset.
fn with_app_window<R: Default>(f: impl FnOnce(&AppWindow) -> R) -> R {
    WINDOW.with(|w| {
        let binding = w.borrow();
        let Some(app_win) = binding.as_ref() else { return R::default() };
        let Ok(aw) = app_win.try_borrow() else { return R::default() };
        f(&aw)
    })
}

/// Mutable access to the AppWindow. Returns the closure's return value,
/// or the default if WINDOW is unset.
fn with_app_window_mut<R: Default>(f: impl FnOnce(&mut AppWindow) -> R) -> R {
    WINDOW.with(|w| {
        let binding = w.borrow();
        let Some(app_win) = binding.as_ref() else { return R::default() };
        let Ok(mut aw) = app_win.try_borrow_mut() else { return R::default() };
        f(&mut aw)
    })
}

/// Initialize the window with sidebar + notebook.
pub fn init(
    window: &gtk4::ApplicationWindow,
    working_directory: Option<&str>,
    command: Option<&str>,
) {
    sidebar::load_css();

    let (sidebar_box, sidebar_list) = sidebar::build();
    let notebook = gtk4::Notebook::new();
    notebook.set_show_tabs(false); // sidebar replaces tab bar
    notebook.set_scrollable(true);

    // Horizontal paned: sidebar on the left, notebook on the right
    let main_paned = gtk4::Paned::new(gtk4::Orientation::Horizontal);
    main_paned.set_start_child(Some(&sidebar_box));
    main_paned.set_end_child(Some(&notebook));
    main_paned.set_position(180);
    main_paned.set_resize_start_child(false);
    main_paned.set_resize_end_child(true);
    main_paned.set_shrink_start_child(false);
    main_paned.set_shrink_end_child(false);
    set_paned_separator_cursor(&main_paned);

    let app_window = Rc::new(RefCell::new(AppWindow {
        notebook: notebook.clone(),
        sidebar_list: sidebar_list.clone(),
        main_paned: main_paned.clone(),
        sidebar_box: sidebar_box.clone(),
        sidebar_visible: true,
        sidebar_last_width: 180,
        workspaces: Vec::new(),
        workspace_widgets: std::collections::HashMap::new(),
        working_directory: working_directory.map(|s| s.to_string()),
        command: command.map(|s| s.to_string()),
    }));

    WINDOW.with(|w| {
        *w.borrow_mut() = Some(app_window.clone());
    });

    // Sidebar row selection → switch workspace
    let app_win_select = app_window.clone();
    let nb_select = notebook.clone();
    sidebar_list.connect_row_selected(move |_, row| {
        if let Some(row) = row {
            if let Some(ws_id) = sidebar::row_workspace_id(row) {
                let Ok(aw) = app_win_select.try_borrow() else { return };
                // Find the notebook page for this workspace
                if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                    let page_num = nb_select.page_num(&widgets.overlay);
                    if let Some(n) = page_num {
                        nb_select.set_current_page(Some(n));
                    }
                }
            }
        }
    });

    // When notebook page changes (from any source), refresh and focus
    let app_win_switch = app_window.clone();
    notebook.connect_switch_page(move |_notebook, _page, page_num| {
        let page_num = page_num as usize;
        let aw_clone = app_win_switch.clone();
        glib::idle_add_local_once(move || {
            // Clear bell indicator on the newly selected workspace
            clear_workspace_bell(page_num);

            let Ok(aw) = aw_clone.try_borrow() else { return };
            if page_num >= aw.workspaces.len() { return; }

            let ws = &aw.workspaces[page_num];

            // Determine which surface should have focus
            let focused_sid = ws.focused_pane()
                .and_then(|pid| ws.panes.get(&pid))
                .and_then(|p| p.active_surface());

            // Refresh all surfaces and unfocus non-active ones
            for sid in ws.all_surface_ids() {
                if let Some(handle) = surfaces::get_handle(sid) {
                    unsafe {
                        crate::ghostty_sys::ghostty_surface_set_focus(
                            handle,
                            focused_sid == Some(sid),
                        );
                        crate::ghostty_sys::ghostty_surface_refresh(handle);
                    };
                }
            }

            // Set GTK focus on the active panel (terminal or browser)
            let focused_pane = ws.focused_pane()
                .and_then(|pid| ws.panes.get(&pid));
            if let Some(pane) = focused_pane {
                if let Some(panel) = pane.active_panel() {
                    panel.focus_gtk();
                }
            }
        });
    });

    // Keyboard shortcuts — built from settings with fallback to defaults
    let shortcut_ctrl = build_shortcut_controller();
    window.add_controller(shortcut_ctrl.clone());
    GTK_WINDOW.with(|w| *w.borrow_mut() = Some(window.clone()));
    SHORTCUT_CTRL.with(|c| *c.borrow_mut() = Some(shortcut_ctrl));

    // Register actions
    let action_group = gtk4::gio::SimpleActionGroup::new();

    let a = gtk4::gio::SimpleAction::new("new-workspace", None);
    a.connect_activate(|_, _| new_workspace());
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("close-workspace", None);
    a.connect_activate(|_, _| close_current_workspace());
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("next-workspace", None);
    a.connect_activate(|_, _| goto_workspace_relative(true));
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("prev-workspace", None);
    a.connect_activate(|_, _| goto_workspace_relative(false));
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("split-right", None);
    a.connect_activate(|_, _| split_focused(Orientation::Horizontal));
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("split-down", None);
    a.connect_activate(|_, _| split_focused(Orientation::Vertical));
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("next-split", None);
    a.connect_activate(|_, _| navigate_split(true));
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("prev-split", None);
    a.connect_activate(|_, _| navigate_split(false));
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("split-left", None);
    a.connect_activate(|_, _| navigate_direction(Direction::Left));
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("split-right-nav", None);
    a.connect_activate(|_, _| navigate_direction(Direction::Right));
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("split-up", None);
    a.connect_activate(|_, _| navigate_direction(Direction::Up));
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("split-down-nav", None);
    a.connect_activate(|_, _| navigate_direction(Direction::Down));
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("equalize-splits", None);
    a.connect_activate(|_, _| equalize_splits());
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("new-pane-tab", None);
    a.connect_activate(|_, _| new_pane_tab());
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("close-pane-tab", None);
    a.connect_activate(|_, _| close_pane_tab());
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("next-pane-tab", None);
    a.connect_activate(|_, _| cycle_pane_tab(true));
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("prev-pane-tab", None);
    a.connect_activate(|_, _| cycle_pane_tab(false));
    action_group.add_action(&a);

    // Ctrl+1-9 actions
    for i in 1..=9u32 {
        let a = gtk4::gio::SimpleAction::new(&format!("select-workspace-{i}"), None);
        let idx = i - 1;
        a.connect_activate(move |_, _| goto_workspace(idx as i32));
        action_group.add_action(&a);
    }

    let a = gtk4::gio::SimpleAction::new("toggle-sidebar", None);
    a.connect_activate(|_, _| toggle_sidebar());
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("open-browser", None);
    a.connect_activate(|_, _| split_focused_browser(Orientation::Horizontal, ""));
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("focus-address-bar", None);
    a.connect_activate(|_, _| focus_browser_address_bar());
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("browser-find", None);
    a.connect_activate(|_, _| toggle_browser_find());
    action_group.add_action(&a);

    let a = gtk4::gio::SimpleAction::new("open-settings", None);
    let win_clone = window.clone();
    a.connect_activate(move |_, _| {
        crate::settings_ui::show(&win_clone);
    });
    action_group.add_action(&a);

    window.insert_action_group("win", Some(&action_group));

    window.set_child(Some(&main_paned));

    // Try to restore session, otherwise create a default workspace
    if let Some(snapshot) = crate::session::load() {
        if !snapshot.workspaces.is_empty() {
            restore_session(&snapshot);
        } else {
            new_workspace();
        }
    } else {
        new_workspace();
    }
}

/// Rebuild the shortcut controller from current settings. Called after
/// shortcut changes in the Settings UI.
pub fn rebuild_shortcuts() {
    GTK_WINDOW.with(|w| {
        let binding = w.borrow();
        let Some(window) = binding.as_ref() else { return };

        // Remove old controller
        SHORTCUT_CTRL.with(|c| {
            if let Some(old) = c.borrow().as_ref() {
                window.remove_controller(old);
            }
        });

        // Build and add new controller
        let new_ctrl = build_shortcut_controller();
        window.add_controller(new_ctrl.clone());
        SHORTCUT_CTRL.with(|c| *c.borrow_mut() = Some(new_ctrl));
    });
}

/// Build a GtkShortcutController from settings (with fallback to defaults).
/// Also adds alternate keysym variants (brace/bracket, plus/equal, bar/backslash)
/// and Ctrl+1-9 for workspace selection.
fn build_shortcut_controller() -> gtk4::ShortcutController {
    let ctrl = gtk4::ShortcutController::new();
    ctrl.set_scope(gtk4::ShortcutScope::Managed);
    ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);

    let shortcuts = crate::settings::merged_shortcuts(&crate::settings::get());

    for (action, accel) in &shortcuts {
        ctrl.add_shortcut(make_shortcut(accel, &format!("win.{action}")));

        // Register alternate keysym variants for keys that produce different
        // keysyms when shifted (same pattern as the old hardcoded block)
        match action.as_str() {
            "next-split" => {
                ctrl.add_shortcut(make_shortcut("<Ctrl><Shift>braceright", "win.next-split"));
            }
            "prev-split" => {
                ctrl.add_shortcut(make_shortcut("<Ctrl><Shift>braceleft", "win.prev-split"));
            }
            "equalize-splits" => {
                ctrl.add_shortcut(make_shortcut("<Ctrl><Shift>equal", "win.equalize-splits"));
            }
            "toggle-sidebar" => {
                ctrl.add_shortcut(make_shortcut("<Ctrl><Shift>bar", "win.toggle-sidebar"));
            }
            "split-right" => {
                // Also register Ctrl+Shift+D as an alternate for split-right
                ctrl.add_shortcut(make_shortcut("<Ctrl><Shift>d", "win.split-right"));
            }
            _ => {}
        }
    }

    // Ctrl+1-9 for workspace selection (always registered)
    for i in 1..=9u32 {
        ctrl.add_shortcut(make_shortcut(
            &format!("<Ctrl>{i}"),
            &format!("win.select-workspace-{i}"),
        ));
    }

    ctrl
}

fn make_shortcut(accel: &str, action: &str) -> gtk4::Shortcut {
    gtk4::Shortcut::new(
        gtk4::ShortcutTrigger::parse_string(accel),
        Some(gtk4::NamedAction::new(action)),
    )
}

// ── Overlay helpers ─────────────────────────────────────────────────

/// Wrap a root_paned in a GtkOverlay with a drop-preview DrawingArea.
/// Returns (overlay, drop_preview, shared rect).
fn build_workspace_overlay(
    root_paned: &gtk4::Paned,
) -> (gtk4::Overlay, gtk4::DrawingArea, Rc<RefCell<Option<PreviewRect>>>) {
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(root_paned));

    let drop_preview = gtk4::DrawingArea::new();
    drop_preview.set_hexpand(true);
    drop_preview.set_vexpand(true);
    drop_preview.set_can_target(false); // pass-through for mouse/drop events
    drop_preview.set_sensitive(false); // don't intercept input
    drop_preview.set_can_focus(false);

    overlay.add_overlay(&drop_preview);
    overlay.set_measure_overlay(&drop_preview, false);

    let rect_state: Rc<RefCell<Option<PreviewRect>>> = Rc::new(RefCell::new(None));

    let rect_for_draw = rect_state.clone();
    drop_preview.set_draw_func(move |_da, cr, _w, _h| {
        let rect = *rect_for_draw.borrow();
        if let Some(r) = rect {
            // Fill — semi-transparent blue
            cr.set_source_rgba(0.39, 0.58, 0.93, 0.2);
            cr.rectangle(r.x, r.y, r.width, r.height);
            let _ = cr.fill();
            // Border — slightly more opaque
            cr.set_source_rgba(0.39, 0.58, 0.93, 0.5);
            cr.set_line_width(2.0);
            cr.rectangle(r.x + 1.0, r.y + 1.0, r.width - 2.0, r.height - 2.0);
            let _ = cr.stroke();
        }
    });

    (overlay, drop_preview, rect_state)
}

// ── Pane widget helpers ──────────────────────────────────────────────

/// Build the widget container for a pane: a vertical GtkBox with a tab strip
/// and a GtkStack for switching between tabs.
fn build_pane_widget(gl_area: &gtk4::GLArea, surface_id: crate::split::SurfaceId) -> PaneWidget {
    let container = gtk4::Box::new(gtk4::Orientation::Vertical, 0);

    // Tab strip — always visible (serves as drag handle for pane reordering)
    let tab_strip = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    tab_strip.add_css_class("pane-tab-strip");
    container.append(&tab_strip);

    let stack = gtk4::Stack::new();
    stack.set_vexpand(true);
    stack.set_hexpand(true);
    stack.set_transition_type(gtk4::StackTransitionType::None);
    stack.add_named(gl_area, Some(&format!("surface-{surface_id}")));
    stack.set_visible_child_name(&format!("surface-{surface_id}"));
    container.append(&stack);

    // Attach file drop target to the GLArea directly (not the stack,
    // which gets its drop targets cleared by tab_strip refresh).
    let drop_target = gtk4::DropTarget::new(gio::File::static_type(), gdk4::DragAction::COPY);
    let sid_for_drop = surface_id;
    drop_target.connect_drop(move |_target, value, _x, _y| {
        handle_file_drop(sid_for_drop, value)
    });
    gl_area.add_controller(drop_target);

    let pw = PaneWidget {
        container,
        stack,
        tab_strip,
    };

    // Populate initial tab button (will be replaced by refresh_pane_tab_strip
    // once the pane is registered, but this ensures the strip is never empty)
    let btn = gtk4::ToggleButton::with_label("Terminal");
    btn.set_active(true);
    btn.add_css_class("flat");
    pw.tab_strip.append(&btn);

    pw
}

/// Handle a file drop on a terminal pane.
fn handle_file_drop(surface_id: crate::split::SurfaceId, value: &glib::Value) -> bool {
    // Try to extract a GFile from the drop value.
    let file: gio::File = match value.get() {
        Ok(f) => f,
        Err(_) => return false,
    };
    let Some(path) = file.path() else { return false };
    let paths = vec![path];

    // Find the workspace that owns this surface.
    let remote_config = with_app_window(|aw| {
        for ws in &aw.workspaces {
            for pane in ws.panes.values() {
                for tab in &pane.tabs {
                    if let PanelKind::Terminal { surface_id: sid } = &tab.panel {
                        if *sid == surface_id {
                            return ws.remote_config.clone();
                        }
                    }
                }
            }
        }
        None
    });

    if let Some(config) = remote_config {
        // Remote workspace: SCP upload in background, then paste remote path.
        let sid = surface_id;
        std::thread::spawn(move || {
            match crate::remote::file_drop::upload_files(&config, &paths) {
                Ok(remote_paths) => {
                    let text = shell_escape_paths(&remote_paths);
                    glib::MainContext::default().invoke(move || {
                        send_text(sid, &text);
                    });
                }
                Err(e) => {
                    eprintln!("[file-drop] upload failed: {e}");
                }
            }
        });
    } else {
        // Local workspace: paste local path directly.
        let local_strs: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        let text = shell_escape_paths(&local_strs);
        send_text(surface_id, &text);
    }

    true
}

/// Shell-escape and space-join paths for pasting into a terminal.
fn shell_escape_paths(paths: &[String]) -> String {
    paths
        .iter()
        .map(|p| {
            if p.contains(|c: char| c.is_whitespace() || "\"'\\$`!#&|;(){}[]<>?*~".contains(c)) {
                format!("'{}'", p.replace('\'', "'\\''"))
            } else {
                p.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build the widget container for a browser pane.
fn build_browser_pane_widget(
    browser_widget: &gtk4::Box,
    browser_id: browser::BrowserPanelId,
) -> PaneWidget {
    let container = gtk4::Box::new(gtk4::Orientation::Vertical, 0);

    let tab_strip = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    tab_strip.add_css_class("pane-tab-strip");
    container.append(&tab_strip);

    let stack = gtk4::Stack::new();
    stack.set_vexpand(true);
    stack.set_hexpand(true);
    stack.set_transition_type(gtk4::StackTransitionType::None);
    stack.add_named(browser_widget, Some(&format!("browser-{browser_id}")));
    stack.set_visible_child_name(&format!("browser-{browser_id}"));
    container.append(&stack);

    let pw = PaneWidget {
        container,
        stack,
        tab_strip,
    };

    let btn = gtk4::ToggleButton::with_label("Browser");
    btn.set_active(true);
    btn.add_css_class("flat");
    pw.tab_strip.append(&btn);

    pw
}

/// Delegate to tab_strip module.
fn refresh_pane_tab_strip(
    pane_widget: &PaneWidget,
    pane: &workspace::Pane,
    drop_preview: &gtk4::DrawingArea,
    drop_preview_rect: &Rc<RefCell<Option<PreviewRect>>>,
) {
    tab_strip::refresh_pane_tab_strip(pane_widget, pane, drop_preview, drop_preview_rect);
}

/// Refresh a pane's tab strip by looking up workspace widgets and pane data.
/// Internalizes the triple-nested lookup that otherwise takes 7 lines.
fn refresh_pane_strip(aw: &AppWindow, ws_id: WorkspaceId, ws_idx: usize, pane_id: PaneId) {
    if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
        if let Some(pw) = widgets.pane_widgets.get(&pane_id) {
            if let Some(pane) = aw.workspaces[ws_idx].panes.get(&pane_id) {
                refresh_pane_tab_strip(pw, pane, &widgets.drop_preview, &widgets.drop_preview_rect);
            }
        }
    }
}

/// Rename a tab and refresh its pane's tab strip.
pub(crate) fn rename_tab(pane_id: PaneId, tab_idx: usize, new_name: &str) {
    with_app_window_mut(|aw| {
        // Find the workspace containing this pane
        let Some((ws_idx, ws_id)) = aw.workspaces.iter().enumerate().find_map(|(i, ws)| {
            if ws.panes.contains_key(&pane_id) { Some((i, ws.id)) } else { None }
        }) else { return };

        // Set the tab title
        if let Some(pane) = aw.workspaces[ws_idx].panes.get_mut(&pane_id) {
            if let Some(tab) = pane.tabs.get_mut(tab_idx) {
                tab.title = new_name.to_string();
            }
        }

        // Refresh tab strip
        refresh_pane_strip(&aw, ws_id, ws_idx, pane_id);
    });
}

/// Get the number of tabs in a pane. Used by tab_strip.rs during drag-and-drop.
pub(crate) fn pane_tab_count(pane_id: PaneId) -> usize {
    with_app_window(|aw| {
        aw.workspaces.iter()
            .find_map(|ws| ws.panes.get(&pane_id).map(|p| p.tabs.len()))
            .unwrap_or(0)
    })
}

// ── Workspace operations ─────────────────────────────────────────────

/// Create a new workspace.
pub fn new_workspace() {
    with_app_window_mut(|aw| {

        // Pre-allocate the surface ID so the stack child name matches
        let surface_id = surfaces::pre_allocate_id();
        let (gl_area, _sid_cell) = surface::create_with_id(
            aw.working_directory.as_deref(),
            aw.command.as_deref(),
            Some(surface_id),
        );

        let mut ws = Workspace::new();
        let ws_id = ws.id;
        let pane = Pane::new(surface_id);
        let pane_id = pane.id;
        ws.add_pane(pane);
        ws.split_tree = SplitTree::new_with_pane(pane_id);

        // Build widgets — surface_id is stable (pre-allocated)
        let pane_widget = build_pane_widget(&gl_area, surface_id);
        let paned = gtk4::Paned::new(gtk4::Orientation::Horizontal);
        configure_split_paned(&paned);
        paned.set_start_child(Some(&pane_widget.container));

        let title = format!("Workspace {}", aw.workspaces.len() + 1);
        ws.title = title.clone();

        let sidebar_row = sidebar::make_row(ws_id, &title);
        aw.sidebar_list.append(&sidebar_row);

        // Wrap in overlay for drop preview
        let (overlay, drop_preview, drop_preview_rect) = build_workspace_overlay(&paned);

        // Populate tab strip now that we have the pane
        if let Some(pane) = ws.panes.get(&pane_id) {
            refresh_pane_tab_strip(&pane_widget, pane, &drop_preview, &drop_preview_rect);
        }
        // Single pane starts as active
        pane_widget.tab_strip.add_css_class("pane-active");

        let mut pane_widgets = std::collections::HashMap::new();
        pane_widgets.insert(pane_id, pane_widget);

        let widgets = WorkspaceWidgets {
            root_paned: paned.clone(),
            overlay: overlay.clone(),
            drop_preview: drop_preview.clone(),
            drop_preview_rect: drop_preview_rect.clone(),
            sidebar_row: sidebar_row.clone(),
            pane_widgets,
            split_paneds: std::collections::HashMap::new(),
        };

        aw.notebook.append_page(&overlay, None::<&gtk4::Label>);
        aw.workspaces.push(ws);
        aw.workspace_widgets.insert(ws_id, widgets);

        // Select the new workspace
        let page_idx = aw.notebook.n_pages() - 1;
        aw.notebook.set_current_page(Some(page_idx));
        aw.sidebar_list.select_row(Some(&sidebar_row));

        // Focus the terminal after realize
        let gl_area_focus = gl_area.clone();
        glib::idle_add_local_once(move || {
            gl_area_focus.grab_focus();
            surfaces::remove_pending(surface_id);
        });

        update_tray_state(aw);
        crate::dbus::emit(crate::dbus::DbusSignal::WorkspaceCreated {
            id: ws_id,
            title,
        });
    });
}

/// Backwards-compatible alias for new_workspace.
pub fn new_tab() {
    new_workspace();
}

// ── Remote workspace functions ─────────────────────────────────────

/// Create a new workspace configured for a remote SSH connection.
/// Picks a relay port, generates credentials, launches the terminal with a
/// shell bootstrap command, and starts the background controller.
pub fn new_remote_workspace(mut config: crate::remote::RemoteConfiguration) -> Option<u32> {
    let mut result_id = None;
    with_app_window_mut(|aw| {
        // Pick relay port and generate credentials if not already set.
        if config.relay_port.is_none() {
            config.relay_port = crate::remote::config::pick_relay_port();
        }
        if config.relay_id.is_none() {
            config.relay_id = Some(crate::remote::config::generate_relay_id());
        }
        if config.relay_token.is_none() {
            config.relay_token = Some(crate::remote::config::generate_relay_token());
        }

        let relay_port = config.relay_port.unwrap_or(0);

        // Build the startup command: SSH with env vars for limux integration.
        let ssh_command = if relay_port > 0 {
            crate::remote::shell::generate_startup_command(&config, relay_port)
        } else {
            config.ssh_command()
        };

        // Pre-allocate surface ID and create a terminal with the SSH command.
        let surface_id = surfaces::pre_allocate_id();
        let (gl_area, _sid_cell) = surface::create_with_id(
            aw.working_directory.as_deref(),
            Some(&ssh_command),
            Some(surface_id),
        );

        let mut ws = Workspace::new();
        let ws_id = ws.id;
        ws.remote_config = Some(config.clone());
        ws.remote_state = crate::remote::RemoteConnectionState::Connecting;

        let pane = Pane::new(surface_id);
        let pane_id = pane.id;
        ws.add_pane(pane);
        ws.split_tree = SplitTree::new_with_pane(pane_id);

        let title = config.display_target();
        ws.title = title.clone();
        ws.custom_title = Some(title.clone());

        let sidebar_row = sidebar::make_row(ws_id, &title);
        aw.sidebar_list.append(&sidebar_row);

        let pane_widget = build_pane_widget(&gl_area, surface_id);
        let paned = gtk4::Paned::new(gtk4::Orientation::Horizontal);
        configure_split_paned(&paned);
        paned.set_start_child(Some(&pane_widget.container));

        let (overlay, drop_preview, drop_preview_rect) = build_workspace_overlay(&paned);

        if let Some(pane) = ws.panes.get(&pane_id) {
            refresh_pane_tab_strip(&pane_widget, pane, &drop_preview, &drop_preview_rect);
        }
        pane_widget.tab_strip.add_css_class("pane-active");

        let mut pane_widgets = std::collections::HashMap::new();
        pane_widgets.insert(pane_id, pane_widget);

        let widgets = WorkspaceWidgets {
            root_paned: paned.clone(),
            overlay: overlay.clone(),
            drop_preview: drop_preview.clone(),
            drop_preview_rect: drop_preview_rect.clone(),
            sidebar_row: sidebar_row.clone(),
            pane_widgets,
            split_paneds: std::collections::HashMap::new(),
        };

        aw.notebook.append_page(&overlay, None::<&gtk4::Label>);
        aw.workspaces.push(ws);
        aw.workspace_widgets.insert(ws_id, widgets);

        let page_idx = aw.notebook.n_pages() - 1;
        aw.notebook.set_current_page(Some(page_idx));
        aw.sidebar_list.select_row(Some(&sidebar_row));

        let gl_area_focus = gl_area.clone();
        glib::idle_add_local_once(move || {
            gl_area_focus.grab_focus();
            surfaces::remove_pending(surface_id);
        });

        update_tray_state(aw);
        crate::dbus::emit(crate::dbus::DbusSignal::WorkspaceCreated {
            id: ws_id,
            title,
        });

        // Start the remote bootstrap controller in the background.
        crate::remote::controller::connect(ws_id, config);

        result_id = Some(ws_id);
    });
    result_id
}

/// Disconnect the remote session for a workspace.
/// If no workspace ID is given, defaults to the focused workspace if remote,
/// otherwise the first remote workspace found.
pub fn disconnect_remote(ws_id: Option<u32>, clear: bool) {
    with_app_window_mut(|aw| {
        let ws_id = ws_id.or_else(|| {
            let focused = aw.notebook.current_page().and_then(|p| {
                aw.workspaces.get(p as usize)
                    .filter(|ws| ws.remote_config.is_some())
                    .map(|ws| ws.id)
            });
            focused.or_else(|| {
                aw.workspaces.iter()
                    .find(|ws| ws.remote_config.is_some())
                    .map(|ws| ws.id)
            })
        });
        let Some(ws_id) = ws_id else { return };

        crate::remote::controller::disconnect(ws_id);

        if let Some(ws) = aw.workspaces.iter_mut().find(|w| w.id == ws_id) {
            ws.remote_state = crate::remote::RemoteConnectionState::Disconnected;
            ws.remote_daemon_status = None;
            if clear {
                ws.remote_config = None;
            }
        }
        if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
            sidebar::update_row_remote_state(
                &widgets.sidebar_row,
                &crate::remote::RemoteConnectionState::Disconnected,
                None,
            );
        }
    });
}

/// Reconnect the remote session for a workspace using its existing config.
/// If no workspace ID is given, defaults to the focused workspace if remote,
/// otherwise the first remote workspace found.
pub fn reconnect_remote(ws_id: Option<u32>) {
    with_app_window_mut(|aw| {
        let ws_id = ws_id.or_else(|| {
            let focused = aw.notebook.current_page().and_then(|p| {
                aw.workspaces.get(p as usize)
                    .filter(|ws| ws.remote_config.is_some())
                    .map(|ws| ws.id)
            });
            focused.or_else(|| {
                aw.workspaces.iter()
                    .find(|ws| ws.remote_config.is_some())
                    .map(|ws| ws.id)
            })
        });
        let Some(ws_id) = ws_id else { return };

        let config = aw
            .workspaces
            .iter()
            .find(|w| w.id == ws_id)
            .and_then(|ws| ws.remote_config.clone());

        if let Some(config) = config {
            crate::remote::controller::reconnect(ws_id, config);
        }
    });
}

/// Get remote status info for a workspace as a JSON string.
/// If no workspace ID is given, defaults to the focused workspace if it's remote,
/// otherwise the first remote workspace found.
pub fn remote_status_info(ws_id: Option<u32>) -> Option<String> {
    let mut result = None;
    with_app_window(|aw| {
        let ws_id = ws_id.or_else(|| {
            // Try focused workspace first.
            let focused = aw.notebook.current_page().and_then(|p| {
                aw.workspaces.get(p as usize)
                    .filter(|ws| ws.remote_config.is_some())
                    .map(|ws| ws.id)
            });
            // Fall back to any remote workspace.
            focused.or_else(|| {
                aw.workspaces.iter()
                    .find(|ws| ws.remote_config.is_some())
                    .map(|ws| ws.id)
            })
        });
        let Some(ws_id) = ws_id else { return };

        if let Some(ws) = aw.workspaces.iter().find(|w| w.id == ws_id) {
            let info = serde_json::json!({
                "workspace_id": ws.id,
                "state": ws.remote_state.as_str(),
                "destination": ws.remote_config.as_ref().map(|c| c.display_target()),
                "daemon": ws.remote_daemon_status.as_ref().map(|s| serde_json::json!({
                    "state": format!("{:?}", s.state).to_lowercase(),
                    "detail": s.detail,
                    "version": s.version,
                    "name": s.name,
                    "capabilities": s.capabilities,
                    "remote_path": s.remote_path,
                })),
            });
            result = Some(info.to_string());
        }
    });
    result
}

/// Called from the remote controller background thread to push state changes.
pub fn update_remote_state(
    ws_id: u32,
    conn_state: crate::remote::RemoteConnectionState,
    daemon_status: Option<crate::remote::RemoteDaemonStatus>,
    detail: Option<String>,
) {
    with_app_window_mut(|aw| {
        if let Some(ws) = aw.workspaces.iter_mut().find(|w| w.id == ws_id) {
            ws.remote_state = conn_state;
            ws.remote_daemon_status = daemon_status;
        }
        if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
            sidebar::update_row_remote_state(&widgets.sidebar_row, &conn_state, detail.as_deref());
        }
    });
}

/// Called from the proxy broker callback to update the proxy endpoint for a workspace.
/// When a proxy tunnel becomes ready (or disconnects), this updates all browser panels
/// in the workspace to use (or stop using) the SOCKS5 proxy.
pub fn update_proxy_endpoint(
    ws_id: u32,
    endpoint: Option<crate::remote::ProxyEndpoint>,
) {
    with_app_window_mut(|aw| {
        let ws_idx = aw.workspaces.iter().position(|w| w.id == ws_id);
        let Some(ws_idx) = ws_idx else { return };

        // Store on workspace so new browser panels pick it up.
        aw.workspaces[ws_idx].proxy_endpoint = endpoint.clone();

        // Collect all browser IDs in this workspace.
        let browser_ids: Vec<_> = aw.workspaces[ws_idx]
            .panes
            .values()
            .flat_map(|pane| pane.tabs.iter())
            .filter_map(|tab| {
                if let crate::workspace::PanelKind::Browser { browser_id, .. } = &tab.panel {
                    Some(*browser_id)
                } else {
                    None
                }
            })
            .collect();

        // Update proxy on each existing browser panel.
        for bid in browser_ids {
            browser::set_proxy_endpoint(bid, endpoint.as_ref());
        }
    });
}

/// Close a specific surface by ID — called when a shell exits.
pub fn close_surface(surface_id: crate::split::SurfaceId) {
    WINDOW.with(|w| {
        let binding = w.borrow();
        let Some(app_win) = binding.as_ref() else { return };
        let mut aw = app_win.borrow_mut();

        // Find which workspace and pane contains this surface
        let Some((ws_idx, ws_id, pane_id)) = aw.find_workspace_with_surface(surface_id) else { return };
        let pane = aw.workspaces[ws_idx].panes.get(&pane_id);
        let pane_tab_count = pane.map_or(0, |p| p.tabs.len());
        let pane_count = aw.workspaces[ws_idx].panes.len();

        if pane_tab_count <= 1 && pane_count <= 1 {
            // Only surface in only pane — close the whole workspace
            surfaces::unregister(surface_id);
            remove_workspace_at(&mut aw, ws_idx);

            if aw.workspaces.is_empty() {
                drop(aw);
                drop(binding);
                crate::close_window();
            }
        } else if pane_tab_count <= 1 {
            // Only tab in this pane but other panes exist — remove pane from split
            surfaces::unregister(surface_id);
            remove_pane_from_split(&mut aw, ws_idx, ws_id, pane_id);
            if let Some(next_id) = aw.workspaces[ws_idx].focused_pane() {
                set_pane_active(&mut aw, ws_idx, next_id);
            }
        } else {
            // Multiple tabs in pane — just remove this tab
            surfaces::unregister(surface_id);

            // Mutate first, collect info for widget update
            let new_panel = if let Some(pane) = aw.workspaces[ws_idx].panes.get_mut(&pane_id) {
                pane.remove_tab(surface_id);
                pane.active_panel().cloned()
            } else {
                None
            };

            // Now update widgets and focus the new active tab
            if let Some(panel) = new_panel {
                if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                    if let Some(pw) = widgets.pane_widgets.get(&pane_id) {
                        pw.stack.set_visible_child_name(&panel.stack_child_name());
                        if let Some(pane) = aw.workspaces[ws_idx].panes.get(&pane_id) {
                            refresh_pane_tab_strip(pw, pane, &widgets.drop_preview, &widgets.drop_preview_rect);
                        }
                    }
                }
                panel.focus_gtk();
            }
        }
    });
}

/// Close a workspace by ID (from sidebar close button).
pub fn close_workspace(workspace_id: WorkspaceId) {
    let panels = with_app_window(|aw| {
        let idx = aw.workspaces.iter().position(|ws| ws.id == workspace_id)?;
        Some(aw.workspaces[idx].all_panels())
    });
    if let Some(panels) = panels {
        for panel in &panels {
            panel.request_close();
        }
    }
}

/// Close the current workspace.
fn close_current_workspace() {
    let panels = with_app_window(|aw| {
        let page = aw.notebook.current_page()?;
        let idx = page as usize;
        if idx >= aw.workspaces.len() { return None; }
        // Pinned workspaces resist keyboard close (Ctrl+Shift+W)
        if aw.workspaces[idx].pinned { return None; }
        Some(aw.workspaces[idx].all_panels())
    });
    if let Some(panels) = panels {
        for panel in &panels {
            panel.request_close();
        }
    }
}

/// Remove workspace at index (internal helper, assumes surfaces already cleaned up).
fn remove_workspace_at(aw: &mut AppWindow, idx: usize) {
    let ws_id = aw.workspaces[idx].id;

    // Stop the remote controller if this workspace is remote.
    if aw.workspaces[idx].remote_config.is_some() {
        crate::remote::controller::disconnect(ws_id);
    }

    aw.workspaces.remove(idx);

    if let Some(widgets) = aw.workspace_widgets.remove(&ws_id) {
        aw.notebook.remove_page(Some(
            aw.notebook.page_num(&widgets.overlay).unwrap_or(0),
        ));
        aw.sidebar_list.remove(&widgets.sidebar_row);
    }

    // Select the sidebar row for the now-current workspace
    if let Some(page) = aw.notebook.current_page() {
        let new_idx = page as usize;
        if let Some(ws) = aw.workspaces.get(new_idx) {
            if let Some(widgets) = aw.workspace_widgets.get(&ws.id) {
                aw.sidebar_list.select_row(Some(&widgets.sidebar_row));
            }
        }
    }

    update_tray_state(aw);
    crate::dbus::emit(crate::dbus::DbusSignal::WorkspaceClosed { id: ws_id });
}

/// Split the focused pane in the current workspace. The focused pane is
/// subdivided in the given orientation, creating a nested split. This enables
/// arbitrary layouts like 2x2 grids (split horizontal, then split each half vertical).
pub fn split_focused(orientation: Orientation) {
    with_app_window_mut(|aw| {

        let Some(page) = aw.notebook.current_page() else { return };
        let ws_idx = page as usize;
        if ws_idx >= aw.workspaces.len() { return; }

        let ws_id = aw.workspaces[ws_idx].id;
        let Some(focused_pane_id) = aw.workspaces[ws_idx].focused_pane() else { return };

        // Inherit working directory from the focused pane's active tab
        let wd = aw.workspaces[ws_idx]
            .panes
            .get(&focused_pane_id)
            .and_then(|p| p.tabs.get(p.selected_tab))
            .and_then(|t| t.working_directory.clone())
            .or_else(|| aw.working_directory.clone());

        // Create a new surface and pane
        let new_surface_id = surfaces::pre_allocate_id();
        let (new_gl_area, _sid_cell) = surface::create_with_id(
            wd.as_deref(),
            aw.command.as_deref(),
            Some(new_surface_id),
        );

        let new_pane = Pane::new_with_id(workspace::next_pane_id(), new_surface_id);
        let new_pane_id = new_pane.id;
        let pane_count_before = aw.workspaces[ws_idx].panes.len();
        aw.workspaces[ws_idx].add_pane(new_pane);

        let new_pane_widget = build_pane_widget(&new_gl_area, new_surface_id);

        // Populate tab strip now that we have the pane
        let dp = aw.workspace_widgets.get(&ws_id).map(|w| (w.drop_preview.clone(), w.drop_preview_rect.clone()));
        if let (Some(pane), Some((dp_w, dp_r))) = (aw.workspaces[ws_idx].panes.get(&new_pane_id), dp.as_ref()) {
            refresh_pane_tab_strip(&new_pane_widget, pane, dp_w, dp_r);
        }

        let gtk_orient: gtk4::Orientation = orientation.into();

        // Update split tree first to get the new split node ID
        let split_node_id = aw.workspaces[ws_idx].split_tree.split(
            focused_pane_id, new_pane_id, orientation,
        );

        if let Some(widgets) = aw.workspace_widgets.get_mut(&ws_id) {
            if pane_count_before == 1 {
                // First split: use the existing root_paned directly
                widgets.root_paned.set_orientation(gtk_orient);
                widgets.root_paned.set_end_child(Some(&new_pane_widget.container));

                // Track root_paned as the split node and sync its ratio
                if let Some(nid) = split_node_id {
                    widgets.split_paneds.insert(nid, widgets.root_paned.clone());
                    connect_paned_ratio_sync(&widgets.root_paned, ws_id, nid);
                }

                // Set 50/50 after layout
                set_paned_position_after_layout(&widgets.root_paned, 0.5);
            } else if let Some(focused_pw) = widgets.pane_widgets.get(&focused_pane_id) {
                // Nested split: replace the focused pane's container with a new
                // GtkPaned that holds [focused, new_pane].
                let container = focused_pw.container.clone();
                let container_widget = container.upcast_ref::<gtk4::Widget>();
                let parent = container_widget.parent()
                    .and_then(|p| p.downcast::<gtk4::Paned>().ok());

                if let Some(parent_paned) = parent {
                    let is_start = parent_paned.start_child()
                        .map_or(false, |c| c == *container_widget);

                    let new_paned = gtk4::Paned::new(gtk_orient);
                    configure_split_paned(&new_paned);

                    // Remove focused from parent, wrap in new paned with new pane
                    if is_start {
                        parent_paned.set_start_child(gtk4::Widget::NONE);
                        new_paned.set_start_child(Some(container_widget));
                        new_paned.set_end_child(Some(&new_pane_widget.container));
                        parent_paned.set_start_child(Some(&new_paned));
                    } else {
                        parent_paned.set_end_child(gtk4::Widget::NONE);
                        new_paned.set_start_child(Some(container_widget));
                        new_paned.set_end_child(Some(&new_pane_widget.container));
                        parent_paned.set_end_child(Some(&new_paned));
                    }

                    // Track the new paned and sync its ratio
                    if let Some(nid) = split_node_id {
                        widgets.split_paneds.insert(nid, new_paned.clone());
                        connect_paned_ratio_sync(&new_paned, ws_id, nid);
                    }

                    // Set 50/50 after layout
                    set_paned_position_after_layout(&new_paned, 0.5);
                }
            }

            widgets.pane_widgets.insert(new_pane_id, new_pane_widget);
        }

        // Focus new pane
        let new_gl_focus = new_gl_area.clone();
        glib::idle_add_local_once(move || {
            new_gl_focus.grab_focus();
            surfaces::remove_pending(new_surface_id);
        });
    });
}

/// Split the focused pane with a new browser pane.
pub fn split_focused_browser(orientation: Orientation, url: &str) {
    with_app_window_mut(|aw| {

        let Some(page) = aw.notebook.current_page() else { return };
        let ws_idx = page as usize;
        if ws_idx >= aw.workspaces.len() { return; }

        let ws_id = aw.workspaces[ws_idx].id;
        let Some(focused_pane_id) = aw.workspaces[ws_idx].focused_pane() else { return };

        // Create browser panel (with proxy if this is a remote workspace)
        let browser_id = browser::next_browser_id();
        let browser_widget = if let Some(ref ep) = aw.workspaces[ws_idx].proxy_endpoint {
            browser::create_with_proxy(browser_id, url, ep)
        } else {
            browser::create(browser_id, url)
        };

        let new_pane = Pane::new_browser_with_id(workspace::next_pane_id(), browser_id, url);
        let new_pane_id = new_pane.id;
        let pane_count_before = aw.workspaces[ws_idx].panes.len();
        aw.workspaces[ws_idx].add_pane(new_pane);

        let new_pane_widget = build_browser_pane_widget(&browser_widget, browser_id);

        let dp = aw.workspace_widgets.get(&ws_id).map(|w| (w.drop_preview.clone(), w.drop_preview_rect.clone()));
        if let (Some(pane), Some((dp_w, dp_r))) = (aw.workspaces[ws_idx].panes.get(&new_pane_id), dp.as_ref()) {
            refresh_pane_tab_strip(&new_pane_widget, pane, dp_w, dp_r);
        }

        let gtk_orient: gtk4::Orientation = orientation.into();

        let split_node_id = aw.workspaces[ws_idx].split_tree.split(
            focused_pane_id, new_pane_id, orientation,
        );

        if let Some(widgets) = aw.workspace_widgets.get_mut(&ws_id) {
            if pane_count_before == 1 {
                widgets.root_paned.set_orientation(gtk_orient);
                widgets.root_paned.set_end_child(Some(&new_pane_widget.container));

                if let Some(nid) = split_node_id {
                    widgets.split_paneds.insert(nid, widgets.root_paned.clone());
                    connect_paned_ratio_sync(&widgets.root_paned, ws_id, nid);
                }

                set_paned_position_after_layout(&widgets.root_paned, 0.5);
            } else if let Some(focused_pw) = widgets.pane_widgets.get(&focused_pane_id) {
                let container = focused_pw.container.clone();
                let container_widget = container.upcast_ref::<gtk4::Widget>();
                let parent = container_widget.parent()
                    .and_then(|p| p.downcast::<gtk4::Paned>().ok());

                if let Some(parent_paned) = parent {
                    let is_start = parent_paned.start_child()
                        .map_or(false, |c| c == *container_widget);

                    let new_paned = gtk4::Paned::new(gtk_orient);
                    configure_split_paned(&new_paned);

                    if is_start {
                        parent_paned.set_start_child(gtk4::Widget::NONE);
                        new_paned.set_start_child(Some(container_widget));
                        new_paned.set_end_child(Some(&new_pane_widget.container));
                        parent_paned.set_start_child(Some(&new_paned));
                    } else {
                        parent_paned.set_end_child(gtk4::Widget::NONE);
                        new_paned.set_start_child(Some(container_widget));
                        new_paned.set_end_child(Some(&new_pane_widget.container));
                        parent_paned.set_end_child(Some(&new_paned));
                    }

                    if let Some(nid) = split_node_id {
                        widgets.split_paneds.insert(nid, new_paned.clone());
                        connect_paned_ratio_sync(&new_paned, ws_id, nid);
                    }

                    set_paned_position_after_layout(&new_paned, 0.5);
                }
            }

            widgets.pane_widgets.insert(new_pane_id, new_pane_widget);
        }

        set_pane_active(aw, ws_idx, new_pane_id);

        // Focus the address bar so the user can type a URL
        let bid = browser_id;
        glib::idle_add_local_once(move || {
            browser::focus_address_bar(bid);
        });
    });
}

/// Called from browser.rs when a WebKitWebView's title changes.
pub fn on_browser_title_changed(browser_id: browser::BrowserPanelId, title: &str) {
    with_app_window_mut(|aw| {

        let Some((ws_idx, ws_id, pane_id)) = aw.find_workspace_with_browser(browser_id) else { return };

        // Mutate the workspace data
        if let Some(pane) = aw.workspaces[ws_idx].panes.get_mut(&pane_id) {
            pane.set_browser_tab_title(browser_id, title);
        }
        aw.workspaces[ws_idx].update_title_from_focused();

        // Update sidebar title and tab strip
        if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
            sidebar::update_row_title(&widgets.sidebar_row, aw.workspaces[ws_idx].display_title());
            if let Some(pw) = widgets.pane_widgets.get(&pane_id) {
                if let Some(pane) = aw.workspaces[ws_idx].panes.get(&pane_id) {
                    refresh_pane_tab_strip(pw, pane, &widgets.drop_preview, &widgets.drop_preview_rect);
                }
            }
        }
    });
}

/// Called from browser.rs when loading state changes.
pub fn on_browser_loading_changed(
    browser_id: browser::BrowserPanelId,
    is_loading: bool,
    can_back: bool,
    can_forward: bool,
) {
    with_app_window_mut(|aw| {

        for ws in aw.workspaces.iter_mut() {
            for pane in ws.panes.values_mut() {
                for tab in pane.tabs.iter_mut() {
                    if let PanelKind::Browser { browser_id: bid, .. } = &tab.panel {
                        if *bid == browser_id {
                            // Update any cached state if needed
                            let _ = (is_loading, can_back, can_forward);
                            return;
                        }
                    }
                }
            }
        }
    });
}

/// Focus the address bar of the browser in the currently focused pane.
fn focus_browser_address_bar() {
    with_app_window(|aw| {
        let Some(page) = aw.notebook.current_page() else { return };
        let ws_idx = page as usize;
        if ws_idx >= aw.workspaces.len() { return; }

        let ws = &aw.workspaces[ws_idx];
        if let Some(pane_id) = ws.focused_pane() {
            if let Some(pane) = ws.panes.get(&pane_id) {
                if let Some(browser_id) = pane.active_browser() {
                    browser::focus_address_bar(browser_id);
                }
            }
        }
    });
}

/// Toggle the find bar in the currently focused browser pane.
fn toggle_browser_find() {
    with_app_window(|aw| {
        let Some(page) = aw.notebook.current_page() else { return };
        let ws_idx = page as usize;
        if ws_idx >= aw.workspaces.len() { return; }

        let ws = &aw.workspaces[ws_idx];
        if let Some(pane_id) = ws.focused_pane() {
            if let Some(pane) = ws.panes.get(&pane_id) {
                if let Some(browser_id) = pane.active_browser() {
                    browser::toggle_find_bar(browser_id);
                }
            }
        }
    });
}

/// Close a browser panel by ID (called when a browser pane is explicitly closed).
pub fn close_browser(browser_id: browser::BrowserPanelId) {
    WINDOW.with(|w| {
        let binding = w.borrow();
        let Some(app_win) = binding.as_ref() else { return };
        let mut aw = app_win.borrow_mut();

        // Find which workspace and pane contain this browser
        let mut target = None;
        for (ws_idx, ws) in aw.workspaces.iter().enumerate() {
            if let Some(pane_id) = ws.pane_for_browser(browser_id) {
                target = Some((ws_idx, pane_id));
                break;
            }
        }
        let Some((ws_idx, pane_id)) = target else { return };

        let ws_id = aw.workspaces[ws_idx].id;
        let pane = aw.workspaces[ws_idx].panes.get(&pane_id);
        let pane_tab_count = pane.map_or(0, |p| p.tabs.len());
        let pane_count = aw.workspaces[ws_idx].panes.len();

        browser::unregister(browser_id);

        if pane_tab_count <= 1 && pane_count <= 1 {
            // Only tab in only pane — close the workspace
            remove_workspace_at(&mut aw, ws_idx);
            if aw.workspaces.is_empty() {
                drop(aw);
                drop(binding);
                crate::close_window();
            }
        } else if pane_tab_count <= 1 {
            // Only tab in this pane but other panes exist — remove pane from split
            remove_pane_from_split(&mut aw, ws_idx, ws_id, pane_id);

            // Focus another pane
            if let Some(next_id) = aw.workspaces[ws_idx].focused_pane() {
                set_pane_active(&mut aw, ws_idx, next_id);
            }
        } else {
            // Multiple tabs in pane — just remove this browser tab
            if let Some(pane) = aw.workspaces[ws_idx].panes.get_mut(&pane_id) {
                pane.remove_browser_tab(browser_id);
            }
            // Update visible child, tab strip, and focus the new active tab
            let new_panel = aw.workspaces[ws_idx].panes.get(&pane_id)
                .and_then(|p| p.active_panel().cloned());
            if let Some(panel) = new_panel {
                if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                    if let Some(pw) = widgets.pane_widgets.get(&pane_id) {
                        pw.stack.set_visible_child_name(&panel.stack_child_name());
                        if let Some(pane) = aw.workspaces[ws_idx].panes.get(&pane_id) {
                            refresh_pane_tab_strip(pw, pane, &widgets.drop_preview, &widgets.drop_preview_rect);
                        }
                    }
                }
                panel.focus_gtk();
            }
        }
    });
}

/// Remove a pane's widget from the GtkPaned split tree and data model.
/// Shared by `close_surface` and `close_browser`.
fn remove_pane_from_split(
    aw: &mut AppWindow,
    ws_idx: usize,
    ws_id: WorkspaceId,
    pane_id: PaneId,
) {
    if let Some(widgets) = aw.workspace_widgets.get_mut(&ws_id) {
        if let Some(pw) = widgets.pane_widgets.remove(&pane_id) {
            let container_widget = pw.container.upcast_ref::<gtk4::Widget>();
            let parent_widget = container_widget.parent();
            let parent_paned = parent_widget
                .as_ref()
                .and_then(|p| p.downcast_ref::<gtk4::Paned>());

            if let Some(parent) = parent_paned {
                let is_start = parent
                    .start_child()
                    .map_or(false, |c| c == *container_widget);

                // Get the sibling (the other child)
                let sibling = if is_start {
                    parent.end_child()
                } else {
                    parent.start_child()
                };

                // Remove the closed pane's container
                if is_start {
                    parent.set_start_child(gtk4::Widget::NONE);
                } else {
                    parent.set_end_child(gtk4::Widget::NONE);
                }

                let is_root = parent == &widgets.root_paned;

                if is_root {
                    // Root paned: leave the sibling in place
                    if is_start {
                        if let Some(ref sib) = sibling {
                            parent.set_end_child(gtk4::Widget::NONE);
                            parent.set_start_child(Some(sib));
                        }
                    }
                } else if let Some(sib) = sibling {
                    // Nested paned: promote sibling up to the grandparent
                    let grandparent_widget = parent.parent();
                    let grandparent = grandparent_widget
                        .as_ref()
                        .and_then(|gp| gp.downcast_ref::<gtk4::Paned>());

                    if let Some(gp) = grandparent {
                        let parent_is_start = gp
                            .start_child()
                            .map_or(false, |c| c == *parent.upcast_ref::<gtk4::Widget>());

                        // Detach sibling from parent, then detach parent from grandparent
                        if parent.start_child().as_ref() == Some(&sib) {
                            parent.set_start_child(gtk4::Widget::NONE);
                        } else {
                            parent.set_end_child(gtk4::Widget::NONE);
                        }

                        if parent_is_start {
                            gp.set_start_child(gtk4::Widget::NONE);
                            gp.set_start_child(Some(&sib));
                        } else {
                            gp.set_end_child(gtk4::Widget::NONE);
                            gp.set_end_child(Some(&sib));
                        }
                    }

                    // Remove the collapsed paned from split_paneds
                    let collapsed_paned = parent.clone();
                    widgets.split_paneds.retain(|_, p| p != &collapsed_paned);
                }
            }
        }
    }

    // Update split tree and remove pane from data model
    aw.workspaces[ws_idx].split_tree.remove(pane_id);
    aw.workspaces[ws_idx].remove_pane(pane_id);
}

/// Connect a GtkPaned's position changes to update the SplitTree ratio.
fn connect_paned_ratio_sync(paned: &gtk4::Paned, ws_id: WorkspaceId, node_id: NodeId) {
    paned.connect_notify(Some("position"), move |paned, _| {
        let pos = paned.position();
        let total = paned_total_size(paned);
        if total <= 0 { return; }
        let ratio = (pos as f64 / total as f64).clamp(0.05, 0.95);
        with_app_window_mut(|aw| {
            if let Some(ws) = aw.workspaces.iter_mut().find(|ws| ws.id == ws_id) {
                ws.split_tree.set_ratio(node_id, ratio);
            }
        });
    });
}

/// Set a resize cursor on the paned's separator child widget.
/// Set a paned's split position after GTK has allocated layout.
/// Defers to an idle callback so the paned has a valid size.
fn set_paned_position_after_layout(paned: &gtk4::Paned, ratio: f64) {
    let paned_clone = paned.clone();
    glib::idle_add_local_once(move || {
        let total = paned_total_size(&paned_clone);
        if total > 0 {
            paned_clone.set_position((total as f64 * ratio) as i32);
        }
    });
}

/// Total size of a paned along its split axis.
fn paned_total_size(paned: &gtk4::Paned) -> i32 {
    match paned.orientation() {
        gtk4::Orientation::Horizontal => paned.width(),
        _ => paned.height(),
    }
}

/// Standard configuration for split panes: no shrinking, both sides resize,
/// and a resize cursor on the separator.
fn configure_split_paned(paned: &gtk4::Paned) {
    paned.set_shrink_start_child(false);
    paned.set_shrink_end_child(false);
    paned.set_resize_start_child(true);
    paned.set_resize_end_child(true);
    set_paned_separator_cursor(paned);
}

fn set_paned_separator_cursor(paned: &gtk4::Paned) {
    // The separator is a direct child of the paned, between start and end children.
    let cursor_name = match paned.orientation() {
        gtk4::Orientation::Horizontal => "col-resize",
        _ => "row-resize",
    };
    let cursor = gdk4::Cursor::from_name(cursor_name, None);
    // Walk direct children to find the separator widget
    let mut child = paned.first_child();
    while let Some(widget) = child {
        // The separator is the widget that isn't the start or end child
        if paned.start_child().as_ref() != Some(&widget)
            && paned.end_child().as_ref() != Some(&widget)
        {
            widget.set_cursor(cursor.as_ref());
            break;
        }
        child = widget.next_sibling();
    }
}

/// Walk the GtkPaned chain to find the deepest one (follow end_child).
fn find_deepest_paned(paned: &gtk4::Paned) -> gtk4::Paned {
    if let Some(end) = paned.end_child() {
        if let Some(nested) = end.downcast_ref::<gtk4::Paned>() {
            return find_deepest_paned(nested);
        }
    }
    paned.clone()
}

/// Collect all GtkPaneds in the chain (root, then nested end_children).
fn collect_paned_chain(root: &gtk4::Paned) -> Vec<gtk4::Paned> {
    let mut chain = vec![root.clone()];
    let mut current = root.clone();
    while let Some(end) = current.end_child() {
        if let Some(nested) = end.downcast_ref::<gtk4::Paned>() {
            chain.push(nested.clone());
            current = nested.clone();
        } else {
            break;
        }
    }
    chain
}

/// Set positions on a chain of GtkPaneds for equal pane distribution.
/// For N panes: paneds[0] position = total/N, paneds[1] = remaining/(N-1), etc.
fn equalize_paned_chain(paneds: &[gtk4::Paned], pane_count: usize) {
    let mut remaining = pane_count;
    for paned in paneds {
        if remaining <= 1 { break; }
        let total = paned_total_size(paned);
        if total > 0 {
            paned.set_position(total / remaining as i32);
        }
        remaining -= 1;
    }
}

/// Navigate to the next/previous split pane.
pub fn navigate_split(forward: bool) {
    with_app_window_mut(|aw| {
        let Some(page) = aw.notebook.current_page() else { return };
        let ws_idx = page as usize;
        if ws_idx >= aw.workspaces.len() { return; }

        let Some(new_pane_id) = aw.workspaces[ws_idx].split_tree.navigate(forward) else {
            return;
        };

        // Focus the active panel in the new pane
        if let Some(pane) = aw.workspaces[ws_idx].panes.get(&new_pane_id) {
            if let Some(panel) = pane.active_panel() {
                panel.focus_gtk();
            }
        }
    });
}

/// Navigate to the pane in the given direction.
fn navigate_direction(direction: Direction) {
    with_app_window_mut(|aw| {

        let Some(page) = aw.notebook.current_page() else { return };
        let ws_idx = page as usize;
        if ws_idx >= aw.workspaces.len() { return; }

        let Some(new_pane_id) = aw.workspaces[ws_idx]
            .split_tree
            .navigate_directional(direction)
        else {
            return;
        };

        set_pane_active(aw, ws_idx, new_pane_id);
    });
}

/// Equalize all split dividers in the current workspace.
fn equalize_splits() {
    with_app_window_mut(|aw| {

        let Some(page) = aw.notebook.current_page() else { return };
        let ws_idx = page as usize;
        if ws_idx >= aw.workspaces.len() { return; }

        let ws_id = aw.workspaces[ws_idx].id;
        aw.workspaces[ws_idx].split_tree.equalize();

        if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
            let root = widgets.root_paned.clone();
            // Collect all paneds recursively (not just a chain)
            let all_paneds = collect_all_paneds(&root);

            glib::idle_add_local_once(move || {
                // First pass: set root
                equalize_single_paned(&root);
                // Second pass: set nested paneds after root settles
                let nested = all_paneds;
                glib::idle_add_local_once(move || {
                    for paned in &nested {
                        equalize_single_paned(paned);
                    }
                });
            });
        }
    });
}

/// Set a single paned to 50/50.
fn equalize_single_paned(paned: &gtk4::Paned) {
    if paned.end_child().is_none() { return; }
    let total = paned_total_size(paned);
    if total > 0 {
        paned.set_position(total / 2);
    }
}

/// Recursively collect all GtkPaneds in the widget tree (depth-first).
fn collect_all_paneds(paned: &gtk4::Paned) -> Vec<gtk4::Paned> {
    let mut result = Vec::new();
    if let Some(start) = paned.start_child() {
        if let Some(nested) = start.downcast_ref::<gtk4::Paned>() {
            result.push(nested.clone());
            result.extend(collect_all_paneds(nested));
        }
    }
    if let Some(end) = paned.end_child() {
        if let Some(nested) = end.downcast_ref::<gtk4::Paned>() {
            result.push(nested.clone());
            result.extend(collect_all_paneds(nested));
        }
    }
    result
}

/// Move a pane to be adjacent to another pane within the same workspace.
/// Rebuilds the GtkPaned widget tree to match the new SplitTree.
/// Set a pane as the focused pane and update the pane-active CSS class.
fn set_pane_active(aw: &mut AppWindow, ws_idx: usize, pane_id: PaneId) {
    let ws_id = aw.workspaces[ws_idx].id;
    aw.workspaces[ws_idx].set_focused_pane(pane_id);
    if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
        for (&pid, pw) in &widgets.pane_widgets {
            if pid == pane_id {
                pw.tab_strip.add_css_class("pane-active");
            } else {
                pw.tab_strip.remove_css_class("pane-active");
            }
        }
    }
    // Focus the active panel in this pane
    if let Some(pane) = aw.workspaces[ws_idx].panes.get(&pane_id) {
        if let Some(panel) = pane.active_panel() {
            panel.focus_gtk();
        }
    }
}

pub(crate) fn move_pane(source_pane_id: PaneId, target_pane_id: PaneId, before: bool, orientation: Orientation) {
    with_app_window_mut(|aw| {

        let Some(page) = aw.notebook.current_page() else { return };
        let ws_idx = page as usize;
        if ws_idx >= aw.workspaces.len() { return; }

        let ws_id = aw.workspaces[ws_idx].id;

        // Verify both panes are in this workspace
        if aw.workspaces[ws_idx].panes.get(&source_pane_id).is_none()
            || aw.workspaces[ws_idx].panes.get(&target_pane_id).is_none()
        {
            return;
        }

        // Update the split tree
        if !aw.workspaces[ws_idx]
            .split_tree
            .move_pane_adjacent(source_pane_id, target_pane_id, before, orientation)
        {
            return;
        }

        // Rebuild the widget tree. Use index-based access pattern to
        // satisfy the borrow checker (split borrow via temporary).
        rebuild_workspace_split_widgets(aw, ws_idx);
        set_pane_active(aw, ws_idx, source_pane_id);
    });
}

/// Extract a tab from a pane into a new pane via split.
/// Used when dragging a tab within the same pane to a drop zone edge.
pub(crate) fn split_tab_to_pane(
    source_pane_id: PaneId,
    tab_idx: usize,
    orientation: Orientation,
    before: bool,
) {
    with_app_window_mut(|aw| {
        let Some(page) = aw.notebook.current_page() else { return };
        let ws_idx = page as usize;
        if ws_idx >= aw.workspaces.len() { return; }

        let ws_id = aw.workspaces[ws_idx].id;

        // Must have > 1 tab to extract one
        let tab_count = aw.workspaces[ws_idx]
            .panes
            .get(&source_pane_id)
            .map(|p| p.tabs.len())
            .unwrap_or(0);
        if tab_count < 2 {
            return;
        }

        // Remove the tab from the source pane
        let tab = {
            let Some(pane) = aw.workspaces[ws_idx].panes.get_mut(&source_pane_id) else { return };
            if tab_idx >= pane.tabs.len() { return; }
            let tab = pane.tabs.remove(tab_idx);
            if pane.selected_tab >= pane.tabs.len() && !pane.tabs.is_empty() {
                pane.selected_tab = pane.tabs.len() - 1;
            }
            tab
        };

        // Create a new pane with the extracted tab
        let new_pane_id = workspace::next_pane_id();
        let mut new_pane = match &tab.panel {
            PanelKind::Terminal { surface_id } => Pane::new_with_id(new_pane_id, *surface_id),
            PanelKind::Browser { browser_id, url } => {
                Pane::new_browser_with_id(new_pane_id, *browser_id, url)
            }
        };
        // Preserve tab metadata
        if let Some(new_tab) = new_pane.tabs.first_mut() {
            new_tab.title = tab.title.clone();
            new_tab.working_directory = tab.working_directory.clone();
        }

        let pane_count_before = aw.workspaces[ws_idx].panes.len();
        aw.workspaces[ws_idx].add_pane(new_pane);

        // Build widget for new pane — reuse existing GLArea/browser widget from the stack
        let new_pane_widget = match &tab.panel {
            PanelKind::Terminal { surface_id } => {
                if let Some(gl_area) = surfaces::get_gl_area(*surface_id) {
                    // Remove from old pane's stack
                    if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                        if let Some(old_pw) = widgets.pane_widgets.get(&source_pane_id) {
                            old_pw.stack.remove(&gl_area);
                        }
                    }
                    build_pane_widget(&gl_area, *surface_id)
                } else {
                    return;
                }
            }
            PanelKind::Browser { browser_id, .. } => {
                if let Some(bw) = browser::get_widget(*browser_id)
                    .and_then(|w| w.downcast::<gtk4::Box>().ok())
                {
                    if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                        if let Some(old_pw) = widgets.pane_widgets.get(&source_pane_id) {
                            old_pw.stack.remove(&bw);
                        }
                    }
                    build_browser_pane_widget(&bw, *browser_id)
                } else {
                    return;
                }
            }
        };

        // Refresh source pane tab strip (tab was removed)
        let dp = aw.workspace_widgets.get(&ws_id)
            .map(|w| (w.drop_preview.clone(), w.drop_preview_rect.clone()));
        if let Some((dp_w, dp_r)) = dp.as_ref() {
            // Also set visible child on source pane to its new selected tab
            if let Some(pane) = aw.workspaces[ws_idx].panes.get(&source_pane_id) {
                if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                    if let Some(pw) = widgets.pane_widgets.get(&source_pane_id) {
                        if let Some(active_tab) = pane.tabs.get(pane.selected_tab) {
                            pw.stack.set_visible_child_name(&active_tab.panel.stack_child_name());
                        }
                        refresh_pane_tab_strip(pw, pane, dp_w, dp_r);
                    }
                }
            }
            // Refresh new pane tab strip
            if let Some(pane) = aw.workspaces[ws_idx].panes.get(&new_pane_id) {
                refresh_pane_tab_strip(&new_pane_widget, pane, dp_w, dp_r);
            }
        }

        // Update split tree: split source_pane, placing new_pane according to `before`
        let target_pane_id = source_pane_id;
        let split_node_id = if before {
            aw.workspaces[ws_idx].split_tree.split_before(
                target_pane_id, new_pane_id, orientation,
            )
        } else {
            aw.workspaces[ws_idx].split_tree.split(
                target_pane_id, new_pane_id, orientation,
            )
        };

        // Insert widget into the split layout
        let gtk_orient: gtk4::Orientation = orientation.into();

        // Pre-clone tree snapshot for the multi-pane rebuild path (avoids borrow conflict)
        let tree_snapshot = if pane_count_before > 1 {
            clone_split_tree_structure(
                &aw.workspaces[ws_idx].split_tree,
                &aw.workspaces[ws_idx].panes,
            )
        } else {
            None
        };

        if let Some(widgets) = aw.workspace_widgets.get_mut(&ws_id) {
            if pane_count_before == 1 {
                // First split: use existing root_paned
                widgets.root_paned.set_orientation(gtk_orient);
                if before {
                    let source_container = widgets.pane_widgets.get(&source_pane_id)
                        .map(|pw| pw.container.clone());
                    widgets.root_paned.grab_focus();
                    widgets.root_paned.set_start_child(gtk4::Widget::NONE);
                    widgets.root_paned.set_end_child(gtk4::Widget::NONE);
                    widgets.root_paned.set_start_child(Some(&new_pane_widget.container));
                    if let Some(ref sc) = source_container {
                        widgets.root_paned.set_end_child(Some(sc));
                    }
                } else {
                    widgets.root_paned.set_end_child(Some(&new_pane_widget.container));
                }

                if let Some(nid) = split_node_id {
                    widgets.split_paneds.insert(nid, widgets.root_paned.clone());
                    connect_paned_ratio_sync(&widgets.root_paned, ws_id, nid);
                }
                set_paned_separator_cursor(&widgets.root_paned);

                set_paned_position_after_layout(&widgets.root_paned, 0.5);
                widgets.pane_widgets.insert(new_pane_id, new_pane_widget);
            } else {
                // Multiple panes: rebuild entire widget tree from split tree
                widgets.pane_widgets.insert(new_pane_id, new_pane_widget);
                rebuild_split_widgets_from_snapshot(&tree_snapshot, widgets, ws_id);
            }
        }
        set_pane_active(aw, ws_idx, new_pane_id);
    });
}

/// Extract a tab from a source pane and split it adjacent to a *different* target pane.
pub(crate) fn split_tab_to_pane_target(
    source_pane_id: PaneId,
    tab_idx: usize,
    target_pane_id: PaneId,
    orientation: Orientation,
    before: bool,
) {
    with_app_window_mut(|aw| {

        let Some(page) = aw.notebook.current_page() else { return };
        let ws_idx = page as usize;
        if ws_idx >= aw.workspaces.len() { return; }

        let ws_id = aw.workspaces[ws_idx].id;

        // Remove the tab from the source pane
        let tab = {
            let Some(pane) = aw.workspaces[ws_idx].panes.get_mut(&source_pane_id) else { return };
            if tab_idx >= pane.tabs.len() { return; }
            let tab = pane.tabs.remove(tab_idx);
            if pane.selected_tab >= pane.tabs.len() && !pane.tabs.is_empty() {
                pane.selected_tab = pane.tabs.len() - 1;
            }
            tab
        };

        // Build new pane from the extracted tab
        let new_pane_id = workspace::next_pane_id();
        let mut new_pane = match &tab.panel {
            PanelKind::Terminal { surface_id } => Pane::new_with_id(new_pane_id, *surface_id),
            PanelKind::Browser { browser_id, url } => {
                Pane::new_browser_with_id(new_pane_id, *browser_id, url)
            }
        };
        if let Some(new_tab) = new_pane.tabs.first_mut() {
            new_tab.title = tab.title.clone();
            new_tab.working_directory = tab.working_directory.clone();
        }
        aw.workspaces[ws_idx].add_pane(new_pane);

        // Move the widget from source stack to new pane widget
        let new_pane_widget = match &tab.panel {
            PanelKind::Terminal { surface_id } => {
                if let Some(gl_area) = surfaces::get_gl_area(*surface_id) {
                    if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                        if let Some(old_pw) = widgets.pane_widgets.get(&source_pane_id) {
                            old_pw.stack.remove(&gl_area);
                        }
                    }
                    build_pane_widget(&gl_area, *surface_id)
                } else {
                    return;
                }
            }
            PanelKind::Browser { browser_id, .. } => {
                if let Some(bw) = browser::get_widget(*browser_id)
                    .and_then(|w| w.downcast::<gtk4::Box>().ok())
                {
                    if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                        if let Some(old_pw) = widgets.pane_widgets.get(&source_pane_id) {
                            old_pw.stack.remove(&bw);
                        }
                    }
                    build_browser_pane_widget(&bw, *browser_id)
                } else {
                    return;
                }
            }
        };

        // Refresh tab strips
        let dp = aw.workspace_widgets.get(&ws_id)
            .map(|w| (w.drop_preview.clone(), w.drop_preview_rect.clone()));
        if let Some((dp_w, dp_r)) = dp.as_ref() {
            // Source pane: update visible child and tab strip
            if let Some(pane) = aw.workspaces[ws_idx].panes.get(&source_pane_id) {
                if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                    if let Some(pw) = widgets.pane_widgets.get(&source_pane_id) {
                        if let Some(active_tab) = pane.tabs.get(pane.selected_tab) {
                            pw.stack.set_visible_child_name(&active_tab.panel.stack_child_name());
                        }
                        refresh_pane_tab_strip(pw, pane, dp_w, dp_r);
                    }
                }
            }
            // New pane tab strip
            if let Some(pane) = aw.workspaces[ws_idx].panes.get(&new_pane_id) {
                refresh_pane_tab_strip(&new_pane_widget, pane, dp_w, dp_r);
            }
        }

        // If source pane is now empty, remove it from the split tree
        let source_empty = aw.workspaces[ws_idx]
            .panes.get(&source_pane_id)
            .map(|p| p.tabs.is_empty())
            .unwrap_or(false);
        if source_empty {
            aw.workspaces[ws_idx].split_tree.remove(source_pane_id);
            aw.workspaces[ws_idx].panes.remove(&source_pane_id);
            if let Some(widgets) = aw.workspace_widgets.get_mut(&ws_id) {
                widgets.pane_widgets.remove(&source_pane_id);
            }
        }

        // Split the target pane to place the new pane adjacent
        let split_node_id = if before {
            aw.workspaces[ws_idx].split_tree.split_before(
                target_pane_id, new_pane_id, orientation,
            )
        } else {
            aw.workspaces[ws_idx].split_tree.split(
                target_pane_id, new_pane_id, orientation,
            )
        };
        let _ = split_node_id;

        // Insert new pane widget and rebuild the entire split widget tree
        let tree_snapshot = clone_split_tree_structure(
            &aw.workspaces[ws_idx].split_tree,
            &aw.workspaces[ws_idx].panes,
        );
        if let Some(widgets) = aw.workspace_widgets.get_mut(&ws_id) {
            widgets.pane_widgets.insert(new_pane_id, new_pane_widget);
            rebuild_split_widgets_from_snapshot(&tree_snapshot, widgets, ws_id);
        }
        set_pane_active(aw, ws_idx, new_pane_id);
    });
}

/// Move a tab from one pane into another existing pane (merge).
pub(crate) fn move_tab_to_pane(source_pane_id: PaneId, tab_idx: usize, target_pane_id: PaneId) {
    with_app_window_mut(|aw| {

        let Some(page) = aw.notebook.current_page() else { return };
        let ws_idx = page as usize;
        if ws_idx >= aw.workspaces.len() { return; }

        let ws_id = aw.workspaces[ws_idx].id;

        // Remove the tab from the source pane
        let tab = {
            let Some(pane) = aw.workspaces[ws_idx].panes.get_mut(&source_pane_id) else { return };
            if tab_idx >= pane.tabs.len() { return; }
            let tab = pane.tabs.remove(tab_idx);
            if pane.selected_tab >= pane.tabs.len() && !pane.tabs.is_empty() {
                pane.selected_tab = pane.tabs.len() - 1;
            }
            tab
        };

        // Move the widget from source stack to target stack
        let widget_name = tab.panel.stack_child_name();

        if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
            // Remove widget from source stack
            if let Some(src_pw) = widgets.pane_widgets.get(&source_pane_id) {
                if let Some(child) = src_pw.stack.child_by_name(&widget_name) {
                    src_pw.stack.remove(&child);
                    // Add to target stack
                    if let Some(tgt_pw) = widgets.pane_widgets.get(&target_pane_id) {
                        tgt_pw.stack.add_named(&child, Some(&widget_name));
                        tgt_pw.stack.set_visible_child_name(&widget_name);
                    }
                }
            }
        }

        // Add tab to target pane data model
        {
            let Some(target_pane) = aw.workspaces[ws_idx].panes.get_mut(&target_pane_id) else { return };
            target_pane.tabs.push(tab);
            target_pane.selected_tab = target_pane.tabs.len() - 1;
        }

        // If source pane is now empty, remove it
        let source_empty = aw.workspaces[ws_idx]
            .panes
            .get(&source_pane_id)
            .map(|p| p.tabs.is_empty())
            .unwrap_or(false);

        let dp = aw.workspace_widgets.get(&ws_id)
            .map(|w| (w.drop_preview.clone(), w.drop_preview_rect.clone()));

        if source_empty {
            // Remove from split tree and rebuild
            aw.workspaces[ws_idx].split_tree.remove(source_pane_id);
            aw.workspaces[ws_idx].panes.remove(&source_pane_id);
            if let Some(widgets) = aw.workspace_widgets.get_mut(&ws_id) {
                widgets.pane_widgets.remove(&source_pane_id);
            }
            rebuild_workspace_split_widgets(aw, ws_idx);
        } else if let Some((dp_w, dp_r)) = dp.as_ref() {
            // Refresh source pane tab strip
            if let Some(pane) = aw.workspaces[ws_idx].panes.get(&source_pane_id) {
                if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                    if let Some(pw) = widgets.pane_widgets.get(&source_pane_id) {
                        if let Some(active) = pane.tabs.get(pane.selected_tab) {
                            pw.stack.set_visible_child_name(&active.panel.stack_child_name());
                        }
                        refresh_pane_tab_strip(pw, pane, dp_w, dp_r);
                    }
                }
            }
        }

        // Refresh target pane tab strip
        if let Some((dp_w, dp_r)) = dp.as_ref() {
            if let Some(pane) = aw.workspaces[ws_idx].panes.get(&target_pane_id) {
                if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                    if let Some(pw) = widgets.pane_widgets.get(&target_pane_id) {
                        refresh_pane_tab_strip(pw, pane, dp_w, dp_r);
                    }
                }
            }
        }

        set_pane_active(aw, ws_idx, target_pane_id);
    });
}

/// Rebuild the split widgets for a workspace at a given index.
/// Handles the borrow-checker split by extracting the workspace ID first.
fn rebuild_workspace_split_widgets(aw: &mut AppWindow, ws_idx: usize) {
    let ws = &aw.workspaces[ws_idx];
    let ws_id = ws.id;

    // Clone what we need from the workspace to avoid overlapping borrows
    // (SplitTree is read-only here, panes are read-only, widgets are mutated)
    let tree_clone = clone_split_tree_structure(&ws.split_tree, &ws.panes);

    if let Some(widgets) = aw.workspace_widgets.get_mut(&ws_id) {
        rebuild_split_widgets_from_snapshot(&tree_clone, widgets, ws_id);
    }
}

/// Lightweight snapshot of a split tree for rebuild purposes.
enum TreeSnapshot {
    Leaf { pane_id: PaneId },
    Split {
        orientation: Orientation,
        node_id: NodeId,
        ratio: f64,
        first: Box<TreeSnapshot>,
        second: Box<TreeSnapshot>,
    },
}

fn clone_split_tree_structure(
    tree: &SplitTree,
    _panes: &std::collections::HashMap<PaneId, workspace::Pane>,
) -> Option<(NodeId, TreeSnapshot)> {
    let root = tree.root()?;
    Some((root, snapshot_tree_node(tree, root)))
}

fn snapshot_tree_node(tree: &SplitTree, node_id: NodeId) -> TreeSnapshot {
    match tree.node(node_id) {
        Some(Node::Leaf { pane_id }) => TreeSnapshot::Leaf { pane_id: *pane_id },
        Some(Node::Split { orientation, ratio, first, second }) => TreeSnapshot::Split {
            orientation: *orientation,
            node_id,
            ratio: *ratio,
            first: Box::new(snapshot_tree_node(tree, *first)),
            second: Box::new(snapshot_tree_node(tree, *second)),
        },
        None => TreeSnapshot::Leaf { pane_id: 0 }, // shouldn't happen
    }
}

fn rebuild_split_widgets_from_snapshot(
    snapshot: &Option<(NodeId, TreeSnapshot)>,
    widgets: &mut WorkspaceWidgets,
    ws_id: WorkspaceId,
) {
    // Detach all pane containers from their current parents.
    // Grab focus on the root_paned first to prevent GTK focus-child warnings
    // when children are removed from nested paneds.
    widgets.root_paned.grab_focus();
    for pw in widgets.pane_widgets.values() {
        if let Some(parent) = pw.container.parent() {
            if let Some(paned) = parent.downcast_ref::<gtk4::Paned>() {
                if paned.start_child().as_ref() == Some(pw.container.upcast_ref()) {
                    paned.set_start_child(None::<&gtk4::Widget>);
                } else {
                    paned.set_end_child(None::<&gtk4::Widget>);
                }
            }
        }
    }

    // Clear old split_paneds
    widgets.root_paned.set_start_child(None::<&gtk4::Widget>);
    widgets.root_paned.set_end_child(None::<&gtk4::Widget>);
    widgets.split_paneds.clear();

    let Some((_, tree_snap)) = snapshot else { return };
    let root_widget = build_widget_from_snapshot(tree_snap, widgets, ws_id);
    widgets.root_paned.set_start_child(Some(&root_widget));

    // Apply ratios from the tree snapshot after layout settles
    let mut ratio_map: Vec<(gtk4::Paned, f64)> = Vec::new();
    collect_paned_ratios(tree_snap, &widgets.split_paneds, &mut ratio_map);
    glib::idle_add_local_once(move || {
        for (paned, ratio) in &ratio_map {
            let total = paned_total_size(paned);
            if total > 0 {
                paned.set_position((total as f64 * ratio) as i32);
            }
        }
    });
}

/// Collect (GtkPaned, ratio) pairs from a TreeSnapshot for deferred position application.
fn collect_paned_ratios(
    snap: &TreeSnapshot,
    split_paneds: &std::collections::HashMap<NodeId, gtk4::Paned>,
    out: &mut Vec<(gtk4::Paned, f64)>,
) {
    match snap {
        TreeSnapshot::Leaf { .. } => {}
        TreeSnapshot::Split { node_id, ratio, first, second, .. } => {
            if let Some(paned) = split_paneds.get(node_id) {
                out.push((paned.clone(), *ratio));
            }
            collect_paned_ratios(first, split_paneds, out);
            collect_paned_ratios(second, split_paneds, out);
        }
    }
}

fn build_widget_from_snapshot(
    snap: &TreeSnapshot,
    widgets: &mut WorkspaceWidgets,
    ws_id: WorkspaceId,
) -> gtk4::Widget {
    match snap {
        TreeSnapshot::Leaf { pane_id } => {
            if let Some(pw) = widgets.pane_widgets.get(pane_id) {
                pw.container.clone().upcast()
            } else {
                gtk4::Label::new(Some("?")).upcast()
            }
        }
        TreeSnapshot::Split { orientation, node_id, first, second, .. } => {
            let paned = gtk4::Paned::new((*orientation).into());
            configure_split_paned(&paned);

            let first_widget = build_widget_from_snapshot(first, widgets, ws_id);
            let second_widget = build_widget_from_snapshot(second, widgets, ws_id);

            paned.set_start_child(Some(&first_widget));
            paned.set_end_child(Some(&second_widget));
            set_paned_separator_cursor(&paned);

            widgets.split_paneds.insert(*node_id, paned.clone());
            connect_paned_ratio_sync(&paned, ws_id, *node_id);
            paned.upcast()
        }
    }
}

/// Switch workspace by index (0-based).
pub fn goto_workspace(idx: i32) {
    with_app_window(|aw| {
        let n = aw.workspaces.len() as i32;
        if n == 0 || idx < 0 || idx >= n { return; }

        aw.notebook.set_current_page(Some(idx as u32));
        // Select the corresponding sidebar row
        let ws_id = aw.workspaces[idx as usize].id;
        if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
            aw.sidebar_list.select_row(Some(&widgets.sidebar_row));
        }
    });
}

/// Switch to next/previous workspace.
fn goto_workspace_relative(forward: bool) {
    with_app_window(|aw| {
        let n = aw.workspaces.len() as i32;
        if n == 0 { return; }
        let cur = aw.notebook.current_page().unwrap_or(0) as i32;
        let target = if forward {
            ((cur + 1) % n) as u32
        } else {
            ((cur - 1 + n) % n) as u32
        };
        aw.notebook.set_current_page(Some(target));

        if (target as usize) < aw.workspaces.len() {
            let ws_id = aw.workspaces[target as usize].id;
            if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                aw.sidebar_list.select_row(Some(&widgets.sidebar_row));
            }
        }
    });
}

/// Backwards-compatible goto_tab.
pub fn goto_tab(idx: i32) {
    with_app_window(|aw| {
        let n = aw.workspaces.len() as i32;
        if n == 0 { return; }

        let target = if idx == -1 {
            let cur = aw.notebook.current_page().unwrap_or(0) as i32;
            ((cur - 1 + n) % n) as u32
        } else if idx == -2 {
            let cur = aw.notebook.current_page().unwrap_or(0) as i32;
            ((cur + 1) % n) as u32
        } else if idx == -3 {
            (n - 1) as u32
        } else if idx >= 0 && idx < n {
            idx as u32
        } else {
            return;
        };

        aw.notebook.set_current_page(Some(target));
        if (target as usize) < aw.workspaces.len() {
            let ws_id = aw.workspaces[target as usize].id;
            if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                aw.sidebar_list.select_row(Some(&widgets.sidebar_row));
            }
        }
    });
}

// ── Pane tab operations ──────────────────────────────────────────────

/// Add a new tab to the focused pane in the current workspace.
fn new_pane_tab() {
    with_app_window_mut(|aw| {
        let Some(page) = aw.notebook.current_page() else { return };
        let ws_idx = page as usize;
        if ws_idx >= aw.workspaces.len() { return; }

        let ws_id = aw.workspaces[ws_idx].id;
        let Some(pane_id) = aw.workspaces[ws_idx].focused_pane() else { return };

        let new_surface_id = surfaces::pre_allocate_id();
        let (new_gl_area, _sid_cell) = surface::create_with_id(
            aw.working_directory.as_deref(),
            aw.command.as_deref(),
            Some(new_surface_id),
        );

        // Add tab to the pane
        if let Some(pane) = aw.workspaces[ws_idx].panes.get_mut(&pane_id) {
            pane.add_tab(new_surface_id);
        }

        // Add GLArea to the pane's stack and show it
        if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
            if let Some(pw) = widgets.pane_widgets.get(&pane_id) {
                pw.stack.add_named(
                    &new_gl_area,
                    Some(&format!("surface-{new_surface_id}")),
                );
                pw.stack.set_visible_child_name(&format!("surface-{new_surface_id}"));

                if let Some(pane) = aw.workspaces[ws_idx].panes.get(&pane_id) {
                    refresh_pane_tab_strip(pw, pane, &widgets.drop_preview, &widgets.drop_preview_rect);
                }
            }
        }

        let new_gl_focus = new_gl_area.clone();
        glib::idle_add_local_once(move || {
            new_gl_focus.grab_focus();
            surfaces::remove_pending(new_surface_id);
        });
    });
}

/// Close the active tab in the focused pane.
fn close_pane_tab() {
    // Extract panel info, then close outside the borrow — close_browser re-borrows WINDOW.
    let panel = with_app_window(|aw| {
        let Some(page) = aw.notebook.current_page() else { return None };
        let ws_idx = page as usize;
        if ws_idx >= aw.workspaces.len() { return None; }
        let pane_id = aw.workspaces[ws_idx].focused_pane()?;
        let pane = aw.workspaces[ws_idx].panes.get(&pane_id)?;
        pane.active_panel().cloned()
    });
    if let Some(panel) = panel {
        panel.request_close();
    }
}

/// Close a specific tab identified by its PanelKind. Used by close buttons
/// and middle-click on tab strip.
pub(crate) fn close_tab_by_panel(_pane_id: PaneId, panel: &PanelKind) {
    panel.request_close();
}

/// Cycle to the next/previous tab in the focused pane.
fn cycle_pane_tab(forward: bool) {
    with_app_window_mut(|aw| {
        let Some(page) = aw.notebook.current_page() else { return };
        let ws_idx = page as usize;
        if ws_idx >= aw.workspaces.len() { return; }

        let ws_id = aw.workspaces[ws_idx].id;
        let Some(pane_id) = aw.workspaces[ws_idx].focused_pane() else { return };

        let new_surface = if let Some(pane) = aw.workspaces[ws_idx].panes.get_mut(&pane_id) {
            pane.cycle_tab(forward)
        } else {
            None
        };

        if let Some(panel) = new_surface {
            let panel = panel.clone();
            if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                if let Some(pw) = widgets.pane_widgets.get(&pane_id) {
                    pw.stack.set_visible_child_name(&panel.stack_child_name());
                    if let Some(pane) = aw.workspaces[ws_idx].panes.get(&pane_id) {
                        refresh_pane_tab_strip(pw, pane, &widgets.drop_preview, &widgets.drop_preview_rect);
                    }
                }
            }
            panel.focus_gtk();
        }
    });
}

/// Select a specific tab in a pane (called from tab strip buttons).
fn select_pane_tab(pane_id: PaneId, tab_idx: usize, surface_id: crate::split::SurfaceId) {
    select_pane_tab_generic(pane_id, tab_idx, &PanelKind::Terminal { surface_id });
}

pub(crate) fn select_pane_tab_generic(pane_id: PaneId, tab_idx: usize, panel: &PanelKind) {
    with_app_window_mut(|aw| {
        // Find which workspace has this pane — mutate first
        let mut found = None;
        for (i, ws) in aw.workspaces.iter_mut().enumerate() {
            if let Some(pane) = ws.panes.get_mut(&pane_id) {
                pane.select_tab(tab_idx);
                found = Some((i, ws.id));
                break;
            }
        }

        // Now update widgets
        if let Some((ws_idx, ws_id)) = found {
            set_pane_active(aw, ws_idx, pane_id);
            if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                if let Some(pw) = widgets.pane_widgets.get(&pane_id) {
                    pw.stack.set_visible_child_name(&panel.stack_child_name());
                    // Find the pane again (immutable this time) for tab strip refresh
                    for ws in aw.workspaces.iter() {
                        if let Some(pane) = ws.panes.get(&pane_id) {
                            refresh_pane_tab_strip(pw, pane, &widgets.drop_preview, &widgets.drop_preview_rect);
                            break;
                        }
                    }
                }
            }
            // Focus the appropriate widget
            panel.focus_gtk();
        }
    });
}

// ── Title and focus tracking ─────────────────────────────────────────

/// Update the title of a surface (called from SET_TITLE action).
pub fn set_surface_title(surface_handle: crate::ghostty_sys::ghostty_surface_t, title: &str) {
    WINDOW.with(|w| {
        let binding = w.borrow();
        let Some(app_win) = binding.as_ref() else { return };
        let mut aw = app_win.borrow_mut();

        // First pass: find and mutate the matching tab, collect info
        let mut found: Option<(WorkspaceId, String, Option<String>)> = None;
        let mut matched_surface_id: Option<u32> = None;
        for ws in aw.workspaces.iter_mut() {
            let mut dir_changed: Option<String> = None;
            let mut matched = false;
            for pane in ws.panes.values_mut() {
                for tab in pane.tabs.iter_mut() {
                    if tab.surface_id().and_then(surfaces::get_handle) == Some(surface_handle) {
                        matched_surface_id = tab.surface_id();
                        tab.title = title.to_string();
                        if let Some(d) = extract_directory(title) {
                            let old_dir = tab.working_directory.clone();
                            tab.working_directory = Some(d.clone());
                            if old_dir.as_deref() != Some(d.as_str()) {
                                dir_changed = Some(d);
                            }
                        }
                        matched = true;
                        break;
                    }
                }
                if matched { break; }
            }
            if matched {
                ws.update_title_from_focused();
                found = Some((ws.id, ws.display_title().to_string(), dir_changed));
                break;
            }
        }

        // Second pass: update widgets (no mutable workspace borrow needed)
        if let Some((ws_id, display_title, dir)) = found {
            if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                sidebar::update_row_title(&widgets.sidebar_row, &display_title);
            }
            // Check if we should detect git branch
            if let Some(dir) = dir {
                drop(aw);
                drop(binding);
                detect_git_branch(ws_id, &dir);
            }
        }

        if let Some(sid) = matched_surface_id {
            crate::dbus::emit(crate::dbus::DbusSignal::TitleChanged {
                surface_id: sid,
                title: title.to_string(),
            });
        }
    });
}

/// Handle a bell event from a terminal surface.
/// Shows a notification badge on the sidebar row if the workspace is not active,
/// and highlights the specific pane if it is not the focused pane.
pub fn handle_bell(surface_handle: crate::ghostty_sys::ghostty_surface_t) {
    with_app_window_mut(|aw| {

        let active_page = aw.notebook.current_page().unwrap_or(0) as usize;

        // Find which workspace and pane contain this surface
        let mut found: Option<(usize, WorkspaceId, PaneId)> = None;
        for (idx, ws) in aw.workspaces.iter().enumerate() {
            for pane in ws.panes.values() {
                let contains = pane.tabs.iter().any(|tab| {
                    tab.surface_id().and_then(surfaces::get_handle) == Some(surface_handle)
                });
                if contains {
                    found = Some((idx, ws.id, pane.id));
                    break;
                }
            }
            if found.is_some() { break; }
        }

        let Some((ws_idx, ws_id, pane_id)) = found else { return };

        // Sidebar badge: only if not the active workspace
        if ws_idx != active_page {
            let ws = &mut aw.workspaces[ws_idx];
            if !ws.has_bell {
                ws.has_bell = true;
                if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                    sidebar::update_row_bell(&widgets.sidebar_row, true);
                }
            }
        }

        // Pane highlight: only if not the focused pane in this workspace
        let focused_pane = aw.workspaces[ws_idx].focused_pane();
        let is_focused_pane = focused_pane == Some(pane_id) && ws_idx == active_page;

        if !is_focused_pane {
            if let Some(pane) = aw.workspaces[ws_idx].panes.get_mut(&pane_id) {
                pane.has_bell = true;
            }
            if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                if let Some(pw) = widgets.pane_widgets.get(&pane_id) {
                    pw.container.add_css_class("pane-bell");
                }
            }
        }

        // Desktop notification (fires only when window is unfocused, rate-limited)
        let title = aw.workspaces[ws_idx].display_title().to_string();
        crate::notify::send_bell_notification(&title, ws_id);

        // Update tray state for bell badge
        update_tray_state(aw);

        // D-Bus signal — use the pane's active surface ID
        let sid = aw.workspaces[ws_idx].panes.get(&pane_id)
            .and_then(|p| p.active_surface())
            .unwrap_or(0);
        crate::dbus::emit(crate::dbus::DbusSignal::BellFired {
            workspace_id: ws_id,
            surface_id: sid,
        });
    });
}

/// Clear the bell indicator on a workspace by page index.
/// Also clears the focused pane's bell highlight after a brief delay.
fn clear_workspace_bell(page_num: usize) {
    with_app_window_mut(|aw| {

        if page_num >= aw.workspaces.len() { return; }

        // Clear sidebar bell dot
        if aw.workspaces[page_num].has_bell {
            aw.workspaces[page_num].has_bell = false;
            let ws_id = aw.workspaces[page_num].id;
            if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                sidebar::update_row_bell(&widgets.sidebar_row, false);
            }
            update_tray_state(aw);
        }

        // Clear the focused pane's bell highlight after a brief moment
        // (if the user lands back on the same pane that rang)
        let focused_pane_id = aw.workspaces[page_num].focused_pane();
        if let Some(pane_id) = focused_pane_id {
            let has_bell = aw.workspaces[page_num]
                .panes.get(&pane_id)
                .map_or(false, |p| p.has_bell);
            if has_bell {
                let ws_id = aw.workspaces[page_num].id;
                // Brief delay so the user sees the highlight before it clears
                glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
                    clear_pane_bell(ws_id, pane_id);
                });
            }
        }
    });
}

/// Clear the bell highlight on a specific pane.
fn clear_pane_bell(ws_id: WorkspaceId, pane_id: PaneId) {
    with_app_window_mut(|aw| {

        if let Some(ws) = aw.workspaces.iter_mut().find(|ws| ws.id == ws_id) {
            if let Some(pane) = ws.panes.get_mut(&pane_id) {
                pane.has_bell = false;
            }
        }
        if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
            if let Some(pw) = widgets.pane_widgets.get(&pane_id) {
                pw.container.remove_css_class("pane-bell");
            }
        }
    });
}

/// Push current workspace state to the system tray.
fn update_tray_state(aw: &AppWindow) {
    let entries: Vec<crate::tray::TrayWorkspaceEntry> = aw.workspaces.iter().map(|ws| {
        crate::tray::TrayWorkspaceEntry {
            id: ws.id,
            title: ws.display_title().to_string(),
            has_bell: ws.has_bell,
        }
    }).collect();
    crate::tray::update_workspaces(entries);
}

/// Extract a directory path from a terminal title.
/// Handles formats like "user@host:~/projects" or "/home/user/projects"
fn extract_directory(title: &str) -> Option<String> {
    let path = if let Some(colon_pos) = title.rfind(':') {
        &title[colon_pos + 1..]
    } else {
        title
    };

    let path = path.trim();
    if path.starts_with('/') || path.starts_with('~') {
        // Expand ~ to home directory
        if path.starts_with('~') {
            if let Ok(home) = std::env::var("HOME") {
                Some(path.replacen('~', &home, 1))
            } else {
                Some(path.to_string())
            }
        } else {
            Some(path.to_string())
        }
    } else {
        None
    }
}

/// Asynchronously detect the git branch for a workspace's directory.
fn detect_git_branch(ws_id: WorkspaceId, dir: &str) {
    let dir = dir.to_string();
    std::thread::spawn(move || {
        let output = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&dir)
            .output();

        let branch = output.ok().and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok().map(|s| s.trim().to_string())
            } else {
                None
            }
        });

        // Send result back to the main thread
        glib::idle_add_once(move || {
            update_workspace_branch(ws_id, branch.as_deref());
        });
    });
}

/// Update a workspace's git branch and refresh the sidebar.
fn update_workspace_branch(ws_id: WorkspaceId, branch: Option<&str>) {
    with_app_window_mut(|aw| {

        // Mutate workspace
        for ws in aw.workspaces.iter_mut() {
            if ws.id == ws_id {
                ws.git_branch = branch.map(|s| s.to_string());
                break;
            }
        }

        // Update widget
        if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
            sidebar::update_row_branch(&widgets.sidebar_row, branch);
        }
    });
}

/// Update the focused pane when a surface gains focus.
pub fn set_focused_surface(surface_id: crate::split::SurfaceId) {
    with_app_window_mut(|aw| {

        // Find which workspace/pane owns this surface
        let Some((ws_idx, ws_id, pane_id)) = aw.find_workspace_with_surface(surface_id) else { return };

        // Clear bell if set
        let clear_bell = aw.workspaces[ws_idx].panes.get(&pane_id)
            .map(|p| p.has_bell).unwrap_or(false);
        if clear_bell {
            if let Some(pane) = aw.workspaces[ws_idx].panes.get_mut(&pane_id) {
                pane.has_bell = false;
            }
        }

        set_pane_active(aw, ws_idx, pane_id);
        if clear_bell {
            if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                if let Some(pw) = widgets.pane_widgets.get(&pane_id) {
                    pw.container.remove_css_class("pane-bell");
                }
            }
        }
    });
}

/// Get the number of workspaces (backwards-compatible with tab_count).
pub fn tab_count() -> usize {
    with_app_window(|aw| aw.workspaces.len())
}

/// Get the number of workspaces.
pub fn workspace_count() -> usize {
    tab_count()
}

/// Toggle sidebar visibility.
pub fn toggle_sidebar() {
    with_app_window_mut(|aw| {

        if aw.sidebar_visible {
            // Save current width before hiding
            let pos = aw.main_paned.position();
            if pos > 0 {
                aw.sidebar_last_width = pos;
            }
            aw.sidebar_box.set_visible(false);
            aw.main_paned.set_shrink_start_child(true);
            aw.main_paned.set_position(0);
            aw.sidebar_visible = false;
        } else {
            aw.sidebar_box.set_visible(true);
            aw.main_paned.set_shrink_start_child(false);
            aw.main_paned.set_position(aw.sidebar_last_width);
            aw.sidebar_visible = true;
        }
    });
}

/// Rename a workspace (sets custom_title so it persists over shell title updates).
/// Get the currently focused workspace ID.
pub fn focused_workspace_id() -> Option<WorkspaceId> {
    with_app_window(|aw| {
        let page = aw.notebook.current_page()? as usize;
        aw.workspaces.get(page).map(|ws| ws.id)
    })
}

/// Set a status entry on a workspace.
pub fn set_workspace_status(
    ws_id: WorkspaceId,
    key: String,
    value: String,
    icon: Option<String>,
    color: Option<String>,
    priority: i32,
) {
    with_app_window_mut(|aw| {

        if let Some(ws) = aw.workspaces.iter_mut().find(|ws| ws.id == ws_id) {
            ws.status_entries.insert(key.clone(), workspace::SidebarStatusEntry {
                key, value, icon, color, priority,
            });
        }
        if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
            let entries = aw.workspaces.iter()
                .find(|ws| ws.id == ws_id)
                .map(|ws| &ws.status_entries);
            if let Some(entries) = entries {
                sidebar::update_row_status(&widgets.sidebar_row, entries);
            }
        }
    });
}

/// Clear a status entry from a workspace.
pub fn clear_workspace_status(ws_id: WorkspaceId, key: &str) {
    with_app_window_mut(|aw| {

        if let Some(ws) = aw.workspaces.iter_mut().find(|ws| ws.id == ws_id) {
            ws.status_entries.remove(key);
        }
        if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
            let entries = aw.workspaces.iter()
                .find(|ws| ws.id == ws_id)
                .map(|ws| &ws.status_entries);
            if let Some(entries) = entries {
                sidebar::update_row_status(&widgets.sidebar_row, entries);
            }
        }
    });
}

/// Add a log entry to a workspace (capped at 50 entries).
pub fn add_workspace_log(
    ws_id: WorkspaceId,
    message: String,
    level: workspace::LogLevel,
    source: Option<String>,
) {
    with_app_window_mut(|aw| {

        let latest = if let Some(ws) = aw.workspaces.iter_mut().find(|ws| ws.id == ws_id) {
            ws.log_entries.push(workspace::SidebarLogEntry {
                message, level, source, timestamp: std::time::Instant::now(),
            });
            if ws.log_entries.len() > 50 {
                ws.log_entries.remove(0);
            }
            ws.log_entries.last().cloned()
        } else {
            None
        };
        if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
            sidebar::update_row_log(&widgets.sidebar_row, latest.as_ref());
        }
    });
}

/// Clear all log entries from a workspace.
pub fn clear_workspace_log(ws_id: WorkspaceId) {
    with_app_window_mut(|aw| {

        if let Some(ws) = aw.workspaces.iter_mut().find(|ws| ws.id == ws_id) {
            ws.log_entries.clear();
        }
        if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
            sidebar::update_row_log(&widgets.sidebar_row, None);
        }
    });
}

/// Set the progress bar on a workspace.
pub fn set_workspace_progress(ws_id: WorkspaceId, value: f64, label: Option<String>) {
    with_app_window_mut(|aw| {

        let state = workspace::SidebarProgressState { value, label };
        if let Some(ws) = aw.workspaces.iter_mut().find(|ws| ws.id == ws_id) {
            ws.progress = Some(state.clone());
        }
        if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
            sidebar::update_row_progress(&widgets.sidebar_row, Some(&state));
        }
    });
}

/// Clear the progress bar from a workspace.
pub fn clear_workspace_progress(ws_id: WorkspaceId) {
    with_app_window_mut(|aw| {

        if let Some(ws) = aw.workspaces.iter_mut().find(|ws| ws.id == ws_id) {
            ws.progress = None;
        }
        if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
            sidebar::update_row_progress(&widgets.sidebar_row, None);
        }
    });
}

pub fn rename_workspace(workspace_id: WorkspaceId, new_name: &str) {
    with_app_window_mut(|aw| {

        if let Some(ws) = aw.workspaces.iter_mut().find(|ws| ws.id == workspace_id) {
            ws.custom_title = Some(new_name.to_string());
            ws.title = new_name.to_string();
        }
        if let Some(widgets) = aw.workspace_widgets.get(&workspace_id) {
            sidebar::update_row_title(&widgets.sidebar_row, new_name);
        }
        update_tray_state(aw);
        crate::dbus::emit(crate::dbus::DbusSignal::WorkspaceRenamed {
            id: workspace_id,
            title: new_name.to_string(),
        });
    });
}

/// Select a workspace by ID (switch to it).
pub fn select_workspace_by_id(ws_id: WorkspaceId) -> bool {
    with_app_window(|aw| {
        if let Some((idx, _)) = aw.workspaces.iter().enumerate().find(|(_, ws)| ws.id == ws_id) {
            aw.notebook.set_current_page(Some(idx as u32));
            if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                aw.sidebar_list.select_row(Some(&widgets.sidebar_row));
            }
            crate::dbus::emit(crate::dbus::DbusSignal::WorkspaceSwitched { id: ws_id });
            true
        } else {
            false
        }
    })
}

/// Close a workspace by ID. Returns false if not found or pinned.
pub fn close_workspace_by_id(ws_id: WorkspaceId) -> bool {
    let panels = with_app_window(|aw| {
        let (_, ws) = aw.workspaces.iter().enumerate().find(|(_, ws)| ws.id == ws_id)?;
        if ws.pinned { return None; }
        Some(ws.all_panels())
    });
    match panels {
        Some(panels) => {
            for panel in &panels {
                panel.request_close();
            }
            true
        }
        None => false,
    }
}

/// Return info about the focused workspace: (id, title).
pub fn current_workspace_info() -> Option<(WorkspaceId, String)> {
    with_app_window(|aw| {
        let page = aw.notebook.current_page()? as usize;
        let ws = aw.workspaces.get(page)?;
        Some((ws.id, ws.display_title().to_string()))
    })
}

/// List pane IDs in a workspace. Returns None if workspace not found.
pub fn list_panes(ws_id: WorkspaceId) -> Option<Vec<PaneId>> {
    with_app_window(|aw| {
        let ws = aw.workspaces.iter().find(|ws| ws.id == ws_id)?;
        Some(ws.panes.keys().copied().collect())
    })
}

/// Return detailed workspace listing. Each entry: (id, title, pane_count, is_pinned, color).
pub fn list_workspaces_detailed() -> Vec<(WorkspaceId, String, usize, bool, Option<String>)> {
    with_app_window(|aw| {
        aw.workspaces.iter().map(|ws| {
            (
                ws.id,
                ws.display_title().to_string(),
                ws.panes.len(),
                ws.pinned,
                ws.color.map(|c| c.to_string()),
            )
        }).collect()
    })
}

/// Return detailed surface/panel listing for all workspaces.
/// Each line describes one tab: workspace, pane, panel type, and metadata.
pub fn list_surfaces_detailed() -> Vec<String> {
    with_app_window(|aw| {
        let mut lines = Vec::new();
        for ws in &aw.workspaces {
            for pane in ws.panes.values() {
                for tab in &pane.tabs {
                    match &tab.panel {
                        PanelKind::Terminal { surface_id } => {
                            let mut parts = format!(
                                "surface:{surface_id} workspace:{} pane:{} terminal",
                                ws.id, pane.id,
                            );
                            if let Some(ref title) = tab.working_directory {
                                parts.push_str(&format!(" cwd={title}"));
                            }
                            lines.push(parts);
                        }
                        PanelKind::Browser { browser_id, .. } => {
                            let mut parts = format!(
                                "browser:{browser_id} workspace:{} pane:{} browser",
                                ws.id, pane.id,
                            );
                            if let Some(url) = crate::browser::get_url(*browser_id) {
                                parts.push_str(&format!(" url={url}"));
                            }
                            lines.push(parts);
                        }
                    }
                }
            }
        }
        lines
    })
}

/// Focus a specific pane by ID. Returns false if not found.
pub fn focus_pane_by_id(pane_id: PaneId) -> bool {
    with_app_window_mut(|aw| {

        // Find which workspace contains this pane
        let mut found: Option<(usize, WorkspaceId)> = None;
        for (idx, ws) in aw.workspaces.iter().enumerate() {
            if ws.panes.contains_key(&pane_id) {
                found = Some((idx, ws.id));
                break;
            }
        }
        let Some((ws_idx, ws_id)) = found else { return false };

        // Switch to the workspace if needed
        let current_page = aw.notebook.current_page().unwrap_or(0) as usize;
        if current_page != ws_idx {
            aw.notebook.set_current_page(Some(ws_idx as u32));
            if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                aw.sidebar_list.select_row(Some(&widgets.sidebar_row));
            }
        }

        set_pane_active(aw, ws_idx, pane_id);
        true
    })
}

/// Send text to a terminal surface. Returns false if surface not found.
pub fn send_text(surface_id: crate::split::SurfaceId, text: &str) -> bool {
    if let Some(handle) = surfaces::get_handle(surface_id) {
        let bytes = text.as_bytes();
        unsafe {
            crate::ghostty_sys::ghostty_surface_text(
                handle,
                bytes.as_ptr() as *const libc::c_char,
                bytes.len(),
            );
        }
        true
    } else {
        false
    }
}

/// Read the visible screen content from a terminal surface.
pub fn read_screen(surface_id: crate::split::SurfaceId) -> Option<String> {
    read_surface_scrollback(surface_id)
}

/// Set the accent color of a workspace.
pub fn set_workspace_color(workspace_id: WorkspaceId, color: Option<workspace::WorkspaceColor>) {
    with_app_window_mut(|aw| {

        if let Some(ws) = aw.workspaces.iter_mut().find(|ws| ws.id == workspace_id) {
            ws.color = color;
        }
        if let Some(widgets) = aw.workspace_widgets.get(&workspace_id) {
            sidebar::update_row_color(&widgets.sidebar_row, color);
        }
    });
}

/// Toggle the pinned state of a workspace.
pub fn toggle_workspace_pinned(workspace_id: WorkspaceId) {
    with_app_window_mut(|aw| {

        let new_pinned = if let Some(ws) = aw.workspaces.iter_mut().find(|ws| ws.id == workspace_id) {
            ws.pinned = !ws.pinned;
            ws.pinned
        } else {
            return;
        };

        if let Some(widgets) = aw.workspace_widgets.get(&workspace_id) {
            sidebar::update_row_pinned(&widgets.sidebar_row, new_pinned);
        }

        // Re-sort: pinned workspaces go to the top (stable sort preserves relative order)
        reorder_workspaces_for_pin(aw);
    });
}

/// Move a workspace to a new position relative to a target workspace.
/// `before` = true means insert before the target, false means after.
pub fn move_workspace(source_id: WorkspaceId, target_id: WorkspaceId, before: bool) {
    with_app_window_mut(|aw| {

        let Some(source_idx) = aw.workspaces.iter().position(|ws| ws.id == source_id) else { return };
        let Some(target_idx) = aw.workspaces.iter().position(|ws| ws.id == target_id) else { return };

        // Enforce pinned/unpinned group boundaries
        let source_pinned = aw.workspaces[source_idx].pinned;
        let target_pinned = aw.workspaces[target_idx].pinned;
        if source_pinned != target_pinned {
            return;
        }

        let ws = aw.workspaces.remove(source_idx);
        // Recalculate target position after removal
        let target_idx = aw.workspaces.iter().position(|w| w.id == target_id).unwrap_or(0);
        let insert_idx = if before { target_idx } else { target_idx + 1 };
        aw.workspaces.insert(insert_idx, ws);

        reorder_workspace_views(aw);
    });
}

/// Stable-sort workspaces so pinned ones come first, then sync views.
fn reorder_workspaces_for_pin(aw: &mut AppWindow) {
    aw.workspaces.sort_by_key(|ws| if ws.pinned { 0 } else { 1 });
    reorder_workspace_views(aw);
}

/// Sync notebook page order and sidebar row order to match `aw.workspaces` Vec order.
fn reorder_workspace_views(aw: &mut AppWindow) {
    // Detach all sidebar rows
    for ws in &aw.workspaces {
        if let Some(widgets) = aw.workspace_widgets.get(&ws.id) {
            aw.sidebar_list.remove(&widgets.sidebar_row);
        }
    }

    // Re-append sidebar rows and reorder notebook pages
    for (i, ws) in aw.workspaces.iter().enumerate() {
        if let Some(widgets) = aw.workspace_widgets.get(&ws.id) {
            aw.sidebar_list.append(&widgets.sidebar_row);
            if let Some(current_page) = aw.notebook.page_num(&widgets.overlay) {
                if current_page != i as u32 {
                    aw.notebook.reorder_child(&widgets.overlay, Some(i as u32));
                }
            }
        }
    }

    // Re-select the current workspace's sidebar row
    if let Some(page) = aw.notebook.current_page() {
        if let Some(ws) = aw.workspaces.get(page as usize) {
            if let Some(widgets) = aw.workspace_widgets.get(&ws.id) {
                aw.sidebar_list.select_row(Some(&widgets.sidebar_row));
            }
        }
    }
}

/// Create a session snapshot of the current window state.
pub fn session_snapshot() -> Option<crate::session::SessionSnapshot> {
    with_app_window(|aw| {

        let mut ws_snapshots = Vec::new();
        for ws in &aw.workspaces {
            let layout = if let Some(root) = ws.split_tree.root() {
                snapshot_layout_node(&ws.split_tree, root, &ws.panes)
            } else {
                crate::session::LayoutSnapshot::Single { tabs: vec![] }
            };

            let status_snap: Vec<_> = ws.status_entries.values().map(|e| {
                crate::session::StatusEntrySnapshot {
                    key: e.key.clone(), value: e.value.clone(),
                    icon: e.icon.clone(), color: e.color.clone(), priority: e.priority,
                }
            }).collect();
            let log_snap: Vec<_> = ws.log_entries.iter().map(|e| {
                crate::session::LogEntrySnapshot {
                    message: e.message.clone(),
                    level: e.level.as_str().to_string(),
                    source: e.source.clone(),
                }
            }).collect();
            let progress_snap = ws.progress.as_ref().map(|p| {
                crate::session::ProgressSnapshot { value: p.value, label: p.label.clone() }
            });

            ws_snapshots.push(crate::session::WorkspaceSnapshot {
                title: ws.title.clone(),
                custom_title: ws.custom_title.clone(),
                working_directory: ws.working_directory.clone(),
                color: ws.color.map(|c| c.to_string()),
                pinned: Some(ws.pinned),
                layout,
                status_entries: if status_snap.is_empty() { None } else { Some(status_snap) },
                log_entries: if log_snap.is_empty() { None } else { Some(log_snap) },
                progress: progress_snap,
                remote_config: ws.remote_config.clone(),
            });
        }

        let selected = aw.notebook.current_page().map(|p| p as usize);
        let sidebar_width = if aw.sidebar_visible {
            Some(aw.main_paned.position())
        } else {
            Some(aw.sidebar_last_width)
        };

        Some(crate::session::SessionSnapshot {
            version: 1,
            workspaces: ws_snapshots,
            selected_workspace: selected,
            sidebar_width,
            sidebar_visible: Some(aw.sidebar_visible),
        })
    })
}

const MAX_SCROLLBACK_LINES: usize = 5000;

/// Read the full scrollback text from a Ghostty surface.
fn read_surface_scrollback(surface_id: crate::split::SurfaceId) -> Option<String> {
    use crate::ghostty_sys::*;

    let handle = surfaces::get_handle(surface_id)?;

    let selection = ghostty_selection_s {
        top_left: ghostty_point_s {
            tag: GHOSTTY_POINT_SCREEN,
            coord: GHOSTTY_POINT_COORD_TOP_LEFT,
            x: 0,
            y: 0,
        },
        bottom_right: ghostty_point_s {
            tag: GHOSTTY_POINT_SCREEN,
            coord: GHOSTTY_POINT_COORD_BOTTOM_RIGHT,
            x: 0,
            y: 0,
        },
        rectangle: false,
    };

    let mut text_result = ghostty_text_s {
        tl_px_x: 0.0,
        tl_px_y: 0.0,
        offset_start: 0,
        offset_len: 0,
        text: std::ptr::null(),
        text_len: 0,
    };

    unsafe {
        if !ghostty_surface_read_text(handle, selection, &mut text_result) {
            return None;
        }
        if text_result.text.is_null() || text_result.text_len == 0 {
            ghostty_surface_free_text(handle, &mut text_result);
            return None;
        }

        let slice = std::slice::from_raw_parts(
            text_result.text as *const u8,
            text_result.text_len,
        );
        let full_text = String::from_utf8_lossy(slice).into_owned();
        ghostty_surface_free_text(handle, &mut text_result);

        // Truncate to last MAX_SCROLLBACK_LINES lines
        let truncated = truncate_to_last_n_lines(&full_text, MAX_SCROLLBACK_LINES);
        if truncated.is_empty() { None } else { Some(truncated) }
    }
}

/// Keep only the last `n` lines of a string.
fn truncate_to_last_n_lines(text: &str, n: usize) -> String {
    let line_count = text.lines().count();
    if line_count <= n {
        return text.to_string();
    }
    text.lines()
        .skip(line_count - n)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Recursively snapshot a SplitTree node into a LayoutSnapshot.
fn snapshot_layout_node(
    tree: &SplitTree,
    node_id: NodeId,
    panes: &std::collections::HashMap<PaneId, workspace::Pane>,
) -> crate::session::LayoutSnapshot {
    match tree.node(node_id) {
        Some(Node::Leaf { pane_id }) => {
            let tabs = panes.get(pane_id).map_or_else(Vec::new, |pane| {
                pane.tabs.iter().map(|tab| {
                    tab.to_snapshot(read_surface_scrollback, browser::get_url)
                }).collect()
            });
            crate::session::LayoutSnapshot::Single { tabs }
        }
        Some(Node::Split { orientation, ratio, first, second }) => {
            let orient_str = match orientation {
                Orientation::Horizontal => "horizontal",
                Orientation::Vertical => "vertical",
            };
            crate::session::LayoutSnapshot::Split {
                orientation: orient_str.to_string(),
                ratio: *ratio,
                first: Box::new(snapshot_layout_node(tree, *first, panes)),
                second: Box::new(snapshot_layout_node(tree, *second, panes)),
            }
        }
        None => crate::session::LayoutSnapshot::Single { tabs: vec![] },
    }
}

/// Restore workspaces from a session snapshot.
pub fn restore_session(snapshot: &crate::session::SessionSnapshot) {
    for ws_snap in &snapshot.workspaces {
        restore_workspace_from_layout(ws_snap);
    }

    // Select the previously active workspace
    if let Some(idx) = snapshot.selected_workspace {
        goto_workspace(idx as i32);
    }

    // Restore sidebar state
    with_app_window_mut(|aw| {
        if let Some(width) = snapshot.sidebar_width {
            aw.sidebar_last_width = width;
            if aw.sidebar_visible {
                aw.main_paned.set_position(width);
            }
        }
    });

    if snapshot.sidebar_visible == Some(false) {
        toggle_sidebar();
    }
}

/// Restore a workspace from its snapshot, including nested split layouts.
fn restore_workspace_from_layout(ws_snap: &crate::session::WorkspaceSnapshot) {
    with_app_window_mut(|aw| {

        let mut ws = Workspace::new();
        let ws_id = ws.id;

        if let Some(ref ct) = ws_snap.custom_title {
            ws.custom_title = Some(ct.clone());
        }
        let title = ws_snap.custom_title.as_deref()
            .unwrap_or(&format!("Workspace {}", aw.workspaces.len() + 1))
            .to_string();
        ws.title = title.clone();
        ws.working_directory = ws_snap.working_directory.clone();
        ws.color = ws_snap.color.as_deref().and_then(workspace::WorkspaceColor::from_name);
        ws.pinned = ws_snap.pinned.unwrap_or(false);
        ws.remote_config = ws_snap.remote_config.clone();

        let mut pane_widgets = std::collections::HashMap::new();
        let mut split_paneds = std::collections::HashMap::new();
        let mut pending_surface_ids = Vec::new();

        // Pre-create the drop preview refs so build_layout_widget can attach
        // drop targets. The overlay wrapper is created below after root_paned.
        let drop_preview_rect: Rc<RefCell<Option<PreviewRect>>> = Rc::new(RefCell::new(None));
        let drop_preview = gtk4::DrawingArea::new();
        drop_preview.set_hexpand(true);
        drop_preview.set_vexpand(true);
        drop_preview.set_can_target(false);
        drop_preview.set_sensitive(false);
        drop_preview.set_can_focus(false);
        let rect_for_draw = drop_preview_rect.clone();
        drop_preview.set_draw_func(move |_da, cr, _w, _h| {
            let rect = *rect_for_draw.borrow();
            if let Some(r) = rect {
                cr.set_source_rgba(0.39, 0.58, 0.93, 0.2);
                cr.rectangle(r.x, r.y, r.width, r.height);
                let _ = cr.fill();
                cr.set_source_rgba(0.39, 0.58, 0.93, 0.5);
                cr.set_line_width(2.0);
                cr.rectangle(r.x + 1.0, r.y + 1.0, r.width - 2.0, r.height - 2.0);
                let _ = cr.stroke();
            }
        });

        // Recursively build the widget tree from the layout
        let (root_widget, tree) = build_layout_widget(
            &ws_snap.layout,
            &mut ws,
            &mut pane_widgets,
            &mut split_paneds,
            &mut pending_surface_ids,
            aw.working_directory.as_deref(),
            aw.command.as_deref(),
            ws_id,
            &drop_preview,
            &drop_preview_rect,
        );

        ws.split_tree = tree;

        // Create the root paned — always wrap in a paned for consistency
        let root_paned = gtk4::Paned::new(gtk4::Orientation::Horizontal);
        configure_split_paned(&root_paned);
        root_paned.set_start_child(Some(&root_widget));

        // Restore metadata
        if let Some(ref entries) = ws_snap.status_entries {
            for e in entries {
                ws.status_entries.insert(e.key.clone(), workspace::SidebarStatusEntry {
                    key: e.key.clone(), value: e.value.clone(),
                    icon: e.icon.clone(), color: e.color.clone(), priority: e.priority,
                });
            }
        }
        if let Some(ref entries) = ws_snap.log_entries {
            for e in entries {
                ws.log_entries.push(workspace::SidebarLogEntry {
                    message: e.message.clone(),
                    level: workspace::LogLevel::from_str(&e.level),
                    source: e.source.clone(),
                    timestamp: std::time::Instant::now(),
                });
            }
        }
        if let Some(ref p) = ws_snap.progress {
            ws.progress = Some(workspace::SidebarProgressState {
                value: p.value, label: p.label.clone(),
            });
        }

        let sidebar_row = sidebar::make_row(ws_id, &title);
        sidebar::update_row_color(&sidebar_row, ws.color);
        sidebar::update_row_pinned(&sidebar_row, ws.pinned);
        if !ws.status_entries.is_empty() {
            sidebar::update_row_status(&sidebar_row, &ws.status_entries);
        }
        if let Some(log) = ws.log_entries.last() {
            sidebar::update_row_log(&sidebar_row, Some(log));
        }
        if let Some(ref progress) = ws.progress {
            sidebar::update_row_progress(&sidebar_row, Some(progress));
        }
        aw.sidebar_list.append(&sidebar_row);

        // Wrap in overlay for drop preview (using pre-created drop_preview)
        let overlay = gtk4::Overlay::new();
        overlay.set_child(Some(&root_paned));
        overlay.add_overlay(&drop_preview);
        overlay.set_measure_overlay(&drop_preview, false);

        let widgets = WorkspaceWidgets {
            root_paned: root_paned.clone(),
            overlay: overlay.clone(),
            drop_preview: drop_preview.clone(),
            drop_preview_rect: drop_preview_rect.clone(),
            sidebar_row,
            pane_widgets,
            split_paneds,
        };

        aw.notebook.append_page(&overlay, None::<&gtk4::Label>);
        aw.workspaces.push(ws);
        aw.workspace_widgets.insert(ws_id, widgets);

        // Set pane-active on the focused pane (or first pane)
        let ws_idx = aw.workspaces.len() - 1;
        if let Some(ws) = aw.workspaces.last() {
            let focused = ws.focused_pane()
                .or_else(|| ws.panes.keys().next().copied());
            if let Some(fpid) = focused {
                set_pane_active(aw, ws_idx, fpid);
            }
        }

        // Reconnect remote workspaces with fresh relay credentials.
        let remote_config = {
            let ws = aw.workspaces.last_mut().unwrap();
            if let Some(ref mut config) = ws.remote_config {
                // Generate fresh relay credentials — old ones are stale.
                config.relay_port = crate::remote::config::pick_relay_port();
                config.relay_id = Some(crate::remote::config::generate_relay_id());
                config.relay_token = Some(crate::remote::config::generate_relay_token());
                config.local_socket_path = None;
                ws.remote_state = crate::remote::RemoteConnectionState::Connecting;
                Some(config.clone())
            } else {
                None
            }
        };
        if remote_config.is_some() {
            if let Some(widgets) = aw.workspace_widgets.get(&ws_id) {
                sidebar::update_row_remote_state(
                    &widgets.sidebar_row,
                    &crate::remote::RemoteConnectionState::Connecting,
                    Some("Reconnecting..."),
                );
            }
        }

        glib::idle_add_local_once(move || {
            for sid in pending_surface_ids {
                surfaces::remove_pending(sid);
            }
            // Start remote controller after surfaces are settled.
            if let Some(config) = remote_config {
                crate::remote::controller::connect(ws_id, config);
            }
        });
    });
}

/// Recursively build a GTK widget tree from a LayoutSnapshot.
/// Returns the top-level widget and the SplitTree representing the layout.
fn build_layout_widget(
    layout: &crate::session::LayoutSnapshot,
    ws: &mut Workspace,
    pane_widgets: &mut std::collections::HashMap<PaneId, PaneWidget>,
    split_paneds: &mut std::collections::HashMap<NodeId, gtk4::Paned>,
    pending_ids: &mut Vec<crate::split::SurfaceId>,
    default_wd: Option<&str>,
    command: Option<&str>,
    ws_id: WorkspaceId,
    drop_preview: &gtk4::DrawingArea,
    drop_preview_rect: &Rc<RefCell<Option<PreviewRect>>>,
) -> (gtk4::Widget, SplitTree) {
    match layout {
        crate::session::LayoutSnapshot::Single { tabs } => {
            // Check if this is a browser pane
            let is_browser = tabs.first().map_or(false, |t| t.is_browser());

            if is_browser {
                let url = tabs.first()
                    .and_then(|t| t.url.as_deref())
                    .unwrap_or("");
                let browser_id = browser::next_browser_id();
                let browser_widget = browser::create(browser_id, url);

                let pane_id = workspace::next_pane_id();
                let pane = Pane::new_browser_with_id(pane_id, browser_id, url);
                ws.add_pane(pane);

                let pw = build_browser_pane_widget(&browser_widget, browser_id);
                if let Some(pane) = ws.panes.get(&pane_id) {
                    refresh_pane_tab_strip(&pw, pane, drop_preview, drop_preview_rect);
                }

                let widget = pw.container.clone().upcast::<gtk4::Widget>();
                pane_widgets.insert(pane_id, pw);

                let tree = SplitTree::new_with_pane(pane_id);
                (widget, tree)
            } else {
                // Get working directory from first tab or default
                let wd = tabs.first()
                    .and_then(|t| t.working_directory.as_deref())
                    .or(default_wd);

                let surface_id = surfaces::pre_allocate_id();
                let (gl_area, _) = surface::create_with_id(wd, command, Some(surface_id));
                pending_ids.push(surface_id);

                let pane = Pane::new_with_id(workspace::next_pane_id(), surface_id);
                let pane_id = pane.id;
                ws.add_pane(pane);

                let pw = build_pane_widget(&gl_area, surface_id);

                // Populate tab strip
                if let Some(pane) = ws.panes.get(&pane_id) {
                    refresh_pane_tab_strip(&pw, pane, drop_preview, drop_preview_rect);
                }

                let widget = pw.container.clone().upcast::<gtk4::Widget>();
                pane_widgets.insert(pane_id, pw);

                let tree = SplitTree::new_with_pane(pane_id);
                (widget, tree)
            }
        }
        crate::session::LayoutSnapshot::Split { orientation, ratio, first, second } => {
            let (first_widget, first_tree) = build_layout_widget(
                first, ws, pane_widgets, split_paneds, pending_ids, default_wd, command, ws_id,
                drop_preview, drop_preview_rect,
            );
            let (second_widget, second_tree) = build_layout_widget(
                second, ws, pane_widgets, split_paneds, pending_ids, default_wd, command, ws_id,
                drop_preview, drop_preview_rect,
            );

            let gtk_orient = if orientation == "vertical" {
                gtk4::Orientation::Vertical
            } else {
                gtk4::Orientation::Horizontal
            };

            let paned = gtk4::Paned::new(gtk_orient);
            configure_split_paned(&paned);
            paned.set_start_child(Some(&first_widget));
            paned.set_end_child(Some(&second_widget));

            // Merge the two subtrees into one SplitTree
            let orient = if orientation == "vertical" {
                Orientation::Vertical
            } else {
                Orientation::Horizontal
            };
            let tree = SplitTree::merge(first_tree, second_tree, orient, *ratio);

            // Track this paned by its root split node and connect ratio sync
            if let Some(root) = tree.root() {
                split_paneds.insert(root, paned.clone());
                connect_paned_ratio_sync(&paned, ws_id, root);
            }

            // Apply ratio after layout allocation
            set_paned_position_after_layout(&paned, *ratio);

            (paned.upcast::<gtk4::Widget>(), tree)
        }
    }
}
