//! The standalone **Moderation** page (`moderation-tab` → `moderation` stack
//! child) — the user's view onto why their *own* content was labeled /
//! quarantined / rejected, and their lever to correct it.
//!
//! Presentation over the **union** of two sources (`moderation.md` § Layout &
//! flow): the server `fauna.moderation.actions` obligation rows (`ObligationAction`
//! — the enforcement half: mail-ingest / admin quarantine·reject·label + appeals,
//! plus plaintext-mode social labels) and the client's own post-decrypt **local
//! detections** (the encrypted-mode social-content signal the nest can't produce).
//! The two merge + dedupe through the shared `fauna_client_moderation::merge_queue`
//! into one `moderation-queue`; each [`QueueRow`] carries a `content-label-badge`
//! (the category, via the shared `fauna_core::content_category::content_label_style`
//! map — icon + accent colour + i18n label, drift #157), an *optional* enforcement
//! action (present on server rows, **blank** on local detections), confidence, and
//! a `train-correction-button` (a server row submits `fauna.moderation.train`; a
//! local row removes the client-side false-positive flag).
//!
//! Unified shape: a standalone page on standalone clients, embedded in Settings
//! on web, the **same IDs** either way (`docs/goal/behavior/moderation.md`
//! § Architectural rules 1; mirrors Windows' standalone Moderation page). The
//! spam *preferences* (`spam-moderation-controls`) live on the Settings page,
//! not here — the queue consumes those preferences but does not host them.

use adw::prelude::*;
use fauna_client_moderation::{QueueRow, QueueRowSource};
use fauna_ui_ids as ids;
use std::rc::Rc;

use crate::client::FaunaClient;
use crate::i18n::strings::moderation as mod_strings;
use crate::i18n::strings::settings::moderation_page as mod_page_strings;

/// A `train-correction-button` handler: `(content_id, is_local)`.
type OnCorrect = Rc<dyn Fn(&str, bool)>;

/// Handles the message loop needs to repaint the queue on
/// `DataMessage::ModerationActionsLoaded`.
pub struct ModerationViewHandles {
    pub queue_list_box: gtk::ListBox,
}

/// Build the Moderation page: a header + the scrolled `moderation-queue` list.
/// The queue starts empty; rows are painted by [`update_moderation_queue`] on
/// `fauna.moderation.actions` delivery (so no `FaunaClient` is needed here).
pub fn build_moderation_view() -> (gtk::Box, ModerationViewHandles) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let header = adw::HeaderBar::new();
    let title_label = gtk::Label::new(Some(mod_page_strings::TITLE));
    crate::testid::set_test_id(&title_label, ids::PAGE_HEADING);
    header.set_title_widget(Some(&title_label));
    outer.append(&header);

    // Section caption above the queue (mirrors Windows' "Enforcement Actions").
    let section = gtk::Label::new(Some(mod_strings::ENFORCEMENT_TITLE));
    section.set_halign(gtk::Align::Start);
    section.add_css_class("title-4");
    section.set_margin_top(12);
    section.set_margin_start(12);
    section.set_margin_bottom(6);
    outer.append(&section);

    let queue_list_box = gtk::ListBox::new();
    queue_list_box.set_selection_mode(gtk::SelectionMode::None);
    queue_list_box.add_css_class("boxed-list");
    queue_list_box.set_margin_start(12);
    queue_list_box.set_margin_end(12);
    // The `moderation-queue` component container (scope anchor for e2e).
    crate::testid::set_test_id(&queue_list_box, ids::MODERATION_QUEUE);

    // Empty state: `actions: []` is not an error — render the empty placeholder
    // (moderation.md § Errors & edge cases).
    let placeholder = adw::StatusPage::builder()
        .title(mod_strings::NO_ACTIONS)
        .icon_name("security-high-symbolic")
        .build();
    queue_list_box.set_placeholder(Some(&placeholder));

    let scrolled = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&queue_list_box)
        .build();
    outer.append(&scrolled);

    let handles = ModerationViewHandles {
        queue_list_box: queue_list_box.clone(),
    };
    (outer, handles)
}

