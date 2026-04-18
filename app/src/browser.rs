//! Browser panel — WebKitGTK integration for browser panes.
//!
//! Provides a browser widget (toolbar + WebKitWebView) that can be placed
//! in a pane's GtkStack alongside terminal GLAreas.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString};

use glib::translate::FromGlibPtrNone;
use gtk4::prelude::*;
use gtk4::{self, glib};

// ── FFI bindings for WebKitGTK 6.0 ────────────────────────────────────

#[allow(non_camel_case_types)]
type WebKitWebView = libc::c_void;
#[allow(non_camel_case_types)]
type WebKitFindController = libc::c_void;

#[allow(non_camel_case_types)]
type WebKitNetworkSession = libc::c_void;
#[allow(non_camel_case_types)]
type WebKitNetworkProxySettings = libc::c_void;

// Proxy mode enum values for webkit_network_session_set_proxy_settings
#[allow(dead_code)]
const WEBKIT_NETWORK_PROXY_MODE_DEFAULT: u32 = 0;
const WEBKIT_NETWORK_PROXY_MODE_NO_PROXY: u32 = 1;
const WEBKIT_NETWORK_PROXY_MODE_CUSTOM: u32 = 2;

// Find options bitfield
const WEBKIT_FIND_OPTIONS_CASE_INSENSITIVE: u32 = 1 << 0;
const WEBKIT_FIND_OPTIONS_WRAP_AROUND: u32 = 1 << 4;

unsafe extern "C" {
    // WebKitWebView
    fn webkit_web_view_new() -> *mut gtk4::ffi::GtkWidget;
    fn webkit_web_view_load_uri(web_view: *mut WebKitWebView, uri: *const libc::c_char);
    fn webkit_web_view_get_title(web_view: *mut WebKitWebView) -> *const libc::c_char;
    fn webkit_web_view_get_uri(web_view: *mut WebKitWebView) -> *const libc::c_char;
    fn webkit_web_view_reload(web_view: *mut WebKitWebView);
    fn webkit_web_view_go_back(web_view: *mut WebKitWebView);
    fn webkit_web_view_go_forward(web_view: *mut WebKitWebView);
    fn webkit_web_view_can_go_back(web_view: *mut WebKitWebView) -> glib::ffi::gboolean;
    fn webkit_web_view_can_go_forward(web_view: *mut WebKitWebView) -> glib::ffi::gboolean;
    fn webkit_web_view_is_loading(web_view: *mut WebKitWebView) -> glib::ffi::gboolean;

    // FindController
    fn webkit_web_view_get_find_controller(
        web_view: *mut WebKitWebView,
    ) -> *mut WebKitFindController;
    fn webkit_find_controller_search(
        find_controller: *mut WebKitFindController,
        search_text: *const libc::c_char,
        find_options: u32,
        max_match_count: libc::c_uint,
    );
    fn webkit_find_controller_search_finish(find_controller: *mut WebKitFindController);
    fn webkit_find_controller_search_next(find_controller: *mut WebKitFindController);
    fn webkit_find_controller_search_previous(find_controller: *mut WebKitFindController);

    // Network session + proxy + object construction
    fn webkit_web_view_get_type() -> glib::ffi::GType;
    fn webkit_network_session_new(
        data_directory: *const libc::c_char,
        cache_directory: *const libc::c_char,
    ) -> *mut WebKitNetworkSession;
    fn webkit_web_view_get_network_session(
        web_view: *mut WebKitWebView,
    ) -> *mut WebKitNetworkSession;
    fn webkit_network_proxy_settings_new(
        default_proxy_uri: *const libc::c_char,
        ignore_hosts: *const *const libc::c_char,
    ) -> *mut WebKitNetworkProxySettings;
    fn webkit_network_session_set_proxy_settings(
        session: *mut WebKitNetworkSession,
        proxy_mode: u32,
        proxy_settings: *mut WebKitNetworkProxySettings,
    );

    // JavaScript evaluation
    fn webkit_web_view_evaluate_javascript(
        web_view: *mut WebKitWebView,
        script: *const libc::c_char,
        length: isize,
        world_name: *const libc::c_char,
        source_uri: *const libc::c_char,
        cancellable: *mut libc::c_void,
        callback: Option<unsafe extern "C" fn(*mut libc::c_void, *mut libc::c_void, *mut libc::c_void)>,
        user_data: *mut libc::c_void,
    );
}

