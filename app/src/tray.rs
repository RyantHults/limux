//! System tray (StatusNotifierItem) via the `ksni` crate (v0.3, zbus backend).
//!
//! Shows a tray icon with a context menu for workspace switching, bell
//! indicators, notification toggle, and show/hide window. Uses the SNI
//! protocol over D-Bus (works on KDE, XFCE, MATE, and GNOME with the
//! AppIndicator extension).

use std::sync::{Arc, Mutex, OnceLock};

use gtk4::prelude::*;
use gtk4::glib;
use ksni::TrayMethods as _;

use crate::workspace::WorkspaceId;

/// Workspace info shared between the main thread and the tray thread.
#[derive(Clone)]
pub struct TrayWorkspaceEntry {
    pub id: WorkspaceId,
    pub title: String,
    pub has_bell: bool,
}

/// Shared state between the GTK main thread and the ksni tray thread.
struct TrayState {
    workspaces: Vec<TrayWorkspaceEntry>,
    window_visible: bool,
    notifications_enabled: bool,
}

static TRAY_STATE: OnceLock<Arc<Mutex<TrayState>>> = OnceLock::new();
static TRAY_HANDLE: Mutex<Option<ksni::Handle<LimuxTray>>> = Mutex::new(None);

fn state() -> &'static Arc<Mutex<TrayState>> {
    TRAY_STATE.get_or_init(|| {
        Arc::new(Mutex::new(TrayState {
            workspaces: Vec::new(),
            window_visible: true,
            notifications_enabled: true,
        }))
    })
}

/// Start the system tray. Call once after the window is created.
pub fn start() {
    // Spawn a tokio runtime on a background thread for ksni's async API
    std::thread::Builder::new()
        .name("limux-tray".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("Failed to create tray runtime: {e}");
                    return;
                }
            };
            rt.block_on(async {
                match LimuxTray.spawn().await {
                    Ok(handle) => {
                        *TRAY_HANDLE.lock().unwrap() = Some(handle);
                        // Keep the runtime alive; ksni manages its own tasks
                        std::future::pending::<()>().await;
                    }
                    Err(e) => {
                        eprintln!("Failed to start tray: {e}");
                    }
                }
            });
        })
        .ok();
}

/// Stop the tray service (call on shutdown).
pub fn stop() {
    if let Some(handle) = TRAY_HANDLE.lock().unwrap().take() {
        let _ = handle.shutdown();
    }
}

/// Update the workspace list in the tray (called from the GTK main thread).
pub fn update_workspaces(entries: Vec<TrayWorkspaceEntry>) {
    if let Ok(mut st) = state().lock() {
        st.workspaces = entries;
    }
    refresh();
}

/// Update window visibility state.
pub fn update_window_visible(visible: bool) {
    if let Ok(mut st) = state().lock() {
        st.window_visible = visible;
    }
    refresh();
}

/// Update notification toggle state.
pub fn update_notifications_enabled(enabled: bool) {
    if let Ok(mut st) = state().lock() {
        st.notifications_enabled = enabled;
    }
    refresh();
}

/// Tell ksni to re-query our menu and icon.
fn refresh() {
    if let Some(handle) = TRAY_HANDLE.lock().unwrap().as_ref() {
        // update is async in ksni 0.3 but we just fire-and-forget from the main thread
        let handle = handle.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                handle.update(|_tray| {}).await;
            });
        });
    }
}

// ── ksni::Tray implementation ──────────────────────────────────────

struct LimuxTray;

impl ksni::Tray for LimuxTray {
    fn id(&self) -> String {
        "limux-terminal".into()
    }

    fn icon_name(&self) -> String {
        "utilities-terminal".into()
    }

    fn title(&self) -> String {
        "Limux Terminal".into()
    }

    fn attention_icon_name(&self) -> String {
        "dialog-warning".into()
    }

