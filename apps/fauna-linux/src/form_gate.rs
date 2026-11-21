//! The form case of "offer only what is possible" on linux
//! (`docs/goal/ui/README.md` § Offer only what is possible, rules 4 and 7;
//! the page rulings are `ui/feed.md`, `ui/search.md` and `ui/nostr.md`
//! § Errors & edge cases).
//!
//! The *answer* is never linux's: each form's submit is painted from one
//! shared-Rust predicate (`fauna_feed::forms`, `fauna_client_search::
//! query_possibility`, `fauna_client_bridges::nostr_forms`,
//! `fauna_protocol::nostr_relay::add_relay_possibility`). What is linux's is
//! how a GTK control obeys it, in the app's established disabled-with-a-reason
//! idiom — the one `offline_gate` and `settings::mail_gate` use: absent → not
//! shown; blocked → insensitive with the reason on the tooltip; possible →
//! live. The control's sensitivity written here is the PAGE's intent, so the
//! offline gate still ANDs its own verdict on top.
//!
//! The submit asks the same predicate again ([`refuse`]) and puts the reason
//! on the surface's `error-message` — a modal dialog's own
//! ([`dialog_error_label`]) while it is open, else the page's (rule 7); a feed
//! form's dialog closes only once its save succeeded ([`finish_save`]).

use adw::prelude::*;
use fauna_core::possibility::Possibility;

/// The reason text of a blocked answer, through linux's i18n; `None` otherwise.
pub(crate) fn reason_text(answer: &Possibility) -> Option<String> {
    match answer {
        Possibility::Blocked { reason } => Some(reason.resolve(crate::i18n::strings::lookup)),
        _ => None,
    }
}

/// Paint a form's submit from its predicate's answer. `idle_tooltip` is the
/// tooltip the control carries while possible (an icon button's own name,
/// e.g. search's "Search"); `None` for a labelled button.
///
/// Visibility is left to the page (a submit may sit in a bar the person
/// collapsed): no draft predicate answers `Absent` — an absent SECTION is
/// hidden whole by its page ([`paint_section`]) — so an `Absent` here only
/// leaves the submit insensitive.
pub(crate) fn paint_submit(
    submit: &impl IsA<gtk::Widget>,
    answer: &Possibility,
    idle_tooltip: Option<&str>,
) {
    let reason = reason_text(answer);
    // Tooltip first, then sensitivity: the offline gate re-decides on the
    // sensitivity change and must see the page's tooltip already in place.
    crate::offline_gate::set_page_tooltip(submit, reason.as_deref().or(idle_tooltip));
    // What was just painted, read by the e2e driver's `blocked_reason`
    // (`reason`) — the same attr `possible::paint` publishes; empty while live.
    crate::testid::set_test_attr_text(submit, "reason", reason.as_deref().unwrap_or(""));
    submit.set_sensitive(answer.is_possible());
}

/// Show or hide a whole section from its answer: `Absent` → not shown at all
/// (rule 2), anything else → shown (a blocked section's controls carry their
/// own reasons).
pub(crate) fn paint_section(section: &impl IsA<gtk::Widget>, answer: &Possibility) {
    section.set_visible(answer.is_shown());
}

/// Write `text` on an `error-message` label (`None` hides it).
pub(crate) fn show_error(label: &gtk::Label, text: Option<&str>) {
    match text {
        Some(t) => {
            label.set_text(t);
            label.set_visible(true);
        }
        None => {
            label.set_text("");
            label.set_visible(false);
        }
    }
}

/// The submit's own ask (rule 6): `true` when the answer is not possible —
/// the caller issues nothing, and the reason (the one painted beside the
/// disabled submit) is now on `error_label`. A possible answer writes nothing.
pub(crate) fn refuse(answer: &Possibility, error_label: &gtk::Label) -> bool {
    if answer.is_possible() {
        return false;
    }
    show_error(error_label, reason_text(answer).as_deref());
    true
}

/// A form's save finished (rule 7): close `dialog` only on success; a failure
/// keeps it open with what was typed and puts the error on `error_label`.
pub(crate) fn finish_save(
    dialog: &impl IsA<gtk::Window>,
    error_label: &gtk::Label,
    result: Result<(), String>,
) {
    match result {
        Ok(()) => {
            show_error(error_label, None);
            dialog.close();
        }
        Err(e) => show_error(error_label, Some(&e)),
    }
}