// ── Browser panel ID ───────────────────────────────────────────────────

pub type BrowserPanelId = u32;

crate::make_id_allocator!(next_browser_id, NEXT_BROWSER_ID);

// ── Browser panel state ────────────────────────────────────────────────

// BrowserPanel state is tracked in the workspace data model (PanelKind::Browser)
// and in the BrowserEntry registry below. No separate struct needed.

// ── Browser panel registry ─────────────────────────────────────────────

struct BrowserEntry {
    webview_ptr: *mut WebKitWebView,
    widget: gtk4::Widget,
    address_bar: gtk4::Entry,
}

thread_local! {
    static BROWSERS: RefCell<HashMap<BrowserPanelId, BrowserEntry>> = RefCell::new(HashMap::new());
}

/// Immutable access to a browser entry by ID.
fn with_browser<R>(id: BrowserPanelId, f: impl FnOnce(&BrowserEntry) -> R) -> Option<R> {
    BROWSERS.with(|b| {
        let browsers = b.borrow();
        let entry = browsers.get(&id)?;
        Some(f(entry))
    })
}

/// Create a flat icon button with a tooltip.
fn icon_button(icon: &str, tooltip: &str) -> gtk4::Button {
    let btn = gtk4::Button::from_icon_name(icon);
    btn.set_tooltip_text(Some(tooltip));
    btn.add_css_class("flat");
    btn
}

// ── Create a browser widget ────────────────────────────────────────────

/// Create a browser panel widget, returning the top-level container and the
/// widget to embed in a GtkStack (the container itself).
///
/// The returned `gtk4::Box` contains:
///   - Toolbar: back, forward, reload buttons + address bar
///   - WebKitWebView filling the remaining space
pub fn create(id: BrowserPanelId, initial_url: &str) -> gtk4::Box {
    create_inner(id, initial_url, None)
}

/// Create a browser panel with a SOCKS5 proxy pre-configured for remote browsing.
pub fn create_with_proxy(
    id: BrowserPanelId,
    initial_url: &str,
    endpoint: &crate::remote::ProxyEndpoint,
) -> gtk4::Box {
    create_inner(id, initial_url, Some(endpoint))
}

