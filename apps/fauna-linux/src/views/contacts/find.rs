use adw::prelude::*;
use fauna_ui_ids as ids;
use std::rc::Rc;

use super::guardian_ask::GuardianAsk;
use crate::app::{ActionResult, UiMessage};
use crate::client::{FaunaClient, KnockSend};
use crate::i18n::strings::contacts as contacts_strings;

/// Build the "Find User" search results view — displayed as a ListBox of
/// matching actors. This is used inline in the contacts list pane when the
/// search entry is active.
pub fn build_find_results() -> gtk::ListBox {
    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::None);
    list_box.add_css_class("boxed-list");
    list_box.set_margin_start(8);
    list_box.set_margin_end(8);

    list_box
}

/// Build the copy-actor-id button for a find-result row. `contact-actor-id-
/// copy-btn` copies the FULL `actor_id` — `display` is already the
/// short/formatted `handle@domain (shortid)` form (`app.rs`'s
/// `HandleResolved` handler), so a user confirming they found the right
/// person before knocking needs the copy to carry the whole id, not the
/// truncated label (mirrors android/macOS/tui's find-result copy button). Pulled out as its own function — like `list.rs`'s
/// row builders — so a GTK unit test can drive it without a live
/// `FaunaClient` (`build_find_result_row` needs one only for the knock
/// button).
fn build_copy_actor_id_button(actor_id: &str) -> gtk::Button {
    let btn = crate::clipboard::copy_button(actor_id);
    crate::testid::set_test_id(&btn, ids::CONTACT_ACTOR_ID_COPY_BTN);
    btn
}

/// Build a search result row: display text + a copy-actor-id button + "Knock"
/// button wired to send a knock (introductory message) to the given actor ID,
/// plus the supervised ward's guardian-ask pair (`family-safety.md`
/// § Child-initiated contact requests → *App affordance*; tui's Find User
/// result, `apps/fauna-tui/src/contacts.rs`).
///
/// The row is rebuilt per lookup, so the refusal and the ask belong to the peer
/// they were made for by construction — a new lookup can never offer to ask the
/// guardian about somebody the ward did not name.
pub fn build_find_result_row(
    display: &str,
    actor_id: &str,
    client: &Rc<FaunaClient>,
) -> gtk::ListBoxRow {
    let label = gtk::Label::new(Some(display));
    label.set_halign(gtk::Align::Start);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    crate::testid::set_test_id(&label, ids::CONTACT_ACTOR_ID_RESULT);

    let copy_btn = build_copy_actor_id_button(actor_id);

    let knock_btn = gtk::Button::with_label(contacts_strings::KNOCK);
    knock_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&knock_btn, ids::CONTACTS_ADD_BUTTON);

    let ask = GuardianAsk::new(actor_id);

    {
        let c = Rc::clone(client);
        let aid = actor_id.to_string();
        let ask = Rc::clone(&ask);
        knock_btn.connect_clicked(move |btn| {
            btn.set_sensitive(false);
            let btn = btn.clone();
            let ask = Rc::clone(&ask);
            let tx = c.tx();
            // A bare id / same-nest lookup names no foreign nest, so this knock
            // goes to the home nest (`recipient_nest_url = None`), as before.
            c.send_knock_classified(&aid, None, move |outcome| {
                if let Some(error) = apply_knock_outcome(&btn, &ask, outcome) {
                    tx.send(UiMessage::Action(ActionResult::FailedLocalized {
                        message: error,
                    }));
                }
            });
        });
    }
    {
        let c = Rc::clone(client);
        let aid = actor_id.to_string();
        let weak = Rc::downgrade(&ask);
        ask.connect_ask(move || {
            let weak = weak.clone();
            let tx = c.tx();
            c.ask_guardian_for_contact(&aid, move |result| {
                let Some(ask) = weak.upgrade() else { return };
                match result {
                    Ok(requests) => {
                        ask.ask_landed(requests);
                        // Clears the refusal off `error-message`: the ask landed.
                        tx.send(UiMessage::Action(ActionResult::Success {
                            context: "contact_requested".into(),
                        }));
                    }
                    Err(e) => {
                        ask.ask_failed();
                        // The ask's own typed refusals (cap reached, peer
                        // blocked, knob off) are the ward's to read verbatim.
                        tx.send(UiMessage::Action(ActionResult::FailedLocalized {
                            message: e,
                        }));
                    }
                }
            });
        });
    }

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);
    hbox.append(&label);
    hbox.append(&copy_btn);
    hbox.append(&knock_btn);
    hbox.append(ask.widget());

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&hbox));
    row
}

/// Fold one classified knock outcome into the Find User row: `Sent` flips the
/// knock button to "Sent" and keeps it disabled; the TYPED guardian refusal
/// re-enables the knock and offers the ask; any other failure only re-enables.
/// Returns the text for `error-message`, if any — the refusal STAYS a real
/// error (rule (b)): the send did not happen, it is just no longer a dead end.
fn apply_knock_outcome(
    knock_btn: &gtk::Button,
    ask: &GuardianAsk,
    outcome: KnockSend,
) -> Option<String> {
    match outcome {
        KnockSend::Sent => {
            knock_btn.set_label(contacts_strings::SENT);
            knock_btn.set_sensitive(false);
            None
        }
        KnockSend::RefusedByGuardian => {
            knock_btn.set_sensitive(true);
            ask.refused();
            Some(contacts_strings::GUARDIAN_APPROVAL_REQUIRED.to_string())
        }
        KnockSend::Failed(e) => {
            knock_btn.set_sensitive(true);
            Some(e)
        }
    }
}

