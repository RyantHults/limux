mod app;
mod appearance;
mod browser;
mod clipboard;
mod dbus;
mod ghostty_sys;
mod input;
mod notify;
mod remote;
mod session;
mod settings;
mod settings_ui;
mod sidebar;
mod socket;
mod split;
mod surface;
mod surfaces;
mod tab_strip;
mod tray;
mod util;
mod window;
mod workspace;

use std::cell::RefCell;

use gtk4::prelude::*;
use gtk4::{self, gio, glib};

/// CLI options parsed from command line.
struct Options {
    working_directory: Option<String>,
    command: Option<String>,
    socket_path: Option<String>,
}

thread_local! {
    static WINDOW: RefCell<Option<gtk4::ApplicationWindow>> = const { RefCell::new(None) };
}

/// Called from action_cb to set the window title.
pub fn set_window_title(title: &str) {
    WINDOW.with(|w| {
        if let Some(window) = w.borrow().as_ref() {
            window.set_title(Some(title));
        }
    });
}

/// Called from close_surface_cb/action_cb to close the window.
pub fn close_window() {
    WINDOW.with(|w| {
        if let Some(window) = w.borrow().as_ref() {
            window.close();
        }
    });
}

fn main() {
    let app = gtk4::Application::new(
        Some("com.limux.terminal"),
        gio::ApplicationFlags::HANDLES_COMMAND_LINE,
    );

    // Register CLI options
    app.add_main_option(
        "working-directory",
        glib::Char::from(b'd'),
        glib::OptionFlags::NONE,
        glib::OptionArg::String,
        "Initial working directory",
        Some("DIR"),
    );
    app.add_main_option(
        "command",
        glib::Char::from(b'e'),
        glib::OptionFlags::NONE,
        glib::OptionArg::String,
        "Command to execute",
        Some("CMD"),
    );
    app.add_main_option(
        "socket",
        glib::Char::from(b's'),
        glib::OptionFlags::NONE,
        glib::OptionArg::String,
        "Socket path for CLI control",
        Some("PATH"),
    );

    // Parse options from command-line
    let options: std::rc::Rc<RefCell<Options>> = std::rc::Rc::new(RefCell::new(Options {
        working_directory: None,
        command: None,
        socket_path: None,
    }));

    let opts = options.clone();
    app.connect_command_line(move |app, cmdline| {
        let dict = cmdline.options_dict();
        {
            let mut o = opts.borrow_mut();
            o.working_directory = dict.lookup::<String>("working-directory").ok().flatten();
            o.command = dict.lookup::<String>("command").ok().flatten();
            o.socket_path = dict.lookup::<String>("socket").ok().flatten();
        } // drop borrow before activate
        app.activate();
        0
    });

    let opts = options.clone();
    app.connect_activate(move |app| {
        // Only activate once
        if app.active_window().is_some() {
            return;
        }

        // Initialize Ghostty backend
        if let Err(e) = crate::app::GhosttyApp::init() {
            eprintln!("Failed to initialize Ghostty: {e}");
            return;
        }

        let o = opts.borrow();

        // Load settings from disk
        settings::init();

        // Apply notification preference from settings
        if settings::get().notifications_enabled() {
            notify::enable();
        } else {
            notify::disable();
        }

        // Initialize color scheme detection before any CSS is loaded
        appearance::init();

        // Create the main window
        let gtk_window = gtk4::ApplicationWindow::builder()
            .application(app)
            .title("limux")
            .default_width(800)
            .default_height(600)
            .build();

        // Track window focus for notification suppression
        gtk_window.connect_is_active_notify(move |win| {
            notify::set_window_active(win.is_active());
            tray::update_window_visible(win.is_active());
        });

        // Register notification click action: switch to a workspace by ID
        let switch_action = gio::SimpleAction::new("switch-workspace", Some(glib::VariantTy::UINT32));
        let win_for_action = gtk_window.clone();
        switch_action.connect_activate(move |_, param| {
            if let Some(ws_id) = param.and_then(|p| p.get::<u32>()) {
                window::select_workspace_by_id(ws_id);
                win_for_action.present();
            }
        });
        app.add_action(&switch_action);

        // Start socket server before creating terminals so shells inherit LIMUX_SOCKET
        socket::start(o.socket_path.as_deref());

        // Initialize the window with tab bar and first terminal
        window::init(
            &gtk_window,
            o.working_directory.as_deref(),
            o.command.as_deref(),
        );

        gtk_window.present();

        WINDOW.with(|w| {
            *w.borrow_mut() = Some(gtk_window);
        });

        // Start system tray and D-Bus service
        tray::start();
        dbus::start();

        // Global render timer — queues renders for ALL surfaces every ~16ms.
        // This is more reliable than per-widget tick callbacks which get
        // removed during GTK unrealize/re-realize cycles (reparenting).
        glib::timeout_add_local(std::time::Duration::from_millis(16), || {
            surface::queue_render();
            glib::ControlFlow::Continue
        });

        // Autosave session every 30 seconds
        session::start_autosave();
    });

    app.connect_shutdown(|_app| {
        session::save_current_session();
        dbus::stop();
        tray::stop();
        socket::stop();
        crate::app::GhosttyApp::destroy();
    });

    let args: Vec<String> = std::env::args().collect();
    app.run_with_args(&args);
}
