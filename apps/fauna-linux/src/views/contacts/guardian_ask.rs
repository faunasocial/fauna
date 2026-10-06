//! The supervised ward's in-app contact ask — the `contact-request-guardian-button`
//! / `contact-request-pending` pair (`family-safety.md` § Child-initiated
//! contact requests → *App affordance*). One widget, two hosts: the contacts
//! page's Find User result (`find.rs`) and another actor's profile page
//! (`views/profile/mod.rs`), exactly as tui paints the pair on both pages
//! (`apps/fauna-tui/src/contacts.rs`, `apps/fauna-tui/src/profile/mod.rs`).
//!
//! The rules tui established, which this copies rather than re-derives:
//!
//! - **(a) The ask is offered only on the TYPED refusal** — the host calls
//!   [`GuardianAsk::refused`] only for [`crate::client::KnockSend::RefusedByGuardian`].
//!   Painting it on any other failure would tell an unsupervised user their
//!   account is supervised.
//! - **(c) Pending is durable**: it reads `crate::ward_asks` (the ward's own
//!   `status.contact_requests`, gated on `supervised_by` where it is folded)
//!   FIRST, the local just-asked flag second — the flag only makes the render
//!   answer immediately; the durable list is what survives navigation and a
//!   restart.
//! - **(g) The pair belongs to ONE peer**: it is built per result / per
//!   profile open and captures its peer by value, so a reply that outlives the
//!   row it was sent from can only touch that (now detached) row, never the
//!   next actor's.
//! - **(h) No supervision test at render** — both inputs are supervised-only
//!   by construction.
//!
//! The refusal text itself stays on the host page's `error-message` (rule (b)),
//! which the host owns; this widget owns only the pair.

use adw::prelude::*;
use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::i18n::strings::contacts as contacts_strings;

/// The pair for one peer. Build with [`GuardianAsk::new`], append
/// [`GuardianAsk::widget`], and drive it from the host's knock outcome.
pub struct GuardianAsk {
    widget: gtk::Box,
    ask_btn: gtk::Button,
    pending: gtk::Label,
    peer: String,
    /// The nest refused a knock to `peer` with the typed guardian error.
    refused: Cell<bool>,
    /// This session just sent the ask, before any status re-read has landed.
    asked: Cell<bool>,
    on_ask: RefCell<Option<Rc<dyn Fn()>>>,
}

