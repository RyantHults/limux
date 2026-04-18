use clap::Subcommand;
use std::path::Path;

use crate::socket::send_command;

#[derive(Subcommand)]
pub enum RemoteCmd {
    /// Connect to a remote host via SSH
    RemoteConnect {
        /// SSH destination (user@host or host)
        destination: String,
        /// SSH port
        #[arg(long)]
        port: Option<u16>,
        /// Path to SSH identity file
        #[arg(long)]
        identity: Option<String>,
        /// SSH option (e.g., "ProxyCommand=...")
        #[arg(long = "ssh-option")]
        ssh_option: Vec<String>,
        /// Command to run on the remote host
        #[arg(long)]
        command: Option<String>,
    },
    /// Disconnect a remote workspace
    RemoteDisconnect {
        /// Workspace ID (defaults to focused workspace)
        #[arg(long)]
        workspace_id: Option<u32>,
        /// Also clear the remote configuration
        #[arg(long)]
        clear: bool,
    },
    /// Reconnect a remote workspace
    RemoteReconnect {
        /// Workspace ID (defaults to focused workspace)
        #[arg(long)]
        workspace_id: Option<u32>,
    },
    /// Show remote connection status
    RemoteStatus {
        /// Workspace ID (defaults to focused workspace)
        #[arg(long)]
        workspace_id: Option<u32>,
    },
}

impl RemoteCmd {
    pub fn run(&self, socket: &Path) -> Result<String, String> {
        match self {
            Self::RemoteConnect {
                destination,
                port,
                identity,
                ssh_option,
                command,
            } => {
                let mut cmd = format!("remote_connect {}", destination);
                if let Some(p) = port {
                    cmd.push_str(&format!(" --port={}", p));
                }
                if let Some(id) = identity {
                    cmd.push_str(&format!(" --identity={}", id));
                }
                for opt in ssh_option {
                    cmd.push_str(&format!(" --ssh-option={}", opt));
                }
                if let Some(c) = command {
                    cmd.push_str(&format!(" --command={}", c));
                }
                send_command(socket, &cmd)
            }
            Self::RemoteDisconnect { workspace_id, clear } => {
                let mut cmd = "remote_disconnect".to_string();
                if let Some(id) = workspace_id {
                    cmd.push_str(&format!(" {}", id));
                }
                if *clear {
                    cmd.push_str(" --clear");
                }
                send_command(socket, &cmd)
            }
            Self::RemoteReconnect { workspace_id } => {
                let mut cmd = "remote_reconnect".to_string();
                if let Some(id) = workspace_id {
                    cmd.push_str(&format!(" {}", id));
                }
                send_command(socket, &cmd)
            }
            Self::RemoteStatus { workspace_id } => {
                let mut cmd = "remote_status".to_string();
                if let Some(id) = workspace_id {
                    cmd.push_str(&format!(" {}", id));
                }
                send_command(socket, &cmd)
            }
        }
    }
}
