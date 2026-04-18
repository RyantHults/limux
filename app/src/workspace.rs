//! Workspace data model — the hierarchy is Workspace → Pane → Tab.
//!
//! - A **Workspace** is a named unit shown in the sidebar. It contains a split
//!   tree of panes.
//! - A **Pane** is a leaf in the split tree. It contains one or more tabs.
//! - A **Tab** is a single terminal surface.

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::browser::BrowserPanelId;
use crate::remote::{ProxyEndpoint, RemoteConfiguration, RemoteConnectionState, RemoteDaemonStatus};
use crate::split::{SplitTree, SurfaceId};

/// Preset accent colors for workspaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceColor {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
    Pink,
    Gray,
}

impl WorkspaceColor {
    /// CSS class name for this color (e.g., "ws-color-red").
    pub fn css_class(&self) -> &'static str {
        match self {
            Self::Red => "ws-color-red",
            Self::Orange => "ws-color-orange",
            Self::Yellow => "ws-color-yellow",
            Self::Green => "ws-color-green",
            Self::Blue => "ws-color-blue",
            Self::Purple => "ws-color-purple",
            Self::Pink => "ws-color-pink",
            Self::Gray => "ws-color-gray",
        }
    }

    /// All available colors.
    pub const ALL: &'static [WorkspaceColor] = &[
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Blue,
        Self::Purple,
        Self::Pink,
        Self::Gray,
    ];

    /// Hex color value appropriate for the current color scheme.
    /// Dark palette uses brighter/lighter variants for legibility on dark backgrounds.
    pub fn hex(&self, dark: bool) -> &'static str {
        if dark {
            match self {
                Self::Red    => "#ff6b6b",
                Self::Orange => "#ffa94d",
                Self::Yellow => "#ffd43b",
                Self::Green  => "#69db7c",
                Self::Blue   => "#74c0fc",
                Self::Purple => "#cc8ef5",
                Self::Pink   => "#f783ac",
                Self::Gray   => "#adb5bd",
            }
        } else {
            match self {
                Self::Red    => "#e74c3c",
                Self::Orange => "#e67e22",
                Self::Yellow => "#f1c40f",
                Self::Green  => "#2ecc71",
                Self::Blue   => "#3498db",
                Self::Purple => "#9b59b6",
                Self::Pink   => "#e91e8a",
                Self::Gray   => "#95a5a6",
            }
        }
    }

    /// Hex color value for the bell indicator.
    pub fn bell_hex(dark: bool) -> &'static str {
        if dark { "#ff6b6b" } else { "#e74c3c" }
    }

    /// Parse from a string name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "red" => Some(Self::Red),
            "orange" => Some(Self::Orange),
            "yellow" => Some(Self::Yellow),
            "green" => Some(Self::Green),
            "blue" => Some(Self::Blue),
            "purple" => Some(Self::Purple),
            "pink" => Some(Self::Pink),
            "gray" | "grey" => Some(Self::Gray),
            _ => None,
        }
    }
}

impl fmt::Display for WorkspaceColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Red => "red",
            Self::Orange => "orange",
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Purple => "purple",
            Self::Pink => "pink",
            Self::Gray => "gray",
        };
        write!(f, "{}", name)
    }
}

pub type WorkspaceId = u32;
pub type PaneId = u32;

crate::make_id_allocator!(next_workspace_id, NEXT_WORKSPACE_ID);
crate::make_id_allocator!(next_pane_id, NEXT_PANE_ID);

/// What kind of panel a tab holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelKind {
    Terminal { surface_id: SurfaceId },
    Browser { browser_id: BrowserPanelId, url: String },
}

/// A single tab within a pane — either a terminal surface or a browser panel.
pub struct Tab {
    pub panel: PanelKind,
    pub title: String,
    pub working_directory: Option<String>,
}

impl Tab {
    /// Get the surface ID if this is a terminal tab.
    pub fn surface_id(&self) -> Option<SurfaceId> {
        match &self.panel {
            PanelKind::Terminal { surface_id } => Some(*surface_id),
            PanelKind::Browser { .. } => None,
        }
    }

