//! D-Bus scripting interface via zbus.
//!
//! Exposes the same command set as the Unix socket server over the session bus
//! at `com.limuxapp.Limux` / `/com/limuxapp/Limux`. Methods dispatch to the
//! GTK main thread via async oneshot channels; signals are emitted via a
//! broadcast channel from any thread.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use gtk4::glib;
use tokio::sync::{broadcast, oneshot};
use zbus::object_server::SignalEmitter;
use zbus::{connection::Builder, interface, Connection};
use zbus::fdo;

use crate::workspace::WorkspaceId;

// ── Lifecycle ──────────────────────────────────────────────────────

static DBUS_CONN: Mutex<Option<Connection>> = Mutex::new(None);

/// Start the D-Bus service on a background thread.
pub fn start() {
    std::thread::Builder::new()
        .name("limux-dbus".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("[dbus] failed to create runtime: {e}");
                    return;
                }
            };
            rt.block_on(async {
                if let Err(e) = run_service().await {
                    eprintln!("[dbus] service error: {e}");
                }
            });
        })
        .ok();
}

/// Stop the D-Bus service.
pub fn stop() {
    DBUS_CONN.lock().unwrap().take();
}

async fn run_service() -> zbus::Result<()> {
    let iface = LimuxInterface;

    let conn = Builder::session()?
        .name("com.limuxapp.Limux")?
        .serve_at("/com/limuxapp/Limux", iface)?
        .build()
        .await?;

    *DBUS_CONN.lock().unwrap() = Some(conn.clone());

    // Start signal forwarding task
    tokio::spawn(signal_forwarder(conn.clone()));

    // Keep alive
    std::future::pending::<()>().await;
    Ok(())
}

// ── Dispatch helper ────────────────────────────────────────────────

/// Run a closure on the GTK main thread and return its result asynchronously.
/// Neither the D-Bus thread nor the GTK main loop blocks.
async fn dispatch<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> fdo::Result<R> {
    let (tx, rx) = oneshot::channel();
    glib::MainContext::default().invoke(move || {
        let _ = tx.send(f());
    });
    tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .map_err(|_| fdo::Error::Failed("dispatch timeout".into()))?
        .map_err(|_| fdo::Error::Failed("dispatch channel closed".into()))
}

/// Fire-and-forget dispatch to the GTK main thread (no return value).
fn dispatch_fire(f: impl FnOnce() + Send + 'static) {
    glib::MainContext::default().invoke(f);
}

// ── D-Bus interface ────────────────────────────────────────────────

struct LimuxInterface;

#[interface(name = "com.limuxapp.Limux1")]
impl LimuxInterface {
    // ── System ─────────────────────────────────────────────────

    fn ping(&self) -> String {
        "pong".into()
    }

    fn version(&self) -> String {
        "limux 0.1.0".into()
    }

    // ── Workspace ──────────────────────────────────────────────

    async fn workspace_count(&self) -> fdo::Result<u32> {
        dispatch(|| crate::window::workspace_count() as u32).await
    }

    async fn new_workspace(&self) -> fdo::Result<()> {
        dispatch(|| crate::window::new_workspace()).await
    }

    async fn list_workspaces(&self) -> fdo::Result<Vec<(u32, String, u32, bool, String)>> {
        dispatch(|| {
            crate::window::list_workspaces_detailed()
                .into_iter()
                .map(|(id, title, panes, pinned, color)| {
                    (id, title, panes as u32, pinned, color.unwrap_or_default())
                })
                .collect()
        })
        .await
    }

    async fn current_workspace(&self) -> fdo::Result<(u32, String)> {
        dispatch(|| {
            crate::window::current_workspace_info().unwrap_or((0, String::new()))
        })
        .await
    }

    async fn select_workspace(&self, id: u32) -> fdo::Result<bool> {
        dispatch(move || crate::window::select_workspace_by_id(id)).await
    }

