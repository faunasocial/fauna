//! The production [`SegmentRail`]: sealed bytes → the `__index` rail.
//!
//! Two calls, in an order the nest enforces
//! (`docs/goal/behavior/content-index.md` § Ingest triggers, v1):
//!
//! 1. **`PUT /api/v1/blob/{cid}`** — the sealed segment's bytes, addressed by
//!    their own CID under the raw codec. Bulk binary rides HTTP because WS-RPC
//!    frames cap at 2 MiB (`fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE`).
//! 2. **`fauna.index.record`** — the reference `(path, blob hash, size)`, which
//!    the nest turns into the `sync_changes` row that replicates the blob to the
//!    user's other locations.
//!
//! The nest *verifies it holds the blob* before writing the row and refuses
//! otherwise, so the ordering is not a convention this crate could get away with
//! inverting — a journal row can never point at a blob that does not exist.
//!
//! ## What this is NOT
//!
//! It is not a sync-engine consumer. An earlier design published over
//! `SyncEngine::publish_reserved_bytes` / `fauna.sync.changes.record`; that is
//! deleted, because the nest refuses a reserved (`__`) set on that kind unless
//! it is a custody-copy destination, and because the GC classifies a
//! reserved-only reference as a **direct blob** it never walks for chunks — a
//! chunked manifest recorded there would have its live chunks swept. Both are
//! pinned by `bins/fauna-nest/tests/conformance_folders.rs`. The consequence
//! that shapes this file: **one segment is one blob**, so the builder bounds its
//! segment size (`index_builder::MAX_SEGMENT_BYTES`) rather than spilling to the
//! chunk route, and this publisher is a plain `Send + Sync` struct — no engine,
//! no worker thread, no `LocalSet`.

use std::sync::Arc;

use fauna_cbor::Cid;
use fauna_client::NestClient;
use fauna_index::{
    ClassKey, ContentId, ContentKind, Index, IndexError, IndexManifest, IndexMasterKey,
    IndexSegmentKey, KindClass, QueryHit, TimeRange, mailcal_manifest_path, segment_path,
};
use fauna_nest_http::{NestContentApi, paths};
use fauna_protocol::content_index::{
    KIND_LIST, KIND_RECORD, ListIndexBlobsReply, ListIndexBlobsRequest, RecordIndexBlobReply,
    RecordIndexBlobRequest,
};

use crate::index_builder::{IndexBuildError, IndexBuilder, RailEntry, SegmentRail};

/// Publishes sealed index segments to the actor's nest over the `__index` rail.
///
/// One per logged-in actor, shared by every kind's builder — the rail is
/// kind-agnostic (it moves opaque bytes at a virtual path), so the mail builder
/// and the master-key builder rollout slice S4 adds hand it the same instance.
///
/// The nest on the other end can never read what crosses this seam: the bytes
/// are AEAD-sealed under a key no nest holds (`content-index.md` § Encryption
/// posture).
pub struct IndexRailPublisher {
    nest: Arc<NestClient>,
}

impl IndexRailPublisher {
    pub fn new(nest: Arc<NestClient>) -> Self {
        Self { nest }
    }

    /// The blob half. Returns the hex blake3 hash the record half then cites —
    /// the same encoding the nest's handler decodes with
    /// (`fauna_core::hex32`), so the two halves can never disagree about what
    /// a hash *string* is.
    async fn put_bytes(&self, bytes: &[u8]) -> Result<String, String> {
        let content = self.nest.auth().content_api();
        // Raw codec (0x55), not dag-cbor: a sealed segment is opaque bytes with
        // no IPLD structure to link into. The nest re-derives the digest from
        // the body and refuses a mismatch, so a corrupted upload fails here
        // rather than becoming an unreadable row.
        let cid = Cid::of_raw(bytes);
        content
            .put_bytes(
                &paths::blob::by_cid(&cid.to_base32()),
                "application/octet-stream",
                bytes.to_vec(),
            )
            .await
            .map_err(|e| format!("blob put ({} bytes): {e}", bytes.len()))?;
        Ok(fauna_core::hex32::encode(&cid.digest()))
    }

    /// Enumerate the actor's live `__index` blobs — the read half a replica
    /// refresh walks (rollout S3 piece 3) and the direct proof that a publish
    /// landed. Pure read, self-scoped: the nest derives the actor from the
    /// authenticated connection, so this can only ever see the caller's own.
    ///
    /// **Walks the nest's pages to exhaustion and returns the COMPLETE set**, so
    /// every caller keeps the whole-rail view it was written against. The nest
    /// cuts `fauna.index.list` at the frame budget (`content_index::
    /// ListIndexBlobsRequest`), which it must: this rail's segment-path count is
    /// deliberately unbounded, so a large enough index once produced a reply too
    /// big to send and could never be refreshed again.
    ///
    /// Walk to the cursor's ABSENCE, never to the first empty page — a page can
    /// come back empty with a cursor still set (the bridge plane filters
    /// master-class rows out after the page is read). When the reply
    /// carries no cursor (a drained first page), this is exactly one request.
    pub async fn list(&self) -> Result<ListIndexBlobsReply, IndexBuildError> {
        let fail = |e: String| IndexBuildError::Publish {
            path: "__index".into(),
            reason: format!("{KIND_LIST}: {e}"),
        };
        let mut cursor: Option<String> = None;
        let mut all = ListIndexBlobsReply::default();
        loop {
            let sent = cursor.clone();
            let page: ListIndexBlobsReply = self
                .nest
                .request(
                    KIND_LIST,
                    ListIndexBlobsRequest {
                        cursor: sent.clone(),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| fail(e.to_string()))?;
            all.entries.extend(page.entries);
            match page.next_cursor {
                // The nest mints a cursor only when it believes more remain, so
                // this terminates against any honest nest. The guard is on the
                // cursor VALUE rather than on the page being empty (an empty
                // page is legitimate) — it is what stops a nest that echoed
                // back the cursor it was handed from spinning this loop
                // forever.
                Some(next) if Some(&next) != sent.as_ref() => cursor = Some(next),
                _ => break,
            }
        }
        Ok(all)
    }

    /// Fetch one published blob's bytes back by the hex hash
    /// [`list`](Self::list) handed out.
    ///
    /// The bytes arrive exactly as they were sealed — the nest stores them
    /// opaquely — so the caller opens them with its own key. Public because the
    /// local query handle reads segments through this same route.
    pub async fn fetch_blob(&self, blob_hash: &str) -> Result<Vec<u8>, IndexBuildError> {
        let content = self.nest.auth().content_api();
        content
            .get(&paths::blob::by_hash(blob_hash))
            .await
            .map(|b| b.to_vec())
            .map_err(|e| IndexBuildError::Publish {
                path: format!("blob/{blob_hash}"),
                reason: format!("blob get: {e}"),
            })
    }
}

impl IndexRailPublisher {
    /// The blob hash of this actor's `manifest-mailcal.idx`, or `None` if they
    /// have published nothing yet.
    ///
    /// The cheap staleness probe behind the local search arm's reader cache
    /// (`local_search::MailLocalSearch::reader`): one `list()`, no blob fetch,
    /// no unseal. The manifest names every live segment, so its hash moves on
    /// exactly the events that change what a query can find.
    pub async fn mailcal_manifest_hash(&self) -> Result<Option<String>, IndexBuildError> {
        Ok(self
            .list()
            .await?
            .entries
            .into_iter()
            .find(|e| e.path == mailcal_manifest_path())
            .map(|e| e.blob_hash))
    }

