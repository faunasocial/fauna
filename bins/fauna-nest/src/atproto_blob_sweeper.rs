//! Background sweep that retires unreferenced ATProto PDS uploads (F2.4
//! slice 4).
//!
//! `com.atproto.repo.uploadBlob` is a two-step affair: an external app uploads
//! bytes, gets a ref, and puts that ref in a record moments later. Between the
//! two the row in `atproto_blobs` is **transient upload state** — and an app
//! that uploads and then never posts (a cancelled compose, a crash, a retry
//! whose reply it never saw) leaves one behind forever. `atproto-pds-full.md`
//! § Nest state schema sanctions collecting exactly those: an unreferenced row
//! past the reference window is recreatable by re-upload, hence deletable under
//! the no-data-loss rule.
//!
//! **This sweep deletes rows, never bytes** — see
//! [`crate::db::CacheDb::delete_unreferenced_atproto_blobs`] for why that split
//! is what makes it both crash-safe and safe against content-address sharing.
//! Dropping the row is what lets the box-wide GC's reachability walk
//! (`crate::backup::gc`, step 2e) stop pinning the bytes; the reclaim itself is
//! that pass's, on its own cadence.

use std::sync::Arc;

use crate::routes::AppState;

/// **The reference window** — how long an *unreferenced* uploaded blob is kept
/// before its row is collectable.
///
/// A hard-coded Rust constant, deliberately: no user or admin ever has a reason
/// to choose it, so it is not a configuration surface
/// (`docs/goal/principles.md` — every value is either a constant or an app-UI
/// choice, never a knob).
///
/// **Seven days, and the asymmetry is the whole argument.** Over-retention
/// costs disk that the next sweep past the window reclaims anyway.
/// Under-retention destroys an upload a record was about to name — and the
/// record would then commit a ref only this PDS can serve and no longer can,
/// the dangling ref *store-then-reference* forbids. So the window is sized
/// against the *legitimate* upload→reference gap, which is seconds (an app
/// uploads each image as the user attaches it, then posts), and seven days
/// clears that by four orders of magnitude while still bounding what a
/// never-referencing caller can park on disk. It is not the defence against a
/// deliberate upload flood — the `blob` endpoint class's own rate limit and the
/// per-blob ceiling are (`atproto-pds-full.md` § F2 detail) — so there is
/// nothing to buy by shortening it.
pub const ATPROTO_BLOB_REFERENCE_WINDOW_MILLIS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Sweep cadence — hard-coded, same bucket as the window above. Hourly, and the
/// latency is immaterial in both directions: the rows are invisible to every
/// user-facing surface, and the bytes they pin are not reclaimed by this pass
/// anyway but by the box-wide GC's own six-hourly cycle.
const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Spawn the periodic unreferenced-upload sweep via the shared
/// [`crate::sweeper::spawn_periodic_sweeper`] primitive: a bare interval loop
/// whose first tick fires immediately (the boot pass), torn down at process
/// exit.
pub fn spawn_atproto_blob_sweeper(state: Arc<AppState>) {
    let scope = state.clone();
    scope.scope_handle(crate::sweeper::spawn_periodic_sweeper(
        SWEEP_INTERVAL,
        false,
        move || {
            let state = state.clone();
            async move {
                let cutoff = crate::db::now_epoch_millis() - ATPROTO_BLOB_REFERENCE_WINDOW_MILLIS;
                match state.db.delete_unreferenced_atproto_blobs(cutoff).await {
                    Ok(n) if n > 0 => tracing::info!(
                        swept = n,
                        window_millis = ATPROTO_BLOB_REFERENCE_WINDOW_MILLIS,
                        "atproto blob sweep: retired unreferenced upload row(s); their bytes \
                     become collectable at the next GC cycle"
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::error!("atproto blob sweep error: {e}"),
                }
            }
        },
    ));
}
