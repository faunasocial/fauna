//! The **nest-local source arm** of the cross-location segment-backup
//! coordinator — nest-side segment backup slice 2, step B (design tracked
//! internally; `docs/goal/architecture/message-segment-store.md`
//! § Cross-location backup protocol).
//!
//! The 2026-07-23 redesign moved segment backup of the nest-originated message
//! kinds nest-side: the source nest hosts its own coordinator in-process
//! (`crate::segment_backup::NestBackupCoordinator`) and seals its own segment
//! files under the owner's granted `NestBackupKey`
//! (`crate::backup_handlers` is the grant plane). This module is the
//! [`SegmentSource`] impl that coordinator reads through — straight off the
//! nest's own disk, with no HTTP self-fetch over loopback.
//!
//! It is deliberately thin. Both halves reuse the nest's existing segment
//! surfaces rather than restating them:
//!
//! - listing → [`crate::segments::list_handler::segment_listing`], the same
//!   manifest→[`fauna_protocol::segments::SegmentRef`] mapping (plus the saved
//!   counter) `fauna.segments.list` serves, so a
//!   nest-hosted and a client-driven coordinator can never disagree about what
//!   this nest holds;
//! - bytes → the store's own pair read
//!   ([`crate::segments::list_handler::ServedSegments::read_pair`]):
//!   finalize-on-read, then the `.dat` **and** its `.meta` sidecar read under
//!   the scope lock — the same two files
//!   `GET /api/v1/segments/{kind}/{actor}/{id}` and its `/meta` sibling
//!   (`crate::segments::segment_route`) serve, as one observation. Until
//!   2026-08-29 this arm read the `.dat` alone, so nothing it backed up could
//!   be reopened: the record footers live only in the sidecar
//!   (`message-segment-store.md` § Client-device custodian (pull) → *Restore*).
//!
//! [`push_client`](SegmentSource::push_client) is `None` by the trait default:
//! a nest observes its own segment writes directly and drives its own
//! scheduling, so it neither has nor needs a push subscription to itself.
//!
//! It serves every actor-scoped kind's **two families**: the content segments
//! (`mail`, `post`, `calendar`, `card`) and, for a kind with a placement
//! layer, its journal (`mail-placement` since 2026-09-26,
//! `calendar-placement` / `card-placement` since 2026-09-28 —
//! `segment-backup-protocol.md` § Client-device custodian (pull) → *Restore* →
//! *The placement journal rides the set*). Every other tag is refused rather
//! than silently backed up as empty.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use fauna_sync_engine::segment_backup::{
    BACKED_UP_KINDS, SegmentFamily, SegmentListing, SegmentPair, SegmentSource,
};

use crate::routes::AppState;

/// The kinds whose backup scope **is the owning actor** — the only scope shape
/// this source resolves (the scope hex decodes straight to the actor whose
/// segment area it reads).
///
/// Unlike the runner-side kind list — the single shared [`BACKED_UP_KINDS`] —
/// this one cannot simply track the shared list: `conv` scopes on a channel id,
/// and resolving a `conv` request's channel as an actor would read the wrong
/// area rather than refuse. So a kind is served here only once it is listed
/// here, and the pin below holds the two lists together.
const ACTOR_SCOPED_KINDS: &[&str] = &["mail", "post", "calendar", "card"];

/// Is `tag` a serve tag of one of the actor-scoped kinds' families?
///
/// Wider than [`BACKED_UP_KINDS`] on purpose: serving a kind the sweep does not
/// yet back up costs nothing (a coordinator only asks for what it sweeps), and
/// it is what lets the per-kind pass be driven and proven before a kind joins
/// the list. The direction that loses data — a swept kind this source cannot
/// resolve — is what the pin below forbids.
fn is_served_tag(tag: &str) -> bool {
    ACTOR_SCOPED_KINDS.iter().any(|kind| {
        SegmentFamily::PASS_ORDER
            .iter()
            .any(|family| family.serve_kind(kind) == Some(tag))
    })
}

