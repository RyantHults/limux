//! Singleton proxy broker — deduplicates proxy tunnels by transport key
//! so multiple workspaces sharing the same remote host use one tunnel.
//!
//! The broker owns its own tokio runtime (tunnels are shared across workspace
//! threads). Each unique transport key maps to one `ProxyTunnel` + `RpcClient`.
//! Workspaces acquire leases; the tunnel is torn down when the last lease drops.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::config::{ProxyEndpoint, RemoteConfiguration};
use super::proxy_tunnel::ProxyTunnel;
use super::rpc::RpcClient;

// ── Proxy state callback ──────────────────────────────────────────────

/// State updates pushed to the workspace via the callback.
#[derive(Debug, Clone)]
pub enum ProxyState {
    Connecting,
    Ready(ProxyEndpoint),
    Error(String),
}

/// Callback type: receives proxy state changes on the broker's runtime thread.
/// The implementation should dispatch to the GTK main thread.
pub type ProxyCallback = Box<dyn Fn(ProxyState) + Send + Sync + 'static>;

// ── Broker singleton ──────────────────────────────────────────────────

struct BrokerEntry {
    /// Number of active leases.
    subscriber_count: usize,
    /// The running tunnel (None while connecting/restarting).
    tunnel: Option<ProxyTunnel>,
    /// The RPC client (kept alive as long as the tunnel is up).
    rpc: Option<Arc<RpcClient>>,
    /// The resolved endpoint.
    endpoint: Option<ProxyEndpoint>,
    /// Callbacks for all current subscribers.
    callbacks: Vec<Arc<ProxyCallback>>,
    /// Handle for the tunnel management task.
    task: Option<tokio::task::JoinHandle<()>>,
}

struct BrokerInner {
    entries: Mutex<HashMap<String, BrokerEntry>>,
    runtime: tokio::runtime::Runtime,
}

static BROKER: OnceLock<BrokerInner> = OnceLock::new();

fn broker() -> &'static BrokerInner {
    BROKER.get_or_init(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("limux-proxy-broker")
            .enable_all()
            .build()
            .expect("failed to create proxy broker runtime");
        BrokerInner {
            entries: Mutex::new(HashMap::new()),
            runtime,
        }
    })
}

// ── Lease ─────────────────────────────────────────────────────────────

/// A lease on a proxy tunnel. Dropping the lease decrements the subscriber
/// count; when it reaches zero the tunnel is torn down.
pub struct ProxyLease {
    transport_key: String,
}

impl Drop for ProxyLease {
    fn drop(&mut self) {
        release(&self.transport_key);
    }
}

// ── Public API ────────────────────────────────────────────────────────

/// Acquire a proxy tunnel for the given remote configuration.
///
/// If a tunnel already exists for this transport key, the callback is
/// registered and immediately notified with the current state. Otherwise,
/// a new tunnel is started.
///
/// Returns a `ProxyLease` — dropping it releases the subscription.
pub fn acquire(
    config: &RemoteConfiguration,
    daemon_path: &str,
    callback: ProxyCallback,
) -> ProxyLease {
    let key = config.transport_key();
    let b = broker();
    let callback = Arc::new(callback);

    let mut entries = b.entries.lock().unwrap();
    if let Some(entry) = entries.get_mut(&key) {
        entry.subscriber_count += 1;
        entry.callbacks.push(callback.clone());
        eprintln!(
            "[proxy-broker] reusing existing tunnel for key={key} (subscribers={})",
            entry.subscriber_count
        );

        // Notify with current state.
        if let Some(ref ep) = entry.endpoint {
            callback(ProxyState::Ready(ep.clone()));
        } else {
            callback(ProxyState::Connecting);
        }
    } else {
        eprintln!("[proxy-broker] creating new tunnel for key={key}");
        let cb = callback.clone();
        cb(ProxyState::Connecting);

        let entry = BrokerEntry {
            subscriber_count: 1,
            tunnel: None,
            rpc: None,
            endpoint: None,
            callbacks: vec![callback],
            task: None,
        };
        entries.insert(key.clone(), entry);

        // Spawn the tunnel management task.
        let config = config.clone();
        let daemon_path = daemon_path.to_string();
        let key_clone = key.clone();
        let handle = b.runtime.spawn(tunnel_task(key_clone, config, daemon_path));

        if let Some(entry) = entries.get_mut(&key) {
            entry.task = Some(handle);
        }
    }

    ProxyLease { transport_key: key }
}

