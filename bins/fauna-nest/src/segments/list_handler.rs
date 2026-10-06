//! `fauna.segments.list` WS-RPC handler — enumerate the actor's live
//! segments for the requested kind. Owner-or-custodian auth: the request's
//! `actor_id` must equal the WS-handshake-validated auth actor, **or** the
//! caller must hold a live custody row covering that owner's
//! `content:<kind>:<actor_id>` plane (`crate::custody_admission`); otherwise
//! the handler returns `fauna.segments.not_owner`, identically in both cases.
//!
//! The custody arm is the BULK half of the nest custody door (`account-data-plane.md` § Implementation status today → *Recorded residuals
//! (W8 (account-data-plane.md § Workstreams)-wide)*). Its index half — `fauna.sync.changes.list`'s record-cid arm —
//! hands a custodian record-CID *coordinates*; without this door those
//! coordinates named bytes the custodian could never fetch.
//!
//! Plan 5 of the message-segment-store track (design tracked internally).
//!
//! Kinds reach this plane through their own per-kind rollout; the rest return
//! `fauna.segments.unknown_kind`. Five content kinds are wired, and all five are adoptable
//! (`fauna_account_store::segments::ADOPTABLE_KINDS` — records filed under
//! the content hash `admit` re-hashes every block against):
//!
//! - `mail` (Plan 5) — the backup arm's kind; adoptable since its 2026-08-17
//!   record-identity cutover (`segments::mail::append_record` files under
//!   `Cid::of_dag_cbor(envelope_bytes)`).
//! - `post` (2026-08-11) — for the **adoption** arm: a fresh replica
//!   bootstraps its own-posts content scope by pulling those segments
//!   (`account-sync-plane.md` § Feeds and cursors → *Scope partition*, and
//!   `account-data-plane.md` § the bootstrap contract); filed under
//!   `Cid::of_dag_cbor(body)` from
//!   birth (`segments::post::append_body`).
//! - `calendar` / `card` (2026-08-17, the serve-plane wiring — row 69):
//!   content-hash-filed since their joint cutover leg, and served here so an
//!   admitted custodian can actually reach them.
//! - `mail-placement` (2026-09-26), `calendar-placement` and `card-placement`
//!   (2026-09-28, with their kinds' materialize arms) — the message kinds'
//!   placement journals, each its kind's other half
//!   (`segment-backup-protocol.md` § Client-device custodian (pull) →
//!   *Restore* → *The placement journal rides the set*). Served to the
//!   **owner alone**: a journal rests as floor plaintext (mailbox and calendar
//!   names, flags, ETags), not as a sealed content kind, so the
//!   custody-admission door refuses every journal tag to every granted holder
//!   (`crate::custody_admission::admit_segment_plane_for_custody`). They are
//!   the tags here answered by a `PlacementJournal` rather than a
//!   `SegmentManager` — [`ServedSegments`] is that seam.
//!
//! - `conv` (2026-08-18, ruled + built — row 70): the one **channel**-scoped
//!   kind (`AppState.conv_segments` keys on `channel_id`), content-hash-filed
//!   since its 2026-08-17 cutover leg. Addressed by `actor_id = <channel_hex>`
//!   — the scope id, never an owner — and authorized by the **member-mint
//!   rule**: an explicit-list custody row naming exactly that channel scope,
//!   whose owner is a *current* channel member — the floor roster
//!   (`conversation-rooms.md` § The floor roster), the authoritative roster,
//!   never the send-writable `actor_channels` routing roster
//!   — re-derived per
//!   request so leaving the channel severs like a revoke
//!   (`crate::custody_admission::custody_admits_co_authored_scope`;
//!   `message-segment-store.md` § *Which kinds the two planes serve*).
//!
//! ⚠ Listing a kind here does **not** enrol it in backup: that is
//! [`fauna_sync_engine::segment_backup::BACKED_UP_KINDS`], whose own
//! compile-time pin in `segments::backup_source` guards the direction that
//! actually loses data (a kind swept by a client but not by the nest).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use fauna_protocol::segments::{SegmentRef, SegmentsListReply, SegmentsListRequest};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// Register `fauna.segments.list` on the dispatcher. The umbrella
/// `register_segments_handlers` in `mod.rs` calls this plus
/// `compact_handler::register_compact_handler`.
pub fn register_list_handler(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.segments.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_handler(),
        },
    );
}