// `build_node_resolved_row` was removed: a resolved remote domain no longer
// renders a SECOND "Look up" button (which duplicated the
// `contact-actor-id-lookup` id and produced the reported confusing double
// lookup). `app.rs`'s `NestResolved` handler now auto-chains
// `resolve_handle_on_remote` and shows a transient "Looking up…" row instead.

#[cfg(test)]
mod tests {
    use super::*;

    /// `contact-actor-id-copy-btn` is declared a real page element in ui.yaml
    /// (`:417`); it used to exist only inside the dead, unreachable
    /// `contacts/detail.rs::build_contact_detail`.
    /// This pins the shape the id now rides: an activatable button whose
    /// click copies the actor id and flips its own label to confirm it.
    #[test]
    fn the_copy_button_rides_an_activatable_button_and_confirms_the_copy() {
        crate::testid::run_on_gtk_thread(|| {
            let actor_id = "ab".repeat(32);
            let btn = build_copy_actor_id_button(&actor_id);
            assert_eq!(
                btn.label().as_deref(),
                Some(crate::i18n::strings::p2p::COPY_TO_CLIPBOARD),
                "starts on the un-clicked copy label"
            );

            let root: gtk::Widget = btn.clone().upcast();
            let found = crate::automation::find::find_in(&root, "contact-actor-id-copy-btn")
                .expect("the button carries contact-actor-id-copy-btn");
            assert!(
                found.downcast_ref::<gtk::Button>().is_some(),
                "ui.yaml declares contact-actor-id-copy-btn as a button; got {}",
                found.type_().name()
            );

            // (1) the agent's real click path must *deliver* — `gtk_widget_activate`
            // is a no-op on a plain `gtk::Button` outside GTK's own main loop, so
            // this proves `actuate_click` still resolves an activate path for it.
            let reply = crate::automation::agent::actuate_click(&found);
            assert_eq!(
                reply.get("ok").and_then(|v| v.as_bool()),
                Some(true),
                "clicking contact-actor-id-copy-btn must be delivered: {reply:?}"
            );
            // (2) …and something must actually be *connected* to it. `emit_clicked`
            // is the synchronous observable (`gtk_widget_activate` completes
            // through the main loop, which a unit test does not run, so (1) alone
            // cannot see the handler — the same two-part shape as `contact-confirm`
            // in `list.rs`).
            found
                .downcast_ref::<gtk::Button>()
                .expect("checked above")
                .emit_clicked();
            assert_eq!(
                btn.label().as_deref(),
                Some(crate::i18n::strings::settings::account_page::COPIED_CLIPBOARD),
                "a real click must flip the label to confirm the copy landed"
            );
        });
    }

    /// Only the TYPED refusal offers the ask (rule (a)); it stays on
    /// `error-message` (rule (b)); a plain transport failure offers nothing —
    /// painting the ask there would tell an unsupervised user they are
    /// supervised. Mirrors tui's
    /// `a_guardian_refused_knock_offers_the_ask_and_then_shows_it_pending`.
    #[test]
    fn only_the_typed_refusal_offers_the_guardian_ask() {
        crate::testid::run_on_gtk_thread(|| {
            crate::ward_asks::clear_for_identity_change();
            let peer = "cd".repeat(32);
            let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let knock = gtk::Button::with_label(contacts_strings::KNOCK);
            let ask = GuardianAsk::new(&peer);
            root.append(&knock);
            root.append(ask.widget());
            let r: &gtk::Widget = root.upcast_ref();
            let asks =
                || crate::automation::find::count_in(r, ids::CONTACT_REQUEST_GUARDIAN_BUTTON);

            let err =
                apply_knock_outcome(&knock, &ask, KnockSend::Failed("inbox send: boom".into()));
            assert_eq!(err.as_deref(), Some("inbox send: boom"));
            assert_eq!(asks(), 0, "a transport failure must not imply supervision");
            assert!(knock.is_sensitive(), "a failed knock can be retried");

            let err = apply_knock_outcome(&knock, &ask, KnockSend::RefusedByGuardian);
            assert_eq!(
                err.as_deref(),
                Some(contacts_strings::GUARDIAN_APPROVAL_REQUIRED),
                "the refusal stays a real error on error-message"
            );
            assert_eq!(asks(), 1, "the typed refusal reveals the ask");
            assert_eq!(knock.label().as_deref(), Some(contacts_strings::KNOCK));
        });
    }

    /// A landed knock flips the button to "Sent" and keeps it disabled — only
    /// once the nest accepted it, never optimistically.
    #[test]
    fn a_sent_knock_reads_sent_and_stays_disabled() {
        crate::testid::run_on_gtk_thread(|| {
            crate::ward_asks::clear_for_identity_change();
            let knock = gtk::Button::with_label(contacts_strings::KNOCK);
            let ask = GuardianAsk::new(&"cd".repeat(32));
            knock.set_sensitive(false); // in flight
            assert_eq!(apply_knock_outcome(&knock, &ask, KnockSend::Sent), None);
            assert_eq!(knock.label().as_deref(), Some(contacts_strings::SENT));
            assert!(!knock.is_sensitive());
        });
    }
}
