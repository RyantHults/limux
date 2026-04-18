use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Mutex;

use gtk4::glib;

static SOCKET_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Get the path to the running Unix socket, if available.
pub fn socket_path() -> Option<PathBuf> {
    SOCKET_PATH.lock().ok().and_then(|p| p.clone())
}

/// Start the Unix socket server, integrated with the GLib main loop.
pub fn start(path: Option<&str>) {
    let socket_path = PathBuf::from(
        path.unwrap_or_else(|| {
            // Default: /tmp/limux-<pid>.sock
            let default = format!("/tmp/limux-{}.sock", std::process::id());
            // Leak the string so we get a &'static str
            Box::leak(default.into_boxed_str())
        }),
    );

    // Remove stale socket
    let _ = std::fs::remove_file(&socket_path);

    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind socket at {}: {e}", socket_path.display());
            return;
        }
    };

    // Set permissions to owner-only
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600));
    }

    // Set non-blocking so we can poll from GLib
    listener.set_nonblocking(true).ok();

    // Store path for cleanup
    *SOCKET_PATH.lock().unwrap() = Some(socket_path.clone());

    // Export as env var
    // SAFETY: we're single-threaded at this point during app startup
    unsafe { std::env::set_var("LIMUX_SOCKET", &socket_path) };
    println!("Socket listening on: {}", socket_path.display());

    // Integrate with GLib main loop via Unix FD source
    use std::os::unix::io::AsRawFd;
    let fd = listener.as_raw_fd();

    // Keep listener alive by moving it into the closure
    let listener = std::sync::Arc::new(listener);
    let listener_clone = listener.clone();

    glib::unix_fd_add_local(fd, glib::IOCondition::IN, move |_fd, _cond| {
        match listener_clone.accept() {
            Ok((stream, _addr)) => {
                handle_client(stream);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => {
                eprintln!("Socket accept error: {e}");
            }
        }
        glib::ControlFlow::Continue
    });

    // Prevent the Arc from dropping the listener
    std::mem::forget(listener);
}

fn handle_client(mut stream: std::os::unix::net::UnixStream) {
    let Ok(clone) = stream.try_clone() else { return };
    let reader = BufReader::new(clone);

    for line in reader.lines() {
        let Ok(line) = line else { break };
        let response = handle_command(line.trim());
        // Multi-line responses use length-prefix: "OK+<len>\n<data>"
        if response.starts_with("OK+") {
            let _ = write!(stream, "{response}");
        } else {
            let _ = writeln!(stream, "{response}");
        }
    }
}

/// Parse a string into a typed value, returning early with an error string on failure.
macro_rules! parse_arg {
    ($s:expr, $t:ty, $name:expr) => {
        match $s.trim().parse::<$t>() {
            Ok(v) => v,
            Err(_) => return format!("ERROR: invalid {}", $name),
        }
    };
}

