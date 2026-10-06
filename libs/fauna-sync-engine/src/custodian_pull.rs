//! The **client-device custodian's pull pass** — one turn of list → diff →
//! fetch → seal → store → reclaim → check-in.
//!
//! Owner: `docs/goal/architecture/message-segment-store.md` § Client-device
//! custodian (pull); the kind, enrollment and status semantics are
//! `docs/goal/behavior/backup-destinations.md` § State & data shape → *Third destination kind*.
//!
//! # It is a reader, not a second writer
//!
//! Bytes move by **pull, nest-mediated**: the device has no stable address, sits
//! behind NAT, and is backgrounded by its OS most of the day, while the
//! authoritative corpus already lives on the nest. So this drives exactly the
//! client-arm leaves the nest-destination coordinator drives —
//! [`SegmentSource::list_segments`] / [`SegmentSource::segment_bytes`],
//! [`diff_segments`], [`LiveManifestMirror`] — and swaps the destination upload
//! for [`CustodianStore`]. It is a **new consumer** of those leaves, not a
//! revival of the retired client→nest-destination writer (the slice-5 flip
//! deletes that; do not let the deletion sweep the leaves).
//!
//! # Native-only, on purpose
//!
//! There is no web custodian and this must not pretend otherwise: this pull
//! path cannot compile to wasm (`rusqlite`, a `!Sync` `Connection`,
//! `tokio::spawn` — the decisive 2026-06-18 scoping), so a web leg waits on a
//! wasm `SyncDb` foundation that does not exist.
//!
//! # The one report that must not lie
//!
//! A custodian that stopped at its cap but checks in `CAP_STATE_OK` renders as
//! ordinary lag, and the user is never told their backup stopped. The final
//! `plan_reclaim` cannot see that on its own — a pass that stopped because the
//! *next* segment would not fit ends *below* the cap, which reads as healthy. So
//! [`PullReport::cap_state`] is the plan's state **widened** by whether this pass
//! actually refused to pull, and that is what the check-in carries.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use fauna_client_backup::BackupClient;
use fauna_client_backup::audit::{AUDIT_MIN_INTERVAL_SECS, AUDIT_SAMPLE_K};
use fauna_client_backup::custodian::{CapState, ReclaimPlan, plan_reclaim};
use fauna_core::crypto::OwnerSealKey;
use fauna_core::data::ContentHash;
use fauna_core::file_download::FileDownloadKeys;
use fauna_protocol::backup::CoveredFolder;
use fauna_protocol::{RpcError, RpcErrorClass, RpcRequester};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::custodian_store::{
    CustodianStore, HeldRow, SourceFacts, StagedSeal, held_path, path_in_set,
};
use crate::db::MailBackupSegmentState;
use crate::segment_backup::LiveManifestMirror;
use crate::segment_backup::{
    SegmentFamily, SegmentHalf, SegmentListing, SegmentSource, diff_segments, live_manifest_mirror,
    verify_meta_against,
};

/// What one [`CustodianPull::run_once`] pass did.
///
/// [`Default`] is hand-written rather than derived, and [`CapState`] is
/// deliberately left without one: a derived default would hand out
/// [`CapState::Ok`] — "your backup is healthy" — to anything built with
/// `..Default::default()`. Here the starting value is stated once, at the top of
/// a pass that is about to overwrite it, which is the only place it is safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullReport {
    /// Segment ids fetched, sealed and stored this pass — both halves of each.
    pub stored_segments: Vec<u32>,
    /// Segment ids whose `.dat` this store already held and whose `.meta`
    /// sidecar this pass fetched, sealed and stored beside it — the crash
    /// recovery for a torn pull, where the `.dat` put landed and the `.meta`
    /// put did not (`SegmentDiff::to_backfill_meta`). Moves no `.dat` bytes.
    pub backfilled_meta: Vec<u32>,
    /// Segment ids the source no longer lists, tombstoned locally.
    pub tombstoned_segments: Vec<u32>,
    /// Segment ids skipped because storing them would have breached the cap.
    /// Non-empty ⇒ [`Self::cap_state`] is [`CapState::Reached`].
    pub skipped_for_cap: Vec<u32>,
    /// Segment ids re-fetched **only** to repair a flagged local copy whose
    /// fetch or verification failed this pass. Already held and counted, so
    /// the pass carries on without them; their flags stay on the repair list
    /// and the next pass tries again.
    pub unrepaired_segments: Vec<u32>,
    /// `true` iff the `manifest.<kind>` mirror changed and was re-stored.
    pub mirror_stored: bool,
    /// Generations reclaimed before pulling (expired retention, then cap
    /// pressure — never a live path).
    pub reclaimed_generations: usize,
    /// Bytes held after the pass — the check-in's `held_bytes`.
    pub held_bytes: u64,
    /// The check-in's `cap_state`.
    pub cap_state: CapState,
    /// How far this device has pulled **and sealed** — the check-in's
    /// `high_water`, from which the nest derives the destination's backlog.
    /// A statement about the **content** family alone: a journal has no head
    /// of its own worth reporting.
    pub high_water: u64,
    /// What the same pass did for the kind's **placement journal**
    /// ([`SegmentFamily::Placement`]). `None` when the kind has no journal
    /// ([`SegmentFamily::serve_kind`]). The six
    /// per-segment fields above are the content family's alone.
    pub placement: Option<FamilyPull>,
    /// `Some` when the pass was **refused** because the source's saved counter
    /// went below the ledger this store holds ([`SourceRegressed`]): nothing
    /// was fetched, tombstoned, reclaimed or mirrored, no check-in was made,
    /// and every per-segment field above is empty. `held_bytes`, `cap_state`
    /// and `high_water` still describe the store as it stands.
    pub source_regressed: Option<SourceRegressed>,
}

/// A pull pass refused as a **source regression**: for one family of the kind,
/// the source's saved counter (`SegmentListing::next_segment_id`)
/// is below the generation of the ledger this
/// store holds (`LiveManifestMirror::next_segment_id_seen`).
///
/// An honest source's counter never decreases while its data directory lives —
/// append, compaction and re-seed adoption only raise it — so a lower one is a
/// box that lost its corpus (a rebuild) or went back to an older copy, and a
/// diff against it would read every segment above the new counter as dropped
/// and tombstone the copy (`segment-backup-protocol.md` § Client-device
/// custodian (pull) → *The pull never tombstones against a source below its
/// copy*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRegressed {
    /// The family whose counter went back — content is compared first.
    pub family: SegmentFamily,
    /// The generation of the ledger this store holds.
    pub held: u32,
    /// The counter the source served.
    pub served: u32,
}

/// One kind's listings, read and compared against the store's ledgers before
/// anything moves ([`CustodianPull::prepare`]) — split from the pass that acts
/// on them so [`CustodianPull::run_all_kinds`] can hold the covered-folder
/// passes when any kind's source went backwards.
struct PreparedKind {
    content: SegmentListing,
    journal: Option<SegmentListing>,
    regressed: Option<SourceRegressed>,
}

/// What one pass did for ONE family of a backed-up kind — the per-segment half
/// of a [`PullReport`]. The content family's is spread over the report's own
/// fields; the journal's rides in [`PullReport::placement`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FamilyPull {
    /// Segment ids fetched, sealed and stored this pass — both halves of each.
    pub stored_segments: Vec<u32>,
    /// Segment ids whose `.meta` alone was fetched beside a held `.dat`.
    pub backfilled_meta: Vec<u32>,
    /// Segment ids the source no longer lists, tombstoned locally.
    pub tombstoned_segments: Vec<u32>,
    /// Segment ids skipped because storing them would have breached the cap.
    pub skipped_for_cap: Vec<u32>,
    /// Segment ids whose repair fetch failed this pass.
    pub unrepaired_segments: Vec<u32>,
    /// `true` iff this family's mirror changed and was re-stored.
    pub mirror_stored: bool,
}

impl Default for PullReport {
    fn default() -> Self {
        Self {
            stored_segments: Vec::new(),
            backfilled_meta: Vec::new(),
            tombstoned_segments: Vec::new(),
            skipped_for_cap: Vec::new(),
            unrepaired_segments: Vec::new(),
            mirror_stored: false,
            reclaimed_generations: 0,
            held_bytes: 0,
            cap_state: CapState::Ok,
            high_water: 0,
            placement: None,
            source_regressed: None,
        }
    }
}

/// What one [`CustodianPull::run_folder_once`] covered-folder mirror pass did.
/// Deriving [`Default`] is safe here — unlike [`PullReport`] it carries no
/// [`CapState`], only counters whose zero is honest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FolderPullReport {
    /// Paths whose manifest + chunks were fetched, verified and stored as-is.
    pub stored_paths: usize,
    /// Paths the head no longer lists, tombstoned locally.
    pub tombstoned_paths: usize,
    /// Paths skipped because storing them would have breached the cap.
    pub skipped_for_cap: usize,
}

/// One custodian device's pull pass against one source nest.
pub struct CustodianPull<'a, S: SegmentSource, R: RpcRequester> {
    /// The source's segment control plane + byte plane.
    pub source: &'a S,
    /// This device's local sealed store.
    pub store: &'a CustodianStore,
    /// The source nest's `fauna.backup.*` surface, for the check-in.
    pub backup: &'a BackupClient<R>,
    /// The `BackupDestination::destination_id` of this device's own row.
    pub destination_id: String,
    /// The owner's actor scope these segments belong to.
    pub scope_id: [u8; 32],
    /// The owner key this custodian seals under. Derived locally from the
    /// identity seed it already holds — a pull-only custodian needs **no**
    /// `NestBackupKey` grant, because it seals for itself.
    pub seal_key: OwnerSealKey,
    /// `BackupDestination::capacity_cap_bytes`. `None` = uncapped.
    pub cap_bytes: Option<u64>,
    /// This device's stable sync id — the one the assignment was matched on
    /// (`fauna_core::data::CustodianAssignment::device_id`), reported in the
    /// check-in so the nest can refuse a check-in against a row registered to a
    /// *different* device. `None` only in tests that do not exercise that
    /// refusal; the nest REFUSES a check-in that names no device
    /// (`backup_handlers.rs`, the carrier's doc in `fauna-protocol`'s `backup.rs`).
    pub device_id: Option<String>,
    /// The covered-folder byte plane
    /// (`docs/goal/behavior/backup-destinations.md` § Ordinary-folder
    /// coverage) — `Some` wherever the host has a wire source
    /// ([`crate::segment_backup::SourceBinding`] implements it); `None` skips
    /// the folder axis entirely, which is also the honest reading for tests
    /// that fake only the segment plane.
    pub folder_source: Option<&'a (dyn crate::segment_backup::FolderCorpusSource + 'a)>,
    /// Fired when the source nest refuses a check-in with the typed
    /// [`RpcError::CODE_BACKUP_CUSTODIAN_NOT_ASSIGNED`]: its registry no longer
    /// assigns [`Self::destination_id`] to this device. A host that re-reads its
    /// assignment on a timer waits on this to re-read at once instead
    /// (`docs/goal/architecture/apps/sync-agent.md` § A7). `notify_one`, so a
    /// refusal that lands while nobody waits is kept for the next waiter.
    /// `None` for a host that re-discovers its assignment on every run anyway
    /// (the mobile construct-run-drop shells) and for tests that do not
    /// exercise the refusal.
    pub not_assigned: Option<&'a Notify>,
}

