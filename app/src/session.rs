//! Session persistence — save/restore workspace layout as JSON.
//!
//! Session file: `$XDG_CONFIG_HOME/limux/session.json`
//! (defaults to `~/.config/limux/session.json`)

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use gtk4::glib;

use crate::remote::RemoteConfiguration;
use crate::split::SurfaceId;
use crate::workspace::{PanelKind, Tab};

/// Top-level session snapshot.
#[derive(Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub version: u32,
    pub workspaces: Vec<WorkspaceSnapshot>,
    pub selected_workspace: Option<usize>,
    pub sidebar_width: Option<i32>,
    #[serde(default)]
    pub sidebar_visible: Option<bool>,
}

/// Snapshot of a single workspace.
#[derive(Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub title: String,
    pub custom_title: Option<String>,
    pub working_directory: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub pinned: Option<bool>,
    pub layout: LayoutSnapshot,
    #[serde(default)]
    pub status_entries: Option<Vec<StatusEntrySnapshot>>,
    #[serde(default)]
    pub log_entries: Option<Vec<LogEntrySnapshot>>,
    #[serde(default)]
    pub progress: Option<ProgressSnapshot>,
    /// Remote SSH configuration (None for local workspaces).
    #[serde(default)]
    pub remote_config: Option<RemoteConfiguration>,
}

#[derive(Serialize, Deserialize)]
pub struct StatusEntrySnapshot {
    pub key: String,
    pub value: String,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub priority: i32,
}

#[derive(Serialize, Deserialize)]
pub struct LogEntrySnapshot {
    pub message: String,
    pub level: String,
    pub source: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ProgressSnapshot {
    pub value: f64,
    pub label: Option<String>,
}

/// Recursive split layout snapshot.
#[derive(Serialize, Deserialize)]
pub enum LayoutSnapshot {
    Single {
        /// Pane tabs: each has an optional working directory.
        tabs: Vec<PaneTabSnapshot>,
    },
    Split {
        orientation: String,
        ratio: f64,
        first: Box<LayoutSnapshot>,
        second: Box<LayoutSnapshot>,
    },
}

/// A single tab within a pane.
#[derive(Serialize, Deserialize)]
pub struct PaneTabSnapshot {
    pub working_directory: Option<String>,
    pub title: Option<String>,
    #[serde(default)]
    pub scrollback: Option<String>,
    /// "terminal" (default) or "browser"
    #[serde(default)]
    pub panel_kind: Option<String>,
    /// URL for browser tabs
    #[serde(default)]
    pub url: Option<String>,
}

impl PaneTabSnapshot {
    /// Whether this snapshot represents a browser tab.
    pub fn is_browser(&self) -> bool {
        self.panel_kind.as_deref() == Some("browser")
    }
}

impl Tab {
    /// Convert a Tab to a PaneTabSnapshot for session persistence.
    ///
    /// `scrollback_reader` is called for terminal tabs to capture scrollback.
    /// `url_reader` is called for browser tabs to get the current URL.
    pub fn to_snapshot(
        &self,
        scrollback_reader: impl Fn(SurfaceId) -> Option<String>,
        url_reader: impl Fn(crate::browser::BrowserPanelId) -> Option<String>,
    ) -> PaneTabSnapshot {
        let (panel_kind_str, url, scrollback) = match &self.panel {
            PanelKind::Terminal { surface_id } => {
                ("terminal".to_string(), None, scrollback_reader(*surface_id))
            }
            PanelKind::Browser { browser_id, url } => {
                let current_url = url_reader(*browser_id).unwrap_or_else(|| url.clone());
                ("browser".to_string(), Some(current_url), None)
            }
        };
        PaneTabSnapshot {
            working_directory: self.working_directory.clone(),
            title: if self.title.is_empty() { None } else { Some(self.title.clone()) },
            scrollback,
            panel_kind: Some(panel_kind_str),
            url,
        }
    }
}

/// Get the session file path.
fn session_file_path() -> PathBuf {
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(home).join(".config")
        });
    config_dir.join("limux").join("session.json")
}

/// Save a session snapshot to disk.
pub fn save(snapshot: &SessionSnapshot) -> Result<(), String> {
    let path = session_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let json = serde_json::to_string_pretty(snapshot).map_err(|e| format!("json: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("write: {e}"))?;
    Ok(())
}

/// Load a session snapshot from disk.
pub fn load() -> Option<SessionSnapshot> {
    let path = session_file_path();
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Start the autosave timer (every 30 seconds).
pub fn start_autosave() {
    glib::timeout_add_local(std::time::Duration::from_secs(30), || {
        save_current_session();
        glib::ControlFlow::Continue
    });
}

/// Save the current window state as a session snapshot.
pub fn save_current_session() {
    if let Some(snapshot) = crate::window::session_snapshot() {
        if let Err(e) = save(&snapshot) {
            eprintln!("Session save failed: {e}");
        }
    }
}
