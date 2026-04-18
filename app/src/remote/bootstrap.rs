//! SSH bootstrap chain: probe remote platform, download/upload daemon binary, hello handshake.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use super::config::{
    DaemonHello, DaemonManifest, RemoteConfiguration, REQUIRED_CAPABILITY,
};

// ── Error types ────────────────────────────────────────────────────

#[derive(Debug)]
pub enum BootstrapError {
    SshFailed { status: ExitStatus, stderr: String },
    ScpFailed { status: ExitStatus, stderr: String },
    Timeout { operation: String, seconds: u64 },
    UnsupportedPlatform { os: String, arch: String },
    ProbeFailed { detail: String },
    DownloadFailed { detail: String },
    ChecksumMismatch,
    HelloFailed { detail: String },
    MissingCapability { capability: String },
    IoError(std::io::Error),
}

impl fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SshFailed { status, stderr } => {
                write!(f, "SSH failed ({}): {}", status, stderr.trim())
            }
            Self::ScpFailed { status, stderr } => {
                write!(f, "SCP failed ({}): {}", status, stderr.trim())
            }
            Self::Timeout { operation, seconds } => {
                write!(f, "{} timed out after {}s", operation, seconds)
            }
            Self::UnsupportedPlatform { os, arch } => {
                write!(f, "unsupported platform: {}-{}", os, arch)
            }
            Self::ProbeFailed { detail } => write!(f, "platform probe failed: {}", detail),
            Self::DownloadFailed { detail } => write!(f, "binary download failed: {}", detail),
            Self::ChecksumMismatch => write!(f, "binary checksum mismatch"),
            Self::HelloFailed { detail } => write!(f, "daemon hello failed: {}", detail),
            Self::MissingCapability { capability } => {
                write!(f, "daemon missing required capability: {}", capability)
            }
            Self::IoError(e) => write!(f, "I/O error: {}", e),
        }
    }
}

impl From<std::io::Error> for BootstrapError {
    fn from(e: std::io::Error) -> Self {
        Self::IoError(e)
    }
}

// ── SSH/SCP execution helpers ──────────────────────────────────────

struct SshOutput {
    stdout: String,
    stderr: String,
    status: ExitStatus,
}

/// Run an SSH command with optional stdin, enforcing a timeout.
async fn run_ssh(
    config: &RemoteConfiguration,
    remote_command: &str,
    extra_args: &[&str],
    timeout: Duration,
) -> Result<SshOutput, BootstrapError> {
    let mut args = config.ssh_batch_args();
    for a in extra_args {
        args.push(a.to_string());
    }
    args.push(config.destination.clone());
    args.push(remote_command.to_string());

    run_command("ssh", &args, None, timeout).await
}

/// Run an SCP command with the given arguments, enforcing a timeout.
async fn run_scp(
    args: &[String],
    timeout: Duration,
) -> Result<SshOutput, BootstrapError> {
    run_command("scp", args, None, timeout).await
}

/// Spawn a command, collect stdout/stderr, enforce timeout.
async fn run_command(
    program: &str,
    args: &[String],
    stdin_data: Option<&[u8]>,
    timeout: Duration,
) -> Result<SshOutput, BootstrapError> {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    let mut cmd = Command::new(program);
    cmd.args(args);
    if stdin_data.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    } else {
        cmd.stdin(std::process::Stdio::null());
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| BootstrapError::IoError(e))?;

    if let Some(data) = stdin_data {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(data).await;
            drop(stdin);
        }
    }

    let timeout_secs = timeout.as_secs();
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| BootstrapError::Timeout {
            operation: program.to_string(),
            seconds: timeout_secs,
        })?
        .map_err(|e| BootstrapError::IoError(e))?;

    let result = SshOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        status: output.status,
    };

    if program == "ssh" && !result.status.success() {
        return Err(BootstrapError::SshFailed {
            status: result.status,
            stderr: result.stderr.clone(),
        });
    }
    if program == "scp" && !result.status.success() {
        return Err(BootstrapError::ScpFailed {
            status: result.status,
            stderr: result.stderr.clone(),
        });
    }

    Ok(result)
}

// ── Platform probe ─────────────────────────────────────────────────

/// Result of probing the remote host's platform.
#[derive(Debug)]
pub struct ProbeResult {
    pub go_os: String,
    pub go_arch: String,
    pub binary_exists: bool,
}