    /// Get the browser panel ID if this is a browser tab.
    pub fn browser_id(&self) -> Option<BrowserPanelId> {
        match &self.panel {
            PanelKind::Browser { browser_id, .. } => Some(*browser_id),
            PanelKind::Terminal { .. } => None,
        }
    }

    /// Whether this tab is a terminal.
    pub fn is_terminal(&self) -> bool {
        matches!(self.panel, PanelKind::Terminal { .. })
    }

    /// Whether this tab is a browser.
    pub fn is_browser(&self) -> bool {
        matches!(self.panel, PanelKind::Browser { .. })
    }
}

/// A pane is a leaf in the split tree. It holds one or more tabs and tracks
/// which tab is currently visible.
pub struct Pane {
    pub id: PaneId,
    pub tabs: Vec<Tab>,
    pub selected_tab: usize,
    /// Bell fired in this pane while it was not focused.
    pub has_bell: bool,
}

impl Pane {
    pub fn new(surface_id: SurfaceId) -> Self {
        Self {
            id: next_pane_id(),
            tabs: vec![Tab {
                panel: PanelKind::Terminal { surface_id },
                title: String::new(),
                working_directory: None,
            }],
            selected_tab: 0,
            has_bell: false,
        }
    }

    pub fn new_with_id(id: PaneId, surface_id: SurfaceId) -> Self {
        Self {
            id,
            tabs: vec![Tab {
                panel: PanelKind::Terminal { surface_id },
                title: String::new(),
                working_directory: None,
            }],
            selected_tab: 0,
            has_bell: false,
        }
    }

    /// Create a pane with a browser tab.
    pub fn new_browser(browser_id: BrowserPanelId, url: &str) -> Self {
        Self {
            id: next_pane_id(),
            tabs: vec![Tab {
                panel: PanelKind::Browser {
                    browser_id,
                    url: url.to_string(),
                },
                title: String::new(),
                working_directory: None,
            }],
            selected_tab: 0,
            has_bell: false,
        }
    }

    /// Create a pane with a browser tab and a specific pane ID.
    pub fn new_browser_with_id(id: PaneId, browser_id: BrowserPanelId, url: &str) -> Self {
        Self {
            id,
            tabs: vec![Tab {
                panel: PanelKind::Browser {
                    browser_id,
                    url: url.to_string(),
                },
                title: String::new(),
                working_directory: None,
            }],
            selected_tab: 0,
            has_bell: false,
        }
    }

    /// The currently active surface in this pane (terminal tabs only).
    pub fn active_surface(&self) -> Option<SurfaceId> {
        self.tabs.get(self.selected_tab).and_then(|t| t.surface_id())
    }

    /// The currently active browser panel in this pane (browser tabs only).
    pub fn active_browser(&self) -> Option<BrowserPanelId> {
        self.tabs.get(self.selected_tab).and_then(|t| t.browser_id())
    }

    /// The panel kind of the currently active tab.
    pub fn active_panel(&self) -> Option<&PanelKind> {
        self.tabs.get(self.selected_tab).map(|t| &t.panel)
    }

    /// All surface IDs in this pane (terminal tabs only).
    pub fn surface_ids(&self) -> Vec<SurfaceId> {
        self.tabs.iter().filter_map(|t| t.surface_id()).collect()
    }

    /// All browser panel IDs in this pane.
    pub fn browser_ids(&self) -> Vec<BrowserPanelId> {
        self.tabs.iter().filter_map(|t| t.browser_id()).collect()
    }

    /// Add a new terminal tab to this pane, returning its index.
    pub fn add_tab(&mut self, surface_id: SurfaceId) -> usize {
        let idx = self.tabs.len();
        self.tabs.push(Tab {
            panel: PanelKind::Terminal { surface_id },
            title: String::new(),
            working_directory: None,
        });
        self.selected_tab = idx;
        idx
    }

    /// Add a new browser tab to this pane, returning its index.
    pub fn add_browser_tab(&mut self, browser_id: BrowserPanelId, url: &str) -> usize {
        let idx = self.tabs.len();
        self.tabs.push(Tab {
            panel: PanelKind::Browser {
                browser_id,
                url: url.to_string(),
            },
            title: String::new(),
            working_directory: None,
        });
        self.selected_tab = idx;
        idx
    }

