//! Startup update checks: compare the running version against the latest
//! GitHub release and offer to open the release page.
//!
//! Checks run on a background thread so startup is never blocked; results
//! are dispatched back to the GTK main thread. Any failure (no curl/wget,
//! no network, unparseable response) is a silent no-op.

use std::process::Command;

use gtk4::prelude::*;
use gtk4;

use crate::install;
use crate::settings;

/// GitHub "latest release" endpoint. Daemon releases share the same repo,
/// but every daemon release ships with a main-app version bump, so the
/// newest release is always a `vX.Y.Z` limux release.
const RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/RyantHults/limux/releases/latest";

/// Minimum seconds between checks for "daily" frequency.
const DAILY_SECS: i64 = 24 * 60 * 60;
/// Minimum seconds between checks for "weekly" frequency.
const WEEKLY_SECS: i64 = 7 * DAILY_SECS;

/// A parsed `X.Y.Z` version.
struct Version {
    major: u32,
    minor: u32,
    patch: u32,
}

impl Version {
    fn parse(s: &str) -> Option<Version> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut parts = s.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(Version { major, minor, patch })
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.major == other.major && self.minor == other.minor && self.patch == other.patch
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(
            (self.major, self.minor, self.patch)
                .cmp(&(other.major, other.minor, other.patch)),
        )
    }
}

/// Whether a check is due right now based on the configured frequency.
fn check_due() -> bool {
    let settings = settings::get();
    if !settings.check_for_updates() {
        return false;
    }
    let Some(last) = settings.last_update_check() else {
        return true;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let interval = match settings.update_check_frequency().as_str() {
        "startup" => return true,
        "weekly" => WEEKLY_SECS,
        _ => DAILY_SECS,
    };
    now.saturating_sub(last) >= interval
}

/// Kick off an update check for the given parent window. Returns
/// immediately; the network fetch runs on a background thread. No-op unless
/// gating conditions (AppImage, enabled, due) all hold.
pub fn maybe_check(parent: &gtk4::ApplicationWindow) {
    if !install::is_appimage() {
        return;
    }
    if !check_due() {
        return;
    }

    let parent = parent.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("limux-update-check".into())
        .spawn(move || {
            let result = fetch_latest();
            let _ = tx.send(result);
        })
        .ok();

    // Poll the channel on the GTK main thread. The closure is non-Send, so
    // it may own widgets directly.
    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        match rx.try_recv() {
            Ok(result) => {
                match result {
                    Ok(Some(latest)) => {
                        let current = env!("CARGO_PKG_VERSION");
                        let latest_ver = Version::parse(&latest.tag_name);
                        let current_ver = Version::parse(current);
                        if let (Some(latest_ver), Some(current_ver)) = (latest_ver, current_ver) {
                            if latest_ver > current_ver {
                                // Already skipped for exactly this version? Stay quiet.
                                if settings::get().skip_update_version()
                                    == Some(latest.tag_name.clone())
                                {
                                    return glib::ControlFlow::Break;
                                }
                                show_prompt(&parent, &latest.tag_name, &latest.html_url);
                            }
                        }
                    }
                    Ok(None) | Err(_) => {
                        // No network / bad response: keep quiet.
                    }
                }
                glib::ControlFlow::Break
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
        }
    });
}

/// Result of fetching the latest release info.
struct LatestRelease {
    tag_name: String,
    html_url: String,
}

/// Fetch the latest release JSON via curl (fallback wget) and extract the
/// tag name + HTML URL. Returns `Ok(None)` when no release exists.
fn fetch_latest() -> Result<Option<LatestRelease>, String> {
    let url = RELEASES_LATEST_URL;
    let out = Command::new("curl")
        .args(["-fsSL", "--connect-timeout", "5", "--max-time", "15"])
        .arg(url)
        .output();
    let output = match out {
        Ok(o) if o.status.success() => o,
        Ok(_) => {
            // Fall back to wget.
            let o = Command::new("wget")
                .args(["-q", "-O", "-", "--timeout=15", "-T", "15"])
                .arg(url)
                .output();
            match o {
                Ok(o) if o.status.success() => o,
                _ => return Err("download failed".into()),
            }
        }
        Err(_) => {
            // curl missing; try wget directly.
            let o = Command::new("wget")
                .args(["-q", "-O", "-", "--timeout=15", "-T", "15"])
                .arg(url)
                .output();
            match o {
                Ok(o) if o.status.success() => o,
                _ => return Err("download failed".into()),
            }
        }
    };

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;
    let tag_name = json
        .get("tag_name")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "missing tag_name".to_string())?;
    let html_url = json
        .get("html_url")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "missing html_url".to_string())?;

    // Cheap safety net: only treat vX.Y.Z tags as app releases.
    if Version::parse(&tag_name).is_none() {
        return Ok(None);
    }

    // Record the check regardless of outcome so the frequency gate applies.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    settings::update(|s| s.general.last_update_check = Some(now));

    Ok(Some(LatestRelease { tag_name, html_url }))
}

/// Show the modal "new version available" prompt on the given window.
fn show_prompt(parent: &gtk4::ApplicationWindow, new_version: &str, html_url: &str) {
    let current = env!("CARGO_PKG_VERSION");

    let dialog = gtk4::Window::builder()
        .title("Update available")
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .build();

    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    vbox.set_margin_top(20);
    vbox.set_margin_bottom(20);
    vbox.set_margin_start(20);
    vbox.set_margin_end(20);

    let message_label = gtk4::Label::new(Some("A new version of limux is available"));
    message_label.set_halign(gtk4::Align::Start);
    message_label.add_css_class("heading");
    vbox.append(&message_label);

    let detail_label = gtk4::Label::new(Some(&format!(
        "limux {} is available. You are running {}.",
        new_version.trim_start_matches('v'),
        current
    )));
    detail_label.set_wrap(true);
    detail_label.set_halign(gtk4::Align::Start);
    detail_label.set_max_width_chars(60);
    vbox.append(&detail_label);

    vbox.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));

    let button_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    button_box.set_halign(gtk4::Align::End);

    let not_now_btn = gtk4::Button::with_label("Not now");
    let skip_btn = gtk4::Button::with_label("Skip this version");
    let update_btn = gtk4::Button::with_label("Update");
    update_btn.add_css_class("suggested-action");

    button_box.append(&not_now_btn);
    button_box.append(&skip_btn);
    button_box.append(&update_btn);
    vbox.append(&button_box);

    dialog.set_child(Some(&vbox));

    let dialog_close = dialog.clone();
    not_now_btn.connect_clicked(move |_| dialog_close.close());

    let dialog_skip = dialog.clone();
    let version = new_version.to_string();
    skip_btn.connect_clicked(move |_| {
        settings::update(|s| s.general.skip_update_version = Some(version.clone()));
        dialog_skip.close();
    });

    let dialog_update = dialog.clone();
    let url = html_url.to_string();
    update_btn.connect_clicked(move |_| {
        let _ = Command::new("xdg-open").arg(&url).spawn();
        dialog_update.close();
    });

    dialog.present();
}
