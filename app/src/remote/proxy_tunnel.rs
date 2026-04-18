//! Local TCP listener that bridges browser connections to the remote host
//! through the RPC client. Supports SOCKS5 and HTTP CONNECT protocols.
//!
//! Each accepted connection:
//! 1. Detects protocol (first byte 0x05 = SOCKS5, otherwise HTTP CONNECT)
//! 2. Parses the target host:port
//! 3. Opens a remote stream via `proxy.open` RPC
//! 4. Bridges data bidirectionally: local→remote via `proxy.write`,
//!    remote→local via stream event channel

use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::rpc::{RpcClient, RpcError};

/// A running proxy tunnel: a local TCP listener that forwards connections
/// to the remote host via the RPC client.
pub struct ProxyTunnel {
    /// The local address the tunnel is listening on.
    pub port: u16,
    /// Handle to stop the accept loop.
    cancel: tokio::sync::watch::Sender<bool>,
}

impl ProxyTunnel {
    /// Start a new proxy tunnel. Binds to `127.0.0.1:0` and returns immediately.
    ///
    /// The RPC client must already be connected. The tunnel spawns a background
    /// task that accepts connections until `shutdown()` is called.
    pub async fn start(rpc: Arc<RpcClient>) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();

        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

        tokio::spawn(accept_loop(listener, rpc, cancel_rx));

        Ok(Self {
            port,
            cancel: cancel_tx,
        })
    }

    /// Shut down the tunnel, stopping the accept loop.
    pub fn shutdown(&self) {
        let _ = self.cancel.send(true);
    }
}

impl Drop for ProxyTunnel {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn accept_loop(
    listener: TcpListener,
    rpc: Arc<RpcClient>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        let rpc = rpc.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, rpc).await {
                                eprintln!("[proxy-tunnel] session error: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        eprintln!("[proxy-tunnel] accept error: {e}");
                        break;
                    }
                }
            }
            _ = cancel.changed() => {
                break;
            }
        }
    }
}

/// Handle a single inbound connection: detect protocol, parse target, bridge.
async fn handle_connection(mut stream: TcpStream, rpc: Arc<RpcClient>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Peek at the first byte to detect the protocol.
    let mut first = [0u8; 1];
    stream.peek(&mut first).await?;

    let is_socks5 = first[0] == 0x05;
    let protocol = if is_socks5 { "SOCKS5" } else { "HTTP CONNECT" };
    let (host, port) = if is_socks5 {
        socks5_handshake(&mut stream).await?
    } else {
        http_connect_handshake(&mut stream).await?
    };

    eprintln!("[proxy-tunnel] {protocol} session: {host}:{port}");

    // Open a remote proxy stream.
    let open_result = match rpc
        .call(
            "proxy.open",
            serde_json::json!({ "host": host, "port": port }),
        )
        .await
    {
        Ok(result) => result,
        Err(e) => {
            let msg = format!("proxy.open failed: {e}");
            eprintln!("[proxy-tunnel] {msg}");
            // Send a proper error reply so the client gets a meaningful error
            // instead of "connection to proxy closed".
            if is_socks5 {
                // SOCKS5 reply: VER=5, REP=5 (connection refused), RSV=0, ATYP=1, BIND=0.0.0.0:0
                let _ = stream
                    .write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                    .await;
            } else {
                let _ = stream
                    .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                    .await;
            }
            return Err(msg.into());
        }
    };

    let stream_id = open_result
        .get("stream_id")
        .and_then(|v| v.as_str())
        .ok_or("proxy.open did not return stream_id")?
        .to_string();

    // Register local event handler FIRST so we don't miss early data,
    // then tell the daemon to start pushing events for this stream.
    let mut event_rx = rpc.subscribe(&stream_id).await;

    let sub_result = rpc.call(
        "proxy.stream.subscribe",
        serde_json::json!({ "stream_id": stream_id }),
    )
    .await
    .map_err(|e| format!("proxy.stream.subscribe failed: {e}"))?;

    // Send success response back to the client.
    if is_socks5 {
        // SOCKS5 success reply: VER=5, REP=0 (succeeded), RSV=0, ATYP=1 (IPv4), BIND.ADDR=0.0.0.0, BIND.PORT=0
        stream
            .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;
    } else {
        stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
    }

    // Bridge: bidirectional data forwarding.
    let (mut read_half, mut write_half) = stream.into_split();
    let rpc_write = rpc.clone();
    let sid_close = stream_id.clone();

    // local → remote
    let sid_write = stream_id.clone();
    let local_to_remote = async move {
        use base64::Engine as _;
        let mut buf = vec![0u8; 16384];
        loop {
            let n = match read_half.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            let encoded = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
            if rpc_write
                .call(
                    "proxy.write",
                    serde_json::json!({
                        "stream_id": sid_write,
                        "data_base64": encoded,
                    }),
                )
                .await
                .is_err()
            {
                break;
            }
        }
    };

    // remote → local
    let remote_to_local = async move {
        use base64::Engine as _;
        while let Some(evt) = event_rx.recv().await {
            if evt.event == "proxy.stream.data" {
                if let Some(data_b64) = evt.data.get("data_base64").and_then(|v| v.as_str()) {
                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data_b64) {
                        if write_half.write_all(&bytes).await.is_err() {
                            break;
                        }
                    }
                }
            } else if evt.event == "proxy.stream.eof" || evt.event == "proxy.stream.close" {
                break;
            }
        }
    };

    // Run both directions concurrently; when either finishes, clean up.
    tokio::select! {
        _ = local_to_remote => {}
        _ = remote_to_local => {}
    }

    // Clean up the remote stream.
    let _ = rpc
        .call(
            "proxy.close",
            serde_json::json!({ "stream_id": sid_close }),
        )
        .await;
    rpc.unsubscribe(&sid_close).await;

    Ok(())
}