    /// Remove a tab by surface ID. Returns true if removed.
    pub fn remove_tab(&mut self, surface_id: SurfaceId) -> bool {
        if let Some(idx) = self.tabs.iter().position(|t| t.surface_id() == Some(surface_id)) {
            self.tabs.remove(idx);
            if self.selected_tab >= self.tabs.len() && !self.tabs.is_empty() {
                self.selected_tab = self.tabs.len() - 1;
            }
            true
        } else {
            false
        }
    }

    /// Remove a tab by browser panel ID. Returns true if removed.
    pub fn remove_browser_tab(&mut self, browser_id: BrowserPanelId) -> bool {
        if let Some(idx) = self.tabs.iter().position(|t| t.browser_id() == Some(browser_id)) {
            self.tabs.remove(idx);
            if self.selected_tab >= self.tabs.len() && !self.tabs.is_empty() {
                self.selected_tab = self.tabs.len() - 1;
            }
            true
        } else {
            false
        }
    }

    /// Select a tab by index.
    pub fn select_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.selected_tab = idx;
        }
    }

    /// Select the next or previous tab. Returns the new active panel kind.
    pub fn cycle_tab(&mut self, forward: bool) -> Option<&PanelKind> {
        if self.tabs.len() <= 1 {
            return self.active_panel();
        }
        if forward {
            self.selected_tab = (self.selected_tab + 1) % self.tabs.len();
        } else {
            self.selected_tab =
                (self.selected_tab + self.tabs.len() - 1) % self.tabs.len();
        }
        self.active_panel()
    }

    /// Update the title of a tab by surface ID.
    pub fn set_tab_title(&mut self, surface_id: SurfaceId, title: &str) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.surface_id() == Some(surface_id)) {
            tab.title = title.to_string();
        }
    }

    /// Update the title of a browser tab by browser ID.
    pub fn set_browser_tab_title(&mut self, browser_id: BrowserPanelId, title: &str) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.browser_id() == Some(browser_id)) {
            tab.title = title.to_string();
        }
    }

    /// Update the working directory of a tab by surface ID.
    pub fn set_tab_directory(&mut self, surface_id: SurfaceId, dir: &str) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.surface_id() == Some(surface_id)) {
            tab.working_directory = Some(dir.to_string());
        }
    }

    /// Find which pane contains a given surface ID.
    pub fn has_surface(&self, surface_id: SurfaceId) -> bool {
        self.tabs.iter().any(|t| t.surface_id() == Some(surface_id))
    }

    /// Find which pane contains a given browser panel ID.
    pub fn has_browser(&self, browser_id: BrowserPanelId) -> bool {
        self.tabs.iter().any(|t| t.browser_id() == Some(browser_id))
    }
}

/// A workspace shown in the sidebar. Contains a split tree of panes.
// ── Sidebar metadata types ──────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Progress,
    Success,
    Warning,
    Error,
}

