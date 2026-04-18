use clap::Subcommand;
use std::path::Path;

use crate::socket::send_command;

#[derive(Subcommand)]
pub enum SplitCmd {
    /// Split the focused pane horizontally (side by side)
    SplitRight,
    /// Split the focused pane vertically (top and bottom)
    SplitDown,
}

impl SplitCmd {
    pub fn run(&self, socket: &Path) -> Result<String, String> {
        match self {
            Self::SplitRight => send_command(socket, "split_right"),
            Self::SplitDown => send_command(socket, "split_down"),
        }
    }
}