fn create_inner(
    id: BrowserPanelId,
    initial_url: &str,
    proxy: Option<&crate::remote::ProxyEndpoint>,
) -> gtk4::Box {
    let container = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    container.add_css_class("browser-panel");

    // ── Toolbar ────────────────────────────────────────────────────
    let toolbar = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    toolbar.add_css_class("browser-toolbar");
    toolbar.set_margin_start(4);
    toolbar.set_margin_end(4);
    toolbar.set_margin_top(2);
    toolbar.set_margin_bottom(2);

    let back_btn = icon_button("go-previous-symbolic", "Back");
    back_btn.set_sensitive(false);

    let forward_btn = icon_button("go-next-symbolic", "Forward");
    forward_btn.set_sensitive(false);

    let reload_btn = icon_button("view-refresh-symbolic", "Reload");

    let address_bar = gtk4::Entry::new();
    address_bar.set_hexpand(true);
    address_bar.set_placeholder_text(Some("Enter URL…"));
    address_bar.add_css_class("browser-address-bar");
    if !initial_url.is_empty() {
        address_bar.set_text(initial_url);
    }

    toolbar.append(&back_btn);
    toolbar.append(&forward_btn);
    toolbar.append(&reload_btn);
    toolbar.append(&address_bar);
    container.append(&toolbar);

    // ── Find bar (hidden by default) ───────────────────────────────
    let find_bar = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    find_bar.add_css_class("browser-find-bar");
    find_bar.set_margin_start(4);
    find_bar.set_margin_end(4);
    find_bar.set_margin_top(2);
    find_bar.set_margin_bottom(2);
    find_bar.set_visible(false);
    find_bar.set_widget_name("find-bar");

    let find_entry = gtk4::Entry::new();
    find_entry.set_hexpand(true);
    find_entry.set_placeholder_text(Some("Find in page…"));

    let find_prev_btn = icon_button("go-up-symbolic", "Previous match");
    let find_next_btn = icon_button("go-down-symbolic", "Next match");
    let find_close_btn = icon_button("window-close-symbolic", "Close");

    let find_count_label = gtk4::Label::new(None);
    find_count_label.set_widget_name("find-count");
    find_count_label.add_css_class("caption");

    find_bar.append(&find_entry);
    find_bar.append(&find_count_label);
    find_bar.append(&find_prev_btn);
    find_bar.append(&find_next_btn);
    find_bar.append(&find_close_btn);
    container.append(&find_bar);

    // ── WebKitWebView ──────────────────────────────────────────────
    let webview_ptr = if let Some(ep) = proxy {
        // Create an isolated network session with SOCKS5 proxy for remote workspaces.
        unsafe {
            let session = webkit_network_session_new(std::ptr::null(), std::ptr::null());
            let proxy_uri = ep.socks5_uri();
            if let Ok(uri_c) = CString::new(proxy_uri.as_str()) {
                let settings =
                    webkit_network_proxy_settings_new(uri_c.as_ptr(), std::ptr::null());
                webkit_network_session_set_proxy_settings(
                    session,
                    WEBKIT_NETWORK_PROXY_MODE_CUSTOM,
                    settings,
                );
            }
            let prop_name = CString::new("network-session").unwrap();
            glib::gobject_ffi::g_object_new(
                webkit_web_view_get_type(),
                prop_name.as_ptr(),
                session,
                std::ptr::null::<libc::c_char>(),
            ) as *mut gtk4::ffi::GtkWidget
        }
    } else {
        unsafe { webkit_web_view_new() }
    };
    let webview_widget: gtk4::Widget =
        unsafe { glib::Object::from_glib_none(webview_ptr as *mut glib::gobject_ffi::GObject) }
            .downcast()
            .expect("WebKitWebView should be a GtkWidget");
    webview_widget.set_vexpand(true);
    webview_widget.set_hexpand(true);
    webview_widget.set_focusable(true);
    container.append(&webview_widget);

    // Load initial URL
    let wv_ptr = webview_ptr as *mut WebKitWebView;
    if !initial_url.is_empty() {
        if let Ok(cstr) = CString::new(initial_url) {
            unsafe { webkit_web_view_load_uri(wv_ptr, cstr.as_ptr()) };
        }
    }

    // ── Signal: title changed → update tab label ───────────────────
    let browser_id = id;
    webview_widget.connect_notify_local(Some("title"), move |widget: &gtk4::Widget, _| {
        let ptr = widget.as_ptr() as *mut WebKitWebView;
        let title = unsafe { crate::util::cstr_to_string(webkit_web_view_get_title(ptr)) }
            .unwrap_or_default();
        crate::window::on_browser_title_changed(browser_id, &title);
    });

    // ── Signal: uri changed → update address bar ───────────────────
    let addr_clone = address_bar.clone();
    webview_widget.connect_notify_local(Some("uri"), move |widget: &gtk4::Widget, _| {
        let ptr = widget.as_ptr() as *mut WebKitWebView;
        let uri = unsafe { crate::util::cstr_to_string(webkit_web_view_get_uri(ptr)) }
            .unwrap_or_default();
        addr_clone.set_text(&uri);
    });

    // ── Signal: is-loading changed → update button state ───────────
    let reload_clone = reload_btn.clone();
    let back_clone2 = back_btn.clone();
    let forward_clone2 = forward_btn.clone();
    let browser_id2 = id;
    webview_widget.connect_notify_local(Some("is-loading"), move |widget: &gtk4::Widget, _| {
        let ptr = widget.as_ptr() as *mut WebKitWebView;
        let loading = unsafe { webkit_web_view_is_loading(ptr) != 0 };
        let can_back = unsafe { webkit_web_view_can_go_back(ptr) != 0 };
        let can_fwd = unsafe { webkit_web_view_can_go_forward(ptr) != 0 };

        if loading {
            reload_clone.set_icon_name("process-stop-symbolic");
            reload_clone.set_tooltip_text(Some("Stop"));
        } else {
            reload_clone.set_icon_name("view-refresh-symbolic");
            reload_clone.set_tooltip_text(Some("Reload"));
        }
        back_clone2.set_sensitive(can_back);
        forward_clone2.set_sensitive(can_fwd);

        // Update data model
        crate::window::on_browser_loading_changed(browser_id2, loading, can_back, can_fwd);
    });

    // ── Toolbar button actions ─────────────────────────────────────
    let wv_back = webview_widget.clone();
    back_btn.connect_clicked(move |_| {
        let ptr = wv_back.as_ptr() as *mut WebKitWebView;
        unsafe { webkit_web_view_go_back(ptr) };
    });

    let wv_fwd = webview_widget.clone();
    forward_btn.connect_clicked(move |_| {
        let ptr = wv_fwd.as_ptr() as *mut WebKitWebView;
        unsafe { webkit_web_view_go_forward(ptr) };
    });

    let wv_reload = webview_widget.clone();
    reload_btn.connect_clicked(move |_| {
        let ptr = wv_reload.as_ptr() as *mut WebKitWebView;
        unsafe { webkit_web_view_reload(ptr) };
    });

    // ── Address bar: Enter → navigate ──────────────────────────────
    let wv_nav = webview_widget.clone();
    address_bar.connect_activate(move |entry| {
        let text = entry.text().to_string();
        let url = normalize_url(&text);
        if let Ok(cstr) = CString::new(url.as_str()) {
            let ptr = wv_nav.as_ptr() as *mut WebKitWebView;
            unsafe { webkit_web_view_load_uri(ptr, cstr.as_ptr()) };
        }
        // Return focus to webview after navigation
        crate::util::defer_focus(&wv_nav);
    });

    // ── Find bar wiring ────────────────────────────────────────────
    // Search on text change
    let wv_search = webview_widget.clone();
    find_entry.connect_changed(move |entry| {
        let text = entry.text().to_string();
        let ptr = wv_search.as_ptr() as *mut WebKitWebView;
        let fc = unsafe { webkit_web_view_get_find_controller(ptr) };
        if text.is_empty() {
            unsafe { webkit_find_controller_search_finish(fc) };
        } else if let Ok(cstr) = CString::new(text.as_str()) {
            let opts = WEBKIT_FIND_OPTIONS_CASE_INSENSITIVE | WEBKIT_FIND_OPTIONS_WRAP_AROUND;
            unsafe { webkit_find_controller_search(fc, cstr.as_ptr(), opts, 1000) };
        }
    });

    // Enter → next match
    let wv_next = webview_widget.clone();
    find_entry.connect_activate(move |_| {
        let ptr = wv_next.as_ptr() as *mut WebKitWebView;
        let fc = unsafe { webkit_web_view_get_find_controller(ptr) };
        unsafe { webkit_find_controller_search_next(fc) };
    });

    // Find bar buttons
    let wv_prev_btn = webview_widget.clone();
    find_prev_btn.connect_clicked(move |_| {
        let ptr = wv_prev_btn.as_ptr() as *mut WebKitWebView;
        let fc = unsafe { webkit_web_view_get_find_controller(ptr) };
        unsafe { webkit_find_controller_search_previous(fc) };
    });

    let wv_next_btn = webview_widget.clone();
    find_next_btn.connect_clicked(move |_| {
        let ptr = wv_next_btn.as_ptr() as *mut WebKitWebView;
        let fc = unsafe { webkit_web_view_get_find_controller(ptr) };
        unsafe { webkit_find_controller_search_next(fc) };
    });

    // Close find bar
    let find_bar_close = find_bar.clone();
    let wv_close_find = webview_widget.clone();
    find_close_btn.connect_clicked(move |_| {
        let ptr = wv_close_find.as_ptr() as *mut WebKitWebView;
        let fc = unsafe { webkit_web_view_get_find_controller(ptr) };
        unsafe { webkit_find_controller_search_finish(fc) };
        find_bar_close.set_visible(false);
        // Return focus to webview
        crate::util::defer_focus(&wv_close_find);
    });

    // Escape in find entry closes find bar
    let find_bar_esc = find_bar.clone();
    let wv_esc = webview_widget.clone();
    let key_ctrl = gtk4::EventControllerKey::new();
    key_ctrl.connect_key_pressed(move |_, key, _, _| {
        if key == gdk4::Key::Escape {
            let ptr = wv_esc.as_ptr() as *mut WebKitWebView;
            let fc = unsafe { webkit_web_view_get_find_controller(ptr) };
            unsafe { webkit_find_controller_search_finish(fc) };
            find_bar_esc.set_visible(false);
            crate::util::defer_focus(&wv_esc);
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    find_entry.add_controller(key_ctrl);

    // ── FindController signals: match count ─────────────────────────
    // Connect "found-text" and "failed-to-find-text" signals on the find controller
    let fc_ptr = unsafe { webkit_web_view_get_find_controller(wv_ptr) };
    if !fc_ptr.is_null() {
        let fc_obj: glib::Object =
            unsafe { glib::Object::from_glib_none(fc_ptr as *mut glib::gobject_ffi::GObject) };
        let count_label = find_count_label.clone();
        fc_obj.connect_local("found-text", false, move |args| {
            if let Some(count) = args.get(1).and_then(|v| v.get::<u32>().ok()) {
                count_label.set_text(&format!("{count} matches"));
            }
            None
        });
        let count_label2 = find_count_label.clone();
        fc_obj.connect_local("failed-to-find-text", false, move |_| {
            count_label2.set_text("No matches");
            None
        });
    }

    // ── Register in browser registry ───────────────────────────────
    BROWSERS.with(|b| {
        b.borrow_mut().insert(id, BrowserEntry {
            webview_ptr: wv_ptr,
            widget: webview_widget.clone(),
            address_bar: address_bar.clone(),
        });
    });

    // Store browser ID on the container for lookup
    unsafe { container.set_data("browser-panel-id", id) };

    container
}

// ── Public API for controlling browser panels ──────────────────────────

/// Navigate a browser panel to a URL.
pub fn navigate(id: BrowserPanelId, url: &str) {
    let url = normalize_url(url);
    with_browser(id, |entry| {
        if let Ok(cstr) = CString::new(url.as_str()) {
            unsafe { webkit_web_view_load_uri(entry.webview_ptr, cstr.as_ptr()) };
        }
    });
}

/// Execute an action on a browser's WebView by ID.
fn browser_action(id: BrowserPanelId, f: unsafe extern "C" fn(*mut WebKitWebView)) {
    with_browser(id, |entry| unsafe { f(entry.webview_ptr) });
}

/// Go back in browser history.
pub fn go_back(id: BrowserPanelId) { browser_action(id, webkit_web_view_go_back); }

/// Go forward in browser history.
pub fn go_forward(id: BrowserPanelId) { browser_action(id, webkit_web_view_go_forward); }

/// Reload the current page.
pub fn reload(id: BrowserPanelId) { browser_action(id, webkit_web_view_reload); }

/// Get the current URL.
pub fn get_url(id: BrowserPanelId) -> Option<String> {
    with_browser(id, |entry| {
        unsafe { crate::util::cstr_to_string(webkit_web_view_get_uri(entry.webview_ptr)) }
    })?
}

/// Get the current title.
pub fn get_title(id: BrowserPanelId) -> Option<String> {
    with_browser(id, |entry| {
        unsafe { crate::util::cstr_to_string(webkit_web_view_get_title(entry.webview_ptr)) }
    })?
}

/// Evaluate JavaScript in the browser panel (fire-and-forget).
pub fn evaluate_js(id: BrowserPanelId, script: &str) {
    let Ok(cstr) = CString::new(script) else { return };
    with_browser(id, |entry| {
        unsafe {
            webkit_web_view_evaluate_javascript(
                entry.webview_ptr,
                cstr.as_ptr(),
                -1, // auto-detect length
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                None,
                std::ptr::null_mut(),
            );
        }
    });
}

/// Show or hide the find bar for a browser panel.
pub fn toggle_find_bar(id: BrowserPanelId) {
    with_browser(id, |entry| {
        let Some(container) = entry.widget.parent() else { return };
        let Some(find_bar) = crate::util::find_widget_by_name(&container, "find-bar") else { return };

        if find_bar.is_visible() {
            // Hide and clear search highlights
            find_bar.set_visible(false);
            let fc = unsafe { webkit_web_view_get_find_controller(entry.webview_ptr) };
            unsafe { webkit_find_controller_search_finish(fc) };
            crate::util::defer_focus(&entry.widget);
        } else {
            find_bar.set_visible(true);
            if let Some(find_entry) = find_bar.first_child() {
                find_entry.grab_focus();
            }
        }
    });
}

/// Focus the address bar of a browser panel.
pub fn focus_address_bar(id: BrowserPanelId) {
    with_browser(id, |entry| {
        entry.address_bar.grab_focus();
        entry.address_bar.select_region(0, -1);
    });
}

/// Focus the webview of a browser panel.
pub fn focus_webview(id: BrowserPanelId) {
    with_browser(id, |entry| { entry.widget.grab_focus(); });
}

/// Unregister a browser panel (when closing).
pub fn unregister(id: BrowserPanelId) {
    BROWSERS.with(|b| { b.borrow_mut().remove(&id); });
}

/// Get the WebView widget for a browser panel (for embedding in GtkStack).
pub fn get_widget(id: BrowserPanelId) -> Option<gtk4::Widget> {
    with_browser(id, |e| e.widget.clone())
}

/// Update the network proxy for an existing browser panel.
/// When `endpoint` is Some, the browser's network session is configured with a
/// SOCKS5 proxy pointing at the tunnel. When None, the proxy is cleared.
pub fn set_proxy_endpoint(
    id: BrowserPanelId,
    endpoint: Option<&crate::remote::ProxyEndpoint>,
) {
    with_browser(id, |entry| {
        let session = unsafe { webkit_web_view_get_network_session(entry.webview_ptr) };
        if session.is_null() {
            return;
        }
        match endpoint {
            Some(ep) => {
                let proxy_uri = ep.socks5_uri();
                apply_socks5_proxy(session, &proxy_uri);
            }
            None => {
                clear_proxy(session);
            }
        }
    });
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Apply a SOCKS5 proxy to a WebKitNetworkSession.
fn apply_socks5_proxy(session: *mut WebKitNetworkSession, proxy_uri: &str) {
    if let Ok(uri_c) = CString::new(proxy_uri) {
        unsafe {
            let settings =
                webkit_network_proxy_settings_new(uri_c.as_ptr(), std::ptr::null());
            webkit_network_session_set_proxy_settings(
                session,
                WEBKIT_NETWORK_PROXY_MODE_CUSTOM,
                settings,
            );
        }
    }
}

/// Clear proxy settings on a WebKitNetworkSession (revert to no proxy).
fn clear_proxy(session: *mut WebKitNetworkSession) {
    unsafe {
        webkit_network_session_set_proxy_settings(
            session,
            WEBKIT_NETWORK_PROXY_MODE_NO_PROXY,
            std::ptr::null_mut(),
        );
    }
}

/// Normalize user input into a URL. If it looks like a URL, ensure it has
/// a scheme. Otherwise, treat it as a search query.
fn normalize_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("file://")
    {
        return trimmed.to_string();
    }
    // Looks like a domain (has a dot, no spaces)
    if trimmed.contains('.') && !trimmed.contains(' ') {
        return format!("https://{trimmed}");
    }
    // Treat as search
    format!(
        "https://www.google.com/search?q={}",
        trimmed.replace(' ', "+")
    )
}