    async fn close_workspace(&self, id: u32) -> fdo::Result<bool> {
        dispatch(move || crate::window::close_workspace_by_id(id)).await
    }

    async fn rename_workspace(&self, id: u32, name: String) -> fdo::Result<()> {
        dispatch(move || crate::window::rename_workspace(id, &name)).await
    }

    async fn workspace_set_color(&self, id: u32, color: String) -> fdo::Result<()> {
        dispatch(move || {
            let c = if color.is_empty() || color == "none" {
                None
            } else {
                crate::workspace::WorkspaceColor::from_name(&color)
            };
            crate::window::set_workspace_color(id, c);
        })
        .await
    }

    async fn workspace_pin(&self, id: u32) -> fdo::Result<()> {
        dispatch(move || crate::window::toggle_workspace_pinned(id)).await
    }

    async fn toggle_sidebar(&self) -> fdo::Result<()> {
        dispatch(|| crate::window::toggle_sidebar()).await
    }

    // ── Panes ──────────────────────────────────────────────────

    async fn list_panes(&self, ws_id: u32) -> fdo::Result<Vec<u32>> {
        dispatch(move || crate::window::list_panes(ws_id).unwrap_or_default()).await
    }

    async fn focus_pane(&self, pane_id: u32) -> fdo::Result<bool> {
        dispatch(move || crate::window::focus_pane_by_id(pane_id)).await
    }

    async fn split_right(&self) -> fdo::Result<()> {
        dispatch(|| {
            crate::window::split_focused(crate::split::Orientation::Horizontal);
        })
        .await
    }

    async fn split_down(&self) -> fdo::Result<()> {
        dispatch(|| {
            crate::window::split_focused(crate::split::Orientation::Vertical);
        })
        .await
    }

    // ── Surfaces ───────────────────────────────────────────────

    async fn list_surfaces(&self) -> fdo::Result<Vec<String>> {
        dispatch(|| crate::window::list_surfaces_detailed()).await
    }

    async fn send(&self, surface_id: u32, text: String) -> fdo::Result<bool> {
        dispatch(move || crate::window::send_text(surface_id, &text)).await
    }

    async fn read_screen(&self, surface_id: u32) -> fdo::Result<String> {
        dispatch(move || crate::window::read_screen(surface_id).unwrap_or_default()).await
    }

    // ── Browser ────────────────────────────────────────────────

    async fn open_browser(&self, url: String) -> fdo::Result<()> {
        dispatch(move || {
            crate::window::split_focused_browser(
                crate::split::Orientation::Horizontal,
                &url,
            );
        })
        .await
    }

    async fn navigate(&self, browser_id: u32, url: String) -> fdo::Result<()> {
        dispatch(move || crate::browser::navigate(browser_id, &url)).await
    }

    async fn browser_back(&self, browser_id: u32) -> fdo::Result<()> {
        dispatch(move || crate::browser::go_back(browser_id)).await
    }

    async fn browser_forward(&self, browser_id: u32) -> fdo::Result<()> {
        dispatch(move || crate::browser::go_forward(browser_id)).await
    }

    async fn browser_reload(&self, browser_id: u32) -> fdo::Result<()> {
        dispatch(move || crate::browser::reload(browser_id)).await
    }

    async fn get_url(&self, browser_id: u32) -> fdo::Result<String> {
        dispatch(move || crate::browser::get_url(browser_id).unwrap_or_default()).await
    }

    async fn js_eval(&self, browser_id: u32, script: String) -> fdo::Result<()> {
        dispatch(move || crate::browser::evaluate_js(browser_id, &script)).await
    }

    // ── Metadata ───────────────────────────────────────────────

