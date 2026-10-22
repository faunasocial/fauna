//! **The store-serving half — one implementation, two transports.**
//!
//! A replica that holds an account's planes in an [`AccountStore`] serves them
//! onward from here: a `fauna.sync.changes.list` page from the relay plane by
//! the dialer's own frontier (both item-class arms), and the adopted segment
//! pairs the bulk plane names. Two doors call it:
//!
//! - the **peer channel** ([`crate::server::PeerSyncServer`]) — a device
//!   serving its own account's store and every custodied store it holds;
//! - the **custodian-NEST serve door** (`fauna-nest`'s `custody_fleet`) — a
//!   nest handing a held copy back to its owner's fleet over the nest wire
//!   (`account-replica-posture.md` § Replica posture → *The custody grant +
//!   ceremony*, the custodian-NEST serve door bullet, ruling 1: "lifted to a
//!   transport-agnostic shared function that the peer channel and this door
//!   both call, so the two serve sides cannot drift").
//!
//! **Admission is never this module's job.** Each door resolves its own
//! verdict between [`changes_list_scope`] (which names the scope the request
//! asks about) and [`serve_changes_list`] (which answers it); this module only
//! answers from the store it is handed. Refusals come back typed
//! ([`ServeRefusal`]) so each transport keeps its own error vocabulary.
//!
//! # The store owner thread
//!
//! Handlers run on spawned tasks, which demand `Send` futures — and the
//! store's sqlite backend is single-connection by design. So a serving store
//! is owned by a **dedicated OS thread** (WAL + busy-wait is the store's own
//! multi-connection posture — `SqliteBackend::open`'s contract), and handlers
//! proxy through [`ServeStoreHandle`]'s message channel. This also keeps every
//! store touch on one thread — the engine-singleton shape (§ Multi-instance
//! concurrency) applied to the serve side.

use std::path::PathBuf;

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::segments::SegmentSidecarView;
use fauna_account_store::sqlite::SqliteBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::{RelayRow, WriterId};
use fauna_core::data::ContentHash;
use fauna_protocol::account_state::{ACCOUNT_STATE_SCOPE, ItemClass};
use fauna_protocol::segments::{SegmentRef, SegmentsListReply};
use fauna_protocol::sync::{SyncChange, SyncChangesListReply, SyncChangesListRequest};
use tokio::sync::{mpsc, oneshot};

/// Serve-page bounds, sized under the peer channel's 1 MiB frame
/// (`fauna_peer_channel::MAX_FRAME_LEN`) — and so under the nest's WS frame
/// budget too: a page stops at whichever cap it hits first. Rust constants —
/// never a knob.
const MAX_ROWS_PER_PAGE: u32 = 64;
const MAX_ENTRY_BYTES_PER_PAGE: usize = 512 * 1024;

// ── The store owner thread ───────────────────────────────────────────────────

/// One block lookup the store thread answers: the block's index scope (for
/// the verdict's scope enforcement) and its bytes when held.
pub(crate) struct BlockLookup {
    pub(crate) cid: ContentHash,
    pub(crate) scope: Option<String>,
    pub(crate) bytes: Option<Vec<u8>>,
}

/// One adopted segment whose container this store still holds, as the store
/// thread reports it: the verbatim sidecar and where the `.dat` rests.
struct HeldSegment {
    segment_id: u32,
    meta: Vec<u8>,
    dat_path: PathBuf,
}

enum StoreCmd {
    RelayPage {
        scope: String,
        item_class: String,
        frontier: Vec<(WriterId, u64)>,
        limit: u32,
        reply: oneshot::Sender<Result<Vec<RelayRow>>>,
    },
    Blocks {
        cids: Vec<ContentHash>,
        reply: oneshot::Sender<Result<Vec<BlockLookup>>>,
    },
    HeldSegments {
        scope: String,
        kind: String,
        /// Just this segment id, or every held one.
        only: Option<u32>,
        reply: oneshot::Sender<Result<Vec<HeldSegment>>>,
    },
}

