//! Last-started read wins: the ordering rule for a page machine whose re-reads
//! overlap its own mutations.
//!
//! A machine re-reads its list after every mutation, and the page re-reads on
//! its own too (mount, becoming visible). Those reads run concurrently. A read
//! that took its copy of the list BEFORE a mutation can land AFTER the
//! mutation's own re-read and put the old rows back. Each read takes a ticket
//! when it starts; its reply is applied only if no read that started later has
//! already been applied.

use std::sync::Mutex;

#[derive(Default)]
pub(crate) struct ReadOrder {
    state: Mutex<Counters>,
}

#[derive(Default)]
struct Counters {
    issued: u64,
    applied: u64,
}

/// A read's place in start order. Taken before the read is sent.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReadTicket(u64);

impl ReadOrder {
    /// Take a ticket for a read that is about to be sent.
    pub(crate) fn begin(&self) -> ReadTicket {
        let mut c = self.state.lock().expect("read-order mutex");
        c.issued += 1;
        ReadTicket(c.issued)
    }

    /// Whether `ticket`'s reply may be applied. `false` means a read that
    /// started later has already been applied, so this reply describes an older
    /// list. Call it with the machine's own state lock held, so that the check
    /// and the write it gates are one step.
    pub(crate) fn admit(&self, ticket: ReadTicket) -> bool {
        let mut c = self.state.lock().expect("read-order mutex");
        if ticket.0 <= c.applied {
            return false;
        }
        c.applied = ticket.0;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_later_started_read_shadows_an_earlier_one_that_lands_after_it() {
        let order = ReadOrder::default();
        let early = order.begin();
        let late = order.begin();
        assert!(order.admit(late));
        assert!(
            !order.admit(early),
            "the earlier read describes an older list"
        );
    }

    #[test]
    fn reads_that_land_in_start_order_are_all_applied() {
        let order = ReadOrder::default();
        let first = order.begin();
        let second = order.begin();
        assert!(order.admit(first));
        assert!(order.admit(second));
    }

    #[test]
    fn a_read_whose_later_sibling_never_lands_is_still_applied() {
        // A later read that failed is never admitted, so it shadows nothing.
        let order = ReadOrder::default();
        let early = order.begin();
        let _failed = order.begin();
        assert!(order.admit(early));
    }
}
