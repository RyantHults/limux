//! Persistent, bidirectional JSON-RPC client over an SSH stdio connection
//! to the remote daemon.
//!
//! Spawns `ssh -T -S none [batch_args] <dest> "<daemon_path> serve --stdio"` as a
//! long-lived child process. Stdin is serialized via a mutex to prevent
//! interleaving from concurrent callers. A dedicated reader task dispatches
//! stdout lines by type:
//!   - Lines with `"id"` → RPC responses → routed to a pending-call registry
//!   - Lines with `"event"` → stream events → routed to subscription channels

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

use super::config::RemoteConfiguration;

/// A stream event received from the daemon (e.g., `proxy.stream.data`).
#[derive(Debug, Clone)]
pub struct StreamEvent {
    pub stream_id: String,
    pub event: String,
    pub data: Value,
}

/// Error type for RPC operations.
#[derive(Debug)]
pub enum RpcError {
    /// SSH process exited unexpectedly.
    ProcessDied(String),
    /// The call timed out waiting for a response.
    Timeout,
    /// The daemon returned an error.
    Remote(String),
    /// Serialization/IO failure.
    Io(std::io::Error),
    /// Hello handshake failed.
    HelloFailed(String),
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProcessDied(s) => write!(f, "SSH process died: {s}"),
            Self::Timeout => write!(f, "RPC call timed out"),
            Self::Remote(s) => write!(f, "remote error: {s}"),
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::HelloFailed(s) => write!(f, "hello failed: {s}"),
        }
    }
}

impl From<std::io::Error> for RpcError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

type PendingMap = HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>;
type SubscriptionMap = HashMap<String, mpsc::UnboundedSender<StreamEvent>>;

/// A persistent JSON-RPC connection to the remote daemon over SSH stdio.
pub struct RpcClient {
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    next_id: AtomicU64,
    pending: Arc<Mutex<PendingMap>>,
    subscriptions: Arc<Mutex<SubscriptionMap>>,
    child: Arc<Mutex<Child>>,
    _reader_task: tokio::task::JoinHandle<()>,
}

impl RpcClient {
    /// Connect to the remote daemon via SSH and perform the `hello` handshake.
    pub async fn connect(
        config: &RemoteConfiguration,
        daemon_path: &str,
    ) -> Result<Self, RpcError> {
        let mut args = config.ssh_batch_args();
        args.extend([
            "-T".into(),
            "-S".into(),
            "none".into(),
            config.destination.clone(),
            format!("{} serve --stdio", daemon_path),
        ]);

        let mut child = Command::new("ssh")
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;

        let stdin = child.stdin.take().ok_or_else(|| {
            RpcError::Io(std::io::Error::new(std::io::ErrorKind::Other, "no stdin"))
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            RpcError::Io(std::io::Error::new(std::io::ErrorKind::Other, "no stdout"))
        })?;

        let stdin = Arc::new(Mutex::new(stdin));
        let pending: Arc<Mutex<PendingMap>> = Arc::new(Mutex::new(HashMap::new()));
        let subscriptions: Arc<Mutex<SubscriptionMap>> = Arc::new(Mutex::new(HashMap::new()));

        // Spawn the reader task.
        let reader_task = {
            let pending = pending.clone();
            let subs = subscriptions.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break, // EOF
                        Ok(_) => {}
                        Err(_) => break,
                    }
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let Ok(obj) = serde_json::from_str::<Value>(trimmed) else {
                        continue;
                    };

                    // RPC response: has "id" field
                    if let Some(id) = obj.get("id").and_then(|v| v.as_u64()) {
                        let result = if let Some(err) = obj.get("error") {
                            let msg = err
                                .get("message")
                                .and_then(|m| m.as_str())
                                .unwrap_or("unknown error");
                            Err(RpcError::Remote(msg.to_string()))
                        } else {
                            Ok(obj.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let mut map = pending.lock().await;
                        if let Some(tx) = map.remove(&id) {
                            let _ = tx.send(result);
                        }
                        continue;
                    }

                    // Stream event: has "event" field
                    if let Some(event_name) = obj.get("event").and_then(|v| v.as_str()) {
                        let stream_id = obj
                            .get("stream_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let evt = StreamEvent {
                            stream_id: stream_id.clone(),
                            event: event_name.to_string(),
                            data: obj.clone(),
                        };

                        let subs_map = subs.lock().await;
                        if let Some(tx) = subs_map.get(&stream_id) {
                            let _ = tx.send(evt);
                        }
                    }
                }

                // EOF — notify all pending calls that the process died.
                let mut map = pending.lock().await;
                for (_, tx) in map.drain() {
                    let _ = tx.send(Err(RpcError::ProcessDied(
                        "SSH process exited".to_string(),
                    )));
                }
            })
        };

        let client = Self {
            stdin,
            next_id: AtomicU64::new(1),
            pending,
            subscriptions,
            child: Arc::new(Mutex::new(child)),
            _reader_task: reader_task,
        };

        // Perform hello handshake.
        let resp = client.call("hello", serde_json::json!({})).await?;
        if resp.get("name").is_none() {
            return Err(RpcError::HelloFailed(format!(
                "unexpected hello response: {resp}"
            )));
        }

        Ok(client)
    }

    /// Send a JSON-RPC call and await the response (with 30s timeout).
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let mut line = serde_json::to_string(&request).map_err(|e| {
            RpcError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        })?;
        line.push('\n');

        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await?;
        }

        match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RpcError::ProcessDied("channel closed".to_string())),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(RpcError::Timeout)
            }
        }
    }

    /// Register a subscription for stream events on a given stream_id.
    /// Returns a receiver that will get all events for that stream.
    pub async fn subscribe(
        &self,
        stream_id: &str,
    ) -> mpsc::UnboundedReceiver<StreamEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subscriptions
            .lock()
            .await
            .insert(stream_id.to_string(), tx);
        rx
    }

    /// Remove a subscription for a stream.
    pub async fn unsubscribe(&self, stream_id: &str) {
        self.subscriptions.lock().await.remove(stream_id);
    }

    /// Kill the underlying SSH process.
    pub async fn shutdown(&self) {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
    }
}

impl Drop for RpcClient {
    fn drop(&mut self) {
        // Best-effort kill — the reader task will see EOF and clean up.
        // kill_on_drop is set on the child, so this is a safety net.
        self._reader_task.abort();
    }
}