/// Repaint the queue from a freshly-merged [`QueueRow`] list (the server obligation
/// rows ∪ the client's local detections). Clears the list and rebuilds one row per
/// entry (or shows the empty placeholder).
pub fn update_moderation_queue(
    list_box: &gtk::ListBox,
    rows: &[QueueRow],
    client: &Rc<FaunaClient>,
) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
    // `train-correction-button` applies a correction — a server row submits a `ham`
    // (not-spam) `fauna.moderation.train`, a local row removes the false-positive
    // flag from the session store (see `FaunaClient::correct_moderation_row`). The
    // closure is shared across rows; each row binds its own `content_id` + whether
    // it is a local detection.
    let on_correct: OnCorrect = {
        let client = Rc::clone(client);
        Rc::new(move |content_id: &str, is_local: bool| {
            client.correct_moderation_row(content_id, is_local)
        })
    };
    for row in rows {
        list_box.append(&build_queue_row(row, Rc::clone(&on_correct)));
    }
}

/// Build the shared `content-label-badge` widget for `category`: icon + accent
/// colour + i18n label, entirely through `fauna_core::content_category::content_label_style`
/// so no client hard-codes the category→style map (moderation.md § Where logic
/// lives, drift #157). The **one** place linux paints this badge — the
/// moderation queue, feed post-cards, and DM bubbles all call it, so none of
/// them can drift from each other. The category text carries no per-page ID
/// in the spec (moderation.md § Element IDs), but the testid rides on the
/// text label so e2e reads the clean category.
pub(crate) fn build_content_label_badge(category: &str) -> gtk::Box {
    let style = fauna_core::content_category::content_label_style(category);
    let badge_box = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    badge_box.add_css_class(&format!("content-label-{category}"));

    let icon = gtk::Label::new(Some(&style.icon));
    icon.add_css_class("caption");
    badge_box.append(&icon);

    let badge_text = style.label.resolve(crate::i18n::strings::lookup);
    let badge = gtk::Label::new(None);
    // Pango markup paints the shared accent colour inline (no per-widget
    // CssProvider, no hard-coded linux colour) while `Label::text()` still
    // returns the bare label for e2e / the widget test.
    badge.set_markup(&format!(
        "<span foreground=\"{}\">{}</span>",
        style.accent,
        gtk::glib::markup_escape_text(&badge_text)
    ));
    badge.add_css_class("caption-heading");
    crate::testid::set_test_id(&badge, ids::CONTENT_LABEL_BADGE);
    badge_box.append(&badge);
    badge_box
}

