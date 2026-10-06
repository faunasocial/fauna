//! The share leg's ROW-half store: [`fauna_peer_share::server::ShareStore`]
//! over the engine's own per-set `SyncDb` (B2 — `p2p-shared-set-build.md` § *Build design —
//! the row half*).
//!
//! `changes_since` serves every row this replica holds for the set: the rows
//! `own_change_log` retained at the engine's recording funnel — **real
//! recorded rows**, this replica's own authorship with the nest-assigned seq,
//! each carrying the writer signature the engine made over it as served — and
//! the other writers' rows `relayed_change_log` kept byte-exact after the
//! reader verified them (the relayed-row lift, `p2p.md` § *Peer-served
//! change-row provenance*; `serves_held_row` filters in the crate, so this impl
//! never encodes the ruling). Rows synthesized from `sync_entries` are the trap
//! this store exists to refuse: they carry no seq, no authorship and no
//! signature, so the receiving side would have nothing to check.
//!
//! One store serves ONE set — the set id is pinned at construction and a
//! request naming any other set is a loud error, never an empty page: the
//! `ShareServer` only calls after its verdict admits the set, so a mismatch
//! here is a mis-wired assembly (the wrong store handed to a listener), and
//! serving `[]` would make that bug read as "no changes".
//!
//! The byte half (`manifest_bytes` / `chunk_body`) deliberately answers
//! `None` here: chunk re-derivation needs local plaintext plus the set's
//! generation keys, which live with the app-side node assembly (slice E), not
//! in the state DB. A composite store there wraps this one for rows and adds
//! the byte half beside it.
//!
//! Also home to the cached-writer-roster refresh
//! ([`refresh_writer_roster_via`], B2.2): one nest roster read reduced to the
//! per-actor writer fact the peer-row ingest consults fail-closed
//! (`SyncDb::cached_share_writer`), with the ruling's staleness posture on
//! the error arms.

use std::sync::Mutex;

use fauna_peer_share::provenance::LocalShareChange;
use fauna_peer_share::server::ShareStore;

use crate::db::SyncDb;

/// [`ShareStore`] over one set's `SyncDb` — the row half only (module doc).
pub struct SyncDbShareStore {
    set_id: [u8; 32],
    // A plain mutex, held only across quick synchronous SQLite calls (never
    // an await): `SyncDb` wraps a `rusqlite::Connection`, which is `Send` but
    // not `Sync`, and the trait object must be shareable.
    db: Mutex<SyncDb>,
}