/// A handle to a serving store, owned by a dedicated thread (see the module
/// docs). Cloneable; the thread exits when every handle is dropped.
#[derive(Clone)]
pub struct ServeStoreHandle {
    tx: mpsc::Sender<StoreCmd>,
}

impl ServeStoreHandle {
    /// Take ownership of `store` on a dedicated thread and answer commands
    /// over a channel. The thread runs a current-thread tokio runtime, so the
    /// store's AFIT futures never need to be `Send`.
    pub fn spawn(store: AccountStore<SqliteBackend>) -> Self {
        let (tx, mut rx) = mpsc::channel::<StoreCmd>(64);
        std::thread::Builder::new()
            .name("peer-sync-store".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .expect("current-thread runtime for the serving store");
                rt.block_on(async move {
                    while let Some(cmd) = rx.recv().await {
                        answer(&store, cmd).await;
                    }
                });
            })
            .expect("spawn serving-store thread");
        Self { tx }
    }

    /// [`Self::spawn`], with the store OPENED on its owner thread too — for a
    /// caller on a `Send`-demanding task that cannot hold the store's own
    /// futures across an await (a nest RPC handler serving a custodied store
    /// per request). `open` runs first on the thread; its failure is returned
    /// here and the thread exits.
    pub async fn open_on_thread<F, Fut>(open: F) -> Result<Self>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<AccountStore<SqliteBackend>>>,
    {
        let (tx, mut rx) = mpsc::channel::<StoreCmd>(64);
        let (opened_tx, opened_rx) = oneshot::channel::<Result<()>>();
        std::thread::Builder::new()
            .name("serving-store".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .expect("current-thread runtime for the serving store");
                rt.block_on(async move {
                    let store = match open().await {
                        Ok(store) => {
                            let _ = opened_tx.send(Ok(()));
                            store
                        }
                        Err(e) => {
                            let _ = opened_tx.send(Err(e));
                            return;
                        }
                    };
                    while let Some(cmd) = rx.recv().await {
                        answer(&store, cmd).await;
                    }
                });
            })
            .context("spawn serving-store thread")?;
        opened_rx
            .await
            .context("serving-store thread exited before opening")??;
        Ok(Self { tx })
    }

    async fn relay_page(
        &self,
        scope: String,
        item_class: String,
        frontier: Vec<(WriterId, u64)>,
        limit: u32,
    ) -> Result<Vec<RelayRow>> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(StoreCmd::RelayPage {
                scope,
                item_class,
                frontier,
                limit,
                reply,
            })
            .await
            .context("serving-store thread is gone")?;
        rx.await.context("serving-store thread dropped a reply")?
    }

    pub(crate) async fn blocks(&self, cids: Vec<ContentHash>) -> Result<Vec<BlockLookup>> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(StoreCmd::Blocks { cids, reply })
            .await
            .context("serving-store thread is gone")?;
        rx.await.context("serving-store thread dropped a reply")?
    }

    async fn held_segments(
        &self,
        scope: String,
        kind: String,
        only: Option<u32>,
    ) -> Result<Vec<HeldSegment>> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(StoreCmd::HeldSegments {
                scope,
                kind,
                only,
                reply,
            })
            .await
            .context("serving-store thread is gone")?;
        rx.await.context("serving-store thread dropped a reply")?
    }
}