    /// The published **master-class** manifest's blob hash, or `None` when this
    /// actor has published no master-class content. The staleness key a
    /// long-lived master arm re-opens on, mirroring
    /// [`Self::mailcal_manifest_hash`].
    pub async fn master_manifest_hash(&self) -> Result<Option<String>, IndexBuildError> {
        Ok(self
            .list()
            .await?
            .entries
            .into_iter()
            .find(|e| e.path == fauna_index::manifest_path())
            .map(|e| e.blob_hash))
    }
}

/// Every key that may **open** this actor's mail/calendar slice: the current
/// generation's MSEK-derived key first, then one per prior grace generation the
/// MLS snapshot carries.
///
/// This is the reader half of the grace list shipped 2026-08-03
/// (`key-material-hierarchy.md` § Path B-sibling-4 → *Snapshot carriage*; the
/// field is `MlsSnapshotPlaintext::index_seg_grace_keys`, newest first). Without
/// it, an MSEK rotation makes every segment sealed before the rotation
/// permanently unopenable to its own owner until the rewrap pass converges —
/// silent data invisibility, not an error anyone would see.
///
/// **Reads try the ring; writes use [`current`](Self::current) and nothing
/// else.** Sealing under a grace key would extend a superseded generation's
/// reach forward, which is exactly what rotation exists to end. The two legs
/// build a ring the same way — a client from its own `fauna.state.mail` MSEK,
/// the MDA from the session MSEK — so a segment either leg writes is one the
/// other can open (`content-index.md` § Where the index is built).
pub struct MailcalKeyRing {
    /// Newest first. Index 0 is the current generation's key and is the only
    /// entry any writer ever touches; the rest are grace generations, in the
    /// order the snapshot carries them.
    keys: Vec<IndexSegmentKey>,
}

impl MailcalKeyRing {
    /// The current generation alone — the ring a caller with no snapshot in
    /// hand builds. Correct whenever every segment on the rail was sealed
    /// under the live MSEK, which is every actor that has not rotated.
    pub fn from_msek(msek: &[u8; 32]) -> Self {
        Self {
            keys: vec![IndexSegmentKey::from_bytes(
                *fauna_mls::wrapped_blob::derive_index_segment_key(msek),
            )],
        }
    }

    /// The current generation, then each grace generation the snapshot carries.
    ///
    /// `snapshot_plaintext_bytes` is a canonical DAG-CBOR `MlsSnapshotPlaintext`
    /// the caller has already AEAD-unwrapped under this same MSEK — the same
    /// contract as `MailRecordOpener::new_with_msek`, whose `[current] ++
    /// [grace…]` shape this deliberately mirrors so the two MSEK-derived
    /// openers stay one pattern.
    ///
    /// A snapshot with no grace entries yields exactly [`from_msek`](Self::from_msek).
    pub fn from_msek_and_snapshot(
        msek: &[u8; 32],
        snapshot_plaintext_bytes: &[u8],
    ) -> Result<Self, IndexBuildError> {
        let snapshot = fauna_mls::wrapped_blob::MlsSnapshotPlaintext::from_canonical_bytes(
            snapshot_plaintext_bytes,
        )
        .map_err(|e| IndexBuildError::Publish {
            path: "mls-snapshot".into(),
            reason: format!("decode mls-snapshot plaintext: {e}"),
        })?;
        let mut ring = Self::from_msek(msek);
        for grace in &snapshot.index_seg_grace_keys {
            // `from_canonical_bytes` already enforced the 32-byte length; a
            // shorter entry cannot reach here, and skipping rather than failing
            // keeps one malformed grace entry from making the *current* key
            // unusable too.
            let Ok(bytes) = <[u8; 32]>::try_from(grace.as_slice()) else {
                continue;
            };
            ring.keys.push(IndexSegmentKey::from_bytes(bytes));
        }
        Ok(ring)
    }

    /// The current generation, then one grace generation per prior MSEK — the
    /// ring a client builds from its own mail custody (`MailConfig.prior_mseks`,
    /// newest first), with no snapshot in hand.
    ///
    /// It opens exactly what [`from_msek_and_snapshot`](Self::from_msek_and_snapshot)
    /// opens: each snapshot grace entry is this same derivation of a prior
    /// MSEK. The caller caps `prior_mseks` to the grace window, as it does for
    /// the standing keys and epoch roots it derives beside this ring.
    pub fn from_msek_and_priors(msek: &[u8; 32], prior_mseks: &[[u8; 32]]) -> Self {
        let mut ring = Self::from_msek(msek);
        ring.keys.extend(prior_mseks.iter().map(|prior| {
            IndexSegmentKey::from_bytes(*fauna_mls::wrapped_blob::derive_index_segment_key(prior))
        }));
        ring
    }

    /// The key every writer seals under (see the type docs).
    pub fn current(&self) -> &IndexSegmentKey {
        self.keys
            .first()
            .expect("a ring always holds the current key")
    }

    /// How many generations this ring can open — 1 plus the grace count.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Always false: a ring always carries at least the current key. Present
    /// because clippy asks for it beside [`len`](Self::len).
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Open a sealed mail/calendar segment under the newest key that works.
    ///
    /// A wrong key fails at the AEAD unwrap of the header's wrapped data key,
    /// so "try the next one" is a cheap, constant-size operation — it never
    /// touches the segment body.
    pub fn open_segment(&self, sealed: &[u8]) -> Result<Vec<u8>, IndexError> {
        self.try_each(
            |key| fauna_index::open_segment_bytes_mailcal(sealed, key),
            "segment",
        )
    }

    /// [`open_segment`](Self::open_segment)'s manifest twin.
    pub fn open_manifest(&self, sealed: &[u8]) -> Result<IndexManifest, IndexError> {
        self.try_each(
            |key| IndexManifest::from_sealed_bytes_mailcal(sealed, key),
            "manifest-mailcal.idx",
        )
    }

    /// Walk the ring newest-first, returning the first success.
    ///
    /// On exhaustion it reports the **current** key's error, because that is the
    /// diagnosis a caller acts on ("this blob is not yours / is corrupt"), and
    /// it names the ring width so a rotation-grace miss reads as one — a blob
    /// older than the grace window is a rewrap-pass gap, not corruption.
    fn try_each<T>(
        &self,
        mut open: impl FnMut(&IndexSegmentKey) -> Result<T, IndexError>,
        what: &str,
    ) -> Result<T, IndexError> {
        let mut first_err = None;
        for key in &self.keys {
            match open(key) {
                Ok(v) => return Ok(v),
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        Err(match first_err {
            Some(IndexError::Crypto(msg)) => IndexError::Crypto(format!(
                "{what}: no key in the ring opens it ({} tried: current + {} grace): {msg}",
                self.keys.len(),
                self.keys.len() - 1
            )),
            // A non-crypto failure (schema, version floor) is the same under
            // every key — surface it verbatim rather than dressing it as a
            // key-ring miss.
            Some(other) => other,
            None => IndexError::Crypto(format!("{what}: empty key ring")),
        })
    }
}

/// The mail/calendar slice, opened locally and ready to answer queries.
///
/// This is the seam the shared Search snapshot's **local arm** calls (rollout
/// S3 piece 5, `ui/search.md` § State & data shape). It holds a decrypted
/// in-memory index — which is the correct at-rest posture on a client, where
/// the user *is* the box owner and the OS disk encryption is the protection
/// (`content-index.md` § Encryption posture); sealing exists to keep an
/// untrusted box owner out, and there is none here.
pub struct MailIndexReader {
    index: Index,
    /// The blob hash of the `manifest-mailcal.idx` this reader was opened at, or
    /// `None` when the actor has published nothing yet.
    ///
    /// The manifest is the authority on what the index *contains*, so its hash
    /// changing is exactly the condition under which a held-open reader has gone
    /// stale — which is what lets a long-lived local-search arm decide a refresh
    /// with one cheap `list()` instead of refetching every segment per query
    /// (`local_search::MailLocalSearch`).
    manifest_hash: Option<String>,
}

/// The kinds a mail/calendar index-segment key can open — the ceiling every
/// query against this reader is intersected with.
pub const MAILCAL_KINDS: &[ContentKind] = &[ContentKind::Mail, ContentKind::Calendar];

impl MailIndexReader {
    /// The manifest hash this reader was opened at (see the field).
    pub fn manifest_hash(&self) -> Option<&str> {
        self.manifest_hash.as_deref()
    }

    /// Search the opened segments, over the caller's kinds **intersected** with
    /// [`MAILCAL_KINDS`].
    ///
    /// The intersection only ever narrows: a mail/calendar key opens nothing
    /// else, so a caller cannot widen the answer by asking for more, and asking
    /// for a kind this key class does not cover stays a question with a
    /// guaranteed empty answer that never reaches tantivy. Narrowing is what the
    /// Search page's type filter needs — a filter set to mail must not return
    /// calendar rows. The master-key kinds arrive with rollout S4, through their
    /// own reader over their own manifest.
    pub fn query(
        &self,
        query: &str,
        kinds: &[ContentKind],
        range: Option<TimeRange>,
        limit: usize,
    ) -> Result<Vec<QueryHit>, IndexBuildError> {
        let selected: Vec<ContentKind> = MAILCAL_KINDS
            .iter()
            .copied()
            .filter(|k| kinds.contains(k))
            .collect();
        if selected.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self.index.query(query, &selected, range, limit)?)
    }

    /// Search the opened segments requiring **every** token, over the caller's
    /// kinds intersected with [`MAILCAL_KINDS`] exactly as [`Self::query`] does.
    ///
    /// The AND entry point ([`Index::query_all_of`]) rather than the parser one,
    /// for the IMAP `SEARCH` body axis — see that method for why the two cannot
    /// be the same call. `limit: None` returns every match, which is what a
    /// `SEARCH` reply requires.
    pub fn query_all_of(
        &self,
        tokens: &[String],
        kinds: &[ContentKind],
        range: Option<TimeRange>,
        limit: Option<usize>,
    ) -> Result<Vec<QueryHit>, IndexBuildError> {
        let selected: Vec<ContentKind> = MAILCAL_KINDS
            .iter()
            .copied()
            .filter(|k| kinds.contains(k))
            .collect();
        if selected.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self.index.query_all_of(tokens, &selected, range, limit)?)
    }