/// Register `fauna.segments.counter_floor`
/// (`segment-backup-protocol.md` § Client-device custodian (pull) → *Restore*
/// → *Recovery into the lived-in nest that regressed*, part (0)).
///
/// `forbid_replay: false` — monotonic and idempotent: a repeat raises nothing
/// and answers the same counter.
pub fn register_counter_floor_handler(b: &mut RpcRouterBuilder) {
    b.add(
        fauna_protocol::segments::KIND_SEGMENTS_COUNTER_FLOOR,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: counter_floor_handler(),
        },
    );
}

/// The one write this plane takes: raise the owner's saved counter for one
/// family to the floor the device pinned.
///
/// **Owner only** — unlike the list, no custody grant reaches it: a floor is a
/// write to the owner's own store, and the caller's worst case against it is
/// spending ids, a capability only the owner's own devices hold. The family is
/// resolved by the same serve-tag gate the list and the byte route use
/// ([`served_segments_for_kind`]); a floor beyond the id space's headroom is
/// refused `invalid_params` before anything is touched.
fn counter_floor_handler() -> RpcHandler {
    Box::new(|state, auth_actor, payload| {
        Box::pin(async move {
            use fauna_protocol::segments::{
                SegmentsCounterFloorReply, SegmentsCounterFloorRequest,
            };
            let req: SegmentsCounterFloorRequest = decode(&payload).map_err(|e| {
                RpcError::new("fauna.segments.malformed", "error.segments.malformed")
                    .with_details_text(format!("decode: {e}"))
            })?;
            let scope: [u8; 32] = fauna_core::hex32::decode(&req.actor_id).map_err(|_| {
                RpcError::new("fauna.segments.malformed", "error.segments.bad_actor_id")
            })?;
            if scope != auth_actor {
                return Err(RpcError::new(
                    "fauna.segments.not_owner",
                    "error.segments.not_owner",
                ));
            }
            let served = served_segments_for_kind(&state, &req.kind).ok_or_else(|| {
                RpcError::new("fauna.segments.unknown_kind", "error.segments.unknown_kind")
            })?;
            fauna_segment_store::check_counter_floor(req.floor)
                .map_err(|e| crate::rpc_errors::invalid_params_ns("segments", e.to_string()))?;
            let next_segment_id = served.floor_counter(&scope, req.floor).await.map_err(|e| {
                RpcError::new("fauna.segments.internal", "error.segments.internal")
                    .with_details_text(format!("{e}"))
            })?;
            let reply = SegmentsCounterFloorReply {
                next_segment_id,
                extra: Default::default(),
            };
            let bytes = encode_canonical(&reply).map_err(|e| {
                RpcError::new("fauna.segments.internal", "error.segments.internal")
                    .with_details_text(format!("encode reply: {e}"))
            })?;
            Ok(Bytes::from(bytes.to_vec()))
        })
    })
}

fn list_handler() -> RpcHandler {
    Box::new(|state, auth_actor, payload| {
        Box::pin(async move {
            let req: SegmentsListRequest = match decode(&payload) {
                Ok(r) => r,
                Err(e) => {
                    return Err(RpcError::new(
                        "fauna.segments.malformed",
                        "error.segments.malformed",
                    )
                    .with_details_text(format!("decode: {e}")));
                }
            };

            // Decode the request actor_id (hex) and verify owner-only.
            let req_actor_bytes: [u8; 32] = match fauna_core::hex32::decode(&req.actor_id) {
                Ok(b) => b,
                Err(_) => {
                    return Err(RpcError::new(
                        "fauna.segments.malformed",
                        "error.segments.bad_actor_id",
                    ));
                }
            };
            // Owner, or a custodian the owner granted this content plane to
            // (the bulk half of the nest custody door). The
            // custody verdict is the LIVE capability row, re-derived here on
            // every request, so `fauna.capabilities.revoke` severs a live
            // session at its very next enumeration.
            //
            // ⚠ The refusal is deliberately the SAME `not_owner` in both
            // directions: a custodian whose grant does not cover this plane
            // learns exactly what an unrelated stranger learns. Answering a
            // distinguishable "you hold a grant, but not that one" would make
            // this door an oracle for the shape of someone else's grants.
            if req_actor_bytes != auth_actor
                && crate::custody_admission::admit_segment_plane_for_custody(
                    &state,
                    &auth_actor,
                    &req_actor_bytes,
                    &req.kind,
                )
                .await
                .is_err()
            {
                return Err(RpcError::new(
                    "fauna.segments.not_owner",
                    "error.segments.not_owner",
                ));
            }

            if served_segments_for_kind(&state, &req.kind).is_none() {
                return Err(RpcError::new(
                    "fauna.segments.unknown_kind",
                    "error.segments.unknown_kind",
                ));
            }

            // The OWNER's segments, never the caller's — the two are the same
            // actor on the self-serve path and deliberately different on the
            // custody path.
            list_segments(state, &req.kind, &req_actor_bytes)
                .await
                .map_err(|e| {
                    RpcError::new("fauna.segments.internal", "error.segments.internal")
                        .with_details_text(format!("{e}"))
                })
        })
    })
}