/// Compile-time pin, the other half of the treatment: adding a kind to
/// the shared list must **fail the build here** rather than leave this source
/// silently refusing a kind the coordinator now sweeps — or, worse, resolving
/// its scope with the wrong shape. Give the new kind its own scope resolution
/// (and a store in `served_segments_for_kind`), then list it in
/// [`ACTOR_SCOPED_KINDS`] or a sibling list with its own arm.
const _: () = assert!(
    all_listed(BACKED_UP_KINDS, ACTOR_SCOPED_KINDS),
    "NestLocalSegmentSource resolves actor-scoped kinds only — a new BACKED_UP_KINDS \
     entry needs a scope-resolution arm here first",
);

/// Is every entry of `kinds` one of `known`? (`const`, for the pin above.)
const fn all_listed(kinds: &[&str], known: &[&str]) -> bool {
    let mut i = 0;
    while i < kinds.len() {
        let mut j = 0;
        let mut found = false;
        while j < known.len() {
            if const_str_eq(kinds[i], known[j]) {
                found = true;
            }
            j += 1;
        }
        if !found {
            return false;
        }
        i += 1;
    }
    true
}

/// `str` equality usable in a `const` assertion (`==` on `&str` is not `const`).
const fn const_str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// A [`SegmentSource`] over this nest's own on-disk segment files.
pub struct NestLocalSegmentSource {
    state: Arc<AppState>,
    source_id: String,
}

impl NestLocalSegmentSource {
    /// `source_id` is the stable key this source takes in the coordinator's
    /// `sync_db` segment-state rows — the source nest's own identity hex, so
    /// state recorded under it survives a restart and never collides with the
    /// client-driven arm's rows for the same nest.
    pub fn new(state: Arc<AppState>, source_id: impl Into<String>) -> Self {
        Self {
            state,
            source_id: source_id.into(),
        }
    }

    /// Resolve a `(kind, scope_hex)` pair to the owning actor.
    ///
    /// For every kind this source serves the backup scope *is* the actor
    /// ([`ACTOR_SCOPED_KINDS`]; conv scopes on channel_id and is not served
    /// here). Refusing an unknown
    /// kind here — rather than returning an empty list — keeps a
    /// mis-constructed coordinator loud instead of silently backing up nothing.
    fn scope_actor(kind: &str, scope_hex: &str) -> Result<[u8; 32]> {
        if !is_served_tag(kind) {
            bail!("nest-local segment source: unsupported kind {kind:?}");
        }
        fauna_core::hex32::decode(scope_hex)
            .map_err(|e| anyhow::anyhow!("nest-local segment source: bad scope {scope_hex:?}: {e}"))
    }
}

#[async_trait]
impl SegmentSource for NestLocalSegmentSource {
    fn source_id(&self) -> &str {
        &self.source_id
    }

    async fn list_segments(&self, kind: &str, scope_hex: &str) -> Result<SegmentListing> {
        let actor = Self::scope_actor(kind, scope_hex)?;
        crate::segments::list_handler::segment_listing(&self.state, kind, &actor)
            .await
            .map(SegmentListing::from)
            .with_context(|| format!("list nest-local segments kind={kind} scope={scope_hex}"))
    }

