//! Remote session controller — manages the background bootstrap thread, relay server,
//! reverse SSH tunnel, and connection state machine.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use super::bootstrap;
use super::config::{
    DaemonHello, ProxyEndpoint, RemoteConfiguration, RemoteConnectionState, RemoteDaemonState,
    RemoteDaemonStatus,
};
use super::proxy_broker::{self, ProxyLease, ProxyState};
use super::relay::RelayServer;
use crate::workspace::WorkspaceId;

/// Daemon version — used for binary path and cache.
const DAEMON_VERSION: &str = match option_env!("LIMUX_REMOTE_DAEMON_VERSION") {
    Some(v) => v,
    None => "dev",
};

// ── Controller registry ────────────────────────────────────────────

struct ControllerHandle {
    stop: Arc<AtomicBool>,
    _thread: JoinHandle<()>,
}

static REGISTRY: OnceLock<Mutex<HashMap<WorkspaceId, ControllerHandle>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<WorkspaceId, ControllerHandle>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

// ── Public API (called from GTK main thread) ───────────────────────

/// Start a remote connection for a workspace. Spawns a background thread.
pub fn connect(ws_id: WorkspaceId, config: RemoteConfiguration) {
    disconnect(ws_id);

    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let config_clone = config.clone();

    let thread = std::thread::Builder::new()
        .name(format!("limux-remote-{}", ws_id))
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("[remote] failed to create runtime for ws {}: {}", ws_id, e);
                    push_state(ws_id, RemoteConnectionState::Error, None, Some(e.to_string()));
                    return;
                }
            };
            rt.block_on(connection_loop(ws_id, config_clone, stop_clone));
        })
        .expect("failed to spawn remote controller thread");

    if let Ok(mut reg) = registry().lock() {
        reg.insert(ws_id, ControllerHandle { stop, _thread: thread });
    }
}

/// Stop the remote connection for a workspace.
pub fn disconnect(ws_id: WorkspaceId) {
    if let Ok(mut reg) = registry().lock() {
        if let Some(handle) = reg.remove(&ws_id) {
            handle.stop.store(true, Ordering::Release);
        }
    }
}

/// Stop and restart the remote connection with existing or new config.
pub fn reconnect(ws_id: WorkspaceId, config: RemoteConfiguration) {
    disconnect(ws_id);
    connect(ws_id, config);
}

/// Check if a controller is running for a workspace.
pub fn is_active(ws_id: WorkspaceId) -> bool {
    registry()
        .lock()
        .map(|reg| reg.contains_key(&ws_id))
        .unwrap_or(false)
}

// ── Background connection loop ─────────────────────────────────────

async fn connection_loop(
    ws_id: WorkspaceId,
    config: RemoteConfiguration,
    stop: Arc<AtomicBool>,
) {
    let mut retry_count: u32 = 0;

    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }

        push_state(
            ws_id,
            RemoteConnectionState::Connecting,
            Some(RemoteDaemonStatus {
                state: RemoteDaemonState::Bootstrapping,
                detail: Some(format!("Connecting to {}...", config.display_target())),
                version: None,
                name: None,
                capabilities: Vec::new(),
                remote_path: None,
            }),
            None,
        );

        match run_full_connection(&config, &stop).await {
            Ok(hello) => {
                retry_count = 0;
                push_state(
                    ws_id,
                    RemoteConnectionState::Connected,
                    Some(daemon_status_from_hello(&hello)),
                    Some(format!("Connected to {}", config.display_target())),
                );

                // Acquire a proxy tunnel lease for browser tunneling.
                let daemon_path = hello.remote_path.clone();
                let _proxy_lease = acquire_proxy_lease(ws_id, &config, &daemon_path);

                // Monitor the reverse SSH tunnel. If it dies, we'll restart
                // the full connection loop.
                monitor_tunnel(&config, &stop).await;

                // Lease drops here — broker may tear down tunnel if no other subscribers.
                drop(_proxy_lease);
                push_proxy_state(ws_id, None);

                if stop.load(Ordering::Acquire) {
                    return;
                }
                // Tunnel died — retry the full connection.
                retry_count += 1;
                let delay = retry_delay(2.0, retry_count);
                push_state(
                    ws_id,
                    RemoteConnectionState::Connecting,
                    None,
                    Some(format!("Relay tunnel lost, reconnecting in {:.0}s...", delay)),
                );
                tokio::time::sleep(Duration::from_secs_f64(delay)).await;
            }
            Err(e) => {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                retry_count += 1;
                let delay = retry_delay(4.0, retry_count);
                let detail = format!("{} (retry {} in {:.0}s)", e, retry_count, delay);

                push_state(
                    ws_id,
                    RemoteConnectionState::Error,
                    Some(RemoteDaemonStatus {
                        state: RemoteDaemonState::Error,
                        detail: Some(e.to_string()),
                        version: None,
                        name: None,
                        capabilities: Vec::new(),
                        remote_path: None,
                    }),
                    Some(detail),
                );

                tokio::time::sleep(Duration::from_secs_f64(delay)).await;
            }
        }
    }
}

