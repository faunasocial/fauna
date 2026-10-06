//! The **refused-change inbox** — where both inbound rails' sinks hand a
//! refused scheduling change (`inbound-scheduling-authority.md` § *Surfacing*)
//! on its way to the account plane's `fauna.state.refused-scheduling-changes`
//! row, which this crate never reaches.
//!
//! The record rests on the account plane, whose store exists only once the
//! host's account runtime is assembled — seconds after sign-in — while the
//! inbound drains start with the session. So a sink never waits for the
//! store: it hands the row to this inbox, which forwards it to the registered
//! [`RefusedChangeLog`] when there is one and otherwise **holds** it in
//! memory, under the same ceilings the row keeps
//! ([`RefusedSchedulingChanges::record`]). The host registers the log at its
//! account-store-ready edge (`fauna_account_seams::conversation_seams::wire`),
//! and the held list is handed over whole, to be joined into the stored row —
//! the offline-share seat's *lent late* shape (`p2p.md` § Offline share
//! initiation → *The seat's record is lent late*). A process that never
//! assembles a runtime keeps the held list until it exits: a notice, not an
//! apply, is what is lost, and the calendar was already left untouched.

use std::sync::{Arc, Mutex};

use fauna_core::data::{RefusedSchedulingChange, RefusedSchedulingChanges};

use crate::backend::RefusedChangeLog;

/// The inbox a session's inbound sinks record refusals into
/// ([`crate::ConversationsManager::refused_changes`]).
#[derive(Default)]
pub struct RefusedChangeInbox {
    slot: Mutex<Slot>,
}

#[derive(Default)]
struct Slot {
    log: Option<Arc<dyn RefusedChangeLog>>,
    /// What arrived while no log was registered, held under the ceilings.
    held: RefusedSchedulingChanges,
}

impl RefusedChangeInbox {
    /// Record one refused inbound scheduling change — forwarded to the log,
    /// or held until one is registered. Never blocks.
    pub fn record(&self, row: RefusedSchedulingChange) {
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        match slot.log.clone() {
            Some(log) => {
                drop(slot);
                log.record(row);
            }
            None => {
                slot.held.record(row);
            }
        }
    }

    /// Register `log` (the account-store-ready edge), handing it everything
    /// held so far; `None` retires the current one (an identity change), and
    /// later refusals are held again.
    pub fn register(&self, log: Option<Arc<dyn RefusedChangeLog>>) {
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        slot.log = log;
        let Some(log) = slot.log.clone() else {
            return;
        };
        let held = std::mem::take(&mut slot.held);
        drop(slot);
        // A refusal recorded between the drop and the adopt goes straight to
        // the log; the two writes join in either order.
        if !held.rows.is_empty() {
            log.adopt(held);
        }
    }

    /// Forget the held list and the log — the outgoing account's refusals
    /// must never land on the incoming account's row.
    pub fn clear(&self) {
        *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = Slot::default();
    }

    /// What is held while no log is registered — for a test's inspection.
    #[must_use]
    pub fn held(&self) -> RefusedSchedulingChanges {
        self.slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .held
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorded {
        records: Mutex<Vec<RefusedSchedulingChange>>,
        adopted: Mutex<Vec<RefusedSchedulingChanges>>,
    }

    impl RefusedChangeLog for Recorded {
        fn record(&self, row: RefusedSchedulingChange) {
            self.records.lock().unwrap().push(row);
        }
        fn adopt(&self, held: RefusedSchedulingChanges) {
            self.adopted.lock().unwrap().push(held);
        }
    }

    fn refusal(uid: u8) -> RefusedSchedulingChange {
        RefusedSchedulingChange {
            uid_hash: fauna_core::hex32::encode(&[uid; 32]),
            author: Some(fauna_core::hex32::encode(&[0xaa; 32])),
            author_home_nest_url: String::new(),
            sender_address: String::new(),
            method: "CANCEL".into(),
            reason: "not_the_organizer".into(),
            summary: String::new(),
            first_refused_at: 100,
            last_refused_at: 100,
            occurrences: 0,
            dismissed_through: 0,
            extra: Default::default(),
        }
    }

    /// A refusal drained before the account store is up is held, handed over
    /// whole when the log registers, and later ones go straight through.
    #[test]
    fn a_refusal_before_the_store_is_up_is_held_and_handed_over() {
        let inbox = RefusedChangeInbox::default();
        inbox.record(refusal(1));
        inbox.record(refusal(1));
        assert_eq!(
            inbox.held().rows.len(),
            1,
            "a repeat collapses onto its row"
        );
        assert_eq!(inbox.held().rows[0].occurrences, 2);

        let log = Arc::new(Recorded::default());
        inbox.register(Some(log.clone()));
        assert_eq!(
            log.adopted.lock().unwrap().len(),
            1,
            "the held list is handed over"
        );
        assert!(inbox.held().rows.is_empty());
        inbox.record(refusal(2));
        assert_eq!(
            log.records.lock().unwrap().len(),
            1,
            "later ones go straight through"
        );

        inbox.register(None);
        inbox.record(refusal(3));
        assert_eq!(inbox.held().rows.len(), 1, "a retired log holds again");
        inbox.clear();
        assert!(
            inbox.held().rows.is_empty(),
            "an identity change forgets them"
        );
    }
}
