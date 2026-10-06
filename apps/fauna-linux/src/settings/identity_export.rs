//! Identity export — the Settings/Account section that reveals the identity QR a second
//! device scans to import (`docs/goal/ui/settings.md` § Identity export). The counterpart
//! of onboarding's `identity_import` step.
//!
//! Both halves are shared Rust: `fauna_core::identity_qr::IdentityQr::to_uri` builds exactly
//! the URI the import parser accepts, and `fauna_core::qr_matrix::qr_matrix` turns it into a
//! boolean module grid. `crate::qr_widget` paints that grid with cairo (shared with the Nostr
//! Connect bunker-invite QR) — no client links a platform QR library (priorities #1/#2).

use adw::prelude::*;
use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;

use crate::i18n::strings::settings::identity_export as ie;
use fauna_core::qr_matrix::QrMatrix;

/// Side of the drawn QR, in px. The grid scales to fit; the quiet zone is inside this box.
const QR_SIZE_PX: i32 = 220;

/// Build the "Export Identity" preferences group.
///
/// The description and the toggle are always visible; the warning and the QR appear **only
/// after the user presses show**, and the button's label flips to `hide_qr`. Hiding is not a
/// security control — it exists so the secret is never on screen by accident when a user
/// opens Settings (e.g. while sharing a screen). Nothing is persisted, no server call is
/// made; the toggle is pure view state.
pub fn build_identity_export_group() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(ie::TITLE).build();
    crate::testid::set_test_id(&group, ids::IDENTITY_EXPORT_SECTION);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(8);

    // Always visible — explains what the QR is for, whether or not it is shown.
    let description = gtk::Label::builder().label(ie::DESC).wrap(true).build();
    description.set_halign(gtk::Align::Start);
    description.add_css_class("dim-label");
    crate::testid::set_test_id(&description, ids::IDENTITY_EXPORT_DESCRIPTION);
    content.append(&description);

    // Only while the QR is shown. The QR carries the full Ed25519 identity secret —
    // whoever scans it gains the identity — so the warning renders adjacent to the code,
    // never below the fold (settings.md § Identity export, "Risk").
    let warning = gtk::Label::builder()
        .label(ie::WARNING)
        .wrap(true)
        .visible(false)
        .build();
    warning.set_halign(gtk::Align::Start);
    warning.add_css_class("warning");
    crate::testid::set_test_id(&warning, ids::IDENTITY_EXPORT_WARNING);
    content.append(&warning);

    // The matrix the draw func paints. `None` until the user presses show — and the draw
    // func is a no-op then, so a failed encode simply paints nothing rather than panicking
    // across a UI callback.
    let matrix: Rc<RefCell<Option<QrMatrix>>> = Rc::new(RefCell::new(None));

    let qr_area = gtk::DrawingArea::builder()
        .content_width(QR_SIZE_PX)
        .content_height(QR_SIZE_PX)
        .visible(false)
        .build();
    qr_area.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&qr_area, ids::IDENTITY_EXPORT_QR);
    qr_area.set_draw_func({
        let matrix = matrix.clone();
        move |_area, cr, width, height| {
            let borrowed = matrix.borrow();
            let Some(m) = borrowed.as_ref() else { return };
            crate::qr_widget::draw_matrix(cr, m, width, height);
        }
    });
    content.append(&qr_area);

    let toggle = gtk::Button::builder()
        .label(ie::SHOW_QR)
        .halign(gtk::Align::Start)
        .build();
    crate::testid::set_test_id(&toggle, ids::IDENTITY_EXPORT_SHOW_QR_BUTTON);
    content.append(&toggle);

    toggle.connect_clicked({
        let warning = warning.clone();
        let qr_area = qr_area.clone();
        let matrix = matrix.clone();
        move |btn| {
            if qr_area.is_visible() {
                // Hide: drop the matrix so the secret's encoding isn't retained in memory
                // any longer than it is on screen.
                matrix.replace(None);
                qr_area.set_visible(false);
                warning.set_visible(false);
                btn.set_label(ie::SHOW_QR);
                return;
            }

            let Some(encoded) = encode_identity_qr() else {
                tracing::error!("[settings/identity_export] no secret available to export");
                return;
            };
            matrix.replace(Some(encoded));
            qr_area.queue_draw();
            qr_area.set_visible(true);
            warning.set_visible(true);
            btn.set_label(ie::HIDE_QR);
        }
    });

    group.add(&content);
    group
}

/// Read the identity secret from the platform secure store, pair it with the cached handle,
/// and encode the `(identity, handle)` URI as a QR matrix.
///
/// The secret comes from the secure store, never from a settings snapshot
/// (`architecture/apps/common.md` § Credential storage). A handle-less client writes the
/// bare secret form; a scanned import then just doesn't pre-fill the handle step.
fn encode_identity_qr() -> Option<QrMatrix> {
    let client = crate::settings::get_client()?;
    let handle = crate::client::load_account_cache().0;
    let uri = fauna_core::identity_qr::IdentityQr::to_uri(client.secret_hex(), handle.as_deref());
    match fauna_core::qr_matrix::qr_matrix(&uri) {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::error!("[settings/identity_export] QR encoding failed: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::{find_by_test_id, widget_names};

    /// The section exposes every ui.yaml `identity-export-*` ID, and — the behavior
    /// contract — the warning and the QR start **hidden**: they render only after the user
    /// presses show (settings.md § Identity export), while the description and the toggle
    /// are always visible. The e2e suite drives the toggle across clients; this pins the
    /// initial state here, where a regression is cheapest to catch.
    #[test]
    fn identity_export_group_exposes_ui_yaml_ids_and_starts_collapsed() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let group = build_identity_export_group();
            let names = widget_names(&group);
            for id in [
                "identity-export-section",
                "identity-export-description",
                "identity-export-show-qr-button",
                "identity-export-warning",
                "identity-export-qr",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing ui.yaml ID {id}; got {names:?}"
                );
            }

            for always_visible in [
                "identity-export-description",
                "identity-export-show-qr-button",
            ] {
                let w = find_by_test_id(&group, always_visible).expect(always_visible);
                assert!(w.is_visible(), "{always_visible} must always be visible");
            }
            // The secret must never be on screen by accident when a user opens Settings.
            for hidden_until_shown in ["identity-export-warning", "identity-export-qr"] {
                let w = find_by_test_id(&group, hidden_until_shown).expect(hidden_until_shown);
                assert!(
                    !w.is_visible(),
                    "{hidden_until_shown} must be hidden until the user presses show"
                );
            }
        });
    }

    /// The section encodes the same URI the import parser accepts — export → scan → import
    /// is closed end-to-end. Guards against the section growing its own payload format.
    #[test]
    fn encodes_the_importable_identity_uri() {
        const SECRET: &str = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
        let uri = fauna_core::identity_qr::IdentityQr::to_uri(SECRET, Some("alice@fauna.social"));
        assert!(fauna_core::qr_matrix::qr_matrix(&uri).is_ok());
        assert_eq!(
            fauna_core::identity_qr::parse_import_input(&uri)
                .map(|i| i.secret)
                .as_deref(),
            Some(SECRET)
        );
    }
}
