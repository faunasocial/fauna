//! Compaction worker for the message-segment store.
//!
//! Reads per-actor-per-kind tombstone fractions from `segment_records`,
//! consults the snapshot pin set (active manifest + every pinned-snapshot
//! manifest for that actor+kind), and rewrites eligible buckets via
//! [`fauna_segment_store::compact`]. The output segment is written by
//! `segments::mail::compact_bucket`, which lands the manifest swap
//! and the `segment_records` tombstone-and-INSERT in one SQLite
//! transaction.
//!
//! ### Read-your-own-writes discipline
//!
//! Compaction opens segments directly via `FramedSegment::read_record`
//! (through `fauna_segment_store::compact`). Per Plan-2 T9, the
//! centralised flush-on-read policy lives in `SegmentManager`
//! — any direct segment reader must call
//! `SegmentManager::finalize_open(actor_id)` first to flush any
//! pending appends. `compact_bucket` already does that before opening
//! anything, but the worker also calls it once per (actor, kind) pass
//! so any post-compact reads in the same loop iteration see consistent
//! state.

use anyhow::{Context, Result};
use fauna_segment_store::{CompactionPlan, PinSet, SegmentStats, pick_compaction_inputs};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;

use crate::routes::AppState;

/// What to compact. `None` for either field = "all".
#[derive(Debug, Clone, Default)]
pub struct CompactScope {
    pub kind: Option<String>, // 'mail' (Plan 3 ships only mail); future kinds add their own
    pub scope_id: Option<[u8; 32]>,
}

#[derive(Debug, Clone, Copy)]
pub enum CompactionTrigger {
    /// 6h scheduled pass; threshold gate at 25 % tombstones (spec D7).
    Scheduled,
    /// Admin-initiated pass via the `fauna.segments.compact` WS-RPC
    /// kind; threshold drops to 0 % (spec D10).
    Manual,
}

impl CompactionTrigger {
    fn threshold(self) -> f32 {
        match self {
            CompactionTrigger::Scheduled => 0.25,
            CompactionTrigger::Manual => 0.0,
        }
    }
}

pub struct CompactionWorker {
    state: Arc<AppState>,
    interval: Duration,
}

impl CompactionWorker {
    pub fn new(state: Arc<AppState>, interval: Duration) -> Self {
        Self { state, interval }
    }

    /// Construct a worker for a one-shot manual-compact call (spec D10).
    /// `interval` is unused by `run_once`; this constructor makes the intent
    /// explicit and avoids a magic constant at the call site.
    pub fn for_manual(state: Arc<AppState>) -> Self {
        Self {
            state,
            interval: Duration::ZERO,
        }
    }