    /// Every `content_id` this reader can actually answer for.
    ///
    /// The **queryable** set, deliberately — not a builder's re-index guard,
    /// which also holds ids staged but not yet flushed. A caller deciding
    /// "can the index answer for this message, or must I fall back?" must ask
    /// this one: the MDA's flush is best-effort, so an id can sit in the guard
    /// while its bytes never reached the rail, and trusting the guard there
    /// would drop the message from `SEARCH` results with no error anywhere.
    pub fn content_ids(&self) -> Result<Vec<ContentId>, IndexBuildError> {
        Ok(self.index.content_ids()?)
    }

    /// [`Self::content_ids`] plus each doc's stored secondary identity —
    /// the coverage walk for a caller whose candidates are nest message ids
    /// (the MDA's `SEARCH` path; `content-index.md` § Where the index is
    /// built → the 2026-08-10 carrier ruling).
    pub fn doc_identities(&self) -> Result<Vec<fauna_index::DocIdentity>, IndexBuildError> {
        Ok(self.index.doc_identities()?)
    }
}

/// Open this actor's mail/calendar index off the synced `__index` replica.
///
/// The read counterpart of the build path: list once, fetch every **live**
/// segment the manifest names, unseal each with the MSEK-derived key, and open
/// them as one queryable index. Tombstoned segments are skipped — that is what
/// makes compaction's tombstone-only rule (§ Where the index is built) show up
/// in query results without any blob ever being deleted.
///
/// An actor with nothing published yields an empty reader that answers every
/// query with no hits, rather than an error: "no index yet" is a normal state
/// on a fresh device, and the Search page renders it as *no local results*, not
/// as a failure.
pub async fn open_mail_reader(
    ring: &MailcalKeyRing,
    rail: &dyn SegmentRail,
) -> Result<MailIndexReader, IndexBuildError> {
    let opened = open_mailcal(ring, rail).await?;
    Ok(MailIndexReader {
        index: opened.index,
        manifest_hash: opened.manifest_hash,
    })
}

/// A reader over this actor's **master-class** slice — the query-side twin of
/// [`resume_master_builder`], and the counterpart of [`MailIndexReader`] for the
/// other key class.
///
/// Deliberately a separate type rather than a class parameter on
/// [`MailIndexReader`]: the two classes are a **security** boundary, not a
/// configuration axis (`content-index.md` § Where the index is built → *the rail
/// has two planes*). A mail/calendar reader is built from an MSEK-derived key
/// that a leaked MUA credential can reach; this one is built from the
/// seed-derived master key that it cannot. Keeping them distinct types means no
/// call site can widen one into the other by passing a different enum value.
pub struct MasterIndexReader {
    index: Index,
    /// The blob hash of the `manifest.idx` this reader was opened at, or `None`
    /// when the actor has published no master-class content yet. Same staleness
    /// key, for the same reason, as [`MailIndexReader::manifest_hash`].
    manifest_hash: Option<String>,
}

impl MasterIndexReader {
    /// The manifest hash this reader was opened at (see the field).
    pub fn manifest_hash(&self) -> Option<&str> {
        self.manifest_hash.as_deref()
    }

    /// Search the opened segments, over the caller's kinds **intersected** with
    /// the master class's own kinds.
    ///
    /// The intersection narrows and never widens, exactly as
    /// [`MailIndexReader::query`] documents — asking this reader for mail is a
    /// question with a guaranteed empty answer, and it never reaches tantivy.
    /// The ceiling comes from [`KindClass::kinds`] rather than a hand-listed
    /// const so that a future kind cannot be silently unsearchable here: it
    /// picks its class in one exhaustive match and appears automatically.
    pub fn query(
        &self,
        query: &str,
        kinds: &[ContentKind],
        range: Option<TimeRange>,
        limit: usize,
    ) -> Result<Vec<QueryHit>, IndexBuildError> {
        let selected: Vec<ContentKind> = KindClass::Master
            .kinds()
            .into_iter()
            .filter(|k| kinds.contains(k))
            .collect();
        if selected.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self.index.query(query, &selected, range, limit)?)
    }
}

/// Open this actor's master-class index off the synced `__index` replica.
///
/// The read counterpart of [`resume_master_builder`], and the piece whose
/// absence made every master-class kind build correctly and stay unsearchable:
/// the manager holds exactly one local arm, app glue filled it with the
/// mail/calendar reader, and that reader answers `None` for every master kind.
/// See `content-index.md` § Ingest triggers, v1 → *Registration happens at BOTH
/// ends of the pipeline*.
///
/// Like the mail reader: an actor with nothing published yields an empty reader
/// that answers every query with no hits, which is a normal state on a fresh
/// device and never an error.
pub async fn open_master_reader(
    key: IndexMasterKey,
    rail: &dyn SegmentRail,
) -> Result<MasterIndexReader, IndexBuildError> {
    let opened = open_class(&ClassKey::Master(key), KindClass::Master, rail).await?;
    Ok(MasterIndexReader {
        index: opened.index,
        manifest_hash: opened.manifest_hash,
    })
}

/// The one rail read both [`open_mail_reader`] and [`resume_mail_builder`] are
/// built on: list once, fetch the mail/calendar manifest, then fetch and unseal
/// every live segment it names, and open them as one index.
///
/// Shared so a resuming client pays the list + fetch **once** and gets both
/// halves out of it — the manifest its builder appends to, and the opened index
/// its re-index guard is seeded from. Returns `None` for the manifest when this
/// actor has never published, which is the first-run path, not an error.
async fn open_mailcal(
    ring: &MailcalKeyRing,
    rail: &dyn SegmentRail,
) -> Result<OpenedMailcal, IndexBuildError> {
    open_class(ring, KindClass::MailCal, rail).await
}

/// The class-generic form of [`open_mailcal`].
///
/// The two classes differ in exactly three things — which manifest path to
/// fetch, which kinds to walk, and what opens the bytes — and all three come
/// from the [`ClassOpener`]. The master class deliberately has **no grace
/// ring**: its key is seed-derived and the seed does not rotate outside the
/// succession ceremony (`key-material-hierarchy.md` § Path A-sibling), so a
/// single key is the whole ring.
async fn open_class(
    opener: &dyn ClassOpener,
    class: KindClass,
    rail: &dyn SegmentRail,
) -> Result<OpenedMailcal, IndexBuildError> {
    // Over the `SegmentRail` seam rather than the concrete publisher, because
    // the two builder legs reach the same rail through different transports:
    // a client over `NestClient` + the blob route, the MDA bridge over the Go
    // MDA's WS-RPC connection behind the FFI rail callback
    // (`content-index.md` § Where the index is built). What they share is this
    // read, so it must not name either one's transport.
    let listed = rail.list_entries().await?;
    let manifest_path = opener.manifest_path();
    let Some(entry) = listed.iter().find(|e| e.path == manifest_path) else {
        return unbuilt_class();
    };
    let manifest = match opener.open_manifest(&rail.fetch_blob(&entry.blob_hash).await?) {
        Ok(manifest) => manifest,
        Err(e) => return rebuild_or_propagate(opener, &manifest_path, e),
    };

    let mut plaintexts = Vec::new();
    let mut stale_segments = Vec::new();
    for kind in class.kinds() {
        let Some(km) = manifest.kind(kind) else {
            continue;
        };
        for seg_id in &km.live_segments {
            let path = segment_path(kind, *seg_id);
            let Some(seg) = listed.iter().find(|e| e.path == path) else {
                // The manifest is the authority on what is live, but the rail
                // is the authority on what exists. A manifest naming a segment
                // the nest does not list is a partially-synced replica, not
                // corruption — the missing segment simply is not searchable
                // yet, and the next refresh picks it up.
                tracing::warn!(%path, "index: a live segment is not on the rail yet — skipping");
                continue;
            };
            let sealed = rail.fetch_blob(&seg.blob_hash).await?;
            // The format gate, decided from the plaintext seal stamp before
            // any open: a segment stamped below this build's format carries
            // an earlier schema, and `open_multi_segment`
            // refuses mixed schemas — so it is skipped here and retired by a
            // resuming builder (`OpenedMailcal::stale_segments`).
            if fauna_index::peek_sealed_stamp(&sealed)
                .is_some_and(|s| s.format_version < fauna_index::CURRENT_INDEX_FORMAT_VERSION)
            {
                tracing::info!(
                    %path,
                    "index: segment below the current format — skipping; a resuming \
                     builder tombstones it and the next walk re-stages its docs"
                );
                stale_segments.push((kind, *seg_id));
                continue;
            }
            match opener.open_segment(&sealed) {
                Ok(plaintext) => plaintexts.push(plaintext),
                // Deliberately the whole class, not "skip this segment": a
                // manifest that opens over segments that do not is the
                // half-rewrapped state (`key-material-hierarchy.md` § Path
                // A-sibling — the rewrap pass is the *other* complete answer),
                // and skipping would leave the unreadable segments listed live
                // forever while the index silently answers short. Rebuilding is
                // complete by construction; a partial index is not.
                Err(e) => return rebuild_or_propagate(opener, &path, e),
            }
        }
    }

    Ok(OpenedMailcal {
        manifest: Some(manifest),
        manifest_hash: Some(entry.blob_hash.clone()),
        index: Index::open_multi_segment(&plaintexts)?,
        stale_segments,
    })
}

