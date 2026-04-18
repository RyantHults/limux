use clap::Subcommand;
use std::path::Path;

use crate::socket::send_command;

#[derive(Subcommand)]
pub enum TerminalCmd {
    /// Send text to a terminal surface
    Send {
        /// Surface ID
        surface_id: u32,
        /// Text to send
        text: String,
    },
    /// Read the visible screen content from a terminal surface
    ReadScreen {
        /// Surface ID
        surface_id: u32,
    },
}

impl TerminalCmd {
    pub fn run(&self, socket: &Path) -> Result<String, String> {
        match self {
            Self::Send { surface_id, text } => {
                send_command(socket, &format!("send {surface_id} {text}"))
            }
            Self::ReadScreen { surface_id } => {
                send_command(socket, &format!("read_screen {surface_id}"))
            }
        }
    }
}
