//! CLI relay server — authenticates TCP connections via HMAC-SHA256 and bridges
//! them to the local limux Unix socket.

use std::sync::Arc;
use std::time::Duration;

use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

type HmacSha256 = Hmac<Sha256>;

/// Shared state for the relay server.
pub struct RelayServer {
    pub port: u16,
    relay_id: String,
    relay_token: Vec<u8>,
    local_socket_path: String,
}

impl RelayServer {
    /// Start the relay server on the given port. Returns the server handle.
    ///
    /// `relay_id` and `relay_token_hex` are the HMAC-SHA256 credentials.
    /// `local_socket_path` is the path to the limux Unix socket.
    pub async fn start(
        port: u16,
        relay_id: String,
        relay_token_hex: &str,
        local_socket_path: String,
    ) -> Result<Arc<Self>, std::io::Error> {
        let listener = TcpListener::bind(format!("127.0.0.1:{}", port)).await?;
        let actual_port = listener.local_addr()?.port();

        let token_bytes = hex_decode(relay_token_hex);

        let server = Arc::new(Self {
            port: actual_port,
            relay_id,
            relay_token: token_bytes,
            local_socket_path,
        });

        let srv = server.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _addr)) => {
                        let srv = srv.clone();
                        tokio::spawn(async move {
                            if let Err(e) = srv.handle_connection(stream).await {
                                eprintln!("[relay] connection error: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        eprintln!("[relay] accept error: {}", e);
                    }
                }
            }
        });

        Ok(server)
    }

    /// Handle a single relay connection: authenticate, then bridge to the Unix socket.
    async fn handle_connection(
        &self,
        stream: tokio::net::TcpStream,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let auth_timeout = Duration::from_secs(5);
        let (reader_half, mut writer_half) = stream.into_split();
        let mut reader = BufReader::new(reader_half);

        // 1. Generate nonce and send challenge.
        let nonce = super::config::generate_relay_id(); // 32 hex chars
        let challenge = format!(
            "{{\"protocol\":\"cmux-relay-auth\",\"version\":1,\"relay_id\":\"{}\",\"nonce\":\"{}\"}}\n",
            self.relay_id, nonce
        );
        tokio::time::timeout(auth_timeout, writer_half.write_all(challenge.as_bytes())).await??;

        // 2. Read client response.
        let mut response_line = String::new();
        tokio::time::timeout(auth_timeout, reader.read_line(&mut response_line)).await??;

        // 3. Parse and validate.
        let response: serde_json::Value = serde_json::from_str(response_line.trim())?;
        let client_relay_id = response["relay_id"].as_str().unwrap_or("");
        let client_mac_hex = response["mac"].as_str().unwrap_or("");

        if client_relay_id != self.relay_id {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = writer_half.write_all(b"{\"ok\":false}\n").await;
            return Err("relay_id mismatch".into());
        }

        // 4. Compute expected MAC.
        let message = format!(
            "relay_id={}\nnonce={}\nversion=1",
            self.relay_id, nonce
        );
        let mut mac = HmacSha256::new_from_slice(&self.relay_token)
            .map_err(|e| format!("hmac init: {}", e))?;
        mac.update(message.as_bytes());
        let expected = mac.finalize().into_bytes();
        let expected_hex = hex_encode(&expected);

        // 5. Constant-time compare.
        if !constant_time_eq(client_mac_hex.as_bytes(), expected_hex.as_bytes()) {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = writer_half.write_all(b"{\"ok\":false}\n").await;
            return Err("MAC mismatch".into());
        }

        // 6. Auth succeeded.
        writer_half.write_all(b"{\"ok\":true}\n").await?;

        // 7. Read command from client (one line).
        let mut cmd_line = String::new();
        let read_timeout = Duration::from_secs(15);
        match tokio::time::timeout(read_timeout, reader.read_line(&mut cmd_line)).await {
            Ok(Ok(0)) | Err(_) => return Ok(()), // EOF or timeout
            Ok(Err(e)) => return Err(e.into()),
            Ok(Ok(_)) => {}
        }
        let cmd = cmd_line.trim();
        if cmd.is_empty() {
            return Ok(());
        }

        // 8. Detect JSON-RPC v2 and translate to v1 text protocol.
        let (socket_cmd, rpc_id) = translate_v2_to_v1(cmd);

        // 9. Forward to local Unix socket.
        let response = self.forward_to_socket(&socket_cmd).await?;

        // 10. If this was a JSON-RPC request, wrap the response in JSON-RPC format.
        let final_response = if let Some(id) = rpc_id {
            translate_v1_response_to_v2(&id, &response)
        } else {
            response
        };

        // 11. Send response back.
        writer_half.write_all(final_response.as_bytes()).await?;

        Ok(())
    }

    /// Connect to the local Unix socket, send a command, read the response.
    async fn forward_to_socket(
        &self,
        cmd: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        use tokio::net::UnixStream;

        let mut sock = UnixStream::connect(&self.local_socket_path).await?;

        // Write command + newline.
        sock.write_all(cmd.as_bytes()).await?;
        sock.write_all(b"\n").await?;
        sock.shutdown().await?;

        // Read full response with timeout.
        let mut response = String::new();
        tokio::time::timeout(Duration::from_secs(15), sock.read_to_string(&mut response)).await??;

        Ok(response)
    }
}

