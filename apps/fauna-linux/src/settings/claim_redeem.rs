//! The claim-redeem pair (`subscription-claim-redeem-{input,button}`) both
//! redeem doors paint: Settings → Subscriptions (`settings/subscriptions.rs`)
//! and another person's profile (`views/profile/offers.rs`) —
//! `monetization.md` § Pillar 3 → *Which nest an in-app redeem binds on* →
//! *The doors*. One widget tree, one answer.
//!
//! The answer is the shared `fauna_client_features::gate_possibility` over the
//! `payments` row of the features courtesy read (`ui/README.md` § Offer only
//! what is possible, rules 2, 5 and 6), the twin of tui's
//! `settings::subscriptions::redeem_gate`:
//!
//! * before the read answers the button is blocked still loading (rule 5 —
//!   never offered on a guess); a failed read blocks it with load-failed;
//! * a nest without the payments plane (no `payments` row, or a `hidden` one)
//!   answers absent, and the WHOLE block — title, code field and button — is
//!   left out (rule 2);
//! * a `disabled` row blocks it with the row's own restriction.
//!
//! The button is painted through [`crate::possible::paint`] with its reason on
//! the block's why-line; the press asks [`RedeemPair::answer`] again and
//! refuses with the reason on the page's `error-message`
//! ([`RedeemPair::refuse`]). Each door issues the features read itself
//! ([`RedeemPair::read_gate`]), so the profile never depends on Settings
//! having been visited. Compile-time excision is the orthogonal axis: the
//! module exists only with the `payments` feature.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use fauna_core::possibility::{Possibility, ReadState};
use fauna_ui_ids as ids;

use crate::async_helper::spawn_with_snapshot;
use crate::client::FaunaClient;
use crate::i18n::strings::subscriptions as s;
use crate::testid::set_test_id;

/// The redeem block's gate over a features read: `None` until it answers.
pub(crate) fn redeem_gate(rows: Option<&[fauna_client_features::FeatureRow]>) -> Possibility {
    fauna_client_features::gate_possibility(rows, "payments", crate::i18n::strings::lookup)
}

/// The redeem block: its section (title + code field + button + why-line) and
/// the gate it is painted from.
#[derive(Clone)]
pub(crate) struct RedeemPair {
    pub(crate) section: gtk::Box,
    pub(crate) input: gtk::Entry,
    pub(crate) button: gtk::Button,
    reason_line: gtk::Label,
    /// The `payments` gate — blocked still loading until the read answers.
    gate: Rc<RefCell<Possibility>>,
}

impl RedeemPair {
    /// Build the block, painted blocked still loading (no read yet).
    pub(crate) fn build() -> Self {
        let section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        let title = gtk::Label::new(Some(s::REDEEM_CLAIM_TITLE));
        title.set_halign(gtk::Align::Start);
        title.add_css_class("title-4");
        section.append(&title);
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        let input = gtk::Entry::builder()
            .placeholder_text(s::CLAIM_CODE)
            .hexpand(true)
            .build();
        set_test_id(&input, ids::SUBSCRIPTION_CLAIM_REDEEM_INPUT);
        row.append(&input);
        let button = gtk::Button::with_label(s::REDEEM);
        button.add_css_class("suggested-action");
        set_test_id(&button, ids::SUBSCRIPTION_CLAIM_REDEEM_BUTTON);
        crate::offline_gate::declare_wire_kind(&button, "fauna.payments.claims.redeem");
        row.append(&button);
        section.append(&row);
        // The button's why-line (rule 2: the reason within eyeshot), painted
        // by `crate::possible::paint`, hidden while possible.
        let reason_line = gtk::Label::builder().visible(false).build();
        reason_line.set_halign(gtk::Align::Start);
        reason_line.set_wrap(true);
        reason_line.add_css_class("dim-label");
        reason_line.add_css_class("caption");
        section.append(&reason_line);
        let pair = Self {
            section,
            input,
            button,
            reason_line,
            gate: Rc::new(RefCell::new(redeem_gate(None))),
        };
        pair.repaint();
        {
            let pair_c = pair.clone();
            pair.input.connect_changed(move |_| pair_c.repaint());
        }
        pair
    }

    /// The answer the button is painted from and its press asks again: the
    /// `payments` gate first, then the shared code-draft predicate
    /// (`fauna_client_subscriptions::forms::redeem_code_possibility` — a blank
    /// code is blocked with its own reason; `ui/profile.md` § Errors & edge
    /// cases).
    pub(crate) fn answer(&self) -> Possibility {
        self.gate
            .borrow()
            .clone()
            .and(fauna_client_subscriptions::forms::redeem_code_possibility(
                &self.input.text(),
            ))
    }

