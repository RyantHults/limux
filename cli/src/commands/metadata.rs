use clap::Subcommand;
use std::path::Path;

use crate::socket::send_command;

#[derive(Subcommand)]
pub enum MetadataCmd {
    /// Set a status entry on the focused workspace
    SetStatus {
        /// Status key
        key: String,
        /// Status value
        value: String,
        /// Icon name
        #[arg(long)]
        icon: Option<String>,
        /// Color name
        #[arg(long)]
        color: Option<String>,
        /// Sort priority
        #[arg(long)]
        priority: Option<i32>,
    },
    /// Clear a status entry from the focused workspace
    ClearStatus {
        /// Status key to clear
        key: String,
    },
    /// Set a progress bar on the focused workspace
    SetProgress {
        /// Progress value (0.0 to 1.0)
        value: f64,
        /// Progress label
        #[arg(long)]
        label: Option<String>,
    },
    /// Clear the progress bar from the focused workspace
    ClearProgress,
    /// Add a log entry to the focused workspace
    Log {
        /// Log message
        message: String,
        /// Log level (info, warning, error, success)
        #[arg(long, default_value = "info")]
        level: String,
    },
    /// Clear all log entries from the focused workspace
    ClearLog,
    /// Enable desktop notifications
    NotifyEnable,
    /// Disable desktop notifications
    NotifyDisable,
    /// Check notification status
    NotifyStatus,
}

impl MetadataCmd {
    pub fn run(&self, socket: &Path) -> Result<String, String> {
        match self {
            Self::SetStatus { key, value, icon, color, priority } => {
                let mut cmd = format!("set_status {key} {value}");
                if let Some(icon) = icon { cmd.push_str(&format!(" --icon={icon}")); }
                if let Some(color) = color { cmd.push_str(&format!(" --color={color}")); }
                if let Some(priority) = priority { cmd.push_str(&format!(" --priority={priority}")); }
                send_command(socket, &cmd)
            }
            Self::ClearStatus { key } => {
                send_command(socket, &format!("clear_status {key}"))
            }
            Self::SetProgress { value, label } => {
                let mut cmd = format!("set_progress {value}");
                if let Some(label) = label { cmd.push_str(&format!(" --label={label}")); }
                send_command(socket, &cmd)
            }
            Self::ClearProgress => send_command(socket, "clear_progress"),
            Self::Log { message, level } => {
                send_command(socket, &format!("log {message} --level={level}"))
            }
            Self::ClearLog => send_command(socket, "clear_log"),
            Self::NotifyEnable => send_command(socket, "notify_enable"),
            Self::NotifyDisable => send_command(socket, "notify_disable"),
            Self::NotifyStatus => send_command(socket, "notify_status"),
        }
    }
}
