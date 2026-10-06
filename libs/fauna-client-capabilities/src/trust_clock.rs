//! The Nests page trust facet's RENDER clock — the `now` a grant's liveness
//! (`view_model::trust_facet_for_holders`), the auto-renew loop's *due*
//! decision (`view_model::grants_due_for_renewal`) and a custodian's receipt
//! freshness (`view_model::fold_custody_facet`) are judged against, plus the
//! e2e offset that lets a test move it. The loop rides this one clock rather
//! than a second seam, so a test that moves it sees the grant it lapses toward
//! "expiring soon" also renewed.
//!
//! **Deliberately scoped to those comparisons, never the mint clock.** A
//! mint or renew stamps its window from the real clock
//! (`fauna_client_pair`'s `TrustPlatform::now_epoch_secs`) — faking that would
//! deposit a future-dated window on the nest, a code path the nest has never
//! seen for real. Only the liveness and freshness reads are fake-clock-aware,
//! so a journey can reach `expiring soon` / `expired` (a ~90-day window) and a
//! stale receipt (a 48-hour window) without sleeping them out (`testing.md`
//! convention 14 — a fake clock, never a sleep). Both windows are hard-coded
//! Rust constants with no config surface, so a clock is the only way in.
//!
//! Mirrors `fauna_client_backup::audit_clock` and
//! `fauna_atproto_settings_machine::delegation_clock` in shape (same
//! `AtomicI64` + cfg gate + reset caveat) and stays its own static for the
//! reason those two do: one shared offset would let a trust-facet test
//! silently move an unrelated backup-audit or delegation assertion in the same
//! process.
//!
//! Spec: `docs/goal/ui/nests.md` § Expiry / renewal — first-class states and
//! § Trust facet — custody rows (the three-state receipt honesty).

/// Seconds added to the trust facet's render `now`, set by the
/// `trust_facet_advance_clock` agent command. Zero in every real run.
///
/// Compiled out of release artifacts (testing.md convention 15), like every
/// other automation hook. A consumer forwards its own opt-in feature to this
/// crate's `e2e-agent` — tui's is `e2e-agent`.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
static CLOCK_OFFSET_SECS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// The trust facet's clock offset, in seconds. The accessor pair is
/// byte-identical to its two siblings on purpose: the cfg gate names **this
/// crate's** `e2e-agent` feature, so a shared wrapper in a lower crate would
/// resolve it against the wrong crate's features.
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
/// test that leaves an offset behind lapses every grant the next test in the
/// same app process renders. Zero it once the lapse assertions are done.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn set_clock_offset_secs(offset: i64) {
    CLOCK_OFFSET_SECS.store(offset, std::sync::atomic::Ordering::SeqCst);
}

/// A real epoch-seconds `now` moved by the offset — what the grant-liveness
/// fold is judged against. The caller supplies the real clock (the trust
/// machine's platform seam, fakeable in unit tests), so this only adds.
pub fn render_now_secs(real_secs: u64) -> u64 {
    (real_secs as i64)
        .saturating_add(clock_offset_secs())
        .max(0) as u64
}

/// The same shift in epoch **microseconds** — what the custody receipt
/// freshness fold is judged against.
pub fn render_now_micros(real_micros: u64) -> u64 {
    let shift = clock_offset_secs().saturating_mul(1_000_000);
    (real_micros as i64).saturating_add(shift).max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test owns the process-wide offset, setting and zeroing it, so no
    /// sibling test in this crate can observe a leaked value.
    #[test]
    fn the_offset_moves_both_render_clocks_and_zero_restores_them() {
        set_clock_offset_secs(0);
        assert_eq!(render_now_secs(1_000), 1_000);
        assert_eq!(render_now_micros(5_000_000), 5_000_000);

        set_clock_offset_secs(91 * 24 * 60 * 60);
        assert_eq!(render_now_secs(1_000), 1_000 + 91 * 24 * 60 * 60);
        assert_eq!(
            render_now_micros(5_000_000),
            5_000_000 + 91 * 24 * 60 * 60 * 1_000_000
        );

        // A negative offset past the epoch clamps rather than wrapping.
        set_clock_offset_secs(-10_000);
        assert_eq!(render_now_secs(1_000), 0);

        set_clock_offset_secs(0);
        assert_eq!(render_now_secs(1_000), 1_000);
    }
}