impl<S: SegmentSource, R: RpcRequester> CustodianPull<'_, S, R>
where
    R::Error: RpcErrorClass,
{
    // The store's row paths for one kind's set — the family's within-set path,
    // qualified by the set through the store's one formatter
    // (`custodian_store` module doc, *Row paths are set-qualified*).

    fn segment_path(set: &str, family: SegmentFamily, scope_hex: &str, segment_id: u32) -> String {
        held_path(set, &family.dat_path(scope_hex, segment_id))
    }

    fn meta_path(set: &str, family: SegmentFamily, scope_hex: &str, segment_id: u32) -> String {
        held_path(set, &family.meta_path(scope_hex, segment_id))
    }

    fn mirror_path(set: &str, family: SegmentFamily, scope_hex: &str, kind: &str) -> String {
        held_path(set, &family.mirror_path(scope_hex, kind))
    }

    /// The custody set `kind`'s rows rest under in this store — the name its
    /// destination set carries (`reserved_backup_set_name`).
    fn kind_set(&self, kind: &str) -> Result<String> {
        crate::segment_backup::reserved_backup_set_name(kind, &self.scope_id)
            .ok_or_else(|| anyhow::anyhow!("kind has no backup surface: {kind}"))
    }

    /// Reconstitute the local segment state the shared [`diff_segments`] reads.
    ///
    /// The custodian's "local state" is its held index, not a `SyncDb` table —
    /// but the diff is a **shared leaf** and must not be re-implemented here, so
    /// the index is projected into the shape it takes. Only live (non-tombstoned)
    /// generations count: a tombstoned path is one the source stopped listing, so
    /// re-listing it is genuinely new. A segment's row is keyed by its `.dat`;
    /// the live `.meta` sibling, when held, fills `last_meta_size`, and its
    /// absence is exactly what the diff turns into a sidecar backfill.
    ///
    /// One kind's set and one family at a time: the store holds every kind's
    /// set, each keyed by its name ([`path_in_set`]), and within a set each
    /// family's parser refuses the other's paths — so the content's state never
    /// counts a journal segment, another kind's segment, or the reverse.
    fn local_state(
        rows: &[HeldRow],
        set: &str,
        family: SegmentFamily,
        scope_hex: &str,
    ) -> HashMap<u32, MailBackupSegmentState> {
        let mut out = HashMap::new();
        for row in rows {
            let Some((segment_id, SegmentHalf::Dat)) =
                path_in_set(set, &row.path).and_then(|p| family.parse(scope_hex, p))
            else {
                continue;
            };
            // Liveness is per path, and `live_at` is the one derivation of it.
            if CustodianStore::live_at(rows, &row.path).map(|r| r.stored_at) != Some(row.stored_at)
            {
                continue;
            }
            let last_meta_size =
                CustodianStore::live_at(rows, &Self::meta_path(set, family, scope_hex, segment_id))
                    .map(|meta| meta.source_size_bytes);
            out.insert(
                segment_id,
                MailBackupSegmentState {
                    last_chunk_count: row.source_record_count,
                    last_byte_size: row.source_size_bytes,
                    last_synced_at: row.stored_at.max(0) as u64,
                    last_meta_size,
                },
            );
        }
        out
    }

    /// The facts recorded on a sidecar's row: its own plaintext size (what
    /// [`Self::local_state`] reads back as `last_meta_size`), and the segment's
    /// record count — a fact about the pair, observed from the same listing.
    fn meta_facts(meta_len: u64, src_ref: &fauna_protocol::segments::SegmentRef) -> SourceFacts {
        SourceFacts {
            size_bytes: meta_len,
            record_count: src_ref.record_count as u64,
        }
    }

    /// The greatest segment id this device holds live for `scope`, plus one —
    /// **pulled and sealed**, never merely listed. The nest computes backlog as
    /// its own head minus this.
    ///
    /// The **content** family's, always: the nest's head is its content head
    /// (`owner_segment_head`), and a journal segment counted here would report
    /// a device level with a nest it has pulled no mail from. And **one kind's**,
    /// parsed within that kind's `set`: the nest's head is per `(scope, kind)`,
    /// so counting another kind's ids would report a level the kind never
    /// reached.
    fn high_water(rows: &[HeldRow], set: &str, scope_hex: &str) -> u64 {
        Self::local_state(rows, set, SegmentFamily::Content, scope_hex)
            .keys()
            .max()
            .map(|m| *m as u64 + 1)
            .unwrap_or(0)
    }

    /// Fetch one segment pair and verify it, half by half, against the
    /// source's own listing: a torn pair (a rotation between the two fetches)
    /// or a substituted body is refused here, where a re-fetch fixes it,
    /// rather than sealed into a store as bytes nothing can reopen.
    async fn fetch_verified_pair(
        &self,
        kind: &str,
        scope_hex: &str,
        src_ref: &fauna_protocol::segments::SegmentRef,
    ) -> Result<crate::segment_backup::StagedPair> {
        let pair = self
            .source
            .stage_segment_pair(
                kind,
                scope_hex,
                src_ref.segment_id,
                &self.store.staging_dir(),
            )
            .await
            .with_context(|| {
                format!(
                    "custodian fetch kind={kind} scope={scope_hex} seg={}",
                    src_ref.segment_id
                )
            })?;
        pair.verify_against(src_ref)?;
        Ok(pair)
    }

    /// Run one pass for one segment-store `kind`.
    ///
    /// `now` is unix seconds; the caller supplies it so a scheduler and a test
    /// drive the same code (convention 14 — no wall-clock reads buried in logic).
    pub async fn run_once(&self, kind: &str, now: i64) -> Result<PullReport> {
        let prepared = self.prepare(kind, now).await?;
        self.run_prepared(kind, prepared, now).await
    }

    /// Steps 1 and 1′ of a pass: list both families, and compare each
    /// listing's saved counter against the ledger this store holds for it.
    async fn prepare(&self, kind: &str, now: i64) -> Result<PreparedKind> {
        let scope_hex = hex::encode(self.scope_id);

        // 1 — what the source has, in both families, before anything moves: a
        // source that cannot list is a failed pass, not a half-run one — the
        // journal included: every nest serves its kinds' journals, so a
        // refused journal tag fails the pass like any other listing error. A
        // journal listing of `None` means only that the kind has no journal
        // (`SegmentSource::list_family`).
        let content = self
            .source
            .list_segments(kind, &scope_hex)
            .await
            .with_context(|| format!("custodian list segments kind={kind}"))?;
        let journal = self
            .source
            .list_family(kind, SegmentFamily::Placement, &scope_hex)
            .await
            .with_context(|| format!("custodian list placement journal kind={kind}"))?;

        // 1′ — the source's word against the copy's, family by family, content
        // first. Every family is compared (and its record written or cleared)
        // even once one has regressed, so the record says which ledgers the
        // source went below, not only the first.
        let mut regressed = None;
        let families = std::iter::once((SegmentFamily::Content, &content))
            .chain(journal.as_ref().map(|j| (SegmentFamily::Placement, j)));
        for (family, listing) in families {
            let found = self.compare_counter(kind, family, listing, now).await?;
            regressed = regressed.or(found);
        }
        Ok(PreparedKind {
            content,
            journal,
            regressed,
        })
    }

    /// Compare one family's listed counter against the generation of the
    /// ledger this store holds for it, and record or clear the regression.
    ///
    /// A store holding no live ledger for the family compares nothing — the
    /// first pass pulls from any source. A ledger this store holds live but
    /// cannot open (evicted or rotted bytes, waiting on the repair only a pass
    /// that runs can make) falls back to the greatest segment id held live plus
    /// one: the source's counter was above every id it ever minted, so that
    /// floor is one the honest source still clears, and a pass is never
    /// admitted against a corpus-less source for want of its ledger.
    async fn compare_counter(
        &self,
        kind: &str,
        family: SegmentFamily,
        listing: &SegmentListing,
        now: i64,
    ) -> Result<Option<SourceRegressed>> {
        let scope_hex = hex::encode(self.scope_id);
        let set = self.kind_set(kind)?;
        let ledger = Self::mirror_path(&set, family, &scope_hex, kind);
        let rows = self.store.held().await?;
        if CustodianStore::live_at(&rows, &ledger).is_none() {
            return Ok(None);
        }
        let keys = FileDownloadKeys::owner(self.seal_key.clone());
        let held = match self.store.open(&ledger, &keys).await.and_then(|bytes| {
            LiveManifestMirror::from_bytes(&bytes)
                .map_err(|e| anyhow::anyhow!("custodian ledger {ledger} does not decode: {e}"))
        }) {
            Ok(mirror) => mirror.next_segment_id_seen,
            Err(e) => {
                tracing::warn!(
                    destination_id = %self.destination_id,
                    kind,
                    ?family,
                    error = %e,
                    "custodian ledger unreadable; comparing the source's counter against the held segments instead"
                );
                Self::local_state(&rows, &set, family, &scope_hex)
                    .keys()
                    .max()
                    .map_or(0, |m| m + 1)
            }
        };
        let served = listing.next_segment_id;
        if served < held {
            self.store
                .record_source_regression(&ledger, held, served, now)
                .await?;
            Ok(Some(SourceRegressed {
                family,
                held,
                served,
            }))
        } else {
            self.store.clear_source_regression(&ledger).await?;
            Ok(None)
        }
    }

    /// Steps 2–6 of a pass, over listings [`Self::prepare`] read and compared.
    async fn run_prepared(
        &self,
        kind: &str,
        prepared: PreparedKind,
        now: i64,
    ) -> Result<PullReport> {
        let scope_hex = hex::encode(self.scope_id);
        let mut report = PullReport::default();
        let PreparedKind {
            content: src_listing,
            journal: journal_listing,
            regressed,
        } = prepared;

        // 1″ — a source below its copy: the pass is refused whole. No fetch,
        // no diff, no tombstone, no reclaim, no mirror, and above all no
        // check-in — one would tell the nest this device is caught up on a
        // corpus the source no longer holds. The report still describes the
        // store as it stands.
        if let Some(regressed) = regressed {
            let rows = self.store.held().await?;
            let policy: Vec<_> = rows.iter().map(HeldRow::to_policy).collect();
            let standing = plan_reclaim(&policy, self.cap_bytes, now);
            report.held_bytes = standing.held_bytes;
            report.cap_state = standing.cap_state;
            report.high_water = Self::high_water(&rows, &self.kind_set(kind)?, &scope_hex);
            report.source_regressed = Some(regressed);
            tracing::warn!(
                destination_id = %self.destination_id,
                kind,
                family = ?regressed.family,
                held = regressed.held,
                served = regressed.served,
                "CustodianPull: the source's counter is below the ledger this device holds — \
                 pass refused, nothing tombstoned, no check-in",
            );
            return Ok(report);
        }

        // 2 — make room BEFORE pulling: expired retention first, then in-grace
        // retention under cap pressure. `plan_reclaim` never names a live path,
        // so this cannot destroy the owner's only copy of anything.
        let rows = self.store.held().await?;
        let policy: Vec<_> = rows.iter().map(HeldRow::to_policy).collect();
        let plan = plan_reclaim(&policy, self.cap_bytes, now);
        if !plan.reclaim.is_empty() {
            let victims: Vec<HeldRow> = plan.reclaim.iter().map(|&i| rows[i].clone()).collect();
            report.reclaimed_generations = self.store.reclaim(&victims).await?.generations;
        }
        // A pair a crashed pass was staging, and never will finish.
        crate::segment_backup::sweep_stale_staging(&self.store.staging_dir()).await;

        // The running admission budget. Carried locally rather than re-planning
        // per segment: `ReclaimPlan::admits` answers against a fixed
        // `held_bytes`, so a stale plan would over-admit.
        let mut budget = ReclaimPlan {
            reclaim: Vec::new(),
            held_bytes: plan.held_bytes,
            cap_state: plan.cap_state,
        };

        // 3–5 — once per family, content first.
        let content = self
            .pull_family(kind, SegmentFamily::Content, &src_listing, now, &mut budget)
            .await?;
        report.stored_segments = content.stored_segments;
        report.backfilled_meta = content.backfilled_meta;
        report.tombstoned_segments = content.tombstoned_segments;
        report.skipped_for_cap = content.skipped_for_cap;
        report.unrepaired_segments = content.unrepaired_segments;
        report.mirror_stored = content.mirror_stored;
        if let Some(journal_listing) = journal_listing {
            report.placement = Some(
                self.pull_family(
                    kind,
                    SegmentFamily::Placement,
                    &journal_listing,
                    now,
                    &mut budget,
                )
                .await?,
            );
        }

        // 6 — report honestly. `plan_reclaim` over the final store gives
        // `held_bytes`; the cap state is that plan's, WIDENED by whether this
        // pass actually refused to pull. A pass that stopped because the next
        // segment would not fit lands under the cap, and reporting the plan's
        // bare verdict would render a stopped backup as ordinary lag.
        let rows = self.store.held().await?;
        let policy: Vec<_> = rows.iter().map(HeldRow::to_policy).collect();
        let settled = plan_reclaim(&policy, self.cap_bytes, now);
        let cap_state = if budget.cap_state == CapState::Reached {
            CapState::Reached
        } else {
            settled.cap_state
        };

        report.held_bytes = settled.held_bytes;
        report.cap_state = cap_state;
        report.high_water = Self::high_water(&rows, &self.kind_set(kind)?, &scope_hex);

        // 6b — carry the self-audit verdict. Read from the store's persisted
        // record rather than audited here: the audit runs on its own debounce
        // (`run_all_kinds`), while EVERY check-in must carry the standing
        // verdict — a rotted store that only reported on audit passes would go
        // back to looking healthy on the very next pull pass.
        let audit = self.store.audit_record().await.verdict();

        ReclaimPlan {
            reclaim: Vec::new(),
            held_bytes: settled.held_bytes,
            cap_state,
        }
        .check_in(
            self.backup,
            &self.destination_id,
            report.high_water,
            audit,
            self.device_id.as_deref(),
        )
        .await
        .map_err(|e| {
            // Classified here, on the transport's typed error, because the
            // `anyhow` below flattens it to text: past this point a
            // not-assigned refusal and a timeout look the same.
            if e.as_rpc_error()
                .is_some_and(|rpc| rpc.code == RpcError::CODE_BACKUP_CUSTODIAN_NOT_ASSIGNED)
                && let Some(not_assigned) = self.not_assigned
            {
                not_assigned.notify_one();
            }
            anyhow::anyhow!("custodian check-in failed: {e}")
        })?;

        tracing::info!(
            destination_id = %self.destination_id,
            kind,
            stored = report.stored_segments.len(),
            backfilled_meta = report.backfilled_meta.len(),
            tombstoned = report.tombstoned_segments.len(),
            skipped_for_cap = report.skipped_for_cap.len(),
            reclaimed = report.reclaimed_generations,
            held_bytes = report.held_bytes,
            high_water = report.high_water,
            "CustodianPull: pass complete",
        );
        Ok(report)
    }

    /// Steps 3–5 of a pass, for ONE family of `kind`: diff the source's listing
    /// against what this store holds, pull what fits, tombstone what the source
    /// dropped, store the family's mirror.
    ///
    /// **One body for both families** (`segment-backup-protocol.md` §
    /// Client-device custodian (pull) → *Restore* → *The placement journal
    /// rides the set*), and the two share one `budget`: the cap is the kind's
    /// only knob and it bounds the whole store, so a journal segment is
    /// admitted or refused against the same running figure as a content one.
    /// Content runs first ([`SegmentFamily::PASS_ORDER`]), so under a tight cap
    /// it is the journal that waits — the honest order, since a journal whose
    /// records are absent files nothing.
    async fn pull_family(
        &self,
        kind: &str,
        family: SegmentFamily,
        src_listing: &SegmentListing,
        now: i64,
        budget: &mut ReclaimPlan,
    ) -> Result<FamilyPull> {
        let src_refs = &src_listing.segments;
        let tag = family
            .serve_kind(kind)
            .ok_or_else(|| anyhow::anyhow!("kind {kind} has no {family:?} family"))?;
        let scope_hex = hex::encode(self.scope_id);
        let set = self.kind_set(kind)?;
        let seal_root = self.seal_key.convergent_chunk_root();
        let mut out = FamilyPull::default();

        // 3 — diff against what survived, and pull what fits.
        let rows = self.store.held().await?;
        let local = Self::local_state(&rows, &set, family, &scope_hex);
        let mut diff = diff_segments(src_refs, &local);

        // 3′ — the paths the standing self-audit could not produce re-enter the
        // diff. Nothing else ever would: the diff reads the index, and the
        // index goes on calling an evicted or rotted generation held at its
        // current manifest, so the audit's finding was an alarm with no remedy
        // behind it. This is the whole of "partial OS eviction is tolerated by
        // construction — content addressing re-converges on the next pull"
        // (`docs/goal/architecture/segment-backup-protocol.md` § Client-device
        // custodian (pull) → *Custody = the local store*).
        //
        // **Added to `to_upload` rather than dropped from `local_state`**,
        // which would have been the shorter spelling: `to_drop` is derived from
        // the local keys, so a segment that rotted AND was compacted away by
        // the source would then never tombstone — a path left live, and
        // unproducible, for ever.
        let unproducible = self.store.audit_record().await.failed_paths;
        let repairing: std::collections::HashSet<u32> = unproducible
            .iter()
            .filter_map(|path| {
                path_in_set(&set, path)
                    .and_then(|p| family.parse(&scope_hex, p))
                    .map(|(id, _)| id)
            })
            .collect();
        //
        // `repair_only` names the ones in the diff for no other reason: held,
        // counted, and covered by `high_water` already.
        let mut repair_only: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for src_ref in src_refs {
            if repairing.contains(&src_ref.segment_id)
                && !diff
                    .to_upload
                    .iter()
                    .any(|r| r.segment_id == src_ref.segment_id)
            {
                diff.to_upload.push(src_ref.clone());
                repair_only.insert(src_ref.segment_id);
            }
        }

        for src_ref in &diff.to_upload {
            let pair = match self.fetch_verified_pair(tag, &scope_hex, src_ref).await {
                Ok(pair) => pair,
                // A repair-only segment is already held and counted, so
                // skipping it leaves every figure the check-in carries true,
                // and its flag stays on the repair list for the next pass.
                // Aborting instead would cost the tombstones, the mirror, every
                // other segment and the check-in — every pass, for as long as
                // the source cannot serve it. A segment this store has NEVER
                // held still stops the pass: a later one would raise
                // `high_water` over the gap.
                Err(e) if repair_only.contains(&src_ref.segment_id) => {
                    tracing::warn!(
                        destination_id = %self.destination_id,
                        kind,
                        segment_id = src_ref.segment_id,
                        error = %e,
                        "custodian repair fetch failed; the flag stands and the next pass retries"
                    );
                    out.unrepaired_segments.push(src_ref.segment_id);
                    continue;
                }
                Err(e) => return Err(e),
            };
            // Sealed off the staged files a chunk at a time, the bodies staged
            // beside them until the put: the pass holds a manifest per half,
            // never a half (`message-segment-store.md` § Segment size).
            let seal = Some((seal_root, None));
            let sealed_dat =
                StagedSeal::seal_file(&pair.dat_path(), &pair.dir().join("dat.sealed"), seal)?;
            let sealed_meta =
                StagedSeal::seal_file(&pair.meta_path(), &pair.dir().join("meta.sealed"), seal)?;

            // The pair is admitted or refused as ONE unit: a `.dat` held
            // without its `.meta` is the exact half-corpus this widening
            // exists to end, so a cap that fits one half and not the other
            // stores neither. Charged at what the pair ADDS: a half already
            // live at this manifest costs nothing, so a repair that converges
            // is admitted even at the cap it grows by nothing.
            let dat_path = Self::segment_path(&set, family, &scope_hex, src_ref.segment_id);
            let meta_path = Self::meta_path(&set, family, &scope_hex, src_ref.segment_id);
            let pair_bytes = self
                .store
                .charge_for(&dat_path, &sealed_dat)
                .await?
                .saturating_add(self.store.charge_for(&meta_path, &sealed_meta).await?);
            if pair_bytes > 0 && !budget.admits(pair_bytes, self.cap_bytes) {
                // Stop pulling and say so — never evict a live path to make room.
                out.skipped_for_cap.push(src_ref.segment_id);
                budget.cap_state = CapState::Reached;
                continue;
            }

            self.store
                .put(&dat_path, &sealed_dat, SourceFacts::of(src_ref), now)
                .await?;
            self.store
                .put(
                    &meta_path,
                    &sealed_meta,
                    Self::meta_facts(pair.meta_len(), src_ref),
                    now,
                )
                .await?;
            // A flagged pair was re-fetched precisely because its local bytes
            // are absent or wrong — which is the one thing the convergent put
            // above trusts they are not. Rewrite them, whether the put above
            // recorded a new generation or converged on the held one: either
            // way the bytes now in hand are what the path should hold, and
            // this arm runs only for a flagged segment. `put_repair` declines
            // outright if a concurrent pass made some other generation live in
            // between, which is the one case rewriting would be wrong.
            if repairing.contains(&src_ref.segment_id) {
                self.store.put_repair(&dat_path, &sealed_dat).await?;
                self.store.put_repair(&meta_path, &sealed_meta).await?;
            }
            budget.held_bytes = budget.held_bytes.saturating_add(pair_bytes);
            out.stored_segments.push(src_ref.segment_id);
        }

        // The mirror this pass will store, sealed once. Its content is fixed by
        // `src_listing` — nothing from here to step 5 changes it — which is
        // what lets the sidecar backfill below store it FIRST and step 5 re-put
        // the identical bytes as a convergent no-op. Its generation is the
        // source's saved counter (`live_manifest_mirror`), the same choice the
        // nest-hosted writer makes.
        let mirror = live_manifest_mirror(src_listing);
        let mirror_sealed = crate::seal::seal_blob(&mirror.to_bytes()?, Some((seal_root, None)))?;

        // 3a′ — every listed ref carries its sidecar's hash (a listing without
        // one is refused at decode), so every backfill is anchorable.
        let mut to_backfill_meta: Vec<_> = diff.to_backfill_meta.iter().collect();

        // 3b′ — the mirror that will NAME those sidecars lands before they do.
        //
        // The corpus mirror is the only thing binding a sidecar to its `.dat`
        // (`docs/goal/architecture/message-segment-store.md` § Cross-location
        // backup protocol → *A sidecar in the corpus is anchored, or it is not
        // restored*). Stored last, it was the largest single blob behind the
        // smallest ones, so a capped custodian converged on a **steady** state
        // of sidecars its own mirror could not name — and stayed
        // there, since `ReclaimPlan::admits` answers `false` for the rest of a
        // pass once `cap_state` is `Reached`, and the next pass starts from the
        // same cap. Ordering fixes it without touching the cap, which is the
        // kind's only knob (`../behavior/backup-destinations.md`): if the mirror
        // does not fit, no sidecar is placed at all. The residual state is then
        // always the safe one — the mirror naming a half not yet held, which is
        // the designed not-yet-caught-up refusal — including after a crash
        // anywhere in the loop below.
        let mirror_path = Self::mirror_path(&set, family, &scope_hex, kind);
        let mirror_charge = self.store.charge_for(&mirror_path, &mirror_sealed).await?;
        if !to_backfill_meta.is_empty() {
            if mirror_charge == 0 || budget.admits(mirror_charge, self.cap_bytes) {
                if self
                    .store
                    .put(&mirror_path, &mirror_sealed, SourceFacts::default(), now)
                    .await?
                {
                    out.mirror_stored = true;
                    budget.held_bytes = budget.held_bytes.saturating_add(mirror_charge);
                }
            } else {
                for src_ref in &to_backfill_meta {
                    out.skipped_for_cap.push(src_ref.segment_id);
                }
                budget.cap_state = CapState::Reached;
                // No sidecar may be placed that the mirror cannot name.
                to_backfill_meta.clear();
            }
        }

        // 3b — the sidecar backfill: segments whose `.dat` this store already
        // holds without a `.meta` (a crash between the two puts). Only the `.meta`
        // moves; the `.dat` stays exactly where it is. Fires once per such
        // segment, then never again — the next `local_state` sees the pair.
        for src_ref in to_backfill_meta {
            let meta = self
                .source
                .segment_meta_bytes(tag, &scope_hex, src_ref.segment_id)
                .await
                .with_context(|| {
                    format!(
                        "custodian sidecar backfill kind={kind} scope={scope_hex} seg={}",
                        src_ref.segment_id
                    )
                })?;
            verify_meta_against(&meta, src_ref)?;
            let sealed_meta = crate::seal::seal_blob(&meta, Some((seal_root, None)))?;
            if !budget.admits(sealed_meta.stored_bytes(), self.cap_bytes) {
                out.skipped_for_cap.push(src_ref.segment_id);
                budget.cap_state = CapState::Reached;
                continue;
            }
            self.store
                .put(
                    &Self::meta_path(&set, family, &scope_hex, src_ref.segment_id),
                    &sealed_meta,
                    Self::meta_facts(meta.len() as u64, src_ref),
                    now,
                )
                .await?;
            budget.held_bytes = budget.held_bytes.saturating_add(sealed_meta.stored_bytes());
            out.backfilled_meta.push(src_ref.segment_id);
        }

        // 4 — compacted-out paths. A tombstone ends liveness while what it covers
        // ages out on the ordinary grace clock, so a source that "deletes"
        // everything cannot make this device drop the bytes today. Both halves
        // tombstone; the sidecar's only when a live one is actually held (a
        // dat-only store holds none).
        for segment_id in &diff.to_drop {
            self.store
                .put_tombstone(
                    &Self::segment_path(&set, family, &scope_hex, *segment_id),
                    now,
                )
                .await?;
            let meta_path = Self::meta_path(&set, family, &scope_hex, *segment_id);
            if CustodianStore::live_at(&rows, &meta_path).is_some() {
                self.store.put_tombstone(&meta_path, now).await?;
            }
            out.tombstoned_segments.push(*segment_id);
        }

        // 5 — the manifest mirror, sealed above. The convergent seal is its own
        // change detector: unchanged content re-seals to the same manifest hash,
        // which `put` already treats as "already held" — so no side-table of
        // prior hashes has to be kept in step with the store, and a mirror the
        // backfill above already stored costs nothing here. `|=` rather than `=`
        // for exactly that: "stored this pass" is either store, not the last one.
        // Charged as step 3b′ charged it — at what it adds, which a mirror the
        // backfill already stored, or an unchanged one, does not.
        let mirror_charge = self.store.charge_for(&mirror_path, &mirror_sealed).await?;
        if mirror_charge == 0 || budget.admits(mirror_charge, self.cap_bytes) {
            out.mirror_stored |= self
                .store
                .put(&mirror_path, &mirror_sealed, SourceFacts::default(), now)
                .await?;
            // The mirror needs no diff arm — step 3 re-seals it from `src_refs`
            // every pass, so the bytes are already in hand — but it does need
            // the repair, and it needs it badly: the mirror is the only thing
            // binding a sidecar to its `.dat`, so a rotted one makes every pair
            // it names unrestorable.
            if unproducible.contains(&mirror_path) {
                self.store.put_repair(&mirror_path, &mirror_sealed).await?;
            }
        } else {
            budget.cap_state = CapState::Reached;
        }

        Ok(out)
    }

    /// One as-is mirror pass over one covered folder
    /// (`docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage).
    ///
    /// The head arrives already sealed per the folder's audience, so — unlike
    /// [`Self::run_once`]'s step 3 — there is **no seal step here**: each
    /// path's manifest + chunks are fetched by hash, hash-verified (the plane's
    /// ratified audit floor), and stored verbatim under
    /// `{folder_set}/{path_hash_hex}` — the same layout the destination set
    /// carries, so restore tooling addresses both alike. Paths the head no
    /// longer lists tombstone onto the ordinary grace clock, and the cap is
    /// enforced with the same stop-and-report rule as the segment axis.
    ///
    /// The folder's display name is recorded first, before any byte: it comes
    /// off the coverage listing, not the head, and a re-seed restores the
    /// folder under it (`CustodianStore::put_folder_name`) — after a box loss
    /// this store is the only place the label survives.
    pub async fn run_folder_once(
        &self,
        folders: &dyn crate::segment_backup::FolderCorpusSource,
        covered: &CoveredFolder,
        now: i64,
    ) -> Result<FolderPullReport> {
        let mut report = FolderPullReport::default();
        // The address and sealed label — all a listing carries for the set once
        // the source's row holds no plaintext name.
        let label = match (&covered.name_hash, &covered.name_sealed) {
            (Some(hash), Some(sealed)) if !sealed.is_empty() => <[u8; 32]>::try_from(&hash[..])
                .ok()
                .map(|h| (h, &sealed[..])),
            _ => None,
        };
        // The display name: the listing's plaintext while it still carries one,
        // else the sealed label opened under the owner key this custodian seals
        // with — the owner's own seal, read on the owner's own device
        // (`segment-backup-protocol.md` § *Where a restored folder's name comes
        // from*). A label this key cannot open (a set sealed under a group
        // generation) records no name, and a re-seed reports the set unnamed.
        let name = covered
            .name
            .as_deref()
            .filter(|n| !n.is_empty())
            .map(str::to_string)
            .or_else(|| {
                let (hash, sealed) = label?;
                fauna_core::label_custody::render_set_name(
                    &fauna_core::file_download::FileDownloadKeys::owner(self.seal_key.clone()),
                    Some(sealed),
                    "",
                    Some(&hash[..]),
                )
                .text()
                .map(str::to_string)
            });
        if let Some(name) = name.as_deref() {
            self.store
                .put_folder_name(&covered.folder_set, name)
                .await?;
        }
        if let Some((hash, sealed)) = label {
            self.store
                .put_folder_label(&covered.folder_set, &hash, sealed)
                .await?;
        }
        let head = folders.folder_head(covered.folder_id).await?;

        let rows = self.store.held().await?;
        let policy: Vec<_> = rows.iter().map(HeldRow::to_policy).collect();
        let plan = plan_reclaim(&policy, self.cap_bytes, now);
        let mut budget = ReclaimPlan {
            reclaim: Vec::new(),
            held_bytes: plan.held_bytes,
            cap_state: plan.cap_state,
        };

        // This folder's live local paths → held manifest hex, via the store's
        // one liveness derivation.
        let mut local: HashMap<String, String> = HashMap::new();
        for row in &rows {
            if path_in_set(&covered.folder_set, &row.path).is_none() {
                continue;
            }
            if let Some(live) = CustodianStore::live_at(&rows, &row.path)
                && !live.manifest_hash.is_empty()
            {
                local.insert(row.path.clone(), live.manifest_hash.clone());
            }
        }

        // The folder axis's own arm of the repair above. Its change detector
        // is an early-out *before* the fetch, so a mirrored path whose bytes
        // rotted is unrepairable by exactly the same mechanism the segment
        // diff was.
        let unproducible = self.store.audit_record().await.failed_paths;

        let mut live_paths: std::collections::HashSet<String> = Default::default();
        for entry in &head {
            let path = held_path(&covered.folder_set, &hex::encode(&entry.path_hash));
            live_paths.insert(path.clone());
            let manifest_hex = hex::encode(entry.manifest_hash.digest());
            let head_sealed = entry.path_sealed.as_ref().map(hex::encode);
            if local.get(&path) == Some(&manifest_hex) && !unproducible.contains(&path) {
                // Content unchanged — no bytes to move.
                continue;
            }

            let manifest_bytes = folders.manifest_bytes(&entry.manifest_hash).await?;
            if ContentHash::of_raw(&manifest_bytes) != entry.manifest_hash {
                anyhow::bail!(
                    "manifest {} failed hash verification — refusing to store corrupt bytes",
                    manifest_hex
                );
            }
            let manifest: fauna_core::chunk::ChunkManifest =
                fauna_core::encoding::canonical_decode(&manifest_bytes)
                    .map_err(|e| anyhow::anyhow!("manifest {manifest_hex} does not decode: {e}"))?;
            let mut chunks = Vec::new();
            for key in manifest.store_keys() {
                let body = folders.chunk_bytes(&key).await?;
                if ContentHash::of_raw(&body) != key {
                    anyhow::bail!(
                        "chunk {} failed hash verification — refusing to store corrupt bytes",
                        hex::encode(key.digest())
                    );
                }
                chunks.push((key, body));
            }
            let sealed = crate::seal::SealedBlob {
                manifest,
                manifest_bytes,
                manifest_hash: entry.manifest_hash,
                chunks,
                content_key_version: None,
            };

            if !budget.admits(sealed.stored_bytes(), self.cap_bytes) {
                report.skipped_for_cap += 1;
                budget.cap_state = CapState::Reached;
                continue;
            }
            self.store
                .put_folder_row(
                    &path,
                    &sealed,
                    SourceFacts::of_folder_head(entry),
                    head_sealed.clone(),
                    now,
                )
                .await?;
            if unproducible.contains(&path) {
                self.store.put_repair(&path, &sealed).await?;
            }
            budget.held_bytes = budget.held_bytes.saturating_add(sealed.stored_bytes());
            report.stored_paths += 1;
        }

        for path in local.keys().filter(|p| !live_paths.contains(*p)) {
            self.store.put_tombstone(path, now).await?;
            report.tombstoned_paths += 1;
        }

        tracing::info!(
            destination_id = %self.destination_id,
            folder_id = covered.folder_id,
            stored = report.stored_paths,
            tombstoned = report.tombstoned_paths,
            skipped_for_cap = report.skipped_for_cap,
            "CustodianPull: covered-folder pass complete",
        );
        Ok(report)
    }

    /// One pass over every folder this custodian's own destination row covers
    /// — a no-op without a [`Self::folder_source`]. Per-folder failures are
    /// logged and the loop continues, like the kind loop.
    pub async fn run_covered_folders(&self, now: i64) -> Vec<FolderPullReport> {
        let Some(folders) = self.folder_source else {
            return Vec::new();
        };
        let covered = match self.backup.destination_list().await {
            Ok(reply) => reply
                .destinations
                .into_iter()
                .find(|d| d.destination_id == self.destination_id)
                .map(|d| d.covered_folders)
                .unwrap_or_default(),
            Err(e) => {
                tracing::warn!(
                    destination_id = %self.destination_id,
                    error = %e,
                    "CustodianPull: coverage read failed; skipping the folder axis"
                );
                return Vec::new();
            }
        };
        let mut reports = Vec::new();
        for cf in &covered {
            match self.run_folder_once(folders, cf, now).await {
                Ok(report) => reports.push(report),
                Err(e) => tracing::warn!(
                    destination_id = %self.destination_id,
                    folder_id = cf.folder_id,
                    error = %e,
                    "CustodianPull: covered-folder pass failed; continuing"
                ),
            }
        }
        reports
    }

    /// One pass over **every** backed-up kind, at wall-clock `now`.
    ///
    /// Per-kind failures are logged and the loop continues, exactly as the
    /// nest arm's `run_all_tuples` does for the upload side: one unreachable
    /// kind must not stop a device from pulling the rest of the owner's
    /// corpus, and the next pass retries it anyway.
    /// The self-audit runs here, **before** the kinds, so the check-ins those
    /// passes make already carry this pass's verdict rather than the previous
    /// one's. The covered-folder axis runs before the kinds too, so the kind
    /// passes' check-ins already report the folder bytes in `held_bytes`.
    ///
    /// Every kind is listed and compared ([`Self::prepare`]) before the folder
    /// axis runs: a source whose counter went below this store's ledger for
    /// any kind is a box that lost its corpus or went back to an older copy,
    /// and its coverage head would tombstone this device's folder mirrors just
    /// as its segment listing would the segments — so the covered-folder passes
    /// are held with the refused kinds.
    pub async fn run_all_kinds(&self, now: i64) -> Vec<PullReport> {
        self.audit_if_due(now).await;

        let mut prepared = Vec::new();
        for kind in crate::segment_backup::BACKED_UP_KINDS {
            match self.prepare(kind, now).await {
                Ok(p) => prepared.push((kind, p)),
                Err(e) => tracing::warn!(
                    destination_id = %self.destination_id,
                    kind,
                    error = %e,
                    "CustodianPull: per-kind pass failed; continuing"
                ),
            }
        }
        if prepared.iter().any(|(_, p)| p.regressed.is_some()) {
            tracing::warn!(
                destination_id = %self.destination_id,
                "CustodianPull: covered-folder passes held — the source went below this device's copy"
            );
        } else {
            let _ = self.run_covered_folders(now).await;
        }

        let mut reports = Vec::new();
        for (kind, prepared) in prepared {
            match self.run_prepared(kind, prepared, now).await {
                Ok(report) => reports.push(report),
                Err(e) => tracing::warn!(
                    destination_id = %self.destination_id,
                    kind,
                    error = %e,
                    "CustodianPull: per-kind pass failed; continuing"
                ),
            }
        }

        // Last, because it grades what the passes above just did: every flagged
        // path this store can produce again comes off the repair list, and one
        // it still cannot stays on it for the next pass to retry. Never
        // propagates — a repair list that could not be trimmed costs one
        // redundant re-fetch, and refusing the pass over it would turn a
        // bookkeeping fault into a stalled backup.
        if let Err(e) = self
            .store
            .clear_repaired(&FileDownloadKeys::owner(self.seal_key.clone()))
            .await
        {
            tracing::warn!(
                destination_id = %self.destination_id,
                error = %e,
                "custodian repair list could not be trimmed; the next pass retries"
            );
        }
        reports
    }

    /// Self-audit this device's store if one is due, and persist the verdict.
    ///
    /// Debounced to [`AUDIT_MIN_INTERVAL_SECS`] — the same floor the owner-side
    /// loop uses, taken by reference rather than as a custodian-specific sibling
    /// that would be free to drift. A pull pass runs every 15 minutes; opening
    /// `AUDIT_SAMPLE_K` files that often would spend the device's battery
    /// re-answering a question whose answer changes on the scale of days.
    ///
    /// **Never propagates a failure.** An audit that cannot even run (the index
    /// went unreadable) must not stop the pull: refusing to pull because the
    /// audit broke would convert an observability fault into data loss. The
    /// verdict is simply not advanced, and the standing one keeps being
    /// reported.
    async fn audit_if_due(&self, now: i64) {
        let record = self.store.audit_record().await;
        if let Some(last) = record.last_run_at
            && now.saturating_sub(last) < AUDIT_MIN_INTERVAL_SECS
        {
            return;
        }

        let keys = FileDownloadKeys::owner(self.seal_key.clone());
        let report = match self
            .store
            .self_audit(&keys, AUDIT_SAMPLE_K, now.max(0) as u64)
            .await
        {
            Ok(report) => report,
            Err(e) => {
                tracing::warn!(
                    destination_id = %self.destination_id,
                    error = %e,
                    "custodian self-audit could not run; keeping the standing verdict"
                );
                return;
            }
        };

        if report.passed() {
            tracing::debug!(
                destination_id = %self.destination_id,
                sampled = report.sampled(),
                "custodian self-audit passed"
            );
        } else {
            // The loudest surface a headless host has. The wire carries only the
            // verdict, so which generation rotted can be learned nowhere else.
            tracing::error!(
                destination_id = %self.destination_id,
                sampled = report.sampled(),
                failed = ?report
                    .failed_paths
                    .iter()
                    .map(|p| fauna_core::log_redact::log_path(p))
                    .collect::<Vec<_>>(),
                "custodian self-audit FAILED — this device cannot produce what it \
                 claims to hold; the owner's Backups page will show this row failing"
            );
        }

        if let Err(e) = self.store.record_audit(now, &report).await {
            tracing::warn!(
                destination_id = %self.destination_id,
                error = %e,
                "custodian self-audit verdict could not be persisted"
            );
        }
    }

    /// The always-on driver — periodic tick **and** push debounce. The desktop
    /// entry point (the per-user sync agent), where one driver owns both
    /// cadences.
    ///
    /// Runs at the **ratified global cadence**, not a custodian-specific one:
    /// `behavior/backup-destinations.md` § Scheduling settles that there is one coordinator
    /// cadence for every `(destination, kind, scope)` tuple, so this reuses
    /// [`crate::segment_backup::PERIODIC_INTERVAL`] and
    /// [`crate::segment_backup::PUSH_DEBOUNCE`] by reference rather than
    /// declaring a sibling constant that could drift from it. A custodian is a
    /// destination like any other; the only thing that inverts is which
    /// direction the bytes move.
    ///
    /// Terminate via `cancel`. Same single-call rule as the upload driver: the
    /// push subscriptions are not re-entrant.
    pub async fn run_forever(&self, cancel: CancellationToken) -> Result<()> {
        self.run_loop(cancel, true).await
    }

    /// The **push-debounce loop only** — [`Self::run_forever`] minus the
    /// periodic arm, for platforms whose cadence an OS scheduler owns (Android
    /// `WorkManager`, iOS `BGProcessingTask`). Those shells call
    /// [`Self::run_all_kinds`] from their scheduled task and use this only while
    /// foregrounded, so the in-process kick does not double the period the OS
    /// scheduler already drives.
    ///
    /// Unlike `run_forever` this does **not** self-exit when the source
    /// disconnects — with no periodic tick nothing wakes the loop to observe the
    /// closed pumps — so the foreground holder MUST cancel on teardown.
    pub async fn run_push_debounce(&self, cancel: CancellationToken) -> Result<()> {
        self.run_loop(cancel, false).await
    }

    async fn run_loop(&self, cancel: CancellationToken, periodic: bool) -> Result<()> {
        let notify = Arc::new(Notify::new());
        let (pump_seg, pump_mail) = crate::segment_backup::spawn_push_pumps(
            self.source,
            self.scope_id,
            crate::segment_backup::BACKED_UP_KINDS,
            Arc::clone(&notify),
            "CustodianPull",
        );

        let mut interval = tokio::time::interval(crate::segment_backup::PERIODIC_INTERVAL);
        // The first tick fires immediately; mute it so the loop body controls
        // when the first pass lands.
        interval.tick().await;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick(), if periodic => {
                    self.run_all_kinds(now_secs()).await;
                }
                _ = notify.notified() => {
                    fauna_core::debounce::absorb_burst(
                        &notify,
                        &cancel,
                        crate::segment_backup::PUSH_DEBOUNCE,
                    )
                    .await;
                    if !cancel.is_cancelled() {
                        self.run_all_kinds(now_secs()).await;
                    }
                }
                else => break,
            }

            // A source with no pumps at all has no subscription to lose, so it
            // runs on the periodic timer until cancelled.
            let pumps_ended = match (&pump_seg, &pump_mail) {
                (Some(seg), Some(mail)) => seg.is_finished() && mail.is_finished(),
                _ => false,
            };
            if pumps_ended {
                // Both pumps ended: the source disconnected and nothing is going
                // to revive the subscribers. Drain one final pass — the check-in
                // it writes is what keeps this device's status row from ageing
                // into `overdue` over a restart — then exit.
                if !cancel.is_cancelled() {
                    self.run_all_kinds(now_secs()).await;
                }
                break;
            }
        }

        for pump in [pump_seg, pump_mail].into_iter().flatten() {
            pump.abort();
        }
        Ok(())
    }
}

