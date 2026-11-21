// apps/fauna-linux/src/clipboard.rs

use adw::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use std::rc::Rc;
use std::time::Duration;

use crate::i18n::strings;

/// Copy text to the system clipboard via GTK4 native API.
///
/// On X11, content persists after app exit. On Wayland, content lives
/// as long as the process (protocol limitation).
pub fn copy_text(text: &str) {
    if let Some(display) = gdk::Display::default() {
        display.clipboard().set_text(text);
    }
}

/// Set up Ctrl+V image paste on a `gtk::TextView`.
///
/// When the user presses Ctrl+V and the clipboard contains an image texture,
/// saves it as a PNG temp file and calls `on_image` with the path.
/// Text paste is unaffected — GTK handles it via the default handler.
///
/// No metadata strip happens here: the PNG is re-encoded by GDK from a raw
/// clipboard texture, so it carries no EXIF to begin with, and the upload path
/// strips losslessly regardless (`stage_attachment` → `upload_public_post_blob`
/// → `fauna_media::pipeline::process_and_seal` → `process_media`).
pub fn setup_image_paste(text_view: &gtk::TextView, on_image: impl Fn(String) + 'static) {
    let on_image = Rc::new(on_image);
    let key_ctrl = gtk::EventControllerKey::new();
    let on_image_ref = Rc::clone(&on_image);
    key_ctrl.connect_key_pressed(move |_, key, _, modifiers| {
        if key == gdk::Key::v
            && modifiers.contains(gdk::ModifierType::CONTROL_MASK)
            && let Some(display) = gdk::Display::default()
        {
            let clipboard = display.clipboard();
            let on_img = Rc::clone(&on_image_ref);
            clipboard.read_texture_async(gio::Cancellable::NONE, move |result| {
                if let Ok(Some(texture)) = result {
                    let filename = format!("fauna-paste-{}.png", uuid::Uuid::new_v4());
                    let path = std::env::temp_dir().join(&filename);
                    let path_str = path.display().to_string();
                    match texture.save_to_png(&path_str) {
                        Ok(()) => {
                            on_img(path_str);
                        }
                        Err(e) => {
                            tracing::error!("save_to_png failed: {e}");
                        }
                    }
                }
            });
        }
        // Always proceed — let GTK handle text paste normally
        glib::Propagation::Proceed
    });
    text_view.add_controller(key_ctrl);
}

/// Returns a small `gtk::Button` that copies `text` to the clipboard on click.
///
/// Shows "Copied!" feedback for 2 seconds, then reverts to the original label.
pub fn copy_button(text: impl Into<String>) -> gtk::Button {
    let text = text.into();
    copy_button_dynamic(move || text.clone())
}

/// Like [`copy_button`], but reads the text to copy at click time via
/// `get_text` instead of capturing a fixed string — for a button copying a
/// value that changes after the button is built (e.g. a preview field that
/// updates with another widget's selection).
pub fn copy_button_dynamic(get_text: impl Fn() -> String + 'static) -> gtk::Button {
    let btn = gtk::Button::with_label(strings::p2p::COPY_TO_CLIPBOARD);
    btn.add_css_class("flat");
    btn.connect_clicked(move |btn| copy_and_confirm(btn, &get_text()));
    btn
}

/// The copy-button funnel: put `text` on the clipboard, then confirm it on
/// `btn` — "Copied to clipboard" for two seconds, back to "Copy to clipboard".
///
/// The confirmation is a displayed success line, so it also logs at `info`
/// (`observability.md` § What must be logged, category 1 — the `show_toast`
/// posture). The log carries the displayed text only, never `text` itself
/// (§ 2's redaction rule: a copied value may be a secret). The button's
/// `copied` attr reports the exact string that reached the clipboard — the
/// copy-button contract a test reads, since no driver reads the OS clipboard.
pub fn copy_and_confirm(btn: &gtk::Button, text: &str) {
    copy_text(text);
    confirm_copied(btn, text, strings::p2p::COPY_TO_CLIPBOARD);
}

/// The confirmation half of [`copy_and_confirm`], for a copy whose write has
/// already succeeded: the `copied` attr, the logged and displayed "Copied to
/// clipboard" for two seconds, then back to `idle_label` (the button's own
/// resting label — a page's "Copy" button returns to "Copy").
pub fn confirm_copied(btn: &gtk::Button, text: &str, idle_label: &str) {
    // Free text, not a CSS-class token: a copied value carries spaces and
    // semicolons (a DNS record's `v=spf1 …`).
    crate::testid::set_test_attr_text(btn, "copied", text);
    tracing::info!("{}", strings::settings::account_page::COPIED_CLIPBOARD);
    btn.set_label(strings::settings::account_page::COPIED_CLIPBOARD);
    let btn_weak = btn.downgrade();
    let idle_label = idle_label.to_string();
    glib::timeout_add_local_once(Duration::from_secs(2), move || {
        if let Some(btn) = btn_weak.upgrade() {
            btn.set_label(&idle_label);
        }
    });
}

/// Put `text` on the default display's clipboard, or say why it could not be:
/// the one failure GTK reports here is having no display to own a clipboard
/// (the write itself is fire-and-forget once a display exists).
pub fn try_copy_text(text: &str) -> Result<(), String> {
    match gdk::Display::default() {
        Some(display) => {
            display.clipboard().set_text(text);
            Ok(())
        }
        None => Err("no display".to_string()),
    }
}

/// The `common.copy_failed` sentence for a clipboard write that failed — the
/// twin of tui's `wizard::copy_failed_text`.
pub fn copy_failed_text(error: &str) -> String {
    strings::common::COPY_FAILED.replace("{error}", error)
}

/// Whether `text` is a line [`record_copy_outcome`] wrote — a success clears
/// only its own earlier failure, never a read error the page shows.
fn is_copy_failure_line(text: &str) -> bool {
    let prefix = strings::common::COPY_FAILED
        .split("{error}")
        .next()
        .unwrap_or_default();
    !prefix.is_empty() && text.starts_with(prefix)
}

/// Record a page copy's outcome (`docs/goal/ui/README.md` § Offer only what is
/// possible, rule 7): a write that succeeded is confirmed on `btn`
/// ([`confirm_copied`]) and clears an earlier copy failure from `error_line`;
/// one that failed drops the `copied` attr and puts `common.copy_failed` on
/// `error_line`. The twin of tui's `settings::logs::record_copy_outcome`.
pub fn record_copy_outcome(
    btn: &gtk::Button,
    error_line: &gtk::Label,
    idle_label: &str,
    text: &str,
    outcome: Result<(), String>,
) {
    match outcome {
        Ok(()) => {
            if error_line.is_visible() && is_copy_failure_line(&error_line.text()) {
                error_line.set_text("");
                error_line.set_visible(false);
            }
            confirm_copied(btn, text, idle_label);
        }
        Err(e) => {
            crate::testid::set_test_attr_text(btn, "copied", "");
            error_line.set_text(&copy_failed_text(&e));
            error_line.set_visible(true);
        }
    }
}