/// One `moderation-queue` row: category badge, an *optional* action, confidence,
/// and a truncated content ref, with a `train-correction-button`. `on_correct` is
/// invoked with the row's `content_id` + whether it is a local detection when the
/// button is clicked — kept as a callback (not the `FaunaClient`) so the renderer is
/// unit-testable without a live client (the `render_mode_fields` idiom). A server
/// row renders its enforcement action; a **local** detection's action column is
/// blank (`moderation.md` § Layout & flow — "a local-only row shows a blank action
/// column"; never fabricate an action, § Don't do these).
fn build_queue_row(row: &QueueRow, on_correct: OnCorrect) -> gtk::ListBoxRow {
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 4);
    vbox.set_hexpand(true);

    // Top line: the content-label badge + the action taken.
    let top_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    // `content-label-badge` — see `build_content_label_badge`'s doc comment.
    top_line.append(&build_content_label_badge(&row.category));

    // The enforcement action taken, via the shared discriminant→label map
    // (co-located with the enum in `fauna_core::obligation`). Server rows only —
    // a local detection carries no `action`, so its action column stays blank.
    if let Some(action) = row.action {
        let action_text = fauna_core::obligation::obligation_action_label(action)
            .resolve(crate::i18n::strings::lookup);
        let action_label = gtk::Label::new(Some(&action_text));
        action_label.add_css_class("caption");
        action_label.add_css_class("dim-label");
        top_line.append(&action_label);
    }

    vbox.append(&top_line);

    // Content ref (type + truncated id) — an opaque handle, shown so the user
    // can correlate the row with their content.
    let id_short = fauna_core::format::short_id(&row.content_id);
    let ref_label = gtk::Label::new(Some(&format!("{} · {}", row.content_type, id_short)));
    ref_label.set_halign(gtk::Align::Start);
    ref_label.add_css_class("caption");
    ref_label.add_css_class("dim-label");
    ref_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    vbox.append(&ref_label);

    // Confidence as a whole-percent via the shared half-up rounding contract
    // (`fauna_core::format::confidence_percent`); the wire forbids floats, so
    // `confidence_per_mille` is 0..=1000.
    let pct = fauna_core::format::confidence_percent(row.confidence_per_mille);
    let conf_label = gtk::Label::new(Some(&format!("{pct}% {}", mod_strings::CONFIDENCE)));
    conf_label.set_halign(gtk::Align::Start);
    conf_label.add_css_class("caption");
    conf_label.add_css_class("dim-label");
    vbox.append(&conf_label);

    hbox.append(&vbox);

    // `train-correction-button` — a single correction: the queue lists items the
    // classifier *flagged*, so the correction is "this was a false positive →
    // not spam" (`verdict: "ham"`), training the caller's Bayesian model
    // (moderation.md § User actions). Mirrors Windows' single "Correct" button.
    let correct_btn = gtk::Button::with_label(mod_strings::CORRECT);
    correct_btn.add_css_class("flat");
    correct_btn.set_valign(gtk::Align::Center);
    crate::testid::set_test_id(&correct_btn, ids::TRAIN_CORRECTION_BUTTON);
    {
        let content_id = row.content_id.clone();
        let is_local = matches!(row.source, QueueRowSource::Local);
        correct_btn.connect_clicked(move |_| on_correct(&content_id, is_local));
    }
    hbox.append(&correct_btn);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&hbox));
    row.set_selectable(false);
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sample [`QueueRow`]. `action = Some(_)` is a server obligation row;
    /// `action = None` is a client-side local detection (blank action column).
    fn sample(category: &str, action: Option<u8>, conf: u16) -> QueueRow {
        QueueRow {
            content_id: "ab".repeat(32),
            content_type: "post".into(),
            category: category.into(),
            confidence_per_mille: conf,
            action,
            timestamp: 1_700_000_000_000_000,
            source: if action.is_some() {
                QueueRowSource::Server
            } else {
                QueueRowSource::Local
            },
        }
    }

    #[test]
    fn sample_has_canonical_category() {
        // Guards the test helper against an off-list category slipping in.
        let a = sample("spam", Some(2), 880);
        assert_eq!(
            fauna_core::content_category::content_label_style(&a.category)
                .label
                .key,
            "moderation.category.spam"
        );
    }

    /// Walk a widget subtree collecting `(test_id, label_text)` for every
    /// `gtk::Label`/`gtk::Button` carrying a widget name.
    fn collect_ids(widget: &gtk::Widget, out: &mut Vec<(String, String)>) {
        let name = widget.widget_name().to_string();
        if !name.is_empty() {
            let text = widget
                .downcast_ref::<gtk::Label>()
                .map(|l| l.text().to_string())
                .or_else(|| {
                    widget
                        .downcast_ref::<gtk::Button>()
                        .map(|b| b.label().map(|s| s.to_string()).unwrap_or_default())
                })
                .unwrap_or_default();
            out.push((name, text));
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            collect_ids(&c, out);
            child = c.next_sibling();
        }
    }

    #[test]
    fn row_renders_badge_and_train_button() {
        crate::testid::run_on_gtk_thread(|| {
            let row = build_queue_row(&sample("phishing", Some(1), 925), Rc::new(|_, _| {}));
            let mut ids = Vec::new();
            collect_ids(row.upcast_ref::<gtk::Widget>(), &mut ids);

            // The badge resolves the canonical category label (not the raw `category`
            // wire value) through the shared map + i18n.
            let badge = ids
                .iter()
                .find(|(id, _)| id == "content-label-badge")
                .expect("content-label-badge present");
            assert_eq!(badge.1, "Phishing");

            // The single correction button carries the canonical id + the shared label.
            let btn = ids
                .iter()
                .find(|(id, _)| id == "train-correction-button")
                .expect("train-correction-button present");
            assert_eq!(btn.1, "Correct");
        });
    }

    /// Walk a widget subtree collecting the text of **every** `gtk::Label`
    /// (regardless of whether it carries a test id) — the action label is not
    /// test-id'd, so [`collect_ids`] can't see it.
    fn collect_all_label_texts(widget: &gtk::Widget, out: &mut Vec<String>) {
        if let Some(l) = widget.downcast_ref::<gtk::Label>() {
            out.push(l.text().to_string());
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            collect_all_label_texts(&c, out);
            child = c.next_sibling();
        }
    }

    /// F3 (legal-takedown badge verify; tracked internally § F3). A server obligation row for a
    /// **taken-down** post carries `category="illegal"` (the enforcement
    /// descriptor the nest writes at `moderation_handlers.rs` `post_legal_\
    /// takedown_txn`), deliberately OUTSIDE the canonical-5 classifier categories
    /// — the real signal is the `TakenDown` action label. The queue row must
    /// render **gracefully**: the off-list category degrades to the shared
    /// `content_label_style` `Other` fallback (grey badge, capitalized "Illegal"),
    /// never a panic and never a dropped row, and the `TakenDown` enforcement
    /// label "Removed under legal obligation" still renders alongside it.
    #[test]
    fn legal_takedown_row_renders_off_list_category_and_action_label() {
        crate::testid::run_on_gtk_thread(|| {
            // A server TakenDown obligation (action = 7 = ObligationAction::TakenDown)
            // carrying the off-list `illegal` enforcement descriptor at max confidence.
            let taken_down_action = fauna_core::obligation::ObligationAction::TakenDown as u8;
            let row = build_queue_row(
                &sample("illegal", Some(taken_down_action), 1000),
                Rc::new(|_, _| {}),
            );
            let mut ids = Vec::new();
            collect_ids(row.upcast_ref::<gtk::Widget>(), &mut ids);

            // The row is NOT dropped: its `content-label-badge` (and correction
            // button) rendered, and the badge degrades to the shared `Other` fallback
            // — the capitalized raw string, not a canonical-5 label and not a panic.
            let badge = ids
                .iter()
                .find(|(id, _)| id == "content-label-badge")
                .expect("content-label-badge present for an off-list category");
            assert_eq!(badge.1, "Illegal");
            assert!(ids.iter().any(|(id, _)| id == "train-correction-button"));

            // The `TakenDown` enforcement label renders alongside the badge (server
            // row → non-blank action column via the shared discriminant→label map).
            let taken_down_label =
                fauna_core::obligation::obligation_action_label(taken_down_action)
                    .resolve(crate::i18n::strings::lookup);
            assert_eq!(taken_down_label, "Removed under legal obligation");
            let mut texts = Vec::new();
            collect_all_label_texts(row.upcast_ref::<gtk::Widget>(), &mut texts);
            assert!(
                texts.iter().any(|t| t == &taken_down_label),
                "TakenDown action label {taken_down_label:?} must render in the row; got {texts:?}"
            );
        });
    }

    #[test]
    fn local_detection_row_has_blank_action_and_train_button() {
        crate::testid::run_on_gtk_thread(|| {
            // A local detection (action = None) still renders the badge + confidence +
            // train button, but its action column is blank — never a fabricated action
            // (`moderation.md` § Layout & flow / § Don't do these).
            let obligation_labels: Vec<String> = (0u8..=4)
                .map(|a| {
                    fauna_core::obligation::obligation_action_label(a)
                        .resolve(crate::i18n::strings::lookup)
                })
                .collect();
            let row = build_queue_row(&sample("spam", None, 720), Rc::new(|_, _| {}));
            let mut ids = Vec::new();
            collect_ids(row.upcast_ref::<gtk::Widget>(), &mut ids);

            // Badge + train button present, exactly as a server row.
            assert!(ids.iter().any(|(id, _)| id == "content-label-badge"));
            assert!(ids.iter().any(|(id, _)| id == "train-correction-button"));
            // No obligation-action label was rendered anywhere in the row (blank column).
            for (_, text) in &ids {
                assert!(
                    !obligation_labels.iter().any(|l| l == text),
                    "local row must not render an enforcement action; found {text:?}"
                );
            }
        });
    }
}