    fn status(&self) -> ksni::Status {
        let has_any_bell = state()
            .lock()
            .map(|st| st.workspaces.iter().any(|ws| ws.has_bell))
            .unwrap_or(false);
        if has_any_bell {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let ws_count = state().lock().map(|st| st.workspaces.len()).unwrap_or(0);
        ksni::ToolTip {
            icon_name: "utilities-terminal".into(),
            icon_pixmap: Vec::new(),
            title: "Limux Terminal".into(),
            description: format!("{} workspace{}", ws_count, if ws_count == 1 { "" } else { "s" }),
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let mut items: Vec<ksni::MenuItem<Self>> = Vec::new();

        // Show/Hide Window
        let visible = state().lock().map(|st| st.window_visible).unwrap_or(true);
        let show_label = if visible { "Hide Window" } else { "Show Window" };
        items.push(ksni::MenuItem::Standard(ksni::menu::StandardItem {
            label: show_label.into(),
            activate: Box::new(|_| {
                dispatch_to_main(TrayAction::ToggleWindow);
            }),
            ..Default::default()
        }));

        items.push(ksni::MenuItem::Separator);

        // Workspace list
        let workspaces = state()
            .lock()
            .map(|st| st.workspaces.clone())
            .unwrap_or_default();

        for ws in workspaces {
            let label = if ws.has_bell {
                format!("\u{1F534} {}", ws.title)
            } else {
                ws.title.clone()
            };
            let ws_id = ws.id;
            items.push(ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label,
                activate: Box::new(move |_| {
                    dispatch_to_main(TrayAction::SwitchWorkspace(ws_id));
                }),
                ..Default::default()
            }));
        }

        items.push(ksni::MenuItem::Separator);

        // Notifications toggle
        let notif_enabled = state()
            .lock()
            .map(|st| st.notifications_enabled)
            .unwrap_or(true);
        let notif_label = if notif_enabled {
            "Notifications: On"
        } else {
            "Notifications: Off"
        };
        items.push(ksni::MenuItem::Standard(ksni::menu::StandardItem {
            label: notif_label.into(),
            activate: Box::new(|_| {
                dispatch_to_main(TrayAction::ToggleNotifications);
            }),
            ..Default::default()
        }));

        items.push(ksni::MenuItem::Separator);

        // Quit
        items.push(ksni::MenuItem::Standard(ksni::menu::StandardItem {
            label: "Quit".into(),
            activate: Box::new(|_| {
                dispatch_to_main(TrayAction::Quit);
            }),
            ..Default::default()
        }));

        items
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        dispatch_to_main(TrayAction::ToggleWindow);
    }
}

// ── Actions dispatched from tray thread to GTK main thread ─────────

enum TrayAction {
    ToggleWindow,
    SwitchWorkspace(WorkspaceId),
    ToggleNotifications,
    Quit,
}

/// Dispatch an action from the tray thread to the GTK main thread.
fn dispatch_to_main(action: TrayAction) {
    glib::MainContext::default().invoke(move || match action {
        TrayAction::ToggleWindow => {
            crate::WINDOW.with(|w| {
                if let Some(window) = w.borrow().as_ref() {
                    if window.is_visible() {
                        window.set_visible(false);
                    } else {
                        window.set_visible(true);
                        window.present();
                    }
                }
            });
        }
        TrayAction::SwitchWorkspace(ws_id) => {
            crate::window::select_workspace_by_id(ws_id);
            crate::WINDOW.with(|w| {
                if let Some(window) = w.borrow().as_ref() {
                    window.present();
                }
            });
        }
        TrayAction::ToggleNotifications => {
            let new_state = !crate::notify::is_enabled();
            if new_state {
                crate::notify::enable();
            } else {
                crate::notify::disable();
            }
            update_notifications_enabled(new_state);
        }
        TrayAction::Quit => {
            if let Some(app) = gtk4::gio::Application::default() {
                app.quit();
            }
        }
    });
}
