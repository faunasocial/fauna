//! The hold on a supersession this device's OWN stolen-identity ceremony
//! caused (`settings.md` § Recovery kit → *The persist-failure message
//! survives the page*, its closing rule).
//!
//! The ceremony supersedes the identity the running session holds, so the
//! session's next auth refresh is refused the instant the nest commits —
//! typically before the ceremony's own result is folded. Escalating then runs
//! the launch-escalation handler (`main.rs::register_launch_escalation_handler`),
//! which shuts the outgoing client's runtime down — and with it the
//! `do_succeed_identity` task still on it — so `RecoverySucceeded` never
//! arrives, and when the successor's seed could not be stored the only copy of
//! the new key is lost. So while the ceremony runs, while its outcome is the
//! message the user is reading on Account, or while its persist-failure
//! message is parked, the escalation is OWED rather than performed; it is
//! performed when the user leaves Account, or at once when the ceremony ends
//! while the user is elsewhere with nothing parked.
//!
//! tui's twin is `App::defer_own_supersession` /
//! `escalate_deferred_supersession` / `forget_deferred_supersession`
//! (`apps/fauna-tui/src/app.rs`), FaunaKit's `StolenCeremonyHold`, windows'
//! port in `FaunaApp.Core`. The two decisions are the pure
//! [`StolenCeremonyHold`] and [`AccountVisit`], unit-pinned below; the
//! thread-local glue around them only reads and writes GTK-thread state.

use std::cell::RefCell;

/// The ceremony's claim on a supersession it caused.
#[derive(Debug, Default)]
pub(crate) struct StolenCeremonyHold {
    /// `identity-stolen-button` dispatched the ceremony and its
    /// `RecoverySucceeded` fold has not landed yet.
    in_flight: bool,
    /// The ceremony ended WITHOUT adopting a successor while the user was on
    /// Account, so its message (the undecidable arm's *reopen the app*, or the
    /// persist-failure key) is what the page shows. The refusal the ceremony
    /// causes has no fixed order against the fold — tui measured the launch
    /// machine's landing ~30 ms after an undecidable fold — so the ceremony
    /// keeps owning its supersession until the user leaves Account.
    outcome_on_screen: bool,
    /// A supersession arrived while the ceremony owned the screen; its
    /// escalation is owed.
    deferred: bool,
}

impl StolenCeremonyHold {
    /// The ceremony has been dispatched.
    pub(crate) fn begin(&mut self) {
        self.in_flight = true;
    }

    /// Would a supersession arriving now be this device's own ceremony's?
    pub(crate) fn owns(&self, message_parked: bool) -> bool {
        self.in_flight || self.outcome_on_screen || message_parked
    }

    /// A supersession arrived: record it as owed and answer `true` when the
    /// ceremony owns it, else answer `false` (the caller escalates now).
    pub(crate) fn defer(&mut self, message_parked: bool) -> bool {
        let owned = self.owns(message_parked);
        if owned {
            self.deferred = true;
        }
        owned
    }

    /// The ceremony's `RecoverySucceeded` fold landed. `adopted`: the app is
    /// switching to the successor, which is itself the full relaunch, so an
    /// owed escalation is SPENT, not performed. Answers whether the owed
    /// escalation is due now.
    pub(crate) fn settle(&mut self, adopted: bool, on_account: bool, message_parked: bool) -> bool {
        self.in_flight = false;
        if adopted {
            self.deferred = false;
            return false;
        }
        if on_account {
            // The leave edge performs it: the user reads this fold's message
            // first.
            self.outcome_on_screen = true;
            return false;
        }
        // Off Account: due now, unless a parked key still holds it — escalating
        // tears down the page that key is shown on.
        !message_parked && std::mem::take(&mut self.deferred)
    }

    /// The user left Account. Answers whether an owed escalation is due now.
    pub(crate) fn leave_account(&mut self) -> bool {
        self.outcome_on_screen = false;
        std::mem::take(&mut self.deferred)
    }

    /// The session is being torn down (an account switch, a sign-out, the
    /// escalation itself): whatever the ceremony was doing belongs to the
    /// departing session, so nothing may go on deferring on its behalf — and a
    /// supersession owed for that session is moot.
    pub(crate) fn teardown(&mut self) {
        *self = Self::default();
    }
}

