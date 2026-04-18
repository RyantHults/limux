use clap::Subcommand;
use std::path::Path;

use crate::socket::send_command;

#[derive(Subcommand)]
pub enum WorkspaceCmd {
    /// Create a new workspace
    NewWorkspace,
    /// Get the number of workspaces
    WorkspaceCount,
    /// List all workspaces
    ListWorkspaces,
    /// Set a workspace's accent color
    WorkspaceSetColor {
        /// Workspace ID
        id: u32,
        /// Color name (red, orange, yellow, green, blue, purple, pink, gray, none)
        color: String,
    },
    /// Toggle a workspace's pinned state
    WorkspacePin {
        /// Workspace ID
        id: u32,
    },
    /// Select (switch to) a workspace by ID
    SelectWorkspace {
        /// Workspace ID
        id: u32,
    },
    /// Close a workspace by ID (closes current if omitted)
    CloseWorkspace {
        /// Workspace ID (optional, defaults to current)
        id: Option<u32>,
    },
    /// Show the current workspace's ID and title
    CurrentWorkspace,
    /// List pane IDs in a workspace
    ListPanes {
        /// Workspace ID (optional, defaults to current)
        id: Option<u32>,
    },
    /// Focus a specific pane by ID
    FocusPane {
        /// Pane ID
        id: u32,
    },
    /// Rename a workspace
    RenameWorkspace {
        /// Workspace ID
        id: u32,
        /// New name
        name: String,
    },
    /// Toggle the sidebar
    ToggleSidebar,
}

impl WorkspaceCmd {
    pub fn run(&self, socket: &Path) -> Result<String, String> {
        match self {
            Self::NewWorkspace => send_command(socket, "new_workspace"),
            Self::WorkspaceCount => send_command(socket, "workspace_count"),
            Self::ListWorkspaces => send_command(socket, "list_workspaces"),
            Self::WorkspaceSetColor { id, color } => {
                send_command(socket, &format!("workspace_set_color {id} {color}"))
            }
            Self::WorkspacePin { id } => {
                send_command(socket, &format!("workspace_pin {id}"))
            }
            Self::SelectWorkspace { id } => {
                send_command(socket, &format!("select_workspace {id}"))
            }
            Self::CloseWorkspace { id } => {
                match id {
                    Some(id) => send_command(socket, &format!("close_workspace {id}")),
                    None => send_command(socket, "close_workspace"),
                }
            }
            Self::CurrentWorkspace => send_command(socket, "current_workspace"),
            Self::ListPanes { id } => {
                match id {
                    Some(id) => send_command(socket, &format!("list_panes {id}")),
                    None => send_command(socket, "list_panes"),
                }
            }
            Self::FocusPane { id } => {
                send_command(socket, &format!("focus_pane {id}"))
            }
            Self::RenameWorkspace { id, name } => {
                send_command(socket, &format!("rename_workspace {id} {name}"))
            }
            Self::ToggleSidebar => send_command(socket, "toggle_sidebar"),
        }
    }
}