fn release(key: &str) {
    let b = broker();
    let mut entries = b.entries.lock().unwrap();
    let should_remove = if let Some(entry) = entries.get_mut(key) {
        entry.subscriber_count = entry.subscriber_count.saturating_sub(1);
        if entry.subscriber_count == 0 {
            eprintln!("[proxy-broker] last subscriber released, tearing down tunnel for key={key}");
            // Tear down: abort the management task and shut down tunnel/rpc.
            if let Some(task) = entry.task.take() {
                task.abort();
            }
            if let Some(ref tunnel) = entry.tunnel {
                tunnel.shutdown();
            }
            if let Some(ref rpc) = entry.rpc {
                let rpc = rpc.clone();
                b.runtime.spawn(async move { rpc.shutdown().await });
            }
            true
        } else {
            eprintln!(
                "[proxy-broker] subscriber released, {} remaining for key={key}",
                entry.subscriber_count
            );
            // Remove this subscriber's callback (remove the last one added).
            entry.callbacks.pop();
            false
        }
    } else {
        false
    };

    if should_remove {
        entries.remove(key);
    }
}

// ── Tunnel management task ────────────────────────────────────────────

/// Long-running task that connects the RPC client, starts the tunnel,
/// and restarts with exponential backoff on failure.
async fn tunnel_task(
    key: String,
    config: RemoteConfiguration,
    daemon_path: String,
) {
    let mut retry_count: u32 = 0;
    let base_delay = 3.0_f64;
    let max_delay = 60.0_f64;

    loop {
        // Attempt to connect.
        match start_tunnel(&config, &daemon_path).await {
            Ok((rpc, tunnel, endpoint)) => {
                retry_count = 0;

                // Store state and notify.
                let entry_gone = {
                    let b = broker();
                    let mut entries = b.entries.lock().unwrap();
                    if let Some(entry) = entries.get_mut(&key) {
                        entry.tunnel = Some(tunnel);
                        entry.rpc = Some(rpc.clone());
                        entry.endpoint = Some(endpoint.clone());
                        for cb in &entry.callbacks {
                            cb(ProxyState::Ready(endpoint.clone()));
                        }
                        false
                    } else {
                        true
                    }
                };
                if entry_gone {
                    rpc.shutdown().await;
                    return;
                }

                // Wait until the tunnel stops working.
                // We poll by attempting a no-op call periodically.
                loop {
                    tokio::time::sleep(Duration::from_secs(10)).await;

                    // Check if entry still exists (lease might have been dropped).
                    let exists = broker()
                        .entries
                        .lock()
                        .unwrap()
                        .contains_key(&key);
                    if !exists {
                        rpc.shutdown().await;
                        return;
                    }

                    // Ping the daemon to check liveness.
                    match rpc.call("ping", serde_json::json!({})).await {
                        Ok(_) => {}
                        Err(_) => {
                            eprintln!("[proxy-broker] RPC ping failed for key={key}, restarting tunnel");
                            break;
                        }
                    }
                }

                // Tunnel died — clean up state and retry.
                {
                    let b = broker();
                    let mut entries = b.entries.lock().unwrap();
                    if let Some(entry) = entries.get_mut(&key) {
                        entry.tunnel = None;
                        entry.rpc = None;
                        entry.endpoint = None;
                        for cb in &entry.callbacks {
                            cb(ProxyState::Connecting);
                        }
                    } else {
                        return;
                    }
                }
                rpc.shutdown().await;
                retry_count += 1;
            }
            Err(e) => {
                retry_count += 1;
                let delay = (base_delay * 2.0_f64.powf((retry_count - 1) as f64)).min(max_delay);
                eprintln!(
                    "[proxy-broker] tunnel failed for key={key}: {e} (retry {retry_count} in {delay:.0}s)"
                );

                {
                    let b = broker();
                    let entries = b.entries.lock().unwrap();
                    if let Some(entry) = entries.get(&key) {
                        for cb in &entry.callbacks {
                            cb(ProxyState::Error(e.to_string()));
                        }
                    } else {
                        return;
                    }
                }

                tokio::time::sleep(Duration::from_secs_f64(delay)).await;

                // Check if still alive.
                if !broker().entries.lock().unwrap().contains_key(&key) {
                    return;
                }
            }
        }
    }
}

/// Connect the RPC client and start the proxy tunnel.
async fn start_tunnel(
    config: &RemoteConfiguration,
    daemon_path: &str,
) -> Result<(Arc<RpcClient>, ProxyTunnel, ProxyEndpoint), String> {
    let rpc = RpcClient::connect(config, daemon_path)
        .await
        .map_err(|e| format!("RPC connect failed: {e}"))?;

    let rpc = Arc::new(rpc);
    let tunnel = ProxyTunnel::start(rpc.clone())
        .await
        .map_err(|e| format!("tunnel bind failed: {e}"))?;

    let endpoint = ProxyEndpoint {
        host: "127.0.0.1".to_string(),
        port: tunnel.port,
    };

    eprintln!(
        "[proxy-broker] tunnel ready on socks5://127.0.0.1:{} → {}",
        tunnel.port,
        config.display_target()
    );

    Ok((rpc, tunnel, endpoint))
}