/// The (top-level page, Settings sub-page) pair and its falling edge — "the
/// user left Account". Two stacks feed it: leaving the Settings shell
/// (`settings-nav-back`, a rail click) changes only the outer stack and leaves
/// the sub-stack on `account`, so an edge watched on the sub-stack alone would
/// fire only on the NEXT visit, when the shell's canonical-entry reset moves
/// the sub-stack off `account`.
#[derive(Debug, Default)]
pub(crate) struct AccountVisit {
    on_settings: bool,
    sub_on_account: bool,
    was_on_account: bool,
}

impl AccountVisit {
    /// The outer stack moved; answers whether the user just left Account.
    pub(crate) fn note_shell(&mut self, on_settings: bool) -> bool {
        self.on_settings = on_settings;
        self.edge()
    }

    /// The Settings sub-stack moved; answers whether the user just left Account.
    pub(crate) fn note_sub(&mut self, on_account: bool) -> bool {
        self.sub_on_account = on_account;
        self.edge()
    }

    pub(crate) fn on_account(&self) -> bool {
        self.on_settings && self.sub_on_account
    }

    fn edge(&mut self) -> bool {
        let now = self.on_account();
        let was = std::mem::replace(&mut self.was_on_account, now);
        was && !now
    }
}

thread_local! {
    static HOLD: RefCell<StolenCeremonyHold> = RefCell::new(StolenCeremonyHold::default());
    static VISIT: RefCell<AccountVisit> = RefCell::new(AccountVisit::default());
}

/// `identity-stolen-button` is dispatching the ceremony.
pub(crate) fn begin_stolen_ceremony() {
    HOLD.with(|h| h.borrow_mut().begin());
}

/// A mid-session supersession arrived (`DataMessage::IdentitySuperseded`, from
/// the silent sign-in or the supervisor's stop): `true` when this device's own
/// ceremony owns it and the escalation is now owed.
pub(crate) fn defer_own_supersession() -> bool {
    let parked = super::has_pending_stolen_failed_message();
    HOLD.with(|h| h.borrow_mut().defer(parked))
}

/// The ceremony's fold landed — called by `apply_recovery_succeeded` AFTER any
/// persist-failure message is parked and BEFORE an adopting switch runs.
pub(super) fn settle_stolen_ceremony(adopted: bool) {
    let on_account = on_account();
    let parked = super::has_pending_stolen_failed_message();
    if HOLD.with(|h| h.borrow_mut().settle(adopted, on_account, parked)) {
        super::escalate_to_launch("identity-superseded (held back by the ceremony)");
    }
}

/// The session is being torn down; see [`StolenCeremonyHold::teardown`].
pub(crate) fn stolen_ceremony_teardown() {
    HOLD.with(|h| h.borrow_mut().teardown());
}

/// The top-level content stack's visible page changed (`app.rs`). Call it
/// AFTER the shell's canonical-entry reset, so entering Settings never reads
/// the previous visit's `account` sub-page as a fresh visit.
pub(crate) fn note_shell_page(on_settings: bool) {
    let left = VISIT.with(|v| v.borrow_mut().note_shell(on_settings));
    if left {
        left_account();
    }
}

/// The Settings sub-stack's visible page changed (`views/settings_shell.rs`).
pub(crate) fn note_settings_sub_page(on_account: bool) {
    let left = VISIT.with(|v| v.borrow_mut().note_sub(on_account));
    if left {
        left_account();
    }
}

/// Whether the user is on Settings → Account right now.
pub(super) fn on_account() -> bool {
    VISIT.with(|v| v.borrow().on_account())
}

/// A fresh Settings shell is being built (first build, or a rebuild after a
/// teardown destroyed the old windows without notifying their stacks): the
/// previous tree's visit is over.
pub(crate) fn reset_account_visit() {
    VISIT.with(|v| *v.borrow_mut() = AccountVisit::default());
}

