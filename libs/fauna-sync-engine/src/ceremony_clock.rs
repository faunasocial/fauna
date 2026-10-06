//! The co-present ceremony's **admission clock** — the `now` a receive-act
//! expectation is minted and judged against, plus the e2e offset that lets a
//! test reach the lapse without sleeping out the real window.
//!
//! The window is a Rust constant and never a knob
//! (`fauna_client_capabilities::group_ceremony_peer::GROUP_CEREMONY_EXPECTATION_TTL_SECS`,
//! 15 minutes): a co-present ceremony either happens within the sitting that
//! minted the expectation, or is re-initiated by re-scanning. So the only way
//! a journey can witness *"someone arriving after it has lapsed is refused
//! like a stranger"* (`p2p.md` § Offline share initiation) is to move the
//! clock — convention 14's fake clock, never a sleep, and never a
//! configurable TTL.
//!
//! **Why an offset setter and not a launch-time env seed.** The receive act
//! happens mid-session: the app is already running and already bound when the
//! window opens, so a value read once at launch could not move it afterwards.
//! That is the same shape `fauna_client_backup::audit_clock` and
//! `fauna_atproto_settings_machine::delegation_clock` take, and this is the
//! fourth instance of it; `fauna_protocol::client_clock`'s env seed (the
//! bearer clock, read as `fauna_launch_machine::launch_clock`) is the *other*
//! shape, for a clock that only has to be wrong before startup.
//!
//! **Scoped to the ceremony's own clock, and process-wide within it.** The
//! offset moves only the [`NowFn`](fauna_client_capabilities::group_ceremony_peer::NowFn)
//! a seat is bound with — `offline_share::now_fn` — which both mints an
//! expectation's `expires_at` and judges it. Nothing else reads it: the
//! ceremony's record timestamps keep `offline_share::now_secs`' real clock, so
//! an offset cannot forward-date a durable row. A seat reads the offset at
//! every call rather than at bind, which is what lets a test move the window
//! of a seat that is already listening.
//!
//! Compiled out of release artifacts (`e2e-automation-surface-gating.md`
//! convention 15) exactly as its three siblings are: the static, the setter
//! and the read are all behind this crate's own `e2e-agent` (a consumer
//! forwards its own feature to it — tui's and linux's are `e2e-agent`), so a
//! release build has no offset to move and [`now_secs`] is the bare wall
//! clock.

/// Seconds added to the ceremony admission clock, set by the
/// `offline_share_advance_clock` agent command. Zero in every real run.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
static CLOCK_OFFSET_SECS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// The ceremony clock offset, in seconds.
///
/// The `#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]` gate names
/// **this crate's** `e2e-agent` feature, which is why this accessor pair sits
/// here beside its own static rather than being shared with `audit_clock` or
/// `delegation_clock`: a shared wrapper in a lower crate would resolve the
/// gate against the wrong crate's features, and one shared offset would let a
/// ceremony test silently move an unrelated backup-audit or delegation
/// assertion in the same process.
pub fn clock_offset_secs() -> i64 {
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    {
        CLOCK_OFFSET_SECS.load(std::sync::atomic::Ordering::SeqCst)
    }
    #[cfg(not(any(test, debug_assertions, feature = "e2e-agent")))]
    {
        0
    }
}

/// Set the offset above. ⚠ **Process-wide, and nothing auto-resets it** — a
/// test that leaves an offset behind lapses the very next expectation this
/// process mints, which reads as an unrelated ceremony mysteriously being
/// refused. Zero it once the lapse assertions are done.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn set_clock_offset_secs(offset: i64) {
    CLOCK_OFFSET_SECS.store(offset, std::sync::atomic::Ordering::SeqCst);
}

/// The instant a receive-act expectation is minted and judged against — real
/// wall-clock seconds plus the offset above, which is zero in every real run.
pub fn now_secs() -> u64 {
    let real = fauna_core::data::Timestamp::now_secs_or_zero();
    real.saturating_add(clock_offset_secs()).max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The offset moves the clock in both directions and zeroes back to the
    /// real one. Serialized with the test below by running both assertions
    /// here: the static is process-wide, so two tests racing it would flake.
    #[test]
    fn the_offset_moves_the_admission_clock_and_resets() {
        let real = fauna_core::data::Timestamp::now_secs_or_zero() as u64;
        assert!(
            now_secs().abs_diff(real) <= 2,
            "with no offset this is the wall clock"
        );

        set_clock_offset_secs(3600);
        assert!(
            now_secs() >= real + 3595,
            "an hour of offset must move the admission clock an hour"
        );

        // A negative offset is legal and must not underflow the u64 — a test
        // that moves the clock backwards past the epoch would otherwise wrap
        // to a colossal `now` and admit everything for ever.
        set_clock_offset_secs(-(real as i64) - 10_000);
        assert_eq!(now_secs(), 0, "the clock floors at zero, never wraps");

        set_clock_offset_secs(0);
        assert!(
            now_secs().abs_diff(real) <= 2,
            "zero puts the real clock back"
        );
    }
}
