//! The audit's clock — the one `now` every freshness/overdue comparison is made
//! against, plus the e2e offset that lets a test move it.
//!
//! **Platform-neutral on purpose.** This started life inside
//! [`crate::native_store`], which is `cfg(not(target_arch = "wasm32"))` because
//! it holds a *filesystem* store. The clock has no filesystem in it — it is an
//! `AtomicI64` and one [`fauna_core::data::Timestamp::now_secs`] call, both of
//! which work on wasm32 (fauna-core's `js` time backend, forwarded by this
//! crate's wasm dependency block). Leaving it behind that cfg would have forced
//! web's `localStorage` store to reimplement the trio byte-for-byte — exactly
//! the duplication `native_store`'s own doc comment records itself as having
//! ended for linux and tui (priority #4: resolve drift, don't match it).
//!
//! Spec: `docs/goal/ui/backups.md` § Audit-alert surface;
//! `docs/goal/behavior/backup-restore.md` § Background Tasks owns the loop and
//! its ratified constants.

/// Seconds added to the audit's `now`, set by the `backup_audit_run_now` agent
/// command. Zero in every real run.
///
/// Compiled out of release artifacts (testing.md convention 15), like every
/// other automation hook. Each shell forwards its own opt-in feature to this
/// crate's `e2e-agent` — natively that is `e2e-agent`, on web `fauna-wasm`'s
/// `test-helpers`.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
static CLOCK_OFFSET_SECS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// The audit's clock offset. **Only time is fakeable, and deliberately so:** the
/// freshness verdict floors a destination's high-water at its `added_at`, so a
/// destination enrolled seconds ago cannot be stale in real time — correct
/// behavior, and it means a staleness proof has to move the clock rather than
/// sleep out a 48-hour window (testing.md convention 14). Everything else in
/// the pass stays real: the connection to the destination, its `custody.list`
/// reply, and the client's own persisted observation high-water. Nothing about
/// the *finding* is injectable.
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

/// Set the offset above. ⚠ **Process-wide and nothing auto-resets it** — a test
/// that leaves an offset behind silently subtracts from the next audit test's
/// elapsed time (it cost one run: a leftover 25 h turned an intended 8 d into
/// 167 h, one hour under `AUDIT_OVERDUE`). Zero it at test start *and* end.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn set_clock_offset_secs(offset: i64) {
    CLOCK_OFFSET_SECS.store(offset, std::sync::atomic::Ordering::SeqCst);
}

/// Unix seconds, the clock every audit comparison is made against — plus the
/// e2e offset above, which is zero in every real run.
pub fn now_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs().saturating_add(clock_offset_secs())
}

/// The same instant in epoch **milliseconds**, for the label side
/// (`fauna_core::format::backup_last_audit_label` takes `now_ms`). Kept beside
/// [`now_secs`] so a shell cannot render a row against a different clock than
/// the verdict it is rendering was reached on.
pub fn now_ms() -> i64 {
    now_secs().saturating_mul(1_000)
}
