//! First-run prompt: ask the user whether to integrate with the desktop.
//!
//! Ideally this would use `gtk4::AlertDialog` with an extra child widget
//! ("Don't ask again" checkbox), but `set_extra_child` was added in GTK
//! 4.16 and `gtk4-rs 0.9` wraps GTK 4.12.  We fall back to a custom
//! modal `gtk4::Window` transient for the parent window.

use gtk4::prelude::*;
use gtk4;

use crate::install;
use crate::settings;

/// Show the first-run integration prompt as a modal dialog on the given
/// window.  Returns immediately; the user response is handled
/// asynchronously.
///
/// The prompt is shown only when ALL of these hold:
///   - Running from an AppImage (`install::is_appimage()`)
///   - Not already integrated (`install::is_integrated()`)
///   - User has not opted out (`settings.get().hide_integrate_prompt()`)
///
/// Otherwise this is a no-op.
pub fn maybe_show(parent: &gtk4::ApplicationWindow) {
    // ── Gating checks ───────────────────────────────────────────────
    if !install::is_appimage() {
        return;
    }
    if install::is_integrated() {
        return;
    }
    if settings::get().hide_integrate_prompt() {
        return;
    }

    // ── Build the dialog window ─────────────────────────────────────
    // (AlertDialog::set_extra_child is unavailable before GTK 4.16, so
    //  we build a custom modal Window instead.)

    let dialog = gtk4::Window::builder()
        .title("Integrate limux with your desktop?")
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .build();

    // Layout container
    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    vbox.set_margin_top(20);
    vbox.set_margin_bottom(20);
    vbox.set_margin_start(20);
    vbox.set_margin_end(20);

    // ── Message ─────────────────────────────────────────────────────
    let message_label = gtk4::Label::new(Some("Integrate limux with your desktop?"));
    message_label.set_halign(gtk4::Align::Start);
    message_label.add_css_class("heading");
    vbox.append(&message_label);

    // ── Detail / body ───────────────────────────────────────────────
    let detail_label = gtk4::Label::new(Some(
        "Integrating adds limux to your app menu, registers a .desktop \
         file, and enables desktop notifications and file associations.",
    ));
    detail_label.set_wrap(true);
    detail_label.set_halign(gtk4::Align::Start);
    detail_label.set_max_width_chars(60);
    vbox.append(&detail_label);

    // ── Spacer ──────────────────────────────────────────────────────
    vbox.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));

    // ── "Don't ask again" checkbox ──────────────────────────────────
    let check = gtk4::CheckButton::with_label("Don't ask again");
    vbox.append(&check);

    // ── Button row ──────────────────────────────────────────────────
    let button_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    button_box.set_halign(gtk4::Align::End);

    let cancel_btn = gtk4::Button::with_label("Cancel");
    let integrate_btn = gtk4::Button::with_label("Integrate");
    integrate_btn.add_css_class("suggested-action");

    button_box.append(&cancel_btn);
    button_box.append(&integrate_btn);
    vbox.append(&button_box);

    dialog.set_child(Some(&vbox));

    // ── Wire up Cancel ──────────────────────────────────────────────
    {
        let dialog = dialog.clone();
        let check = check.clone();
        cancel_btn.connect_clicked(move |_| {
            if check.is_active() {
                settings::update(|s| s.general.hide_integrate_prompt = Some(true));
            }
            dialog.close();
        });
    }

    // ── Wire up Integrate ───────────────────────────────────────────
    let dialog_integrate = dialog.clone();
    integrate_btn.connect_clicked(move |_| {
        let hide = check.is_active();
        match install::perform_integration() {
            Ok(()) => {
                settings::update(|s| {
                    s.general.desktop_integrated = Some(true);
                    if hide {
                        s.general.hide_integrate_prompt = Some(true);
                    }
                });
            }
            Err(e) => {
                eprintln!("Desktop integration failed: {e}");
            }
        }
        dialog_integrate.close();
    });

    dialog.present();
}