/// The store thread's one dispatch.
async fn answer(store: &AccountStore<SqliteBackend>, cmd: StoreCmd) {
    match cmd {
        StoreCmd::RelayPage {
            scope,
            item_class,
            frontier,
            limit,
            reply,
        } => {
            let r = store
                .relay_rows(&scope, &item_class, &frontier, limit)
                .await;
            let _ = reply.send(r);
        }
        StoreCmd::Blocks { cids, reply } => {
            let mut out = Vec::with_capacity(cids.len());
            let mut err = None;
            for cid in cids {
                let scope = match store.record(&cid).await {
                    Ok(entry) => entry.map(|e| e.scope),
                    Err(e) => {
                        err = Some(e);
                        break;
                    }
                };
                let bytes = match store.block(&cid).await {
                    Ok(b) => b,
                    Err(e) => {
                        err = Some(e);
                        break;
                    }
                };
                out.push(BlockLookup { cid, scope, bytes });
            }
            let _ = reply.send(match err {
                None => Ok(out),
                Some(e) => Err(e),
            });
        }
        StoreCmd::HeldSegments {
            scope,
            kind,
            only,
            reply,
        } => {
            let _ = reply.send(held_segments(store, &scope, &kind, only).await);
        }
    }
}

/// Every adopted segment of `kind` under `scope` whose `.dat` this store still
/// holds, ascending by id. An evicted container (the metadata floor kept its
/// row and sidecar, never its bytes) is not servable and is left out — a
/// listing that advertised it would promise bytes the byte door cannot hand.
/// `only` narrows it to one id — the byte door's lookup, which reads that one
/// segment's sidecar rather than every held one's.
async fn held_segments(
    store: &AccountStore<SqliteBackend>,
    scope: &str,
    kind: &str,
    only: Option<u32>,
) -> Result<Vec<HeldSegment>> {
    let backend = store.backend();
    let mut out = Vec::new();
    for key in store.adopted_segments(scope).await? {
        if key.kind != kind || only.is_some_and(|id| id != key.segment_id) {
            continue;
        }
        let Some(dat_path) = backend.held_segment_dat_path(&key)? else {
            continue;
        };
        let Some(meta) = backend.segment_meta(&key).await? else {
            continue;
        };
        out.push(HeldSegment {
            segment_id: key.segment_id,
            meta,
            dat_path,
        });
    }
    Ok(out)
}

// ── The changes-list serve ───────────────────────────────────────────────────