/// What serves one kind tag on this plane: a content kind's segment store, or
/// a placement journal.
///
/// The two are different managers over the same on-disk segment shape (a
/// CARv2 `.dat` and its `.meta` sidecar), and every read this plane performs
/// exists on both. This enum is the whole of the difference, so the list
/// handler, the byte route and the nest-local backup source each stay one code
/// path with no per-kind branch of their own.
pub(crate) enum ServedSegments<'a> {
    /// A content kind (`mail`, `post`, `calendar`, `card`, `conv`).
    Content(&'a Arc<fauna_segment_store::SegmentManager>),
    /// A backed-up kind's placement journal (`mail-placement`,
    /// `calendar-placement`, `card-placement`).
    Placement(&'a dyn ServedJournal),
}

/// The reads this plane takes of a placement journal, over any journal kind.
///
/// One object-safe seam over [`crate::segments::PlacementJournal`]'s own
/// backup read surface, so [`ServedSegments`] needs one journal variant rather
/// than one per journal kind — a kind joining the sweep adds a gate arm and
/// nothing here.
#[async_trait::async_trait]
pub(crate) trait ServedJournal: Send + Sync {
    async fn finalize_open(&self, scope_id: &[u8; 32]) -> anyhow::Result<()>;
    /// The live ids and the journal manifest's saved `next_seg_id`, one
    /// observation ([`ServedSegments::live_segment_ids`]).
    async fn live_segments_for_backup(
        &self,
        scope_id: &[u8; 32],
    ) -> anyhow::Result<(Vec<u32>, u32)>;
    async fn describe_segment_for_backup(
        &self,
        scope_id: &[u8; 32],
        segment_id: u32,
    ) -> anyhow::Result<fauna_segment_store::SegmentBackupMeta>;
    async fn read_segment_pair(
        &self,
        scope_id: &[u8; 32],
        segment_id: u32,
    ) -> anyhow::Result<fauna_segment_store::SegmentPairBytes>;
    fn segment_file_path(&self, scope_id: &[u8; 32], segment_id: u32) -> std::path::PathBuf;
    fn segment_meta_path(&self, scope_id: &[u8; 32], segment_id: u32) -> std::path::PathBuf;
    /// `PlacementJournal::floor_counter`.
    async fn floor_counter(&self, scope_id: &[u8; 32], floor: u32) -> anyhow::Result<u32>;
}

#[async_trait::async_trait]
impl<K: crate::segments::PlacementKind> ServedJournal for crate::segments::PlacementJournal<K> {
    async fn finalize_open(&self, scope_id: &[u8; 32]) -> anyhow::Result<()> {
        crate::segments::PlacementJournal::finalize_open(self, scope_id).await
    }
    async fn live_segments_for_backup(
        &self,
        scope_id: &[u8; 32],
    ) -> anyhow::Result<(Vec<u32>, u32)> {
        crate::segments::PlacementJournal::live_segments_for_backup(self, scope_id).await
    }
    async fn describe_segment_for_backup(
        &self,
        scope_id: &[u8; 32],
        segment_id: u32,
    ) -> anyhow::Result<fauna_segment_store::SegmentBackupMeta> {
        crate::segments::PlacementJournal::describe_segment_for_backup(self, scope_id, segment_id)
            .await
    }
    async fn read_segment_pair(
        &self,
        scope_id: &[u8; 32],
        segment_id: u32,
    ) -> anyhow::Result<fauna_segment_store::SegmentPairBytes> {
        crate::segments::PlacementJournal::read_segment_pair(self, scope_id, segment_id).await
    }
    fn segment_file_path(&self, scope_id: &[u8; 32], segment_id: u32) -> std::path::PathBuf {
        crate::segments::PlacementJournal::segment_file_path(self, scope_id, segment_id)
    }
    fn segment_meta_path(&self, scope_id: &[u8; 32], segment_id: u32) -> std::path::PathBuf {
        crate::segments::PlacementJournal::segment_meta_path(self, scope_id, segment_id)
    }
    async fn floor_counter(&self, scope_id: &[u8; 32], floor: u32) -> anyhow::Result<u32> {
        crate::segments::PlacementJournal::floor_counter(self, scope_id, floor).await
    }
}

impl ServedSegments<'_> {
    /// Raise this family's saved counter to at least `floor` — the one write
    /// this plane takes ([`register_counter_floor_handler`]). Returns the
    /// counter as it now stands.
    pub(crate) async fn floor_counter(
        &self,
        scope_id: &[u8; 32],
        floor: u32,
    ) -> anyhow::Result<u32> {
        match self {
            Self::Content(mgr) => Ok(mgr.floor_counter(scope_id, floor).await?),
            Self::Placement(journal) => journal.floor_counter(scope_id, floor).await,
        }
    }

    /// Finalize whatever segment is open, so the files on disk are a
    /// self-consistent pair and the sidecar exists at all.
    pub(crate) async fn finalize_open(&self, scope_id: &[u8; 32]) -> anyhow::Result<()> {
        match self {
            Self::Content(mgr) => {
                mgr.finalize_open(scope_id).await?;
            }
            Self::Placement(journal) => journal.finalize_open(scope_id).await?,
        }
        Ok(())
    }

    /// The live segment ids, in manifest order, and the manifest's saved
    /// `next_seg_id` — read together, as one observation of the manifest, so
    /// the counter a listing advertises is the one its segments were live
    /// under ([`SegmentsListReply::next_segment_id`]).
    pub(crate) async fn live_segment_ids(
        &self,
        scope_id: &[u8; 32],
    ) -> anyhow::Result<(Vec<u32>, u32)> {
        match self {
            Self::Content(mgr) => {
                mgr.finalize_open(scope_id).await?;
                let km = mgr.load_manifest(scope_id).await?.kind_manifest;
                Ok((km.live_segments, km.next_seg_id))
            }
            Self::Placement(journal) => journal.live_segments_for_backup(scope_id).await,
        }
    }

    /// One segment's store-level description.
    pub(crate) async fn describe(
        &self,
        scope_id: &[u8; 32],
        segment_id: u32,
    ) -> anyhow::Result<fauna_segment_store::SegmentBackupMeta> {
        match self {
            Self::Content(mgr) => mgr
                .describe_segment_for_backup(scope_id, segment_id)
                .await
                .map_err(|e| anyhow::anyhow!("describe_segment_for_backup: {e}")),
            Self::Placement(journal) => {
                journal
                    .describe_segment_for_backup(scope_id, segment_id)
                    .await
            }
        }
    }

    /// One finalized segment's pair, both files read as one observation.
    pub(crate) async fn read_pair(
        &self,
        scope_id: &[u8; 32],
        segment_id: u32,
    ) -> anyhow::Result<fauna_segment_store::SegmentPairBytes> {
        match self {
            Self::Content(mgr) => Ok(mgr.read_segment_pair(scope_id, segment_id).await?),
            Self::Placement(journal) => journal.read_segment_pair(scope_id, segment_id).await,
        }
    }

    /// On-disk path of a segment's `.dat`.
    pub(crate) fn file_path(&self, scope_id: &[u8; 32], segment_id: u32) -> std::path::PathBuf {
        match self {
            Self::Content(mgr) => mgr.segment_file_path(scope_id, segment_id),
            Self::Placement(journal) => journal.segment_file_path(scope_id, segment_id),
        }
    }

    /// On-disk path of a segment's `.meta` sidecar.
    pub(crate) fn meta_path(&self, scope_id: &[u8; 32], segment_id: u32) -> std::path::PathBuf {
        match self {
            Self::Content(mgr) => mgr.segment_meta_path(scope_id, segment_id),
            Self::Placement(journal) => journal.segment_meta_path(scope_id, segment_id),
        }
    }

    /// Does this kind have a `segment_records` mirror to count tombstones in?
    /// A placement journal has none: its manifest's compacted state is its
    /// only secondary index, and a journal record is never tombstoned.
    fn has_record_mirror(&self) -> bool {
        matches!(self, Self::Content(_))
    }
}

