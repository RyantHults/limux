//! Desktop integration: detect + perform.
//!
//! See `scripts/build-appimage.sh` AppRun --install handler for what the
//! integration actually does (copies .desktop + icon, refreshes caches).

use std::path::PathBuf;

/// True iff this process is running from an AppImage (i.e. $APPIMAGE is set
/// and the file exists). When false, desktop integration is not applicable.
pub fn is_appimage() -> bool {
    std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .map(|p| p.is_file())
        .unwrap_or(false)
}

/// Path to the AppImage we're running from, if any.
pub fn appimage_path() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE").map(PathBuf::from)
}

/// True if desktop integration has already been performed — i.e. the
/// `.desktop` file is present in `~/.local/share/applications/`.
pub fn is_integrated() -> bool {
    if let Some(desktop_dir) = xdg_dir("XDG_DATA_HOME", ".local/share") {
        let desktop = desktop_dir.join("applications").join("limux.desktop");
        if desktop.is_file() {
            return true;
        }
    }
    false
}

/// Spawn the AppImage with `--install` and wait for it. Returns Ok if the
/// child exits with status 0, Err otherwise. Returns Err if not running
/// from an AppImage.
pub fn perform_integration() -> Result<(), String> {
    let appimage = appimage_path().ok_or("not running from an AppImage")?;
    let status = std::process::Command::new(&appimage)
        .arg("--install")
        .status()
        .map_err(|e| format!("failed to spawn {}: {}", appimage.display(), e))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("integration exited with {}", status))
    }
}

/// Helper: resolve an XDG-style directory, defaulting to ~/.local/share.
fn xdg_dir(env_var: &str, default_suffix: &str) -> Option<PathBuf> {
    if let Some(v) = std::env::var_os(env_var) {
        let p = PathBuf::from(v);
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(default_suffix);
        return Some(p);
    }
    None
}