impl LogLevel {
    pub fn from_str(s: &str) -> Self {
        match s {
            "progress" => Self::Progress,
            "success" => Self::Success,
            "warning" | "warn" => Self::Warning,
            "error" => Self::Error,
            _ => Self::Info,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Progress => "progress",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SidebarStatusEntry {
    pub key: String,
    pub value: String,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub priority: i32,
}

#[derive(Debug, Clone)]
pub struct SidebarLogEntry {
    pub message: String,
    pub level: LogLevel,
    pub source: Option<String>,
    pub timestamp: std::time::Instant,
}

#[derive(Debug, Clone)]
pub struct SidebarProgressState {
    pub value: f64,
    pub label: Option<String>,
}

// ── Workspace ───────────────────────────────────────────────────────

pub struct Workspace {
    pub id: WorkspaceId,
    pub title: String,
    pub custom_title: Option<String>,
    pub working_directory: Option<String>,
    pub git_branch: Option<String>,
    /// Accent color shown as a left-border indicator in the sidebar.
    pub color: Option<WorkspaceColor>,
    /// Pinned workspaces sort to the top of the sidebar and resist accidental close.
    pub pinned: bool,
    /// Bell fired in this workspace while it was not focused.
    pub has_bell: bool,
    /// Panes indexed by PaneId.
    pub panes: HashMap<PaneId, Pane>,
    /// The split layout tree — source of truth for pane arrangement and focus.
    pub split_tree: SplitTree,
    /// Status entries pushed by external tools (key → entry).
    pub status_entries: HashMap<String, SidebarStatusEntry>,
    /// Log entries (most recent last, capped at 50).
    pub log_entries: Vec<SidebarLogEntry>,
    /// Progress bar state.
    pub progress: Option<SidebarProgressState>,
    /// Remote SSH configuration (None for local workspaces).
    pub remote_config: Option<RemoteConfiguration>,
    /// Remote connection state.
    pub remote_state: RemoteConnectionState,
    /// Remote daemon status information.
    pub remote_daemon_status: Option<RemoteDaemonStatus>,
    /// Proxy endpoint for browser tunneling (set when proxy tunnel is ready).
    pub proxy_endpoint: Option<ProxyEndpoint>,
}

impl Workspace {
    pub fn new() -> Self {
        Self {
            id: next_workspace_id(),
            title: String::new(),
            custom_title: None,
            working_directory: None,
            git_branch: None,
            color: None,
            pinned: false,
            has_bell: false,
            panes: HashMap::new(),
            split_tree: SplitTree::new(),
            status_entries: HashMap::new(),
            log_entries: Vec::new(),
            progress: None,
            remote_config: None,
            remote_state: RemoteConnectionState::default(),
            remote_daemon_status: None,
            proxy_endpoint: None,
        }
    }

    /// The pane that currently has focus (delegated to split_tree).
    pub fn focused_pane(&self) -> Option<PaneId> {
        self.split_tree.focused_pane()
    }

    /// Set the focused pane (delegated to split_tree).
    pub fn set_focused_pane(&mut self, pane_id: PaneId) {
        self.split_tree.set_focused(pane_id);
    }

    /// The display title: custom_title if set, otherwise title, otherwise default.
    pub fn display_title(&self) -> &str {
        if let Some(ref ct) = self.custom_title {
            if !ct.is_empty() {
                return ct;
            }
        }
        if !self.title.is_empty() {
            return &self.title;
        }
        "Terminal"
    }

    /// Register a pane in this workspace.
    pub fn add_pane(&mut self, pane: Pane) -> PaneId {
        let id = pane.id;
        self.panes.insert(id, pane);
        id
    }

    /// Remove a pane by ID.
    pub fn remove_pane(&mut self, pane_id: PaneId) -> Option<Pane> {
        self.panes.remove(&pane_id)
    }

    /// Get all surface IDs across all panes.
    pub fn all_surface_ids(&self) -> Vec<SurfaceId> {
        self.panes
            .values()
            .flat_map(|p| p.surface_ids())
            .collect()
    }

    /// Get all browser panel IDs across all panes.
    pub fn all_browser_ids(&self) -> Vec<BrowserPanelId> {
        self.panes
            .values()
            .flat_map(|p| p.browser_ids())
            .collect()
    }

    /// Get all panels across all panes (for closing an entire workspace).
    pub fn all_panels(&self) -> Vec<PanelKind> {
        self.panes
            .values()
            .flat_map(|p| p.tabs.iter().map(|t| t.panel.clone()))
            .collect()
    }

    /// Find which pane contains a given surface ID.
    pub fn pane_for_surface(&self, surface_id: SurfaceId) -> Option<PaneId> {
        self.panes
            .iter()
            .find(|(_, p)| p.has_surface(surface_id))
            .map(|(id, _)| *id)
    }

    /// Find which pane contains a given browser panel ID.
    pub fn pane_for_browser(&self, browser_id: BrowserPanelId) -> Option<PaneId> {
        self.panes
            .iter()
            .find(|(_, p)| p.has_browser(browser_id))
            .map(|(id, _)| *id)
    }

    /// Update title based on the focused pane's active tab.
    pub fn update_title_from_focused(&mut self) {
        if let Some(pane_id) = self.split_tree.focused_pane() {
            if let Some(pane) = self.panes.get(&pane_id) {
                if let Some(tab) = pane.tabs.get(pane.selected_tab) {
                    if !tab.title.is_empty() {
                        self.title = tab.title.clone();
                    }
                    if tab.working_directory.is_some() {
                        self.working_directory = tab.working_directory.clone();
                    }
                }
            }
        }
    }
}