/// What serves `kind` on this plane, or `None` for a tag whose rollout has not
/// landed.
///
/// The single gate for **both** wire surfaces — this handler and the byte
/// route (`crate::segments::segment_route`) — so a kind can never be
/// enumerable but unfetchable, or the reverse.
pub(crate) fn served_segments_for_kind<'a>(
    state: &'a AppState,
    kind: &str,
) -> Option<ServedSegments<'a>> {
    match kind {
        "mail" => Some(ServedSegments::Content(&state.mail_segments)),
        "post" => Some(ServedSegments::Content(&state.post_segments)),
        "calendar" => Some(ServedSegments::Content(&state.cal_segments)),
        "card" => Some(ServedSegments::Content(&state.card_segments)),
        // Scope key = the CHANNEL id, not an actor — the request's actor field
        // carries the scope id, and admission is the member-mint rule
        // (`crate::custody_admission::custody_admits_co_authored_scope`;
        // `message-segment-store.md` § *Which kinds the two planes serve*,
        // ruled 2026-08-18).
        "conv" => Some(ServedSegments::Content(&state.conv_segments)),
        // The backed-up kinds' placement journals: each kind's other half,
        // owner only (the admission door refuses every journal tag to every
        // granted holder). `SegmentFamily::serve_kind` names the tags.
        "mail-placement" => Some(ServedSegments::Placement(state.mail_placement.as_ref())),
        "calendar-placement" => Some(ServedSegments::Placement(state.cal_placement.as_ref())),
        "card-placement" => Some(ServedSegments::Placement(state.card_placement.as_ref())),
        _ => None,
    }
}

