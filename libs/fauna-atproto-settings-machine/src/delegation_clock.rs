//! The D10 delegation row's RENDER clock — the `now` `refresh_delegation`
//! compares a stored cert's `expires_at` against, plus the e2e offset that
//! lets a test move it.
//!
//! **Deliberately scoped to this one comparison, never the mint clock.**
//! [`crate::machine::AtprotoSettingsMachine::authorize_external_apps`] always
//! stamps a freshly minted cert's `created_at` with the real
//! [`fauna_core::data::Timestamp::now`] — faking that would mint a
//! future-dated cert (confusing "authorized on" text, and a code path the
//! nest's provision-time check has never seen for real). Only the liveness
//! read is fake-clock-aware, so a test can advance past the ~90-day window to
//! reach `expiring_soon`/`expired` without sleeping months
//! (`testing.md` convention 14 — a fake clock, never a sleep).
//!
//! Mirrors `fauna_client_backup::audit_clock` byte-for-byte in shape (same
//! `AtomicI64` + cfg gate + reset caveat); kept as its own module rather than
//! reused because the two clocks live in different domains, and sharing one
//! would make a delegation test's offset silently perturb an unrelated
//! backup-audit assertion in the same process, or vice versa.
//!
//! Spec: `docs/goal/behavior/atproto-pds-full.md` § D10 → "Re-authorizing is
//! the renewal gesture" (the liveness-row UX contract this clock exists to
//! test).

/// Seconds added to the delegation row's `now`, set by the
/// `atproto_delegation_advance_clock` agent command. Zero in every real run.
///
/// Compiled out of release artifacts (testing.md convention 15), like every
/// other automation hook. A consumer forwards its own opt-in feature to this
/// crate's `e2e-agent` — tui's is `e2e-agent`.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
static CLOCK_OFFSET_SECS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// The delegation clock offset, in seconds.
///
/// **`fauna_client_backup::audit_clock` holds a byte-identical accessor pair
/// over its own static, and that is deliberate on both counts** (adjudicated
/// 2026-08-23 by the near-duplicate sweep). The *statics* must stay separate —
/// one shared offset would let `atproto_delegation_advance_clock` silently move
/// the backup audit's clock too, and each module's own doc already warns how
/// far a leaked offset travels. The *accessors* cannot move either: the
/// `#[cfg(any(debug_assertions, feature = "e2e-agent"))]` gate names **this
/// crate's** `e2e-agent` feature (convention 15), so a shared wrapper in a lower
/// crate would resolve the gate against the wrong crate's features. What is
/// left to share is the bare `AtomicI64`, which is not worth the indirection.
pub fn clock_offset_secs() -> i64 {
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    {
        CLOCK_OFFSET_SECS.load(std::sync::atomic::Ordering::SeqCst)
    }
    #[cfg(not(any(debug_assertions, feature = "e2e-agent")))]
    {
        0
    }
}

/// Set the offset above. ⚠ **Process-wide and nothing auto-resets it** — a
/// test that leaves an offset behind silently lapses the very next
/// delegation this process mints. Zero it once the lapse assertions are done,
/// before re-authorizing.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn set_clock_offset_secs(offset: i64) {
    CLOCK_OFFSET_SECS.store(offset, std::sync::atomic::Ordering::SeqCst);
}

/// The instant [`crate::machine::AtprotoSettingsMachine::refresh_delegation`]
/// compares a cert's `expires_at` against — real wall-clock microseconds plus
/// the e2e offset above, which is zero in every real run.
pub fn now() -> fauna_core::data::Timestamp {
    let real = fauna_core::data::Timestamp::now();
    let offset_micros = clock_offset_secs().saturating_mul(1_000_000);
    fauna_core::data::Timestamp(real.0.saturating_add_signed(offset_micros))
}