// ── SOCKS5 handshake ──────────────────────────────────────────────────

async fn socks5_handshake(
    stream: &mut TcpStream,
) -> Result<(String, u16), Box<dyn std::error::Error + Send + Sync>> {
    // Greeting: VER | NMETHODS | METHODS...
    let mut header = [0u8; 2];
    stream.read_exact(&mut header).await?;
    if header[0] != 0x05 {
        return Err("not SOCKS5".into());
    }
    let nmethods = header[1] as usize;
    let mut methods = vec![0u8; nmethods];
    stream.read_exact(&mut methods).await?;

    // Reply: no authentication required
    stream.write_all(&[0x05, 0x00]).await?;

    // Request: VER | CMD | RSV | ATYP | DST.ADDR | DST.PORT
    let mut req_header = [0u8; 4];
    stream.read_exact(&mut req_header).await?;
    if req_header[1] != 0x01 {
        // Only CONNECT (0x01) supported
        stream
            .write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;
        return Err("unsupported SOCKS5 command".into());
    }

    let host = match req_header[3] {
        0x01 => {
            // IPv4
            let mut addr = [0u8; 4];
            stream.read_exact(&mut addr).await?;
            format!("{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3])
        }
        0x03 => {
            // Domain name
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut domain = vec![0u8; len[0] as usize];
            stream.read_exact(&mut domain).await?;
            String::from_utf8(domain)?
        }
        0x04 => {
            // IPv6
            let mut addr = [0u8; 16];
            stream.read_exact(&mut addr).await?;
            let parts: Vec<String> = (0..8)
                .map(|i| format!("{:x}", u16::from_be_bytes([addr[i * 2], addr[i * 2 + 1]])))
                .collect();
            parts.join(":")
        }
        _ => return Err("unsupported SOCKS5 address type".into()),
    };

    let mut port_bytes = [0u8; 2];
    stream.read_exact(&mut port_bytes).await?;
    let port = u16::from_be_bytes(port_bytes);

    Ok((host, port))
}

// ── HTTP CONNECT handshake ────────────────────────────────────────────

async fn http_connect_handshake(
    stream: &mut TcpStream,
) -> Result<(String, u16), Box<dyn std::error::Error + Send + Sync>> {
    // Buffer until we see \r\n\r\n
    let mut buf = Vec::with_capacity(4096);
    loop {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).await?;
        buf.push(byte[0]);
        if buf.len() >= 4 && &buf[buf.len() - 4..] == b"\r\n\r\n" {
            break;
        }
        if buf.len() > 8192 {
            return Err("HTTP CONNECT request too large".into());
        }
    }

    let request = String::from_utf8_lossy(&buf);
    let first_line = request.lines().next().unwrap_or("");

    // Parse "CONNECT host:port HTTP/1.x"
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 || !parts[0].eq_ignore_ascii_case("CONNECT") {
        return Err(format!("not an HTTP CONNECT request: {first_line}").into());
    }

    let authority = parts[1];
    let (host, port) = if let Some(colon_pos) = authority.rfind(':') {
        let host = &authority[..colon_pos];
        let port: u16 = authority[colon_pos + 1..]
            .parse()
            .map_err(|_| format!("invalid port in CONNECT: {authority}"))?;
        (host.to_string(), port)
    } else {
        (authority.to_string(), 443)
    };

    Ok((host, port))
}