/// Run the full connection sequence: bootstrap → relay → tunnel → metadata.
async fn run_full_connection(
    config: &RemoteConfiguration,
    stop: &Arc<AtomicBool>,
) -> Result<DaemonHello, bootstrap::BootstrapError> {
    // 1. Bootstrap the daemon on the remote host.
    let hello = bootstrap::bootstrap_daemon(config, DAEMON_VERSION).await?;

    if stop.load(Ordering::Acquire) {
        return Ok(hello);
    }

    // 2. Resolve relay credentials from config (set by new_remote_workspace).
    let relay_port = config.relay_port.unwrap_or(0);
    let relay_id = config.relay_id.clone().unwrap_or_default();
    let relay_token = config.relay_token.clone().unwrap_or_default();

    if relay_port == 0 || relay_id.is_empty() || relay_token.is_empty() {
        eprintln!("[remote] relay credentials not configured, skipping relay setup");
        return Ok(hello);
    }

    // 3. Get the local socket path for bridging.
    let local_socket = crate::socket::socket_path()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    if local_socket.is_empty() {
        eprintln!("[remote] local socket path not available, skipping relay setup");
        return Ok(hello);
    }

    // 4. Start the relay server.
    let _relay = RelayServer::start(
        relay_port,
        relay_id.clone(),
        &relay_token,
        local_socket,
    )
    .await
    .map_err(|e| bootstrap::BootstrapError::IoError(e))?;

    eprintln!("[remote] relay server listening on 127.0.0.1:{}", relay_port);

    if stop.load(Ordering::Acquire) {
        return Ok(hello);
    }

    // 5. Start the reverse SSH tunnel.
    start_reverse_tunnel(config, relay_port).await?;

    // Give the tunnel a moment to establish.
    tokio::time::sleep(Duration::from_millis(500)).await;

    if stop.load(Ordering::Acquire) {
        return Ok(hello);
    }

    // 6. Install remote metadata (auth files, limux wrapper).
    bootstrap::install_remote_metadata(
        config,
        relay_port,
        &relay_id,
        &relay_token,
        &hello.remote_path,
    )
    .await?;

    eprintln!("[remote] metadata installed on {}", config.display_target());

    Ok(hello)
}

/// Start the reverse SSH tunnel: remote:relayPort → local:relayPort.
async fn start_reverse_tunnel(
    config: &RemoteConfiguration,
    relay_port: u16,
) -> Result<(), bootstrap::BootstrapError> {
    use tokio::process::Command;

    let mut args = config.ssh_batch_args();
    args.extend([
        "-N".into(),
        "-T".into(),
        "-S".into(),
        "none".into(),
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        "-o".into(),
        "RequestTTY=no".into(),
        "-R".into(),
        format!("127.0.0.1:{}:127.0.0.1:{}", relay_port, relay_port),
        config.destination.clone(),
    ]);

    let mut child = Command::new("ssh")
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(bootstrap::BootstrapError::IoError)?;

    // Wait briefly to see if it exits immediately (port conflict, auth failure, etc.)
    tokio::time::sleep(Duration::from_millis(500)).await;

    match child.try_wait() {
        Ok(Some(status)) => {
            let stderr = if let Some(mut err) = child.stderr.take() {
                let mut s = String::new();
                use tokio::io::AsyncReadExt;
                let _ = err.read_to_string(&mut s).await;
                s
            } else {
                String::new()
            };
            Err(bootstrap::BootstrapError::SshFailed {
                status,
                stderr: format!("reverse tunnel exited immediately: {}", stderr.trim()),
            })
        }
        Ok(None) => {
            // Still running — tunnel is up. Store the child for monitoring.
            // We leak it into a background task that the monitor loop will track.
            TUNNEL_CHILD.lock().unwrap().replace(child);
            Ok(())
        }
        Err(e) => Err(bootstrap::BootstrapError::IoError(e)),
    }
}

