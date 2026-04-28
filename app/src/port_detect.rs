use std::collections::{BTreeSet, HashMap};

use crate::workspace::WorkspaceId;

/// Scan /proc for listening TCP ports owned by descendant processes of this
/// process, and attribute them to workspaces.
///
/// Workspaces with a known `working_directory` use CWD-prefix matching
/// (longest-prefix-wins). Workspaces with no known CWD receive all ports
/// that weren't claimed by a CWD-based workspace.
///
/// Returns one entry per workspace that has at least one listening port.
/// Ports are sorted ascending and deduplicated. All I/O errors are silently
/// ignored — a missing /proc entry just means that process is skipped.
pub fn scan(workspaces: &[(WorkspaceId, Option<String>)]) -> Vec<(WorkspaceId, Vec<u16>)> {
    let inode_map = read_listening_inodes();
    if inode_map.is_empty() {
        return Vec::new();
    }

    let own_pid = std::process::id();
    let all_pids = collect_descendant_pids(own_pid);

    // Collect (cwd, ports) for every descendant that has listening sockets.
    let mut pid_ports: Vec<(String, Vec<u16>)> = Vec::new();
    for pid in all_pids {
        let ports = pid_listening_ports(pid, &inode_map);
        if ports.is_empty() {
            continue;
        }
        if let Some(cwd) = pid_cwd(pid) {
            pid_ports.push((cwd, ports));
        }
    }

    if pid_ports.is_empty() {
        return Vec::new();
    }

    // Separate workspaces that have a known CWD from those that don't.
    let cwd_workspaces: Vec<(WorkspaceId, &str)> = workspaces
        .iter()
        .filter_map(|(id, dir)| dir.as_deref().map(|d| (*id, d)))
        .collect();

    // For each process, find the best (longest-prefix) matching CWD workspace.
    // Track which ports were claimed by any CWD workspace.
    let mut cwd_results: HashMap<WorkspaceId, BTreeSet<u16>> = HashMap::new();
    let mut claimed_ports: BTreeSet<u16> = BTreeSet::new();

    for (cwd, ports) in &pid_ports {
        // Find the longest-matching workspace directory.
        let best = cwd_workspaces
            .iter()
            .filter(|(_, dir)| is_under(cwd, dir))
            .max_by_key(|(_, dir)| dir.len());

        if let Some((ws_id, _)) = best {
            let entry = cwd_results.entry(*ws_id).or_default();
            for &port in ports {
                entry.insert(port);
                claimed_ports.insert(port);
            }
        }
    }

    // Workspaces with no known CWD receive all ports not claimed above.
    let fallback_ids: Vec<WorkspaceId> = workspaces
        .iter()
        .filter(|(_, dir)| dir.is_none())
        .map(|(id, _)| *id)
        .collect();

    let mut results: Vec<(WorkspaceId, Vec<u16>)> = Vec::new();

    for (ws_id, ports_set) in cwd_results {
        if !ports_set.is_empty() {
            results.push((ws_id, ports_set.into_iter().collect()));
        }
    }

    if !fallback_ids.is_empty() && !claimed_ports.is_empty() {
        // Collect all detected ports not already shown on a CWD workspace.
        // Also include any ports from processes whose CWD didn't match anything.
        let all_ports: BTreeSet<u16> = pid_ports
            .iter()
            .flat_map(|(_, ports)| ports.iter().copied())
            .collect();
        let unclaimed: Vec<u16> = all_ports
            .difference(&claimed_ports)
            .copied()
            .collect();
        // If there are no CWD workspaces at all, show everything.
        let fallback_ports: Vec<u16> = if cwd_workspaces.is_empty() {
            all_ports.into_iter().collect()
        } else {
            unclaimed
        };
        if !fallback_ports.is_empty() {
            for ws_id in fallback_ids {
                results.push((ws_id, fallback_ports.clone()));
            }
        }
    } else if !fallback_ids.is_empty() && claimed_ports.is_empty() {
        // No CWD workspaces matched anything — show all ports on fallback workspaces.
        let all_ports: Vec<u16> = pid_ports
            .iter()
            .flat_map(|(_, ports)| ports.iter().copied())
            .collect::<BTreeSet<u16>>()
            .into_iter()
            .collect();
        if !all_ports.is_empty() {
            for ws_id in fallback_ids {
                results.push((ws_id, all_ports.clone()));
            }
        }
    }

    results
}

