//! Settings data model and persistence.
//!
//! Settings file: `$XDG_CONFIG_HOME/limux/settings.json`
//! (defaults to `~/.config/limux/settings.json`)
//!
//! Terminal appearance (font, colors, cursor) lives in Ghostty's own config
//! file (`~/.config/ghostty/config`). This module only manages limux-specific
//! preferences.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ── Data model ─────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Settings {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub general: GeneralSettings,
    #[serde(default)]
    pub sidebar: SidebarSettings,
    #[serde(default)]
    pub shortcuts: HashMap<String, String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct GeneralSettings {
    pub default_shell: Option<String>,
    pub working_directory: Option<String>,
    pub session_restore: Option<bool>,
    pub notifications_enabled: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SidebarSettings {
    pub width: Option<i32>,
    pub visible: Option<bool>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            general: GeneralSettings::default(),
            sidebar: SidebarSettings::default(),
            shortcuts: HashMap::new(),
        }
    }
}

impl Settings {
    /// Get the effective value for session restore (default: true).
    pub fn session_restore(&self) -> bool {
        self.general.session_restore.unwrap_or(true)
    }

    /// Get the effective value for notifications enabled (default: true).
    pub fn notifications_enabled(&self) -> bool {
        self.general.notifications_enabled.unwrap_or(true)
    }

    /// Get the effective sidebar width (default: 180).
    pub fn sidebar_width(&self) -> i32 {
        self.sidebar.width.unwrap_or(180)
    }

    /// Get the effective sidebar visibility (default: true).
    pub fn sidebar_visible(&self) -> bool {
        self.sidebar.visible.unwrap_or(true)
    }

    /// Get the accelerator for a shortcut action, falling back to defaults.
    pub fn shortcut_accel(&self, action: &str) -> Option<&str> {
        if let Some(accel) = self.shortcuts.get(action) {
            Some(accel.as_str())
        } else {
            default_shortcut(action)
        }
    }
}

// ── Shortcut defaults ──────────────────────────────────────────────

/// All shortcut definitions with their default accelerators and display labels.
pub struct ShortcutDef {
    pub action: &'static str,
    pub default_accel: &'static str,
    pub label: &'static str,
}

/// Complete list of shortcut defaults matching the hardcoded values in window.rs.
pub const SHORTCUT_DEFAULTS: &[ShortcutDef] = &[
    // Workspace
    ShortcutDef { action: "new-workspace", default_accel: "<Ctrl><Shift>t", label: "New Workspace" },
    ShortcutDef { action: "close-workspace", default_accel: "<Ctrl><Shift>w", label: "Close Workspace" },
    ShortcutDef { action: "next-workspace", default_accel: "<Ctrl>Page_Down", label: "Next Workspace" },
    ShortcutDef { action: "prev-workspace", default_accel: "<Ctrl>Page_Up", label: "Previous Workspace" },
    // Splits
    ShortcutDef { action: "split-right", default_accel: "<Ctrl><Shift>Return", label: "Split Right" },
    ShortcutDef { action: "split-down", default_accel: "<Ctrl><Shift>e", label: "Split Down" },
    ShortcutDef { action: "next-split", default_accel: "<Ctrl><Shift>bracketright", label: "Next Split" },
    ShortcutDef { action: "prev-split", default_accel: "<Ctrl><Shift>bracketleft", label: "Previous Split" },
    ShortcutDef { action: "equalize-splits", default_accel: "<Ctrl><Shift>plus", label: "Equalize Splits" },
    // Directional navigation
    ShortcutDef { action: "split-left", default_accel: "<Alt>Left", label: "Navigate Left" },
    ShortcutDef { action: "split-right-nav", default_accel: "<Alt>Right", label: "Navigate Right" },
    ShortcutDef { action: "split-up", default_accel: "<Alt>Up", label: "Navigate Up" },
    ShortcutDef { action: "split-down-nav", default_accel: "<Alt>Down", label: "Navigate Down" },
    // Pane tabs
    ShortcutDef { action: "new-pane-tab", default_accel: "<Ctrl>t", label: "New Tab in Pane" },
    ShortcutDef { action: "close-pane-tab", default_accel: "<Ctrl>w", label: "Close Tab in Pane" },
    ShortcutDef { action: "next-pane-tab", default_accel: "<Ctrl>Tab", label: "Next Tab in Pane" },
    ShortcutDef { action: "prev-pane-tab", default_accel: "<Ctrl><Shift>Tab", label: "Previous Tab in Pane" },
    // Sidebar
    ShortcutDef { action: "toggle-sidebar", default_accel: "<Ctrl><Shift>backslash", label: "Toggle Sidebar" },
    // Browser
    ShortcutDef { action: "open-browser", default_accel: "<Ctrl><Shift>b", label: "Open Browser" },
    ShortcutDef { action: "focus-address-bar", default_accel: "<Ctrl>l", label: "Focus Address Bar" },
    ShortcutDef { action: "browser-find", default_accel: "<Ctrl>f", label: "Browser Find" },
    // Settings
    ShortcutDef { action: "open-settings", default_accel: "<Ctrl>comma", label: "Open Settings" },
];

/// Get the default accelerator for an action.
fn default_shortcut(action: &str) -> Option<&'static str> {
    SHORTCUT_DEFAULTS
        .iter()
        .find(|d| d.action == action)
        .map(|d| d.default_accel)
}

/// Get the merged shortcut map: user overrides + defaults for missing entries.
pub fn merged_shortcuts(settings: &Settings) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for def in SHORTCUT_DEFAULTS {
        let accel = settings
            .shortcuts
            .get(def.action)
            .cloned()
            .unwrap_or_else(|| def.default_accel.to_string());
        map.insert(def.action.to_string(), accel);
    }
    map
}

// ── Persistence ────────────────────────────────────────────────────

fn settings_file_path() -> PathBuf {
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(home).join(".config")
        });
    config_dir.join("limux").join("settings.json")
}

/// Load settings from disk. Returns defaults if file is missing or corrupt.
pub fn load() -> Settings {
    let path = settings_file_path();
    match std::fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => Settings::default(),
    }
}

/// Save settings to disk.
pub fn save(settings: &Settings) -> Result<(), String> {
    let path = settings_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let json = serde_json::to_string_pretty(settings).map_err(|e| format!("json: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("write: {e}"))?;
    Ok(())
}

// ── Global access ──────────────────────────────────────────────────

thread_local! {
    static SETTINGS: RefCell<Settings> = RefCell::new(Settings::default());
}

/// Initialize settings from disk. Call once at startup.
pub fn init() {
    let s = load();
    SETTINGS.with(|cell| {
        *cell.borrow_mut() = s;
    });
}

/// Get a clone of the current settings.
pub fn get() -> Settings {
    SETTINGS.with(|cell| cell.borrow().clone())
}

/// Update settings in memory and persist to disk.
pub fn update(f: impl FnOnce(&mut Settings)) {
    SETTINGS.with(|cell| {
        let mut s = cell.borrow_mut();
        f(&mut s);
        if let Err(e) = save(&s) {
            eprintln!("Settings save failed: {e}");
        }
    });
}

/// Get the path to the Ghostty config file.
pub fn ghostty_config_path() -> PathBuf {
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(home).join(".config")
        });
    config_dir.join("ghostty").join("config")
}