impl SyncDbShareStore {
    /// A store serving `set_id`'s rows from `db` — the per-set state DB whose
    /// `own_change_log` the engine's recording funnel writes through.
    pub fn new(set_id: [u8; 32], db: SyncDb) -> Self {
        Self {
            set_id,
            db: Mutex::new(db),
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl ShareStore for SyncDbShareStore {
    async fn changes_since(
        &self,
        set: &[u8; 32],
        since: i64,
        max_rows: u32,
    ) -> anyhow::Result<Vec<LocalShareChange>> {
        if *set != self.set_id {
            anyhow::bail!(
                "mis-wired share store: this store serves set {}, asked for {}",
                hex::encode(self.set_id),
                hex::encode(set)
            );
        }
        let db = self.db.lock().expect("share-store db mutex poisoned");
        // The sequenced page: own and relayed rows share the nest's one seq
        // space for the set, so they merge into one seq-ordered page.
        let mut page: Vec<(i64, LocalShareChange)> = db
            .own_changes_since(since, max_rows)?
            .into_iter()
            .map(|r| {
                // `own_change_log`'s sequenced read serves rows retained at
                // the funnel's `Ok(seq)` — nest-sequenced and this replica's
                // own authorship by construction.
                let seq = r.seq.expect("own_changes_since serves sequenced rows only");
                (seq, own_share_change(r, seq, true))
            })
            .collect();
        for r in db.relayed_changes_since(since, max_rows)? {
            let seq = r
                .seq
                .expect("relayed_changes_since serves sequenced rows only");
            page.push((seq, relayed_share_change(r, true)?));
        }
        page.sort_by_key(|(seq, _)| *seq);
        page.truncate(max_rows as usize);
        let mut out: Vec<LocalShareChange> = page.into_iter().map(|(_, c)| c).collect();
        // Pending (offline-authored, un-sequenced) rows — this replica's own
        // and the other writers' it relays — have no cursor position, so they
        // serve ONLY on the TAIL page — sequenced results short of max_rows —
        // capped at max_rows total (B2.5). A truncated pending set simply
        // reappears at the next tail: the puller's advancing `since` drains
        // the sequenced rows ahead of it, never the pending ones.
        let room = (max_rows as usize).saturating_sub(out.len());
        if room > 0 {
            // The own-pending wire form: `seq: 0` ("no nest coordinate"),
            // `sequenced: false`.
            for r in db.own_pending_changes(room as u32)? {
                out.push(own_share_change(r, 0, false));
            }
            let room = (max_rows as usize).saturating_sub(out.len());
            if room > 0 {
                for r in db.relayed_pending_changes(room as u32)? {
                    out.push(relayed_share_change(r, false)?);
                }
            }
        }
        Ok(out)
    }

    async fn manifest_bytes(
        &self,
        _set: &[u8; 32],
        _manifest_hash: &[u8; 32],
    ) -> anyhow::Result<Option<Vec<u8>>> {
        // The byte half lives with the app-side node assembly (module doc).
        Ok(None)
    }

    async fn chunk_body(
        &self,
        _set: &[u8; 32],
        _store_key: &[u8; 32],
    ) -> anyhow::Result<Option<Vec<u8>>> {
        // The byte half lives with the app-side node assembly (module doc).
        Ok(None)
    }
}

/// A stored cert blob (canonical embed-as-bytes) back to the wire form. A
/// blob that no longer decodes is dropped: the row then serves without it
/// and fails the chain on the receiver — never a page-wide error.
fn stored_cert(blob: Option<&[u8]>) -> Option<fauna_core::encoding::EmbedAsBytes> {
    blob.and_then(|b| fauna_protocol::decode_strict(b).ok())
}

fn own_share_change(r: crate::db::OwnChangeRow, seq: i64, sequenced: bool) -> LocalShareChange {
    LocalShareChange {
        change: crate::engine::wire_change(&r, seq),
        sequenced,
        locally_authored: true,
        signer_cert: stored_cert(r.signer_cert.as_deref()),
    }
}

fn relayed_share_change(
    r: crate::db::RelayedChangeRow,
    sequenced: bool,
) -> anyhow::Result<LocalShareChange> {
    let change: fauna_protocol::sync::SyncChange = fauna_protocol::decode_strict(&r.row)
        .map_err(|e| anyhow::anyhow!("relayed_change_log row does not decode: {e}"))?;
    Ok(LocalShareChange {
        change,
        sequenced,
        locally_authored: false,
        signer_cert: stored_cert(r.signer_cert.as_deref()),
    })
}

/// The cached roster as the reader's judge takes it
/// ([`fauna_peer_share::reader_over_cached_roster`]): the cached writers, each
/// with its proven predecessors nearest first — the shape one roster read
/// installs on the nest pull's reader, so the peer door judges over the same
/// thing offline — the read's owner row included, which a reader whose binding
/// names no owner judges by (ruling (11)(c)). An id that does not decode is
/// dropped (no writer, no link, no owner); a failed read answers the empty
/// roster — fail closed.
pub fn cached_writer_roster(db: &SyncDb) -> fauna_protocol::sync_row_verify::WriterRoster {
    let cached = db.cached_share_roster().unwrap_or_default();
    let id = |hex: &String| fauna_core::hex32::decode(hex).ok();
    fauna_protocol::sync_row_verify::WriterRoster {
        writers: cached.writers.iter().filter_map(id).collect(),
        predecessors: cached
            .chains
            .iter()
            .filter_map(|(writer, chain)| {
                Some((id(writer)?, chain.iter().filter_map(id).collect()))
            })
            .collect(),
        owner: cached.owner.as_ref().and_then(id),
    }
}

/// What one roster read did to the cached writer roster — the three arms of
/// the staleness posture (`p2p.md` § Peer-served change-row provenance).
#[derive(Debug, PartialEq, Eq)]
pub enum RosterRefresh {
    /// A successful read replaced the cache wholesale.
    Replaced { members: usize },
    /// The nest says the set is not shared — an unshared set has no peers,
    /// so the cache is cleared (a successful read of an empty roster, not a
    /// degrade).
    ClearedNotShared,
    /// The read failed; the stale cache stands. Denies fresh writers until
    /// reconnect (heals) and honors recently-demoted ones briefly — the
    /// ruling's stated posture, and why a failure must never clear.
    KeptStale,
}

/// One roster read → the cached writer roster, the refresh the ingest
/// consult depends on (`SyncDb::cached_share_writer`). Generic over the
/// requester so the three arms are tier_1-testable against a scripted nest;
/// production passes the engine's connected control-plane client.
///
/// The WRITER fact is `role == "owner"` (the owner has no grant — they own)
/// or `access == "writer"` (the multi-writer Phase 1 grant); everything else
/// — reader grant, absent grant (a non-conforming nest) — is a member whose
/// rows the ingest refuses.
pub async fn refresh_writer_roster_via<R>(db: &SyncDb, folder: &str, nest: R) -> RosterRefresh
where
    R: fauna_protocol::RpcRequester,
    R::Error: fauna_protocol::RpcErrorClass + core::fmt::Display,
{
    use fauna_protocol::RpcErrorClass;
    let client = fauna_client_folders::FoldersClient::new(nest);
    match client.actor_members_list(folder).await {
        Ok(reply) => {
            let members: Vec<(String, bool)> = reply
                .members
                .iter()
                .map(|m| {
                    (
                        m.actor_id.to_ascii_lowercase(),
                        fauna_protocol::sync_row_verify::roster_member_is_writer(
                            &m.role,
                            m.access.as_deref(),
                        ),
                    )
                })
                .collect();
            // Each writer's PROVEN predecessor ids beside the writer, nearest
            // first (`writer-signed-change-records.md` rulings (8)(e),
            // (11)(c)/(i)): the one statement walk (`writer_roster`), never a
            // nest-asserted list — and never a writer row of the
            // predecessor's own, under which a cut predecessor would be
            // admitted as a writer itself. The peer door's judge reads the
            // chain ([`cached_writer_roster`]) and places a retired
            // identity's row offline as the nest pull places it.
            let roster = fauna_protocol::sync_row_verify::writer_roster(&reply.members);
            let chains: Vec<(String, Vec<String>)> = roster
                .predecessors
                .iter()
                .map(|(writer, chain)| {
                    (hex::encode(writer), chain.iter().map(hex::encode).collect())
                })
                .collect();
            // The read's owner row, marked on its member row: the owner a
            // reader that holds no marker judges by, offline as online.
            let owner = roster.owner.as_ref().map(hex::encode);
            match db.cache_share_writer_roster(&members, &chains, owner.as_deref()) {
                Ok(()) => {
                    // The success path was the quietest of the three, and it
                    // is the one that decides whether ANY peer row is ever
                    // accepted: a roster that lists members but names no
                    // writer among them refuses everything, and reads from
                    // outside exactly like a roster that was never read.
                    tracing::debug!(
                        folder = %fauna_core::log_redact::log_folder_name(folder),
                        members = members.len(),
                        writers = members.iter().filter(|(_, w)| *w).count(),
                        "share writer roster: cached"
                    );
                    RosterRefresh::Replaced {
                        members: members.len(),
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "caching the writer roster failed; stale roster stands");
                    RosterRefresh::KeptStale
                }
            }
        }
        Err(e)
            if e.as_rpc_error()
                .is_some_and(|r| r.code == "fauna.folders.not_shared") =>
        {
            if let Err(e) = db.cache_share_writer_roster(&[], &[], None) {
                tracing::warn!(error = %e, "clearing the writer roster failed; stale roster stands");
                return RosterRefresh::KeptStale;
            }
            // Emptying the roster is the most consequential thing this
            // function does — every subsequent peer row is refused until a
            // later read repopulates it — and it happens on a nest answer
            // (`not_shared`) that a MEMBER asking about someone else's set
            // could plausibly receive. Say so.
            tracing::debug!(
                folder = %fauna_core::log_redact::log_folder_name(folder),
                "share writer roster: the nest answered not_shared; roster CLEARED"
            );
            RosterRefresh::ClearedNotShared
        }
        Err(e) => {
            tracing::debug!(error = %e, "roster read failed; stale writer roster stands");
            RosterRefresh::KeptStale
        }
    }
}

/// The materialization decision for one accepted provisional create/modify
/// (B2.3 — `p2p.md` § Build design → *Materialization commits like a
/// download*). Deliberately FAR more conservative than the nest apply's
/// licensed folds: the provisional plane holds no causal licence to overwrite
/// anything, so every ambiguous local state skips — the overlay row still
/// lands (unmaterialized) and the nest reconcile converges the path later.
#[derive(Debug, PartialEq, Eq)]
pub enum MaterializeVerdict {
    /// Fetch and write: the path is locally absent, or clean at its tracked
    /// state (overwriting a CLEAN synced file loses nothing this replica
    /// authored — the old version stays nest-fetchable).
    Write,
    /// The tracked head already IS the peer row's manifest — stamp the
    /// overlay materialized, fetch nothing.
    AlreadyCurrent,
    /// Leave disk and entry untouched (reason for the report). Covers every
    /// state where writing could destroy local novelty (modified /
    /// conflicted / mid-transfer / untracked-but-present / disk drifted from
    /// tracked state) and the placeholder rows whose absence of bytes is a
    /// storage-policy choice materialization must not override.
    Skip(&'static str),
}

/// What the judge sees of one local path. `disk` is the CURRENT on-disk
/// content hash (`None` = absent) — hashed at judge time, never trusted from
/// the entry.
pub struct LocalPathState {
    pub entry: Option<crate::db::SyncEntry>,
    pub disk: Option<fauna_core::data::ContentHash>,
}

/// Judge one provisional create/modify against the local path state. Pure —
/// every arm is tier_1-checkable without an engine.
pub fn judge_materialization(
    local: &LocalPathState,
    peer_manifest: &fauna_core::data::ContentHash,
) -> MaterializeVerdict {
    use crate::db::SyncState;
    match (&local.entry, &local.disk) {
        // Fresh path: nothing local to lose.
        (None, None) => MaterializeVerdict::Write,
        // Untracked bytes at the path — the scan has not accounted them, so
        // they may be user novelty. Never overwritten.
        (None, Some(_)) => MaterializeVerdict::Skip("untracked local file present"),
        (Some(entry), disk) => {
            // The tracked head already is this manifest: nothing to fetch.
            // (Placeholder rows included — their bytes stay absent by
            // storage policy; the overlay row records provisional-ness.)
            if entry.manifest_hash.as_ref() == Some(peer_manifest) {
                return MaterializeVerdict::AlreadyCurrent;
            }
            if entry.state == SyncState::Placeholder {
                // Materializing bytes for a dehydrated row would override
                // the storage-policy choice that dehydrated it.
                return MaterializeVerdict::Skip("placeholder — storage policy");
            }
            if entry.state != SyncState::Synced {
                return MaterializeVerdict::Skip("path not clean-synced locally");
            }
            match disk {
                // Clean at its tracked state: overwriting loses nothing this
                // replica authored.
                Some(d) if entry.local_hash.as_ref() == Some(d) => MaterializeVerdict::Write,
                // Disk drifted from the tracked state (an edit the scan has
                // not seen yet) — possible user novelty, never overwritten.
                Some(_) => MaterializeVerdict::Skip("disk differs from tracked state"),
                // Tracked-but-missing: a half-state the provisional plane has
                // no licence to interpret.
                None => MaterializeVerdict::Skip("tracked file missing on disk"),
            }
        }
    }
}

/// The serve-side byte half's chunk index — a bounded memo of recently served
/// manifests' chunk coordinates, so `chunk_body` can re-derive one chunk from
/// a plaintext **range** read (never a whole-file buffer).
///
/// Manifest-anchored by design: the shared download walk always fetches a
/// file's manifest before its chunks, so indexing at manifest-serve time
/// covers every honest puller; a chunk request for a manifest this store
/// never served (or one evicted past the bound) answers `None` — "this
/// replica cannot produce it", the multi-source degrade, never an error.
#[derive(Default)]
pub struct ShareServeMemo {
    /// FIFO of indexed manifests, newest last; evicted past [`Self::MAX`].
    entries: std::collections::VecDeque<MemoManifest>,
    /// FIFO of resealed chunk bodies, newest last; evicted once
    /// [`Self::CHUNK_BODY_CACHE_MAX_BYTES`] is exceeded. Keyed by store key —
    /// the ciphertext's own content hash — so a hit is safe forever: there is
    /// no staleness question for content-addressed data, only a memory bound
    /// on how much of it stays resident (`chunk_body_from_hit`'s
    /// read + compress + encrypt is the expensive half of serving a chunk,
    /// and `fauna-peer-share/server.rs`'s per-reply byte budget forces every
    /// chunk larger than that budget across several pull rounds; without this
    /// cache each round re-pays the full reseal for every still-incomplete
    /// chunk, not just the one it can afford to slice this round).
    chunk_bodies: std::collections::VecDeque<([u8; 32], Vec<u8>)>,
    chunk_body_cache_bytes: u64,
}

/// One indexed manifest: where its chunks live in the local plaintext file
/// and what each seals to.
pub struct MemoManifest {
    pub manifest_hash: [u8; 32],
    /// Relative path whose seal produced the manifest (the range source).
    pub path: String,
    /// The M2 generation the chunks sealed under.
    pub content_key_version: Option<u64>,
    /// store key → coordinates, one per sealed chunk.
    chunks: std::collections::HashMap<[u8; 32], ChunkCoords>,
}

/// One chunk's plaintext coordinates + integrity anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkCoords {
    pub offset: u64,
    pub len: u64,
    /// The manifest's recorded plaintext hash — checked against the range
    /// read before sealing, so a drifted file refuses early.
    pub plain_hash: fauna_core::data::ContentHash,
}

/// What `chunk_body` needs about one memoized chunk, resolved in one lock hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoChunkHit {
    pub path: String,
    pub content_key_version: Option<u64>,
    pub coords: ChunkCoords,
}

impl ShareServeMemo {
    /// Bound on indexed manifests. Small on purpose: a puller works one file
    /// at a time per channel, and an evicted manifest re-indexes on its next
    /// manifest fetch.
    const MAX: usize = 8;

    /// Bound on the resealed-chunk-body cache, by total bytes rather than
    /// count: a handful of typical chunks' worth, so a few large chunks and
    /// many small ones both stay within one fixed memory budget regardless
    /// of how a file happened to chunk.
    const CHUNK_BODY_CACHE_MAX_BYTES: u64 = 16 * 1024 * 1024;

    /// Index `manifest` (replacing any prior entry for the same hash), evicting
    /// the oldest entries past the bound.
    pub fn index(&mut self, manifest: MemoManifest) {
        self.entries
            .retain(|m| m.manifest_hash != manifest.manifest_hash);
        self.entries.push_back(manifest);
        while self.entries.len() > Self::MAX {
            self.entries.pop_front();
        }
    }

    /// A previously-resealed chunk body, if still cached. Safe to serve
    /// unconditionally: `store_key` is the ciphertext's own content hash, so
    /// a hit was, and still is, exactly the bytes a caller asking for that
    /// key wants — content-addressed data cannot go stale.
    fn cached_chunk_body(&self, store_key: &[u8; 32]) -> Option<Vec<u8>> {
        self.chunk_bodies
            .iter()
            .rev()
            .find(|(k, _)| k == store_key)
            .map(|(_, body)| body.clone())
    }

    /// Cache a freshly-resealed chunk body, evicting the oldest entries past
    /// [`Self::CHUNK_BODY_CACHE_MAX_BYTES`] (always keeping at least the one
    /// just inserted, even if it alone exceeds the budget).
    fn cache_chunk_body(&mut self, store_key: [u8; 32], body: Vec<u8>) {
        self.chunk_body_cache_bytes = self
            .chunk_body_cache_bytes
            .saturating_add(body.len() as u64);
        self.chunk_bodies.push_back((store_key, body));
        while self.chunk_body_cache_bytes > Self::CHUNK_BODY_CACHE_MAX_BYTES
            && self.chunk_bodies.len() > 1
        {
            if let Some((_, evicted)) = self.chunk_bodies.pop_front() {
                self.chunk_body_cache_bytes = self
                    .chunk_body_cache_bytes
                    .saturating_sub(evicted.len() as u64);
            }
        }
    }

    /// Resolve one store key against the indexed manifests, newest first.
    pub fn chunk(&self, store_key: &[u8; 32]) -> Option<MemoChunkHit> {
        self.entries.iter().rev().find_map(|m| {
            m.chunks.get(store_key).map(|c| MemoChunkHit {
                path: m.path.clone(),
                content_key_version: m.content_key_version,
                coords: c.clone(),
            })
        })
    }
}

/// Build a [`MemoManifest`] from a decoded [`fauna_core::chunk::ChunkManifest`].
///
/// `None` for a manifest with no `stored_hashes` — an unsealed manifest has no
/// ciphertext store keys, and a cross-user shared set's content is M2-sealed
/// by definition, so nothing servable is lost. Offsets are the prefix sums of
/// `chunk_sizes` (chunks are contiguous — `chunker::extract_chunks` is the
/// same walk).
pub fn memo_manifest_from(
    manifest_hash: [u8; 32],
    path: &str,
    content_key_version: Option<u64>,
    manifest: &fauna_core::chunk::ChunkManifest,
) -> Option<MemoManifest> {
    manifest.stored_hashes.as_ref()?;
    let chunks = crate::serve_core::held_chunks_of(manifest)?
        .into_iter()
        .map(|c| {
            (
                c.store_key,
                ChunkCoords {
                    offset: c.offset,
                    len: c.len,
                    plain_hash: c.plain_hash,
                },
            )
        })
        .collect();
    Some(MemoManifest {
        manifest_hash,
        path: path.to_string(),
        content_key_version,
        chunks,
    })
}

/// Re-derivation cap for a manifest with no retention row: past this, a file is
/// unservable offline until it re-records (stated, not hidden — the
/// build-record's named limit). 256 MiB, the in-memory upload path's own
/// working scale.
pub const SHARE_RESEAL_IN_MEMORY_MAX: u64 = 256 * 1024 * 1024;

/// Decode + index one served manifest into `memo` (best-effort — an
/// undecodable or unsealed manifest, or one no held generation of
/// `content_key_version` opens, simply leaves its chunks unservable). The
/// served bytes name no plaintext chunk hash (`ChunkManifest::sealed_hashes`),
/// and re-deriving a chunk needs it, so the manifest is opened under every
/// same-version candidate of `content_keys` first. Shared by the engine's
/// serve methods and [`ShareServeSource`].
pub fn index_share_manifest(
    memo: &Mutex<ShareServeMemo>,
    manifest_hash: [u8; 32],
    path: &str,
    content_key_version: Option<u64>,
    bytes: &[u8],
    content_keys: Option<&fauna_core::folder_keys::FolderContentKeys>,
) {
    let Ok(manifest) =
        fauna_core::encoding::canonical_decode::<fauna_core::chunk::ChunkManifest>(bytes)
    else {
        return;
    };
    let (Some(version), Some(keys)) = (content_key_version, content_keys) else {
        return;
    };
    let Some(opened) = keys
        .keys_for(version)
        .find_map(|root| manifest.clone().unseal_hashes(root).ok())
    else {
        return;
    };
    index_opened_share_manifest(memo, manifest_hash, path, content_key_version, &opened);
}

/// Index a manifest whose plaintext view is already in hand — a re-derived
/// seal's own [`fauna_core::blob_seal::SealedBlob::manifest`].
pub fn index_opened_share_manifest(
    memo: &Mutex<ShareServeMemo>,
    manifest_hash: [u8; 32],
    path: &str,
    content_key_version: Option<u64>,
    manifest: &fauna_core::chunk::ChunkManifest,
) {
    if let Some(m) = memo_manifest_from(manifest_hash, path, content_key_version, manifest) {
        memo.lock().expect("share serve memo poisoned").index(m);
    }
}

/// Re-derive one chunk's sealed body from the plaintext this replica holds,
/// read through `body` ([`crate::share_body::BodySource`]) — the engine's one
/// serve core ([`crate::serve_core::serve_held_chunk`]) — or serve it from `memo`'s
/// bounded cache when a prior call already paid that cost (a puller
/// re-asks for the SAME still-incomplete chunk on every pull round once its
/// sealed size exceeds one reply's byte budget, so without this cache the
/// read+reseal reran in full on every round for every chunk not yet served
/// that round — O(chunks × rounds) instead of O(chunks)). `Ok(None)` for
/// every honest refusal: no body on this replica (a placeholder), file
/// gone/shrunk, drifted from the manifest's anchor, or a re-derived body that does not reproduce `store_key` (a
/// cross-build seal difference) — never cached, so the next call re-checks.
/// Shared by the engine's [`SyncEngine::share_chunk_body`] and
/// [`ShareServeSource`].
///
/// [`SyncEngine::share_chunk_body`]: crate::engine::SyncEngine::share_chunk_body
pub async fn chunk_body_from_hit(
    body: &dyn crate::share_body::BodySource,
    memo: &Mutex<ShareServeMemo>,
    hit: &MemoChunkHit,
    secret: [u8; 32],
    store_key: &[u8; 32],
) -> anyhow::Result<Option<Vec<u8>>> {
    if let Some(cached) = memo
        .lock()
        .expect("share serve memo poisoned")
        .cached_chunk_body(store_key)
    {
        return Ok(Some(cached));
    }

    let held = crate::serve_core::HeldChunk {
        store_key: *store_key,
        offset: hit.coords.offset,
        len: hit.coords.len,
        plain_hash: hit.coords.plain_hash,
    };
    let Some(body) =
        crate::serve_core::serve_held_chunk(body, &hit.path, &held, Some(&secret)).await?
    else {
        return Ok(None);
    };
    memo.lock()
        .expect("share serve memo poisoned")
        .cache_chunk_body(*store_key, body.clone());
    Ok(Some(body))
}

/// Re-derive a manifest with no retention row (its best-effort write failed)
/// from one candidate file read through `body`, bounded by
/// [`SHARE_RESEAL_IN_MEMORY_MAX`]. `Ok(None)` when the file is gone (or a
/// placeholder), over the
/// cap (with the limit named), unreadable, or seals to a different manifest
/// (disk drifted from the recorded head).
pub async fn rederive_manifest_from_disk(
    body: &dyn crate::share_body::BodySource,
    path: &str,
    secret: [u8; 32],
    version: u64,
    target: &fauna_core::data::ContentHash,
) -> anyhow::Result<Option<crate::seal::SealedBlob>> {
    let size = match body.body_len(path).await {
        Some(size) if size <= SHARE_RESEAL_IN_MEMORY_MAX => size,
        Some(size) => {
            tracing::debug!(
                path = %fauna_core::log_redact::log_path(path),
                size,
                limit = SHARE_RESEAL_IN_MEMORY_MAX,
                "manifest re-derivation refused: file exceeds the in-memory cap"
            );
            return Ok(None);
        }
        None => return Ok(None),
    };
    let Some(bytes) = body.read_range(path, 0, size).await else {
        return Ok(None);
    };
    let sealed = crate::seal::seal_blob(&bytes, Some((secret, Some(version))))?;
    if sealed.manifest_hash != *target {
        return Ok(None); // disk drifted from the recorded head
    }
    Ok(Some(sealed))
}

/// The app-side per-set composite store — the "composite store there wraps
/// this one for rows and adds the byte half beside it" this module's doc
/// promised: [`SyncDbShareStore`] for rows, the shared byte-half functions
/// for manifests + chunks, over one SyncDb connection + the **app-held**
/// content keys.
///
/// Needs NO engine, deliberately: on an out-of-process-agent app (tui,
/// windows) serving runs in the app process against the agent's live state —
/// a cross-process WAL read, the sanctioned pattern
/// (`fauna_account_store::set_unrecorded_rels` documents it) — plus
/// read-only range reads through the set's body source (the bound tree, or an
/// on-demand replica's two roots). The one write is the
/// backfill retention, best-effort under the connection's busy timeout. An
/// in-process-engine app (linux) composes the same type over the same
/// triple; only who opened the DB differs.
pub struct ShareServeSource {
    rows: SyncDbShareStore,
    body: std::sync::Arc<dyn crate::share_body::BodySource>,
    content_keys: fauna_core::folder_keys::FolderContentKeys,
    memo: Mutex<ShareServeMemo>,
}

impl ShareServeSource {
    /// A source serving `set_id` from `db` (the set's state DB), `body`
    /// (where the replica's plaintext is read) and `content_keys` (the set's
    /// M2 generations — app custody).
    pub fn new(
        set_id: [u8; 32],
        db: SyncDb,
        body: std::sync::Arc<dyn crate::share_body::BodySource>,
        content_keys: fauna_core::folder_keys::FolderContentKeys,
    ) -> Self {
        Self {
            rows: SyncDbShareStore::new(set_id, db),
            body,
            content_keys,
            memo: Mutex::new(ShareServeMemo::default()),
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl ShareStore for ShareServeSource {
    async fn changes_since(
        &self,
        set: &[u8; 32],
        since: i64,
        max_rows: u32,
    ) -> anyhow::Result<Vec<LocalShareChange>> {
        self.rows.changes_since(set, since, max_rows).await
    }

    async fn manifest_bytes(
        &self,
        _set: &[u8; 32],
        manifest_hash: &[u8; 32],
    ) -> anyhow::Result<Option<Vec<u8>>> {
        // The e2e hold (compiled out of release): a transfer parked here is
        // provably mid-flight, which is what lets a journey cut it on a
        // state rather than a race.
        crate::share_serve_tally::wait_if_held().await?;
        let hex_hash = hex::encode(manifest_hash);
        let retained = {
            let db = self.rows.db.lock().expect("share-store db mutex poisoned");
            db.retained_manifest(&hex_hash)?
        };
        if let Some(retained) = retained {
            if fauna_core::data::ContentHash::of_raw(&retained.bytes).digest() == *manifest_hash {
                index_share_manifest(
                    &self.memo,
                    *manifest_hash,
                    &retained.path,
                    retained.content_key_version,
                    &retained.bytes,
                    Some(&self.content_keys),
                );
                crate::share_serve_tally::record_manifest_served(&retained.path);
                return Ok(Some(retained.bytes));
            }
            tracing::warn!(
                manifest = %hex_hash,
                "retained manifest bytes fail their own hash; ignoring the row"
            );
        }
        // No retention row (its best-effort write failed): re-derive from the
        // entry's recorded head under the RECORDED generation, then backfill
        // the retention.
        let target = fauna_core::data::ContentHash::from_digest_raw(*manifest_hash);
        let entries = {
            let db = self.rows.db.lock().expect("share-store db mutex poisoned");
            db.entries_by_manifest(&target)?
        };
        for entry in entries {
            let Some(version) = entry.content_key_version else {
                continue;
            };
            let Some(secret) = self.content_keys.key_for(version).copied() else {
                continue;
            };
            let Some(sealed) =
                rederive_manifest_from_disk(&*self.body, &entry.path, secret, version, &target)
                    .await?
            else {
                continue;
            };
            {
                let db = self.rows.db.lock().expect("share-store db mutex poisoned");
                if let Err(e) = db.retain_manifest(
                    &hex_hash,
                    &sealed.manifest_bytes,
                    &entry.path,
                    Some(version),
                ) {
                    tracing::warn!(error = %e, "backfill manifest retention failed");
                }
            }
            index_opened_share_manifest(
                &self.memo,
                *manifest_hash,
                &entry.path,
                Some(version),
                &sealed.manifest,
            );
            crate::share_serve_tally::record_manifest_served(&entry.path);
            return Ok(Some(sealed.manifest_bytes));
        }
        Ok(None)
    }

    async fn chunk_body(
        &self,
        _set: &[u8; 32],
        store_key: &[u8; 32],
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let Some(hit) = self
            .memo
            .lock()
            .expect("share serve memo poisoned")
            .chunk(store_key)
        else {
            return Ok(None);
        };
        let Some(version) = hit.content_key_version else {
            return Ok(None);
        };
        let Some(secret) = self.content_keys.key_for(version).copied() else {
            return Ok(None);
        };
        let body = chunk_body_from_hit(&*self.body, &self.memo, &hit, secret, store_key).await?;
        if body.is_some() {
            crate::share_serve_tally::record_chunk_served(&hit.path);
        }
        Ok(body)
    }

    /// The cheap half of [`Self::chunk_body`]'s answer — the memo lookup and
    /// key-material presence checks, with no file I/O and no reseal. `true`
    /// here is not an absolute guarantee `chunk_body` will succeed (the file
    /// could still have drifted since it was indexed), only that every check
    /// answerable without touching disk passed; a caller that only needs
    /// `false` vs `true` to decide `missing` vs `deferred` (this trait
    /// method's one caller, `handle_chunks_pull` under an exhausted reply
    /// budget) gets the fast path, and a rare drift is simply caught on the
    /// next round's real `chunk_body` call instead of this one.
    async fn chunk_exists(&self, _set: &[u8; 32], store_key: &[u8; 32]) -> anyhow::Result<bool> {
        let memo = self.memo.lock().expect("share serve memo poisoned");
        let Some(hit) = memo.chunk(store_key) else {
            return Ok(false);
        };
        let Some(version) = hit.content_key_version else {
            return Ok(false);
        };
        Ok(self.content_keys.key_for(version).is_some())
    }
}

/// The spool's manifest file for `hash` — one namer for the write side (the
/// app's pump) and the read side ([`SpoolFetcher`]), so the layout cannot
/// drift across the process boundary.
pub fn spool_manifest_path(
    dir: &std::path::Path,
    hash: &fauna_core::data::ContentHash,
) -> std::path::PathBuf {
    dir.join("manifests").join(hex::encode(hash.digest()))
}

/// The spool's chunk-body file for `store_key` — [`spool_manifest_path`]'s
/// chunk twin.
pub fn spool_chunk_path(
    dir: &std::path::Path,
    store_key: &fauna_core::data::ContentHash,
) -> std::path::PathBuf {
    dir.join("chunks").join(hex::encode(store_key.digest()))
}

/// A [`fauna_core::file_download::BlobFetcher`] over a caller-populated SPOOL
/// directory — the ingest side of the app↔agent handoff on
/// out-of-process-agent apps: the app (which owns the peer channel)
/// pre-fetches the bodies materialization will need into
/// `spool/manifests/<hex>` + `spool/chunks/<hex>`, and the agent's engine
/// ingests with this fetcher. A spool miss is an error, which the ingest door
/// already treats as a per-row skip — never the page — so an
/// under-provisioned spool degrades row by row, honestly.
pub struct SpoolFetcher {
    dir: std::path::PathBuf,
}

impl SpoolFetcher {
    pub fn new(dir: std::path::PathBuf) -> Self {
        Self { dir }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_core::file_download::BlobFetcher for SpoolFetcher {
    async fn fetch_manifest(
        &self,
        hash: &fauna_core::data::ContentHash,
    ) -> anyhow::Result<Vec<u8>> {
        let path = spool_manifest_path(&self.dir, hash);
        tokio::fs::read(&path)
            .await
            .map_err(|e| anyhow::anyhow!("spool manifest {}: {e}", hex::encode(hash.digest())))
    }

    async fn fetch_chunks(
        &self,
        store_keys: &[fauna_core::data::ContentHash],
        relative_path: &str,
    ) -> anyhow::Result<Vec<Vec<u8>>> {
        let mut out = Vec::with_capacity(store_keys.len());
        for key in store_keys {
            let path = spool_chunk_path(&self.dir, key);
            out.push(tokio::fs::read(&path).await.map_err(|e| {
                anyhow::anyhow!(
                    "spool chunk {} for {}: {e}",
                    hex::encode(key.digest()),
                    fauna_core::log_redact::log_path(relative_path)
                )
            })?);
        }
        Ok(out)
    }
}

/// One ingest pass's outcome, per accepted row — the pump's log line and the
/// tests' assertion surface.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PeerIngestReport {
    /// Rows the engine's own provenance judgement refused (the pump may have
    /// screened already; the engine never trusts that it did).
    pub refused: usize,
    /// Rows admitted on the channel's proof alone — the serving peer's own row
    /// of a class the signature check does not apply to.
    pub channel_proven: usize,
    /// Overlay rows landed or refreshed (deletes included).
    pub overlaid: usize,
    /// Provisional rows whose bytes were fetched and written.
    pub materialized: usize,
    /// Rows already current locally (stamped, nothing fetched).
    pub already_current: usize,
    /// `(path, reason)` — rows whose materialization was skipped; the
    /// overlay row still stands unmaterialized.
    pub skipped: Vec<(String, &'static str)>,
    /// The lowest seq of a SEQUENCED row whose bytes did not arrive (a spool
    /// miss or a failed fetch). A retry can land these rows, unlike the
    /// permanent skips above, so the pull cursor must stay below them or the
    /// peer never serves them again ([`crate::always_resident::ingest_share_page`]).
    pub retry_floor: Option<i64>,
    /// A wanted body was not landed because that would take the device's
    /// free space under the storage floor (an on-demand replica only).
    pub storage_limited: bool,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db::OwnChangeRow;

    const SET_A: [u8; 32] = [0xA7; 32];
    const SET_B: [u8; 32] = [0xB8; 32];
    const AUTHOR: &str = "a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4";

    fn retained(seq: i64) -> OwnChangeRow {
        OwnChangeRow {
            seq: Some(seq),
            path: format!("photos/v{seq}.mp4"),
            path_hash: "ab".repeat(32),
            path_sealed: Some(vec![0x5E; 4]),
            manifest_hash: Some("cd".repeat(32)),
            size_bytes: 9000,
            change_type: "create".to_string(),
            created_at: 1_700_000_000_000,
            content_key_version: Some(2),
            thumbnail_hash: None,
            derived_through: Some(11),
            is_resolution: Some(false),
            author_actor_id: AUTHOR.to_string(),
            device_id: "d2".repeat(16),
            signature: Some(vec![0x51; 64]),
            signer_key: Some(vec![0x4B; 32]),
            signer_cert: None,
        }
    }

    fn store_with_rows(rows: &[OwnChangeRow]) -> SyncDbShareStore {
        let db = SyncDb::open_in_memory().unwrap();
        for r in rows {
            db.retain_own_change(r).unwrap();
        }
        SyncDbShareStore::new(SET_A, db)
    }

    /// The served row IS the retained row — every wire field maps from the
    /// recorded one, and the two provenance facts are what retention
    /// guarantees by construction (sequenced, own-authored).
    #[tokio::test]
    async fn serves_the_recorded_row_not_a_synthesis() {
        let store = store_with_rows(&[retained(5)]);
        let got = store.changes_since(&SET_A, 0, 100).await.unwrap();
        assert_eq!(got.len(), 1);
        let row = &got[0];
        assert!(row.sequenced, "funnel-retained rows are nest-sequenced");
        assert!(row.locally_authored, "own-authored by construction");
        assert_eq!(row.change.seq, 5);
        assert_eq!(row.change.path.as_deref(), Some("photos/v5.mp4"));
        assert_eq!(row.change.path_hash, "ab".repeat(32));
        assert_eq!(row.change.manifest_hash.as_deref(), Some(&*"cd".repeat(32)));
        assert_eq!(row.change.author_actor_id.as_deref(), Some(AUTHOR));
        assert_eq!(row.change.content_key_version, Some(2));
        assert_eq!(row.change.derived_through, Some(11));
        assert_eq!(
            row.change.path_sealed.as_deref().map(|b| b.to_vec()),
            Some(vec![0x5E; 4]),
            "the sealed sibling travels verbatim"
        );
        assert_eq!(
            row.change.signature.as_deref().map(|b| b.to_vec()),
            Some(vec![0x51; 64]),
            "the writer signature made at retention travels with the row"
        );
        assert_eq!(
            row.change.signer_key.as_deref().map(|b| b.to_vec()),
            Some(vec![0x4B; 32])
        );
    }

    /// Relayed rows (other writers', verified by this replica's reader) merge
    /// into the SAME seq-ordered page as own rows — one seq space per set —
    /// byte-exact, with their inline cert, and their pending rows join the
    /// tail after this replica's own.
    #[tokio::test]
    async fn relayed_rows_merge_into_the_page_by_seq_and_join_the_tail() {
        let db = SyncDb::open_in_memory().unwrap();
        db.retain_own_change(&retained(2)).unwrap();
        db.retain_own_change(&retained(6)).unwrap();
        db.mint_pending_own_change(&pending("own.txt")).unwrap();
        let cert = fauna_core::encoding::EmbedAsBytes {
            envelope: vec![1; 100],
            bytes: vec![2; 8],
            signer_auth: None,
        };
        let relayed = |seq: i64| fauna_protocol::sync::SyncChange {
            seq,
            path_hash: "ef".repeat(32),
            author_actor_id: Some("b7".repeat(32)),
            signature: Some(fauna_protocol::ByteBuf::from(vec![0x52; 64])),
            signer_key: Some(fauna_protocol::ByteBuf::from(vec![0x4C; 32])),
            ..Default::default()
        };
        for (seq, change) in [(Some(4), relayed(4)), (None, relayed(0))] {
            db.retain_relayed_change(&crate::db::RelayedChangeRow {
                seq,
                author_actor_id: "b7".repeat(32),
                path_hash: "ef".repeat(32),
                row: fauna_protocol::encode_canonical(&change).unwrap().to_vec(),
                signer_cert: Some(fauna_protocol::encode_canonical(&cert).unwrap().to_vec()),
            })
            .unwrap();
        }
        let store = SyncDbShareStore::new(SET_A, db);

        let got = store.changes_since(&SET_A, 0, 10).await.unwrap();
        let shape: Vec<(i64, bool, bool)> = got
            .iter()
            .map(|r| (r.change.seq, r.sequenced, r.locally_authored))
            .collect();
        assert_eq!(
            shape,
            vec![
                (2, true, true),
                (4, true, false),
                (6, true, true),
                (0, false, true),
                (0, false, false),
            ]
        );
        assert_eq!(got[1].change, relayed(4), "relayed byte-exact");
        assert_eq!(got[1].signer_cert, Some(cert));

        let bounded: Vec<i64> = store
            .changes_since(&SET_A, 2, 1)
            .await
            .unwrap()
            .iter()
            .map(|r| r.change.seq)
            .collect();
        assert_eq!(
            bounded,
            vec![4],
            "the merged page honours since and max_rows"
        );
    }

    /// `since`/`max_rows` fall straight through to the retention read — the
    /// page contract the crate's server relies on.
    #[tokio::test]
    async fn pages_by_seq_with_a_strict_since() {
        let store = store_with_rows(&[retained(2), retained(4), retained(6)]);
        let seqs: Vec<i64> = store
            .changes_since(&SET_A, 2, 1)
            .await
            .unwrap()
            .iter()
            .map(|r| r.change.seq)
            .collect();
        assert_eq!(seqs, vec![4], "strictly-after since, bounded by max_rows");
    }

    /// A request for any other set is a mis-wired assembly and fails loud —
    /// an empty page would make the bug read as "no changes".
    #[tokio::test]
    async fn a_foreign_set_is_a_loud_error_never_an_empty_page() {
        let store = store_with_rows(&[retained(1)]);
        let err = store.changes_since(&SET_B, 0, 10).await.unwrap_err();
        assert!(
            err.to_string().contains("mis-wired share store"),
            "got: {err}"
        );
    }

    // ---- the own-pending tail (B2.5) ----

    fn pending(path: &str) -> OwnChangeRow {
        OwnChangeRow {
            seq: None,
            path: path.to_string(),
            thumbnail_hash: None,
            ..retained(0)
        }
    }

    fn store_with(sequenced: &[OwnChangeRow], pending_rows: &[OwnChangeRow]) -> SyncDbShareStore {
        let db = SyncDb::open_in_memory().unwrap();
        for r in sequenced {
            db.retain_own_change(r).unwrap();
        }
        for r in pending_rows {
            db.mint_pending_own_change(r).unwrap();
        }
        SyncDbShareStore::new(SET_A, db)
    }

    /// A pending row serves as `seq: 0`, `sequenced: false`, carrying its
    /// writer and the signature made at mint — the signed actor, so the row
    /// verifies wherever it is relayed — and the crate's own serve filter
    /// admits it.
    #[tokio::test]
    async fn a_pending_row_serves_signed_and_unsequenced_at_the_tail() {
        let store = store_with(&[retained(3)], &[pending("cabin/draft.txt")]);
        let got = store.changes_since(&SET_A, 0, 10).await.unwrap();
        assert_eq!(got.len(), 2, "the sequenced row, then the pending tail");
        let p = &got[1];
        assert!(!p.sequenced);
        assert!(p.locally_authored);
        assert_eq!(p.change.seq, 0, "no nest coordinate");
        assert_eq!(
            p.change.author_actor_id.as_deref(),
            Some(AUTHOR),
            "the signed actor travels — the statement names it"
        );
        assert!(p.change.signature.is_some());
        assert_eq!(p.change.path.as_deref(), Some("cabin/draft.txt"));
        assert!(
            fauna_peer_share::provenance::serves_held_row(p, AUTHOR),
            "the serve filter admits the own-pending shape"
        );
    }

    /// Pending rows appear ONLY on the tail page: a full sequenced page has
    /// no room, and the next page (since advanced past the last seq) serves
    /// the whole pending set again — capped at max_rows total.
    #[tokio::test]
    async fn pending_rows_serve_only_on_the_tail_page() {
        let store = store_with(
            &[retained(1), retained(2)],
            &[pending("a.txt"), pending("b.txt"), pending("c.txt")],
        );

        // Full page of sequenced rows — no pending.
        let full = store.changes_since(&SET_A, 0, 2).await.unwrap();
        assert_eq!(full.len(), 2);
        assert!(full.iter().all(|r| r.sequenced), "no pending off the tail");

        // Tail page with partial room — pending fills up to max_rows total.
        let tail = store.changes_since(&SET_A, 1, 2).await.unwrap();
        assert_eq!(tail.len(), 2, "capped at max_rows total");
        assert!(tail[0].sequenced);
        assert!(
            !tail[1].sequenced,
            "one pending row fits the remaining room"
        );

        // The next tail (since past every seq) re-serves the whole pending set.
        let next = store.changes_since(&SET_A, 2, 10).await.unwrap();
        assert_eq!(next.len(), 3, "a truncated pending set reappears in full");
        assert!(next.iter().all(|r| !r.sequenced));
    }

    // ---- the cached writer roster (B2.2) ----

    use fauna_protocol::folders::{ActorMembersListReply, FolderActorMember};

    /// The scripted nest: one arm, `fauna.folders.members.list_actors`,
    /// answering the scripted result.
    pub(crate) struct ScriptedRoster(pub(crate) Result<ActorMembersListReply, &'static str>);

    #[derive(Debug)]
    pub(crate) struct ScriptedErr {
        code: String,
        rpc: Option<fauna_protocol::RpcError>,
    }
    impl core::fmt::Display for ScriptedErr {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "{}", self.code)
        }
    }
    impl fauna_protocol::RpcErrorClass for ScriptedErr {
        fn is_rejection(&self) -> bool {
            self.rpc.is_some()
        }
        fn as_rpc_error(&self) -> Option<&fauna_protocol::RpcError> {
            self.rpc.as_ref()
        }
    }

    impl fauna_protocol::RpcRequester for ScriptedRoster {
        type Error = ScriptedErr;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            assert_eq!(kind, "fauna.folders.members.list_actors");
            match &self.0 {
                Ok(reply) => {
                    let bytes = fauna_protocol::encode_canonical(reply).expect("encode");
                    Ok(fauna_protocol::decode_strict(&bytes).expect("decode"))
                }
                Err(code) => Err(ScriptedErr {
                    code: (*code).to_string(),
                    // A "transport" script (empty code) carries no RpcError.
                    rpc: (!code.is_empty()).then(|| {
                        fauna_protocol::RpcError::new((*code).to_string(), "error.test".to_string())
                    }),
                }),
            }
        }
    }

    pub(crate) fn member(actor: &str, role: &str, access: Option<&str>) -> FolderActorMember {
        FolderActorMember {
            actor_id: actor.to_string(),
            handle: String::new(),
            role: role.to_string(),
            access: access.map(str::to_string),
            succession_statements: Vec::new(),
            byte_cap: None,
            bytes_used: None,
            remote: None,
            extra: Default::default(),
        }
    }

    /// One carried succession statement `old` → `new`, its `new_sig` made by
    /// `new_signer` — the successor itself for a link that proves, anyone
    /// else for one that does not.
    pub(crate) fn succession_link(
        old: &fauna_core::identity::ActorKeypair,
        new: &fauna_core::identity::ActorKeypair,
        new_signer: &fauna_core::identity::ActorKeypair,
    ) -> fauna_protocol::ByteBuf {
        let recovery = fauna_core::recovery::RecoveryKey::generate();
        let signed = fauna_core::recovery::IdentitySuccession {
            old_actor_id: old.actor_id(),
            new_actor_id: new.actor_id(),
            recovery_pubkey: recovery.public(),
            seq: 2,
            created_at: fauna_core::data::Timestamp(0),
        }
        .sign(&recovery, new_signer.signing_key(), None)
        .expect("sign the succession");
        fauna_protocol::ByteBuf::from(fauna_core::encoding::canonical_encode(&signed).unwrap())
    }

    pub(crate) fn roster_reply(members: Vec<FolderActorMember>) -> ActorMembersListReply {
        ActorMembersListReply {
            members,
            ..Default::default()
        }
    }

    /// The writer fact reduces exactly as the design rules it: the owner and
    /// the "writer"-granted member answer true; a reader, an ungranted
    /// member (a non-conforming nest), and an actor absent from the roster answer false —
    /// fail-closed in every direction.
    #[tokio::test]
    async fn a_successful_roster_read_caches_the_writer_facts() {
        let db = SyncDb::open_in_memory().unwrap();
        let owner = "AA".repeat(32); // mixed case on purpose — normalized on write
        let writer = "bb".repeat(32);
        let reader = "cc".repeat(32);
        let ungranted = "dd".repeat(32);

        let refresh = refresh_writer_roster_via(
            &db,
            "photos",
            ScriptedRoster(Ok(roster_reply(vec![
                member(&owner, "owner", None),
                member(&writer, "member", Some("writer")),
                member(&reader, "member", Some("reader")),
                member(&ungranted, "member", None),
            ]))),
        )
        .await;
        assert_eq!(refresh, RosterRefresh::Replaced { members: 4 });

        assert!(db.cached_share_writer(&owner.to_lowercase()).unwrap());
        assert!(
            db.cached_share_writer(&owner).unwrap(),
            "the consult is case-insensitive (normalized on write and read)"
        );
        assert!(db.cached_share_writer(&writer).unwrap());
        assert!(!db.cached_share_writer(&reader).unwrap());
        assert!(!db.cached_share_writer(&ungranted).unwrap());
        assert!(
            !db.cached_share_writer(&"ee".repeat(32)).unwrap(),
            "an actor absent from the roster is fail-closed false"
        );
    }

    /// Ruling (8)(e), the p2p clause: the cached roster stores each WRITER's
    /// proven predecessor ids beside the writer, so a peer-served row signed
    /// under a writer's retired identity is admitted offline as the nest pull
    /// admits it — and only a chain that verifies counts: a reader's chain,
    /// or a link the successor did not sign, proves nothing.
    #[tokio::test]
    async fn the_cached_roster_carries_each_writers_proven_predecessors() {
        use fauna_core::identity::ActorKeypair;
        let link = succession_link;
        let kp = |b: u8| ActorKeypair::from_secret([b; 32]);
        let (writer, writers_old, reader, readers_old, forged_old, stranger) =
            (kp(1), kp(2), kp(3), kp(4), kp(5), kp(6));
        let db = SyncDb::open_in_memory().unwrap();
        let mut writer_row = member(&writer.actor_id().to_hex(), "member", Some("writer"));
        writer_row.succession_statements = vec![link(&writers_old, &writer, &writer)];
        let mut reader_row = member(&reader.actor_id().to_hex(), "member", Some("reader"));
        reader_row.succession_statements = vec![link(&readers_old, &reader, &reader)];
        let mut forged_row = member(&stranger.actor_id().to_hex(), "member", Some("writer"));
        forged_row.succession_statements = vec![link(&forged_old, &stranger, &writer)];
        refresh_writer_roster_via(
            &db,
            "photos",
            ScriptedRoster(Ok(roster_reply(vec![writer_row, reader_row, forged_row]))),
        )
        .await;
        let cached = cached_writer_roster(&db);
        assert_eq!(
            cached.predecessors,
            [(writer.actor_id().0, vec![writers_old.actor_id().0])]
                .into_iter()
                .collect(),
            "the writer's proven predecessor, beside the writer; a reader's chain \
             and a link the successor did not sign prove nothing"
        );
        assert_eq!(
            cached.writers,
            [writer.actor_id().0, stranger.actor_id().0]
                .into_iter()
                .collect(),
        );
        // Beside the writer, never a writer row of its own (ruling (11)(i)):
        // flattened, a cut predecessor would be admitted as a writer itself.
        for old in [&writers_old, &readers_old, &forged_old] {
            assert!(!db.cached_share_writer(&old.actor_id().to_hex()).unwrap());
        }
    }

    /// The chain keeps its order — nearest predecessor first — across the
    /// store (ruling (11)(c): which of two identities is the earlier is a
    /// question only an ordered chain answers), and a later read replaces it
    /// with the members.
    #[tokio::test]
    async fn the_cached_chain_is_ordered_nearest_first_and_replaced_wholesale() {
        use fauna_core::identity::ActorKeypair;
        let kp = |b: u8| ActorKeypair::from_secret([b; 32]);
        let (owner, older, oldest) = (kp(1), kp(2), kp(3));
        let db = SyncDb::open_in_memory().unwrap();
        let mut owner_row = member(&owner.actor_id().to_hex(), "owner", None);
        owner_row.succession_statements = vec![
            succession_link(&oldest, &older, &older),
            succession_link(&older, &owner, &owner),
        ];
        refresh_writer_roster_via(
            &db,
            "photos",
            ScriptedRoster(Ok(roster_reply(vec![owner_row]))),
        )
        .await;
        assert_eq!(
            cached_writer_roster(&db)
                .predecessors
                .get(&owner.actor_id().0),
            Some(&vec![older.actor_id().0, oldest.actor_id().0])
        );

        refresh_writer_roster_via(
            &db,
            "photos",
            ScriptedRoster(Ok(roster_reply(vec![member(
                &owner.actor_id().to_hex(),
                "owner",
                None,
            )]))),
        )
        .await;
        assert!(cached_writer_roster(&db).predecessors.is_empty());
    }

    /// A later read replaces wholesale: a demoted/departed writer drops to
    /// false by absence, never lingering from the previous roster.
    #[tokio::test]
    async fn a_later_read_replaces_the_roster_wholesale() {
        let db = SyncDb::open_in_memory().unwrap();
        let old_writer = "bb".repeat(32);
        let new_writer = "cc".repeat(32);

        refresh_writer_roster_via(
            &db,
            "photos",
            ScriptedRoster(Ok(roster_reply(vec![member(
                &old_writer,
                "member",
                Some("writer"),
            )]))),
        )
        .await;
        assert!(db.cached_share_writer(&old_writer).unwrap());

        refresh_writer_roster_via(
            &db,
            "photos",
            ScriptedRoster(Ok(roster_reply(vec![member(
                &new_writer,
                "member",
                Some("writer"),
            )]))),
        )
        .await;
        assert!(!db.cached_share_writer(&old_writer).unwrap());
        assert!(db.cached_share_writer(&new_writer).unwrap());
    }

    // ---- the materialization judge (B2.3) ----

    use crate::db::SyncState;
    use fauna_core::data::ContentHash;

    fn h(byte: u8) -> ContentHash {
        ContentHash::of_raw(&[byte])
    }

    /// A `SyncEntry` built through the real store (state serialization
    /// included), so the judge sees exactly what the ingest path hands it.
    fn entry_with(
        state: SyncState,
        local: Option<ContentHash>,
        manifest: Option<ContentHash>,
    ) -> crate::db::SyncEntry {
        let db = SyncDb::open_in_memory().unwrap();
        db.upsert_entry("p", local, None, manifest, state, 0, 0, 1, 1, None)
            .unwrap();
        db.get_entry("p").unwrap().unwrap()
    }

    /// Every arm of the judge, pinned: the provisional plane writes only
    /// where nothing local can be lost, and skips every ambiguous state.
    #[test]
    fn the_judge_writes_only_where_nothing_local_can_be_lost() {
        let peer_m = h(0xAA);
        let judge = judge_materialization;

        // Fresh path: write.
        assert_eq!(
            judge(
                &LocalPathState {
                    entry: None,
                    disk: None
                },
                &peer_m
            ),
            MaterializeVerdict::Write
        );
        // Untracked bytes present: possible user novelty — never overwritten.
        assert!(matches!(
            judge(
                &LocalPathState {
                    entry: None,
                    disk: Some(h(1))
                },
                &peer_m
            ),
            MaterializeVerdict::Skip(_)
        ));
        // Tracked head already IS the peer manifest: nothing to fetch.
        assert_eq!(
            judge(
                &LocalPathState {
                    entry: Some(entry_with(SyncState::Synced, Some(h(1)), Some(peer_m))),
                    disk: Some(h(1))
                },
                &peer_m
            ),
            MaterializeVerdict::AlreadyCurrent
        );
        // Placeholder: absent bytes are a storage-policy choice.
        assert!(matches!(
            judge(
                &LocalPathState {
                    entry: Some(entry_with(SyncState::Placeholder, None, Some(h(2)))),
                    disk: None
                },
                &peer_m
            ),
            MaterializeVerdict::Skip("placeholder — storage policy")
        ));
        // Clean at tracked state: overwriting loses nothing this replica
        // authored — write.
        assert_eq!(
            judge(
                &LocalPathState {
                    entry: Some(entry_with(SyncState::Synced, Some(h(1)), Some(h(2)))),
                    disk: Some(h(1))
                },
                &peer_m
            ),
            MaterializeVerdict::Write
        );
        // Locally modified / conflicted / mid-transfer: never.
        for state in [
            SyncState::LocallyModified,
            SyncState::Conflicted,
            SyncState::Uploading,
            SyncState::Downloading,
        ] {
            assert!(
                matches!(
                    judge(
                        &LocalPathState {
                            entry: Some(entry_with(state, Some(h(1)), Some(h(2)))),
                            disk: Some(h(1))
                        },
                        &peer_m
                    ),
                    MaterializeVerdict::Skip("path not clean-synced locally")
                ),
                "state {state:?} must skip"
            );
        }
        // Disk drifted from the tracked state (unscanned edit): never.
        assert!(matches!(
            judge(
                &LocalPathState {
                    entry: Some(entry_with(SyncState::Synced, Some(h(1)), Some(h(2)))),
                    disk: Some(h(3))
                },
                &peer_m
            ),
            MaterializeVerdict::Skip("disk differs from tracked state")
        ));
        // Tracked but missing on disk: a half-state — never interpreted.
        assert!(matches!(
            judge(
                &LocalPathState {
                    entry: Some(entry_with(SyncState::Synced, Some(h(1)), Some(h(2)))),
                    disk: None
                },
                &peer_m
            ),
            MaterializeVerdict::Skip("tracked file missing on disk")
        ));
    }

    // ---- the serve memo (slice E byte half) ----

    fn hand_manifest(sealed: bool) -> fauna_core::chunk::ChunkManifest {
        fauna_core::chunk::ChunkManifest {
            file_hash: ContentHash::of_raw(b"file"),
            total_size: 5 + 7 + 11,
            chunk_hashes: vec![h(1), h(2), h(3)],
            chunk_sizes: vec![5, 7, 11],
            stored_hashes: sealed.then(|| vec![h(0x51), h(0x52), h(0x53)]),
            sealed_hashes: None,
            min_reader: None,
        }
    }

    /// Offsets are the prefix sums of `chunk_sizes`, and resolution is by the
    /// STORE key (the ciphertext hash a puller asks by), carrying the
    /// PLAINTEXT hash as the range read's integrity anchor.
    #[test]
    fn memo_offsets_are_prefix_sums_keyed_by_store_key() {
        let m = memo_manifest_from([0xAA; 32], "a/b.mp4", Some(4), &hand_manifest(true))
            .expect("a sealed manifest indexes");
        let mut memo = ShareServeMemo::default();
        memo.index(m);

        let second = memo.chunk(&h(0x52).digest()).expect("resolves");
        assert_eq!(second.path, "a/b.mp4");
        assert_eq!(second.content_key_version, Some(4));
        assert_eq!(second.coords.offset, 5);
        assert_eq!(second.coords.len, 7);
        assert_eq!(second.coords.plain_hash, h(2));
        assert_eq!(memo.chunk(&h(0x53).digest()).unwrap().coords.offset, 12);
        assert_eq!(
            memo.chunk(&h(2).digest()),
            None,
            "plaintext hash is not a key"
        );
    }

    /// An unsealed manifest has no ciphertext store keys — nothing to index,
    /// and nothing servable is lost (a shared set's content is M2-sealed by
    /// definition).
    #[test]
    fn an_unsealed_manifest_indexes_nothing() {
        assert!(memo_manifest_from([1; 32], "p", None, &hand_manifest(false)).is_none());
    }

    /// The memo is bounded FIFO: past the cap the oldest manifest's chunks
    /// stop resolving (its next manifest fetch re-indexes it).
    #[test]
    fn the_memo_evicts_oldest_past_the_bound() {
        let mut memo = ShareServeMemo::default();
        for i in 0..=ShareServeMemo::MAX {
            let mut m = hand_manifest(true);
            // Distinct store keys per manifest so entries don't shadow.
            m.stored_hashes = Some(vec![
                h(0x60 + i as u8),
                h(0x70 + i as u8),
                h(0x80 + i as u8),
            ]);
            memo.index(memo_manifest_from([i as u8; 32], &format!("f{i}"), Some(1), &m).unwrap());
        }
        assert_eq!(
            memo.chunk(&h(0x60).digest()),
            None,
            "the first manifest evicted"
        );
        assert!(memo.chunk(&h(0x61).digest()).is_some(), "the second stands");
    }

    // ---- the app-side composite (ShareServeSource) ----

    /// The composite serves rows AND bytes with NO engine anywhere, through a
    /// SECOND connection onto a state DB another connection wrote — the
    /// out-of-process-agent shape (the agent's engine writes retention; the
    /// app process serves it cross-process over WAL).
    #[tokio::test]
    async fn the_composite_source_serves_rows_and_bytes_with_no_engine() {
        const KEY: [u8; 32] = [0x3C; 32];
        let dir = tempfile::tempdir().unwrap();
        let bytes: Vec<u8> = (0..40_000u32).map(|i| (i % 241) as u8).collect();
        std::fs::write(dir.path().join("clip.mp4"), &bytes).unwrap();
        let sealed = crate::seal::seal_blob(&bytes, Some((KEY, Some(1)))).unwrap();

        let db_path = dir.path().join("fs-state.db");
        // Connection A — "the agent": retains the row + the manifest and STAYS
        // OPEN, as a live agent's would.
        let writer = SyncDb::open(&db_path).unwrap();
        writer
            .retain_own_change(&OwnChangeRow {
                manifest_hash: Some(hex::encode(sealed.manifest_hash.digest())),
                ..retained(7)
            })
            .unwrap();
        writer
            .retain_manifest(
                &hex::encode(sealed.manifest_hash.digest()),
                &sealed.manifest_bytes,
                "clip.mp4",
                Some(1),
            )
            .unwrap();

        // Connection B — "the app": the composite source.
        let source = ShareServeSource::new(
            SET_A,
            SyncDb::open(&db_path).unwrap(),
            std::sync::Arc::new(crate::share_body::TreeBodySource::new(
                dir.path().to_path_buf(),
            )),
            fauna_core::folder_keys::FolderContentKeys::genesis(KEY, 1_760_000_000),
        );

        let rows = source.changes_since(&SET_A, 0, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].sequenced && rows[0].locally_authored);

        let manifest = source
            .manifest_bytes(&SET_A, &sealed.manifest_hash.digest())
            .await
            .unwrap()
            .expect("manifest serves cross-connection");
        assert_eq!(manifest, sealed.manifest_bytes);
        for (store_key, body) in &sealed.chunks {
            let got = source
                .chunk_body(&SET_A, &store_key.digest())
                .await
                .unwrap()
                .expect("chunk serves");
            assert_eq!(&got, body);
        }
        drop(writer);
    }

    /// The two error arms of the staleness posture: `not_shared` is a
    /// successful "no peers" answer and CLEARS; any other failure KEEPS the
    /// stale roster (clearing on a transport blip would flip every cached
    /// writer to refused exactly when offline serving matters).
    #[tokio::test]
    async fn not_shared_clears_but_a_failed_read_keeps_the_stale_roster() {
        let db = SyncDb::open_in_memory().unwrap();
        let writer = "bb".repeat(32);
        refresh_writer_roster_via(
            &db,
            "photos",
            ScriptedRoster(Ok(roster_reply(vec![member(
                &writer,
                "member",
                Some("writer"),
            )]))),
        )
        .await;

        // A failed read (transport-shaped: no RpcError) keeps the cache.
        let refresh = refresh_writer_roster_via(&db, "photos", ScriptedRoster(Err(""))).await;
        assert_eq!(refresh, RosterRefresh::KeptStale);
        assert!(
            db.cached_share_writer(&writer).unwrap(),
            "a failed read must not clear the stale roster"
        );

        // A wire-level refusal that is NOT not_shared also keeps it.
        let refresh =
            refresh_writer_roster_via(&db, "photos", ScriptedRoster(Err("fauna.rpc.denied"))).await;
        assert_eq!(refresh, RosterRefresh::KeptStale);
        assert!(db.cached_share_writer(&writer).unwrap());

        // not_shared is an authoritative empty roster — cleared.
        let refresh = refresh_writer_roster_via(
            &db,
            "photos",
            ScriptedRoster(Err("fauna.folders.not_shared")),
        )
        .await;
        assert_eq!(refresh, RosterRefresh::ClearedNotShared);
        assert!(!db.cached_share_writer(&writer).unwrap());
    }
}