/// The class as a fresh device sees it: nothing to append to, nothing already
/// indexed, and a reader that answers every query with no hits.
///
/// Two paths reach it and they mean different things — *never published* (the
/// first-run path) and *published under key material this seat cannot open*
/// (the succession rebuild below). Both leave the caller with the same correct
/// state, which is why they share one constructor.
fn unbuilt_class() -> Result<OpenedMailcal, IndexBuildError> {
    Ok(OpenedMailcal {
        manifest: None,
        manifest_hash: None,
        index: Index::open_multi_segment(&[])?,
        stale_segments: Vec::new(),
    })
}

/// What [`open_class`] does when a blob it fetched will not open: take the
/// ratified drop-and-re-index arm, or surface the error.
///
/// **The condition is a crypto failure specifically, never "it would not
/// open".** A blob that unseals cleanly and then fails to decode is not a
/// wrong-key story, and [`IndexError::Incompatible`] — *written by a newer
/// build, intact, do not touch it* — is exactly the blob a rebuild would
/// destroy (`version-compatibility.md` § 5 item 9: conflating "too old to read"
/// with "corrupt" is what licenses healing-by-overwrite).
fn rebuild_or_propagate(
    opener: &dyn ClassOpener,
    path: &str,
    err: IndexError,
) -> Result<OpenedMailcal, IndexBuildError> {
    match (opener.unopenable_disposition(), &err) {
        (UnopenableDisposition::RebuildFresh, IndexError::Crypto(_)) => {
            // The diagnosis lives here rather than at the launcher's retry warn
            // because this is the only frame that knows *which* key was tried
            // against *which* blob; upstack it is one opaque error among the
            // transient rail failures, which is how it came to be logged as one
            // ("retrying on the next sweep") while never healing.
            tracing::warn!(
                %path,
                error = %err,
                "index: this blob was sealed under different key material — most likely an \
                 identity succession, whose master key re-derives from the new seed. Treating \
                 the class as unbuilt and re-indexing under the current key (the index is \
                 derived data; key-material-hierarchy.md § Path A-sibling)"
            );
            unbuilt_class()
        }
        _ => Err(err.into()),
    }
}

/// What an unopenable blob means for a class — the one axis on which the two
/// classes' recovery differs.
///
/// Ratified by `key-material-hierarchy.md` § Path A-sibling: the index is
/// derived data, so *rewrap-under-the-new-key and drop-and-re-index are both
/// complete answers*. Which one a class may take alone, with no coordination,
/// is decided by whether its key is convergent.
enum UnopenableDisposition {
    /// Drop the class and re-index it.
    ///
    /// Sound **because the master key is seed-derived with no grace ring**:
    /// every device of the account derives the identical key from the identical
    /// seed, so a device that rebuilds cannot be fighting a sibling that reads
    /// the old blobs fine — post-succession no device can, and pre-succession
    /// every device agrees. The predecessor's blobs stay on the rail referenced
    /// by nothing, which is what "drop" means here: nothing is deleted, and the
    /// source content the index is derived from is untouched.
    RebuildFresh,
    /// Surface the error and leave the blobs alone.
    ///
    /// The mail/calendar class's key rotates *with a grace ring*, which exists
    /// precisely so a rotation in flight stays survivable. A device whose ring
    /// is merely stale — a snapshot it has not synced yet — would, on the
    /// rebuild arm, answer a recoverable miss by discarding a corpus every
    /// other device still reads, then republish it under a generation those
    /// devices may not hold. Surfacing costs a wedge; rebuilding costs a
    /// fleet-wide flip-flop, and only one of those is recoverable by waiting.
    Propagate,
}

/// What [`open_class`] needs from a class's key material: where its manifest
/// lives and how to unseal both halves.
///
/// Deliberately **not** public: the two implementors are the mail/calendar
/// grace ring and a single master [`ClassKey`], and a third would mean a third
/// key class, which is a design change rather than a new caller.
/// `Sync` so the rail read's future stays `Send` — the local search arm awaits
/// it from a `Send` async-trait method.
trait ClassOpener: Sync {
    fn manifest_path(&self) -> String;
    fn open_manifest(&self, sealed: &[u8]) -> Result<IndexManifest, IndexError>;
    fn open_segment(&self, sealed: &[u8]) -> Result<Vec<u8>, IndexError>;
    /// See [`UnopenableDisposition`] — a per-class property of the key
    /// material, which is why it is answered by the opener rather than by a
    /// branch on [`KindClass`] at the call site.
    fn unopenable_disposition(&self) -> UnopenableDisposition;
}

impl ClassOpener for MailcalKeyRing {
    fn manifest_path(&self) -> String {
        mailcal_manifest_path()
    }
    fn open_manifest(&self, sealed: &[u8]) -> Result<IndexManifest, IndexError> {
        MailcalKeyRing::open_manifest(self, sealed)
    }
    fn open_segment(&self, sealed: &[u8]) -> Result<Vec<u8>, IndexError> {
        MailcalKeyRing::open_segment(self, sealed)
    }
    fn unopenable_disposition(&self) -> UnopenableDisposition {
        // A ring miss can be a stale ring rather than a dead key.
        UnopenableDisposition::Propagate
    }
}

impl ClassOpener for ClassKey {
    fn manifest_path(&self) -> String {
        ClassKey::manifest_path(self)
    }
    fn open_manifest(&self, sealed: &[u8]) -> Result<IndexManifest, IndexError> {
        ClassKey::open_manifest(self, sealed)
    }
    fn open_segment(&self, sealed: &[u8]) -> Result<Vec<u8>, IndexError> {
        ClassKey::open_segment_bytes(self, sealed)
    }
    fn unopenable_disposition(&self) -> UnopenableDisposition {
        match self {
            // Convergent by derivation — see the variant docs.
            ClassKey::Master(_) => UnopenableDisposition::RebuildFresh,
            // The same class as `MailcalKeyRing` above, reached with a
            // single generation instead of a ring; a bare key is *more*
            // likely to be missing a generation than the ring is, never
            // less, so it can only be at least as conservative.
            ClassKey::MailCal(_) => UnopenableDisposition::Propagate,
        }
    }
}

/// What one [`open_class`] rail read yields: the manifest the builder appends
/// to, the hash a reader decides staleness by, and the opened index.
struct OpenedMailcal {
    /// `None` when this actor has never published — the first-run path.
    manifest: Option<IndexManifest>,
    manifest_hash: Option<String>,
    index: Index,
    /// Live manifest entries whose sealed blobs are stamped **below** this
    /// build's format version — segments of an earlier schema, which must
    /// not be opened beside a current
    /// segment (`open_multi_segment` requires one schema). Skipped from
    /// `index`, so their docs never seed a re-index guard (the next walk
    /// re-stages them); a resuming **builder** additionally retires them
    /// (`IndexBuilder::retire_segments`) — the ratified tombstone-only
    /// retirement from `content-index.md` § Where the index is built → the
    /// 2026-08-10 carrier ruling. Readers just skip: narrowed coverage, and
    /// the scan arm answers for the skipped docs.
    stale_segments: Vec<(ContentKind, u32)>,
}

