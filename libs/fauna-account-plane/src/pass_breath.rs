//! The pass breath — the scheduler yield every unit of local work inside a
//! pump pass ends in (`account-data-plane.md` § The client-side lifecycle,
//! the pump bullet → *Commands and passes*).
//!
//! The account runtime drives every pass beside its command channel: a local
//! command is served at the pass's next yield point, and a sign-out's cut
//! lands there too. A pass that is waiting on the network yields by itself.
//! A pass grinding through local work — a page of rows to ingest, a
//! generation key to re-derive, a wrap to re-seal — does not: every store
//! call is synchronous underneath its `async fn`, so a loop over such units
//! holds the store thread for the whole loop, and every command waits for
//! all of it. Measured on linux 2026-09-22: a prologue held the thread more
//! than four seconds past a sign-out's grace, uncut, and a preference read
//! waited out a 20 s budget behind it.
//!
//! So every loop over units of local work calls [`pass_breath`] once per
//! unit. The rule is the discipline, not the helper: a new pass step that
//! loops over rows without breathing re-creates the stall.

/// One scheduler yield. Returns `Pending` exactly once (waking itself), so a
/// test can count the breaths a pass takes by polling it by hand.
pub async fn pass_breath() {
    tokio::task::yield_now().await;
}