// Global tunnel child for the monitor loop.
static TUNNEL_CHILD: Mutex<Option<tokio::process::Child>> = Mutex::new(None);

/// Monitor the reverse SSH tunnel. Returns when the tunnel exits.
async fn monitor_tunnel(
    _config: &RemoteConfiguration,
    stop: &Arc<AtomicBool>,
) {
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        if stop.load(Ordering::Acquire) {
            // Kill the tunnel on stop.
            if let Some(mut child) = TUNNEL_CHILD.lock().unwrap().take() {
                let _ = child.kill().await;
            }
            return;
        }

        // Check if tunnel process is still alive.
        let exited = TUNNEL_CHILD.lock().unwrap().as_mut().and_then(|c| c.try_wait().ok()).flatten();
        if exited.is_some() {
            TUNNEL_CHILD.lock().unwrap().take();
            eprintln!("[remote] reverse SSH tunnel exited");
            return;
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────

fn retry_delay(base: f64, retry: u32) -> f64 {
    let exponent = (retry.saturating_sub(1)) as f64;
    (base * 2.0_f64.powf(exponent)).min(60.0)
}

fn daemon_status_from_hello(hello: &DaemonHello) -> RemoteDaemonStatus {
    RemoteDaemonStatus {
        state: RemoteDaemonState::Ready,
        detail: None,
        version: Some(hello.version.clone()),
        name: Some(hello.name.clone()),
        capabilities: hello.capabilities.clone(),
        remote_path: Some(hello.remote_path.clone()),
    }
}

fn push_state(
    ws_id: WorkspaceId,
    conn_state: RemoteConnectionState,
    daemon_status: Option<RemoteDaemonStatus>,
    detail: Option<String>,
) {
    let state_str = conn_state.as_str().to_string();
    let detail_clone = detail.clone().unwrap_or_default();

    glib::MainContext::default().invoke(move || {
        crate::window::update_remote_state(ws_id, conn_state, daemon_status, detail);
        crate::dbus::emit(crate::dbus::DbusSignal::RemoteStateChanged {
            workspace_id: ws_id,
            state: state_str,
            detail: detail_clone,
        });
    });
}

/// Acquire a proxy lease from the broker, wiring state changes to the GTK main thread.
fn acquire_proxy_lease(
    ws_id: WorkspaceId,
    config: &RemoteConfiguration,
    daemon_path: &str,
) -> ProxyLease {
    proxy_broker::acquire(
        config,
        daemon_path,
        Box::new(move |state| {
            match state {
                ProxyState::Ready(endpoint) => {
                    eprintln!(
                        "[remote] proxy ready for ws {ws_id}: socks5://{}:{}",
                        endpoint.host, endpoint.port
                    );
                    push_proxy_state(ws_id, Some(endpoint));
                }
                ProxyState::Connecting => {
                    eprintln!("[remote] proxy connecting for ws {ws_id}");
                    push_proxy_state(ws_id, None);
                }
                ProxyState::Error(detail) => {
                    eprintln!("[remote] proxy error for ws {ws_id}: {detail}");
                    push_proxy_state(ws_id, None);
                }
            }
        }),
    )
}

/// Push proxy endpoint state to the GTK main thread.
fn push_proxy_state(ws_id: WorkspaceId, endpoint: Option<ProxyEndpoint>) {
    glib::MainContext::default().invoke(move || {
        crate::window::update_proxy_endpoint(ws_id, endpoint);
    });
}

use gtk4::glib;