/// Probe the remote host to detect OS, architecture, and whether the daemon binary exists.
pub async fn probe_remote_platform(
    config: &RemoteConfiguration,
    version: &str,
) -> Result<ProbeResult, BootstrapError> {
    // Shell script matches the macOS reference — uses __LIMUX_REMOTE_*__ markers for reliable parsing.
    let script = format!(
        r#"limux_uname_os="$(uname -s)"
limux_uname_arch="$(uname -m)"
printf '%s%s\n' '__LIMUX_REMOTE_OS__=' "$limux_uname_os"
printf '%s%s\n' '__LIMUX_REMOTE_ARCH__=' "$limux_uname_arch"
case "$(printf '%s' "$limux_uname_os" | tr '[:upper:]' '[:lower:]')" in
  linux|darwin|freebsd) limux_go_os="$(printf '%s' "$limux_uname_os" | tr '[:upper:]' '[:lower:]')" ;;
  *) exit 70 ;;
esac
case "$(printf '%s' "$limux_uname_arch" | tr '[:upper:]' '[:lower:]')" in
  x86_64|amd64) limux_go_arch=amd64 ;;
  aarch64|arm64) limux_go_arch=arm64 ;;
  armv7l) limux_go_arch=arm ;;
  *) exit 71 ;;
esac
limux_remote_path="$HOME/.limux/bin/limuxd-remote/{version}/${{limux_go_os}}-${{limux_go_arch}}/limuxd-remote"
if [ -x "$limux_remote_path" ]; then
  printf '%syes\n' '__LIMUX_REMOTE_EXISTS__='
else
  printf '%sno\n' '__LIMUX_REMOTE_EXISTS__='
fi"#,
        version = version
    );

    let remote_cmd = format!("sh -c '{}'", script.replace('\'', "'\\''"));
    let output = run_ssh(config, &remote_cmd, &[], Duration::from_secs(20)).await?;

    let mut raw_os = None;
    let mut raw_arch = None;
    let mut exists = false;

    for line in output.stdout.lines() {
        if let Some(val) = line.strip_prefix("__LIMUX_REMOTE_OS__=") {
            raw_os = Some(val.to_string());
        } else if let Some(val) = line.strip_prefix("__LIMUX_REMOTE_ARCH__=") {
            raw_arch = Some(val.to_string());
        } else if let Some(val) = line.strip_prefix("__LIMUX_REMOTE_EXISTS__=") {
            exists = val.trim() == "yes";
        }
    }

    let raw_os = raw_os.ok_or_else(|| BootstrapError::ProbeFailed {
        detail: "missing __LIMUX_REMOTE_OS__ in probe output".into(),
    })?;
    let raw_arch = raw_arch.ok_or_else(|| BootstrapError::ProbeFailed {
        detail: "missing __LIMUX_REMOTE_ARCH__ in probe output".into(),
    })?;

    let go_os = match raw_os.to_lowercase().as_str() {
        "linux" => "linux",
        "darwin" => "darwin",
        "freebsd" => "freebsd",
        _ => {
            return Err(BootstrapError::UnsupportedPlatform {
                os: raw_os,
                arch: raw_arch,
            })
        }
    }
    .to_string();

    let go_arch = match raw_arch.to_lowercase().as_str() {
        "x86_64" | "amd64" => "amd64",
        "aarch64" | "arm64" => "arm64",
        "armv7l" => "arm",
        _ => {
            return Err(BootstrapError::UnsupportedPlatform {
                os: raw_os,
                arch: raw_arch,
            })
        }
    }
    .to_string();

    Ok(ProbeResult {
        go_os,
        go_arch,
        binary_exists: exists,
    })
}

// ── Local binary cache ─────────────────────────────────────────────

/// Cache directory for downloaded daemon binaries.
fn cache_dir() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".cache")
        });
    base.join("limux").join("remote-daemons")
}

/// Path to the cached daemon binary for a given platform and version.
fn cached_binary_path(go_os: &str, go_arch: &str, version: &str) -> PathBuf {
    cache_dir()
        .join(version)
        .join(format!("{}-{}", go_os, go_arch))
        .join("limuxd-remote")
}

