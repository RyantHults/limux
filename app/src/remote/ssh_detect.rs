//! Detect foreground SSH sessions on a terminal TTY via `/proc`.
//!
//! Reads `/proc/<pid>/stat` for process group and TTY info, and
//! `/proc/<pid>/cmdline` for the SSH command-line arguments. Parses
//! the arguments into a `DetectedSSHSession` that can be used to
//! build SCP commands for file upload.

use std::fs;
use std::path::Path;

use super::config::RemoteConfiguration;

/// A detected SSH session parsed from a foreground process's command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedSSHSession {
    pub destination: String,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
    pub ssh_options: Vec<String>,
}

impl DetectedSSHSession {
    /// Convert to a `RemoteConfiguration` for SCP operations.
    pub fn to_remote_config(&self) -> RemoteConfiguration {
        RemoteConfiguration {
            destination: self.destination.clone(),
            port: self.port,
            identity_file: self.identity_file.clone(),
            ssh_options: self.ssh_options.clone(),
            terminal_startup_command: None,
            relay_port: None,
            relay_id: None,
            relay_token: None,
            local_socket_path: None,
        }
    }
}

/// Detect a foreground SSH session on the given TTY.
///
/// `tty_path` is the full path like `/dev/pts/5`. Returns `None` if no
/// foreground SSH process is found.
pub fn detect(tty_path: &str) -> Option<DetectedSSHSession> {
    let tty_nr = tty_path_to_nr(tty_path)?;

    // Scan /proc for processes on this TTY.
    let mut candidates: Vec<(i32, Vec<String>)> = Vec::new();

    let proc_dir = fs::read_dir("/proc").ok()?;
    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let name_str = name.to_str()?;
        let pid: i32 = name_str.parse().ok()?;

        if let Some(args) = check_pid(pid, tty_nr) {
            candidates.push((pid, args));
        }
    }

    // Sort by PID descending (prefer newest SSH process, matching macOS behavior).
    candidates.sort_by(|a, b| b.0.cmp(&a.0));

    for (_pid, args) in candidates {
        if let Some(session) = parse_ssh_command_line(&args) {
            return Some(session);
        }
    }

    None
}

/// Check if a process is a foreground SSH process on the given TTY.
/// Returns the command-line arguments if it is.
fn check_pid(pid: i32, target_tty_nr: u32) -> Option<Vec<String>> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;

    // /proc/<pid>/stat format:
    //   pid (comm) state ppid pgrp session tty_nr tpgid ...
    // The comm field can contain spaces and parens, so find the last ')'.
    let comm_end = stat.rfind(')')?;
    let fields_str = &stat[comm_end + 2..]; // skip ") "
    let fields: Vec<&str> = fields_str.split_whitespace().collect();

    // fields[0] = state, [1] = ppid, [2] = pgrp, [3] = session, [4] = tty_nr, [5] = tpgid
    if fields.len() < 6 {
        return None;
    }

    let pgrp: i32 = fields[2].parse().ok()?;
    let tty_nr: u32 = fields[4].parse().ok()?;
    let tpgid: i32 = fields[5].parse().ok()?;

    // Must be on the target TTY and in the foreground process group.
    if tty_nr != target_tty_nr || pgrp <= 0 || tpgid <= 0 || pgrp != tpgid {
        return None;
    }

    // Read the command line.
    let cmdline = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    if cmdline.is_empty() {
        return None;
    }

    // cmdline is null-byte separated argv.
    let args: Vec<String> = cmdline
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();

    if args.is_empty() {
        return None;
    }

    // Check that the executable is "ssh" (basename of argv[0]).
    let exe_name = args[0].rsplit('/').next().unwrap_or(&args[0]);
    if exe_name != "ssh" {
        return None;
    }

    Some(args)
}

/// Convert a TTY device path to the kernel tty_nr value.
///
/// For `/dev/pts/N`: tty_nr = (136 + N/256) * 256 + (N % 256)
/// Simplified for pts < 256: tty_nr = 136 * 256 + N = 34816 + N
fn tty_path_to_nr(tty_path: &str) -> Option<u32> {
    // Handle /dev/pts/N
    if let Some(n_str) = tty_path.strip_prefix("/dev/pts/") {
        let n: u32 = n_str.parse().ok()?;
        let major = 136 + n / 256;
        let minor = n % 256;
        return Some(major * 256 + minor);
    }
    // Handle /dev/ttyN (virtual console) — major 4
    if let Some(n_str) = tty_path.strip_prefix("/dev/tty") {
        if let Ok(n) = n_str.parse::<u32>() {
            return Some(4 * 256 + n);
        }
    }
    None
}

// ── SSH command-line parsing ──────────────────────────────────────────

/// SSH flags that take no argument value.
const NO_ARG_FLAGS: &str = "46AaCfGgKkMNnqsTtVvXxYy";

/// SSH flags that consume the next argument as their value.
const VALUE_ARG_FLAGS: &str = "BbcDEeFIiJLlmOopQRSWw";

/// SSH `-o` option keys to filter out (not useful for SCP reuse).
const FILTERED_OPTION_KEYS: &[&str] = &[
    "batchmode",
    "controlmaster",
    "controlpersist",
    "forkafterauthentication",
    "localcommand",
    "permitlocalcommand",
    "remotecommand",
    "requesttty",
    "sendenv",
    "sessiontype",
    "setenv",
    "stdioforward",
];