    /// Apply a new gate answer and repaint.
    pub(crate) fn set_gate(&self, gate: Possibility) {
        *self.gate.borrow_mut() = gate;
        self.repaint();
    }

    /// Paint the block from [`Self::answer`]: an absent answer leaves the
    /// whole block out, otherwise the button carries the answer.
    pub(crate) fn repaint(&self) {
        let answer = self.answer();
        crate::form_gate::paint_section(&self.section, &answer);
        crate::possible::paint(&self.button, Some(&self.reason_line), answer);
    }

    /// The press's own ask (rule 6): `true` when the answer is not possible —
    /// the caller sends nothing, and the reason is on `error_label`.
    pub(crate) fn refuse(&self, error_label: &gtk::Label) -> bool {
        let refused = crate::form_gate::refuse(&self.answer(), error_label);
        if refused {
            error_label.add_css_class("error");
        }
        refused
    }

    /// Issue the features courtesy read and apply its `payments` answer — a
    /// failed read blocks the button with load-failed.
    pub(crate) fn read_gate(&self, client: &FaunaClient) {
        let features = fauna_client_features::FeaturesClient::new(client.nest_rpc().clone());
        let pair = self.clone();
        spawn_with_snapshot(
            &client.runtime_handle(),
            move || async move { features.rows().await.map_err(|e| e.to_string()) },
            move |res| {
                let gate = match res {
                    Ok(rows) => redeem_gate(Some(&rows)),
                    Err(e) => {
                        tracing::error!("[claim-redeem] features fetch error: {e}");
                        Possibility::from_read(ReadState::Failed)
                    }
                };
                pair.set_gate(gate);
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An available row keyed `payments` — the shared fixture's composed row,
    /// re-keyed: the gate reads only the key and the affordance.
    fn payments_row() -> fauna_client_features::FeatureRow {
        let mut row = fauna_client_features::test_fixtures::zaps_row(&[]);
        row.feature = "payments".to_string();
        row
    }

    fn reason(key: &str) -> String {
        fauna_core::localized::LocalizedText::key(key).resolve(crate::i18n::strings::lookup)
    }

    /// Rule 5: before the read answers, the redeem is blocked still loading
    /// with its reason beside it, and a press refuses with the same reason on
    /// `error-message`; a failed read blocks it with load-failed.
    #[test]
    fn redeem_waits_for_the_read_and_refuses_with_its_reason() {
        crate::testid::run_on_gtk_thread(|| {
            let pair = RedeemPair::build();
            let error = gtk::Label::new(None);
            let loading = reason("common.still_loading");
            assert!(pair.section.is_visible());
            assert!(pair.button.is_visible() && !pair.button.is_sensitive());
            assert!(pair.reason_line.is_visible());
            assert_eq!(pair.reason_line.text().as_str(), loading);
            assert!(pair.refuse(&error));
            assert_eq!(error.text().as_str(), loading);

            pair.set_gate(Possibility::from_read(ReadState::Failed));
            let failed = reason("common.load_failed");
            assert!(!pair.button.is_sensitive());
            assert_eq!(pair.reason_line.text().as_str(), failed);
            assert!(pair.refuse(&error));
            assert_eq!(error.text().as_str(), failed);
        });
    }

    /// An available `payments` row makes the redeem live; a loaded read with
    /// no `payments` row leaves the whole block out (rule 2).
    #[test]
    fn redeem_is_live_with_the_plane_and_absent_without_it() {
        crate::testid::run_on_gtk_thread(|| {
            // Connected, so the offline gate does not mask the page's answer.
            crate::offline_gate::reset_for_test("connected");
            let pair = RedeemPair::build();
            let error = gtk::Label::new(None);
            pair.set_gate(redeem_gate(Some(&[payments_row()])));
            // With the plane, a blank code is blocked by the shared code
            // predicate with its own reason; a typed code is live.
            let required = reason("subscriptions.redeem_code_required");
            assert!(!pair.button.is_sensitive());
            assert_eq!(pair.reason_line.text().as_str(), required);
            assert!(pair.refuse(&error));
            assert_eq!(error.text().as_str(), required);
            pair.input.set_text("CODE42");
            assert!(pair.section.is_visible());
            assert!(pair.button.is_sensitive());
            assert!(!pair.reason_line.is_visible());
            assert!(!pair.refuse(&error));

            pair.set_gate(redeem_gate(Some(&[])));
            assert_eq!(pair.answer(), Possibility::Absent);
            assert!(!pair.section.is_visible(), "the whole block is left out");
            assert!(!pair.button.is_visible());
            crate::offline_gate::reset_for_test("disconnected");
        });
    }
}