/// Ensure the daemon binary is available locally.
///
/// Priority chain:
/// 1. `LIMUX_REMOTE_DAEMON_BINARY` env var — explicit path, fail hard if invalid
/// 2. Cached binary at `$XDG_CACHE_HOME/limux/remote-daemons/<version>/<os>-<arch>/`
/// 3. Manifest download + SHA-256 verification
pub async fn ensure_local_binary(
    go_os: &str,
    go_arch: &str,
    version: &str,
) -> Result<PathBuf, BootstrapError> {
    // 1. Explicit override — highest priority, fail hard if set but invalid.
    if let Ok(override_path) = std::env::var("LIMUX_REMOTE_DAEMON_BINARY") {
        if !override_path.is_empty() {
            let p = PathBuf::from(&override_path);
            if !p.exists() {
                return Err(BootstrapError::DownloadFailed {
                    detail: format!("LIMUX_REMOTE_DAEMON_BINARY path does not exist: {}", override_path),
                });
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&p)?.permissions().mode();
                if mode & 0o111 == 0 {
                    return Err(BootstrapError::DownloadFailed {
                        detail: format!("LIMUX_REMOTE_DAEMON_BINARY is not executable: {}", override_path),
                    });
                }
            }
            eprintln!("[remote] using override binary: {}", override_path);
            return Ok(p);
        }
    }

    // 2. Check cache.
    let path = cached_binary_path(go_os, go_arch, version);
    if path.exists() {
        return Ok(path);
    }

    // 3. Try to load manifest and download.
    let manifest = load_manifest(version).await?;
    let entry = manifest
        .find_entry(go_os, go_arch)
        .ok_or_else(|| BootstrapError::DownloadFailed {
            detail: format!("no manifest entry for {}-{}", go_os, go_arch),
        })?;

    // Download the binary.
    let tmp_path = path.with_extension("tmp");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    download_binary(&entry.download_url, &tmp_path).await?;

    // Verify SHA-256.
    verify_sha256(&tmp_path, &entry.sha256)?;

    // Set executable permissions and move into place.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp_path, &path)?;

    Ok(path)
}

/// Load the daemon manifest, either from embedded data or by fetching from the release URL.
async fn load_manifest(version: &str) -> Result<DaemonManifest, BootstrapError> {
    // Check for embedded manifest (set at build time).
    let embedded = option_env!("LIMUX_REMOTE_MANIFEST_JSON").unwrap_or("");
    if !embedded.is_empty() {
        if let Ok(m) = serde_json::from_str::<DaemonManifest>(embedded) {
            if m.app_version == version {
                return Ok(m);
            }
        }
    }

    // Fetch live manifest from release URL.
    let url = format!(
        "https://github.com/RyantHults/limux/releases/download/limuxd-remote-v{}/limuxd-remote-manifest.json",
        version
    );
    let tmp = cache_dir().join(format!("manifest-{}.json.tmp", version));
    if let Some(parent) = tmp.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    download_binary(&url, &tmp).await?;
    let data = std::fs::read_to_string(&tmp).map_err(|e| BootstrapError::DownloadFailed {
        detail: format!("read manifest: {}", e),
    })?;
    let _ = std::fs::remove_file(&tmp);
    serde_json::from_str(&data).map_err(|e| BootstrapError::DownloadFailed {
        detail: format!("parse manifest: {}", e),
    })
}

/// Download a file using curl (fallback to wget).
async fn download_binary(url: &str, dest: &Path) -> Result<(), BootstrapError> {
    let dest_str = dest.to_string_lossy().to_string();

    // Try curl first.
    let curl_args: Vec<String> = vec![
        "-fsSL".into(),
        "-o".into(),
        dest_str.clone(),
        url.into(),
    ];
    let result = run_command("curl", &curl_args, None, Duration::from_secs(120)).await;
    if result.is_ok() {
        return Ok(());
    }

    // Fall back to wget.
    let wget_args: Vec<String> = vec!["-q".into(), "-O".into(), dest_str, url.into()];
    run_command("wget", &wget_args, None, Duration::from_secs(120))
        .await
        .map_err(|_| BootstrapError::DownloadFailed {
            detail: format!("both curl and wget failed for {}", url),
        })?;
    Ok(())
}

/// Verify the SHA-256 checksum of a file.
fn verify_sha256(path: &Path, expected_hex: &str) -> Result<(), BootstrapError> {
    use sha2::{Digest, Sha256};

    let data = std::fs::read(path)?;
    let hash = Sha256::digest(&data);
    let actual_hex = format!("{:x}", hash);

    if actual_hex != expected_hex.to_lowercase() {
        let _ = std::fs::remove_file(path);
        return Err(BootstrapError::ChecksumMismatch);
    }
    Ok(())
}

// ── Upload to remote ───────────────────────────────────────────────

/// Remote daemon binary path template (with `$HOME` for shell expansion in SSH commands).
pub fn remote_daemon_path(go_os: &str, go_arch: &str, version: &str) -> String {
    format!(
        "$HOME/.limux/bin/limuxd-remote/{}/{}-{}/limuxd-remote",
        version, go_os, go_arch
    )
}