/// The text of a submit-time `gtk::TextView` buffer.
pub(crate) fn text_of(view: &gtk::TextView) -> String {
    let buffer = view.buffer();
    let (start, end) = buffer.bounds();
    buffer.text(&start, &end, false).to_string()
}

/// A dialog's own `error-message` label: a modal dialog covers the page, so
/// the form's refusal and its save failure are painted inside it.
pub(crate) fn dialog_error_label() -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_visible(false);
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.add_css_class("error");
    crate::testid::set_test_id(&label, fauna_ui_ids::ERROR_MESSAGE);
    label
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testid::run_on_gtk_thread;
    use fauna_core::localized::LocalizedText;
    use gtk::glib;

    fn blocked() -> Possibility {
        Possibility::blocked(LocalizedText::key("feed.compose_empty"))
    }

    fn expected_reason() -> String {
        LocalizedText::key("feed.compose_empty").resolve(crate::i18n::strings::lookup)
    }

    #[test]
    fn form_gate_paints_blocked_disabled_with_the_reason_and_possible_live() {
        run_on_gtk_thread(|| {
            crate::offline_gate::reset_for_test("connected");
            let btn = gtk::Button::with_label("Post");
            paint_submit(&btn, &blocked(), None);
            assert!(!btn.is_sensitive());
            assert_eq!(
                btn.tooltip_text().as_deref(),
                Some(expected_reason().as_str())
            );

            paint_submit(&btn, &Possibility::Possible, Some("Search"));
            assert!(btn.is_sensitive());
            assert_eq!(btn.tooltip_text().as_deref(), Some("Search"));

            paint_submit(&btn, &Possibility::Absent, None);
            assert!(!btn.is_sensitive());
            paint_section(&btn, &Possibility::Absent);
            assert!(!btn.is_visible());
            paint_section(&btn, &Possibility::Possible);
            assert!(btn.is_visible());
            crate::offline_gate::reset_for_test("disconnected");
        });
    }

    /// A page clearing its blocked reason while the link is down hands the
    /// tooltip back to the offline gate rather than leaving it empty — and the
    /// gate never clears the page's own reason.
    #[test]
    fn form_gate_and_offline_gate_share_the_tooltip() {
        run_on_gtk_thread(|| {
            crate::offline_gate::reset_for_test("disconnected");
            let btn = gtk::Button::with_label("Post");
            crate::offline_gate::declare_wire_kind(&btn, crate::offline_gate::tests::online_only());
            paint_submit(&btn, &blocked(), None);
            assert_eq!(
                btn.tooltip_text().as_deref(),
                Some(expected_reason().as_str())
            );
            crate::offline_gate::set_connection_state("connected");
            assert_eq!(
                btn.tooltip_text().as_deref(),
                Some(expected_reason().as_str()),
                "a reconnect must not clear the page's reason"
            );
            crate::offline_gate::set_connection_state("disconnected");
            paint_submit(&btn, &Possibility::Possible, None);
            assert!(!btn.is_sensitive(), "still offline");
            assert!(btn.tooltip_text().is_some(), "the gate's reason returns");
            crate::offline_gate::reset_for_test("disconnected");
        });
    }

    #[test]
    fn form_gate_refuse_puts_the_reason_on_error_message() {
        run_on_gtk_thread(|| {
            let label = gtk::Label::new(None);
            label.set_visible(false);
            assert!(!refuse(&Possibility::Possible, &label));
            assert!(!label.is_visible());
            assert!(refuse(&blocked(), &label));
            assert!(label.is_visible());
            assert_eq!(label.text().as_str(), expected_reason());
        });
    }

    #[test]
    fn form_gate_finish_save_closes_only_on_success() {
        run_on_gtk_thread(|| {
            let dialog = gtk::Window::new();
            dialog.present();
            let closes = std::rc::Rc::new(std::cell::Cell::new(0));
            {
                let closes = std::rc::Rc::clone(&closes);
                dialog.connect_close_request(move |_| {
                    closes.set(closes.get() + 1);
                    glib::Propagation::Stop
                });
            }
            let label = gtk::Label::new(None);
            finish_save(&dialog, &label, Err("nest unreachable".into()));
            assert_eq!(closes.get(), 0, "a failed save keeps the form open");
            assert_eq!(label.text().as_str(), "nest unreachable");
            finish_save(&dialog, &label, Ok(()));
            assert_eq!(closes.get(), 1);
            assert!(!label.is_visible());
        });
    }
}