/// Start a mail builder from whatever this actor's nest already holds.
///
/// The one call app glue makes at login: resume against the published
/// mail/calendar manifest so a second run **appends to the same segment chain**
/// (and inherits the advisory ingest cursor) instead of restarting segment ids.
/// A fresh actor — nothing published yet — gets an empty manifest, which is the
/// first-run path and not an error.
///
/// **This does not stop the mailbox being re-walked** — nothing can, and nothing
/// should: the receive loop's paging cursors are per-session and in-memory
/// (`fauna_conversations::session`), so every launch re-pages the whole mailbox
/// from UID 0 to rebuild the conversation view, and the index seam fires for all
/// of it. Resuming the manifest keeps the *segment chain* continuous; what keeps
/// that re-walk from re-*publishing* an index the actor already has is the
/// stage-time re-index guard, which this function seeds below from the segments
/// it just opened (`content-index.md` § Ingest triggers, v1 — the 2026-08-03
/// correction).
///
/// Deliberately a free function rather than a `IndexBuilder` constructor:
/// the builder stays generic over the [`SegmentRail`] seam (that is what
/// keeps this crate free of a transport dependency in its core), and only this
/// module knows the rail.
pub async fn resume_mail_builder(
    msek: &[u8; 32],
    ring: &MailcalKeyRing,
    rail: Arc<dyn SegmentRail>,
) -> Result<IndexBuilder, IndexBuildError> {
    // `msek` and `ring` are both taken, and neither is redundant: the builder
    // *writes* under the current generation the msek derives, while the resume
    // *read* below must reach back across an MSEK rotation through the ring's
    // grace generations. Collapsing them would silently pick one behaviour for
    // both halves.
    let opened = open_mailcal(ring, rail.as_ref()).await?;
    let Some(manifest) = opened.manifest else {
        // Never published — nothing to append to and nothing already indexed.
        return Ok(IndexBuilder::mail(msek, rail));
    };
    let builder = IndexBuilder::with_manifest(
        IndexBuilder::mail_key(msek),
        // Mail only, not all of `MAILCAL_KINDS`: the calendar kind shares this
        // class's key and manifest but has no producer yet, and a kind is added
        // to a builder when its arm lands, not when its class does.
        [ContentKind::Mail],
        manifest,
        rail,
    );
    // Seed the re-index guard from the segments we just opened: this is what
    // stops the receive loop's every-launch mailbox re-walk from re-publishing
    // an index the actor already has. Below-format segments were skipped from that
    // open, so their docs are deliberately NOT seeded — the walk re-stages
    // them under the current format — and the segments themselves are retired
    // at the next flush.
    //
    // Identities, not bare ids: the guard keys on the nest record id beside the
    // content id, so a Message-ID whose content a `Sent`-copy displacement has
    // replaced is re-staged rather than skipped (`content-index-ingest.md`
    // § Ingest triggers, v1 → *A mail content id is a stable identity, not
    // immutable content*).
    builder.seed_indexed(opened.index.doc_identities()?);
    builder.retire_segments(opened.stale_segments);
    Ok(builder)
}

/// The master-class twin of [`resume_mail_builder`]: resume `kind`'s builder
/// against `manifest.idx`, seeding the re-index guard from every master-class
/// segment already published.
///
/// **The guard is seeded from the whole class, not just `kind`.** That is
/// deliberate and costs nothing: `content_ids` are unique per `(kind,
/// content_id)` and a builder only ever stages its own kind, so the extra ids
/// can never suppress a doc this builder should have written — while opening
/// the class once is what a future second master kind needs anyway.
///
/// No key ring: the master key is seed-derived and the seed does not rotate
/// outside the succession ceremony, so there are no grace generations to reach
/// back through (`key-material-hierarchy.md` § Path A-sibling — succession
/// re-keys by rewrap or re-index, not by a grace list).
pub async fn resume_master_builder(
    key: IndexMasterKey,
    kinds: impl IntoIterator<Item = ContentKind>,
    rail: Arc<dyn SegmentRail>,
) -> Result<IndexBuilder, IndexBuildError> {
    let class_key = ClassKey::Master(key);
    let opened = open_class(&class_key, KindClass::Master, rail.as_ref()).await?;
    // Never published → a fresh master manifest; nothing to append to and
    // nothing already indexed.
    let manifest = opened.manifest.unwrap_or_else(|| {
        IndexManifest::empty(KindClass::Master, fauna_index::TOKENIZER_PIPELINE_VERSION)
    });
    let builder = IndexBuilder::with_manifest(class_key, kinds, manifest, rail);
    // The opened index spans every kind the class manifest lists, so this seeds
    // the guard for all of them at once — which is exactly right for a builder
    // that stages several of those kinds, and harmless for ids belonging to a
    // kind it does not stage (nothing will ever offer them to it). Below-format
    // segments were skipped from the open (not seeded), and are retired at the
    // next flush.
    builder.seed_indexed(opened.index.doc_identities()?);
    builder.retire_segments(opened.stale_segments);
    Ok(builder)
}

#[async_trait::async_trait]
impl SegmentRail for IndexRailPublisher {
    async fn list_entries(&self) -> Result<Vec<RailEntry>, IndexBuildError> {
        Ok(self
            .list()
            .await?
            .entries
            .into_iter()
            .map(|e| RailEntry {
                path: e.path,
                blob_hash: e.blob_hash,
                // The rail's wire type carries an `i64`; a negative size is
                // not representable by any writer, and clamping keeps a
                // hostile or corrupt row from wrapping the fold's budget
                // arithmetic into "this segment is enormous".
                size_bytes: e.size_bytes.max(0) as u64,
            })
            .collect())
    }

    async fn fetch_blob(&self, blob_hash: &str) -> Result<Vec<u8>, IndexBuildError> {
        // Explicitly the inherent method: the trait method shares its name, and
        // `self.fetch_blob(..)` here would be a silent infinite recursion if the
        // inherent one were ever removed or renamed.
        IndexRailPublisher::fetch_blob(self, blob_hash).await
    }

    async fn publish(&self, path: &str, bytes: &[u8]) -> Result<(), IndexBuildError> {
        let blob_hash = self
            .put_bytes(bytes)
            .await
            .map_err(|reason| IndexBuildError::Publish {
                path: path.to_string(),
                reason,
            })?;

        // Bytes first, reference second — the nest enforces it (typed retryable
        // `fauna.index.bytes_not_held` if the blob is absent), and the builder's
        // segment-then-manifest order rests on it.
        self.nest
            .request::<_, RecordIndexBlobReply>(
                KIND_RECORD,
                RecordIndexBlobRequest {
                    path: path.to_string(),
                    blob_hash,
                    size_bytes: bytes.len() as i64,
                    extra: Default::default(),
                },
            )
            .await
            .map_err(|e| IndexBuildError::Publish {
                path: path.to_string(),
                reason: format!("{KIND_RECORD}: {e}"),
            })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_mls::wrapped_blob::{MlsSnapshotPlaintext, derive_index_segment_key};
    use serde_bytes::ByteBuf;

    /// A minimal in-memory [`SegmentRail`], last-write-wins per path — enough
    /// for a resume to find what a flush published.
    #[derive(Default)]
    struct MemRail {
        blobs: std::sync::Mutex<Vec<(String, Vec<u8>)>>,
    }

    /// Hashed exactly the way the production rail hashes
    /// ([`IndexRailPublisher::put_bytes`]), so a resume looks blobs up the same
    /// way it would against a real nest.
    fn mem_hash(bytes: &[u8]) -> String {
        fauna_core::hex32::encode(&Cid::of_raw(bytes).digest())
    }

    #[async_trait::async_trait]
    impl SegmentRail for MemRail {
        async fn list_entries(&self) -> Result<Vec<RailEntry>, IndexBuildError> {
            let blobs = self.blobs.lock().unwrap();
            let mut latest: Vec<(String, Vec<u8>)> = Vec::new();
            for (path, bytes) in blobs.iter() {
                match latest.iter_mut().find(|(p, _)| p == path) {
                    Some(slot) => slot.1 = bytes.clone(),
                    None => latest.push((path.clone(), bytes.clone())),
                }
            }
            Ok(latest
                .into_iter()
                .map(|(path, bytes)| RailEntry {
                    blob_hash: mem_hash(&bytes),
                    size_bytes: bytes.len() as u64,
                    path,
                })
                .collect())
        }

        async fn fetch_blob(&self, blob_hash: &str) -> Result<Vec<u8>, IndexBuildError> {
            self.blobs
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|(_, b)| mem_hash(b) == blob_hash)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| IndexBuildError::Publish {
                    path: blob_hash.into(),
                    reason: "no such blob".into(),
                })
        }