fn handle_command(cmd: &str) -> String {
    let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
    let command = parts[0];
    let _args = parts.get(1).unwrap_or(&"");

    match command {
        "ping" => "OK pong".to_string(),
        "version" => "OK limux 0.1.0".to_string(),
        "list_surfaces" => {
            let lines = crate::window::list_surfaces_detailed();
            if lines.is_empty() {
                "OK".to_string()
            } else {
                let text = lines.join("\n");
                format!("OK+{}\n{text}", text.len())
            }
        }
        // Workspace commands
        "new_workspace" | "new_tab" => {
            crate::window::new_workspace();
            "OK".to_string()
        }
        "workspace_count" | "tab_count" => {
            format!("OK {}", crate::window::workspace_count())
        }
        "list_workspaces" => {
            let entries = crate::window::list_workspaces_detailed();
            if entries.is_empty() {
                "OK".to_string()
            } else {
                let lines: Vec<String> = entries.iter().map(|(id, title, panes, pinned, color)| {
                    let mut line = format!("id:{id} title:{title:?} panes:{panes}");
                    if *pinned { line.push_str(" pinned"); }
                    if let Some(c) = color { line.push_str(&format!(" color={c}")); }
                    line
                }).collect();
                let text = lines.join("\n");
                format!("OK+{}\n{text}", text.len())
            }
        }
        "workspace_set_color" => {
            let parts: Vec<&str> = _args.splitn(2, ' ').collect();
            if parts.len() < 2 {
                return "ERROR: usage: workspace_set_color <id> <color|none>".to_string();
            }
            let ws_id = parse_arg!(parts[0], u32, "workspace id");
            let color = if parts[1] == "none" {
                None
            } else {
                match crate::workspace::WorkspaceColor::from_name(parts[1]) {
                    Some(c) => Some(c),
                    None => return "ERROR: unknown color".to_string(),
                }
            };
            crate::window::set_workspace_color(ws_id, color);
            "OK".to_string()
        }
        "workspace_pin" => {
            let ws_id = parse_arg!(_args, u32, "workspace id");
            crate::window::toggle_workspace_pinned(ws_id);
            "OK".to_string()
        }
        "toggle_sidebar" => {
            crate::window::toggle_sidebar();
            "OK".to_string()
        }
        // Split commands
        "split_right" => {
            crate::window::split_focused(crate::split::Orientation::Horizontal);
            "OK".to_string()
        }
        "split_down" => {
            crate::window::split_focused(crate::split::Orientation::Vertical);
            "OK".to_string()
        }
        // Metadata commands (target focused workspace)
        "set_status" => {
            let Some(ws_id) = crate::window::focused_workspace_id() else {
                return "ERROR: no focused workspace".to_string();
            };
            let parsed = parse_opts(_args);
            if parsed.positional.len() < 2 {
                return "ERROR: usage: set_status <key> <value> [--icon=X] [--color=X] [--priority=N]".to_string();
            }
            let key = parsed.positional[0].to_string();
            let value = parsed.positional[1..].join(" ");
            let icon = parsed.opts.get("icon").map(|s| s.to_string());
            let color = parsed.opts.get("color").map(|s| s.to_string());
            let priority = parsed.opts.get("priority")
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(0);
            crate::window::set_workspace_status(ws_id, key, value, icon, color, priority);
            "OK".to_string()
        }
        "clear_status" => {
            let Some(ws_id) = crate::window::focused_workspace_id() else {
                return "ERROR: no focused workspace".to_string();
            };
            let key = _args.trim();
            if key.is_empty() {
                return "ERROR: usage: clear_status <key>".to_string();
            }
            crate::window::clear_workspace_status(ws_id, key);
            "OK".to_string()
        }
        "set_progress" => {
            let Some(ws_id) = crate::window::focused_workspace_id() else {
                return "ERROR: no focused workspace".to_string();
            };
            let parsed = parse_opts(_args);
            if parsed.positional.is_empty() {
                return "ERROR: usage: set_progress <0.0-1.0> [--label=X]".to_string();
            }
            let value = parse_arg!(parsed.positional[0], f64, "progress value");
            let label = parsed.opts.get("label").map(|s| s.to_string());
            crate::window::set_workspace_progress(ws_id, value, label);
            "OK".to_string()
        }
        "clear_progress" => {
            let Some(ws_id) = crate::window::focused_workspace_id() else {
                return "ERROR: no focused workspace".to_string();
            };
            crate::window::clear_workspace_progress(ws_id);
            "OK".to_string()
        }
        "log" => {
            let Some(ws_id) = crate::window::focused_workspace_id() else {
                return "ERROR: no focused workspace".to_string();
            };
            let parsed = parse_opts(_args);
            if parsed.positional.is_empty() {
                return "ERROR: usage: log <message> [--level=info|warning|error|success]".to_string();
            }
            let message = parsed.positional.join(" ");
            let level = crate::workspace::LogLevel::from_str(
                parsed.opts.get("level").copied().unwrap_or("info"),
            );
            crate::window::add_workspace_log(ws_id, message, level, None);
            "OK".to_string()
        }
        "clear_log" => {
            let Some(ws_id) = crate::window::focused_workspace_id() else {
                return "ERROR: no focused workspace".to_string();
            };
            crate::window::clear_workspace_log(ws_id);
            "OK".to_string()
        }
        // Workspace management
        "select_workspace" => {
            let ws_id = parse_arg!(_args, u32, "workspace id");
            if crate::window::select_workspace_by_id(ws_id) {
                "OK".to_string()
            } else {
                "ERROR: workspace not found".to_string()
            }
        }
        "close_workspace" => {
            let id_str = _args.trim();
            let ws_id = if id_str.is_empty() {
                // Close current workspace
                match crate::window::focused_workspace_id() {
                    Some(id) => id,
                    None => return "ERROR: no focused workspace".to_string(),
                }
            } else {
                match id_str.parse::<u32>() {
                    Ok(id) => id,
                    Err(_) => return "ERROR: invalid workspace id".to_string(),
                }
            };
            if crate::window::close_workspace_by_id(ws_id) {
                "OK".to_string()
            } else {
                "ERROR: workspace not found or pinned".to_string()
            }
        }
        "current_workspace" => {
            match crate::window::current_workspace_info() {
                Some((id, title)) => format!("OK {id} {title}"),
                None => "ERROR: no focused workspace".to_string(),
            }
        }
        "list_panes" => {
            let ws_id = if _args.trim().is_empty() {
                crate::window::focused_workspace_id()
            } else {
                _args.trim().parse::<u32>().ok()
            };
            let Some(ws_id) = ws_id else {
                return "ERROR: no workspace".to_string();
            };
            match crate::window::list_panes(ws_id) {
                Some(ids) => {
                    let strs: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
                    format!("OK {}", strs.join(","))
                }
                None => "ERROR: workspace not found".to_string(),
            }
        }
        "focus_pane" => {
            let pane_id = parse_arg!(_args, u32, "pane id");
            if crate::window::focus_pane_by_id(pane_id) {
                "OK".to_string()
            } else {
                "ERROR: pane not found".to_string()
            }
        }
        "rename_workspace" => {
            let parts: Vec<&str> = _args.splitn(2, ' ').collect();
            if parts.len() < 2 {
                return "ERROR: usage: rename_workspace <id> <name>".to_string();
            }
            let ws_id = parse_arg!(parts[0], u32, "workspace id");
            crate::window::rename_workspace(ws_id, parts[1].trim());
            "OK".to_string()
        }
        // Terminal I/O
        "send" => {
            let parts: Vec<&str> = _args.splitn(2, ' ').collect();
            if parts.len() < 2 {
                return "ERROR: usage: send <surface_id> <text>".to_string();
            }
            let sid = parse_arg!(parts[0], u32, "surface id");
            if crate::window::send_text(sid, parts[1]) {
                "OK".to_string()
            } else {
                "ERROR: surface not found".to_string()
            }
        }
        "read_screen" => {
            let sid = parse_arg!(_args, u32, "surface id");
            match crate::window::read_screen(sid) {
                Some(text) => format!("OK+{}\n{text}", text.len()),
                None => "ERROR: surface not found".to_string(),
            }
        }
        // Browser commands
        "open_browser" => {
            let url = _args.trim();
            crate::window::split_focused_browser(
                crate::split::Orientation::Horizontal,
                url,
            );
            "OK".to_string()
        }
        "navigate" => {
            let parts: Vec<&str> = _args.splitn(2, ' ').collect();
            if parts.len() < 2 {
                return "ERROR: usage: navigate <browser_id> <url>".to_string();
            }
            let id = parse_arg!(parts[0], u32, "browser_id");
            crate::browser::navigate(id, parts[1].trim());
            "OK".to_string()
        }
        "browser_back" => {
            let id = parse_arg!(_args, u32, "browser_id");
            crate::browser::go_back(id);
            "OK".to_string()
        }
        "browser_forward" => {
            let id = parse_arg!(_args, u32, "browser_id");
            crate::browser::go_forward(id);
            "OK".to_string()
        }
        "browser_reload" => {
            let id = parse_arg!(_args, u32, "browser_id");
            crate::browser::reload(id);
            "OK".to_string()
        }
        "get_url" => {
            let id = parse_arg!(_args, u32, "browser_id");
            match crate::browser::get_url(id) {
                Some(url) => format!("OK {url}"),
                None => "ERROR: browser not found".to_string(),
            }
        }
        "js_eval" => {
            let parts: Vec<&str> = _args.splitn(2, ' ').collect();
            if parts.len() < 2 {
                return "ERROR: usage: js_eval <browser_id> <script>".to_string();
            }
            let id = parse_arg!(parts[0], u32, "browser_id");
            crate::browser::evaluate_js(id, parts[1]);
            "OK".to_string()
        }
        "notify_enable" => {
            crate::notify::enable();
            crate::tray::update_notifications_enabled(true);
            "OK".to_string()
        }
        "notify_disable" => {
            crate::notify::disable();
            crate::tray::update_notifications_enabled(false);
            "OK".to_string()
        }
        "notify_status" => {
            let status = if crate::notify::is_enabled() { "enabled" } else { "disabled" };
            format!("OK {status}")
        }

        // ── Remote SSH ─────────────────────────────────────────────
        "remote_connect" => {
            let parsed = parse_opts(_args);
            let Some(destination) = parsed.positional.first().copied() else {
                return "ERROR: remote_connect requires a destination".to_string();
            };
            let port = parsed.opts.get("port").and_then(|v| v.parse::<u16>().ok());
            let identity = parsed.opts.get("identity").map(|v| v.to_string());
            let command = parsed.opts.get("command").map(|v| v.to_string());
            let ssh_options: Vec<String> = parsed.opts.get("ssh-option")
                .map(|v| vec![v.to_string()])
                .unwrap_or_default();
            let config = crate::remote::RemoteConfiguration {
                destination: destination.to_string(),
                port,
                identity_file: identity,
                ssh_options,
                terminal_startup_command: command,
                relay_port: None,
                relay_id: None,
                relay_token: None,
                local_socket_path: None,
            };
            match crate::window::new_remote_workspace(config) {
                Some(id) => format!("OK {id}"),
                None => "ERROR: failed to create remote workspace".to_string(),
            }
        }
        "remote_disconnect" => {
            let parsed = parse_opts(_args);
            let ws_id = parsed.positional.first().and_then(|v| v.parse::<u32>().ok());
            let clear = parsed.opts.contains_key("clear");
            crate::window::disconnect_remote(ws_id, clear);
            "OK".to_string()
        }
        "remote_reconnect" => {
            let ws_id = _args.split_whitespace().next().and_then(|v| v.parse::<u32>().ok());
            crate::window::reconnect_remote(ws_id);
            "OK".to_string()
        }
        "remote_status" => {
            let ws_id = _args.split_whitespace().next().and_then(|v| v.parse::<u32>().ok());
            match crate::window::remote_status_info(ws_id) {
                Some(json) => {
                    format!("OK+{}\n{}", json.len(), json)
                }
                None => "ERROR: workspace not found or not remote".to_string(),
            }
        }

        _ => format!("ERROR: unknown command: {command}"),
    }
}

/// Parsed command options: positional args + --key=value flags.
struct ParsedOpts<'a> {
    positional: Vec<&'a str>,
    opts: std::collections::HashMap<&'a str, &'a str>,
}

/// Parse "arg1 arg2 --key=value --flag=x" into positional args and options.
fn parse_opts(args: &str) -> ParsedOpts<'_> {
    let mut positional = Vec::new();
    let mut opts = std::collections::HashMap::new();
    for part in args.split_whitespace() {
        if let Some(kv) = part.strip_prefix("--") {
            if let Some((k, v)) = kv.split_once('=') {
                opts.insert(k, v);
            }
        } else {
            positional.push(part);
        }
    }
    ParsedOpts { positional, opts }
}

/// Stop the socket server and remove the socket file.
pub fn stop() {
    if let Some(path) = SOCKET_PATH.lock().unwrap().take() {
        let _ = std::fs::remove_file(path);
    }
}