    async fn set_status(
        &self,
        key: String,
        value: String,
        icon: String,
        color: String,
        priority: i32,
    ) -> fdo::Result<()> {
        dispatch(move || {
            let ws_id = crate::window::focused_workspace_id().unwrap_or(0);
            crate::window::set_workspace_status(
                ws_id,
                key,
                value,
                if icon.is_empty() { None } else { Some(icon) },
                if color.is_empty() { None } else { Some(color) },
                priority,
            );
        })
        .await
    }

    async fn clear_status(&self, key: String) -> fdo::Result<()> {
        dispatch(move || {
            let ws_id = crate::window::focused_workspace_id().unwrap_or(0);
            crate::window::clear_workspace_status(ws_id, &key);
        })
        .await
    }

    async fn set_progress(&self, value: f64, label: String) -> fdo::Result<()> {
        dispatch(move || {
            let ws_id = crate::window::focused_workspace_id().unwrap_or(0);
            crate::window::set_workspace_progress(
                ws_id,
                value,
                if label.is_empty() { None } else { Some(label) },
            );
        })
        .await
    }

    async fn clear_progress(&self) -> fdo::Result<()> {
        dispatch(|| {
            let ws_id = crate::window::focused_workspace_id().unwrap_or(0);
            crate::window::clear_workspace_progress(ws_id);
        })
        .await
    }

    async fn log(&self, message: String, level: String) -> fdo::Result<()> {
        dispatch(move || {
            let ws_id = crate::window::focused_workspace_id().unwrap_or(0);
            crate::window::add_workspace_log(
                ws_id,
                message,
                crate::workspace::LogLevel::from_str(&level),
                None,
            );
        })
        .await
    }

    async fn clear_log(&self) -> fdo::Result<()> {
        dispatch(|| {
            let ws_id = crate::window::focused_workspace_id().unwrap_or(0);
            crate::window::clear_workspace_log(ws_id);
        })
        .await
    }

    // ── Notifications ──────────────────────────────────────────

    async fn notify_enable(&self) -> fdo::Result<()> {
        dispatch(|| {
            crate::notify::enable();
            crate::tray::update_notifications_enabled(true);
        })
        .await
    }

    async fn notify_disable(&self) -> fdo::Result<()> {
        dispatch(|| {
            crate::notify::disable();
            crate::tray::update_notifications_enabled(false);
        })
        .await
    }

    async fn notify_status(&self) -> fdo::Result<bool> {
        dispatch(|| crate::notify::is_enabled()).await
    }

    // ── Remote SSH ────────────────────────────────────────────

    async fn remote_connect(
        &self,
        destination: &str,
        port: u32,
        identity_file: &str,
        ssh_options: &str,
    ) -> fdo::Result<u32> {
        let dest = destination.to_string();
        let port_opt = if port == 0 { None } else { Some(port as u16) };
        let identity = if identity_file.is_empty() { None } else { Some(identity_file.to_string()) };
        let opts: Vec<String> = if ssh_options.is_empty() {
            Vec::new()
        } else {
            ssh_options.split(',').map(|s| s.to_string()).collect()
        };

        dispatch(move || {
            let config = crate::remote::RemoteConfiguration {
                destination: dest,
                port: port_opt,
                identity_file: identity,
                ssh_options: opts,
                terminal_startup_command: None,
                relay_port: None,
                relay_id: None,
                relay_token: None,
                local_socket_path: None,
            };
            crate::window::new_remote_workspace(config).unwrap_or(0)
        })
        .await
    }

    async fn remote_disconnect(&self, workspace_id: u32, clear: bool) -> fdo::Result<()> {
        dispatch(move || {
            let ws_id = if workspace_id == 0 { None } else { Some(workspace_id) };
            crate::window::disconnect_remote(ws_id, clear);
        })
        .await
    }

    async fn remote_reconnect(&self, workspace_id: u32) -> fdo::Result<()> {
        dispatch(move || {
            let ws_id = if workspace_id == 0 { None } else { Some(workspace_id) };
            crate::window::reconnect_remote(ws_id);
        })
        .await
    }