/// Parse an SSH command-line (argv) into a `DetectedSSHSession`.
fn parse_ssh_command_line(args: &[String]) -> Option<DetectedSSHSession> {
    if args.is_empty() {
        return None;
    }

    let mut index = 0;

    // Skip argv[0] if it's the ssh executable.
    if let Some(exe) = args[0].rsplit('/').next() {
        if exe == "ssh" {
            index = 1;
        }
    }

    let mut destination: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut identity_file: Option<String> = None;
    let mut login_name: Option<String> = None;
    let mut ssh_options: Vec<String> = Vec::new();

    while index < args.len() {
        let arg = &args[index];

        // `--` marks end of options.
        if arg == "--" {
            index += 1;
            if index < args.len() {
                destination = Some(args[index].clone());
            }
            break;
        }

        // Non-flag argument = destination.
        if !arg.starts_with('-') || arg == "-" {
            destination = Some(arg.clone());
            break;
        }

        let chars: Vec<char> = arg.chars().skip(1).collect(); // skip leading '-'

        // Flag with value attached: e.g., `-p22` or `-i/path/to/key`
        if chars.len() > 1 {
            if let Some(&first) = chars.first() {
                if VALUE_ARG_FLAGS.contains(first) {
                    let value: String = chars[1..].iter().collect();
                    consume_value(first, &value, &mut port, &mut identity_file, &mut login_name, &mut ssh_options);
                    index += 1;
                    continue;
                }
            }
        }

        // Flag with value in next argument: e.g., `-p 22`
        if chars.len() == 1 {
            let flag = chars[0];
            if VALUE_ARG_FLAGS.contains(flag) {
                let next = index + 1;
                if next >= args.len() {
                    return None;
                }
                consume_value(flag, &args[next], &mut port, &mut identity_file, &mut login_name, &mut ssh_options);
                index += 2;
                continue;
            }
        }

        // Boolean flags: e.g., `-4`, `-6`, `-A`, `-C`, `-Nv`
        if chars.iter().all(|c| NO_ARG_FLAGS.contains(*c)) {
            // We don't track boolean flags for SCP reuse — just skip them.
            index += 1;
            continue;
        }

        // Unknown flag — bail out.
        return None;
    }

    let dest = destination?;
    let final_dest = resolve_destination(&dest, login_name.as_deref());
    if final_dest.is_empty() {
        return None;
    }

    Some(DetectedSSHSession {
        destination: final_dest,
        port,
        identity_file,
        ssh_options,
    })
}

/// Process a value for a flag character.
fn consume_value(
    flag: char,
    value: &str,
    port: &mut Option<u16>,
    identity_file: &mut Option<String>,
    login_name: &mut Option<String>,
    ssh_options: &mut Vec<String>,
) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    match flag {
        'p' => {
            if let Ok(p) = trimmed.parse::<u16>() {
                *port = Some(p);
            }
        }
        'i' => *identity_file = Some(trimmed.to_string()),
        'l' => *login_name = Some(trimmed.to_string()),
        'o' => consume_ssh_option(trimmed, port, identity_file, login_name, ssh_options),
        _ => {} // Other value flags (D, E, L, R, etc.) — not needed for SCP.
    }
}

/// Process an SSH `-o Key=Value` option.
fn consume_ssh_option(
    option: &str,
    port: &mut Option<u16>,
    identity_file: &mut Option<String>,
    login_name: &mut Option<String>,
    ssh_options: &mut Vec<String>,
) {
    let trimmed = option.trim();
    if trimmed.is_empty() {
        return;
    }

    let key = ssh_option_key(trimmed);
    let value = ssh_option_value(trimmed);

    match key.as_deref() {
        Some("port") => {
            if let Some(v) = value {
                if let Ok(p) = v.parse::<u16>() {
                    *port = Some(p);
                }
            }
        }
        Some("identityfile") => {
            if let Some(v) = value {
                *identity_file = Some(v.to_string());
            }
        }
        Some("user") => {
            if let Some(v) = value {
                *login_name = Some(v.to_string());
            }
        }
        Some(k) if FILTERED_OPTION_KEYS.contains(&k) => {
            // Skip filtered options.
        }
        _ => {
            ssh_options.push(trimmed.to_string());
        }
    }
}

/// Extract the key from an SSH option string (lowercased).
fn ssh_option_key(option: &str) -> Option<String> {
    option
        .split(|c: char| c == '=' || c.is_whitespace())
        .next()
        .map(|k| k.to_lowercase())
}

/// Extract the value from an SSH option string.
fn ssh_option_value(option: &str) -> Option<String> {
    if let Some(eq_pos) = option.find('=') {
        let v = option[eq_pos + 1..].trim();
        if v.is_empty() { None } else { Some(v.to_string()) }
    } else {
        let parts: Vec<&str> = option.splitn(2, char::is_whitespace).collect();
        if parts.len() == 2 {
            let v = parts[1].trim();
            if v.is_empty() { None } else { Some(v.to_string()) }
        } else {
            None
        }
    }
}

/// Prepend login name to destination if not already present.
fn resolve_destination(destination: &str, login_name: Option<&str>) -> String {
    let trimmed = destination.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some(login) = login_name {
        let login = login.trim();
        if !login.is_empty() && !trimmed.contains('@') {
            return format!("{login}@{trimmed}");
        }
    }
    trimmed.to_string()
}
