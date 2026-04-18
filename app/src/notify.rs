//! Desktop notifications via the freedesktop Notifications D-Bus interface.
//!
//! Sends system notifications when a bell rings in an unfocused workspace/pane,
//! especially when the window itself is not active. Rate-limited to prevent
//! notification spam from rapid bell sequences.
//!
//! Uses `org.freedesktop.Notifications.Notify` directly over D-Bus (via GDBus)
//! instead of `gio::Notification`, which requires an installed `.desktop` file
//! matching the app ID — a requirement that fails during development.

use std::cell::Cell;
use std::collections::HashMap;
use std::time::Instant;

use gtk4::prelude::*;
use gtk4::{gio, glib};

use crate::workspace::WorkspaceId;

thread_local! {
    /// Whether the GTK window is currently focused.
    static WINDOW_ACTIVE: Cell<bool> = const { Cell::new(true) };
    /// Whether notifications are enabled.
    static ENABLED: Cell<bool> = const { Cell::new(true) };
    /// Per-workspace rate limiter: last notification time.
    static LAST_NOTIFY: std::cell::RefCell<HashMap<WorkspaceId, Instant>> =
        std::cell::RefCell::new(HashMap::new());
    /// Global rate limiter: timestamps of recent notifications.
    static RECENT: std::cell::RefCell<Vec<Instant>> =
        std::cell::RefCell::new(Vec::new());
    /// Cached D-Bus connection.
    static DBUS_CONN: std::cell::RefCell<Option<gio::DBusConnection>> =
        std::cell::RefCell::new(None);
}

/// Minimum interval between notifications for the same workspace (seconds).
const PER_WORKSPACE_COOLDOWN_SECS: u64 = 5;
/// Maximum notifications globally within the burst window.
const GLOBAL_MAX_BURST: usize = 3;
/// Burst window duration (seconds).
const GLOBAL_BURST_WINDOW_SECS: u64 = 10;

/// Called from window focus tracking in main.rs.
pub fn set_window_active(active: bool) {
    WINDOW_ACTIVE.with(|c| c.set(active));
}

/// Whether the window is currently active/focused.
pub fn is_window_active() -> bool {
    WINDOW_ACTIVE.with(|c| c.get())
}

/// Enable notifications.
pub fn enable() {
    ENABLED.with(|c| c.set(true));
}

/// Disable notifications.
pub fn disable() {
    ENABLED.with(|c| c.set(false));
}

/// Whether notifications are currently enabled.
pub fn is_enabled() -> bool {
    ENABLED.with(|c| c.get())
}

/// Send a bell notification for a workspace, if rate limits allow.
/// Only fires when the window is not active.
pub fn send_bell_notification(workspace_title: &str, workspace_id: WorkspaceId) {
    if !ENABLED.with(|c| c.get()) {

        return;
    }

    // Only notify when the window is not focused
    if WINDOW_ACTIVE.with(|c| c.get()) {

        return;
    }

    let now = Instant::now();

    // Per-workspace cooldown
    let throttled = LAST_NOTIFY.with(|map| {
        let map = map.borrow();
        if let Some(last) = map.get(&workspace_id) {
            now.duration_since(*last).as_secs() < PER_WORKSPACE_COOLDOWN_SECS
        } else {
            false
        }
    });
    if throttled {

        return;
    }

    // Global burst limiter
    let burst_exceeded = RECENT.with(|vec| {
        let mut vec = vec.borrow_mut();
        let cutoff = now - std::time::Duration::from_secs(GLOBAL_BURST_WINDOW_SECS);
        vec.retain(|t| *t > cutoff);
        vec.len() >= GLOBAL_MAX_BURST
    });
    if burst_exceeded {

        return;
    }

    // Record this notification
    LAST_NOTIFY.with(|map| {
        map.borrow_mut().insert(workspace_id, now);
    });
    RECENT.with(|vec| {
        vec.borrow_mut().push(now);
    });

    // Send via freedesktop Notifications D-Bus interface
    let summary = format!("Bell in {}", workspace_title);
    let body = format!("Terminal bell in workspace \"{}\"", workspace_title);

    send_dbus_notification(&summary, &body);
}

/// Send a notification via org.freedesktop.Notifications.Notify over D-Bus.
fn send_dbus_notification(summary: &str, body: &str) {
    let conn = DBUS_CONN.with(|cell| {
        let mut cached = cell.borrow_mut();
        if cached.is_none() {
            match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
                Ok(c) => *cached = Some(c),
                Err(e) => {
                    eprintln!("[notify] failed to connect to session bus: {e}");
                    return None;
                }
            }
        }
        cached.clone()
    });

    let Some(conn) = conn else { return };

    // org.freedesktop.Notifications.Notify signature:
    // (app_name: s, replaces_id: u, app_icon: s, summary: s, body: s,
    //  actions: as, hints: a{sv}, expire_timeout: i)
    //
    // Build from GVariant format string for the tricky array types.
    let params = glib::Variant::parse(
        None,
        &format!(
            "('Limux', uint32 0, 'utilities-terminal', '{}', '{}', @as [], @a{{sv}} {{}}, 5000)",
            summary.replace('\'', "\\'"),
            body.replace('\'', "\\'"),
        ),
    );
    let params = match params {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[notify] failed to build D-Bus params: {e}");
            return;
        }
    };

    // Fire-and-forget async call
    conn.call(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
        "Notify",
        Some(&params),
        None,
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
        |result| {
            if let Err(e) = result {
                eprintln!("[notify] D-Bus Notify call failed: {e}");
            }
        },
    );
}