impl GuardianAsk {
    /// Build the pair for `peer` (hex actor id) and render it from the durable
    /// ask list — so an ask outstanding from an earlier session shows pending
    /// on an open that never saw the refusal.
    pub fn new(peer: &str) -> Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Horizontal, 8);

        let ask_btn = gtk::Button::with_label(contacts_strings::ASK_GUARDIAN);
        crate::testid::set_test_id(&ask_btn, ids::CONTACT_REQUEST_GUARDIAN_BUTTON);
        // Queued like the knock it stands in for; declared for parity with
        // tui's `Action::RequestContact` wire kind.
        crate::offline_gate::declare_wire_kind(&ask_btn, "fauna.family.contact.request");

        let pending = gtk::Label::new(Some(contacts_strings::CONTACT_REQUEST_PENDING));
        pending.add_css_class("dim-label");
        crate::testid::set_test_id(&pending, ids::CONTACT_REQUEST_PENDING);

        widget.append(&ask_btn);
        widget.append(&pending);

        let this = Rc::new(Self {
            widget,
            ask_btn,
            pending,
            peer: peer.to_string(),
            refused: Cell::new(false),
            asked: Cell::new(false),
            on_ask: RefCell::new(None),
        });
        {
            let weak = Rc::downgrade(&this);
            this.ask_btn.connect_clicked(move |btn| {
                let Some(this) = weak.upgrade() else { return };
                // Belt-and-suspenders beside the render: a re-ask while one is
                // pending is a quiet nest-side no-op anyway, but not issuing it
                // keeps the affordance honest about its own state.
                if this.is_pending() {
                    return;
                }
                btn.set_sensitive(false);
                let cb = this.on_ask.borrow().clone();
                if let Some(cb) = cb {
                    cb();
                }
            });
        }
        this.render();
        this
    }

    /// The container to append into the host row.
    pub fn widget(&self) -> &gtk::Box {
        &self.widget
    }

    /// What the ask button does when pressed — the host wires
    /// `FaunaClient::ask_guardian_for_contact` and routes the result back to
    /// [`Self::ask_landed`] / [`Self::ask_failed`].
    pub fn connect_ask(&self, f: impl Fn() + 'static) {
        *self.on_ask.borrow_mut() = Some(Rc::new(f));
    }

    /// The knock to this peer came back with the TYPED guardian refusal.
    pub fn refused(&self) {
        self.refused.set(true);
        self.render();
    }

    /// The ask landed; `requests` is the nest's re-read (empty = the re-read
    /// failed, which keeps what the durable store already holds).
    pub fn ask_landed(&self, requests: Vec<fauna_client_family::FamilyContactRequestInfo>) {
        crate::ward_asks::replace_contact_requests(requests);
        self.asked.set(true);
        self.render();
    }

    /// The ask itself failed — re-enable the button for a retry; the host puts
    /// the ask's own typed refusal on its `error-message`.
    pub fn ask_failed(&self) {
        self.ask_btn.set_sensitive(true);
        self.render();
    }

    fn is_pending(&self) -> bool {
        crate::ward_asks::contact_ask_pending(&self.peer) || self.asked.get()
    }

    /// Pending (durable first) wins; else the ask button only after the
    /// refusal; else nothing.
    fn render(&self) {
        let pending = self.is_pending();
        self.pending.set_visible(pending);
        self.ask_btn.set_visible(!pending && self.refused.get());
        self.widget.set_visible(pending || self.refused.get());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::find::count_in;

    fn ask(byte: u8) -> fauna_client_family::FamilyContactRequestInfo {
        fauna_client_family::FamilyContactRequestInfo {
            peer_actor_id: fauna_protocol::ByteBuf::from(vec![byte; 32]),
            peer_handle: String::new(),
            created_at: 0,
            extra: Default::default(),
        }
    }

    /// Mount the pair under a root so `count_in` sees what a real walk sees
    /// (hidden widgets pruned). The root is returned so it outlives the reads.
    fn mount(a: &GuardianAsk) -> gtk::Box {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(a.widget());
        root
    }

    /// `(ask buttons, pending labels)` showing under `root`.
    fn counts(root: &gtk::Box) -> (usize, usize) {
        let r: &gtk::Widget = root.upcast_ref();
        (
            count_in(r, ids::CONTACT_REQUEST_GUARDIAN_BUTTON),
            count_in(r, ids::CONTACT_REQUEST_PENDING),
        )
    }

    /// Nothing paints until the nest actually refused: an unsupervised user
    /// must never be offered an ask (rule (a)).
    #[test]
    fn nothing_paints_before_a_refusal() {
        crate::testid::run_on_gtk_thread(|| {
            crate::ward_asks::clear_for_identity_change();
            let a = GuardianAsk::new(&"cd".repeat(32));
            let root = mount(&a);
            assert_eq!(counts(&root), (0, 0));
        });
    }

    /// The typed refusal reveals the ask; the landed ask swaps it for pending
    /// and lands in the durable store.
    #[test]
    fn a_guardian_refusal_offers_the_ask_and_then_shows_it_pending() {
        crate::testid::run_on_gtk_thread(|| {
            crate::ward_asks::clear_for_identity_change();
            let peer = "cd".repeat(32);
            let a = GuardianAsk::new(&peer);
            let root = mount(&a);
            let pressed = Rc::new(Cell::new(0));
            {
                let pressed = Rc::clone(&pressed);
                a.connect_ask(move || pressed.set(pressed.get() + 1));
            }
            a.refused();
            assert_eq!(counts(&root), (1, 0));

            a.ask_btn.emit_clicked();
            assert_eq!(pressed.get(), 1, "the ask button must issue the ask");

            // The nest's re-read lands (via the durable store).
            crate::ward_asks::set_from_status(true, Vec::new(), Vec::new());
            a.ask_landed(vec![ask(0xcd)]);
            assert_eq!(counts(&root), (0, 1));
            assert!(crate::ward_asks::contact_ask_pending(&peer));

            a.ask_btn.emit_clicked();
            assert_eq!(pressed.get(), 1, "a pending ask is not re-sent");
            crate::ward_asks::clear_for_identity_change();
        });
    }

    /// A failed re-read (empty list) still renders pending on the local flag —
    /// a landed ask is never rendered as un-asked.
    #[test]
    fn a_failed_reread_still_renders_pending() {
        crate::testid::run_on_gtk_thread(|| {
            crate::ward_asks::clear_for_identity_change();
            let a = GuardianAsk::new(&"cd".repeat(32));
            let root = mount(&a);
            a.refused();
            a.ask_landed(Vec::new());
            assert_eq!(counts(&root), (0, 1));
        });
    }

    /// The durable `status.contact_requests` paints pending on a build that
    /// never saw the refusal (a restart or a return visit) — and only for its
    /// own peer.
    #[test]
    fn an_outstanding_ask_renders_pending_without_a_refusal() {
        crate::testid::run_on_gtk_thread(|| {
            crate::ward_asks::set_from_status(true, vec![ask(0xcd)], Vec::new());
            let mine = GuardianAsk::new(&"cd".repeat(32));
            assert_eq!(counts(&mount(&mine)), (0, 1));
            let other = GuardianAsk::new(&"ef".repeat(32));
            assert_eq!(
                counts(&mount(&other)),
                (0, 0),
                "another actor's ask is not this one's"
            );
            crate::ward_asks::clear_for_identity_change();
        });
    }
}