/// Leaving Account is the acknowledgment gesture: it discharges a parked
/// persist-failure message (the user has had the whole visit to copy the key),
/// and performs a supersession the ceremony held back.
fn left_account() {
    super::acknowledge_stolen_failed_message();
    if HOLD.with(|h| h.borrow_mut().leave_account()) {
        // Synchronous is safe from a stack notify: the handler only counts the
        // teardown here and defers the window teardown to a 100 ms timeout.
        super::escalate_to_launch("identity-superseded (held back by the ceremony)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The race this hold exists for (tui's
    /// `the_ceremonys_own_supersession_waits_for_the_parked_key`): the refusal
    /// lands mid-ceremony, the fold parks the key the keystore refused, and the
    /// escalation waits until the user leaves Account.
    #[test]
    fn the_ceremonys_own_supersession_waits_for_the_parked_key() {
        let mut hold = StolenCeremonyHold::default();
        hold.begin();
        assert!(
            hold.defer(false),
            "mid-ceremony, the supersession is the ceremony's own"
        );
        assert!(
            !hold.settle(false, true, true),
            "the fold parked the key on Account — escalating would tear it down"
        );
        assert!(
            hold.defer(true),
            "a second refusal while the key is parked is held too"
        );
        assert!(
            hold.leave_account(),
            "leaving Account performs the owed escalation"
        );
        assert!(!hold.leave_account(), "performed once, never twice");
    }

    #[test]
    fn a_supersession_with_no_ceremony_escalates_at_once() {
        let mut hold = StolenCeremonyHold::default();
        assert!(!hold.defer(false));
    }

    /// Past the fold: an undecidable outcome on Account still owns a refusal
    /// that arrives AFTER it (tui `stolen_outcome_on_screen`).
    #[test]
    fn an_outcome_that_adopted_nothing_keeps_owning_its_supersession_on_account() {
        let mut hold = StolenCeremonyHold::default();
        hold.begin();
        assert!(!hold.settle(false, true, false));
        assert!(
            hold.defer(false),
            "the late refusal waits for the leave edge"
        );
        assert!(hold.leave_account());
        assert!(
            !hold.defer(false),
            "once the user has left, a supersession is ordinary"
        );
    }

    #[test]
    fn an_adopting_outcome_spends_the_owed_escalation() {
        let mut hold = StolenCeremonyHold::default();
        hold.begin();
        assert!(hold.defer(false));
        assert!(
            !hold.settle(true, true, false),
            "the switch IS the relaunch"
        );
        assert!(!hold.leave_account(), "nothing is owed after an adoption");
    }

    #[test]
    fn a_fold_off_account_with_nothing_parked_performs_it_at_once() {
        let mut hold = StolenCeremonyHold::default();
        hold.begin();
        assert!(hold.defer(false));
        assert!(hold.settle(false, false, false));
    }

    #[test]
    fn a_fold_off_account_with_a_parked_key_still_waits_for_the_edge() {
        let mut hold = StolenCeremonyHold::default();
        hold.begin();
        assert!(hold.defer(false));
        assert!(!hold.settle(false, false, true));
        assert!(hold.leave_account());
    }

    #[test]
    fn a_teardown_ends_the_ceremonys_claim() {
        let mut hold = StolenCeremonyHold::default();
        hold.begin();
        assert!(hold.defer(false));
        hold.teardown();
        assert!(!hold.defer(false));
        assert!(!hold.leave_account());
    }

    /// `settings-nav-back` moves only the outer stack: the sub-stack stays on
    /// `account`, and that alone must read as leaving Account.
    #[test]
    fn leaving_the_settings_shell_from_account_is_leaving_account() {
        let mut visit = AccountVisit::default();
        assert!(!visit.note_shell(true));
        assert!(!visit.note_sub(true));
        assert!(visit.on_account());
        assert!(
            visit.note_shell(false),
            "the shell left with the sub-stack still on account"
        );
    }

    #[test]
    fn switching_sub_pages_off_account_is_leaving_account() {
        let mut visit = AccountVisit::default();
        visit.note_shell(true);
        visit.note_sub(true);
        assert!(visit.note_sub(false));
    }

    /// Re-entering Settings: `app.rs` resets the sub-stack to the canonical
    /// entry BEFORE noting the shell, so the previous visit's `account` never
    /// reads as a visit that then ends.
    #[test]
    fn re_entering_settings_is_not_a_visit_to_account() {
        let mut visit = AccountVisit::default();
        visit.note_shell(true);
        visit.note_sub(true);
        assert!(visit.note_shell(false));
        assert!(
            !visit.note_sub(false),
            "the canonical-entry reset while outside"
        );
        assert!(!visit.note_shell(true));
        assert!(!visit.on_account());
    }
}