        async fn publish(&self, path: &str, bytes: &[u8]) -> Result<(), IndexBuildError> {
            self.blobs
                .lock()
                .unwrap()
                .push((path.to_string(), bytes.to_vec()));
            Ok(())
        }
    }

    /// The below-format retirement gate (`content-index.md` § Where the index is
    /// built — the 2026-08-10 carrier ruling): a live segment whose seal stamp
    /// is below this build's format is (1) skipped from the resume's open — so
    /// its docs never seed the re-index guard and the next walk re-stages them
    /// under the current format — and (2) tombstoned by the resumed builder's
    /// next flush, so no reader ever meets a mixed-schema slice.
    ///
    /// The stale blob is forged bytes with a real `FXSG` v2 prefix and garbage
    /// past it, which is exactly the point: the gate must decide from the
    /// plaintext stamp alone, before any open — fed to `open_multi_segment`
    /// these bytes would error, and fed to the unsealer they would fail AEAD.
    #[tokio::test]
    async fn a_below_format_segment_is_skipped_at_resume_and_tombstoned_at_the_next_flush() {
        const MSEK: [u8; 32] = [7u8; 32];
        let rail = Arc::new(MemRail::default());
        let ring = MailcalKeyRing::from_msek(&MSEK);
        let thread = fauna_conversations::thread::ThreadId("t-1".into());
        let message = fauna_conversations::message::MessageId("<a@x>".into());
        let stage = |builder: &IndexBuilder| {
            builder.stage(&fauna_conversations::index_sink::IndexableMessage {
                kind: fauna_conversations::index_sink::IndexableKind::Mail,
                thread_id: &thread,
                message_id: &message,
                subject: None,
                body: "harbour lights",
                sender_actor_id: None,
                nest_message_id: Some(&[0xA1u8; 32]),
                timestamp_ms: 1,
                is_own: false,
            });
        };

        // A first seat publishes normally, then its segment blob is shadowed by
        // a forged below-format blob at the same path — the manifest still
        // names the segment as live.
        let first = IndexBuilder::mail(&MSEK, rail.clone());
        stage(&first);
        assert_eq!(first.flush().await.expect("flush").len(), 1);
        let seg_path = segment_path(ContentKind::Mail, 1);
        let mut forged = b"FXSG".to_vec();
        forged.extend_from_slice(&[2, 2, 0, 0]); // format_version 2, floor 2
        forged.extend_from_slice(b"below-format segment bytes no current build may open");
        rail.blobs
            .lock()
            .unwrap()
            .push((seg_path.clone(), forged.clone()));

        // Resume: the stale segment is skipped, so the guard is empty and the
        // walk's re-presentation of the same message stages again.
        let resumed = resume_mail_builder(&MSEK, &ring, rail.clone())
            .await
            .expect("resume must not try to open the stale blob");
        assert_eq!(
            resumed.indexed_len(),
            0,
            "a skipped below-format segment must not seed the re-index guard"
        );
        stage(&resumed);
        assert_eq!(resumed.flush().await.expect("flush").len(), 1);

        // The flush retired the stale segment: the published manifest lists
        // exactly one live mail segment (the new one), with seg 1 tombstoned.
        let manifest_bytes = SegmentRail::fetch_blob(
            rail.as_ref(),
            &SegmentRail::list_entries(rail.as_ref())
                .await
                .expect("list")
                .into_iter()
                .find(|e| e.path == mailcal_manifest_path())
                .expect("manifest on the rail")
                .blob_hash,
        )
        .await
        .expect("fetch manifest");
        let manifest = ring.open_manifest(&manifest_bytes).expect("open manifest");
        let km = manifest.kind(ContentKind::Mail).expect("mail kind");
        assert_eq!(
            km.live_segments,
            vec![2],
            "the stale segment must be retired, the fresh one live"
        );
        assert!(
            km.tombstoned_segments.contains(&1),
            "retirement is tombstone-only — the id is recorded, the blob untouched"
        );

        // And a third resume finds a clean, current-format slice: the guard
        // seeds and nothing further is staged or retired.
        let healed = resume_mail_builder(&MSEK, &ring, rail.clone())
            .await
            .expect("resume over the healed slice");
        assert_eq!(healed.indexed_len(), 1, "the re-staged doc seeds the guard");
        stage(&healed);
        assert_eq!(
            healed.pending_len(),
            0,
            "the guard declines the walk's re-presentation — no growth"
        );
    }

    /// The master arm's resume contract, and the reason it is not optional: the
    /// conversation store re-presents its whole corpus at every launch (the
    /// catch-up walk), so a resume that failed to seed the re-index guard would
    /// republish the entire corpus as a fresh segment chain every single launch
    /// — the unbounded growth the mail arm's guard exists to prevent, on a kind
    /// whose corpus is far larger.
    #[tokio::test]
    async fn a_resumed_master_builder_inherits_the_chain_and_the_re_index_guard() {
        const MASTER: [u8; 32] = [44u8; 32];
        let rail = Arc::new(MemRail::default());
        let key = || IndexMasterKey::from_bytes(MASTER);

        let first = IndexBuilder::master(key(), [ContentKind::Conversation], rail.clone());
        let thread = fauna_conversations::thread::ThreadId("t-1".into());
        let message = fauna_conversations::message::MessageId("m-1".into());
        first.stage(&fauna_conversations::index_sink::IndexableMessage {
            kind: fauna_conversations::index_sink::IndexableKind::Conversation,
            thread_id: &thread,
            message_id: &message,
            subject: None,
            body: "harbour lights",
            sender_actor_id: None,
            nest_message_id: None,
            timestamp_ms: 1,
            is_own: false,
        });
        assert_eq!(
            first.flush().await.expect("flush").len(),
            1,
            "one kind, one segment"
        );

        let resumed = resume_master_builder(key(), [ContentKind::Conversation], rail.clone())
            .await
            .expect("resume");
        assert_eq!(
            resumed.indexed_len(),
            1,
            "the guard must be seeded from the published master-class segment"
        );

        // Re-presenting the same message — exactly what the launch walk does —
        // stages nothing, so the next flush publishes nothing.
        resumed.stage(&fauna_conversations::index_sink::IndexableMessage {
            kind: fauna_conversations::index_sink::IndexableKind::Conversation,
            thread_id: &thread,
            message_id: &message,
            subject: None,
            body: "harbour lights",
            sender_actor_id: None,
            nest_message_id: None,
            timestamp_ms: 1,
            is_own: false,
        });
        assert_eq!(resumed.pending_len(), 0, "skip-if-present must transfer");
    }

    /// A fresh actor has published nothing; the resume is the first-run path,
    /// not an error.
    #[tokio::test]
    async fn a_master_resume_against_an_empty_rail_is_a_fresh_builder() {
        let rail = Arc::new(MemRail::default());
        let builder = resume_master_builder(
            IndexMasterKey::from_bytes([55u8; 32]),
            [ContentKind::Conversation],
            rail,
        )
        .await
        .expect("an unpublished actor resumes cleanly");
        assert_eq!(builder.indexed_len(), 0);
    }

    /// Two distinct MSEK generations: `NOW` is live, `PRIOR` was rotated away.
    const NOW: [u8; 32] = [11u8; 32];
    const PRIOR: [u8; 32] = [22u8; 32];
    const STRANGER: [u8; 32] = [33u8; 32];

    /// A snapshot as `build_mls_snapshot_plaintext` writes it: the grace list
    /// carries **prior** generations only, newest first — the current key is
    /// re-derived from the session MSEK and never carried.
    fn snapshot_with_grace(priors: &[[u8; 32]]) -> Vec<u8> {
        let snap = MlsSnapshotPlaintext {
            index_seg_grace_keys: priors
                .iter()
                .map(|m| ByteBuf::from(derive_index_segment_key(m).to_vec()))
                .collect(),
            ..Default::default()
        };
        snap.to_canonical_bytes().expect("encode snapshot")
    }

    fn seal_under(msek: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
        let key = IndexSegmentKey::from_bytes(*derive_index_segment_key(msek));
        fauna_index::seal_segment_bytes_mailcal(plaintext, &key).expect("seal")
    }

    /// The property the grace list exists for: after an MSEK rotation, a
    /// segment sealed under the *previous* generation still opens — otherwise
    /// every pre-rotation segment silently vanishes from its owner's search
    /// until the rewrap pass converges.
    #[test]
    fn a_segment_sealed_under_a_prior_generation_opens_through_the_grace_ring() {
        let sealed = seal_under(&PRIOR, b"pre-rotation segment");
        let ring = MailcalKeyRing::from_msek_and_snapshot(&NOW, &snapshot_with_grace(&[PRIOR]))
            .expect("ring");

        assert_eq!(ring.len(), 2, "current + one grace generation");
        assert_eq!(
            ring.open_segment(&sealed).expect("grace key opens it"),
            b"pre-rotation segment"
        );
    }

    /// The same bytes are unopenable without the grace list — which is what
    /// makes the test above assert the ring, not the seal.
    #[test]
    fn a_current_only_ring_cannot_open_a_prior_generations_segment() {
        let sealed = seal_under(&PRIOR, b"pre-rotation segment");
        assert!(
            MailcalKeyRing::from_msek(&NOW)
                .open_segment(&sealed)
                .is_err()
        );
    }

    /// A ring is not a skeleton key: a generation it never held stays shut.
    #[test]
    fn a_segment_from_outside_the_ring_is_refused() {
        let sealed = seal_under(&STRANGER, b"someone else's segment");
        let ring = MailcalKeyRing::from_msek_and_snapshot(&NOW, &snapshot_with_grace(&[PRIOR]))
            .expect("ring");

        let err = ring.open_segment(&sealed).expect_err("must not open");
        assert!(
            format!("{err}").contains("no key in the ring opens it"),
            "the exhaustion error should read as a ring miss, got: {err}"
        );
    }

    /// Writes never reach past index 0. Sealing under a grace key would extend
    /// a superseded generation's reach forward, which is what rotation ends.
    #[test]
    fn the_ring_writes_under_the_current_generation_only() {
        let ring = MailcalKeyRing::from_msek_and_snapshot(&NOW, &snapshot_with_grace(&[PRIOR]))
            .expect("ring");
        let sealed =
            fauna_index::seal_segment_bytes_mailcal(b"fresh", ring.current()).expect("seal");

        // Openable by a peer holding only the live MSEK — i.e. it was sealed
        // under the current generation, not the grace one.
        assert_eq!(
            MailcalKeyRing::from_msek(&NOW)
                .open_segment(&sealed)
                .expect("current-only ring opens it"),
            b"fresh"
        );
    }

    /// Every generation the snapshot names is reachable, not just the newest
    /// grace entry — asserted through the seal rather than through the private
    /// key vector, so it stays a statement about what the ring can *open*.
    #[test]
    fn the_ring_opens_every_generation_the_snapshot_carries() {
        let ring =
            MailcalKeyRing::from_msek_and_snapshot(&NOW, &snapshot_with_grace(&[PRIOR, STRANGER]))
                .expect("ring");

        assert_eq!(ring.len(), 3, "current + two grace generations");
        for (msek, body) in [
            (&NOW, b"current".as_slice()),
            (&PRIOR, b"one back".as_slice()),
            (&STRANGER, b"two back".as_slice()),
        ] {
            assert_eq!(
                ring.open_segment(&seal_under(msek, body))
                    .expect("every carried generation opens"),
                body
            );
        }
    }

    /// A snapshot from before the grace field existed (or from an actor that
    /// has never rotated) degrades to exactly the current-only ring.
    #[test]
    fn a_snapshot_with_no_grace_entries_is_the_current_only_ring() {
        let ring =
            MailcalKeyRing::from_msek_and_snapshot(&NOW, &snapshot_with_grace(&[])).expect("ring");
        assert_eq!(ring.len(), 1);
    }

    /// The client leg's ring, built from its own mail custody's MSEK history
    /// rather than a snapshot, opens exactly what the snapshot ring opens: the
    /// grace key a snapshot carries is the derivation of the prior MSEK the
    /// custody holds. Without it, a user who rotates their mail keys loses
    /// their own local mail/calendar search — the manifest sealed before the
    /// rotation no longer opens and the builder never resumes.
    #[test]
    fn the_custody_ring_opens_a_prior_generations_manifest_like_the_snapshot_ring() {
        let manifest = IndexManifest::empty(
            fauna_index::KindClass::MailCal,
            fauna_index::TOKENIZER_PIPELINE_VERSION,
        );
        let prior_key = IndexSegmentKey::from_bytes(*derive_index_segment_key(&PRIOR));
        let sealed = manifest.to_sealed_bytes_mailcal(&prior_key).expect("seal");

        let ring = MailcalKeyRing::from_msek_and_priors(&NOW, &[PRIOR]);
        assert_eq!(ring.len(), 2, "current + one grace generation");
        ring.open_manifest(&sealed)
            .expect("the prior MSEK's key opens the pre-rotation manifest");
        assert_eq!(
            ring.open_segment(&seal_under(&PRIOR, b"one back"))
                .expect("and its segments"),
            b"one back"
        );
        // Writes stay on the current generation, as for every ring.
        assert_eq!(
            MailcalKeyRing::from_msek(&NOW)
                .open_segment(
                    &fauna_index::seal_segment_bytes_mailcal(b"fresh", ring.current())
                        .expect("seal")
                )
                .expect("current-only ring opens it"),
            b"fresh"
        );
        assert_eq!(MailcalKeyRing::from_msek_and_priors(&NOW, &[]).len(), 1);
    }

    /// The manifest twin walks the same ring — proven separately because it
    /// goes through a different open path (`from_sealed_bytes_mailcal`) whose
    /// class check runs *after* the unwrap.
    #[test]
    fn the_manifest_opens_through_the_grace_ring_too() {
        let mut manifest = IndexManifest::empty(
            fauna_index::KindClass::MailCal,
            fauna_index::TOKENIZER_PIPELINE_VERSION,
        );
        manifest
            .append_segment(ContentKind::Mail)
            .expect("append a mail segment");
        let prior_key = IndexSegmentKey::from_bytes(*derive_index_segment_key(&PRIOR));
        let sealed = manifest.to_sealed_bytes_mailcal(&prior_key).expect("seal");

        let ring = MailcalKeyRing::from_msek_and_snapshot(&NOW, &snapshot_with_grace(&[PRIOR]))
            .expect("ring");
        let opened = ring.open_manifest(&sealed).expect("grace key opens it");
        assert_eq!(
            opened
                .kind(ContentKind::Mail)
                .map(|k| k.live_segments.len()),
            Some(1)
        );
    }

    /// Two identity generations: the master key is seed-derived, so a succession
    /// hands the successor a *different* key over the *same* rail — the
    /// predecessor's `__index` set re-points to it wholesale
    /// (`bins/fauna-nest/src/db/successions.rs`, the `folders.actor_id`
    /// re-point), blobs and all.
    const PREDECESSOR_MASTER: [u8; 32] = [77u8; 32];
    const SUCCESSOR_MASTER: [u8; 32] = [78u8; 32];

    fn conversation_doc(
        id: &str,
        body: &'static str,
    ) -> fauna_conversations::index_sink::IndexableMessage<'static> {
        // Leaked so the borrowed ids outlive the call — test-only, and one
        // allocation per staged doc.
        let thread = Box::leak(Box::new(fauna_conversations::thread::ThreadId(format!(
            "t-{id}"
        ))));
        let message = Box::leak(Box::new(fauna_conversations::message::MessageId(format!(
            "m-{id}"
        ))));
        fauna_conversations::index_sink::IndexableMessage {
            kind: fauna_conversations::index_sink::IndexableKind::Conversation,
            thread_id: thread,
            message_id: message,
            subject: None,
            body,
            sender_actor_id: None,
            nest_message_id: None,
            timestamp_ms: 1,
            is_own: false,
        }
    }

    /// Every live segment the published manifest names must open under the key
    /// the caller holds — the general invariant, asserted over whatever the
    /// manifest actually lists rather than an id the test picked, so it holds
    /// for kinds and flush shapes nobody wrote a case for.
    async fn assert_every_live_segment_opens(rail: &MemRail, key: &ClassKey) {
        let listed = rail.list_entries().await.expect("list");
        let entry = listed
            .iter()
            .find(|e| e.path == key.manifest_path())
            .expect("a manifest was published");
        let manifest = key
            .open_manifest(&rail.fetch_blob(&entry.blob_hash).await.expect("fetch"))
            .expect("the manifest opens under the current key");
        let mut seen = 0usize;
        for kind in KindClass::Master.kinds() {
            let Some(km) = manifest.kind(kind) else {
                continue;
            };
            for seg_id in &km.live_segments {
                let path = segment_path(kind, *seg_id);
                let seg = listed
                    .iter()
                    .find(|e| e.path == path)
                    .unwrap_or_else(|| panic!("{path} is named live but is not on the rail"));
                let sealed = rail.fetch_blob(&seg.blob_hash).await.expect("fetch");
                key.open_segment_bytes(&sealed)
                    .unwrap_or_else(|e| panic!("{path} is named live but will not open: {e}"));
                seen += 1;
            }
        }
        assert!(seen > 0, "the invariant must have had something to check");
    }

    /// **The half-rewrapped state** — the gap a rewrap pass leaves if it
    /// re-seals `manifest.idx` and stops (`key-material-hierarchy.md` § Path
    /// A-sibling names rewrap as the *other* complete answer). The manifest now
    /// opens and its segments do not, so the wrong-key arm must be reachable
    /// through the segment door too: skipping the unopenable segment instead
    /// would leave it named live forever while the index silently answered
    /// short — a partial index that no later flush ever repairs, because
    /// nothing retires a live segment it cannot read.
    #[tokio::test]
    async fn a_manifest_that_opens_over_segments_that_do_not_rebuilds_the_whole_class() {
        let rail = Arc::new(MemRail::default());
        let predecessor = ClassKey::Master(IndexMasterKey::from_bytes(PREDECESSOR_MASTER));
        let successor = ClassKey::Master(IndexMasterKey::from_bytes(SUCCESSOR_MASTER));

        let before = IndexBuilder::master(
            IndexMasterKey::from_bytes(PREDECESSOR_MASTER),
            [ContentKind::Conversation],
            rail.clone(),
        );
        before.stage(&conversation_doc("old", "harbour lights"));
        before.flush().await.expect("flush");

        // The rewrap pass that only got as far as the manifest.
        let listed = rail.list_entries().await.expect("list");
        let entry = listed
            .iter()
            .find(|e| e.path == predecessor.manifest_path())
            .expect("published");
        let manifest = predecessor
            .open_manifest(&rail.fetch_blob(&entry.blob_hash).await.expect("fetch"))
            .expect("opens under the predecessor's key");
        rail.publish(
            &successor.manifest_path(),
            &manifest
                .to_sealed_bytes(match &successor {
                    ClassKey::Master(k) => k,
                    ClassKey::MailCal(_) => unreachable!("constructed as Master"),
                })
                .expect("re-seal"),
        )
        .await
        .expect("publish");

        let resumed = resume_master_builder(
            IndexMasterKey::from_bytes(SUCCESSOR_MASTER),
            [ContentKind::Conversation],
            rail.clone(),
        )
        .await
        .expect("the arm must attach");
        resumed.stage(&conversation_doc("new", "pier and swell"));
        resumed.flush().await.expect("flush");

        assert_every_live_segment_opens(&rail, &successor).await;
    }

    /// **The succession pin**. A manifest sealed under the
    /// predecessor's master key is not a transient rail failure and never heals:
    /// the key derives from the identity seed, so every later login derives the
    /// successor's key and meets the same unopenable blob. Taking the ratified
    /// drop-and-re-index arm (`key-material-hierarchy.md` § Path A-sibling) must
    /// leave the arm *attached* with a fresh manifest, and the next flush must
    /// republish under the successor's key.
    #[tokio::test]
    async fn a_predecessor_sealed_master_manifest_rebuilds_fresh_instead_of_wedging() {
        let rail = Arc::new(MemRail::default());

        let before = IndexBuilder::master(
            IndexMasterKey::from_bytes(PREDECESSOR_MASTER),
            [ContentKind::Conversation],
            rail.clone(),
        );
        before.stage(&conversation_doc("old", "harbour lights"));
        assert_eq!(before.flush().await.expect("flush").len(), 1);

        let resumed = resume_master_builder(
            IndexMasterKey::from_bytes(SUCCESSOR_MASTER),
            [ContentKind::Conversation],
            rail.clone(),
        )
        .await
        .expect("a predecessor-sealed manifest must not wedge the master arm");
        assert_eq!(
            resumed.indexed_len(),
            0,
            "drop-and-re-index: nothing openable was inherited, so the guard starts empty"
        );

        resumed.stage(&conversation_doc("new", "pier and swell"));
        assert_eq!(
            resumed.flush().await.expect("flush").len(),
            1,
            "the rebuilt chain publishes under the successor's key"
        );

        let reader =
            open_master_reader(IndexMasterKey::from_bytes(SUCCESSOR_MASTER), rail.as_ref())
                .await
                .expect("reader");
        assert_eq!(
            reader
                .query("swell", &[ContentKind::Conversation], None, 10)
                .expect("query")
                .len(),
            1,
            "the republished segment is searchable under the successor's key"
        );
    }

    /// The reader twin of the pin above: the query side opens the *same*
    /// `open_class`, so a successor querying before anything has been rebuilt
    /// must get the empty index — the same posture as a never-published actor —
    /// rather than an error the Search page would have to render.
    #[tokio::test]
    async fn the_master_reader_answers_empty_on_a_predecessor_sealed_manifest() {
        let rail = Arc::new(MemRail::default());
        let before = IndexBuilder::master(
            IndexMasterKey::from_bytes(PREDECESSOR_MASTER),
            [ContentKind::Conversation],
            rail.clone(),
        );
        before.stage(&conversation_doc("old", "harbour lights"));
        before.flush().await.expect("flush");

        let reader =
            open_master_reader(IndexMasterKey::from_bytes(SUCCESSOR_MASTER), rail.as_ref())
                .await
                .expect("the reader must not error on a predecessor-sealed manifest");
        assert!(reader.manifest_hash().is_none(), "nothing was inherited");
        assert!(
            reader
                .query("harbour", &[ContentKind::Conversation], None, 10)
                .expect("query")
                .is_empty(),
            "the predecessor's content is not readable — and that is a no-hits answer, not an error"
        );
    }

    /// **The asymmetry is deliberate, and this pins it.** Only the master class
    /// takes the rebuild arm: its key is seed-derived with no grace ring, so
    /// every device converges on the same successor key. The mail/calendar class
    /// has a ring precisely so a rotation in flight is *survivable*, and a device
    /// whose ring is merely stale must not answer by dropping the corpus every
    /// other device can still read.
    #[tokio::test]
    async fn a_mailcal_manifest_outside_the_ring_still_propagates() {
        let rail = Arc::new(MemRail::default());
        let mut manifest = IndexManifest::empty(
            fauna_index::KindClass::MailCal,
            fauna_index::TOKENIZER_PIPELINE_VERSION,
        );
        manifest
            .append_segment(ContentKind::Mail)
            .expect("append a mail segment");
        let stranger = IndexSegmentKey::from_bytes(*derive_index_segment_key(&STRANGER));
        rail.publish(
            &mailcal_manifest_path(),
            &manifest.to_sealed_bytes_mailcal(&stranger).expect("seal"),
        )
        .await
        .expect("publish");

        let ring = MailcalKeyRing::from_msek(&NOW);
        assert!(
            resume_mail_builder(&NOW, &ring, rail.clone())
                .await
                .is_err(),
            "a mail/calendar manifest no ring key opens must surface, not silently drop the corpus"
        );
    }

    /// The rebuild arm is keyed on a **crypto** failure alone, never on "the
    /// manifest would not open". A blob that unseals cleanly and then fails to
    /// decode is not a wrong-key story, and the typed
    /// [`fauna_index::IndexError::Incompatible`] — *written by a newer build, the
    /// index is intact, do not touch it* — travels this same non-crypto path.
    /// Healing either by overwriting is the version-compatibility cliff
    /// (`version-compatibility.md` § 5 item 9).
    #[tokio::test]
    async fn a_master_manifest_that_unseals_but_will_not_decode_propagates() {
        let rail = Arc::new(MemRail::default());
        let key = ClassKey::Master(IndexMasterKey::from_bytes(SUCCESSOR_MASTER));
        rail.publish(
            &key.manifest_path(),
            &key.seal_blob(b"not a manifest").expect("seal"),
        )
        .await
        .expect("publish");

        assert!(
            resume_master_builder(
                IndexMasterKey::from_bytes(SUCCESSOR_MASTER),
                [ContentKind::Conversation],
                rail.clone(),
            )
            .await
            .is_err(),
            "only an unopenable-under-our-key blob rebuilds; a decode failure must surface"
        );
    }
}