    async fn remote_status(&self, workspace_id: u32) -> fdo::Result<String> {
        dispatch(move || {
            let ws_id = if workspace_id == 0 { None } else { Some(workspace_id) };
            crate::window::remote_status_info(ws_id).unwrap_or_else(|| "{}".to_string())
        })
        .await
    }

    // ── Signals ────────────────────────────────────────────────

    #[zbus(signal)]
    async fn workspace_created(emitter: &SignalEmitter<'_>, id: u32, title: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn workspace_closed(emitter: &SignalEmitter<'_>, id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn workspace_switched(emitter: &SignalEmitter<'_>, id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn workspace_renamed(emitter: &SignalEmitter<'_>, id: u32, title: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn bell_fired(emitter: &SignalEmitter<'_>, workspace_id: u32, surface_id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn title_changed(emitter: &SignalEmitter<'_>, surface_id: u32, title: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn remote_state_changed(emitter: &SignalEmitter<'_>, workspace_id: u32, state: &str, detail: &str) -> zbus::Result<()>;
}

// ── Signal emission ────────────────────────────────────────────────

/// Events that can be emitted as D-Bus signals.
#[derive(Clone, Debug)]
pub enum DbusSignal {
    WorkspaceCreated { id: u32, title: String },
    WorkspaceClosed { id: u32 },
    WorkspaceSwitched { id: u32 },
    WorkspaceRenamed { id: u32, title: String },
    BellFired { workspace_id: u32, surface_id: u32 },
    TitleChanged { surface_id: u32, title: String },
    RemoteStateChanged { workspace_id: u32, state: String, detail: String },
}

static SIGNAL_TX: OnceLock<broadcast::Sender<DbusSignal>> = OnceLock::new();

/// Emit a D-Bus signal from any thread. No-op if D-Bus is not yet initialized.
pub fn emit(signal: DbusSignal) {
    if let Some(tx) = SIGNAL_TX.get() {
        let _ = tx.send(signal);
    }
}

/// Task that receives signals from the broadcast channel and emits them over D-Bus.
async fn signal_forwarder(conn: Connection) {
    let tx = SIGNAL_TX.get_or_init(|| broadcast::channel(64).0);
    let mut rx = tx.subscribe();

    let iface_ref = conn
        .object_server()
        .interface::<_, LimuxInterface>("/com/limuxapp/Limux")
        .await;
    let Ok(iface_ref) = iface_ref else {
        eprintln!("[dbus] failed to get interface ref for signals");
        return;
    };

    loop {
        match rx.recv().await {
            Ok(signal) => {
                let emitter = iface_ref.signal_emitter();
                let res = match &signal {
                    DbusSignal::WorkspaceCreated { id, title } => {
                        LimuxInterface::workspace_created(&emitter, *id, title).await
                    }
                    DbusSignal::WorkspaceClosed { id } => {
                        LimuxInterface::workspace_closed(&emitter, *id).await
                    }
                    DbusSignal::WorkspaceSwitched { id } => {
                        LimuxInterface::workspace_switched(&emitter, *id).await
                    }
                    DbusSignal::WorkspaceRenamed { id, title } => {
                        LimuxInterface::workspace_renamed(&emitter, *id, title).await
                    }
                    DbusSignal::BellFired { workspace_id, surface_id } => {
                        LimuxInterface::bell_fired(&emitter, *workspace_id, *surface_id).await
                    }
                    DbusSignal::TitleChanged { surface_id, title } => {
                        LimuxInterface::title_changed(&emitter, *surface_id, title).await
                    }
                    DbusSignal::RemoteStateChanged { workspace_id, state, detail } => {
                        LimuxInterface::remote_state_changed(&emitter, *workspace_id, state, detail).await
                    }
                };
                if let Err(e) = res {
                    eprintln!("[dbus] signal emission error: {e}");
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                eprintln!("[dbus] dropped {n} stale signals");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}
