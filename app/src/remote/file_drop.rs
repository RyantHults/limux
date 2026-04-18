//! SCP-based file upload for drag-and-drop onto remote terminals.
//!
//! When files are dropped onto a terminal pane in a remote workspace,
//! this module uploads them to the remote host via SCP and returns
//! the remote paths for pasting into the terminal.

use std::path::PathBuf;

use super::config::RemoteConfiguration;

/// Upload local files to a remote host via SCP.
///
/// Uploads to `/tmp/limux-drop-<random>/` on the remote, preserving filenames.
/// Returns the remote paths on success.
pub fn upload_files(
    config: &RemoteConfiguration,
    local_paths: &[PathBuf],
) -> Result<Vec<String>, String> {
    if local_paths.is_empty() {
        return Ok(Vec::new());
    }

    // Create a random remote directory.
    let random_suffix = super::config::generate_relay_id(); // reuse hex generator
    let remote_dir = format!("/tmp/limux-drop-{random_suffix}");

    // Create the remote directory.
    let mkdir_args = build_ssh_args(config, &format!("mkdir -p '{remote_dir}'"));
    let status = std::process::Command::new("ssh")
        .args(&mkdir_args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("failed to run ssh mkdir: {e}"))?;

    if !status.success() {
        return Err(format!("ssh mkdir failed with status {status}"));
    }

    // SCP each file to the remote directory.
    let mut remote_paths = Vec::new();
    for local_path in local_paths {
        let filename = local_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        let remote_path = format!("{remote_dir}/{filename}");

        let mut scp_args = scp_base_args(config);
        scp_args.push(local_path.to_string_lossy().into_owned());
        scp_args.push(format!("{}:{remote_path}", config.destination));

        let status = std::process::Command::new("scp")
            .args(&scp_args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("failed to run scp: {e}"))?;

        if !status.success() {
            return Err(format!("scp failed for {}: status {status}", local_path.display()));
        }

        remote_paths.push(remote_path);
    }

    Ok(remote_paths)
}

/// Build SSH arguments for a remote command.
fn build_ssh_args(config: &RemoteConfiguration, command: &str) -> Vec<String> {
    let mut args = config.ssh_batch_args();
    args.push(config.destination.clone());
    args.push(command.to_string());
    args
}

/// Build SCP base arguments (port, identity, options — no source/dest).
fn scp_base_args(config: &RemoteConfiguration) -> Vec<String> {
    let mut args = Vec::new();
    args.push("-o".into());
    args.push("BatchMode=yes".into());
    args.push("-o".into());
    args.push("StrictHostKeyChecking=accept-new".into());
    if let Some(port) = config.port {
        args.push("-P".into()); // SCP uses -P (uppercase) for port
        args.push(port.to_string());
    }
    if let Some(ref identity) = config.identity_file {
        args.push("-i".into());
        args.push(identity.clone());
    }
    for opt in &config.ssh_options {
        args.push("-o".into());
        args.push(opt.clone());
    }
    args
}