/// Why a serve call answered no page. Typed so each transport maps it onto
/// its own vocabulary (`fauna.peer.sync.unsupported` on the peer channel,
/// `fauna.sync.invalid_request` on the nest wire) — the refusal's *meaning*
/// is shared, its spelling is the door's.
#[derive(Debug)]
pub enum ServeRefusal {
    /// The request asks for something this serve does not answer — loudly,
    /// never an empty page, because an empty page reads as "converged" to a
    /// walk.
    Unsupported(&'static str),
    /// The store failed.
    Internal(anyhow::Error),
}

/// Name the scope and item class a `fauna.sync.changes.list` request asks
/// about — the step every door runs BEFORE its own admission check, since the
/// verdict is per scope.
///
/// The serve carries only the item-class arms: the file-row shape (no
/// `item_class`, the sync engine's own file pull) is nest-mediated and refused
/// loudly, as is a record-cid walk that names no scope (there is no default
/// content scope to imply).
pub fn changes_list_scope(req: &SyncChangesListRequest) -> Result<(String, String), ServeRefusal> {
    let item_class = req.item_class.as_deref().ok_or(ServeRefusal::Unsupported(
        "this feed serves item-class arms only (state-entry, record-cid)",
    ))?;
    let scope = match ItemClass::from_wire(item_class) {
        Some(ItemClass::StateEntry) => req
            .scope
            .clone()
            .unwrap_or_else(|| ACCOUNT_STATE_SCOPE.to_string()),
        Some(ItemClass::RecordCid) => req.scope.clone().ok_or(ServeRefusal::Unsupported(
            "a record-cid walk names its content scope explicitly",
        ))?,
        _ => {
            return Err(ServeRefusal::Unsupported(
                "this feed serves the state-entry and record-cid arms only",
            ));
        }
    };
    Ok((item_class.to_string(), scope))
}

/// Answer one `fauna.sync.changes.list` page from `store` — the relay plane by
/// the dialer's own frontier. `item_class` and `scope` are what
/// [`changes_list_scope`] named and the calling door admitted.
pub async fn serve_changes_list(
    store: &ServeStoreHandle,
    item_class: String,
    scope: String,
    req: &SyncChangesListRequest,
) -> Result<SyncChangesListReply, ServeRefusal> {
    let frontier: Vec<(WriterId, u64)> = match &req.frontier {
        Some(f) => f
            .iter()
            .filter_map(|(hex_writer, seq)| {
                let bytes: [u8; 32] = hex::decode(hex_writer).ok()?.try_into().ok()?;
                Some((WriterId(bytes), u64::try_from(*seq).ok()?))
            })
            .collect(),
        // The feed contract (charter § Feeds and cursors): "an omitted
        // frontier is `{nest: since}`" — the scalar-cursor form the
        // one-writer content walk sends. The class-2 walk always sends an
        // explicit frontier, so for state-entry this arm changes nothing.
        None => vec![(
            WriterId::NEST_SEQUENCER,
            u64::try_from(req.since).unwrap_or(0),
        )],
    };

    let rows = store
        .relay_page(scope, item_class, frontier, MAX_ROWS_PER_PAGE)
        .await
        .map_err(ServeRefusal::Internal)?;

    // Trim to the frame budget; a truncated page is re-requested with the
    // client's advanced frontier (the ordered-prefix-per-writer law keeps
    // truncation safe at any point).
    let mut changes = Vec::new();
    let mut entry_bytes = 0usize;
    for row in rows {
        entry_bytes += row.entry.as_ref().map_or(0, Vec::len);
        if !changes.is_empty() && entry_bytes > MAX_ENTRY_BYTES_PER_PAGE {
            break;
        }
        changes.push(relay_row_to_change(row));
    }
    Ok(SyncChangesListReply {
        changes,
        caller_access: None,
        residency: None,
        // Never echoed, and deliberately: a store serves `(writer,
        // writer_seq)` order, with no cross-writer order a watermark could
        // be a coordinate in — the ruling defers the store-served legs
        // (account-sync-plane.md § Feeds and cursors → *Compaction is a
        // serve-order watermark*). A requester's `held_through_seq` is
        // therefore ignored here, which is exactly the no-watermark answer.
        complete_through_seq: None,
        retirable_through_seq: None,
        replica_id: None,
        extra: Default::default(),
        signer_certs: Vec::new(),
    })
}

/// A relay row as the wire row the nest would have served — same slots, same
/// meanings (`fauna_sync_engine::account_state_plane` reads them back with
/// `row_coordinates` + `item_key_of`; `fauna_sync_engine::content_scope_plane`
/// reads `seq` + `path_hash`).
fn relay_row_to_change(row: RelayRow) -> SyncChange {
    SyncChange {
        // State-entry readers account (origin_writer, origin_seq) and never
        // this slot — mirroring origin_seq keeps their error messages ("row
        // at seq N") meaningful. Record-cid readers DO account it: their
        // writer is the nest sequencer, whose relay `writer_seq` is the
        // nest's own feed coordinate, so the mirror is exact there too.
        seq: row.writer_seq as i64,
        path_hash: hex::encode(&row.item_key),
        manifest_hash: None,
        size_bytes: row.entry.as_ref().map_or(0, |e| e.len() as i64),
        change_type: row.op,
        created_at: 0,
        path: None,
        device_id: None,
        content_key_version: None,
        thumbnail_hash: None,
        author_actor_id: None,
        path_sealed: None,
        derived_through: None,
        is_resolution: None,
        is_retention: None,
        item_class: Some(row.item_class),
        origin_writer: Some(row.writer.to_hex()),
        origin_seq: Some(row.writer_seq as i64),
        entry: row.entry.map(serde_bytes::ByteBuf::from),
        extra: Default::default(),
        signature: None,
        signer_key: None,
    }
}

// ── The adopted-segment serve ────────────────────────────────────────────────

/// `fauna.segments.list` answered from `store`: the adopted segments of
/// `kind` the store holds under the content scope `scope`, each described the
/// way the nest's own listing describes a segment it wrote — so an adopting
/// replica's bootstrap source (which reads `segment_id`, `size_bytes` and the
/// two in-transit hashes) cannot tell a held copy from the original.
///
/// Three fields a mirror honestly cannot know are stated, not invented:
/// `tombstone_count` is `0` and `created_at_secs` is `0` (the adopted sidecar
/// view carries neither — `SegmentSidecarView`), and `next_segment_id` is the
/// highest held id plus one: a mirror keeps no saved counter of its own, and
/// the owner's counter is the owner's nest's to state.
pub async fn serve_segment_listing(
    store: &ServeStoreHandle,
    scope: String,
    kind: String,
) -> Result<SegmentsListReply> {
    let held = store.held_segments(scope, kind, None).await?;
    let next_segment_id = held
        .iter()
        .map(|s| s.segment_id.saturating_add(1))
        .max()
        .unwrap_or(0);
    let segments = tokio::task::spawn_blocking(move || -> Result<Vec<SegmentRef>> {
        let mut out = Vec::with_capacity(held.len());
        for seg in held {
            let sidecar = SegmentSidecarView::decode(&seg.meta)?;
            let file = std::fs::File::open(&seg.dat_path)
                .with_context(|| format!("open held segment {}", seg.dat_path.display()))?;
            let size_bytes = file.metadata()?.len();
            let dat_blake3 = fauna_core::chunker_stream::blake3_of_reader(file)?;
            let meta_blake3 = fauna_core::chunker_stream::blake3_of_reader(seg.meta.as_slice())?;
            out.push(SegmentRef {
                segment_id: seg.segment_id,
                blake3_hex: hex::encode(dat_blake3),
                bucket: sidecar.bucket,
                record_count: u32::try_from(sidecar.record_order.len()).unwrap_or(u32::MAX),
                tombstone_count: 0,
                size_bytes,
                created_at_secs: 0,
                // An adopted pair is finalized by construction.
                is_open: false,
                meta_blake3_hex: hex::encode(meta_blake3),
                extra: Default::default(),
            });
        }
        Ok(out)
    })
    .await
    .context("segment listing task")??;
    Ok(SegmentsListReply {
        segments,
        next_segment_id,
        extra: Default::default(),
    })
}

/// Which half of an adopted segment pair a byte request wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentPart {
    /// The CARv2 container, verbatim.
    Dat,
    /// The dag-cbor sidecar, verbatim.
    Meta,
}