    async fn segment_pair(
        &self,
        kind: &str,
        scope_hex: &str,
        segment_id: u32,
    ) -> Result<SegmentPair> {
        let actor = Self::scope_actor(kind, scope_hex)?;
        // Finalize-on-read, exactly as the HTTP route does before serving —
        // and both files read under the scope lock, so the pair describes the
        // same records and any further append rotates a *new* segment rather
        // than growing this one under the coordinator's feet.
        let served = crate::segments::list_handler::served_segments_for_kind(&self.state, kind)
            .with_context(|| format!("nest-local segment source: no store serves {kind:?}"))?;
        let pair = served
            .read_pair(&actor, segment_id)
            .await
            .with_context(|| {
                format!(
                    "read nest-local segment pair kind={kind} scope={scope_hex} seg={segment_id}"
                )
            })?;
        Ok(SegmentPair {
            dat: pair.dat,
            meta: pair.meta,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segments::test_helpers::{build_state, floor};
    use bytes::Bytes;
    use fauna_protocol::segments::{SegmentRef, SegmentsListReply, SegmentsListRequest};
    use fauna_protocol::{decode_strict as decode, encode_canonical};
    use std::collections::BTreeMap;

    /// Append `n` mail records for `actor`, spread far enough apart in time to
    /// cross a bucket boundary after the second, so the actor ends up with more
    /// than one live segment.
    async fn append_records(state: &AppState, actor: &[u8; 32], stamps: &[i64]) {
        for (i, ts) in stamps.iter().enumerate() {
            // Bodies must be unique per record: identity is the content hash,
            // so identical bytes would dedup into one record.
            crate::segments::mail::append_record(
                &state.mail_segments,
                &state.db,
                actor,
                &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                    format!("sealed-body-bytes-{}-{i}", actor[0]).into_bytes(),
                ),
                &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                    b"sealed-index-hint".to_vec(),
                ),
                floor(*ts),
            )
            .await
            .expect("append");
        }
    }

    /// What the **real registered** `fauna.segments.list` handler answers for
    /// `actor` — the wire surface a client-driven coordinator reads.
    async fn segments_over_the_wire(state: Arc<AppState>, actor: [u8; 32]) -> Vec<SegmentRef> {
        kind_over_the_wire(state, "mail", actor).await
    }

    /// [`segments_over_the_wire`] for any tag the plane serves.
    async fn kind_over_the_wire(
        state: Arc<AppState>,
        kind: &str,
        actor: [u8; 32],
    ) -> Vec<SegmentRef> {
        let router = {
            let mut b = crate::rpc_router::RpcRouter::builder();
            crate::segments::list_handler::register_list_handler(&mut b);
            b.build()
        };
        let meta = router
            .kind_meta("fauna.segments.list")
            .expect("fauna.segments.list registered");
        let req = SegmentsListRequest {
            kind: kind.to_string(),
            actor_id: hex::encode(actor),
            extra: BTreeMap::new(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = (meta.handler)(state, actor, payload)
            .await
            .expect("list ok");
        let reply: SegmentsListReply = decode(&reply_bytes).expect("decode");
        reply.segments
    }

    /// The load-bearing agreement: what the nest-hosted coordinator sees
    /// through this source is exactly what a client-driven one sees over
    /// `fauna.segments.list`. Drift here would make the coordinator diff a
    /// segment list against state recorded from a different one.
    #[tokio::test]
    async fn list_segments_matches_the_wire_handler() {
        let (_tmp, state) = build_state();
        let actor = [0xEEu8; 32];
        // two in 2024-05 (one segment), one in 2024-06 (rotates a second)
        append_records(
            &state,
            &actor,
            &[1_715_000_000_000, 1_715_000_100_000, 1_718_000_000_000],
        )
        .await;

        let src = NestLocalSegmentSource::new(state.clone(), "nest-under-test");
        let mine = src
            .list_segments("mail", &hex::encode(actor))
            .await
            .expect("list_segments ok")
            .segments;
        let wire = segments_over_the_wire(state.clone(), actor).await;

        assert_eq!(mine.len(), 2, "two live segments after the bucket rotation");
        assert_eq!(
            mine, wire,
            "the nest-local source and fauna.segments.list must answer identically"
        );
    }

    /// `segment_pair` serves both files verbatim — the coordinator treats
    /// these bytes as opaque and re-seals them, so a single altered or
    /// truncated byte in either half would corrupt the backup silently — and
    /// the pair it serves is the one the same source advertises, hash by hash.
    #[tokio::test]
    async fn segment_pair_is_byte_identical_to_both_files_on_disk() {
        let (_tmp, state) = build_state();
        let actor = [0xA1u8; 32];
        append_records(&state, &actor, &[1_715_000_000_000]).await;

        let src = NestLocalSegmentSource::new(state.clone(), "nest-under-test");
        let refs = src
            .list_segments("mail", &hex::encode(actor))
            .await
            .expect("list_segments ok")
            .segments;
        let seg_id = refs.first().expect("one segment").segment_id;

        let served = src
            .segment_pair("mail", &hex::encode(actor), seg_id)
            .await
            .expect("segment_pair ok");

        let dat_on_disk = std::fs::read(state.mail_segments.segment_file_path(&actor, seg_id))
            .expect("read segment file");
        let meta_on_disk = std::fs::read(state.mail_segments.segment_meta_path(&actor, seg_id))
            .expect("read sidecar");
        assert_eq!(
            served.dat, dat_on_disk,
            "served .dat must be the file verbatim"
        );
        assert_eq!(
            served.meta, meta_on_disk,
            "served .meta must be the sidecar verbatim"
        );
        assert!(
            !served.meta.is_empty(),
            "a finalized segment always has a sidecar"
        );
        assert_eq!(
            served.dat.len() as u64,
            refs[0].size_bytes,
            "and must match the size the same source advertised"
        );
        served
            .verify_against(&refs[0])
            .expect("both halves hash to what the same source advertised");
    }

    /// `segment_pair` finalizes on its own, without a preceding
    /// `list_segments`.
    ///
    /// A still-open segment's file on disk is a payload prefix with no
    /// footer/trailer yet, and **no sidecar at all** (`SegmentStore::finalize_open`
    /// → `Segment::finalize` writes both). So a `segment_pair` that skipped the
    /// finalize would serve that short prefix and fail on the sidecar, while
    /// the *next* list — which does finalize — would advertise the full size
    /// and a different `blake3`: the coordinator would record custody for
    /// content whose bytes it never uploaded. Asserting against the file on
    /// disk cannot catch this (both sides are the same short file at that
    /// instant); asserting against the later advertisement can.
    #[tokio::test]
    async fn segment_pair_finalizes_without_a_preceding_list() {
        let (_tmp, state) = build_state();
        let actor = [0xF1u8; 32];
        append_records(&state, &actor, &[1_715_000_000_000]).await;

        let src = NestLocalSegmentSource::new(state.clone(), "nest-under-test");
        // Deliberately no list_segments() call before this one.
        let served = src
            .segment_pair("mail", &hex::encode(actor), 1)
            .await
            .expect("segment_pair must finalize the open segment itself");
        assert!(
            !served.dat.is_empty(),
            "an open segment must still serve bytes"
        );
        assert!(
            !served.meta.is_empty(),
            "…and its sidecar, which finalize writes"
        );

        let advertised = src
            .list_segments("mail", &hex::encode(actor))
            .await
            .expect("list ok")
            .segments;
        let seg = advertised
            .iter()
            .find(|s| s.segment_id == 1)
            .expect("seg 1");
        assert_eq!(
            served.dat.len() as u64,
            seg.size_bytes,
            "bytes served before any list must be the finalized segment, \
             not an unfinalized prefix"
        );
        served
            .verify_against(seg)
            .expect("both halves must hash to what the source later advertises for them");
    }

    /// The scope selects the actor: one actor's backup must never carry
    /// another's segments.
    #[tokio::test]
    async fn scope_hex_selects_the_actor() {
        let (_tmp, state) = build_state();
        let mine = [0xB1u8; 32];
        let theirs = [0xB2u8; 32];
        append_records(&state, &mine, &[1_715_000_000_000]).await;

        let src = NestLocalSegmentSource::new(state.clone(), "nest-under-test");
        assert_eq!(
            src.list_segments("mail", &hex::encode(mine))
                .await
                .expect("ok")
                .segments
                .len(),
            1
        );
        assert!(
            src.list_segments("mail", &hex::encode(theirs))
                .await
                .expect("ok")
                .segments
                .is_empty(),
            "an actor with no appends has no segments"
        );
    }

    /// **The backed-up kind's other family.** The placement journal is listed
    /// and read through this same source under its own tag, its pair served
    /// verbatim and anchored by the hashes the same listing advertised —
    /// exactly the contract the content family keeps, because the coordinator
    /// moves both families with one code path and diffs neither against the
    /// other.
    #[tokio::test]
    async fn the_placement_journal_is_served_as_the_kinds_other_family() {
        use fauna_mail::segments::placement::MailPlacementRecord;

        let (_tmp, state) = build_state();
        let actor = [0xB7u8; 32];
        append_records(&state, &actor, &[1_715_000_000_000]).await;
        for (i, mailbox) in ["INBOX", "Archive"].iter().enumerate() {
            state
                .mail_placement
                .append_event(
                    &actor,
                    &MailPlacementRecord::Create {
                        mailbox: (*mailbox).to_string(),
                        uid_validity: 1 + i as u32,
                        attrs: Vec::new(),
                    },
                )
                .await
                .expect("journal a mailbox create");
        }

        let src = NestLocalSegmentSource::new(state.clone(), "nest-under-test");
        let scope = hex::encode(actor);
        let journal = src
            .list_segments("mail-placement", &scope)
            .await
            .expect("the journal lists under its own tag")
            .segments;
        assert_eq!(
            journal.len(),
            1,
            "both creates landed in one journal segment"
        );
        assert_eq!(journal[0].record_count, 2);
        assert_eq!(
            journal[0].tombstone_count, 0,
            "a journal has no record mirror, so it never counts a tombstone"
        );
        assert_eq!(
            journal,
            kind_over_the_wire(state.clone(), "mail-placement", actor).await,
            "the nest-local source and fauna.segments.list answer identically"
        );

        let seg_id = journal[0].segment_id;
        let served = src
            .segment_pair("mail-placement", &scope, seg_id)
            .await
            .expect("the journal's pair reads");
        assert_eq!(
            served.dat,
            std::fs::read(state.mail_placement.segment_file_path(&actor, seg_id))
                .expect("read the journal segment"),
            "served .dat is the journal file verbatim"
        );
        assert_eq!(
            served.meta,
            std::fs::read(state.mail_placement.segment_meta_path(&actor, seg_id))
                .expect("read the journal sidecar"),
            "served .meta is the journal sidecar verbatim"
        );
        served
            .verify_against(&journal[0])
            .expect("both halves hash to what the same source advertised");

        // The two families are two listings, never one: the content family
        // still answers for the content alone.
        let content = src
            .list_segments("mail", &scope)
            .await
            .expect("content lists")
            .segments;
        assert_eq!(content.len(), 1);
        assert_eq!(content[0].record_count, 1);
        assert_ne!(
            content[0].blake3_hex, journal[0].blake3_hex,
            "same segment id, different family, different file"
        );
    }

    /// An unknown kind is refused loudly on both halves — never answered as
    /// "this actor has nothing to back up".
    #[tokio::test]
    async fn unsupported_kind_is_refused_not_empty() {
        let (_tmp, state) = build_state();
        let actor = [0xC1u8; 32];
        let src = NestLocalSegmentSource::new(state.clone(), "nest-under-test");

        assert!(
            src.list_segments("conv", &hex::encode(actor))
                .await
                .is_err(),
            "listing an unwired kind must error"
        );
        assert!(
            src.segment_pair("conv", &hex::encode(actor), 1)
                .await
                .is_err(),
            "fetching an unwired kind must error"
        );
        // A tag no backed-up kind's family derives is unwired too: the serve
        // plane knowing a shape does not make this source serve it.
        assert!(
            src.list_segments("post-placement", &hex::encode(actor))
                .await
                .is_err(),
            "a kind with no placement layer has no journal to serve"
        );
    }

    /// A malformed scope is an error, not a panic — the coordinator's tuple
    /// list is data, and a bad row must not take the nest down.
    #[tokio::test]
    async fn malformed_scope_is_an_error() {
        let (_tmp, state) = build_state();
        let src = NestLocalSegmentSource::new(state.clone(), "nest-under-test");
        assert!(src.list_segments("mail", "not-hex").await.is_err());
    }

    /// A segment id the actor does not have is an error, not empty bytes —
    /// uploading an empty "segment" would record custody for content that
    /// isn't there.
    #[tokio::test]
    async fn missing_segment_is_an_error_not_empty_bytes() {
        let (_tmp, state) = build_state();
        let actor = [0xD1u8; 32];
        append_records(&state, &actor, &[1_715_000_000_000]).await;
        let src = NestLocalSegmentSource::new(state.clone(), "nest-under-test");
        assert!(
            src.segment_pair("mail", &hex::encode(actor), 9999)
                .await
                .is_err()
        );
    }

    /// An in-process source has no push channel — `run_forever` must fall back
    /// to its periodic timer rather than wait on a subscription to itself.
    #[test]
    fn has_no_push_client() {
        let (_tmp, state) = build_state();
        let src = NestLocalSegmentSource::new(state, "nest-under-test");
        assert!(src.push_client().is_none());
        assert_eq!(src.source_id(), "nest-under-test");
    }
}