/// Remote daemon binary path with `~` prefix (for SCP, which expands `~` but not `$HOME`).
fn remote_daemon_path_scp(go_os: &str, go_arch: &str, version: &str) -> String {
    format!(
        ".limux/bin/limuxd-remote/{}/{}-{}/limuxd-remote",
        version, go_os, go_arch
    )
}

/// Upload the daemon binary to the remote host via SCP.
pub async fn upload_daemon_binary(
    config: &RemoteConfiguration,
    local_path: &Path,
    remote_path: &str,
) -> Result<(), BootstrapError> {
    // Step 1: Create remote directory.
    let remote_dir = remote_path.rsplit_once('/').map(|(d, _)| d).unwrap_or(".");
    let mkdir_cmd = format!("sh -c 'mkdir -p {}'", remote_dir);
    run_ssh(config, &mkdir_cmd, &[], Duration::from_secs(12)).await?;

    // Step 2: SCP upload to temp path.
    // SCP doesn't expand $HOME — use a relative path (relative to remote home dir).
    let scp_path = remote_path.strip_prefix("$HOME/").unwrap_or(remote_path);
    let rand_suffix: u32 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let temp_scp = format!("{}.tmp-{:08x}", scp_path, rand_suffix);

    let mut scp_args = Vec::new();
    scp_args.push("-q".to_string());
    if !config.has_ssh_option("StrictHostKeyChecking") {
        scp_args.push("-o".into());
        scp_args.push("StrictHostKeyChecking=accept-new".into());
    }
    scp_args.push("-o".into());
    scp_args.push("ControlMaster=no".into());
    if let Some(port) = config.port {
        scp_args.push("-P".into());
        scp_args.push(port.to_string());
    }
    if let Some(ref identity) = config.identity_file {
        scp_args.push("-i".into());
        scp_args.push(identity.clone());
    }
    for opt in &config.ssh_options {
        scp_args.push("-o".into());
        scp_args.push(opt.clone());
    }
    scp_args.push(local_path.to_string_lossy().into_owned());
    scp_args.push(format!("{}:{}", config.destination, temp_scp));

    run_scp(&scp_args, Duration::from_secs(45)).await?;

    // Step 3: chmod + mv to final path (uses $HOME, expanded by remote shell).
    let temp_remote = format!("{}.tmp-{:08x}", remote_path, rand_suffix);
    let finalize_cmd = format!(
        "sh -c 'chmod 755 {} && mv {} {}'",
        temp_remote, temp_remote, remote_path
    );
    run_ssh(config, &finalize_cmd, &[], Duration::from_secs(12)).await?;

    Ok(())
}

// ── Remote metadata installation ───────────────────────────────────

/// Install relay metadata and the limux CLI wrapper on the remote host.
///
/// Creates:
/// - `~/.limux/relay/<port>.auth` — JSON with relay_id and relay_token
/// - `~/.limux/relay/<port>.daemon_path` — path to daemon binary
/// - `~/.limux/socket_addr` — relay address (127.0.0.1:<port>)
/// - `~/.limux/bin/limux` — CLI wrapper script
pub async fn install_remote_metadata(
    config: &RemoteConfiguration,
    relay_port: u16,
    relay_id: &str,
    relay_token: &str,
    daemon_remote_path: &str,
) -> Result<(), BootstrapError> {
    let wrapper_script = r#"#!/usr/bin/env bash
set -euo pipefail
daemon="$HOME/.limux/bin/limuxd-remote-current"
socket_path="${LIMUX_SOCKET_PATH:-}"
if [ -z "$socket_path" ] && [ -r "$HOME/.limux/socket_addr" ]; then
  socket_path="$(tr -d '\r\n' < "$HOME/.limux/socket_addr")"
fi
if [ -n "$socket_path" ] && [ "${socket_path#/}" = "$socket_path" ] && [ "${socket_path#*:}" != "$socket_path" ]; then
  relay_port="${socket_path##*:}"
  relay_map="$HOME/.limux/relay/${relay_port}.daemon_path"
  if [ -r "$relay_map" ]; then
    mapped_daemon="$(tr -d '\r\n' < "$relay_map")"
    if [ -n "$mapped_daemon" ] && [ -x "$mapped_daemon" ]; then
      daemon="$mapped_daemon"
    fi
  fi
fi
exec "$daemon" "$@"
"#;

    let install_script = format!(
        r#"umask 077
mkdir -p "$HOME/.limux" "$HOME/.limux/relay" "$HOME/.limux/bin"
chmod 700 "$HOME/.limux/relay"
cat > "$HOME/.limux/bin/limux" <<'LIMUXWRAPPER'
{wrapper}
LIMUXWRAPPER
chmod 755 "$HOME/.limux/bin/limux"
printf '%s' "{daemon_path}" > "$HOME/.limux/relay/{port}.daemon_path"
cat > "$HOME/.limux/relay/{port}.auth" <<'LIMUXRELAYAUTH'
{{"relay_id":"{relay_id}","relay_token":"{relay_token}"}}
LIMUXRELAYAUTH
chmod 600 "$HOME/.limux/relay/{port}.auth"
printf '%s' '127.0.0.1:{port}' > "$HOME/.limux/socket_addr"
"#,
        wrapper = wrapper_script,
        daemon_path = daemon_remote_path,
        port = relay_port,
        relay_id = relay_id,
        relay_token = relay_token,
    );

    let remote_cmd = format!("sh -c '{}'", install_script.replace('\'', "'\\''"));
    run_ssh(config, &remote_cmd, &[], Duration::from_secs(12)).await?;

    Ok(())
}

