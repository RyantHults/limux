//! Shell bootstrap script generation for remote SSH sessions.
//!
//! Generates the SSH terminal startup command that configures the remote shell
//! environment with PATH, CMUX_SOCKET_PATH, and other variables so that
//! `cmux` commands work from the remote host.

use super::config::RemoteConfiguration;

/// Generate the full SSH command for launching a remote terminal with env vars configured.
///
/// The command SSHes to the destination and runs an inline bootstrap script that:
/// 1. Exports environment variables (PATH, CMUX_SOCKET_PATH, COLORTERM, TERM_PROGRAM)
/// 2. Execs an interactive login shell
pub fn generate_startup_command(config: &RemoteConfiguration, relay_port: u16) -> String {
    let bootstrap = generate_bootstrap_script(relay_port);

    let mut parts = vec!["ssh".to_string()];
    parts.extend(config.ssh_interactive_args());
    // Request a TTY since we're running an inline command that execs a shell.
    parts.push("-t".into());
    parts.push(config.destination.clone());
    // Pass the bootstrap script as the remote command.
    // Single-quote the script, escaping any embedded single quotes.
    let escaped = bootstrap.replace('\'', "'\\''");
    parts.push(format!("'{}'", escaped));

    parts.join(" ")
}

/// Generate the inline bootstrap script that runs on the remote host.
fn generate_bootstrap_script(relay_port: u16) -> String {
    format!(
        r#"export PATH="$HOME/.cmux/bin:$PATH"
export CMUX_SOCKET_PATH='127.0.0.1:{relay_port}'
export COLORTERM='truecolor'
export TERM_PROGRAM='ghostty'
exec "$SHELL" -il"#,
        relay_port = relay_port
    )
}
