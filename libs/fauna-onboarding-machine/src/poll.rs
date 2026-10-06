//! Client-owned poll cadences for the wizard's two waiting surfaces.
//!
//! Both surfaces wait on something no push channel can tell an anonymous or
//! not-yet-registered caller about, so the client drives a timer and calls the
//! machine's single-shot recheck. The *intervals* live here, in shared Rust,
//! because the alternative is what shipped before: seven hand-copied numbers
//! that drift apart silently (priority #1, and `onboarding.md` § The
//! pending-invite surface says it in as many words — "read by all 7 apps, never
//! seven hand-copied numbers").
//!
//! The apps own the *timer*, not the number: each platform's idiomatic timer
//! (glib `timeout_add_local`, a Svelte `$effect` `setInterval`, Compose
//! `delay`, a `DispatcherTimer`, a tui tick) reads its interval from here.

/// How often the `invite_request` page re-polls while it shows `PendingReview`
/// (`onboarding.md` § The pending-invite surface).
///
/// Admin review is human-latency, so this is 3× lighter than the DNS surface's
/// cadence while staying snappy on approval. The first poll fires *immediately*
/// when the page shows a hydrated `PendingReview` (the relaunch case), then on
/// this interval while the page is visible.
pub const INVITE_RECHECK_POLL_MS: u64 = 30_000;

/// How often the `awaiting_manual_dns` page re-checks whether the user's
/// hand-entered DNS records have propagated.
///
/// Faster than the invite cadence because the user is actively working in
/// another tab and expects the page to notice quickly.
pub const AWAITING_DNS_POLL_MS: u64 = 10_000;

/// `INVITE_RECHECK_POLL_MS` for the native apps — UniFFI exports functions, not
/// constants, so the value still crosses the FFI from this one definition.
#[cfg(feature = "uniffi")]
#[uniffi::export]
pub fn invite_recheck_poll_ms() -> u64 {
    INVITE_RECHECK_POLL_MS
}

/// `AWAITING_DNS_POLL_MS` for the native apps — see
/// [`invite_recheck_poll_ms`].
#[cfg(feature = "uniffi")]
#[uniffi::export]
pub fn awaiting_dns_poll_ms() -> u64 {
    AWAITING_DNS_POLL_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cadence rationale in `onboarding.md` § The pending-invite surface is
    /// stated as a RATIO ("3× lighter than the DNS surface's 10s"), so pin the
    /// ratio, not just the literals — a future tuning pass that moves one and
    /// forgets the other lands here rather than in a goal-doc contradiction.
    #[test]
    fn invite_review_polls_three_times_lighter_than_dns() {
        assert_eq!(INVITE_RECHECK_POLL_MS, 3 * AWAITING_DNS_POLL_MS);
    }

    /// A zero or sub-second cadence would hammer an unauthenticated nest
    /// endpoint from every waiting app.
    ///
    /// `const {}` so this is a COMPILE-time floor, not a runtime one: both
    /// operands are constants, so a violating edit should fail the build rather
    /// than wait for someone to run the test (and clippy rejects the runtime
    /// form for exactly that reason).
    #[test]
    fn both_cadences_are_at_least_a_second() {
        const { assert!(INVITE_RECHECK_POLL_MS >= 1_000) };
        const { assert!(AWAITING_DNS_POLL_MS >= 1_000) };
    }
}