    /// Spawn the scheduled loop. Returns a handle the caller can store;
    /// the worker runs for the lifetime of the spawned task (the
    /// existing snapshot scheduler pattern intentionally drops its
    /// handle).
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        // spawn-ok(returns-handle-for-scope): caller adopts via `AppState::scope_handle`
        tokio::spawn(async move {
            let mut ticker = interval(self.interval);
            loop {
                ticker.tick().await;
                let scope = CompactScope::default();
                match self.run_once(scope, CompactionTrigger::Scheduled).await {
                    Ok(report) => {
                        if !report.acquired_lock {
                            tracing::debug!(
                                "mail compaction tick: skipped — gc lock held by another holder"
                            );
                        } else if report.segments_rewritten > 0 || report.errors > 0 {
                            tracing::info!(
                                "mail compaction tick: rewritten={} errors={}",
                                report.segments_rewritten,
                                report.errors,
                            );
                        }
                    }
                    Err(e) => tracing::warn!("mail compaction run failed: {e:#}"),
                }
            }
        })
    }

    /// One pass over the requested scope. Acquires the snapshot-GC
    /// advisory lock (`"gc"`) so compaction and snapshot-GC never race
    /// (spec D7). Returns a report with the action counts; if another
    /// holder owns the `"gc"` lock, returns immediately with
    /// `acquired_lock = false`.
    pub async fn run_once(
        &self,
        scope: CompactScope,
        trigger: CompactionTrigger,
    ) -> Result<CompactionReport> {
        // Unique holder per call: process id + a 64-bit random nonce so
        // parallel manual triggers (T4 endpoint) don't collide.
        let holder = format!(
            "mail-compaction-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        );
        let lock_acquired = self
            .state
            .db
            .try_acquire_op_lock("gc", -1, &holder)
            .await
            .context("acquire snapshot-GC advisory lock")?;
        if !lock_acquired {
            return Ok(CompactionReport {
                acquired_lock: false,
                ..Default::default()
            });
        }

        let mut report = CompactionReport {
            acquired_lock: true,
            ..Default::default()
        };

        let actors = self
            .resolve_actors(&scope)
            .await
            .context("resolve actors for compaction")?;

        for actor_id in actors {
            for kind in self.resolve_kinds(&scope) {
                // Skip pure-backup destinations per (scope, kind): a
                // `__<kind>` (mail/post) / `__conv/<channel>` (conv) reserved
                // custody-copy set holds opaque chunks only — no local
                // plaintext-framed segments — so compaction would fail with a
                // misleading I/O error. Gating here, rather than in
                // `resolve_actors`, makes it per-(scope, kind): the whole-nest
                // scheduled default runs every kind `resolve_kinds` returns
                // over every scope, and a nest can be a pure-backup
                // destination for one kind/scope while still serving the
                // kinds/scopes it holds locally. (Manual scoped requests hit
                // the same predicate at the route's 409 gate,
                // `compact_handler.rs`, before the worker runs; this is the
                // scheduled-path enforcement + a defensive backstop.)
                if self
                    .state
                    .db
                    .is_pure_backup_destination(&kind, &actor_id)
                    .await
                    .context("is_pure_backup_destination")?
                {
                    tracing::debug!(
                        "compaction: skipping pure-backup destination scope=0x{} kind={kind}",
                        hex::encode(actor_id)
                    );
                    continue;
                }
                match self.compact_actor_kind(&actor_id, &kind, trigger).await {
                    Ok(n) => report.segments_rewritten += n,
                    Err(e) => {
                        tracing::warn!(
                            "compaction failed for actor=0x{} kind={kind}: {e:#}",
                            hex::encode(actor_id)
                        );
                        report.errors += 1;
                    }
                }
            }
        }

        let _ = self.state.db.release_op_lock("gc", -1).await;
        Ok(report)
    }

    /// Enumerate the scopes a compaction pass touches. A scoped request
    /// (`scope_id = Some`) returns just that scope; the whole-nest scheduled
    /// pass (`scope_id = None`) returns every scope with `segment_records`
    /// rows for `scope.kind` (or all kinds when `kind = None`).
    ///
    /// The pure-backup filter is **not** applied here — it is per-(scope, kind)
    /// at the `run_once` loop, because the whole-nest pass compacts each scope
    /// against multiple kinds and a scope's pure-backup status is per-kind
    /// (`is_pure_backup_destination(kind, scope)`). Filtering once here with a
    /// single kind would leave a misconfigured pure-backup *conv* channel (or
    /// vice-versa) undefended.
    async fn resolve_actors(&self, scope: &CompactScope) -> Result<Vec<[u8; 32]>> {
        if let Some(a) = scope.scope_id {
            return Ok(vec![a]);
        }
        self.state
            .db
            .segment_records_list_scopes(scope.kind.as_deref())
            .await
            .context("list scopes with segment_records rows")
    }

    fn resolve_kinds(&self, scope: &CompactScope) -> Vec<String> {
        match &scope.kind {
            Some(k) => vec![k.clone()],
            // Whole-nest scheduled default. Every kind on the segment store gets
            // a scheduled pass so acked/tombstoned disk is reclaimed for mail,
            // conv, post (Plan 8 / Track C posts) *and* — since S6.8c — the two
            // DAV content kinds. `segment_records_list_scopes(Some(kind))`
            // enumerates that kind's scopes (kind-parameterized) — actors for
            // mail/post/calendar/card, channels for conv.
            None => vec![
                "mail".to_string(),
                "conv".to_string(),
                "post".to_string(),
                "calendar".to_string(),
                "card".to_string(),
            ],
        }
    }

    /// Map a segment-store kind to the `SegmentManager` that owns its
    /// on-disk segments. The whole worker is kind-table-driven through this one
    /// helper so a new kind only needs an arm here plus a `rewrite_bucket` arm.
    fn segments_for_kind(&self, kind: &str) -> Option<&Arc<fauna_segment_store::SegmentManager>> {
        match kind {
            "mail" => Some(&self.state.mail_segments),
            "conv" => Some(&self.state.conv_segments),
            "post" => Some(&self.state.post_segments),
            "calendar" => Some(&self.state.cal_segments),
            "card" => Some(&self.state.card_segments),
            _ => None,
        }
    }

    /// Tombstone the kind's orphaned mirror rows before compacting it (S6.8b) —
    /// records left unreachable by a rejected PUT, a crash between the append
    /// and the row INSERT. They are live to
    /// compaction until tombstoned, so a rewrite would otherwise copy them
    /// forward forever.
    ///
    /// Runs under the `"gc"` op lock this worker holds, and only for kinds the
    /// `run_once` loop has already cleared as *not* pure-backup destinations —
    /// both load-bearing: on a nest whose metadata rows are absent or not yet
    /// rebuilt, every mirror row looks orphaned. (The reaper carries its own
    /// fail-closed guard for that case too; this is defense in depth.)
    async fn reap_orphans(&self, actor_id: &[u8; 32], kind: &str) -> Result<u32> {
        match kind {
            "calendar" => crate::segments::cal::reap_orphan_records(
                &self.state.db,
                actor_id,
                crate::db::now_epoch_secs(),
            )
            .await
            .context("reap orphan calendar records"),
            "card" => crate::segments::card::reap_orphan_records(
                &self.state.db,
                actor_id,
                crate::db::now_epoch_secs(),
            )
            .await
            .context("reap orphan card records"),
            // Mail's orphans are headless continuation parts (crash between parts
            // and head, or a fanned-out delete that missed a part). The grace ages
            // on the floor's `stored_at` (when THIS nest stored the part), which is
            // in MILLISECONDS — so pass the ms clock, unlike cal/card's seconds
            // (message-segment-store.md § Continuation records).
            "mail" => crate::segments::mail::reap_headless_parts(
                &self.state.mail_segments,
                &self.state.db,
                actor_id,
                crate::db::now_epoch_millis(),
            )
            .await
            .context("reap headless continuation parts"),
            _ => Ok(0),
        }
    }

    async fn compact_actor_kind(
        &self,
        actor_id: &[u8; 32],
        kind: &str,
        trigger: CompactionTrigger,
    ) -> Result<u32> {
        let Some(mgr) = self.segments_for_kind(kind) else {
            tracing::debug!("kind {kind} not yet implemented in compaction");
            return Ok(0);
        };

        // S6.8b, before the pin-set + bucket walk: turn this kind's unreachable
        // records into tombstones, so the pass below can actually reclaim them.
        // A no-op for kinds whose writers already tombstone at the row DELETE.
        self.reap_orphans(actor_id, kind).await?;

        // Plan-2 T9 discipline: anything reading segments directly must
        // finalize the open segment first. `compact_bucket` repeats this
        // under its per-actor lock for safety; doing it here too keeps
        // the pin-set's manifest read consistent with on-disk state.
        mgr.finalize_open(actor_id)
            .await
            .context("finalize open segment before compaction sweep")?;

        let pin_set = self.build_pin_set(actor_id, kind).await?;
        let buckets = self
            .state
            .db
            .segment_records_list_buckets_with_tombstones(actor_id, kind)
            .await?;
        let threshold = trigger.threshold();
        let mut rewritten = 0u32;
        for bucket in buckets {
            let stats_rows = self
                .state
                .db
                .segment_records_count_segment_stats(actor_id, kind, &bucket)
                .await?;
            // Map the DAO row → the crate's input shape (the two share the
            // same `segment_id` / `record_count` / `tombstone_count` fields).
            let segment_stats: Vec<SegmentStats> = stats_rows
                .iter()
                .map(|r| SegmentStats {
                    segment_id: r.segment_id,
                    record_count: r.record_count,
                    tombstone_count: r.tombstone_count,
                })
                .collect();
            let plan = match pick_compaction_inputs(&segment_stats, &bucket, threshold, &pin_set) {
                Some(p) => p,
                None => continue,
            };
            let outcome = self
                .rewrite_bucket(actor_id, kind, &plan)
                .await
                .context("rewrite_bucket")?;
            // Plan 5 T6: emit `fauna.segments.changed` pushes for every
            // segment-id transition this compaction produced. The data
            // owner's custodian pull uses CompactedIn to wake on
            // the new segment and CompactedOut on the inputs so its
            // local backup state can drop the now-tombstoned ids.
            self.emit_compaction_pushes(actor_id, kind, &outcome);
            if outcome.new_segment.is_some() {
                rewritten += 1;
            }
        }
        Ok(rewritten)
    }

    /// Emit one `fauna.segments.changed` push per segment-id transition
    /// produced by a compaction pass:
    /// - `CompactedIn` for the new segment (if any survivors).
    /// - `CompactedOut` for every consumed input segment.
    ///
    /// Pushes go through `WsState::notify_push`, which is a no-op when
    /// the actor has no active WS connections — safe to call
    /// unconditionally; the per-connection emit is gated inside.
    fn emit_compaction_pushes(
        &self,
        actor_id: &[u8; 32],
        kind: &str,
        outcome: &crate::segments::BucketCompactionOutcome,
    ) {
        use fauna_protocol::push_events::SegmentChange;
        if let Some(new_id) = outcome.new_segment {
            crate::segments::notify_segments_changed(
                &self.state.ws,
                actor_id,
                kind,
                new_id,
                SegmentChange::CompactedIn,
            );
        }
        for consumed in &outcome.consumed {
            crate::segments::notify_segments_changed(
                &self.state.ws,
                actor_id,
                kind,
                *consumed,
                SegmentChange::CompactedOut,
            );
        }
    }

    /// Build the compaction PinSet — the union of segment IDs that
    /// `pick_compaction_inputs` must filter out before applying its
    /// tombstone-fraction gate. Composition:
    ///
    /// 1. **Active manifest's `tombstoned_segments`.** These are
    ///    segments compaction already rewrote in an earlier pass; they
    ///    are kept on disk for the 14 d tombstoned-retention window
    ///    before fauna-sync GC reclaims their chunks. Re-feeding them
    ///    as compaction inputs would be wasteful (every record on them
    ///    is already tombstoned in `segment_records`, so `compact()`
    ///    returns `None` — but the bucket-iteration loop still pays
    ///    one segment-open + walk per stale tombstoned segment).
    /// 2. **Every pinned snapshot's manifest's `live_segments` +
    ///    `tombstoned_segments`.** Snapshots pin the bytes of every
    ///    segment their manifest references; compacting any of those
    ///    out would corrupt the snapshot.
    ///
    /// **What is intentionally *not* in the pin set:** the active
    /// manifest's `live_segments`. Those *are* the compaction
    /// candidates — they're the only segments compaction can pick up
    /// and rewrite. After compaction, they move from `live_segments`
    /// → `tombstoned_segments` and become pinned for the retention
    /// window via (1).
    ///
    /// (The goal-doc text in
    /// `docs/goal/architecture/message-segment-store.md` § Pin set for
    /// compaction GC at the time this code lands says "active
    /// manifest's `live_segments` + `tombstoned_segments`" — a
    /// transcription error from the design spec § D8 which means
    /// "the union of segments referenced by any active or pinned
    /// manifest is not eligible for **physical delete**." The
    /// pick-inputs-for-compaction gate is a strictly narrower
    /// question and excludes the active manifest's live segments by
    /// construction. The goal doc is updated in the same commit that
    /// lands this worker; see the commit body for details.)
    async fn build_pin_set(&self, actor_id: &[u8; 32], kind: &str) -> Result<PinSet> {
        use fauna_segment_store::KindManifest;
        let mut pin = PinSet::new();
        let active = self
            .segments_for_kind(kind)
            .expect("compact_actor_kind already gated kind")
            .load_manifest(actor_id)
            .await
            .context("load active Manifest")?;
        // Only the active manifest's tombstoned segments are pinned for
        // compaction (per the rationale above). Construct a temporary
        // KindManifest exposing only the tombstoned ids and extend
        // from that.
        let tombstoned_view = KindManifest {
            next_seg_id: active.kind_manifest.next_seg_id,
            live_segments: Vec::new(),
            tombstoned_segments: active.kind_manifest.tombstoned_segments.clone(),
        };
        pin.extend_from_manifest(&tombstoned_view);

        let pinned_blobs = self
            .state
            .db
            .list_active_message_kind_snapshot_manifests(actor_id, kind)
            .await
            .context("list active message-kind snapshot manifests")?;
        for blob in pinned_blobs {
            let m: fauna_segment_store::Manifest = fauna_core::encoding::canonical_decode(&blob)
                .map_err(|e| anyhow::anyhow!("decode pinned Manifest: {e}"))?;
            pin.extend_from_manifest(&m.kind_manifest);
        }
        Ok(pin)
    }

    async fn rewrite_bucket(
        &self,
        actor_id: &[u8; 32],
        kind: &str,
        plan: &CompactionPlan,
    ) -> Result<crate::segments::BucketCompactionOutcome> {
        // Dispatch to the kind's per-kind compaction coordinator. Conv's and
        // post's `compact_bucket` have no `kind` param (their stores are
        // single-kind); mail's carries `kind` for its mirror-rebuild SQL.
        match kind {
            "mail" => {
                crate::segments::mail::compact_bucket(
                    &self.state.mail_segments,
                    &self.state.db,
                    actor_id,
                    kind,
                    plan,
                )
                .await
            }
            "conv" => {
                crate::segments::conv::compact_bucket(
                    &self.state.conv_segments,
                    &self.state.db,
                    actor_id,
                    plan,
                )
                .await
            }
            "post" => {
                crate::segments::post::compact_bucket(
                    &self.state.post_segments,
                    &self.state.db,
                    actor_id,
                    plan,
                )
                .await
            }
            "calendar" => {
                crate::segments::cal::compact_bucket(
                    &self.state.cal_segments,
                    &self.state.db,
                    actor_id,
                    plan,
                )
                .await
            }
            "card" => {
                crate::segments::card::compact_bucket(
                    &self.state.card_segments,
                    &self.state.db,
                    actor_id,
                    plan,
                )
                .await
            }
            other => Err(anyhow::anyhow!(
                "rewrite_bucket called for unsupported kind {other:?} \
                 (compact_actor_kind should have gated this via segments_for_kind)"
            )),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CompactionReport {
    pub acquired_lock: bool,
    pub segments_rewritten: u32,
    pub errors: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use crate::routes::AppState;
    use crate::segments::test_helpers::floor;
    use crate::segments::{CalPlacementSegmentManager, MailPlacementSegmentManager};
    use fauna_segment_store::SegmentManager;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Build a minimal AppState for compaction tests. Returns
    /// `(tempdir, state)`. Drop the tempdir last so segment files
    /// outlive `state.mail_segments`.
    fn build_state() -> (TempDir, Arc<AppState>) {
        let tmp = TempDir::new().expect("tempdir");
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        let mut state = AppState::for_test(db.clone());
        // Override the default test mail_segments path so segments live
        // in our tempdir (the default uses std::env::temp_dir() which
        // tests can collide on).
        state.mail_segments = Arc::new(SegmentManager::new(tmp.path().to_path_buf(), "mail"));
        // Conv segments share the same tempdir (kind = "conv" → `__conv/...`
        // subtree); without this override `conv_segments` keeps its default
        // `fauna-test-conv-{pid}` dir, which is PID-shared across tests in the
        // binary and would collide on a reused channel id.
        state.conv_segments = Arc::new(SegmentManager::new(tmp.path().to_path_buf(), "conv"));
        // Same reason for the two DAV content kinds, live in compaction since
        // S6.8c: without the override they keep a PID-shared default dir.
        state.cal_segments = Arc::new(SegmentManager::new(tmp.path().to_path_buf(), "calendar"));
        state.card_segments = Arc::new(SegmentManager::new(tmp.path().to_path_buf(), "card"));
        state.mail_placement = Arc::new(MailPlacementSegmentManager::new(tmp.path().to_path_buf()));
        state.cal_placement = Arc::new(CalPlacementSegmentManager::new(tmp.path().to_path_buf()));
        (tmp, Arc::new(state))
    }

    /// Tombstone every `segment_records` row for one (scope, segment).
    async fn tombstone_segment(db: &CacheDb, actor: &[u8; 32], seg_id: u32) {
        let conn = db.conn().await;
        conn.execute(
            "UPDATE segment_records SET tombstoned = 1
             WHERE scope_id = ?1 AND segment_id = ?2",
            rusqlite::params![actor.as_slice(), seg_id as i64],
        )
        .expect("tombstone");
    }

    async fn count_live(db: &CacheDb, actor: &[u8; 32], seg_id: u32) -> i64 {
        let conn = db.conn().await;
        conn.query_row(
            "SELECT COUNT(*) FROM segment_records
             WHERE scope_id = ?1 AND segment_id = ?2 AND tombstoned = 0",
            rusqlite::params![actor.as_slice(), seg_id as i64],
            |r| r.get(0),
        )
        .unwrap()
    }

    async fn count_tombstoned(db: &CacheDb, actor: &[u8; 32], seg_id: u32) -> i64 {
        let conn = db.conn().await;
        conn.query_row(
            "SELECT COUNT(*) FROM segment_records
             WHERE scope_id = ?1 AND segment_id = ?2 AND tombstoned = 1",
            rusqlite::params![actor.as_slice(), seg_id as i64],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Helper: append `n` records into the actor's mail segment store
    /// in a fixed bucket (controlled by the floor's `received_at`).
    async fn append_n(
        state: &Arc<AppState>,
        actor: &[u8; 32],
        first_record_byte: u8,
        n: usize,
        received_at: i64,
    ) -> Vec<[u8; 32]> {
        let mut rids = Vec::with_capacity(n);
        for i in 0..n {
            // Unique body per record (identity is the content hash — identical
            // bytes would dedup); the returned rids are the DERIVED digests.
            let outcome = crate::segments::mail::append_record(
                &state.mail_segments,
                &state.db,
                actor,
                &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                    format!("sealed-body-{first_record_byte}-{i}").into_bytes(),
                ),
                &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                    b"sealed-hint".to_vec(),
                ),
                floor(received_at),
            )
            .await
            .expect("append_record");
            rids.push(outcome.cid.digest());
        }
        rids
    }

    /// Insert a folders row mapping the actor to a synthetic folder
    /// id; returns that id. Synchronous helper renamed to async to
    /// avoid `conn_blocking()` (which panics inside a tokio runtime).
    async fn create_folder(db: &CacheDb, actor: &[u8; 32], name: &str) -> i64 {
        let conn = db.conn().await;
        conn.execute(
            "INSERT INTO folders (name, actor_id, created_at) VALUES (?1, ?2, 0)",
            rusqlite::params![name, actor.as_slice()],
        )
        .expect("insert folders");
        conn.last_insert_rowid()
    }

    /// Insert a snapshots row pinning `manifest_blob` for the
    /// `(folder, kind)` pair. `kind` is `"mail"` or `"conv"` — the pin-set
    /// query (`list_active_message_kind_snapshot_manifests`) filters on
    /// `message_kind`, so the conv pinned-survival test passes `"conv"`.
    async fn pin_manifest(db: &CacheDb, folder_id: i64, kind: &str, manifest_blob: &[u8]) {
        let conn = db.conn().await;
        conn.execute(
            "INSERT INTO snapshots
                (folder_id, created_at, file_count, total_bytes,
                 message_kind, message_manifest, deletion_pending, soft_deleted)
             VALUES (?1, 0, 0, 0, ?2, ?3, 0, 0)",
            rusqlite::params![folder_id, kind, manifest_blob],
        )
        .expect("insert snapshots");
    }

    /// Build a `pin_set` indirectly via the worker's public surface
    /// and verify it contains the active manifest's *tombstoned* ids
    /// (NOT live — those are compaction candidates) plus every pinned
    /// snapshot's manifest's segment ids.
    #[tokio::test]
    async fn pin_set_excludes_active_live_and_includes_active_tombstoned_and_snapshot_segments() {
        let (_tmp, state) = build_state();
        let actor = [0x12u8; 32];

        // Append 3 records, finalize. Then more — bucket changes
        // rotate seg 1 → seg 2.
        let _ = append_n(&state, &actor, 1, 3, 1_715_000_000_000).await;
        state
            .mail_segments
            .finalize_open(&actor)
            .await
            .expect("flush");
        let _ = append_n(&state, &actor, 10, 3, 1_718_000_000_000).await;
        state
            .mail_segments
            .finalize_open(&actor)
            .await
            .expect("flush");
        // active manifest now has live = [1, 2].

        // Tombstone seg 1 by running a compaction that drops all its records
        // (all rows in segment_records for seg 1 are tombstoned → alive-set
        // is empty → compact_bucket writes no new segment → seg 1 moves to
        // tombstoned in manifest). This goes through the official code path
        // rather than an external save_atomic write, so it's immune to any
        // OS-level filesystem visibility races between concurrent tests.
        tombstone_segment(&state.db, &actor, 1).await;
        let bucket_1 = fauna_mail::segments::bucket_for(1_715_000_000_000i64 / 1000);
        let plan = CompactionPlan {
            bucket: bucket_1.clone(),
            inputs: vec![1],
        };
        crate::segments::mail::compact_bucket(
            &state.mail_segments,
            &state.db,
            &actor,
            "mail",
            &plan,
        )
        .await
        .expect("compact seg 1 — should tombstone since all rows are dead");
        // Active manifest now: live = [2], tombstoned = [1].

        // Build a synthetic pinned snapshot whose manifest's only
        // segment id is 99 (NOT 2 — we want to assert seg 2 stays
        // unpinned even with a snapshot in play, and 99 proves the
        // snapshot's manifest is being read).
        // Note: pinned blobs are encoded as `fauna_segment_store::Manifest`
        // (not the legacy `MailManifest`) — same format build_pin_set decodes.
        let mut pinned = fauna_segment_store::Manifest::empty("mail");
        // Bump next_seg_id to 99 and append once → live = [99].
        pinned.kind_manifest.next_seg_id = 99;
        pinned.kind_manifest.append_segment(); // now live = [99]
        let blob = fauna_core::encoding::canonical_encode(&pinned).expect("encode");
        let fs = create_folder(&state.db, &actor, "pin_set_includes_test").await;
        pin_manifest(&state.db, fs, "mail", &blob).await;

        let worker = CompactionWorker::new(state.clone(), Duration::from_secs(60));
        let pin = worker.build_pin_set(&actor, "mail").await.expect("pin");
        assert!(pin.contains(1), "active manifest tombstoned seg 1");
        assert!(
            !pin.contains(2),
            "active manifest LIVE seg 2 must NOT be pinned (it's a compaction candidate)"
        );
        assert!(pin.contains(99), "pinned snapshot seg 99");
    }

    /// A segment that is pinned by an active snapshot must survive a
    /// manual compaction pass even when every one of its records is
    /// tombstoned.
    #[tokio::test]
    async fn pinned_segment_survives_manual_compact() {
        let (_tmp, state) = build_state();
        let actor = [0x33u8; 32];

        // Seg 1: 3 records in bucket "2024-04" (received_at chosen for
        // that bucket). Finalize. Then a row in a different bucket so
        // seg 1 stops being the open segment.
        let _ = append_n(&state, &actor, 1, 3, 1_712_000_000_000).await;
        state
            .mail_segments
            .finalize_open(&actor)
            .await
            .expect("flush");
        // Tombstone every row pointing at seg 1.
        tombstone_segment(&state.db, &actor, 1).await;
        assert_eq!(count_live(&state.db, &actor, 1).await, 0);
        assert_eq!(count_tombstoned(&state.db, &actor, 1).await, 3);

        // Pin via a snapshot whose pinned manifest includes seg 1 as
        // tombstoned. (A real snapshot would have captured seg 1 in
        // its live_segments at the snapshot-create instant; either
        // shape lands seg 1 in the PinSet.)
        let pinned_manifest = state
            .mail_segments
            .load_manifest(&actor)
            .await
            .expect("load");
        let blob = fauna_core::encoding::canonical_encode(&pinned_manifest).expect("encode");
        let fs = create_folder(&state.db, &actor, "pinned_segment_survives_test").await;
        pin_manifest(&state.db, fs, "mail", &blob).await;

        let worker = CompactionWorker::new(state.clone(), Duration::from_secs(60));
        let report = worker
            .run_once(
                CompactScope {
                    kind: Some("mail".into()),
                    scope_id: Some(actor),
                },
                CompactionTrigger::Manual,
            )
            .await
            .expect("run_once");
        assert!(report.acquired_lock, "should acquire gc lock");
        assert_eq!(report.errors, 0);
        assert_eq!(
            report.segments_rewritten, 0,
            "pinned seg must not be rewritten"
        );

        // Seg 1 still present on disk + manifest still says live.
        let post = state
            .mail_segments
            .load_manifest(&actor)
            .await
            .expect("load");
        assert!(
            post.kind_manifest.live_segments.contains(&1),
            "seg 1 still live"
        );
        // And the segment_records rows for seg 1 still exist (tombstoned).
        assert_eq!(count_tombstoned(&state.db, &actor, 1).await, 3);
    }

    /// An unpinned segment whose tombstone fraction crosses the Manual
    /// threshold (0 %) is rewritten: the tombstoned rows survive on the
    /// old segment_records ids (as tombstones) and live rows reappear
    /// on the new segment id.
    #[tokio::test]
    async fn unpinned_tombstoned_segment_rewrites_under_manual_trigger() {
        let (_tmp, state) = build_state();
        let actor = [0x44u8; 32];

        // Seg 1: 5 records, tombstone the first 2. No pinned snapshot.
        let rids = append_n(&state, &actor, 1, 5, 1_712_000_000_000).await;
        state
            .mail_segments
            .finalize_open(&actor)
            .await
            .expect("flush");
        for rid in &rids[..2] {
            state
                .db
                .segment_records_mark_tombstoned(
                    &actor,
                    "mail",
                    1,
                    &fauna_cbor::Cid::from_digest_dag_cbor(*rid),
                )
                .await
                .expect("mark tombstoned");
        }
        assert_eq!(count_live(&state.db, &actor, 1).await, 3);
        assert_eq!(count_tombstoned(&state.db, &actor, 1).await, 2);

        let worker = CompactionWorker::new(state.clone(), Duration::from_secs(60));
        let report = worker
            .run_once(
                CompactScope {
                    kind: Some("mail".into()),
                    scope_id: Some(actor),
                },
                CompactionTrigger::Manual,
            )
            .await
            .expect("run_once");
        assert!(report.acquired_lock);
        assert_eq!(report.errors, 0);
        assert_eq!(report.segments_rewritten, 1, "one segment rewritten");

        // After compaction:
        //   - Manifest: seg 1 tombstoned, seg 2 live.
        //   - segment_records: seg 1 rows all tombstoned; seg 2 has 3 live rows.
        let post = state
            .mail_segments
            .load_manifest(&actor)
            .await
            .expect("load");
        assert_eq!(post.kind_manifest.live_segments, vec![2]);
        assert_eq!(post.kind_manifest.tombstoned_segments, vec![1]);

        assert_eq!(count_live(&state.db, &actor, 1).await, 0);
        assert_eq!(count_tombstoned(&state.db, &actor, 1).await, 5);
        assert_eq!(count_live(&state.db, &actor, 2).await, 3);
        assert_eq!(count_tombstoned(&state.db, &actor, 2).await, 0);
    }

    /// **Relay-ack purge → compaction reclaim (Slice 1 T6).** The mail relay
    /// acks by calling `segments::mail::tombstone_up_to_seq` (the
    /// `mail_ack_handler` path) — the SAME `tombstoned` column the
    /// CompactionWorker reclaims, via a different entry point than the direct
    /// `mark_tombstoned` the other tests use. After the ack tombstones every
    /// record, a manual compaction pass physically rewrites the bucket away:
    /// the public relay nest holds no *persistent* copy, not just no readable
    /// one. (T7 covers the readable-copy half over the channel; this closes the
    /// physical-reclaim half on the public nest.)
    #[tokio::test]
    async fn relay_ack_tombstones_are_reclaimed_by_compaction() {
        let (_tmp, state) = build_state();
        let actor = [0x71u8; 32];

        // Four inbound records land for the actor in one bucket (seg 1).
        let _ = append_n(&state, &actor, 1, 4, 1_712_000_000_000).await;
        state
            .mail_segments
            .finalize_open(&actor)
            .await
            .expect("flush");
        assert_eq!(count_live(&state.db, &actor, 1).await, 4);

        // The private nest acks up to the highest seq (4) — the relay-ack purge
        // path. Tombstones all four via the shared `tombstoned` column.
        let purged = crate::segments::mail::tombstone_up_to_seq(&state.db, &actor, 4)
            .await
            .expect("relay-ack tombstone");
        assert_eq!(purged, 4);
        assert_eq!(count_live(&state.db, &actor, 1).await, 0);

        // No readable copy: the relay cursor sees nothing post-ack.
        assert!(
            crate::segments::mail::read_after_seq(&state.mail_segments, &state.db, &actor, 0, 100)
                .await
                .expect("read_after_seq")
                .is_empty(),
            "public nest holds no readable mail after ack"
        );

        // Manual compaction (0 % threshold) physically reclaims the
        // all-tombstoned segment: no survivor segment is written and seg 1 moves
        // to `tombstoned_segments` (then fauna-sync GC drops its chunks after the
        // retention window).
        let worker = CompactionWorker::for_manual(state.clone());
        let report = worker
            .run_once(
                CompactScope {
                    kind: Some("mail".into()),
                    scope_id: Some(actor),
                },
                CompactionTrigger::Manual,
            )
            .await
            .expect("run_once");
        assert_eq!(report.errors, 0);
        assert_eq!(
            report.segments_rewritten, 0,
            "an all-tombstoned bucket writes no survivor segment"
        );

        let post = state
            .mail_segments
            .load_manifest(&actor)
            .await
            .expect("load");
        assert!(
            post.kind_manifest.live_segments.is_empty(),
            "no live mail segments remain on the public relay nest"
        );
        assert!(
            post.kind_manifest.tombstoned_segments.contains(&1),
            "the acked segment is reclaimed → tombstoned"
        );
    }

    // ---- S6.8b + S6.8c: calendar reclaim ---------------------------------

    /// A timestamp comfortably older than the orphan reaper's age watermark
    /// (the worker reads the real clock).
    const CAL_T: i64 = 1_712_000_000;

    fn cal_sealed(plaintext: &[u8]) -> fauna_mls::wrapped_blob::SealedRecordBytes {
        let (_sec, pubkey) = fauna_mls::wrapped_blob::derive_recipient_hpke_keypair(&[0x5Eu8; 32]);
        let bytes =
            crate::bridge_routing_handlers::seal_recipient_blob(plaintext, &pubkey, None, "body")
                .expect("seal");
        fauna_mls::wrapped_blob::SealedRecordBytes::verify(bytes).expect("verifies")
    }

    fn cal_floor(event_id: [u8; 32]) -> fauna_calendar::segments::CalFloorMetadata {
        fauna_calendar::segments::CalFloorMetadata {
            calendar_id: [0x11u8; 32],
            event_id,
            uid_hash: vec![0x33u8; 32],
            ciphertext_size: 11,
            internal_date: CAL_T,
            created_at: CAL_T,
            ..Default::default()
        }
    }

    /// Append one calendar content record for `event_id`, returning its
    /// content-derived cid. Body and hint are passed already-sealed:
    /// `cal_sealed` randomizes (HPKE encapsulation), so a test asserting
    /// byte-identity — or needing the row and the record to share an identity —
    /// must bind the bytes once and reuse them.
    async fn cal_append(
        state: &AppState,
        actor: &[u8; 32],
        event_id: [u8; 32],
        body: &fauna_mls::wrapped_blob::SealedRecordBytes,
        hint: &fauna_mls::wrapped_blob::SealedRecordBytes,
    ) -> fauna_cbor::Cid {
        crate::segments::cal::append_record(
            &state.cal_segments,
            &state.db,
            actor,
            body,
            hint,
            &cal_floor(event_id),
        )
        .await
        .expect("append calendar record")
        .record_cid
    }

    /// Seed a `bridge_caldav_events` row whose stored `record_cid` names the
    /// record `cal_append` files for the same `(body, hint)`.
    ///
    /// ⚠ Load-bearing since the record-identity cutover: the compaction pass
    /// reaps orphans first, and "orphan" now means *no live row stores this
    /// cid*. A fixture that seeded the row from one pair of bytes and appended
    /// another would have its survivor reaped mid-test.
    async fn seed_cal_row_for(
        state: &AppState,
        actor: &[u8; 32],
        cal: &[u8; 32],
        body: &fauna_mls::wrapped_blob::SealedRecordBytes,
        hint: &fauna_mls::wrapped_blob::SealedRecordBytes,
    ) -> [u8; 32] {
        match state
            .db
            .place_caldav_event(
                actor,
                cal,
                &[0x33u8; 32],
                body.as_slice(),
                hint.as_slice(),
                CAL_T,
                body.as_slice().len() as u32,
                CAL_T,
            )
            .await
            .expect("place")
        {
            crate::db::bridge_caldav::PlaceCaldavEventOutcome::Created { event_id, .. } => event_id,
            other => panic!("expected Created, got {other:?}"),
        }
    }

    async fn compact_calendar(state: &Arc<AppState>, actor: [u8; 32]) -> CompactionReport {
        CompactionWorker::for_manual(state.clone())
            .run_once(
                CompactScope {
                    kind: Some("calendar".into()),
                    scope_id: Some(actor),
                },
                CompactionTrigger::Manual,
            )
            .await
            .expect("run_once")
    }

    /// **S6.8c, the point of the whole slice.** A superseded event's content
    /// record is tombstoned (S6.8a) but stays physically on disk until a
    /// compaction arm exists for its kind. With the arm, a churned bucket is
    /// rewritten without it — while the surviving body still reads back
    /// byte-identically (no-data-loss).
    ///
    /// Before this slice `resolve_kinds`/`segments_for_kind` had no `"calendar"`
    /// arm, so `compact_actor_kind` returned `Ok(0)` and the assertions on
    /// `segments_rewritten` and on the superseded record's absence both fail.
    #[tokio::test]
    async fn churned_calendar_bucket_is_physically_reclaimed() {
        let (_tmp, state) = build_state();
        let actor = [0x71u8; 32];
        let cal = [0x11u8; 32];
        // Bound once — the seal is randomized, so this is the only way to assert
        // the survivor's body comes back byte-identically.
        let body = cal_sealed(b"BEGIN:VCALENDAR:survivor");
        let hint = cal_sealed(b"hint");

        // One live event (row + record), and one superseded record whose row is
        // gone — exactly what `replace_caldav_event_by_uid`'s supersede arm
        // leaves behind once S6.8a tombstones the old content record.
        state
            .db
            .insert_bridge_caldav_calendar(&actor, &cal, b"meta", CAL_T)
            .await
            .expect("calendar");
        let live_id = seed_cal_row_for(&state, &actor, &cal, &body, &hint).await;
        let live_cid = cal_append(&state, &actor, live_id, &body, &hint).await;

        let superseded_cid = cal_append(
            &state,
            &actor,
            [0xAAu8; 32],
            &cal_sealed(b"BEGIN:VCALENDAR:old"),
            &hint,
        )
        .await;
        {
            let conn = state.db.conn().await;
            crate::segments::records_db::tombstone_by_cid(
                &conn,
                &actor,
                crate::segments::cal::KIND,
                &superseded_cid,
            )
            .expect("tombstone superseded record");
        }
        state
            .cal_segments
            .finalize_open(&actor)
            .await
            .expect("flush");

        let report = compact_calendar(&state, actor).await;
        assert_eq!(report.errors, 0);
        assert_eq!(
            report.segments_rewritten, 1,
            "a bucket with a tombstoned record must be rewritten"
        );

        // The survivor's body is intact and byte-identical through the open path.
        let (env, _floor) =
            crate::segments::cal::read_record(&state.cal_segments, &state.db, &actor, &live_cid)
                .await
                .expect("read survivor")
                .expect("survivor present");
        assert_eq!(
            env.encrypted_body,
            body.as_slice().to_vec(),
            "the survivor's body must round-trip byte-identically through a rewrite"
        );

        // The superseded record is physically gone from the rewritten segment.
        assert!(
            crate::segments::cal::read_record(
                &state.cal_segments,
                &state.db,
                &actor,
                &superseded_cid
            )
            .await
            .expect("read superseded")
            .is_none(),
            "a tombstoned content record must not survive compaction"
        );
    }

    /// **S6.8b wired into the worker.** An orphan (content record appended, its
    /// metadata row never committed — a rejected PUT) is reaped by the pass and
    /// then physically reclaimed, while the live event is untouched.
    #[tokio::test]
    async fn compaction_reaps_and_reclaims_an_orphan_calendar_record() {
        let (_tmp, state) = build_state();
        let actor = [0x72u8; 32];
        let cal = [0x11u8; 32];

        state
            .db
            .insert_bridge_caldav_calendar(&actor, &cal, b"meta", CAL_T)
            .await
            .expect("calendar");
        let body = cal_sealed(b"survivor");
        let hint = cal_sealed(b"hint");
        let live_id = seed_cal_row_for(&state, &actor, &cal, &body, &hint).await;
        let live_cid = cal_append(&state, &actor, live_id, &body, &hint).await;

        // The rejected PUT's leftover: a record with no row, older than the
        // reaper's watermark.
        let orphan_cid =
            cal_append(&state, &actor, [0xBBu8; 32], &cal_sealed(b"orphan"), &hint).await;
        state
            .cal_segments
            .finalize_open(&actor)
            .await
            .expect("flush");

        let report = compact_calendar(&state, actor).await;
        assert_eq!(report.errors, 0);

        assert!(
            crate::segments::cal::read_record(&state.cal_segments, &state.db, &actor, &orphan_cid)
                .await
                .expect("read orphan")
                .is_none(),
            "the orphan must be reaped and reclaimed"
        );
        assert!(
            crate::segments::cal::read_record(&state.cal_segments, &state.db, &actor, &live_cid)
                .await
                .expect("read live")
                .is_some(),
            "the live event's body must survive"
        );
    }

    /// The whole-nest scheduled tick skips actors whose `__mail` folder
    /// is a custody copy. Only actors with a non-backup `__mail` folder
    /// (or no folder row at all) are compacted.
    ///
    /// NOTE: `folders.name` is globally UNIQUE (schema V1), so two actors
    /// cannot each own a `__mail` row in the same DB. We therefore give actor
    /// A a `__mail/backup` row and leave actor B with no `__mail` row.
    /// `is_pure_backup_destination` returns `false` when no row
    /// exists (COUNT = 0), so B is treated as a non-backup destination and
    /// is included in the compaction pass — which is the correct behaviour.
    ///
    /// To make the filter's effect observable: actor A's only segment has its
    /// record tombstoned (100 % tombstone fraction — above the 25 % scheduled
    /// threshold). If A were not filtered, compaction would rewrite that
    /// segment and `segments_rewritten` would be 1. With the filter,
    /// `segments_rewritten` stays 0.
    #[tokio::test]
    async fn scheduled_compaction_skips_pure_backup_actors() {
        let (_tmp, state) = build_state();

        // Actor A: __mail custody copy — skipped by the per-(scope, kind) gate
        // in the run_once loop. Actor B: no __mail row — kept.
        let a = [0xA1u8; 32];
        let b = [0xB1u8; 32];
        state
            .db
            .create_folder_with_options(
                "__mail",
                &a,
                crate::db::FolderOptions {
                    custody_copy: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // Seed segment_records rows for both actors so resolve_actors lists
        // both before the filter.
        let a_rids = append_n(&state, &a, 1, 1, 1_715_000_000_000).await;
        let _ = append_n(&state, &b, 1, 1, 1_715_000_000_000).await;
        state.mail_segments.finalize_open(&a).await.unwrap();
        state.mail_segments.finalize_open(&b).await.unwrap();

        // Tombstone actor A's record: 1/1 tombstoned → 100 % fraction, above
        // the 25 % scheduled threshold. If A were processed, segments_rewritten
        // would be 1; filtered → stays 0.
        state
            .db
            .segment_records_mark_tombstoned(
                &a,
                "mail",
                1,
                &fauna_cbor::Cid::from_digest_dag_cbor(a_rids[0]),
            )
            .await
            .unwrap();

        let worker = CompactionWorker::new(state.clone(), Duration::from_secs(60));
        let report = worker
            .run_once(CompactScope::default(), CompactionTrigger::Scheduled)
            .await
            .unwrap();

        // Pure-backup actor A is filtered out before compact_actor_kind is
        // reached; actor B is processed (no tombstones → segments_rewritten = 0).
        assert!(report.acquired_lock);
        assert_eq!(report.errors, 0);
        assert_eq!(
            report.segments_rewritten, 0,
            "pure-backup actor A must be skipped; B has no tombstones → 0 rewrites"
        );
    }

    /// Plan 9: the whole-nest scheduled pass must skip a pure-backup *conv*
    /// channel per (scope, kind) — not just pure-backup mail actors. Before the
    /// per-(scope, kind) gate moved into the run_once loop, `resolve_actors`
    /// filtered every scope with `kind = "mail"`, so a pure-backup conv channel
    /// holding local conv segment_records was checked as
    /// `is_pure_backup_destination("mail", channel)` → false → kept → compacted.
    /// Now it is checked as `("conv", channel)` → true → skipped.
    #[tokio::test]
    async fn scheduled_compaction_skips_pure_backup_conv_channel() {
        let (_tmp, state) = build_state();
        let channel = [0xC9u8; 32];

        // Seed a tombstoned conv segment that WOULD cross the 25 % scheduled
        // threshold (3 of 5 tombstoned = 60 %), so a rewrite of 1 is the
        // observable signal that the channel was *not* gated.
        let seqs = append_n_conv(&state, &channel, 5, 1_715_000_000_000).await;
        assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
        state.conv_segments.finalize_open(&channel).await.unwrap();
        let n = crate::segments::conv::tombstone_up_to_seq(&state.db, &[channel], 3)
            .await
            .expect("tombstone");
        assert_eq!(n, 3);

        // Mark the channel's reserved conv set pure-backup.
        let name = format!("__conv/{}", hex::encode(channel));
        state
            .db
            .create_folder_with_options(
                &name,
                &channel,
                crate::db::FolderOptions {
                    custody_copy: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let worker = CompactionWorker::new(state.clone(), Duration::from_secs(60));
        let report = worker
            .run_once(CompactScope::default(), CompactionTrigger::Scheduled)
            .await
            .unwrap();

        assert!(report.acquired_lock);
        assert_eq!(report.errors, 0);
        assert_eq!(
            report.segments_rewritten, 0,
            "pure-backup conv channel must be skipped by the per-(scope, kind) gate"
        );
    }

    /// `CompactionTrigger::Scheduled`'s 25 % threshold blocks a
    /// segment with only 10 % tombstones; switching to Manual (0 %)
    /// rewrites it.
    #[tokio::test]
    async fn scheduled_threshold_skips_below_25_percent() {
        let (_tmp, state) = build_state();
        let actor = [0x55u8; 32];

        // Seg 1: 10 records, tombstone 1 (10 %).
        let rids = append_n(&state, &actor, 1, 10, 1_712_000_000_000).await;
        state
            .mail_segments
            .finalize_open(&actor)
            .await
            .expect("flush");
        state
            .db
            .segment_records_mark_tombstoned(
                &actor,
                "mail",
                1,
                &fauna_cbor::Cid::from_digest_dag_cbor(rids[0]),
            )
            .await
            .expect("mark");
        assert_eq!(count_tombstoned(&state.db, &actor, 1).await, 1);

        let worker = CompactionWorker::new(state.clone(), Duration::from_secs(60));
        // Scheduled — below threshold, no rewrite.
        let report = worker
            .run_once(
                CompactScope {
                    kind: Some("mail".into()),
                    scope_id: Some(actor),
                },
                CompactionTrigger::Scheduled,
            )
            .await
            .expect("run_once");
        assert_eq!(report.segments_rewritten, 0);
        assert_eq!(report.errors, 0);

        // Same state, Manual threshold (0 %) — rewrites.
        let report = worker
            .run_once(
                CompactScope {
                    kind: Some("mail".into()),
                    scope_id: Some(actor),
                },
                CompactionTrigger::Manual,
            )
            .await
            .expect("run_once");
        assert_eq!(report.segments_rewritten, 1);
    }

    // -----------------------------------------------------------------------
    // Conv-kind compaction (Plan 8 T2)
    //
    // Mirrors the mail tests above but drives the `conv_segments` manager via
    // the `segments::conv` free fns. The worker selects the manager by kind
    // (`segments_for_kind`) and routes `rewrite_bucket` to
    // `conv::compact_bucket`, so the same `run_once` path exercises conv.
    // Distinct channel ids per test (for-test segment dirs are PID-shared).
    // -----------------------------------------------------------------------

    /// Append `n` conv records into `channel`'s segment store at a fixed
    /// `received_at` (so they share one bucket → one segment). Returns the
    /// assigned seqs (1..=n).
    async fn append_n_conv(
        state: &Arc<AppState>,
        channel: &[u8; 32],
        n: usize,
        received_at: i64,
    ) -> Vec<i64> {
        let mut seqs = Vec::with_capacity(n);
        for i in 0..n {
            let outcome = crate::segments::conv::append(
                &state.conv_segments,
                &state.db,
                channel,
                format!("conv-body-{i}").as_bytes(),
                received_at,
            )
            .await
            .expect("conv append");
            seqs.push(outcome.seq);
        }
        seqs
    }

    /// An unpinned conv segment whose tombstone fraction crosses the Manual
    /// threshold (0 %) is rewritten under a per-channel manual trigger. The
    /// survivors stay readable via `conv::read_after_seq` with their original
    /// seq values; the tombstoned record is gone. Conv sibling of
    /// `unpinned_tombstoned_segment_rewrites_under_manual_trigger`.
    #[tokio::test]
    async fn conv_unpinned_tombstoned_segment_rewrites_under_manual_trigger() {
        let (_tmp, state) = build_state();
        let channel = [0x61u8; 32];

        // Seg 1: 5 conv records in one bucket. Tombstone seq <= 2 (the two
        // oldest). No pinned snapshot.
        let seqs = append_n_conv(&state, &channel, 5, 1_712_000_000_000).await;
        assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
        state
            .conv_segments
            .finalize_open(&channel)
            .await
            .expect("flush");
        let n = crate::segments::conv::tombstone_up_to_seq(&state.db, &[channel], 2)
            .await
            .expect("tombstone");
        assert_eq!(n, 2);
        assert_eq!(count_live(&state.db, &channel, 1).await, 3);
        assert_eq!(count_tombstoned(&state.db, &channel, 1).await, 2);

        let worker = CompactionWorker::for_manual(state.clone());
        let report = worker
            .run_once(
                CompactScope {
                    kind: Some("conv".into()),
                    scope_id: Some(channel),
                },
                CompactionTrigger::Manual,
            )
            .await
            .expect("run_once");
        assert!(report.acquired_lock);
        assert_eq!(report.errors, 0);
        assert_eq!(report.segments_rewritten, 1, "one conv segment rewritten");

        // Manifest: seg 1 tombstoned, seg 2 live (the rewritten survivors).
        let post = state
            .conv_segments
            .load_manifest(&channel)
            .await
            .expect("load");
        assert_eq!(post.kind_manifest.live_segments, vec![2]);
        assert_eq!(post.kind_manifest.tombstoned_segments, vec![1]);

        // Surviving conv records (seq 3, 4, 5) still readable with their
        // original seq values; the tombstoned seqs (1, 2) are gone.
        let rows = crate::segments::conv::read_after_seq(
            &state.conv_segments,
            &state.db,
            &channel,
            0,
            100,
        )
        .await
        .expect("read");
        assert_eq!(
            rows.iter().map(|r| r.0).collect::<Vec<_>>(),
            vec![3, 4, 5],
            "only the non-tombstoned conv records survive, original seq preserved"
        );
    }

    /// A conv segment pinned by an active `message_kind='conv'` snapshot must
    /// survive a manual compaction pass even when every one of its records is
    /// tombstoned. Conv sibling of `pinned_segment_survives_manual_compact`.
    /// The pin-set query joins `folders.actor_id = channel` AND
    /// `message_kind = 'conv'`, so the folder's `actor_id` carries the
    /// channel id (Decision 3) and the snapshot's manifest references seg 1.
    #[tokio::test]
    async fn pinned_conv_snapshot_segment_survives_manual_compact() {
        let (_tmp, state) = build_state();
        let channel = [0x62u8; 32];

        // Seg 1: 3 conv records in one bucket. Finalize.
        let seqs = append_n_conv(&state, &channel, 3, 1_712_000_000_000).await;
        assert_eq!(seqs, vec![1, 2, 3]);
        state
            .conv_segments
            .finalize_open(&channel)
            .await
            .expect("flush");

        // Tombstone every conv record on seg 1 (100 % → would rewrite if
        // unpinned).
        let n = crate::segments::conv::tombstone_up_to_seq(&state.db, &[channel], 3)
            .await
            .expect("tombstone");
        assert_eq!(n, 3);
        assert_eq!(count_live(&state.db, &channel, 1).await, 0);
        assert_eq!(count_tombstoned(&state.db, &channel, 1).await, 3);

        // Pin via a `message_kind='conv'` snapshot whose manifest references
        // seg 1. The reserved folder's `actor_id` MUST equal the channel id
        // (the pin-set query joins on it) — `create_folder` here passes
        // `&channel` as the folder's scope key.
        let pinned_manifest = state
            .conv_segments
            .load_manifest(&channel)
            .await
            .expect("load");
        let blob = fauna_core::encoding::canonical_encode(&pinned_manifest).expect("encode");
        let fs = create_folder(&state.db, &channel, "pinned_conv_survives_test").await;
        pin_manifest(&state.db, fs, "conv", &blob).await;

        let worker = CompactionWorker::for_manual(state.clone());
        let report = worker
            .run_once(
                CompactScope {
                    kind: Some("conv".into()),
                    scope_id: Some(channel),
                },
                CompactionTrigger::Manual,
            )
            .await
            .expect("run_once");
        assert!(report.acquired_lock, "should acquire gc lock");
        assert_eq!(report.errors, 0);
        assert_eq!(
            report.segments_rewritten, 0,
            "pinned conv seg must not be rewritten"
        );

        // Seg 1 still listed live in the manifest + its mirror rows still
        // present (tombstoned). The segment file is still on disk.
        let post = state
            .conv_segments
            .load_manifest(&channel)
            .await
            .expect("load");
        assert!(
            post.kind_manifest.live_segments.contains(&1),
            "pinned conv seg 1 still live"
        );
        assert_eq!(count_tombstoned(&state.db, &channel, 1).await, 3);
        let seg_path = state.conv_segments.segment_file_path(&channel, 1);
        assert!(seg_path.exists(), "pinned conv segment file still on disk");
    }
}