// ── Hello handshake ────────────────────────────────────────────────

/// Send a hello RPC to the remote daemon and verify capabilities.
pub async fn hello_handshake(
    config: &RemoteConfiguration,
    remote_path: &str,
) -> Result<DaemonHello, BootstrapError> {
    let hello_json = r#"{"id":1,"method":"hello","params":{}}"#;
    let remote_cmd = format!(
        "sh -c 'printf \"%s\\n\" '\"'\"'{}'\"'\"' | {} serve --stdio'",
        hello_json, remote_path
    );

    let output = run_ssh(
        config,
        &remote_cmd,
        &["-T", "-S", "none", "-o", "RequestTTY=no"],
        Duration::from_secs(12),
    )
    .await?;

    // Parse first non-empty line as JSON.
    let response_line = output
        .stdout
        .lines()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| BootstrapError::HelloFailed {
            detail: "empty response from daemon".into(),
        })?;

    let parsed: serde_json::Value =
        serde_json::from_str(response_line).map_err(|e| BootstrapError::HelloFailed {
            detail: format!("invalid JSON: {}", e),
        })?;

    let ok = parsed["ok"].as_bool().unwrap_or(false);
    if !ok {
        let err_msg = parsed["error"]["message"]
            .as_str()
            .unwrap_or("unknown error");
        return Err(BootstrapError::HelloFailed {
            detail: err_msg.to_string(),
        });
    }

    let result = &parsed["result"];
    let name = result["name"].as_str().unwrap_or("limuxd-remote").to_string();
    let version = result["version"].as_str().unwrap_or("dev").to_string();
    let capabilities: Vec<String> = result["capabilities"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    if !capabilities.iter().any(|c| c == REQUIRED_CAPABILITY) {
        return Err(BootstrapError::MissingCapability {
            capability: REQUIRED_CAPABILITY.to_string(),
        });
    }

    Ok(DaemonHello {
        name,
        version,
        capabilities,
        remote_path: remote_path.to_string(),
    })
}

// ── Full bootstrap orchestrator ────────────────────────────────────

/// Run the full bootstrap chain: probe → ensure binary → upload → hello.
///
/// If hello fails on an existing binary, re-uploads and retries once.
pub async fn bootstrap_daemon(
    config: &RemoteConfiguration,
    version: &str,
) -> Result<DaemonHello, BootstrapError> {
    // 1. Probe remote platform.
    let probe = probe_remote_platform(config, version).await?;
    let remote_path = remote_daemon_path(&probe.go_os, &probe.go_arch, version);

    // 2. Ensure local binary is cached.
    let local_binary = ensure_local_binary(&probe.go_os, &probe.go_arch, version).await?;

    // 3. Upload if not present on remote.
    if !probe.binary_exists {
        upload_daemon_binary(config, &local_binary, &remote_path).await?;
    }

    // 4. Hello handshake.
    match hello_handshake(config, &remote_path).await {
        Ok(hello) => return Ok(hello),
        Err(e) if probe.binary_exists => {
            // Binary existed but hello failed — re-upload fresh copy and retry.
            eprintln!("[remote] hello failed on existing binary ({}), re-uploading", e);
            upload_daemon_binary(config, &local_binary, &remote_path).await?;
            hello_handshake(config, &remote_path).await
        }
        Err(e) => Err(e),
    }
}
