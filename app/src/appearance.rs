//! Color scheme detection and runtime theme switching.
//!
//! Detects system dark/light preference via GTK4 Settings and the
//! freedesktop `org.freedesktop.appearance` portal. Triggers CSS reload
//! when the preference changes at runtime.

use std::cell::Cell;

use gtk4::prelude::*;
use gtk4::{self, glib};

thread_local! {
    static IS_DARK: Cell<bool> = const { Cell::new(false) };
}

/// Whether the system is currently using a dark color scheme.
pub fn is_dark() -> bool {
    IS_DARK.with(|c| c.get())
}

/// Initialize color scheme detection and connect change listeners.
/// Must be called after the GtkApplication is active (display available).
pub fn init() {
    let dark = detect_color_scheme();
    IS_DARK.with(|c| c.set(dark));

    // Listen for GTK setting changes
    if let Some(settings) = gtk4::Settings::default() {
        settings.connect_gtk_application_prefer_dark_theme_notify(|_settings| {
            on_scheme_changed();
        });

        // Also listen to gtk-theme-name changes — some DEs switch the theme
        // name (e.g. "Adwaita" → "Adwaita-dark") rather than the boolean.
        settings.connect_gtk_theme_name_notify(|_settings| {
            on_scheme_changed();
        });
    }

    // Also try the portal D-Bus signal for Wayland compositors that set
    // color-scheme without updating GTK properties directly.
    connect_portal_signal();
}

/// Detect whether the system prefers a dark color scheme.
fn detect_color_scheme() -> bool {
    // 1. Check GTK settings boolean
    if let Some(settings) = gtk4::Settings::default() {
        if settings.is_gtk_application_prefer_dark_theme() {
            return true;
        }

        // Check if the theme name contains "dark" (common convention)
        if let Some(theme) = settings.gtk_theme_name() {
            let theme_lower = theme.to_lowercase();
            if theme_lower.contains("dark") {
                return true;
            }
        }
    }

    // 2. Try freedesktop portal color-scheme via GSettings
    //    color-scheme: 0 = no preference, 1 = prefer dark, 2 = prefer light
    if let Ok(portal_settings) = gio_portal_color_scheme() {
        return portal_settings == 1;
    }

    false
}

/// Query org.freedesktop.appearance color-scheme via D-Bus portal.
/// Returns 0 (no pref), 1 (dark), or 2 (light).
fn gio_portal_color_scheme() -> Result<u32, ()> {
    // Use GDBusProxy to call org.freedesktop.portal.Settings.Read
    // Synchronous fallback: try reading from the portal via blocking call
    // We use gio's synchronous proxy since we're on the main thread at init time.
    let bus = gtk4::gio::bus_get_sync(gtk4::gio::BusType::Session, gtk4::gio::Cancellable::NONE)
        .map_err(|_| ())?;

    let result = bus.call_sync(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Settings",
        "Read",
        Some(&glib::Variant::tuple_from_iter([
            "org.freedesktop.appearance".to_variant(),
            "color-scheme".to_variant(),
        ])),
        None,
        gtk4::gio::DBusCallFlags::NONE,
        1000, // 1 second timeout
        gtk4::gio::Cancellable::NONE,
    ).map_err(|_| ())?;

    // Result is (v,) where v is a variant containing a u32
    let outer = result.child_value(0);
    // The portal wraps the value in a variant
    let inner = outer.get::<glib::Variant>().ok_or(())?;
    inner.get::<u32>().ok_or(())
}

/// Connect to the portal's SettingChanged D-Bus signal for runtime updates.
fn connect_portal_signal() {
    let Ok(bus) = gtk4::gio::bus_get_sync(
        gtk4::gio::BusType::Session,
        gtk4::gio::Cancellable::NONE,
    ) else {
        return;
    };

    bus.signal_subscribe(
        Some("org.freedesktop.portal.Desktop"),
        Some("org.freedesktop.portal.Settings"),
        Some("SettingChanged"),
        Some("/org/freedesktop/portal/desktop"),
        Some("org.freedesktop.appearance"),
        gtk4::gio::DBusSignalFlags::NONE,
        |_connection, _sender, _path, _interface, _signal, params| {
            // params: (namespace: s, key: s, value: v)
            if let Some(key) = params.child_value(1).get::<String>() {
                if key == "color-scheme" {
                    on_scheme_changed();
                }
            }
        },
    );
}

/// Called when any color scheme indicator changes. Re-detects and reloads CSS if needed.
fn on_scheme_changed() {
    let new_dark = detect_color_scheme();
    let old_dark = IS_DARK.with(|c| c.get());
    if new_dark != old_dark {
        IS_DARK.with(|c| c.set(new_dark));
        crate::sidebar::reload_css(new_dark);
    }
}