/// One half of an adopted segment pair as the byte door hands it out.
#[derive(Debug)]
pub enum SegmentPartBody {
    /// The `.dat`, **open** — the door streams it, never reads it whole: a
    /// segment has no size ceiling
    /// (`docs/goal/architecture/message-segment-store.md` § Segment size).
    Dat(tokio::fs::File),
    /// The `.meta` sidecar, verbatim. The store keeps it in its own segment
    /// row (the admission's index, decoded whole on adoption), so it is in
    /// memory already.
    Meta(Vec<u8>),
}

/// One half of an adopted segment pair, verbatim — `None` when the store holds
/// no such segment, or no longer holds its container. The byte door's answer.
pub async fn serve_segment_part(
    store: &ServeStoreHandle,
    scope: String,
    kind: String,
    segment_id: u32,
    part: SegmentPart,
) -> Result<Option<SegmentPartBody>> {
    let held = store.held_segments(scope, kind, Some(segment_id)).await?;
    let Some(seg) = held.into_iter().find(|s| s.segment_id == segment_id) else {
        return Ok(None);
    };
    match part {
        SegmentPart::Meta => Ok(Some(SegmentPartBody::Meta(seg.meta))),
        SegmentPart::Dat => match tokio::fs::File::open(&seg.dat_path).await {
            Ok(file) => Ok(Some(SegmentPartBody::Dat(file))),
            // Evicted between the listing read and this one.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("open {}", seg.dat_path.display())),
        },
    }
}