async fn list_segments(
    state: Arc<AppState>,
    kind: &str,
    actor_id: &[u8; 32],
) -> anyhow::Result<Bytes> {
    let reply = segment_listing(&state, kind, actor_id).await?;
    let bytes = encode_canonical(&reply).map_err(|e| anyhow::anyhow!("encode reply: {e}"))?;
    Ok(Bytes::from(bytes.to_vec()))
}

/// The live [`SegmentRef`]s for one actor's segments of `kind`, with the
/// manifest's saved `next_seg_id` beside them — the manifest→wire mapping
/// itself, shared by every surface that must never disagree about what this
/// nest holds: the `fauna.segments.list` handler above (read by a
/// *client-driven* backup coordinator, and by an adopting replica's bootstrap
/// source), and the nest-local `SegmentSource`
/// (`crate::segments::backup_source`, read by the *nest-hosted* one). A
/// second copy of this mapping would let the two answers drift, and the
/// coordinator diffs one against state recorded from the other.
///
/// The counter rides the same reply because the backup ledger carries it as
/// the set's generation ([`SegmentsListReply::next_segment_id`]): it is the
/// saved counter, never the greatest live id plus one, so an empty-output
/// compaction that retires the top segment lowers the listing but not the
/// counter — the property the client audit's generation pin rests on.
pub(crate) async fn segment_listing(
    state: &AppState,
    kind: &str,
    actor_id: &[u8; 32],
) -> anyhow::Result<SegmentsListReply> {
    let Some(served) = served_segments_for_kind(state, kind) else {
        anyhow::bail!("segment kind {kind:?} is not served by this nest");
    };

    // Finalize-on-read: the open segment's bytes need to be flushed to
    // disk before we report its size/blake3 — otherwise the source
    // would advertise a SegmentRef whose `size_bytes` doesn't match
    // what a subsequent byte read (the HTTP route, or the nest-local
    // source's `segment_bytes`) serves. It is also what puts the `.meta`
    // sidecar on disk for the pair route to serve.
    let (live, next_seg_id) = served.live_segment_ids(actor_id).await?;
    let mut segments: Vec<SegmentRef> = Vec::with_capacity(live.len());
    for seg_id in &live {
        let meta = describe_segment(&served, &state.db, kind, actor_id, *seg_id).await?;
        segments.push(SegmentRef {
            segment_id: *seg_id,
            blake3_hex: hex::encode(meta.file_blake3),
            bucket: meta.bucket,
            record_count: meta.record_count,
            tombstone_count: meta.tombstone_count,
            size_bytes: meta.size_bytes,
            created_at_secs: meta.created_at_secs,
            // post-finalize there is no open segment by definition;
            // any further append rotates a new segment.
            is_open: false,
            // The sidecar exists post-finalize too, so the pair's other half
            // is always advertisable here.
            meta_blake3_hex: hex::encode(meta.meta_blake3),
            extra: Default::default(),
        });
    }

    Ok(SegmentsListReply {
        segments,
        next_segment_id: next_seg_id,
        extra: Default::default(),
    })
}