/// Wall-clock unix seconds, read **only** by the driver loop.
///
/// Every pass takes `now` as a parameter (convention 14), so the scheduler and
/// the tests drive one code path; this is the single place the real clock enters
/// it, and it is deliberately outside anything a test needs to reach.
fn now_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs_or_zero()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::crypto::BackupKey;
    use fauna_core::file_download::FileDownloadKeys;
    use fauna_protocol::backup::{
        AUDIT_STATE_FAILED, AUDIT_STATE_OK, CAP_STATE_OK, CAP_STATE_REACHED,
        CustodianCheckinRequest,
    };
    use fauna_protocol::segments::SegmentRef;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    const SCOPE: [u8; 32] = [0xAB; 32];
    const DAY: i64 = 24 * 60 * 60;

    fn scope_hex() -> String {
        hex::encode(SCOPE)
    }

    /// The store row holding `path_in_set` of the `__mail` set — what every
    /// mail-only test here opens, looks up and flags.
    fn in_mail(path_in_set: &str) -> String {
        held_path("__mail", path_in_set)
    }

    fn seed_key() -> BackupKey {
        BackupKey::from_bytes([5u8; 32])
    }

    /// The sidecar a fake source pairs with a `.dat` body: distinct bytes per
    /// segment, opaque to everything under test (exactly as a real one is).
    fn fake_meta(segment_id: u32, body: &[u8]) -> Vec<u8> {
        format!("meta-of-{segment_id}-{}", body.len()).into_bytes()
    }

    /// A source nest holding a fixed set of segment **pairs**.
    #[derive(Default)]
    struct FakeSource {
        segments: Mutex<BTreeMap<u32, crate::segment_backup::SegmentPair>>,
        /// Segment ids whose pair was fetched, in order — proof of what the
        /// diff actually decided to pull.
        fetched: Mutex<Vec<u32>>,
        /// Segment ids whose sidecar ALONE was fetched — the backfill door.
        meta_fetched: Mutex<Vec<u32>>,
        /// When set, the listing advertises THIS sidecar hash instead of the
        /// real one — a source whose listing and bytes disagree.
        lie_about_meta: Mutex<Option<String>>,

        /// Segment ids the source lists but whose pair fetch fails — a torn
        /// connection, or a body the source can no longer serve.
        unfetchable: Mutex<std::collections::HashSet<u32>>,
        /// The kind's **placement journal**, served under its own tag. Every
        /// rig starts with an EMPTY journal, so a test that never mentions a
        /// journal stages a source with content and no placement segments.
        journal: Mutex<BTreeMap<u32, crate::segment_backup::SegmentPair>>,
        /// Journal segment ids whose pair was fetched, in order.
        journal_fetched: Mutex<Vec<u32>>,
        /// Content served under one named kind INSTEAD of [`Self::segments`] —
        /// the other kinds a real source serves beside mail, each with bytes of
        /// its own. A kind absent here serves `segments`, so a test that never
        /// mentions another kind stages exactly the source it always did.
        per_kind: Mutex<HashMap<String, BTreeMap<u32, crate::segment_backup::SegmentPair>>>,
        /// The saved counter each served tag has reached — raised by every
        /// listing to its greatest listed id plus one and never lowered, as a
        /// real source's `KindManifest::next_seg_id` is while its data
        /// directory lives. So removing a segment (a compaction) keeps it.
        counters: Mutex<HashMap<String, u32>>,
        /// A counter served for a tag INSTEAD of [`Self::counters`] — a box
        /// rebuilt empty, or one gone back to an older copy. Cleared by
        /// [`Self::serve_saved_counter`].
        counter_override: Mutex<HashMap<String, u32>>,
    }

    /// The tag a fake source serves the journal under.
    const JOURNAL_TAG: &str = "mail-placement";

    impl FakeSource {
        /// Give this source a journal segment (and so a journal at all).
        fn set_journal(&self, segment_id: u32, body: Vec<u8>) {
            let meta = fake_meta(segment_id, &body);
            self.journal.lock().unwrap().insert(
                segment_id,
                crate::segment_backup::SegmentPair { dat: body, meta },
            );
        }

        fn remove_journal(&self, segment_id: u32) {
            self.journal.lock().unwrap().remove(&segment_id);
        }

        fn journal_fetched(&self) -> Vec<u32> {
            self.journal_fetched.lock().unwrap().clone()
        }

        /// The pairs served under `kind`: the content's, or the journal's. Only
        /// mail's journal is stageable; every other kind's journal tag is
        /// served EMPTY, as a real nest serves a journal with no segments yet.
        fn pairs(&self, kind: &str) -> Result<BTreeMap<u32, crate::segment_backup::SegmentPair>> {
            if kind == JOURNAL_TAG {
                return Ok(self.journal.lock().unwrap().clone());
            }
            if matches!(
                SegmentFamily::from_serve_kind(kind),
                Some((SegmentFamily::Placement, _))
            ) {
                return Ok(BTreeMap::new());
            }
            if let Some(pairs) = self.per_kind.lock().unwrap().get(kind) {
                return Ok(pairs.clone());
            }
            Ok(self.segments.lock().unwrap().clone())
        }

        /// Serve `segments` as `kind`'s content, with sidecars derived as
        /// [`Self::set`] derives them.
        fn set_kind(&self, kind: &str, segments: &[(u32, Vec<u8>)]) {
            self.per_kind.lock().unwrap().insert(
                kind.to_string(),
                segments
                    .iter()
                    .map(|(id, body)| {
                        let meta = fake_meta(*id, body);
                        (
                            *id,
                            crate::segment_backup::SegmentPair {
                                dat: body.clone(),
                                meta,
                            },
                        )
                    })
                    .collect(),
            );
        }

        fn with(segments: &[(u32, Vec<u8>)]) -> Self {
            let src = Self::default();
            for (id, body) in segments {
                src.set(*id, body.clone());
            }
            src
        }

        /// A segment whose sidecar is derived from its body (`fake_meta`).
        fn set(&self, segment_id: u32, body: Vec<u8>) {
            let meta = fake_meta(segment_id, &body);
            self.set_pair(segment_id, body, meta);
        }

        /// A segment with an explicit sidecar — the real-pair test's door.
        fn set_pair(&self, segment_id: u32, dat: Vec<u8>, meta: Vec<u8>) {
            self.segments
                .lock()
                .unwrap()
                .insert(segment_id, crate::segment_backup::SegmentPair { dat, meta });
        }

        fn remove(&self, segment_id: u32) {
            self.segments.lock().unwrap().remove(&segment_id);
        }

        /// Serve `counter` as `kind`'s saved counter from now on, whatever the
        /// source has listed.
        fn serve_counter(&self, kind: &str, counter: u32) {
            self.counter_override
                .lock()
                .unwrap()
                .insert(kind.to_string(), counter);
        }

        /// Back to serving the counter the source actually reached.
        fn serve_saved_counter(&self, kind: &str) {
            self.counter_override.lock().unwrap().remove(kind);
        }

        /// The box rebuilt empty: every content segment gone, and every
        /// kind's counter back at zero.
        fn rebuild_empty(&self) {
            self.segments.lock().unwrap().clear();
            self.per_kind.lock().unwrap().clear();
            for kind in crate::segment_backup::BACKED_UP_KINDS {
                self.serve_counter(kind, 0);
            }
        }

        fn fetched(&self) -> Vec<u32> {
            self.fetched.lock().unwrap().clone()
        }

        fn meta_fetched(&self) -> Vec<u32> {
            self.meta_fetched.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl SegmentSource for FakeSource {
        fn source_id(&self) -> &str {
            "fake-source"
        }

        async fn list_segments(&self, kind: &str, _scope_hex: &str) -> Result<SegmentListing> {
            let lie = self.lie_about_meta.lock().unwrap().clone();
            let segments = self
                .pairs(kind)?
                .iter()
                .map(|(id, pair)| SegmentRef {
                    segment_id: *id,
                    blake3_hex: hex::encode(blake3::hash(&pair.dat).as_bytes()),
                    bucket: "2026-08".into(),
                    record_count: 3,
                    tombstone_count: 0,
                    size_bytes: pair.dat.len() as u64,
                    created_at_secs: 0,
                    is_open: false,
                    meta_blake3_hex: lie
                        .clone()
                        .unwrap_or_else(|| hex::encode(blake3::hash(&pair.meta).as_bytes())),
                    extra: Default::default(),
                })
                .collect::<Vec<_>>();
            let listed = segments.iter().map(|r| r.segment_id + 1).max().unwrap_or(0);
            let reached = {
                let mut counters = self.counters.lock().unwrap();
                let counter = counters.entry(kind.to_string()).or_insert(0);
                *counter = (*counter).max(listed);
                *counter
            };
            let next_segment_id = self
                .counter_override
                .lock()
                .unwrap()
                .get(kind)
                .copied()
                .unwrap_or(reached);
            Ok(SegmentListing {
                segments,
                next_segment_id,
            })
        }

        async fn segment_pair(
            &self,
            kind: &str,
            _scope_hex: &str,
            segment_id: u32,
        ) -> Result<crate::segment_backup::SegmentPair> {
            if kind == JOURNAL_TAG {
                self.journal_fetched.lock().unwrap().push(segment_id);
            } else {
                self.fetched.lock().unwrap().push(segment_id);
                if self.unfetchable.lock().unwrap().contains(&segment_id) {
                    anyhow::bail!("segment {segment_id} could not be fetched");
                }
            }
            self.pairs(kind)?
                .get(&segment_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no such segment {segment_id}"))
        }

        async fn segment_meta_bytes(
            &self,
            kind: &str,
            _scope_hex: &str,
            segment_id: u32,
        ) -> Result<Vec<u8>> {
            if kind != JOURNAL_TAG {
                self.meta_fetched.lock().unwrap().push(segment_id);
            }
            self.pairs(kind)?
                .get(&segment_id)
                .map(|p| p.meta.clone())
                .ok_or_else(|| anyhow::anyhow!("no such segment {segment_id}"))
        }
    }

    /// Captures the check-in payload — the thing the user's other devices see —
    /// and answers the coverage read the folder axis makes.
    #[derive(Default)]
    struct FakeNest {
        checkins: Mutex<Vec<CustodianCheckinRequest>>,
        covered_folders: Mutex<Vec<CoveredFolder>>,
        /// How the next check-ins are answered; accepted when `None`.
        checkin_fault: Mutex<Option<CheckinFault>>,
    }

    /// A check-in the fake nest does not accept.
    #[derive(Clone)]
    enum CheckinFault {
        /// The nest answered with this refusal (`Reply.ok = false`).
        Refused(fauna_protocol::RpcError),
        /// The request never got an answer — a transport fault.
        TimedOut,
    }

    impl FakeNest {
        fn last(&self) -> CustodianCheckinRequest {
            self.checkins
                .lock()
                .unwrap()
                .last()
                .cloned()
                .expect("a pass always checks in")
        }
    }

    /// The real transport's error type, so the pull's refusal classifier
    /// ([`RpcErrorClass::as_rpc_error`]) runs here exactly as it does in
    /// production.
    impl RpcRequester for FakeNest {
        type Error = fauna_client::NestClientError;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, fauna_client::NestClientError>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            use fauna_client::NestClientError;
            let decode = |e: serde_json::Error| NestClientError::Decode(e.to_string());
            let raw = serde_json::to_vec(&payload).map_err(decode)?;
            match kind {
                fauna_client_backup::KIND_CUSTODIAN_CHECKIN => {
                    match self.checkin_fault.lock().unwrap().clone() {
                        Some(CheckinFault::Refused(rpc)) => return Err(NestClientError::Rpc(rpc)),
                        Some(CheckinFault::TimedOut) => return Err(NestClientError::RpcTimeout),
                        None => {}
                    }
                    self.checkins
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(&raw).map_err(decode)?);
                    serde_json::from_str(r#"{"ok":true}"#).map_err(decode)
                }
                fauna_client_backup::KIND_DESTINATION_LIST => {
                    let reply = fauna_protocol::backup::DestinationListReply {
                        destinations: vec![fauna_protocol::backup::DestinationItem {
                            destination_id: "this-ipad".into(),
                            covered_folders: self.covered_folders.lock().unwrap().clone(),
                            ..Default::default()
                        }],
                        extra: Default::default(),
                    };
                    let raw = serde_json::to_vec(&reply).map_err(decode)?;
                    serde_json::from_slice(&raw).map_err(decode)
                }
                other => Err(NestClientError::Decode(format!("unexpected kind {other}"))),
            }
        }
    }

    struct Rig {
        _dir: tempfile::TempDir,
        source: FakeSource,
        nest: BackupClient<FakeNest>,
        store: CustodianStore,
        store_root: std::path::PathBuf,
    }

    impl Rig {
        fn new(segments: &[(u32, Vec<u8>)]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store_root = dir.path().join("custody");
            Self {
                store: CustodianStore::at(store_root.clone()),
                store_root,
                _dir: dir,
                source: FakeSource::with(segments),
                nest: BackupClient::new(FakeNest::default()),
            }
        }

        /// Rot this store: overwrite every sealed blob it holds, leaving the
        /// index untouched, so it goes on claiming generations it can no longer
        /// produce. Returns how many blobs were hit — a caller asserts on it,
        /// because rotting *nothing* would make an audit-failure test pass
        /// vacuously.
        ///
        /// An overwrite rather than a delete, deliberately: a delete fails a
        /// mere `stat` too, so only an overwrite proves the audit verifies the
        /// *content* it claims to hold.
        ///
        /// ⚠ It does **not** distinguish the store's two audit arms, and an
        /// earlier version of this comment claimed it did — measured 2026-08-21
        /// by routing `self_audit`'s sealed branch to `verify_presence` and
        /// watching both tests below stay GREEN. `verify_presence` is not a
        /// stat: it reads the manifest and every chunk and hash-verifies them
        /// against the addresses they are stored under, so rotted bytes fail it
        /// exactly as they fail `open`. What separates the arms is
        /// **decryption** — `open` also opens the sealed bytes under the
        /// owner's key. A test that wants to pin the sealed arm specifically
        /// must therefore corrupt the KEY, not the bytes.
        fn rot_every_blob(&self) -> usize {
            let mut hit = 0;
            let mut stack = vec![self.store_root.join("blobs")];
            while let Some(dir) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else {
                        std::fs::write(&path, b"rot").unwrap();
                        hit += 1;
                    }
                }
            }
            hit
        }

        fn pull(&self, cap: Option<u64>) -> CustodianPull<'_, FakeSource, FakeNest> {
            CustodianPull {
                source: &self.source,
                store: &self.store,
                backup: &self.nest,
                destination_id: "this-ipad".into(),
                scope_id: SCOPE,
                seal_key: OwnerSealKey::Client(seed_key()),
                cap_bytes: cap,
                device_id: Some("dev-ipad".into()),
                folder_source: None,
                not_assigned: None,
            }
        }

        fn nest_saw(&self) -> CustodianCheckinRequest {
            self.nest.transport().last()
        }

        /// Tear the rig down to **just the store on disk**: the source nest and
        /// the backup client are dropped here and cannot be reached again.
        ///
        /// This is what makes the standalone-restore test structural rather
        /// than a promise. A test that merely *doesn't call* the source proves
        /// nothing — the open path could reach for it and the test would still
        /// pass. Consuming the rig makes reaching for either a compile error.
        /// The `TempDir` comes back so the directory outlives the rig that
        /// created it.
        fn into_store_on_disk(self) -> (tempfile::TempDir, std::path::PathBuf) {
            let Self {
                _dir,
                source,
                nest,
                store,
                store_root,
            } = self;
            drop(source);
            drop(nest);
            drop(store);
            (_dir, store_root)
        }
    }

    /// An in-memory covered-folder byte plane: one folder's head + the blobs
    /// behind it, keyed exactly as the wire serves them.
    #[derive(Default)]
    struct FakeFolders {
        head: Mutex<Vec<crate::segment_backup::FolderHeadEntry>>,
        manifests: Mutex<HashMap<ContentHash, Vec<u8>>>,
        chunks: Mutex<HashMap<ContentHash, Vec<u8>>>,
    }

    impl FakeFolders {
        /// Stage one "file": already-sealed chunk bodies + the manifest keying
        /// them via `stored_hashes` (the sealed shape) — and its head entry.
        fn stage_file(&self, path_hash: [u8; 32], bodies: &[Vec<u8>]) -> ContentHash {
            let keys: Vec<ContentHash> = bodies.iter().map(|b| ContentHash::of_raw(b)).collect();
            for (k, b) in keys.iter().zip(bodies) {
                self.chunks.lock().unwrap().insert(*k, b.clone());
            }
            let manifest = fauna_core::chunk::ChunkManifest {
                file_hash: keys[0],
                total_size: bodies.iter().map(|b| b.len() as u64).sum(),
                chunk_hashes: keys.clone(),
                chunk_sizes: bodies.iter().map(|b| b.len() as u64).collect(),
                stored_hashes: Some(keys),
                sealed_hashes: None,
                min_reader: None,
            };
            let bytes = fauna_core::encoding::canonical_encode(&manifest).unwrap();
            let manifest_hash = ContentHash::of_raw(&bytes);
            self.manifests.lock().unwrap().insert(manifest_hash, bytes);
            let size = manifest.total_size as i64;
            let mut head = self.head.lock().unwrap();
            head.retain(|e| e.path_hash != path_hash.to_vec());
            head.push(crate::segment_backup::FolderHeadEntry {
                path_hash: path_hash.to_vec(),
                manifest_hash,
                size_bytes: size,
                // The fake source seals the path the way a real one does: the
                // sealed name is opaque bytes to every layer under test, so a
                // recognisable derivation keeps assertions readable.
                path_sealed: Some(format!("sealed:{}", hex::encode(path_hash)).into_bytes()),
            });
            manifest_hash
        }

        fn drop_path(&self, path_hash: [u8; 32]) {
            self.head
                .lock()
                .unwrap()
                .retain(|e| e.path_hash != path_hash.to_vec());
        }
    }

    #[async_trait::async_trait]
    impl crate::segment_backup::FolderCorpusSource for FakeFolders {
        async fn folder_head(
            &self,
            _folder_id: i64,
        ) -> Result<Vec<crate::segment_backup::FolderHeadEntry>> {
            Ok(self.head.lock().unwrap().clone())
        }

        async fn manifest_bytes(&self, manifest_hash: &ContentHash) -> Result<Vec<u8>> {
            self.manifests
                .lock()
                .unwrap()
                .get(manifest_hash)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no such manifest"))
        }

        async fn chunk_bytes(&self, store_key: &ContentHash) -> Result<Vec<u8>> {
            self.chunks
                .lock()
                .unwrap()
                .get(store_key)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no such chunk"))
        }
    }

    /// Deterministic, **incompressible** bytes (xorshift64).
    ///
    /// Not decoration: the cap tests reason about sealed sizes, and a
    /// compressible fixture seals a "20 KB" segment down to a few hundred bytes,
    /// so a cap meant to admit one segment and refuse the next silently admits
    /// both and the test proves nothing.
    fn body(tag: u8, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ (tag as u64 + 1);
        for _ in 0..len {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            out.push((x >> 24) as u8);
        }
        out
    }

    /// The whole point: a pass pulls the source's segments, seals them locally,
    /// and they open again with no nest in the picture.
    #[tokio::test]
    async fn a_pass_pulls_seals_and_stores_every_segment() {
        let one = body(1, 9_000);
        let two = body(2, 9_000);
        let rig = Rig::new(&[(1, one.clone()), (2, two.clone())]);

        let report = rig.pull(None).run_once("mail", 0).await.unwrap();

        assert_eq!(report.stored_segments, vec![1, 2]);
        assert!(report.mirror_stored);
        assert_eq!(report.cap_state, CapState::Ok);
        assert_eq!(report.high_water, 3, "max segment id 2, plus one");

        let keys = FileDownloadKeys::owner(seed_key());
        let hex = scope_hex();
        assert_eq!(
            rig.store
                .open(&in_mail(&format!("{hex}/seg-00000001.dat")), &keys)
                .await
                .unwrap(),
            one
        );
        assert_eq!(
            rig.store
                .open(&in_mail(&format!("{hex}/seg-00000002.dat")), &keys)
                .await
                .unwrap(),
            two
        );
        // …and each segment's sidecar beside it, sealed the same way.
        assert_eq!(
            rig.store
                .open(&in_mail(&format!("{hex}/seg-00000001.meta")), &keys)
                .await
                .unwrap(),
            fake_meta(1, &one)
        );
        assert_eq!(
            rig.store
                .open(&in_mail(&format!("{hex}/seg-00000002.meta")), &keys)
                .await
                .unwrap(),
            fake_meta(2, &two)
        );
    }

    // ── the kind's other family: the placement journal (2026-09-26) ──────────

    /// **The journal is pulled beside the content, and the two never collide.**
    ///
    /// Both families number their segments from 1 and this store is one flat
    /// path namespace, so segment 1 of each is the case that matters: each must
    /// open to its OWN bytes. A pull that filed the journal under the content's
    /// paths would overwrite the owner's mail with its mailbox index.
    #[tokio::test]
    async fn a_pass_pulls_the_placement_journal_beside_the_content() {
        let mail = body(1, 9_000);
        let journal = body(7, 2_000);
        let rig = Rig::new(&[(1, mail.clone())]);
        rig.source.set_journal(1, journal.clone());

        let report = rig.pull(None).run_once("mail", 0).await.unwrap();

        assert_eq!(report.stored_segments, vec![1], "the content family");
        let placement = report.placement.expect("the source served a journal");
        assert_eq!(placement.stored_segments, vec![1], "the journal family");
        assert!(placement.mirror_stored);
        assert_eq!(rig.source.journal_fetched(), vec![1]);

        let keys = FileDownloadKeys::owner(seed_key());
        let hex = scope_hex();
        let (content, placed) = (SegmentFamily::Content, SegmentFamily::Placement);
        assert_eq!(
            rig.store
                .open(&in_mail(&content.dat_path(&hex, 1)), &keys)
                .await
                .unwrap(),
            mail,
            "segment 1 of the content is still the mail"
        );
        assert_eq!(
            rig.store
                .open(&in_mail(&placed.dat_path(&hex, 1)), &keys)
                .await
                .unwrap(),
            journal,
            "segment 1 of the journal is the journal"
        );
        assert_eq!(
            rig.store
                .open(&in_mail(&placed.meta_path(&hex, 1)), &keys)
                .await
                .unwrap(),
            fake_meta(1, &journal)
        );
        // Each family's mirror names its own segments, and only those.
        for (family, body) in [(content, &mail), (placed, &journal)] {
            let mirror = LiveManifestMirror::from_bytes(
                &rig.store
                    .open(&in_mail(&family.mirror_path(&hex, "mail")), &keys)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(mirror.live.len(), 1, "{family:?}");
            assert_eq!(
                mirror.live[0].blake3_hex,
                hex::encode(blake3::hash(body).as_bytes()),
                "{family:?}"
            );
        }
    }

    /// **The check-in's high water is the content's, whatever the journal
    /// holds.** The nest measures it against its own content head, so a device
    /// that counted journal segments would report itself level with a nest it
    /// had pulled no mail from.
    #[tokio::test]
    async fn high_water_never_counts_a_journal_segment() {
        let rig = Rig::new(&[(1, body(1, 4_000))]);
        for id in 1..=5 {
            rig.source.set_journal(id, body(10 + id as u8, 1_000));
        }

        let report = rig.pull(None).run_once("mail", 0).await.unwrap();

        assert_eq!(
            report.placement.as_ref().unwrap().stored_segments,
            vec![1, 2, 3, 4, 5]
        );
        assert_eq!(report.high_water, 2, "one content segment, plus one");
        assert_eq!(
            rig.nest_saw().high_water,
            2,
            "and that is what the nest is told"
        );
    }

    // ── one store, several kinds (the set-qualified store, 2026-09-29) ───────
    //
    // Every kind's content family spells its segments identically
    // (`{scope_hex}/seg-00000001.dat`), so the set is the only thing keeping
    // one kind's rows from another's. Each test serves the other kinds with
    // bytes of their own: identical bytes would converge on the same sealed
    // generation and hide exactly the overwrite these pin against.

    /// **A second and third kind leave the first kind's rows alone.** A store
    /// holding mail, then pulling posts and calendar — each with fewer segments
    /// than mail, so a shared namespace would both overwrite mail's segment 1
    /// and tombstone the rest as compacted out — keeps every mail row live and
    /// byte-identical.
    #[tokio::test]
    async fn pulling_other_kinds_leaves_every_mail_row_live_and_unchanged() {
        let mail = [
            (1, body(1, 9_000)),
            (2, body(2, 9_000)),
            (3, body(3, 9_000)),
        ];
        let rig = Rig::new(&mail);
        rig.source.set_kind("post", &[(1, body(41, 7_000))]);
        rig.source
            .set_kind("calendar", &[(1, body(51, 5_000)), (2, body(52, 5_000))]);
        let pull = rig.pull(None);
        pull.run_once("mail", 0).await.unwrap();
        let before: Vec<HeldRow> = rig
            .store
            .held()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| path_in_set("__mail", &r.path).is_some())
            .collect();
        assert_eq!(
            before.len(),
            8,
            "three pairs, the content's mirror and the empty journal's mirror"
        );

        pull.run_once("post", 10).await.unwrap();
        pull.run_once("calendar", 20).await.unwrap();

        let rows = rig.store.held().await.unwrap();
        for row in &before {
            assert_eq!(
                CustodianStore::live_at(&rows, &row.path),
                Some(row),
                "{} is no longer the live mail generation",
                row.path
            );
        }
        let keys = FileDownloadKeys::owner(seed_key());
        let hex = scope_hex();
        for (id, bytes) in &mail {
            assert_eq!(
                &rig.store
                    .open(&in_mail(&format!("{hex}/seg-{id:08}.dat")), &keys)
                    .await
                    .unwrap(),
                bytes,
                "mail segment {id}"
            );
        }
        assert_eq!(
            rig.store
                .open(
                    &held_path("__post", &format!("{hex}/seg-00000001.dat")),
                    &keys
                )
                .await
                .unwrap(),
            body(41, 7_000),
            "and posts sit beside it, in their own set"
        );
    }

    /// **A qualified `__mail/…` row is never re-fetched** by a pull listing the
    /// same segment — a pass writes the qualified form, reads it back as its
    /// own, and fetches nothing twice.
    #[tokio::test]
    async fn a_qualified_mail_row_is_never_fetched_again() {
        let rig = Rig::new(&[(1, body(1, 9_000)), (2, body(2, 9_000))]);
        rig.pull(None).run_once("mail", 0).await.unwrap();
        let rows = rig.store.held().await.unwrap();
        assert!(
            rows.iter().all(|r| r.path.starts_with("__mail/")),
            "{:?}",
            rows.iter().map(|r| &r.path).collect::<Vec<_>>()
        );

        rig.pull(None).run_once("mail", 10).await.unwrap();
        assert_eq!(
            rig.source.fetched(),
            vec![1, 2],
            "fetched once each, never again"
        );
    }

    /// **`high_water` is the kind's own**, read straight off a store holding
    /// both kinds live: mail's segments 1, 2 and 7 beside posts' segment 1,
    /// every one spelled `{scope_hex}/seg-NNNNNNNN.dat` within its set.
    #[tokio::test]
    async fn high_water_for_one_kind_ignores_another_kinds_segments() {
        let rig = Rig::new(&[]);
        let hex = scope_hex();
        for (set, id) in [("__mail", 1), ("__mail", 2), ("__mail", 7), ("__post", 1)] {
            rig.store
                .put(
                    &held_path(set, &SegmentFamily::Content.dat_path(&hex, id)),
                    &crate::seal::seal_blob(&body(id as u8, 900), None).unwrap(),
                    SourceFacts::default(),
                    0,
                )
                .await
                .unwrap();
        }
        let rows = rig.store.held().await.unwrap();

        type Pull<'a> = CustodianPull<'a, FakeSource, FakeNest>;
        assert_eq!(
            Pull::high_water(&rows, "__post", &hex),
            2,
            "posts: segment 1, plus one"
        );
        assert_eq!(
            Pull::high_water(&rows, "__mail", &hex),
            8,
            "mail: segment 7, plus one"
        );
    }

    /// The same, end to end: a posts pass checks in the posts' own level. The
    /// nest compares it with its head for that `(scope, kind)`, so a posts pass
    /// must not count mail's segment ids as posts pulled.
    #[tokio::test]
    async fn a_posts_pass_checks_in_the_posts_own_high_water() {
        let rig = Rig::new(&[
            (1, body(1, 4_000)),
            (2, body(2, 4_000)),
            (7, body(7, 4_000)),
        ]);
        rig.source.set_kind("post", &[(1, body(41, 4_000))]);
        let pull = rig.pull(None);
        assert_eq!(pull.run_once("mail", 0).await.unwrap().high_water, 8);

        let post = pull.run_once("post", 10).await.unwrap();

        assert_eq!(post.high_water, 2, "posts hold segment 1 alone, plus one");
        assert_eq!(
            rig.nest_saw().high_water,
            2,
            "and that is what the nest is told"
        );
    }

    /// **An empty journal still has its mail pulled.** A source whose journal
    /// holds no segments yet lists it empty; the content moves and no journal
    /// segment does.
    #[tokio::test]
    async fn an_empty_journal_still_has_its_mail_pulled() {
        let rig = Rig::new(&[(1, body(1, 4_000))]);

        let report = rig.pull(None).run_once("mail", 0).await.unwrap();

        assert_eq!(report.stored_segments, vec![1]);
        let journal = report.placement.expect("mail has a journal family");
        assert!(
            journal.stored_segments.is_empty(),
            "an empty journal moves no segment"
        );
        assert!(rig.source.journal_fetched().is_empty());
        assert!(
            rig.store
                .held()
                .await
                .unwrap()
                .iter()
                .all(|row| !row.path.contains("/placement/seg-")),
            "and the store holds no journal segment"
        );
    }

    /// **A refused journal tag fails the pass.** Every nest serves each
    /// backed-up kind's journal, so a source refusing the journal's tag is an
    /// ordinary listing error — never read as "no journal here" — and the
    /// pass stores nothing rather than a journal-less half.
    #[tokio::test]
    async fn a_refused_journal_tag_fails_the_pass() {
        let dir = tempfile::tempdir().unwrap();
        let source = HalfBrokenSource {
            inner: FakeSource::with(&[(1, body(1, 4_000))]),
            broken_kind: JOURNAL_TAG,
        };
        let store = CustodianStore::at(dir.path().join("custody"));
        let nest = BackupClient::new(FakeNest::default());
        let pull = CustodianPull {
            source: &source,
            store: &store,
            backup: &nest,
            destination_id: "this-ipad".into(),
            scope_id: SCOPE,
            seal_key: OwnerSealKey::Client(seed_key()),
            cap_bytes: None,
            device_id: Some("dev-ipad".into()),
            folder_source: None,
            not_assigned: None,
        };

        let err = pull.run_once("mail", 0).await.unwrap_err();

        let chain = format!("{err:#}");
        assert!(
            chain.contains("custodian list placement journal kind=mail")
                && chain.contains("source refused kind mail-placement"),
            "the refusal propagates with its context: {chain}"
        );
        assert!(
            store.held().await.unwrap().is_empty(),
            "a pass that cannot list the journal moves nothing"
        );
    }

    /// **Each family is diffed against its own state.** A second pass moves
    /// nothing; a journal segment the source drops is tombstoned in the journal
    /// and leaves the content's segment of the same id exactly where it is.
    #[tokio::test]
    async fn the_journal_is_diffed_against_its_own_state_not_the_contents() {
        let mail = body(1, 4_000);
        let rig = Rig::new(&[(1, mail.clone())]);
        rig.source.set_journal(1, body(7, 1_000));
        rig.source.set_journal(2, body(8, 1_000));
        rig.pull(None).run_once("mail", 0).await.unwrap();

        let again = rig.pull(None).run_once("mail", 10).await.unwrap();
        assert!(again.stored_segments.is_empty());
        let again_journal = again.placement.unwrap();
        assert!(again_journal.stored_segments.is_empty() && !again_journal.mirror_stored);
        assert_eq!(
            rig.source.journal_fetched(),
            vec![1, 2],
            "nothing re-fetched"
        );

        rig.source.remove_journal(1);
        let after = rig.pull(None).run_once("mail", 20).await.unwrap();
        assert!(
            after.tombstoned_segments.is_empty(),
            "the content's segment 1 is still listed, so it is not tombstoned"
        );
        assert_eq!(after.placement.unwrap().tombstoned_segments, vec![1]);

        let keys = FileDownloadKeys::owner(seed_key());
        let hex = scope_hex();
        assert_eq!(
            rig.store
                .open(&in_mail(&SegmentFamily::Content.dat_path(&hex, 1)), &keys)
                .await
                .unwrap(),
            mail
        );
        assert!(
            rig.store
                .open(&in_mail(&SegmentFamily::Placement.dat_path(&hex, 1)), &keys)
                .await
                .is_err(),
            "the journal's segment 1 has no live generation any more"
        );
    }

    /// **Under a cap the journal waits, and the pass says so.** The cap bounds
    /// the whole store and content is admitted first, so a cap that fits the
    /// mail and not the journal stores the mail — and reports `Reached`, never
    /// a healthy row over a copy that would restore unfiled.
    #[tokio::test]
    async fn a_cap_that_fits_the_mail_and_not_the_journal_is_reported_as_reached() {
        let rig = Rig::new(&[(1, body(1, 20_000))]);
        rig.source.set_journal(1, body(7, 20_000));
        let sealed_pair = |dat: Vec<u8>, id: u32| {
            let meta = fake_meta(id, &dat);
            let root = seed_key().convergent_chunk_root();
            crate::seal::seal_blob(&dat, Some((root, None)))
                .unwrap()
                .stored_bytes()
                + crate::seal::seal_blob(&meta, Some((root, None)))
                    .unwrap()
                    .stored_bytes()
        };
        // Room for the mail pair and a mirror, not for the journal pair too.
        let cap = sealed_pair(body(1, 20_000), 1) + sealed_pair(body(7, 20_000), 1) / 2;

        let report = rig.pull(Some(cap)).run_once("mail", 0).await.unwrap();

        assert_eq!(report.stored_segments, vec![1], "the mail fits");
        let journal = report.placement.unwrap();
        assert!(journal.stored_segments.is_empty());
        assert_eq!(journal.skipped_for_cap, vec![1]);
        assert_eq!(report.cap_state, CapState::Reached);
        assert_eq!(rig.nest_saw().cap_state, CAP_STATE_REACHED);
    }

    // ── the sidecar half of every segment (2026-08-29) ───────────────────────

    /// Write one real, finalized segment pair with the segment store — the
    /// production writer, not a fixture — and hand its two files back.
    fn real_segment_pair(
        dir: &std::path::Path,
        segment_id: u32,
        records: &[(&[u8], &[u8])],
    ) -> (Vec<u8>, Vec<u8>) {
        use fauna_segment_store::{FramedSegment, SegmentHeader};
        let dat_path = dir.join(format!("seg-{segment_id:08}.dat"));
        let mut seg = FramedSegment::create(
            &dat_path,
            SegmentHeader {
                kind: "mail".into(),
                actor_id: SCOPE,
                segment_id,
                bucket: "2026-08".into(),
                created_at_secs: 1_700_000_000,
                record_count: 0,
            },
        )
        .unwrap();
        for (body, floor) in records {
            seg.append_record(fauna_cbor::Cid::of_dag_cbor(body), body, floor)
                .unwrap();
        }
        seg.finalize().unwrap();
        (
            std::fs::read(&dat_path).unwrap(),
            std::fs::read(seg.meta_path()).unwrap(),
        )
    }

    /// **The property the whole widening exists for.** What a custodian holds
    /// for a segment is a *segment*: both files come back out of the store,
    /// written side by side into a fresh directory, and `FramedSegment::open`
    /// — the production reader, which refuses a `.dat` without its sidecar —
    /// opens them and reads the records back in append order.
    ///
    /// Before 2026-08-29 this test could not have been written: the store
    /// held the `.dat` alone, and the open failed with `missing sidecar`.
    #[tokio::test]
    async fn the_pair_a_custodian_holds_reopens_as_a_segment_with_its_records() {
        let src_dir = tempfile::tempdir().unwrap();
        let records: [(&[u8], &[u8]); 3] = [
            (b"first sealed body", b"floor-1"),
            (b"second sealed body, a bit longer", b"floor-2"),
            (b"third", b"floor-3"),
        ];
        let (dat, meta) = real_segment_pair(src_dir.path(), 4, &records);
        let rig = Rig::new(&[]);
        rig.source.set_pair(4, dat.clone(), meta.clone());

        let report = rig.pull(None).run_once("mail", 0).await.unwrap();
        assert_eq!(report.stored_segments, vec![4]);

        // Reconstitute from the store alone, exactly as a materialize would.
        let keys = FileDownloadKeys::owner(seed_key());
        let hex = scope_hex();
        let out = tempfile::tempdir().unwrap();
        let dat_path = out.path().join("seg-00000004.dat");
        std::fs::write(
            &dat_path,
            rig.store
                .open(&in_mail(&format!("{hex}/seg-00000004.dat")), &keys)
                .await
                .unwrap(),
        )
        .unwrap();
        std::fs::write(
            out.path().join("seg-00000004.meta"),
            rig.store
                .open(&in_mail(&format!("{hex}/seg-00000004.meta")), &keys)
                .await
                .unwrap(),
        )
        .unwrap();

        let reopened = fauna_segment_store::FramedSegment::open(&dat_path)
            .expect("a custodian-held pair must reopen as a segment");
        assert_eq!(reopened.header.record_count, 3);
        assert_eq!(reopened.header.segment_id, 4);
        let entries: Vec<_> = reopened.iter_records().cloned().collect();
        assert_eq!(entries.len(), 3, "the footers came back");
        for (entry, (body, floor)) in entries.iter().zip(records.iter()) {
            assert_eq!(entry.cid, fauna_cbor::Cid::of_dag_cbor(body));
            assert_eq!(
                entry.floor_metadata, *floor,
                "floor metadata lives only in the sidecar"
            );
            assert_eq!(
                reopened.read_record(&entry.cid).unwrap().as_deref(),
                Some(*body),
                "the record body reads back"
            );
        }

        // And the store's index says what the diff will read next pass.
        let rows = rig.store.held().await.unwrap();
        let meta_row =
            CustodianStore::live_at(&rows, &in_mail(&format!("{hex}/seg-00000004.meta")))
                .expect("the sidecar has its own live row");
        assert_eq!(meta_row.source_size_bytes, meta.len() as u64);
        assert!(
            meta_row.path_sealed.is_none(),
            "a sidecar path is machine-authored"
        );
    }

    /// A store torn between the `.dat` put and the `.meta` put holds `.dat`
    /// rows only. The next pass fetches each missing sidecar **alone** — the `.dat`
    /// is neither re-fetched nor re-stored — and after that the pair is
    /// complete and the class never fires again.
    #[tokio::test]
    async fn a_dat_only_store_backfills_sidecars_without_refetching_the_dat() {
        let one = body(1, 9_000);
        let two = body(2, 9_000);
        let rig = Rig::new(&[(1, one.clone()), (2, two.clone())]);
        // Stage the dat-only shape by hand: the `.dat` rows exactly as a pass
        // torn before its `.meta` puts wrote them, no `.meta` rows at all.
        let root = seed_key().convergent_chunk_root();
        for (id, dat) in [(1u32, &one), (2u32, &two)] {
            let sealed = crate::seal::seal_blob(dat, Some((root, None))).unwrap();
            rig.store
                .put(
                    &in_mail(&format!("{}/seg-{id:08}.dat", scope_hex())),
                    &sealed,
                    SourceFacts {
                        size_bytes: dat.len() as u64,
                        record_count: 3,
                    },
                    0,
                )
                .await
                .unwrap();
        }

        let report = rig.pull(None).run_once("mail", DAY).await.unwrap();

        assert!(report.stored_segments.is_empty(), "no .dat moved");
        assert_eq!(report.backfilled_meta, vec![1, 2]);
        assert!(
            rig.source.fetched().is_empty(),
            "the pair door was never opened"
        );
        assert_eq!(rig.source.meta_fetched(), vec![1, 2]);
        let keys = FileDownloadKeys::owner(seed_key());
        assert_eq!(
            rig.store
                .open(
                    &in_mail(&format!("{}/seg-00000001.meta", scope_hex())),
                    &keys
                )
                .await
                .unwrap(),
            fake_meta(1, &one)
        );

        // Once is enough: the pair is whole, so the next pass owes nothing.
        let again = rig.pull(None).run_once("mail", 2 * DAY).await.unwrap();
        assert!(again.backfilled_meta.is_empty());
        assert!(again.stored_segments.is_empty());
        assert_eq!(
            rig.source.meta_fetched(),
            vec![1, 2],
            "no second backfill fetch"
        );
    }

    /// Stage a dat-only store: the two `.dat` rows exactly as a pass torn before
    /// its `.meta` puts wrote them, and no `.meta` row at all. Returns the
    /// sealed size of each half, which is what the cap tests reason in.
    async fn stage_dat_only_store(rig: &Rig, bodies: &[(u32, Vec<u8>)]) -> (u64, u64) {
        let root = seed_key().convergent_chunk_root();
        let (mut dat_bytes, mut meta_bytes) = (0, 0);
        for (id, dat) in bodies {
            let sealed = crate::seal::seal_blob(dat, Some((root, None))).unwrap();
            dat_bytes += sealed.stored_bytes();
            meta_bytes += crate::seal::seal_blob(&fake_meta(*id, dat), Some((root, None)))
                .unwrap()
                .stored_bytes();
            rig.store
                .put(
                    &in_mail(&format!("{}/seg-{id:08}.dat", scope_hex())),
                    &sealed,
                    SourceFacts {
                        size_bytes: dat.len() as u64,
                        record_count: 3,
                    },
                    0,
                )
                .await
                .unwrap();
        }
        (dat_bytes, meta_bytes)
    }

    /// The mirror this store now holds, as materialize would read it.
    async fn held_mirror(rig: &Rig) -> LiveManifestMirror {
        let keys = FileDownloadKeys::owner(seed_key());
        let bytes = rig
            .store
            .open(
                &in_mail(&crate::segment_backup::manifest_rel_path(
                    &scope_hex(),
                    "mail",
                )),
                &keys,
            )
            .await
            .expect("the store holds a mirror");
        LiveManifestMirror::from_bytes(&bytes).unwrap()
    }

    /// **A backfilled sidecar is never left unnamed by the mirror.**
    ///
    /// The corpus mirror is the *only* thing that binds a sidecar to its
    /// `.dat`: `FramedSegment::open` checks lengths, and the convergent seal
    /// carries no path or scope binding. So a pass that places a sidecar must
    /// leave a mirror that anchors it, or restore has nothing to verify against.
    #[tokio::test]
    async fn a_backfilled_sidecar_is_named_by_the_mirror_the_pass_leaves() {
        let (one, two) = (body(1, 9_000), body(2, 9_000));
        let rig = Rig::new(&[(1, one.clone()), (2, two.clone())]);
        stage_dat_only_store(&rig, &[(1, one), (2, two)]).await;

        let report = rig.pull(None).run_once("mail", DAY).await.unwrap();
        assert_eq!(report.backfilled_meta, vec![1, 2]);

        for seg in held_mirror(&rig).await.live {
            assert!(
                !seg.meta_blake3_hex.is_empty(),
                "segment {} holds a sidecar the mirror does not name",
                seg.segment_id
            );
        }
    }

    /// **A cap that cannot fit the mirror places no sidecar.**
    ///
    /// The invariant is reached by ORDERING, never by overrunning the owner's
    /// cap — the cap is the kind's only knob
    /// (`docs/goal/behavior/backup-destinations.md` § Third destination kind).
    /// Before this, the small sidecars each fit and the mirror — largest blob,
    /// stored last — did not, so a capped custodian converged on a corpus of
    /// sidecars its own mirror could not name, and stayed there:
    /// `ReclaimPlan::admits` answers `false` for the rest of the pass once
    /// `cap_state` is `Reached`, and the next pass starts from the same cap.
    #[tokio::test]
    async fn a_cap_that_cannot_fit_the_mirror_places_no_sidecar() {
        let (one, two) = (body(1, 9_000), body(2, 9_000));
        let rig = Rig::new(&[(1, one.clone()), (2, two.clone())]);
        let (dat_bytes, meta_bytes) = stage_dat_only_store(&rig, &[(1, one), (2, two)]).await;
        // Room for both sidecars and not one byte more — the mirror cannot land.
        let cap = dat_bytes + meta_bytes;

        let report = rig.pull(Some(cap)).run_once("mail", DAY).await.unwrap();

        assert!(
            report.backfilled_meta.is_empty(),
            "no sidecar may be placed that the mirror cannot name: {report:?}"
        );
        assert_eq!(report.cap_state, CapState::Reached);
        let rows = rig.store.held().await.unwrap();
        assert!(
            !rows.iter().any(|r| r.path.ends_with(".meta")),
            "not one sidecar reached the corpus: {rows:?}"
        );
    }

    /// A sidecar that contradicts the source's own listing is refused at the
    /// transport — the pair is checked half by half — so a torn pair (a
    /// rotation between the two fetches) or a substituted sidecar never gets
    /// sealed into a store as a segment nothing can reopen.
    #[tokio::test]
    async fn a_sidecar_contradicting_the_listing_is_refused() {
        let rig = Rig::new(&[(1, body(1, 4_000))]);
        *rig.source.lie_about_meta.lock().unwrap() =
            Some(hex::encode(blake3::hash(b"not the sidecar").as_bytes()));

        let err = rig.pull(None).run_once("mail", 0).await.unwrap_err();
        assert!(
            format!("{err:#}").contains(".meta"),
            "want the sidecar's own transit complaint, got: {err:#}"
        );
        assert!(
            rig.store.held().await.unwrap().is_empty(),
            "nothing of a refused pair is stored — not even the .dat"
        );
    }

    /// A compacted-out segment tombstones **both** halves, so neither outlives
    /// the other on the grace clock.
    #[tokio::test]
    async fn a_compacted_out_segment_tombstones_its_sidecar_too() {
        let rig = Rig::new(&[(1, body(1, 9_000))]);
        rig.pull(None).run_once("mail", 0).await.unwrap();

        rig.source.remove(1);
        rig.pull(None).run_once("mail", DAY).await.unwrap();

        let hex = scope_hex();
        let rows = rig.store.held().await.unwrap();
        for half in ["dat", "meta"] {
            let path = in_mail(&format!("{hex}/seg-00000001.{half}"));
            assert!(
                CustodianStore::live_at(&rows, &path).is_none(),
                "{path} must no longer be live"
            );
            assert!(
                rows.iter().any(|r| r.path == path && !r.deleted),
                "{path}'s bytes are retained inside the grace window"
            );
        }
    }

    /// The cap admits a pair as one unit: a cap that fits the `.dat` and not
    /// the `.dat`+`.meta` stores **neither**, because a `.dat` held without
    /// its sidecar is precisely the half-corpus this widening ends.
    #[tokio::test]
    async fn the_cap_admits_or_refuses_a_pair_as_one_unit() {
        let dat = body(1, 20_000);
        let rig = Rig::new(&[(1, dat.clone())]);
        let root = seed_key().convergent_chunk_root();
        let sealed_dat = crate::seal::seal_blob(&dat, Some((root, None))).unwrap();
        let sealed_meta = crate::seal::seal_blob(&fake_meta(1, &dat), Some((root, None))).unwrap();
        // Fits the .dat alone; does not fit the pair.
        let cap = sealed_dat.stored_bytes() + sealed_meta.stored_bytes() - 1;

        let report = rig.pull(Some(cap)).run_once("mail", 0).await.unwrap();

        assert_eq!(report.skipped_for_cap, vec![1]);
        assert!(report.stored_segments.is_empty());
        assert_eq!(report.cap_state, CapState::Reached);
        let rows = rig.store.held().await.unwrap();
        assert!(
            !rows.iter().any(|r| r.path.ends_with("seg-00000001.dat")),
            "the .dat must not be stored without its sidecar"
        );
    }

    /// A second pass over an unchanged source must fetch nothing — the diff is
    /// what makes an intermittent device cheap to wake.
    #[tokio::test]
    async fn an_unchanged_second_pass_fetches_nothing_and_re_stores_nothing() {
        let rig = Rig::new(&[(1, body(1, 9_000))]);
        rig.pull(None).run_once("mail", 0).await.unwrap();
        let after_first = rig.source.fetched();

        let report = rig.pull(None).run_once("mail", DAY).await.unwrap();

        assert_eq!(rig.source.fetched(), after_first, "no re-fetch");
        assert!(report.stored_segments.is_empty());
        assert!(
            !report.mirror_stored,
            "the mirror re-seals to the same hash, so it is already held"
        );
    }

    /// A grown segment is re-pulled, and the superseded generation is retained
    /// (the grace window), not overwritten.
    #[tokio::test]
    async fn a_grown_segment_is_re_pulled_and_the_old_generation_is_retained() {
        let rig = Rig::new(&[(1, body(1, 9_000))]);
        rig.pull(None).run_once("mail", 0).await.unwrap();

        let grown = body(1, 12_000);
        rig.source.set(1, grown.clone());
        let report = rig.pull(None).run_once("mail", DAY).await.unwrap();

        assert_eq!(report.stored_segments, vec![1]);
        let hex = scope_hex();
        let path = in_mail(&format!("{hex}/seg-00000001.dat"));
        let rows = rig.store.held().await.unwrap();
        assert_eq!(
            rows.iter().filter(|r| r.path == path).count(),
            2,
            "both generations held — retention is the policy's call, not the store's"
        );

        let keys = FileDownloadKeys::owner(seed_key());
        assert_eq!(rig.store.open(&path, &keys).await.unwrap(), grown);
    }

    /// A compacted-out segment is tombstoned, not deleted on sight: what it
    /// covers ages out on the ordinary clock, so a rogue source cannot make this
    /// device drop bytes today.
    #[tokio::test]
    async fn a_compacted_out_segment_is_tombstoned_and_its_bytes_survive_the_pass() {
        let rig = Rig::new(&[(1, body(1, 9_000)), (2, body(2, 9_000))]);
        rig.pull(None).run_once("mail", 0).await.unwrap();

        rig.source.remove(1);
        let report = rig.pull(None).run_once("mail", DAY).await.unwrap();

        assert_eq!(report.tombstoned_segments, vec![1]);
        let hex = scope_hex();
        let path = in_mail(&format!("{hex}/seg-00000001.dat"));
        let rows = rig.store.held().await.unwrap();
        assert!(CustodianStore::live_at(&rows, &path).is_none());
        assert!(
            rows.iter().any(|r| r.path == path && !r.deleted),
            "the body it covers is still held, inside its grace window"
        );
    }

    /// Past the grace window the tombstoned generation's bytes finally reclaim.
    #[tokio::test]
    async fn a_tombstoned_generation_reclaims_once_its_grace_window_expires() {
        let rig = Rig::new(&[(1, body(1, 9_000))]);
        rig.pull(None).run_once("mail", 0).await.unwrap();
        rig.source.remove(1);
        rig.pull(None).run_once("mail", DAY).await.unwrap();

        let report = rig.pull(None).run_once("mail", 40 * DAY).await.unwrap();

        assert!(
            report.reclaimed_generations >= 2,
            "the body and its tombstone"
        );
        let hex = scope_hex();
        let path = in_mail(&format!("{hex}/seg-00000001.dat"));
        assert!(
            !rig.store
                .held()
                .await
                .unwrap()
                .iter()
                .any(|r| r.path == path)
        );
    }

    /// The rule the module doc calls out: a pass that stopped at its cap must
    /// **say so**, even though it ends below the cap — otherwise the user reads a
    /// stopped backup as ordinary lag and is never told.
    #[tokio::test]
    async fn a_pass_that_stops_at_the_cap_reports_cap_reached_not_ok() {
        // Two ~20 KB segments; a cap that fits the first and not the second.
        let rig = Rig::new(&[(1, body(1, 20_000)), (2, body(2, 20_000))]);
        let first = crate::seal::seal_blob(
            &body(1, 20_000),
            Some((seed_key().convergent_chunk_root(), None)),
        )
        .unwrap();
        let second = crate::seal::seal_blob(
            &body(2, 20_000),
            Some((seed_key().convergent_chunk_root(), None)),
        )
        .unwrap();
        // Admits the first and not the second, and leaves the pass *under* the
        // cap — which is precisely the state a bare `plan_reclaim` reads as OK.
        let cap = first.stored_bytes() + second.stored_bytes() - 1;

        let report = rig.pull(Some(cap)).run_once("mail", 0).await.unwrap();

        assert_eq!(report.stored_segments, vec![1]);
        assert_eq!(report.skipped_for_cap, vec![2]);
        assert!(
            report.held_bytes < cap,
            "the pass ends UNDER the cap — which is exactly why the plan's own \
             verdict would have been OK"
        );
        assert_eq!(report.cap_state, CapState::Reached);
        assert_eq!(rig.nest_saw().cap_state, CAP_STATE_REACHED);
    }

    /// A capped custodian never evicts a live path to make room — it stops.
    #[tokio::test]
    async fn a_cap_never_evicts_a_live_path() {
        let rig = Rig::new(&[(1, body(1, 20_000))]);
        rig.pull(None).run_once("mail", 0).await.unwrap();

        let live_before = rig.store.held().await.unwrap().len();
        // Now impose a cap far below what is already held, and offer more.
        rig.source.set(2, body(2, 20_000));
        let report = rig.pull(Some(1_000)).run_once("mail", DAY).await.unwrap();

        assert!(report.stored_segments.is_empty());
        assert_eq!(report.cap_state, CapState::Reached);
        assert_eq!(
            report.reclaimed_generations, 0,
            "nothing was retained to give up"
        );
        assert_eq!(
            rig.store.held().await.unwrap().len(),
            live_before,
            "an unsatisfiable cap drops nothing — it stops pulling"
        );

        let hex = scope_hex();
        let keys = FileDownloadKeys::owner(seed_key());
        assert!(
            rig.store
                .open(&in_mail(&format!("{hex}/seg-00000001.dat")), &keys)
                .await
                .is_ok(),
            "the live generation survives an unsatisfiable cap"
        );
    }

    /// The check-in is what the owner's other devices see; it must carry the
    /// pass's own numbers, not defaults.
    #[tokio::test]
    async fn the_check_in_carries_this_passs_high_water_and_held_bytes() {
        let rig = Rig::new(&[(1, body(1, 9_000)), (7, body(7, 9_000))]);
        let report = rig.pull(None).run_once("mail", 0).await.unwrap();

        let seen = rig.nest_saw();
        assert_eq!(seen.destination_id, "this-ipad");
        assert_eq!(seen.high_water, 8, "greatest sealed id 7, plus one");
        assert_eq!(seen.high_water, report.high_water);
        assert_eq!(seen.held_bytes, report.held_bytes);
        assert!(seen.held_bytes > 0);
        assert_eq!(seen.cap_state, CAP_STATE_OK);
    }

    /// High water counts what was **sealed**, not what was listed — a segment
    /// the cap refused must not advance it, or the nest computes a backlog of
    /// zero for a device that is not caught up.
    #[tokio::test]
    async fn high_water_does_not_advance_past_a_segment_the_cap_refused() {
        let rig = Rig::new(&[(1, body(1, 20_000)), (2, body(2, 20_000))]);
        let sealed = |tag| {
            crate::seal::seal_blob(
                &body(tag, 20_000),
                Some((seed_key().convergent_chunk_root(), None)),
            )
            .unwrap()
            .stored_bytes()
        };

        let report = rig
            .pull(Some(sealed(1) + sealed(2) - 1))
            .run_once("mail", 0)
            .await
            .unwrap();

        assert_eq!(report.skipped_for_cap, vec![2]);
        assert_eq!(
            report.high_water, 2,
            "only segment 1 was sealed, so the device is caught up to 1 — not 2"
        );
    }

    // ── the regression arm: the pull never tombstones against a source below
    //    its copy (`segment-backup-protocol.md` § Client-device custodian (pull)) ──

    fn mail_ledger() -> String {
        in_mail(&crate::segment_backup::manifest_rel_path(
            &scope_hex(),
            "mail",
        ))
    }

    fn check_ins(rig: &Rig) -> usize {
        rig.nest.transport().checkins.lock().unwrap().len()
    }

    /// The mail content segments this store holds live, ascending.
    async fn live_mail(rig: &Rig) -> Vec<u32> {
        let rows = rig.store.held().await.unwrap();
        let mut ids: Vec<u32> = CustodianPull::<FakeSource, FakeNest>::local_state(
            &rows,
            "__mail",
            SegmentFamily::Content,
            &scope_hex(),
        )
        .into_keys()
        .collect();
        ids.sort_unstable();
        ids
    }

    fn three() -> Vec<(u32, Vec<u8>)> {
        vec![
            (1, body(1, 4_000)),
            (2, body(2, 4_000)),
            (3, body(3, 4_000)),
        ]
    }

    /// A rig whose store has pulled segments 1–3 once, so it holds the mail
    /// ledger at generation 4.
    async fn pulled_three() -> Rig {
        let rig = Rig::new(&three());
        rig.pull(None).run_once("mail", 0).await.unwrap();
        assert_eq!(held_mirror(&rig).await.next_segment_id_seen, 4);
        rig
    }

    /// **The rebuilt box.** A source listing nothing at counter zero against a
    /// store holding its ledger at 4 is a box that lost its corpus — and this
    /// copy may be the owner's only one. The pass tombstones nothing, fetches
    /// nothing, leaves every held row exactly as it was, makes no check-in (one
    /// would tell the nest this device is caught up on nothing), and records the
    /// regression. Red against the pre-arm pull: it tombstoned 1–3 and checked in.
    #[tokio::test]
    async fn a_source_back_at_zero_tombstones_nothing_and_checks_in_nothing() {
        let rig = pulled_three().await;
        let held_before = rig.store.held().await.unwrap();
        let fetched_before = rig.source.fetched().len();
        let check_ins_before = check_ins(&rig);

        rig.source.rebuild_empty();
        let report = rig.pull(None).run_once("mail", DAY).await.unwrap();

        assert_eq!(
            report.source_regressed,
            Some(SourceRegressed {
                family: SegmentFamily::Content,
                held: 4,
                served: 0,
            })
        );
        assert!(report.tombstoned_segments.is_empty());
        assert_eq!(
            rig.store.held().await.unwrap(),
            held_before,
            "a refused pass changes no held row"
        );
        assert_eq!(
            rig.source.fetched().len(),
            fetched_before,
            "nothing fetched"
        );
        assert_eq!(check_ins(&rig), check_ins_before, "no check-in");
        assert_eq!(report.high_water, 4, "the copy still reaches 4");
        assert_eq!(
            rig.store.audit_record().await.source_regressions,
            BTreeMap::from([(
                mail_ledger(),
                crate::custodian_store::SourceRegression {
                    held: 4,
                    served: 0,
                    observed_at: DAY,
                },
            )])
        );
    }

    /// **What lifts it.** The same box re-seeded from this copy lists the
    /// segments byte-identical at its floored counter: the next pass sees them
    /// held, fetches and tombstones nothing, checks in again, and drops the
    /// record.
    #[tokio::test]
    async fn the_source_back_at_its_counter_resumes_the_pull_and_clears_the_record() {
        let rig = pulled_three().await;
        rig.source.rebuild_empty();
        rig.pull(None).run_once("mail", DAY).await.unwrap();
        assert_eq!(rig.store.audit_record().await.source_regressions.len(), 1);
        let fetched_before = rig.source.fetched().len();
        let check_ins_before = check_ins(&rig);

        for (id, bytes) in three() {
            rig.source.set(id, bytes);
        }
        rig.source.serve_saved_counter("mail");
        let report = rig.pull(None).run_once("mail", 2 * DAY).await.unwrap();

        assert_eq!(report.source_regressed, None);
        assert!(report.stored_segments.is_empty(), "held byte-identical");
        assert!(report.tombstoned_segments.is_empty());
        assert_eq!(rig.source.fetched().len(), fetched_before);
        assert_eq!(check_ins(&rig), check_ins_before + 1, "the pass checks in");
        assert!(
            rig.store.audit_record().await.source_regressions.is_empty(),
            "the record clears itself"
        );
        assert_eq!(live_mail(&rig).await, vec![1, 2, 3]);
    }

    /// A rollback — a counter below the ledger but above zero, a box gone back
    /// to an older copy that still lists some of the segments — is refused
    /// exactly as the rebuilt box is: segment 3, which the older copy never
    /// minted, stays live here.
    #[tokio::test]
    async fn a_rollback_between_zero_and_the_ledger_is_refused_the_same_way() {
        let rig = pulled_three().await;
        let check_ins_before = check_ins(&rig);

        rig.source.remove(3);
        rig.source.serve_counter("mail", 3);
        let report = rig.pull(None).run_once("mail", DAY).await.unwrap();

        assert_eq!(
            report.source_regressed,
            Some(SourceRegressed {
                family: SegmentFamily::Content,
                held: 4,
                served: 3,
            })
        );
        assert!(report.tombstoned_segments.is_empty());
        assert_eq!(live_mail(&rig).await, vec![1, 2, 3]);
        assert_eq!(check_ins(&rig), check_ins_before);
        assert_eq!(
            rig.store.audit_record().await.source_regressions[&mail_ledger()].served,
            3
        );
    }

    /// **Why the saved counter is carried at all.** An honest compaction that
    /// empties the top segment lowers the greatest live id but not the
    /// counter, so the arm never fires on one: the pass tombstones segment 3 and
    /// checks in as it always did. Comparing against the greatest listed id
    /// instead of the counter would refuse this pass.
    #[tokio::test]
    async fn an_honest_compaction_that_lowers_the_top_segment_still_pulls() {
        let rig = pulled_three().await;
        let check_ins_before = check_ins(&rig);

        rig.source.remove(3);
        let report = rig.pull(None).run_once("mail", DAY).await.unwrap();

        assert_eq!(report.source_regressed, None);
        assert_eq!(report.tombstoned_segments, vec![3]);
        assert_eq!(check_ins(&rig), check_ins_before + 1);
        assert!(rig.store.audit_record().await.source_regressions.is_empty());
    }

    /// A store holding no ledger has nothing to compare: the first pass pulls
    /// from any source, one serving a zero counter included.
    #[tokio::test]
    async fn a_store_holding_no_ledger_pulls_from_any_source() {
        let rig = Rig::new(&[(1, body(1, 4_000)), (2, body(2, 4_000))]);
        rig.source.serve_counter("mail", 0);

        let report = rig.pull(None).run_once("mail", 0).await.unwrap();

        assert_eq!(report.source_regressed, None);
        assert_eq!(report.stored_segments, vec![1, 2]);
        assert_eq!(check_ins(&rig), 1);
    }

    /// A ledger this store holds but cannot open (rotted bytes the repair only
    /// a running pass can make) does not admit a pass against a corpus-less
    /// source: the comparison falls back to the greatest segment held live,
    /// plus one.
    #[tokio::test]
    async fn an_unreadable_ledger_still_refuses_a_corpus_less_source() {
        let rig = pulled_three().await;
        assert!(rig.rot_every_blob() > 0);

        rig.source.rebuild_empty();
        let report = rig.pull(None).run_once("mail", DAY).await.unwrap();

        assert_eq!(
            report.source_regressed,
            Some(SourceRegressed {
                family: SegmentFamily::Content,
                held: 4,
                served: 0,
            })
        );
        assert_eq!(live_mail(&rig).await, vec![1, 2, 3]);
    }

    /// The covered-folder passes are held with a refused kind: a box that lost
    /// its corpus lists no folder head either, and mirroring that would
    /// tombstone this device's copy of the folder.
    #[tokio::test]
    async fn a_regressed_source_holds_the_covered_folder_passes() {
        let rig = Rig::new(&three());
        let folders = FakeFolders::default();
        let path_hash = [0x21u8; 32];
        folders.stage_file(path_hash, &[body(7, 3_000)]);
        cover_folder(&rig);
        let mut pull = rig.pull(None);
        pull.folder_source = Some(&folders);
        pull.run_all_kinds(0).await;
        let path = format!("{}/{}", folder_set(), hex::encode(path_hash));
        assert!(CustodianStore::live_at(&rig.store.held().await.unwrap(), &path).is_some());
        let check_ins_before = check_ins(&rig);

        rig.source.rebuild_empty();
        folders.drop_path(path_hash);
        let reports = pull.run_all_kinds(DAY).await;

        assert!(
            reports.iter().all(|r| r.source_regressed.is_some()),
            "every kind's source went back to zero: {reports:?}"
        );
        assert!(
            CustodianStore::live_at(&rig.store.held().await.unwrap(), &path).is_some(),
            "the folder mirror is not tombstoned against the corpus-less box"
        );
        assert_eq!(check_ins(&rig), check_ins_before);
    }

    // ---- the scheduling hook (slice 3c) ----

    /// A source that fails `list_segments` for one named kind — the per-kind
    /// failure `run_all_kinds` must survive.
    struct HalfBrokenSource {
        inner: FakeSource,
        broken_kind: &'static str,
    }

    #[async_trait::async_trait]
    impl SegmentSource for HalfBrokenSource {
        fn source_id(&self) -> &str {
            "half-broken"
        }

        async fn list_segments(&self, kind: &str, scope_hex: &str) -> Result<SegmentListing> {
            if kind == self.broken_kind {
                anyhow::bail!("source refused kind {kind}");
            }
            self.inner.list_segments(kind, scope_hex).await
        }

        async fn segment_pair(
            &self,
            kind: &str,
            scope_hex: &str,
            segment_id: u32,
        ) -> Result<crate::segment_backup::SegmentPair> {
            self.inner.segment_pair(kind, scope_hex, segment_id).await
        }

        async fn segment_meta_bytes(
            &self,
            kind: &str,
            scope_hex: &str,
            segment_id: u32,
        ) -> Result<Vec<u8>> {
            self.inner
                .segment_meta_bytes(kind, scope_hex, segment_id)
                .await
        }
    }

    /// Every backed-up kind is driven, and by the *shared* list — so the day a
    /// per-kind rollout extends `BACKED_UP_KINDS`, the custodian pulls the new
    /// kind with no edit here. A private copy of the list is how a device ends
    /// up silently not backing up a kind the nest is backing up.
    #[tokio::test]
    async fn a_pass_covers_every_kind_the_upload_side_covers() {
        let rig = Rig::new(&[(1, body(1, 4_000))]);
        let reports = rig.pull(None).run_all_kinds(0).await;
        assert_eq!(
            reports.len(),
            crate::segment_backup::BACKED_UP_KINDS.len(),
            "run_all_kinds skipped a kind the upload side covers"
        );
    }

    /// Whether `notify` holds a permit, without waiting for one to arrive.
    async fn was_notified(notify: &Notify) -> bool {
        tokio::time::timeout(std::time::Duration::from_millis(50), notify.notified())
            .await
            .is_ok()
    }

    /// The nest refusing a check-in because its registry no longer assigns
    /// this destination to this device is the authority's answer that the
    /// stint is dead. The pass must hand that on to the host, which re-reads
    /// its assignment at once instead of pulling on for up to one rediscovery
    /// interval (`sync-agent.md` § A7).
    #[tokio::test]
    async fn a_not_assigned_check_in_refusal_notifies_the_host() {
        let rig = Rig::new(&[(1, body(1, 4_000))]);
        *rig.nest.transport().checkin_fault.lock().unwrap() =
            Some(CheckinFault::Refused(fauna_protocol::RpcError::new(
                fauna_protocol::RpcError::CODE_BACKUP_CUSTODIAN_NOT_ASSIGNED,
                "error.backup.custodian_not_assigned",
            )));
        let not_assigned = Notify::new();
        let pull = CustodianPull {
            not_assigned: Some(&not_assigned),
            ..rig.pull(None)
        };

        let reports = pull.run_all_kinds(0).await;

        assert!(
            reports.is_empty(),
            "a refused check-in fails the kind's pass"
        );
        assert!(
            was_notified(&not_assigned).await,
            "the refusal must reach the host, not end in the pass's warn log"
        );
    }

    /// Only the typed refusal is that signal. A transport fault says nothing
    /// about the assignment, and neither does any other refusal: the host's
    /// rule is that a timed-out read must never stop the pull, so these must
    /// not even prompt it.
    #[tokio::test]
    async fn a_timed_out_or_otherwise_refused_check_in_does_not_notify_the_host() {
        for fault in [
            CheckinFault::TimedOut,
            CheckinFault::Refused(fauna_protocol::RpcError::new(
                "fauna.protocol.malformed",
                "error.protocol.malformed",
            )),
        ] {
            let rig = Rig::new(&[(1, body(1, 4_000))]);
            *rig.nest.transport().checkin_fault.lock().unwrap() = Some(fault);
            let not_assigned = Notify::new();
            let pull = CustodianPull {
                not_assigned: Some(&not_assigned),
                ..rig.pull(None)
            };

            pull.run_all_kinds(0).await;

            assert!(!was_notified(&not_assigned).await);
        }
    }

    /// A kind the source refuses must not stop the device pulling the rest of
    /// the corpus — the same non-fail-fast contract `run_all_tuples` keeps.
    #[tokio::test]
    async fn a_failing_kind_does_not_abort_the_pass() {
        let dir = tempfile::tempdir().unwrap();
        let source = HalfBrokenSource {
            inner: FakeSource::with(&[(1, body(1, 4_000))]),
            broken_kind: "mail",
        };
        let store = CustodianStore::at(dir.path().join("custody"));
        let nest = BackupClient::new(FakeNest::default());
        let pull = CustodianPull {
            source: &source,
            store: &store,
            backup: &nest,
            destination_id: "this-ipad".into(),
            scope_id: SCOPE,
            seal_key: OwnerSealKey::Client(seed_key()),
            cap_bytes: None,
            device_id: Some("dev-ipad".into()),
            folder_source: None,
            not_assigned: None,
        };

        // The refused kind reports nothing — and, load-bearingly, the sweep
        // returns rather than propagating, and every other kind still runs.
        let reports = pull.run_all_kinds(0).await;
        assert_eq!(
            reports.len(),
            crate::segment_backup::BACKED_UP_KINDS.len() - 1,
            "a refused kind must not report a pass, nor stop the others"
        );
        let held = store.held().await.unwrap();
        assert!(
            !held.iter().any(|r| r.path.starts_with("__mail/")),
            "the refused kind stored nothing"
        );
        for set in ["__post", "__calendar", "__card"] {
            assert!(
                held.iter().any(|r| path_in_set(set, &r.path).is_some()),
                "{set} was not pulled after the refused kind"
            );
        }
    }

    /// The driver honours cancellation. Generous ceiling, no assertion on how
    /// long it actually took (convention 14): with no push client the loop parks
    /// on a 15-minute timer, so only the cancel arm can wake it — which is
    /// exactly the property a shell's teardown depends on.
    #[tokio::test]
    async fn run_forever_returns_promptly_on_cancellation() {
        let rig = Rig::new(&[]);
        let pull = rig.pull(None);
        let cancel = CancellationToken::new();
        let signaler = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                cancel.cancel();
            })
        };

        let result =
            tokio::time::timeout(std::time::Duration::from_secs(5), pull.run_forever(cancel)).await;

        signaler.await.unwrap();
        result
            .expect("run_forever must return within 5s of cancellation")
            .expect("run_forever should return Ok on cancellation");
    }

    // The OS-scheduler variant parks purely on pushes and cancellation — it has
    // no periodic arm at all, which is what stops an in-process kick doubling
    // the period `WorkManager` / `BGProcessingTask` already drives.

    /// **Standalone restore**: a store written by a real pull pass reopens, with
    /// the source nest and the backup client both dropped, from nothing but its
    /// on-disk root and the owner's key — and gives back the original bytes.
    ///
    /// This is `backup-destinations.md` § Standalone restore, the section that
    /// calls this "the strongest argument for the kind": *a self-custodian can
    /// restore with no nest alive anywhere: open its local sealed store under
    /// seed-derived keys.* No nest destination can do that, so it is the one
    /// property the third kind exists for.
    ///
    /// **What this adds over `custodian_store.rs`'s
    /// `a_stored_generation_opens_again_with_no_nest_alive`.** That test seals a
    /// body by hand, `put`s it, and opens it — proving the store round-trips its
    /// own writes. It cannot see the failure this one catches, because the bytes
    /// it opens are the bytes it sealed a line earlier. Here the store is
    /// written by the **production write path** (`run_all_kinds` → pull → seal →
    /// store), and what comes back out is compared against what the *source*
    /// served, so a pull that sealed under the wrong key, mirrored the wrong
    /// generation, or recorded a manifest hash that does not address its own
    /// chunks fails here and passes there.
    ///
    /// **Why the rig is consumed.** A test that simply refrains from calling the
    /// source proves nothing about reachability — the open path could reach for
    /// a nest and the test would still be green. `into_store_on_disk` drops both
    /// halves, so "no nest alive" is enforced by the borrow checker rather than
    /// asserted in a comment: the reopened `CustodianStore` is built from a
    /// `PathBuf`, and there is no longer a source or a client in scope to reach.
    #[tokio::test]
    async fn a_pulled_store_restores_standalone_with_the_source_and_nest_dropped() {
        // Distinctive, compressible-but-not-uniform bytes, and big enough to
        // cross more than one chunk: a single-chunk body would not exercise the
        // manifest's chunk list at all, which is the part a standalone open has
        // to walk without anyone to ask.
        let plaintext: Vec<u8> = (0..80_000u32).map(|i| (i % 251) as u8).collect();
        let rig = Rig::new(&[(1, plaintext.clone())]);

        {
            let pull = rig.pull(None);
            pull.run_all_kinds(1_000_000).await;
        }

        // Everything that could talk to a nest is dropped right here.
        let (_dir, root) = rig.into_store_on_disk();

        // Rebuilt from the path alone — the state a restore actually starts
        // from: a directory, and a seed the user still has.
        let restored = CustodianStore::at(root.clone());
        let rows = restored.held().await.unwrap();
        assert!(
            !rows.is_empty(),
            "the pull pass left nothing on disk at {}, so there is no store to \
             restore from and every assertion below would be vacuous",
            root.display()
        );

        let keys = FileDownloadKeys::owner(seed_key());
        let mut recovered_the_segment = false;
        for row in &rows {
            let produced = restored.open(&row.path, &keys).await.unwrap_or_else(|e| {
                panic!(
                    "standalone open failed for {} — a custodian that cannot produce \
                     what it holds without a nest is the one thing this destination \
                     kind exists to rule out: {e:#}",
                    row.path
                )
            });
            if produced == plaintext {
                recovered_the_segment = true;
            }
        }
        assert!(
            recovered_the_segment,
            "every held path opened, but none produced the segment the source \
             actually served ({} bytes). Opening successfully is not the property — \
             a store that seals and returns its OWN wrong bytes opens perfectly. \
             Held: {:?}",
            plaintext.len(),
            rows.iter().map(|r| &r.path).collect::<Vec<_>>()
        );
    }

    // ── the self-audit arm (`audit_if_due`) ──────────────────────────────────
    //
    // Until 2026-08-21 this function had no test of its own at all: the shared
    // sampling helpers are covered in `fauna_client_backup::audit` and the
    // banner has render tests, but the piece that decides *whether a pass
    // audits at all* and *what the resulting check-in says* was covered only
    // transitively. Both rules below are silent when wrong — a rotted store
    // that keeps reporting healthy looks exactly like a healthy one.

    /// A pass inside the debounce window does **not** re-audit — and the
    /// check-in it writes therefore still carries the *standing* verdict, not a
    /// fresh one and not silence.
    ///
    /// This is the rule that makes the arm affordable: a pull pass runs every
    /// 15 minutes and opening `AUDIT_SAMPLE_K` sealed files that often would
    /// spend a device's battery re-answering a question whose answer changes on
    /// the scale of days. It is also the reason the tier_3 proof of the failing
    /// arm has to move the clock at all
    /// (`RequestMethod::CustodianRunPassNow`'s `now_offset_secs`): rot that
    /// appears *after* enrollment is invisible until the next audit is due.
    ///
    /// The half that would otherwise be silent is the check-in. Skipping the
    /// audit must not make the row go quiet — silence is read by the 30-day
    /// intermittency rule as a sleeping device, i.e. the wrong alarm thirty
    /// days late — so `run_once` reports the persisted record on **every** pass
    /// (`backup-destinations.md` § Custodian contract, question 4).
    #[tokio::test]
    async fn a_pass_inside_the_debounce_window_reports_the_standing_verdict() {
        let rig = Rig::new(&[(1, b"one".to_vec())]);
        let pull = rig.pull(None);

        const T0: i64 = 1_000_000;
        pull.run_all_kinds(T0).await;
        assert_eq!(
            rig.store.audit_record().await.last_run_at,
            Some(T0),
            "the first pass audits (an empty store, which passes) and records the attempt"
        );

        // Rot it, then run again well inside the window.
        let rotted = rig.rot_every_blob();
        assert!(
            rotted > 0,
            "the first pass stored no blobs, so nothing was rotted"
        );
        pull.run_all_kinds(T0 + 60).await;

        assert_eq!(
            rig.store.audit_record().await.last_run_at,
            Some(T0),
            "a pass 60s later must NOT re-audit — `AUDIT_MIN_INTERVAL_SECS` is a day, \
             and re-opening the sampled files every 15-minute pass is what the \
             debounce exists to prevent"
        );
        assert_eq!(
            rig.nest_saw().audit_state.as_deref(),
            Some(AUDIT_STATE_OK),
            "the pass carried no fresh verdict, so it must report the STANDING one. \
             `None` here would be the silent-row failure: a device that stops \
             reporting reads as a sleeping device, which is the wrong alarm thirty \
             days late rather than the data-loss alarm the audit exists to raise"
        );
    }

    /// Once the debounce has elapsed, a rotted store audits, **fails**, reports
    /// the failure — and leaves the last-passed clock exactly where it was.
    ///
    /// Three rules in one pass, each silent when wrong:
    ///
    /// 1. *The audit is a production check, not a `stat`.* The blobs are
    ///    overwritten, not deleted, so a store that merely checked its files
    ///    exist would still pass here. (It does not separate the store's two
    ///    audit arms from each other — see `rot_every_blob`.)
    /// 2. *A failure is reported, never withheld* — same reason as the test
    ///    above, in the direction that actually matters.
    /// 3. *A failure never advances `last_audit_passed_at`.* Otherwise a store
    ///    that rotted an instant ago reports itself as just-verified, and the
    ///    owner's row reads "checked moments ago" over unproducible bytes.
    ///
    /// This is the tier_1 twin of
    /// `test_backups.py::test_a_custodian_whose_store_rots_reports_a_failing_row_to_its_owner`,
    /// which drives the same transition through a real agent, a real nest and
    /// the rendered row. Keeping both is deliberate: this one pins the rules at
    /// cargo speed, that one proves they survive every boundary in between.
    #[tokio::test]
    async fn a_pass_past_the_debounce_reports_a_rotted_store_without_advancing_the_pass_clock() {
        let rig = Rig::new(&[(1, b"one".to_vec())]);
        let pull = rig.pull(None);

        const T0: i64 = 1_000_000;
        pull.run_all_kinds(T0).await;
        let baseline = rig.store.audit_record().await;
        assert_eq!(
            baseline.last_passed_at,
            Some(T0),
            "the first pass audits an empty store, which passes — a device holding \
             nothing is holding all of it"
        );

        let rotted = rig.rot_every_blob();
        assert!(
            rotted > 0,
            "the first pass stored no blobs, so nothing was rotted"
        );

        let t1 = T0 + AUDIT_MIN_INTERVAL_SECS + 3600;
        pull.run_all_kinds(t1).await;

        let record = rig.store.audit_record().await;
        assert_eq!(
            record.last_run_passed,
            Some(false),
            "every live path the index claims was overwritten with bytes that cannot \
             produce their own hash, so the audit must fail. A pass here means the \
             audit is checking that files EXIST rather than that they can be \
             produced — see `rot_every_blob` for what this does and does not \
             separate"
        );
        assert_eq!(
            record.last_passed_at,
            Some(T0),
            "a FAILURE must leave the last-passed clock frozen at the last real pass. \
             Advancing it would render a store that rotted an instant ago as \
             just-verified — the one inference `SelfAudit::failed` is shaped to make \
             unrepresentable"
        );

        let checkin = rig.nest_saw();
        assert_eq!(
            checkin.audit_state.as_deref(),
            Some(AUDIT_STATE_FAILED),
            "the failure must reach the nest on this pass's own check-in"
        );
        assert_eq!(
            checkin.last_audit_passed_at,
            Some(T0 as u64),
            "the check-in carries the PREVIOUS pass, which is what makes the failure \
             legible on the owner's row: 'last verified then, rotten since' rather \
             than a bare failure with no history"
        );
    }

    #[tokio::test]
    async fn run_push_debounce_returns_promptly_on_cancellation() {
        let rig = Rig::new(&[]);
        let pull = rig.pull(None);
        let cancel = CancellationToken::new();
        let signaler = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                cancel.cancel();
            })
        };

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            pull.run_push_debounce(cancel),
        )
        .await;

        signaler.await.unwrap();
        result
            .expect("run_push_debounce must return within 5s of cancellation")
            .expect("run_push_debounce should return Ok on cancellation");
    }

    // ── the covered-folder mirror axis ────────────────────────────────────────

    fn folder_set() -> String {
        format!("__folder/{}/5", "ab".repeat(32))
    }

    fn cover_folder(rig: &Rig) {
        cover_folder_named(rig, Some("Photos"));
    }

    fn cover_folder_named(rig: &Rig, name: Option<&str>) {
        *rig.nest.transport().covered_folders.lock().unwrap() = vec![CoveredFolder {
            folder_id: 5,
            folder_set: folder_set(),
            name: name.map(str::to_string),
            ..Default::default()
        }];
    }

    /// The folder's display name comes off the coverage listing and lands in
    /// the store beside the mirror — before any byte, so a folder the cap
    /// refuses whole is still named; a rename on the source overwrites it;
    /// and a source that lists no name (the nest projects an optional name) leaves the store's answer
    /// alone rather than erasing a name an earlier pass recorded.
    #[tokio::test]
    async fn a_covered_folders_name_is_recorded_beside_its_mirror() {
        let rig = Rig::new(&[]);
        let folders = FakeFolders::default();
        folders.stage_file([0x21u8; 32], &[body(7, 3_000)]);
        let mut pull = rig.pull(Some(1));
        pull.folder_source = Some(&folders);

        // A cap of one byte admits no path — the name is recorded regardless.
        cover_folder_named(&rig, Some("Photos"));
        let reports = pull.run_covered_folders(0).await;
        assert_eq!(reports[0].stored_paths, 0);
        assert_eq!(reports[0].skipped_for_cap, 1);
        assert_eq!(
            rig.store.folder_names().await.unwrap(),
            std::collections::BTreeMap::from([(folder_set(), "Photos".to_string())])
        );

        cover_folder_named(&rig, Some("Pictures"));
        pull.run_covered_folders(0).await;
        assert_eq!(
            rig.store
                .folder_names()
                .await
                .unwrap()
                .get(&folder_set())
                .map(String::as_str),
            Some("Pictures"),
            "the store tracks the folder's current name"
        );

        cover_folder_named(&rig, None);
        pull.run_covered_folders(0).await;
        assert_eq!(
            rig.store
                .folder_names()
                .await
                .unwrap()
                .get(&folder_set())
                .map(String::as_str),
            Some("Pictures"),
            "a listing that carries no name (the nest projects an optional name) erases nothing"
        );
    }

    /// A sealed set's listing carries its address and sealed label — once the
    /// source's row holds no plaintext name, nothing else — and the pull records
    /// the pair beside the mirror before any byte, overwriting it on a rename,
    /// so a re-seed can name its target by the hash.
    #[tokio::test]
    async fn a_covered_folders_sealed_label_is_recorded_without_a_plaintext_name() {
        let rig = Rig::new(&[]);
        let folders = FakeFolders::default();
        folders.stage_file([0x21u8; 32], &[body(7, 3_000)]);
        let mut pull = rig.pull(Some(1));
        pull.folder_source = Some(&folders);

        let cover_sealed = |hash: [u8; 32], sealed: &[u8]| {
            *rig.nest.transport().covered_folders.lock().unwrap() = vec![CoveredFolder {
                folder_id: 5,
                folder_set: folder_set(),
                name_hash: Some(fauna_protocol::ByteBuf::from(hash.to_vec())),
                name_sealed: Some(fauna_protocol::ByteBuf::from(sealed.to_vec())),
                ..Default::default()
            }];
        };
        cover_sealed([3u8; 32], b"sealed-photos");
        let reports = pull.run_covered_folders(0).await;
        assert_eq!(reports[0].skipped_for_cap, 1, "the cap admits no byte");
        assert!(rig.store.folder_names().await.unwrap().is_empty());
        assert_eq!(
            rig.store.folder_labels().await.unwrap(),
            std::collections::BTreeMap::from([(
                folder_set(),
                fauna_client_backup::reseed::FolderLabel {
                    name_hash: [3u8; 32],
                    name_sealed: b"sealed-photos".to_vec(),
                }
            )])
        );

        cover_sealed([4u8; 32], b"sealed-pictures");
        pull.run_covered_folders(0).await;
        assert_eq!(
            rig.store.folder_labels().await.unwrap()[&folder_set()].name_hash,
            [4u8; 32],
            "a rename overwrites the recorded label"
        );
    }

    /// A blanked set's listing carries no plaintext name, so the pull opens the
    /// sealed label under the owner key it seals with and records the opened
    /// name — what the re-seed's target pre-create, signer and result lines
    /// read. The seal is made exactly as the app's keyed create makes it.
    #[tokio::test]
    async fn a_blanked_covered_folders_name_is_opened_from_its_sealed_label() {
        let rig = Rig::new(&[]);
        let folders = FakeFolders::default();
        folders.stage_file([0x21u8; 32], &[body(7, 3_000)]);
        let mut pull = rig.pull(Some(1));
        pull.folder_source = Some(&folders);

        let root = fauna_core::file_download::FileDownloadKeys::owner(seed_key())
            .label_seal_root()
            .unwrap()
            .expect("an owner key has a label root");
        let sealed = fauna_core::label_custody::seal_set_name(&root, "Photos")
            .unwrap()
            .unwrap();
        *rig.nest.transport().covered_folders.lock().unwrap() = vec![CoveredFolder {
            folder_id: 5,
            folder_set: folder_set(),
            name: None,
            name_hash: Some(fauna_protocol::ByteBuf::from(
                fauna_core::path_crypto::set_name_hash("Photos").to_vec(),
            )),
            name_sealed: Some(fauna_protocol::ByteBuf::from(sealed)),
            ..Default::default()
        }];
        pull.run_covered_folders(0).await;
        assert_eq!(
            rig.store.folder_names().await.unwrap(),
            std::collections::BTreeMap::from([(folder_set(), "Photos".to_string())]),
            "the opened label is the restored folder's display name"
        );
    }

    /// The whole point of the axis: a covered folder's head lands **as-is** —
    /// the stored blob files are byte-identical to the source's sealed bodies
    /// (a re-seal would key and frame them differently and this fails), the
    /// held row carries the source manifest hash under the destination-set
    /// layout, an unchanged head re-stores nothing, and a dropped path
    /// tombstones.
    #[tokio::test]
    async fn a_covered_folder_mirrors_as_is_and_converges() {
        let rig = Rig::new(&[]);
        let folders = FakeFolders::default();
        let path_hash = [0x21u8; 32];
        let c1 = body(7, 3_000);
        let c2 = body(8, 3_000);
        let manifest_hash = folders.stage_file(path_hash, &[c1.clone(), c2.clone()]);
        cover_folder(&rig);
        let mut pull = rig.pull(None);
        pull.folder_source = Some(&folders);

        let reports = pull.run_covered_folders(0).await;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].stored_paths, 1);

        // Byte identity, read off the disk itself: the blob under c1's store
        // key IS c1 — never a re-sealed body under some other key.
        let k1 = ContentHash::of_raw(&c1);
        let hex = hex::encode(k1.digest());
        let blob = rig
            ._dir
            .path()
            .join("custody")
            .join("blobs")
            .join(&hex[0..2])
            .join(&hex);
        assert_eq!(
            std::fs::read(&blob).expect("the chunk blob file exists"),
            c1,
            "the mirrored chunk must be the source's bytes verbatim"
        );

        // The held row lives under the destination-set layout, keyed by the
        // SOURCE path_hash, carrying the SOURCE manifest hash.
        let rows = rig.store.held().await.unwrap();
        let path = format!("{}/{}", folder_set(), hex::encode(path_hash));
        assert!(
            rows.iter()
                .any(|r| r.path == path && r.manifest_hash == hex::encode(manifest_hash.digest())),
            "held rows: {rows:?}"
        );

        // The convergent change detector: nothing to re-store.
        let reports = pull.run_covered_folders(0).await;
        assert_eq!(reports[0].stored_paths, 0);
        assert_eq!(reports[0].tombstoned_paths, 0);

        // The source drops the path → the mirror tombstones it (grace clock,
        // never a hard delete).
        folders.drop_path(path_hash);
        let reports = pull.run_covered_folders(0).await;
        assert_eq!(reports[0].tombstoned_paths, 1);
        let rows = rig.store.held().await.unwrap();
        assert!(
            CustodianStore::live_at(&rows, &path).is_none(),
            "the path is no longer live locally"
        );
    }

    /// **Corpus gap #1, closed.** A mirrored folder row must carry the source
    /// path's *sealed name*, or a custodian-sourced re-seed can never re-home it
    /// into a live folder: materialize re-creates rows from
    /// `(path_hash, path_sealed, manifest_hash)`, `path_hash` is one-way, and
    /// the nest refuses a live row minted without a seal. Before this the mirror
    /// carried `path_hash` + `manifest_hash` only.
    #[tokio::test]
    async fn a_mirrored_folder_row_carries_the_sources_sealed_name() {
        let rig = Rig::new(&[]);
        let folders = FakeFolders::default();
        let path_hash = [0x2Au8; 32];
        folders.stage_file(path_hash, &[body(11, 2_000)]);
        cover_folder(&rig);
        let mut pull = rig.pull(None);
        pull.folder_source = Some(&folders);

        let reports = pull.run_covered_folders(0).await;
        assert_eq!(reports[0].stored_paths, 1);

        let rows = rig.store.held().await.unwrap();
        let path = format!("{}/{}", folder_set(), hex::encode(path_hash));
        let live = CustodianStore::live_at(&rows, &path).expect("the path is live");
        assert_eq!(
            live.path_sealed.as_deref(),
            Some(hex::encode(format!("sealed:{}", hex::encode(path_hash))).as_str()),
            "the mirror row must carry the head's sealed name, hex-encoded"
        );
    }

    /// Hash verification is the plane's audit floor: a chunk whose bytes do
    /// not re-hash to their store key is refused — the pass fails and nothing
    /// lands, rather than corrupt bytes entering the mirror.
    #[tokio::test]
    async fn a_corrupt_chunk_refuses_the_folder_pass() {
        let rig = Rig::new(&[]);
        let folders = FakeFolders::default();
        let path_hash = [0x22u8; 32];
        let c1 = body(9, 2_000);
        folders.stage_file(path_hash, std::slice::from_ref(&c1));
        // Corrupt the body under its (unchanged) store key.
        folders
            .chunks
            .lock()
            .unwrap()
            .insert(ContentHash::of_raw(&c1), b"junk".to_vec());
        cover_folder(&rig);
        let mut pull = rig.pull(None);
        pull.folder_source = Some(&folders);

        let reports = pull.run_covered_folders(0).await;
        assert!(
            reports.is_empty(),
            "the per-folder failure is logged and skipped, never stored"
        );
        let rows = rig.store.held().await.unwrap();
        assert!(
            rows.iter().all(|r| !r.path.starts_with("__folder/")),
            "nothing landed under the mirror set: {rows:?}"
        );
    }

    /// No folder source ⇒ the axis is silent — and the coverage read is never
    /// made (the existing segment-only hosts and tests stay untouched).
    #[tokio::test]
    async fn without_a_folder_source_the_axis_is_a_no_op() {
        let rig = Rig::new(&[]);
        cover_folder(&rig);
        let reports = rig.pull(None).run_covered_folders(0).await;
        assert!(reports.is_empty());
    }

    // ── The copy that never heals ─────────────────────

    /// **A live path this device cannot produce re-enters the diff and is
    /// re-fetched** — the goal's ratified "content addressing re-converges on
    /// the next pull" (`docs/goal/architecture/segment-backup-protocol.md`
    /// § Client-device custodian (pull) → *Custody = the local store*;
    /// `docs/goal/behavior/backup-destinations.md` § Third destination kind →
    /// *Durability + labeling*).
    ///
    /// Nothing re-converged before this: the diff reads the index alone, and
    /// the index still calls the rotted path held at its current manifest, so
    /// no later pass ever asked for those bytes again. The self-audit found it
    /// and reported it through the check-in — and that was the end of it, an
    /// alarm with no remedy behind it, on the one destination kind whose whole
    /// argument is that it can restore with no nest alive.
    ///
    /// Both segment halves and the corpus mirror are asserted, because each
    /// heals by a different arm: the pair through the diff's re-fetch, the
    /// mirror through the repair of bytes step 5 re-seals every pass anyway.
    #[tokio::test]
    async fn a_path_the_audit_cannot_produce_is_refetched_by_the_next_pass() {
        const T0: i64 = 1_000_000;
        let payload = b"segment one".to_vec();
        let rig = Rig::new(&[(1, payload.clone())]);
        let pull = rig.pull(None);
        pull.run_all_kinds(T0).await;

        let rotted = rig.rot_every_blob();
        assert!(rotted > 0, "the first pass stored no blobs to rot");

        let scope_hex = hex::encode(SCOPE);
        let dat = in_mail(&crate::segment_backup::segment_rel_path(&scope_hex, 1));
        let meta = in_mail(&crate::segment_backup::segment_meta_rel_path(&scope_hex, 1));
        let mirror = in_mail(&crate::segment_backup::manifest_rel_path(
            &scope_hex, "mail",
        ));
        let keys = FileDownloadKeys::owner(seed_key());
        assert!(
            rig.store.open(&dat, &keys).await.is_err(),
            "the rot really did break what the index calls live"
        );

        // A pass past the debounce audits, FAILS — and must then re-converge.
        pull.run_all_kinds(T0 + AUDIT_MIN_INTERVAL_SECS + 1).await;

        assert_eq!(
            rig.store.open(&dat, &keys).await.unwrap(),
            payload,
            "the segment the audit could not produce must be re-fetched and \
             rewritten, not merely reported"
        );
        assert!(
            rig.store.open(&meta, &keys).await.is_ok(),
            "the sidecar heals with its `.dat` — the pair is admitted as one unit, \
             and a `.dat` held without a readable `.meta` is a segment restore \
             refuses by name"
        );
        assert!(
            rig.store.open(&mirror, &keys).await.is_ok(),
            "the corpus mirror heals too: it is the only thing binding a sidecar to \
             its `.dat`, so a rotted mirror makes every pair it names unrestorable"
        );
        assert!(
            rig.store.audit_record().await.failed_paths.is_empty(),
            "a pass that re-produced every flagged path clears the repair list — \
             otherwise the next pass re-fetches a healthy corpus forever"
        );
    }

    /// The repair list is cleared **on proof, never on attempt** — and the
    /// verdict stays the audit's to change.
    ///
    /// A path the pass could not re-produce keeps its flag, so the next pass
    /// tries again rather than the device going quiet over bytes it still
    /// cannot produce. Driven by a source that has dropped the segment, which
    /// is the honest version of "the re-fetch did not happen": the path is
    /// rotted, flagged, and tombstoned rather than re-fetched.
    #[tokio::test]
    async fn a_healed_pass_does_not_report_itself_verified() {
        const T0: i64 = 1_000_000;
        let rig = Rig::new(&[(1, b"segment one".to_vec())]);
        let pull = rig.pull(None);
        pull.run_all_kinds(T0).await;
        assert!(rig.rot_every_blob() > 0);

        pull.run_all_kinds(T0 + AUDIT_MIN_INTERVAL_SECS + 1).await;

        let record = rig.store.audit_record().await;
        assert_eq!(
            record.last_run_passed,
            Some(false),
            "the verdict is the AUDIT's to change, never the repair loop's: a pass \
             that healed the corpus must not report itself verified before the next \
             audit has actually re-sampled it"
        );
        assert_eq!(
            record.last_passed_at,
            Some(T0),
            "and the last-passed clock stays frozen at the last real pass"
        );
    }

    /// **A flag that survives an audit which did not re-sample it is still
    /// re-fetched by the next pass.**
    ///
    /// The store-side merge keeps the flag (`record_audit`); this is the other
    /// end of the same promise — the pull reads the standing list, not just the
    /// latest audit's failures, so a path flagged by audit *n* and neither
    /// repaired nor re-opened by audit *n+1* is still re-entered into the diff.
    /// The second audit is recorded by hand because its only property that
    /// matters is the one a real sample of more than `AUDIT_SAMPLE_K` paths
    /// has most days: it opened other paths, and they passed.
    #[tokio::test]
    async fn a_flag_an_audit_did_not_resample_is_still_refetched() {
        const T0: i64 = 1_000_000;
        let payload = b"segment one".to_vec();
        let rig = Rig::new(&[(1, payload.clone())]);
        let pull = rig.pull(None);
        pull.run_all_kinds(T0).await;
        assert!(rig.rot_every_blob() > 0);

        let scope_hex = hex::encode(SCOPE);
        let dat = in_mail(&crate::segment_backup::segment_rel_path(&scope_hex, 1));
        let t1 = T0 + AUDIT_MIN_INTERVAL_SECS + 1;
        rig.store
            .record_audit(
                t1,
                &crate::custodian_store::SelfAuditReport {
                    sampled_paths: vec![dat.clone()],
                    failed_paths: vec![dat.clone()],
                },
            )
            .await
            .unwrap();
        rig.store
            .record_audit(
                t1 + 1,
                &crate::custodian_store::SelfAuditReport {
                    sampled_paths: vec!["some/other-path.dat".into()],
                    failed_paths: Vec::new(),
                },
            )
            .await
            .unwrap();

        // Inside the debounce, so this pass runs no audit of its own: the only
        // thing that can send it after `dat` is the standing list.
        pull.run_all_kinds(t1 + 2).await;

        let keys = FileDownloadKeys::owner(seed_key());
        assert_eq!(
            rig.store.open(&dat, &keys).await.unwrap(),
            payload,
            "an audit that never opened the flagged path must not cancel its repair"
        );
    }

    /// **A repair that adds no generation is not charged against the cap**.
    ///
    /// Rewriting the bytes of a path the store already calls live at this
    /// manifest grows the held total by nothing — the index row was right all
    /// along. Charging the full pair before the convergent put refused it near
    /// the cap, so the one device whose store was already nearly full could
    /// never heal, and fetched the pair in full every pass only to drop it.
    /// The mirror takes the same rule: it re-puts converging bytes every pass.
    ///
    /// Mutation check: charge `sealed.stored_bytes()` unconditionally in
    /// `CustodianStore::charge_for` and the segment is skipped for cap.
    #[tokio::test]
    async fn a_converging_repair_is_not_charged_against_the_cap() {
        const T0: i64 = 1_000_000;
        let payload = b"segment one".to_vec();
        let rig = Rig::new(&[(1, payload.clone())]);
        rig.pull(None).run_all_kinds(T0).await;
        let held = rig.nest_saw().held_bytes;
        assert!(rig.rot_every_blob() > 0);

        // One byte of headroom: any real charge for the pair is refused.
        let pull = rig.pull(Some(held + 1));
        let reports = pull.run_all_kinds(T0 + AUDIT_MIN_INTERVAL_SECS + 1).await;

        assert!(
            reports.iter().all(|r| r.skipped_for_cap.is_empty()),
            "a repair that adds nothing must not be refused for cap: {reports:?}"
        );
        let scope_hex = hex::encode(SCOPE);
        let keys = FileDownloadKeys::owner(seed_key());
        let dat = in_mail(&crate::segment_backup::segment_rel_path(&scope_hex, 1));
        let mirror = in_mail(&crate::segment_backup::manifest_rel_path(
            &scope_hex, "mail",
        ));
        assert_eq!(rig.store.open(&dat, &keys).await.unwrap(), payload);
        assert!(rig.store.open(&mirror, &keys).await.is_ok());
        assert_eq!(
            rig.nest_saw().held_bytes,
            held,
            "healing moved bytes, not the held total"
        );
    }

    /// **One unfetchable flagged segment fails that segment, not the pass**.
    ///
    /// A repair-only segment is already held — the index counts it, and
    /// `high_water` already covers it — so skipping it leaves every figure the
    /// check-in carries true; its flag stays on the repair list for the next
    /// pass. Aborting instead cost the tombstones, the mirror, every other
    /// segment and the check-in, every pass for as long as the source could
    /// not serve it.
    ///
    /// Mutation check: restore the `?` on the repair-only fetch and the pass
    /// errors out before segment 3 is stored.
    #[tokio::test]
    async fn an_unfetchable_flagged_segment_does_not_stop_the_pass() {
        const T0: i64 = 1_000_000;
        let rig = Rig::new(&[(1, b"one".to_vec()), (2, b"two".to_vec())]);
        rig.pull(None).run_all_kinds(T0).await;
        assert!(rig.rot_every_blob() > 0);
        rig.source.unfetchable.lock().unwrap().insert(1);
        rig.source.set(3, b"three".to_vec());

        let reports = rig
            .pull(None)
            .run_all_kinds(T0 + AUDIT_MIN_INTERVAL_SECS + 1)
            .await;

        let report = reports.first().expect("the kind pass must complete");
        assert_eq!(report.unrepaired_segments, vec![1]);
        assert!(report.stored_segments.contains(&3));
        assert_eq!(rig.nest_saw().high_water, 4, "the pass still checked in");

        let scope_hex = hex::encode(SCOPE);
        let keys = FileDownloadKeys::owner(seed_key());
        let two = in_mail(&crate::segment_backup::segment_rel_path(&scope_hex, 2));
        assert_eq!(rig.store.open(&two, &keys).await.unwrap(), b"two".to_vec());
        let one = in_mail(&crate::segment_backup::segment_rel_path(&scope_hex, 1));
        assert!(
            rig.store.audit_record().await.failed_paths.contains(&one),
            "the unrepaired segment keeps its flag for the next pass"
        );
    }

    /// **A flag whose repair keeps failing keeps the check-in failing** —
    /// through a real audit whose rotating sample misses the flagged path.
    ///
    /// The carve-out above lets the pass check in with a segment unrepaired;
    /// this is what keeps that check-in honest. The verdict is the latest
    /// audit's alone, so once a store holds more than `AUDIT_SAMPLE_K` live
    /// paths an audit that sampled only its rotating paths answered "passed",
    /// and every check-in after it carried `ok` over a path the store's own
    /// repair list named unproducible.
    ///
    /// Mutation check: drop the standing-flag loop from
    /// `CustodianStore::self_audit` and the check-in reports `ok`.
    #[tokio::test]
    async fn an_unrepaired_flag_keeps_every_check_in_failing() {
        const T0: i64 = 1_000_000;
        let segments: Vec<(u32, Vec<u8>)> = (1..=20u32)
            .map(|id| (id, format!("segment {id}").into_bytes()))
            .collect();
        let rig = Rig::new(&segments);
        let pull = rig.pull(None);
        pull.run_all_kinds(T0).await;

        // Rot everything and flag everything, then let one pass inside the
        // debounce repair it all — except segment 1, which the source can no
        // longer serve. What is left is one standing flag on a store whose
        // every other path is healthy.
        assert!(rig.rot_every_blob() > 0);
        let rows = rig.store.held().await.unwrap();
        let mut live: Vec<String> = rows
            .iter()
            .map(|r| r.path.clone())
            .filter(|p| CustodianStore::live_at(&rows, p).is_some())
            .collect();
        live.sort_unstable();
        live.dedup();
        assert!(
            live.len() > AUDIT_SAMPLE_K,
            "the sample must be able to miss"
        );
        let t1 = T0 + AUDIT_MIN_INTERVAL_SECS + 1;
        rig.store
            .record_audit(
                t1,
                &crate::custodian_store::SelfAuditReport {
                    sampled_paths: live.clone(),
                    failed_paths: live.clone(),
                },
            )
            .await
            .unwrap();
        rig.source.unfetchable.lock().unwrap().insert(1);
        pull.run_all_kinds(t1 + 1).await;

        let scope_hex = hex::encode(SCOPE);
        let dat = in_mail(&crate::segment_backup::segment_rel_path(&scope_hex, 1));
        let flagged = rig.store.audit_record().await.failed_paths;
        assert!(
            flagged.contains(&dat),
            "segment 1 stays flagged: {flagged:?}"
        );
        assert!(
            flagged.iter().all(|p| p.contains("00000001")),
            "every other path was repaired: {flagged:?}"
        );

        // The next audit's `now` is its seed: pick one whose rotating sample
        // opens none of the flagged paths.
        let t2 = (t1 + AUDIT_MIN_INTERVAL_SECS + 1..)
            .find(|&t| {
                fauna_client_backup::audit::sample_indices(live.len(), AUDIT_SAMPLE_K, t as u64)
                    .into_iter()
                    .all(|i| !flagged.contains(&live[i]))
            })
            .unwrap();
        pull.run_all_kinds(t2).await;
        assert_eq!(
            rig.nest_saw().audit_state.as_deref(),
            Some(AUDIT_STATE_FAILED),
            "the store's own repair list names a path it cannot produce; reporting \
             the destination verified here is the false-ok a standalone restore \
             discovers"
        );

        pull.run_all_kinds(t2 + 60).await;
        assert_eq!(
            rig.nest_saw().audit_state.as_deref(),
            Some(AUDIT_STATE_FAILED),
            "and every check-in after it, while the flag stands"
        );
    }

    /// The carve-out above is for a segment already held. A segment the store
    /// has **never** held still stops the pass: skipping it would let a later
    /// segment raise `high_water` over a gap, and the check-in would claim a
    /// segment this device does not have.
    #[tokio::test]
    async fn an_unfetchable_new_segment_still_stops_the_pass() {
        const T0: i64 = 1_000_000;
        let rig = Rig::new(&[(1, b"one".to_vec()), (2, b"two".to_vec())]);
        rig.source.unfetchable.lock().unwrap().insert(1);
        assert!(rig.pull(None).run_once("mail", T0).await.is_err());
    }

    /// The folder plane repairs by the same rule, through its own early-out.
    ///
    /// `run_folder_once` skips a path whose held manifest equals the head's
    /// before it fetches anything, which is the change detector the axis is
    /// built on — and which makes a rotted mirror permanently unrepairable,
    /// exactly as the segment diff did. A flagged path must take the fetch.
    #[tokio::test]
    async fn an_unproducible_folder_mirror_is_refetched_by_the_next_pass() {
        const T0: i64 = 1_000_000;
        let rig = Rig::new(&[]);
        let folders = FakeFolders::default();
        let path_hash = [0x21u8; 32];
        let c1 = body(7, 3_000);
        let c2 = body(8, 3_000);
        folders.stage_file(path_hash, &[c1.clone(), c2.clone()]);
        cover_folder(&rig);
        let mut pull = rig.pull(None);
        pull.folder_source = Some(&folders);

        pull.run_all_kinds(T0).await;
        let path = format!("{}/{}", folder_set(), hex::encode(path_hash));
        assert!(rig.store.verify_presence(&path).await.is_ok());

        assert!(rig.rot_every_blob() > 0);
        assert!(
            rig.store.verify_presence(&path).await.is_err(),
            "the rot really did break the mirrored path"
        );

        pull.run_all_kinds(T0 + AUDIT_MIN_INTERVAL_SECS + 1).await;

        assert!(
            rig.store.verify_presence(&path).await.is_ok(),
            "a mirrored path the audit could not produce must be re-fetched as-is; \
             the axis's unchanged-head early-out is precisely what stopped it"
        );
        // As-is, still: the healed blob is the source's own sealed body, not a
        // re-seal under some other key — the property the whole axis rests on.
        let hex_key = hex::encode(ContentHash::of_raw(&c1).digest());
        assert_eq!(
            std::fs::read(
                rig._dir
                    .path()
                    .join("custody")
                    .join("blobs")
                    .join(&hex_key[0..2])
                    .join(&hex_key)
            )
            .expect("the chunk blob file exists again"),
            c1,
            "the repair rewrites the SOURCE's bytes verbatim"
        );
    }
}
