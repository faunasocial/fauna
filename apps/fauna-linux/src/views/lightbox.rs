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
    show_lightbox_with(parent, title, &gtk::Picture::for_paintable(paintable));
}

/// The shared modal-window body both entry points fill with their own
/// `gtk::Picture`.
fn show_lightbox_with(parent: Option<&gtk::Window>, title: &str, picture: &gtk::Picture) {
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

    picture.set_can_shrink(true);
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.set_hexpand(true);
    picture.set_vexpand(true);

    outer.append(picture);
    window.set_content(Some(&outer));
    window.present();
}
