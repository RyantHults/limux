//! Remote SSH configuration and state types.

use serde::{Deserialize, Serialize};

/// Per-workspace remote SSH configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteConfiguration {
    /// SSH destination (user@host or host).
    pub destination: String,
    /// SSH port (None = default 22).
    #[serde(default)]
    pub port: Option<u16>,
    /// Path to SSH private key.
    #[serde(default)]
    pub identity_file: Option<String>,
    /// Custom SSH options (e.g., ["ProxyCommand=...", "ForwardAgent=yes"]).
    #[serde(default)]
    pub ssh_options: Vec<String>,
    /// Command to run on remote host after connection.
    #[serde(default)]
    pub terminal_startup_command: Option<String>,

    // Phase 5b relay fields — included for forward-compatible serialization.
    #[serde(default)]
    pub relay_port: Option<u16>,
    #[serde(default)]
    pub relay_id: Option<String>,
    #[serde(default)]
    pub relay_token: Option<String>,
    #[serde(default)]
    pub local_socket_path: Option<String>,
}

impl RemoteConfiguration {
    /// Display string: "user@host" or "user@host:port".
    pub fn display_target(&self) -> String {
        match self.port {
            Some(p) => format!("{}:{}", self.destination, p),
            None => self.destination.clone(),
        }
    }

    /// Build SSH arguments for an interactive terminal session.
    ///
    /// Includes keepalive options, port, identity, and custom options.
    /// Does NOT include BatchMode or ControlMaster restrictions.
    pub fn ssh_interactive_args(&self) -> Vec<String> {
        let mut args = vec![
            "-o".into(),
            "ServerAliveInterval=20".into(),
            "-o".into(),
            "ServerAliveCountMax=2".into(),
        ];
        if !self.has_ssh_option("StrictHostKeyChecking") {
            args.push("-o".into());
            args.push("StrictHostKeyChecking=accept-new".into());
        }
        self.append_connection_args(&mut args);
        args
    }

    /// Build SSH arguments for background/batch operations (probe, upload, hello).
    ///
    /// Adds BatchMode=yes and ControlMaster=no on top of the common options.
    pub fn ssh_batch_args(&self) -> Vec<String> {
        let mut args = vec![
            "-o".into(),
            "ConnectTimeout=6".into(),
            "-o".into(),
            "ServerAliveInterval=20".into(),
            "-o".into(),
            "ServerAliveCountMax=2".into(),
        ];
        if !self.has_ssh_option("StrictHostKeyChecking") {
            args.push("-o".into());
            args.push("StrictHostKeyChecking=accept-new".into());
        }
        args.push("-o".into());
        args.push("BatchMode=yes".into());
        args.push("-o".into());
        args.push("ControlMaster=no".into());
        self.append_connection_args(&mut args);
        args
    }

    /// Build the full SSH command string for launching a terminal.
    pub fn ssh_command(&self) -> String {
        if let Some(ref cmd) = self.terminal_startup_command {
            return cmd.clone();
        }
        let args = self.ssh_interactive_args();
        let mut parts = vec!["ssh".to_string()];
        parts.extend(args);
        parts.push(self.destination.clone());
        parts.join(" ")
    }

    /// Check if a given SSH option key is already set in ssh_options.
    pub fn has_ssh_option(&self, key: &str) -> bool {
        self.ssh_options
            .iter()
            .any(|o| o.starts_with(key) && (o.len() == key.len() || o.as_bytes()[key.len()] == b'='))
    }

    /// Append port, identity file, and custom SSH options to an argument list.
    fn append_connection_args(&self, args: &mut Vec<String>) {
        if let Some(port) = self.port {
            args.push("-p".into());
            args.push(port.to_string());
        }
        if let Some(ref identity) = self.identity_file {
            args.push("-i".into());
            args.push(identity.clone());
        }
        for opt in &self.ssh_options {
            args.push("-o".into());
            args.push(opt.clone());
        }
    }
}

/// Connection lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Error,
}

impl Default for RemoteConnectionState {
    fn default() -> Self {
        Self::Disconnected
    }
}

impl RemoteConnectionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Error => "error",
        }
    }
}

/// Daemon bootstrap state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteDaemonState {
    Unavailable,
    Bootstrapping,
    Ready,
    Error,
}

impl Default for RemoteDaemonState {
    fn default() -> Self {
        Self::Unavailable
    }
}

// ── Credential generation ──────────────────────────────────────────

/// Generate a relay ID (16 random bytes → 32 hex chars).
pub fn generate_relay_id() -> String {
    random_hex(16)
}

/// Generate a relay token (32 random bytes → 64 hex chars).
pub fn generate_relay_token() -> String {
    random_hex(32)
}

/// Read random bytes from /dev/urandom and format as lowercase hex.
fn random_hex(n: usize) -> String {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    }
    buf.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Pick an available high port for the relay by binding to port 0 and reading the assigned port.
pub fn pick_relay_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
}

/// Status information about the remote daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteDaemonStatus {
    pub state: RemoteDaemonState,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub remote_path: Option<String>,
}

// ── Daemon manifest types ──────────────────────────────────────────

/// Release manifest for daemon binaries (matches the JSON published alongside releases).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonManifest {
    pub schema_version: u32,
    pub app_version: String,
    pub release_tag: String,
    pub release_url: String,
    pub entries: Vec<ManifestEntry>,
}

impl DaemonManifest {
    /// Find the entry matching a given OS and architecture.
    pub fn find_entry(&self, go_os: &str, go_arch: &str) -> Option<&ManifestEntry> {
        self.entries
            .iter()
            .find(|e| e.go_os == go_os && e.go_arch == go_arch)
    }
}

/// A single platform entry in the daemon manifest.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntry {
    pub go_os: String,
    pub go_arch: String,
    pub download_url: String,
    pub sha256: String,
}

// ── Hello handshake result ─────────────────────────────────────────

/// Successful result from the daemon hello RPC.
#[derive(Debug, Clone)]
pub struct DaemonHello {
    pub name: String,
    pub version: String,
    pub capabilities: Vec<String>,
    pub remote_path: String,
}

/// The capability that must be present in the daemon hello response.
pub const REQUIRED_CAPABILITY: &str = "proxy.stream.push";

// ── Proxy endpoint ────────────────────────────────────────────────

/// A resolved proxy endpoint (host + port) for browser tunneling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyEndpoint {
    pub host: String,
    pub port: u16,
}

impl ProxyEndpoint {
    /// Format as a SOCKS5 proxy URI for WebKitGTK.
    pub fn socks5_uri(&self) -> String {
        format!("socks5://{}:{}", self.host, self.port)
    }
}

// ── Transport key ─────────────────────────────────────────────────

impl RemoteConfiguration {
    /// Compute a transport key that uniquely identifies the SSH transport.
    ///
    /// Two configurations with the same transport key can share a single
    /// proxy tunnel. The key is derived from destination, port, identity
    /// file, and sorted ssh_options.
    pub fn transport_key(&self) -> String {
        let mut parts = Vec::new();
        parts.push(format!("dest={}", self.destination));
        if let Some(p) = self.port {
            parts.push(format!("port={}", p));
        }
        if let Some(ref id) = self.identity_file {
            parts.push(format!("id={}", id));
        }
        let mut opts = self.ssh_options.clone();
        opts.sort();
        for opt in &opts {
            parts.push(format!("opt={}", opt));
        }
        parts.join("|")
    }
}
