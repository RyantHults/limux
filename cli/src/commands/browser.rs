use clap::Subcommand;
use std::path::Path;

use crate::socket::send_command;

#[derive(Subcommand)]
pub enum BrowserCmd {
    /// Open a browser panel beside the focused pane
    OpenBrowser {
        /// URL to open (optional)
        #[arg(default_value = "")]
        url: String,
    },
    /// Navigate a browser to a URL
    Navigate {
        /// Browser panel ID
        browser_id: u32,
        /// URL to navigate to
        url: String,
    },
    /// Go back in browser history
    BrowserBack {
        /// Browser panel ID
        browser_id: u32,
    },
    /// Go forward in browser history
    BrowserForward {
        /// Browser panel ID
        browser_id: u32,
    },
    /// Reload the browser page
    BrowserReload {
        /// Browser panel ID
        browser_id: u32,
    },
    /// Get the current URL of a browser panel
    GetUrl {
        /// Browser panel ID
        browser_id: u32,
    },
    /// Evaluate JavaScript in a browser panel
    JsEval {
        /// Browser panel ID
        browser_id: u32,
        /// JavaScript code to evaluate
        script: String,
    },
}

impl BrowserCmd {
    pub fn run(&self, socket: &Path) -> Result<String, String> {
        match self {
            Self::OpenBrowser { url } => {
                send_command(socket, &format!("open_browser {url}"))
            }
            Self::Navigate { browser_id, url } => {
                send_command(socket, &format!("navigate {browser_id} {url}"))
            }
            Self::BrowserBack { browser_id } => {
                send_command(socket, &format!("browser_back {browser_id}"))
            }
            Self::BrowserForward { browser_id } => {
                send_command(socket, &format!("browser_forward {browser_id}"))
            }
            Self::BrowserReload { browser_id } => {
                send_command(socket, &format!("browser_reload {browser_id}"))
            }
            Self::GetUrl { browser_id } => {
                send_command(socket, &format!("get_url {browser_id}"))
            }
            Self::JsEval { browser_id, script } => {
                send_command(socket, &format!("js_eval {browser_id} {script}"))
            }
        }
    }
}