/// One segment's wire-facing description, for any kind on this plane.
///
/// The kind-agnostic half is the segment store's own
/// (`SegmentManager::describe_segment_for_backup`, or the journal's twin);
/// only the tombstone count is kind-keyed, because the `segment_records`
/// mirror is — and a placement journal, which has no mirror, counts none.
async fn describe_segment(
    served: &ServedSegments<'_>,
    cache_db: &crate::db::CacheDb,
    kind: &str,
    actor_id: &[u8; 32],
    segment_id: u32,
) -> anyhow::Result<crate::segments::mail::SegmentBackupMeta> {
    let meta = served.describe(actor_id, segment_id).await?;
    let tombstone_count = if served.has_record_mirror() {
        cache_db
            .count_tombstoned_segment_records(actor_id, kind, segment_id)
            .await? as u32
    } else {
        0
    };
    Ok(crate::segments::mail::SegmentBackupMeta {
        file_blake3: meta.file_blake3,
        meta_blake3: meta.meta_blake3,
        size_bytes: meta.byte_size,
        bucket: meta.bucket,
        record_count: meta.record_count,
        tombstone_count,
        created_at_secs: meta.created_at_secs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use crate::routes::AppState;
    use crate::segments::test_helpers::{build_state, floor};
    use bytes::Bytes;
    use fauna_protocol::segments::{SegmentsListReply, SegmentsListRequest};
    use fauna_protocol::{decode_strict as decode, encode_canonical};
    use fauna_segment_store::SegmentManager;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[tokio::test]
    async fn list_handler_rejects_non_owner() {
        let (_tmp, state) = build_state();
        let auth_actor = [0xAAu8; 32];
        let other_actor_hex = "bb".repeat(32);

        let req = SegmentsListRequest {
            kind: "mail".to_string(),
            actor_id: other_actor_hex,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let payload = Bytes::from(bytes.to_vec());

        let h = list_handler();
        let err = h(state, auth_actor, payload).await.unwrap_err();
        assert_eq!(err.code, "fauna.segments.not_owner");
    }

    /// A kind this plane serves no manager for answers `unknown_kind`.
    ///
    /// ⚠ The probe kind must be one `segment_manager_for_kind` genuinely does
    /// **not** know — not merely one unserved on the day the test was written.
    /// This sent `conv`, which stopped being unknown when the conv serve arm
    /// landed (2026-08-18), so the test began asserting a premise
    /// that had stopped being true and reddened against correct code. Every kind
    /// this plane might one day serve is the wrong probe; a syntactically
    /// impossible one cannot decay the same way.
    #[tokio::test]
    async fn list_handler_rejects_unknown_kind() {
        let (_tmp, state) = build_state();
        let auth_actor = [0xAAu8; 32];

        let req = SegmentsListRequest {
            kind: "not-a-segment-kind".to_string(),
            actor_id: hex::encode(auth_actor),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let payload = Bytes::from(bytes.to_vec());

        let h = list_handler();
        let err = h(state, auth_actor, payload).await.unwrap_err();
        assert_eq!(err.code, "fauna.segments.unknown_kind");
    }

    /// `post` reaches this plane for the **adoption** arm (a fresh replica
    /// bootstrapping its own-posts scope), so it must enumerate rather than
    /// answer `unknown_kind` — and the segment it reports must be the one the
    /// post append path actually wrote.
    #[tokio::test]
    async fn list_handler_serves_the_post_kind() {
        let tmp = TempDir::new().expect("tempdir");
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        let mut state = AppState::for_test(db.clone());
        state.post_segments = Arc::new(SegmentManager::new(tmp.path().to_path_buf(), "post"));
        let state = Arc::new(state);

        let author = [0xEEu8; 32];
        crate::segments::post::append_body(&state.post_segments, &state.db, &author, b"a post", 1)
            .await
            .expect("append post body");

        let req = SegmentsListRequest {
            kind: "post".to_string(),
            actor_id: hex::encode(author),
            extra: BTreeMap::new(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());

        let h = list_handler();
        let reply: SegmentsListReply = decode(&h(state, author, payload).await.unwrap()).unwrap();
        assert_eq!(reply.segments.len(), 1);
        assert_eq!(reply.segments[0].record_count, 1);
        assert!(
            !reply.segments[0].is_open,
            "listing finalizes first, which is also what puts the .meta sidecar on disk"
        );
    }

    /// `calendar` and `card` reach this plane for the adoption arm (their record-identity cutover made them adoptable; this gate is what
    /// makes them reachable), so each must enumerate rather than answer
    /// `unknown_kind` — and the segment reported must be the one the kind's
    /// production append path actually wrote.
    #[tokio::test]
    async fn list_handler_serves_the_calendar_and_card_kinds() {
        let tmp_cal = TempDir::new().expect("tempdir");
        let tmp_card = TempDir::new().expect("tempdir");
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        let mut state = AppState::for_test(db.clone());
        state.cal_segments = Arc::new(SegmentManager::new(
            tmp_cal.path().to_path_buf(),
            "calendar",
        ));
        state.card_segments = Arc::new(SegmentManager::new(tmp_card.path().to_path_buf(), "card"));
        let state = Arc::new(state);

        let actor = [0xEEu8; 32];
        crate::segments::cal::append_record(
            &state.cal_segments,
            &state.db,
            &actor,
            &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                b"sealed event".to_vec(),
            ),
            &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                b"hint".to_vec(),
            ),
            &fauna_calendar::segments::CalFloorMetadata {
                created_at: 1_715_000_000,
                ..Default::default()
            },
        )
        .await
        .expect("append calendar record");
        crate::segments::card::append_record(
            &state.card_segments,
            &state.db,
            &actor,
            &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                b"sealed vcard".to_vec(),
            ),
            &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                b"hint".to_vec(),
            ),
            &fauna_contacts::segments::CardFloorMetadata {
                created_at: 1_715_000_000,
                ..Default::default()
            },
        )
        .await
        .expect("append card record");

        for kind in ["calendar", "card"] {
            let req = SegmentsListRequest {
                kind: kind.to_string(),
                actor_id: hex::encode(actor),
                extra: BTreeMap::new(),
            };
            let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
            let h = list_handler();
            let reply: SegmentsListReply =
                decode(&h(state.clone(), actor, payload).await.unwrap()).unwrap();
            assert_eq!(reply.segments.len(), 1, "{kind} enumerates its segment");
            assert_eq!(reply.segments[0].record_count, 1, "{kind}");
            assert!(!reply.segments[0].is_open, "{kind}: listing finalizes");
        }
    }

    #[tokio::test]
    async fn list_handler_rejects_malformed_actor_id() {
        let (_tmp, state) = build_state();
        let auth_actor = [0xAAu8; 32];

        let req = SegmentsListRequest {
            kind: "mail".to_string(),
            actor_id: "not-valid-hex".to_string(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let payload = Bytes::from(bytes.to_vec());

        let h = list_handler();
        let err = h(state, auth_actor, payload).await.unwrap_err();
        assert_eq!(err.code, "fauna.segments.malformed");
    }

    #[tokio::test]
    async fn list_handler_empty_when_no_segments() {
        let (_tmp, state) = build_state();
        let auth_actor = [0xCDu8; 32];

        let req = SegmentsListRequest {
            kind: "mail".to_string(),
            actor_id: hex::encode(auth_actor),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let payload = Bytes::from(bytes.to_vec());

        let h = list_handler();
        let reply_bytes = h(state, auth_actor, payload).await.expect("ok");
        let reply: SegmentsListReply = decode(&reply_bytes).expect("decode");
        assert!(reply.segments.is_empty());
    }

    /// Append two records that share a bucket (one segment) plus one
    /// record in a later bucket (rotates to a second segment); the
    /// list handler returns both segments with the right shape.
    #[tokio::test]
    async fn list_handler_returns_one_segment_per_live_id() {
        let (_tmp, state) = build_state();
        let auth_actor = [0xEEu8; 32];

        // Two appends in 2024-05, one in 2024-06 — rotates after the
        // bucket boundary, producing two live segments.
        for (i, ts) in [
            (1u8, 1_715_000_000_000i64),
            (2u8, 1_715_000_100_000),
            (3u8, 1_718_000_000_000),
        ] {
            // Unique body per record — identical bytes would dedup under
            // content-hash identity.
            crate::segments::mail::append_record(
                &state.mail_segments,
                &state.db,
                &auth_actor,
                &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                    format!("body-{i}").into_bytes(),
                ),
                &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                    b"hint".to_vec(),
                ),
                floor(ts),
            )
            .await
            .expect("append");
        }

        let req = SegmentsListRequest {
            kind: "mail".to_string(),
            actor_id: hex::encode(auth_actor),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let payload = Bytes::from(bytes.to_vec());

        let h = list_handler();
        let state_for_meta = Arc::clone(&state);
        let reply_bytes = h(state, auth_actor, payload).await.expect("ok");
        let reply: SegmentsListReply = decode(&reply_bytes).expect("decode");
        assert_eq!(reply.segments.len(), 2);
        let s0 = &reply.segments[0];
        assert_eq!(s0.segment_id, 1);
        assert_eq!(s0.record_count, 2);
        assert_eq!(s0.tombstone_count, 0);
        assert_eq!(s0.bucket, "2024-05");
        assert!(s0.size_bytes > 0);
        assert!(!s0.is_open);
        assert_eq!(s0.blake3_hex.len(), 64);
        let s1 = &reply.segments[1];
        assert_eq!(s1.segment_id, 2);
        assert_eq!(s1.record_count, 1);

        // The pair's other half is advertised too, and it is the hash of the
        // sidecar actually on disk — the backup corpus anchors both files
        // against this reply (2026-08-29 sidecar widening).
        for s in &reply.segments {
            let on_disk = std::fs::read(
                state_for_meta
                    .mail_segments
                    .segment_meta_path(&auth_actor, s.segment_id),
            )
            .expect("the sidecar exists once listed (finalize-on-read wrote it)");
            assert_eq!(
                s.meta_blake3_hex,
                hex::encode(blake3::hash(&on_disk).as_bytes()),
                "segment {} must advertise its sidecar's hash",
                s.segment_id
            );
        }
    }

    /// The listing carries the manifest's SAVED counter, not the greatest live
    /// id plus one — the two differ exactly when
    /// segments above the live maximum were retired without a successor, and
    /// the backup ledger built from this reply must carry the one that never
    /// decreases, or a device pinning the ledger's generation alarms on an
    /// honest nest.
    #[tokio::test]
    async fn list_handler_carries_the_saved_counter_above_the_live_maximum() {
        let tmp = TempDir::new().expect("tempdir");
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        let mut state = AppState::for_test(db.clone());
        state.post_segments = Arc::new(SegmentManager::new(tmp.path().to_path_buf(), "post"));
        let state = Arc::new(state);

        let author = [0xEDu8; 32];
        crate::segments::post::append_body(&state.post_segments, &state.db, &author, b"a post", 1)
            .await
            .expect("append post body");
        // Live = [1], counter = 2. Push the counter past the live maximum the
        // way a retired top segment does (`adopt_segments` with a floor).
        state
            .post_segments
            .adopt_segments(&author, &[], 7)
            .await
            .expect("raise the counter");

        let req = SegmentsListRequest {
            kind: "post".to_string(),
            actor_id: hex::encode(author),
            extra: BTreeMap::new(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let h = list_handler();
        let reply: SegmentsListReply = decode(&h(state, author, payload).await.unwrap()).unwrap();
        assert_eq!(reply.segments.len(), 1);
        assert_eq!(reply.segments[0].segment_id, 1);
        assert_eq!(
            reply.next_segment_id, 7,
            "the wire carries the saved counter, above max(live) + 1 = 2"
        );
    }

    #[test]
    fn register_list_handler_registers_kind() {
        let mut b = crate::rpc_router::RpcRouter::builder();
        register_list_handler(&mut b);
        let r = b.build();
        let m = r.kind_meta("fauna.segments.list").expect("registered");
        assert!(!m.forbid_replay);
        assert_eq!(m.default_deadline, std::time::Duration::from_secs(5));
    }
}