/// Decode a hex string to bytes.
fn hex_decode(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

/// Encode bytes as lowercase hex.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Constant-time byte comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ── JSON-RPC v2 ↔ v1 text protocol translation ────────────────────

/// V2 method name → v1 socket command mapping.
fn v2_method_to_v1(method: &str) -> Option<&'static str> {
    match method {
        "workspace.list" => Some("list_workspaces"),
        "workspace.create" => Some("new_workspace"),
        "workspace.close" => Some("close_workspace"),
        "workspace.select" => Some("select_workspace"),
        "workspace.current" => Some("current_workspace"),
        "surface.list" => Some("list_surfaces"),
        "surface.focus" => Some("focus_pane"),
        "surface.send_text" => Some("send"),
        "surface.close" => Some("close_surface"),
        "surface.split" => Some("split_right"),
        "pane.list" => Some("list_panes"),
        "pane.create" => Some("split_right"),
        "notification.create" => Some("log"),
        "system.capabilities" => Some("version"),
        _ => None,
    }
}

/// Try to parse a JSON-RPC v2 request and translate it to a v1 text command.
/// Returns (socket_command, Some(rpc_id)) for JSON-RPC, or (original, None) for plain text.
fn translate_v2_to_v1(cmd: &str) -> (String, Option<serde_json::Value>) {
    // Quick check: does it look like JSON?
    if !cmd.starts_with('{') {
        return (cmd.to_string(), None);
    }

    let parsed: serde_json::Value = match serde_json::from_str(cmd) {
        Ok(v) => v,
        Err(_) => return (cmd.to_string(), None),
    };

    let id = parsed.get("id").cloned();
    let method = match parsed["method"].as_str() {
        Some(m) => m,
        None => return (cmd.to_string(), None),
    };

    let v1_cmd = match v2_method_to_v1(method) {
        Some(c) => c,
        None => return (cmd.to_string(), id),
    };

    // Extract params and append as positional args where applicable.
    let params = &parsed["params"];
    let mut socket_cmd = v1_cmd.to_string();

    // Map common param keys to positional/flag arguments.
    if let Some(ws) = params.get("workspace").and_then(|v| v.as_str()) {
        socket_cmd.push(' ');
        socket_cmd.push_str(ws);
    } else if let Some(ws) = params.get("workspace").and_then(|v| v.as_u64()) {
        socket_cmd.push(' ');
        socket_cmd.push_str(&ws.to_string());
    }
    if let Some(surface) = params.get("surface").and_then(|v| v.as_str()) {
        socket_cmd.push(' ');
        socket_cmd.push_str(surface);
    } else if let Some(surface) = params.get("surface_id").and_then(|v| v.as_str()) {
        socket_cmd.push(' ');
        socket_cmd.push_str(surface);
    }
    if let Some(text) = params.get("text").and_then(|v| v.as_str()) {
        socket_cmd.push(' ');
        socket_cmd.push_str(text);
    }
    if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
        socket_cmd.push(' ');
        socket_cmd.push_str(name);
    }
    if let Some(direction) = params.get("direction").and_then(|v| v.as_str()) {
        if direction == "down" || direction == "bottom" {
            socket_cmd = "split_down".to_string();
        }
    }

    (socket_cmd, id)
}

/// Wrap a v1 text response in JSON-RPC v2 format.
fn translate_v1_response_to_v2(id: &serde_json::Value, response: &str) -> String {
    let trimmed = response.trim();

    if trimmed.starts_with("OK+") {
        // Length-prefixed multi-line response: "OK+<len>\n<data>"
        if let Some(newline_pos) = trimmed.find('\n') {
            let data = &trimmed[newline_pos + 1..];
            let result = serde_json::json!({
                "id": id,
                "ok": true,
                "result": data,
            });
            return format!("{}\n", result);
        }
    }

    if let Some(data) = trimmed.strip_prefix("OK") {
        let data = data.trim();
        let result = if data.is_empty() {
            serde_json::json!({ "id": id, "ok": true })
        } else {
            serde_json::json!({ "id": id, "ok": true, "result": data })
        };
        format!("{}\n", result)
    } else if let Some(err) = trimmed.strip_prefix("ERROR:") {
        let result = serde_json::json!({
            "id": id,
            "ok": false,
            "error": { "message": err.trim() },
        });
        format!("{}\n", result)
    } else {
        // Unknown format — pass through as result.
        let result = serde_json::json!({
            "id": id,
            "ok": true,
            "result": trimmed,
        });
        format!("{}\n", result)
    }
}
