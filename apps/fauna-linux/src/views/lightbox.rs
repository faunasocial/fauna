//! Image lightbox — modal window for viewing image attachments.

use adw::prelude::*;
use fauna_ui_ids as ids;

/// Show an already-loaded image in the modal lightbox.
///
/// The feed's `post-image` fetches its blob over the bulk plane and holds the
/// decoded `gdk::Texture` — it has no file on disk, and inventing one just to
/// hand this a path is what the old click handler did (it wrote the blob to a
/// hard-coded `/work/tmp/fauna-blob-…`, a dev-machine path in production UI,
/// and then opened nothing at all).
pub fn show_image_lightbox_paintable(
    parent: Option<&gtk::Window>,
    paintable: &impl IsA<gtk::gdk::Paintable>,
    title: &str,
) {
    let picture = gtk::Picture::for_paintable(paintable);
    picture.set_can_shrink(true);
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    show_lightbox_with(parent, title, &picture);
}

/// Show the lightbox for a picture whose bytes this device does not hold: the
/// same window, its body the picture's DECLARED text (`{filename} ({size})`) in
/// place of the paint — tui's and windows' placeholder form, so a click on the
/// placeholder is never dropped (`conversations.md` § User actions, the
/// `dm-attachment-image[i]` row).
pub fn show_image_lightbox_placeholder(parent: Option<&gtk::Window>, declared: &str, title: &str) {
    let label = gtk::Label::new(Some(declared));
    label.add_css_class("dim-label");
    label.set_wrap(true);
    label.set_hexpand(true);
    label.set_vexpand(true);
    label.set_halign(gtk::Align::Center);
    label.set_valign(gtk::Align::Center);
    show_lightbox_with(parent, title, &label);
}

/// The shared modal-window body every entry point fills with its own content —
/// the decoded picture, or the placeholder's declared text.
fn show_lightbox_with(parent: Option<&gtk::Window>, title: &str, content: &impl IsA<gtk::Widget>) {
    let window = adw::Window::builder()
        .title(title)
        .modal(true)
        .default_width(800)
        .default_height(600)
        .build();

    if let Some(p) = parent {
        window.set_transient_for(Some(p));
        // Register with the application so sign-out's close-all-windows
        // sweep catches this lightbox if the user was viewing an image
        // when they signed out.
        if let Some(app) = p.application() {
            window.set_application(Some(&app));
        }
    }

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    crate::testid::set_test_id(&outer, ids::IMAGE_LIGHTBOX);
    let header = adw::HeaderBar::new();
    outer.append(&header);

    outer.append(content);
    window.set_content(Some(&outer));

    // Escape closes the lightbox — the keyboard's way out on every app (web's
    // `Lightbox.svelte`, tui's `lightbox.rs`); ui.yaml declares no dismiss element.
    let w = window.clone();
    let escape = gtk::ShortcutController::new();
    escape.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("Escape"),
        Some(gtk::CallbackAction::new(move |_, _| {
            w.close();
            gtk::glib::Propagation::Stop
        })),
    ));
    window.add_controller(escape);
    window.present();
}