/// Returns true if `cwd` is exactly `dir` or is a subdirectory of it.
fn is_under(cwd: &str, dir: &str) -> bool {
    cwd == dir || cwd.starts_with(&format!("{dir}/"))
}

/// Parse /proc/net/tcp and /proc/net/tcp6 for LISTEN-state sockets.
/// Returns a map of socket inode → local port.
fn read_listening_inodes() -> HashMap<u64, u16> {
    let mut map = HashMap::new();
    parse_proc_net_tcp("/proc/net/tcp", &mut map);
    parse_proc_net_tcp("/proc/net/tcp6", &mut map);
    map
}

fn parse_proc_net_tcp(path: &str, map: &mut HashMap<u64, u16>) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    for line in content.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // local_address=fields[1], st=fields[3], inode=fields[9]
        if fields.len() < 10 {
            continue;
        }
        if fields[3] != "0A" {
            continue; // not LISTEN
        }
        // local_address is "XXXXXXXX:PPPP" (IPv4) or "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX:PPPP" (IPv6)
        let addr = fields[1];
        let Some(colon) = addr.rfind(':') else { continue };
        let Ok(port) = u16::from_str_radix(&addr[colon + 1..], 16) else {
            continue;
        };
        if port == 0 {
            continue;
        }
        let Ok(inode) = fields[9].parse::<u64>() else {
            continue;
        };
        map.insert(inode, port);
    }
}

/// Walk /proc to build a parent→children map, then BFS from root_pid.
fn collect_descendant_pids(root: u32) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };

    let mut parent_of: HashMap<u32, u32> = HashMap::new(); // pid → ppid

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<u32>() else {
            continue;
        };
        if let Some(ppid) = read_ppid(pid) {
            parent_of.insert(pid, ppid);
        }
    }

    // BFS: collect all descendants of root.
    let mut result = vec![root];
    let mut queue = vec![root];
    while let Some(parent) = queue.pop() {
        for (&pid, &ppid) in &parent_of {
            if ppid == parent {
                result.push(pid);
                queue.push(pid);
            }
        }
    }
    result
}

/// Read the PPID of a process from /proc/<pid>/stat.
/// Handles process names containing spaces by anchoring on the last ')'.
fn read_ppid(pid: u32) -> Option<u32> {
    let content = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Format: "PID (comm) state PPID ..."
    // comm can contain spaces and parens; find the last ')' to skip it.
    let after_comm = content.rfind(')')?;
    let rest = content[after_comm + 1..].trim_start();
    // rest: "state PPID ..."
    let mut parts = rest.split_whitespace();
    parts.next()?; // state
    parts.next()?.parse::<u32>().ok()
}

/// Return the listening ports owned by a process via its open fd symlinks.
fn pid_listening_ports(pid: u32, inodes: &HashMap<u64, u16>) -> Vec<u16> {
    let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return Vec::new();
    };
    let mut ports = Vec::new();
    for entry in entries.flatten() {
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let s = target.to_string_lossy();
        // socket:[inode]
        if let Some(inner) = s.strip_prefix("socket:[").and_then(|s| s.strip_suffix(']')) {
            if let Ok(inode) = inner.parse::<u64>() {
                if let Some(&port) = inodes.get(&inode) {
                    ports.push(port);
                }
            }
        }
    }
    ports
}

/// Resolve /proc/<pid>/cwd to a path string.
fn pid_cwd(pid: u32) -> Option<String> {
    let path = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    Some(path.to_string_lossy().into_owned())
}
