//! The chunk relay (`file-sync.md` § Relay serving): serves a chunk from the
//! local blob store, or relays it from a connection that announced the folder
//! — and the one content-reachability verdict that counts the same holders.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use tokio::sync::{Mutex, oneshot};

use crate::blob_store::BlobStoreBackend;

/// Serves a chunk from the local blob store, or relays it from a connection
/// that announced the folder (`fauna.sync.serve.announce`).
///
/// The blob store holds [`crate::backup::encode_blob`]-format bytes (compress →
/// encrypt-at-rest), addressed by each chunk's **store key** = `BLAKE3(stored
/// bytes before encode_blob)`. For an ordinary set that is `BLAKE3(plaintext)`; for
/// a content-key (E2E shared-set) chunk it is `BLAKE3(ciphertext)` — the chunk is
/// already `chunk_crypto`-AEAD-encrypted under a per-set content key the nest does
/// not hold, so the nest only ever moves it as opaque bytes addressed by that
/// store key (`ChunkManifest::store_keys()`). Callers therefore pass the **store
/// key** as the address (not the plaintext `chunk_hash`), and a seat's answer is
/// verified against it: `BLAKE3(bytes) == store_key` (= plaintext-hash for an
/// ordinary chunk, ciphertext-hash for a content-key chunk). So the resolver is the
/// encode-blob codec boundary only: it **decodes** blob-store bytes before
/// returning them and **encodes** a relayed answer before caching, keeping
/// the store uniformly encode_blob-format — it is content-key-agnostic (it never
/// decrypts the `chunk_crypto` layer, which only the holder of the content key can).
///
/// **It holds a clone of the store handle for the life of the process**, so
/// `backup::service`'s blob-store partition classifies every use of that handle
/// and every caller of [`Self::relay_for_folder`], its one read — the
/// digest it serves is the one a URL named, so its caller must sit behind the
/// takedown gate. There is deliberately no unscoped by-hash read here: the four
/// it once carried (by folder name and by device, for chunks and manifests) had
/// no production caller and no gate, and were removed rather than left for a
/// new door to wire in (`moderation.md` § Legal takedown → *The blob-serve
/// door*).
pub struct ChunkResolver {
    blob_store: Option<Arc<dyn BlobStoreBackend>>,
    /// Owner `BackupKey` for at-rest chunk encryption, or `None` when the nest
    /// stores chunks without encryption (the live config — the nest never holds
    /// `BackupKey`). Mirrors
    /// `BackupService::encryption_key()`.
    encryption_key: Option<BackupKey>,
    /// Whether blob-store writes are zstd-compressed. Mirrors
    /// `BackupService::compression()`.
    compression: bool,
    next_request_id: AtomicU64,
    pending: Mutex<HashMap<u64, PendingAsk>>,
    /// Patience applied to each seat's ask before it is written off as silent.
    /// Configurable for tests (see [`Self::with_fetch_timeout`]): a test that
    /// proves the *bound* on a walk over silent seats cannot afford to pay the
    /// production deadline even once.
    fetch_timeout: std::time::Duration,
    /// The foreign seats this nest leases — the walk's third candidate kind and
    /// a holder for the reachability verdict.
    foreign_seats: ForeignSeats,
}

/// What a holding seat answered an ask with.
///
/// The bytes, plus **the folder they are attributed to**. That attribution is
/// what lets the relay read path decide whether the *hinted* folder's
/// residency is also the *chunk's* residency: the route authorizes on a folder
/// and fetches on a hash, and nothing else in between ties the two
/// together.
/// An announced seat's answer is attributed by construction — the ask names
/// the folder and the seat answers from that folder's state alone
/// ([`ChunkResolver::fetch_announced`]).
///
/// `attributed_folder` is `None` for every non-answer. `None` is never read as
/// permission; it falls back to the per-owner gate ([`RelayCache`]).
#[derive(Debug, Default, Clone)]
pub struct SeatAnswer {
    pub data: Option<Vec<u8>>,
    pub attributed_folder: Option<i64>,
}

/// One ask in flight: where its answer goes, and **who may give it**.
///
/// A request id is a small counter anyone can guess, and the answer arrives
/// over HTTP ([`ChunkResolver::answer_announced`]), where the only thing that
/// ties it to the ask is the answering actor — so the ask records which actor
/// it went to, and no other actor's answer completes it.
struct PendingAsk {
    tx: oneshot::Sender<SeatAnswer>,
    /// The asked connection's actor — the one actor whose answer is taken.
    responder: [u8; 32],
}

/// A seat that announced it serves the folder being read — the relay's
/// candidate (`file-sync.md` § Relay serving), collected by the caller and
/// re-checked against the folder row before it is handed in: a connection on
/// this nest ([`crate::ws::WsState::announced_for_folder`]) or a leased
/// foreign seat ([`ForeignSeats`]).
pub struct AnnouncedSeat {
    /// The seat's actor — the one actor whose answer is taken.
    pub actor: [u8; 32],
    /// How the ask reaches it.
    pub via: SeatVia,
    /// The folder as the seat announced it (`FolderRef` wire string) — what
    /// the ask names, so the seat answers from that folder's state alone.
    pub folder_ref: String,
}

/// How an ask reaches an [`AnnouncedSeat`].
pub enum SeatVia {
    /// A connection on this nest: the ask is a `fauna.sync.chunk.wanted` push
    /// on it.
    Connection(Arc<crate::ws::RpcConnection>),
    /// A foreign seat, asked through its member's nest
    /// (`fauna.federation.folder.chunk.wanted`, `file-sync.md` § Relay serving
    /// → *A member on another nest*, step (3)). The caller builds the call:
    /// given the request id and the store key, it resolves to whether the
    /// member's nest pushed the ask — `false` for anything else, a refusal and
    /// a transport error alike, so the window refills at once.
    Forwarded(ForwardAsk),
}

/// The federated ask of a [`SeatVia::Forwarded`] seat: `(request id, store key
/// hex) → pushed`.
pub type ForwardAsk = Box<
    dyn FnOnce(u64, String) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        + Send,
>;

/// How long a foreign seat stays a candidate without its member's nest renewing
/// it (`federation.md` § Cross-nest shared folders + channel append → *Relay
/// serving across nests*: "a lease of a hard-coded length"). The member's nest
/// renews at half of it, from the length the home nest's reply states. Long
/// enough that one lost renewal does not drop a seat; short enough that a
/// member's nest that went away stops costing a window slot within minutes.
pub const FOREIGN_SEAT_LEASE: std::time::Duration = std::time::Duration::from_secs(120);

/// The most foreign seats one folder holds at once — the walk asks no more
/// than [`RELAY_MAX_CANDIDATE_SEATS`] seats in total anyway.
const FOREIGN_SEATS_PER_FOLDER: usize = RELAY_MAX_CANDIDATE_SEATS;

/// The most foreign seats this nest holds at once, across every folder. The
/// table is in memory, so it is bounded whole: a lease past it is refused
/// until some lapse.
const FOREIGN_SEATS_MAX: usize = 4096;

/// One leased foreign seat (`file-sync.md` § Relay serving → *A member on
/// another nest*, step (2)): a cross-nest writer's device that serves a folder
/// homed here, announced through its own nest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignSeat {
    /// The folder row it serves.
    pub folder_id: i64,
    /// The folder's channel — what the ask names it by on the member's side.
    pub channel_id: [u8; 32],
    /// The member — the one actor whose answer is taken.
    pub member: [u8; 32],
    /// The member's device that serves the folder.
    pub device: [u8; 32],
    /// The verified `nest_id` of the member's nest that forwarded the announce
    /// — the only nest the ask is sent to.
    pub origin_nest_id: [u8; 32],
    /// Where that nest is reached.
    pub nest_url: String,
}

/// Why [`ForeignSeats::lease`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseRefused {
    /// The folder already holds [`FOREIGN_SEATS_PER_FOLDER`] live seats.
    FolderFull,
    /// This nest already holds [`FOREIGN_SEATS_MAX`] live seats.
    TableFull,
}

/// The foreign seats this nest leases (`file-sync.md` § Relay serving → *A
/// member on another nest*): in memory only, bounded in count, each on a
/// [`FOREIGN_SEAT_LEASE`] its member's nest renews. Keyed by (folder row,
/// member, device). A seat is dropped by its *no longer serving*, by its lease
/// lapsing, and at ask time when the write gate no longer holds for it
/// ([`Self::drop_seat`]). A lapsed seat is never a candidate and never counts
/// as a holder, whether or not it has been swept yet.
#[derive(Default)]
pub struct ForeignSeats {
    seats: std::sync::Mutex<HashMap<(i64, [u8; 32], [u8; 32]), (ForeignSeat, std::time::Instant)>>,
}

impl ForeignSeats {
    /// Lease or renew `seat` until `now + FOREIGN_SEAT_LEASE`.
    pub fn lease(&self, seat: ForeignSeat) -> Result<(), LeaseRefused> {
        self.lease_at(seat, std::time::Instant::now())
    }

    fn lease_at(&self, seat: ForeignSeat, now: std::time::Instant) -> Result<(), LeaseRefused> {
        let mut seats = self.seats.lock().unwrap();
        seats.retain(|_, (_, expires)| *expires > now);
        let key = (seat.folder_id, seat.member, seat.device);
        if !seats.contains_key(&key) {
            if seats.len() >= FOREIGN_SEATS_MAX {
                return Err(LeaseRefused::TableFull);
            }
            let in_folder = seats.keys().filter(|k| k.0 == seat.folder_id).count();
            if in_folder >= FOREIGN_SEATS_PER_FOLDER {
                return Err(LeaseRefused::FolderFull);
            }
        }
        seats.insert(key, (seat, now + FOREIGN_SEAT_LEASE));
        Ok(())
    }

    /// The member's nest's *no longer serving*: drop the seat, but only one
    /// `origin_nest_id` leased — a nest withdraws no other nest's seat.
    pub fn withdraw(
        &self,
        folder_id: i64,
        member: &[u8; 32],
        device: &[u8; 32],
        origin_nest_id: &[u8; 32],
    ) {
        let mut seats = self.seats.lock().unwrap();
        let key = (folder_id, *member, *device);
        if seats
            .get(&key)
            .is_some_and(|(s, _)| s.origin_nest_id == *origin_nest_id)
        {
            seats.remove(&key);
        }
    }

    /// Drop a seat the write gate no longer admits.
    pub fn drop_seat(&self, seat: &ForeignSeat) {
        self.seats
            .lock()
            .unwrap()
            .remove(&(seat.folder_id, seat.member, seat.device));
    }

    /// Every live seat of `folder_id` — the relay's third candidate kind.
    pub fn live_for_folder(&self, folder_id: i64) -> Vec<ForeignSeat> {
        self.live_for_folder_at(folder_id, std::time::Instant::now())
    }

    fn live_for_folder_at(&self, folder_id: i64, now: std::time::Instant) -> Vec<ForeignSeat> {
        self.seats
            .lock()
            .unwrap()
            .values()
            .filter(|(s, expires)| s.folder_id == folder_id && *expires > now)
            .map(|(s, _)| s.clone())
            .collect()
    }

    /// Whether `folder_id` has a live seat — the reachability verdict's third
    /// holder ([`folder_content_reachable`]).
    pub fn has_live_for_folder(&self, folder_id: i64) -> bool {
        self.has_live_for_folder_at(folder_id, std::time::Instant::now())
    }

    fn has_live_for_folder_at(&self, folder_id: i64, now: std::time::Instant) -> bool {
        self.seats
            .lock()
            .unwrap()
            .values()
            .any(|(s, expires)| s.folder_id == folder_id && *expires > now)
    }
}

/// What [`ChunkResolver::answer_announced`] made of an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnouncedAnswer {
    /// The ask was pending, went to this actor, and now has its answer.
    Taken,
    /// No such pending ask for this actor — unknown, expired, already
    /// answered, or another actor's. Nothing was touched.
    Refused,
}

/// Timeout for waiting on a seat to answer with chunk data.
const SEAT_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How many seats one relay read will ask **at once**.
///
/// The walk over every seat is what makes the relay correct — a chunk lives on
/// whichever seat happens to hold it, and only asking all of them finds it. What
/// that walk must not do is *serialize* the asking, because a seat that is
/// connected but silent costs a full [`SEAT_FETCH_TIMEOUT`]. Serially that is
/// `N × 30 s` per chunk; raced, the worst case is one deadline per *window*,
/// so the cost stops scaling with the seat count.
///
/// A window rather than a full fan-out, deliberately. The engine already pulls
/// chunks K-parallel (`fauna_sync_engine`'s transfer pool), so a fan-out to
/// every seat would multiply *seat-side* ask load by K × N — trading a
/// nest-side bound for a client-side stampede. Three is enough to hide a couple
/// of silent seats behind the holder without asking the whole fleet at once.
const RELAY_FETCH_CONCURRENCY: usize = 3;

/// A window of one is not a window — it is the serial walk this bound exists to
/// retire, wearing the new shape's name. Guarded here because the guard cannot live in
/// the test that proves the concurrency: that test's own threshold is the thing
/// a `= 1` would move (measured — the first draft asserted
/// `asked >= RELAY_FETCH_CONCURRENCY` and passed happily against `= 1`).
const _: () = assert!(RELAY_FETCH_CONCURRENCY > 1);

/// The most seats one relay read will ask **in total**, however many are
/// connected.
///
/// The concurrency window above bounds the deadline; this bounds the work. Past
/// this many candidates the read gives up rather than walking an unbounded list
/// — a chunk that none of the first [`RELAY_MAX_CANDIDATE_SEATS`] seats holds is
/// treated as absent, which is the same answer the caller already handles. Sized
/// far above any real folder's seat count: exceeding it means something is
/// wrong with announcing, not with this read, and it is logged as such.
const RELAY_MAX_CANDIDATE_SEATS: usize = 16;

/// What becomes of a chunk once a holding seat has relayed it through the nest.
///
/// `Store` is the resolver's historical behaviour — the relayed bytes are
/// encoded into the blob store so the next reader is served locally (a
/// full-residency folder re-hydrating after a flip-back, `file-sync.md`
/// § Content residency, consequence 3). `Transient` is the **metadata-only**
/// arm (enforcement gate 3 of the same §): the bytes are served to the one
/// requester and never touch the nest's disk — the relay is the only
/// nest-mediated content path such a folder has, and the point of the
/// residency choice is that it leaves no copy behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelayCache {
    /// Encode + write the relayed bytes into the blob store (if configured).
    Store,
    /// Write **only if the answer is attributed to the folder the reader
    /// hinted** — the arm for a `full` folder whose owner holds a
    /// `metadata_only` folder somewhere. An attributed
    /// answer settles the question the by-hash rails cannot; an unattributed
    /// one falls to `Transient` — never to `Store`.
    StoreIfAttributed,
    /// Serve without writing; the blob store stays as it was.
    Transient,
}

impl RelayCache {
    /// Whether an answer attributed to the hinted folder (`attributed`) rests
    /// in the blob store under this arm. Taken against the seat that actually
    /// answered: attribution does not travel between seats.
    fn rests(self, attributed: bool) -> bool {
        match self {
            RelayCache::Store => true,
            RelayCache::StoreIfAttributed => attributed,
            RelayCache::Transient => false,
        }
    }
}

impl ChunkResolver {
    pub fn new(
        blob_store: Option<Arc<dyn BlobStoreBackend>>,
        encryption_key: Option<BackupKey>,
        compression: bool,
    ) -> Self {
        Self {
            blob_store,
            encryption_key,
            compression,
            next_request_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            fetch_timeout: SEAT_FETCH_TIMEOUT,
            foreign_seats: ForeignSeats::default(),
        }
    }

    /// The foreign seats this nest leases (`fauna.federation.folder.serve.announce`).
    pub fn foreign_seats(&self) -> &ForeignSeats {
        &self.foreign_seats
    }

    /// Shorten the per-seat fetch deadline (tests only).
    ///
    /// Without this a test that announces several connected-but-silent seats
    /// to prove the walk's bound would pay the production 30 s per window
    /// before it could assert anything — so the assertion that the cost is
    /// bounded would itself be unaffordable to make, which is how the
    /// unbounded walk went unpinned in the first place.
    #[cfg(test)]
    pub fn with_fetch_timeout(mut self, fetch_timeout: std::time::Duration) -> Self {
        self.fetch_timeout = fetch_timeout;
        self
    }

    /// Decode blob-store bytes (compress → encrypt at rest) back to the raw
    /// plaintext chunk/manifest a reader expects. The inverse of [`Self::encode`].
    ///
    /// Unconditional: `encode_blob` is now **always self-describing** (its
    /// `compress=false` branch frames with `PREFIX_UNCOMPRESSED`), so `decode_blob`
    /// is a lossless round-trip for *every* `(encryption_key, compression)` config
    /// — including the live identity config `(None, false)`, where a `0x00`/`0x01`-
    /// leading raw chunk previously had to be guarded against mis-stripping. The
    /// store stays uniformly `encode_blob`-format; every reader decodes.
    fn decode(&self, stored: &[u8]) -> Result<Vec<u8>> {
        crate::backup::decode_blob(stored, self.encryption_key.as_ref())
    }

    /// Encode a raw plaintext chunk/manifest into blob-store format before
    /// caching, so a chunk relayed from a seat is stored in
    /// the same format as one uploaded via `POST /chunks` — keeping the store
    /// uniform so [`Self::decode`] on read is unambiguous.
    fn encode(&self, raw: &[u8]) -> Result<Vec<u8>> {
        crate::backup::encode_blob(raw, self.encryption_key.as_ref(), self.compression)
    }

    /// Verify seat-supplied bytes against the content address they were
    /// requested under; a mismatch is discarded and treated as absent.
    ///
    /// Every other writer into the blob store server-computes or verifies its
    /// key from the bytes; this fetch path was the one exception — an
    /// unverified `put` under the *requested* hash would let a lying or buggy
    /// source seat seed the GC-probed store with bytes that are not the
    /// preimage of their key. Downstream that is indistinguishable from
    /// at-rest corruption (GC's content-address integrity split fail-closes
    /// on exactly that signal — `backup/gc.rs::stored_bytes_match_reference`),
    /// and the bytes would be served as-is to forward destinations. The
    /// destination peer's own `BLAKE3(bytes) == store_key` check covers the
    /// forward leg; this is the nest-side twin for its own store.
    fn verify_fetched(
        &self,
        hash: &ContentHash,
        data: Option<Vec<u8>>,
        what: &str,
    ) -> Option<Vec<u8>> {
        let bytes = data?;
        if ContentHash::of_raw(&bytes).digest() != hash.digest() {
            tracing::warn!(
                requested = hex::encode(hash.digest()),
                what,
                "seat-supplied bytes do not match the requested content \
                 address — discarded, not cached"
            );
            return None;
        }
        Some(bytes)
    }

    /// The relay read path (`file-sync.md` § Relay serving, and § Content
    /// residency gate 3): serve a chunk the blob store does **not** hold by
    /// asking the connections that announced `folder`'s row, applying `cache`
    /// to the result.
    ///
    /// `announced` is the caller's: the connections that announced this
    /// folder's row, the owner's or a roster member's, each re-checked against
    /// the row. The store is still checked first: the caller's miss may have
    /// been filled by an upload in the meantime, and a hit needs no seat
    /// round-trip regardless of `cache`.
    ///
    /// **Every** candidate is tried, in the caller's arbitrary order, until one
    /// answers with bytes. Asking only the first is what this looked like until
    /// 2026-09-02, and it was wrong for a reason the two-seat case makes plain:
    /// the seat *requesting* the chunk may itself be a seat of this folder, so
    /// the pick could land on the asker — which by construction does not hold
    /// the bytes, and which is meanwhile blocked on its own in-flight download.
    /// For a metadata-only folder the relay is the ONLY content path
    /// (`file-sync.md` § Content residency), so that pick was a coin-flip on
    /// whether a second seat could hydrate the file at all.
    ///
    /// A seat that answers nothing is passed over rather than treated as the
    /// answer; a seat that answers with bytes that do not match the requested
    /// address is also passed over (`verify_fetched` discards them), so a single
    /// bad seat cannot deny the file. The `cache` decision is taken against the
    /// seat that actually answered.
    ///
    /// Each seat is asked with a `fauna.sync.chunk.wanted` push on its own
    /// connection and answers over HTTP ([`Self::answer_announced`]). Its
    /// answer is attributed to `folder_id` (the hinted row's id — never its
    /// name, which rests sealed) by construction: the ask names the folder and
    /// the seat answers from that folder's state alone.
    pub async fn relay_for_folder(
        &self,
        hash: &ContentHash,
        folder_id: i64,
        mut announced: Vec<AnnouncedSeat>,
        cache: RelayCache,
    ) -> Result<Option<Vec<u8>>> {
        if let Some(bs) = &self.blob_store
            && let Some(stored) = bs.get(hash).await?
        {
            return Ok(Some(self.decode(&stored)?));
        }
        let hash_hex = hex::encode(hash.digest());

        if announced.len() > RELAY_MAX_CANDIDATE_SEATS {
            tracing::warn!(
                seats = announced.len(),
                cap = RELAY_MAX_CANDIDATE_SEATS,
                "chunk relay: a folder has more announced seats than one read will ask — \
                 serving from the first {RELAY_MAX_CANDIDATE_SEATS}"
            );
            announced.truncate(RELAY_MAX_CANDIDATE_SEATS);
        }

        // Ask the candidates in a bounded window rather than one after another.
        // The seat that holds the chunk is not knowable in advance, so a serial
        // walk pays the silent seats ahead of the holder on every chunk of the
        // file. Racing them collapses that to one deadline per window, and the
        // first answer that survives `verify_fetched` wins.
        //
        // `issued` carries the request ids so the losers can be swept below:
        // abandoning a racing future drops it mid-await, and a dropped future
        // runs no cleanup.
        let issued = std::sync::Mutex::new(Vec::new());
        let winner = {
            use futures_util::stream::StreamExt;
            let mut answers = futures_util::stream::iter(announced.into_iter().map(|seat| {
                let hash_hex = hash_hex.clone();
                let issued = &issued;
                async move {
                    self.fetch_announced(seat, hash_hex, folder_id, issued)
                        .await
                }
            }))
            .buffer_unordered(RELAY_FETCH_CONCURRENCY);

            let mut found = None;
            while let Some(answer) = answers.next().await {
                let attributed = answer.attributed_folder == Some(folder_id);
                let Some(data) = self.verify_fetched(hash, answer.data, "chunk") else {
                    // Nothing, or bytes that did not match the address. Either
                    // way this seat is not the holder — let the window refill.
                    continue;
                };
                found = Some((data, attributed));
                break;
            }
            found
        };
        // Every racer still in flight when the winner broke the loop was dropped
        // by `answers` going out of scope, leaving its `pending` slot behind —
        // only the timeout arm removes its own. Sweep them here, or a busy
        // folder leaks one entry per abandoned seat per chunk.
        self.sweep_pending(&issued).await;

        let Some((data, attributed)) = winner else {
            return Ok(None);
        };
        if cache.rests(attributed) {
            self.cache_fetched(hash, Some(&data), "chunk").await;
        } else if cache == RelayCache::StoreIfAttributed {
            tracing::debug!(
                "chunk relay: the answer is attributed to no hinted folder — \
                 serving without caching"
            );
        }
        Ok(Some(data))
    }

    /// Cache seat-relayed bytes locally — stored in encode_blob format so the
    /// blob store stays uniform (every entry decodes on read), matching what
    /// `POST /chunks` / `POST /manifests` write. The caller's `data` stays raw.
    /// A `None` (nothing fetched) or no configured store is a no-op; a failed
    /// write is logged and swallowed — the bytes were already verified against
    /// their address, so the caller serves them either way.
    async fn cache_fetched(&self, hash: &ContentHash, data: Option<&[u8]>, what: &str) {
        let (Some(bs), Some(bytes)) = (&self.blob_store, data) else {
            return;
        };
        match self.encode(bytes) {
            Ok(encoded) => {
                if let Err(e) = bs.put(hash, &encoded).await {
                    tracing::warn!("failed to cache fetched {what} locally: {e}");
                }
            }
            Err(e) => tracing::warn!("failed to encode fetched {what} for cache: {e}"),
        }
    }

    /// Take an announced seat's answer to a `fauna.sync.chunk.wanted` ask
    /// (`file-sync.md` § Relay serving, step (3)): `data` is the chunk the
    /// seat `POST`ed, `None` its `DELETE` ("I hold no such chunk"), which lets
    /// the window refill at once instead of at the fetch deadline.
    ///
    /// Taken only for an ask that is still pending **and** went to `actor` —
    /// the answering actor the route authenticated, compared by actor id
    /// alone. Anything else is [`AnnouncedAnswer::Refused`] and touches
    /// nothing: the ask stays pending for the seat that was asked. The bytes
    /// are not checked here; the walk checks every answer against its store
    /// key before serving or resting it.
    pub async fn answer_announced(
        &self,
        request_id: u64,
        actor: &[u8; 32],
        data: Option<Vec<u8>>,
    ) -> AnnouncedAnswer {
        let Some(tx) = self.take_pending(request_id, actor).await else {
            return AnnouncedAnswer::Refused;
        };
        // The receiver may already be gone (the walk settled or timed out);
        // the ask was still ours to answer, so it reads as taken.
        let _ = tx.send(SeatAnswer {
            data,
            attributed_folder: None,
        });
        AnnouncedAnswer::Taken
    }

    /// Remove and return the pending ask `request_id` iff it went to
    /// `responder`; leave the map untouched otherwise.
    async fn take_pending(
        &self,
        request_id: u64,
        responder: &[u8; 32],
    ) -> Option<oneshot::Sender<SeatAnswer>> {
        let mut pending = self.pending.lock().await;
        if pending.get(&request_id)?.responder != *responder {
            return None;
        }
        pending.remove(&request_id).map(|ask| ask.tx)
    }

    /// One announced seat's fetch — [`Self::ask`] with a
    /// `fauna.sync.chunk.wanted` push on the seat's connection, or, for a
    /// foreign seat, the federated ask its member's nest turns into that push.
    /// A non-empty answer is attributed to `folder_id` by construction (the
    /// ask named it).
    async fn fetch_announced(
        &self,
        seat: AnnouncedSeat,
        store_key_hex: String,
        folder_id: i64,
        issued: &std::sync::Mutex<Vec<u64>>,
    ) -> SeatAnswer {
        let AnnouncedSeat {
            actor,
            via,
            folder_ref,
        } = seat;
        let mut answer = self
            .ask(
                actor,
                |request_id| -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> {
                    match via {
                        SeatVia::Connection(conn) => {
                            let event = fauna_protocol::PushEvent::SyncChunkWanted(
                                fauna_protocol::push_events::SyncChunkWantedPayload {
                                    request_id,
                                    folder: folder_ref,
                                    store_key: store_key_hex,
                                    extra: Default::default(),
                                },
                            );
                            let pushed = crate::ws::WsState::push_to_connection(&conn, &event);
                            Box::pin(std::future::ready(pushed))
                        }
                        SeatVia::Forwarded(ask) => ask(request_id, store_key_hex),
                    }
                },
                issued,
            )
            .await;
        if answer.data.is_some() {
            answer.attributed_folder = Some(folder_id);
        }
        answer
    }

    /// One ask of one seat, recording its request id in `issued` **before**
    /// the wait.
    ///
    /// The recording is what makes this safe to race. Every failure arm below
    /// removes its own `pending` entry, but a future that is *dropped* mid-await
    /// — which is exactly what happens to the losers of a race — runs none of
    /// them, so its entry would sit in the map until its seat eventually
    /// answered, or forever if it never did. `issued` lets the racing caller
    /// remove them itself once the race is settled.
    ///
    /// Returns `SeatAnswer::default()` for every non-answer (failed push,
    /// dropped oneshot, timeout). That is deliberate and load-bearing for the
    /// relay walk: "this seat did not answer" is not an error, it is the
    /// ordinary case that makes the caller ask another seat.
    async fn ask<S, F>(
        &self,
        responder: [u8; 32],
        send: S,
        issued: &std::sync::Mutex<Vec<u64>>,
    ) -> SeatAnswer
    where
        S: FnOnce(u64) -> F,
        F: std::future::Future<Output = bool>,
    {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();

        self.pending
            .lock()
            .await
            .insert(request_id, PendingAsk { tx, responder });
        issued.lock().unwrap().push(request_id);

        if !send(request_id).await {
            self.pending.lock().await.remove(&request_id);
            return SeatAnswer::default();
        }

        match tokio::time::timeout(self.fetch_timeout, rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => SeatAnswer::default(),
            Err(_) => {
                self.pending.lock().await.remove(&request_id);
                SeatAnswer::default()
            }
        }
    }

    /// Drop the `pending` slots of fetches whose futures were abandoned.
    ///
    /// Takes the recorded ids and removes whatever is left of them under one
    /// lock. Ids whose own arm already cleaned up are simply absent, so this is
    /// idempotent and costs one map lookup each.
    async fn sweep_pending(&self, issued: &std::sync::Mutex<Vec<u64>>) {
        let ids = std::mem::take(&mut *issued.lock().unwrap());
        if ids.is_empty() {
            return;
        }
        let mut pending = self.pending.lock().await;
        for id in ids {
            pending.remove(&id);
        }
    }
}

/// **The one per-folder content-reachability verdict** behind
/// `fauna.media.list`'s per-item `source_online` and `fauna.sync.status`'s
/// `source_online` (owner: `file-sync.md` § Content reachability — the rule
/// lives there; this is its only computation, so the two replies cannot
/// disagree). Reachable when some holder of the folder's bytes can serve them:
/// the nest itself whenever it holds the content (residency full — the
/// default, and what an unparseable value falls back to, via the same
/// [`crate::folder_handlers::residency_of`] every projection reads), or a
/// seat the relay read path would ask: a connection that announced the
/// folder's row for relay serving
/// ([`crate::ws::WsState::has_announced_for_folder`]; `file-sync.md` § Relay
/// serving), or a foreign seat whose lease is live
/// ([`ForeignSeats::has_live_for_folder`]; § Relay serving → *A member on
/// another nest*, step (6)). No seat's place is asked.
pub fn folder_content_reachable(
    ws: &crate::ws::WsState,
    foreign: &ForeignSeats,
    fs: &crate::db::FolderRow,
) -> bool {
    crate::folder_handlers::residency_of(fs) != "metadata_only"
        || ws.has_announced_for_folder(fs.id)
        || foreign.has_live_for_folder(fs.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob_store::DiskBlobStore;
    use crate::ws::WsState;
    use bytes::Bytes;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    /// The one actor every seat in these tests announces under.
    const TEST_ACTOR: [u8; 32] = [0x5a; 32];

    /// A connection of [`TEST_ACTOR`] on `ws`, handed in as an announced seat
    /// of `local:5`, and the receiving end of its outbound queue — the seat's
    /// own socket, where every ask it is sent arrives.
    fn announced_seat(ws: &WsState) -> (AnnouncedSeat, mpsc::Receiver<Bytes>) {
        let (conn, rx) = ws.subscribe(TEST_ACTOR);
        (
            AnnouncedSeat {
                actor: TEST_ACTOR,
                via: SeatVia::Connection(conn),
                folder_ref: "local:5".into(),
            },
            rx,
        )
    }

    /// The request id of the next `fauna.sync.chunk.wanted` ask on a seat's
    /// socket, or `None` once the socket is closed.
    async fn next_ask(rx: &mut mpsc::Receiver<Bytes>) -> Option<u64> {
        loop {
            let frame = fauna_protocol::decode_frame(&rx.recv().await?).expect("a frame");
            if let fauna_protocol::Frame::Push(p) = frame
                && let fauna_protocol::PushEvent::SyncChunkWanted(w) =
                    fauna_protocol::PushEvent::from_push(&p.kind, p.payload)
            {
                return Some(w.request_id);
            }
        }
    }

    /// A store hit decodes the `encode_blob` framing before returning the chunk:
    /// the reader expects the raw chunk, and its own `BLAKE3(chunk) == hash`
    /// check fails on the stored ciphertext (the A3 fix, pinned on the one read
    /// the resolver still makes).
    #[tokio::test]
    async fn a_store_hit_decodes_encoded_blob_store_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let bs: Arc<dyn BlobStoreBackend> = Arc::new(DiskBlobStore::new(dir.path()).unwrap());

        let raw = b"the raw plaintext chunk bytes";
        let key = fauna_core::crypto::BackupKey::from_bytes([0x42u8; 32]);
        // Store the chunk the way `POST /chunks` does: encoded, keyed by the
        // plaintext hash.
        let encoded = crate::backup::encode_blob(raw, Some(&key), true).unwrap();
        let hash = fauna_core::data::ContentHash::of_raw(raw);
        bs.put(&hash, &encoded).await.unwrap();

        // No seat is handed in, so the answer can only come from the store.
        let resolver = ChunkResolver::new(Some(bs), Some(key), true);

        let result = resolver
            .relay_for_folder(&hash, 2, Vec::new(), RelayCache::Transient)
            .await
            .unwrap();
        assert_eq!(
            result,
            Some(raw.to_vec()),
            "a store hit returns the raw chunk, not the stored ciphertext"
        );
    }

    #[tokio::test]
    async fn a_store_hit_does_not_corrupt_a_raw_chunk_under_identity_config() {
        // Live config: BackupService runs with no encryption + no compression. A
        // raw chunk whose first byte is 0x00 must come back losslessly. Before
        // `encode_blob` was made self-describing this required a `store_is_raw()`
        // guard (`decode_blob` would `decompress_chunk` an un-prefixed raw chunk
        // and strip the 0x00); now `encode_blob` frames the chunk with
        // PREFIX_UNCOMPRESSED on store and `decode_blob` strips exactly that one
        // byte on read, so the round-trip is lossless with no guard.
        let dir = tempfile::tempdir().unwrap();
        let bs: Arc<dyn BlobStoreBackend> = Arc::new(DiskBlobStore::new(dir.path()).unwrap());

        // First byte 0x00 — the dangerous case decompress_chunk would mis-strip if
        // the stored bytes were un-framed.
        let raw = [0x00u8, 0xAB, 0xCD, 0xEF, 0x00, 0x11];
        // Stored the way `upload_chunk` does under the identity config: now
        // self-describing, so encode_blob(raw, None, false) == [0x00] ++ raw.
        let encoded = crate::backup::encode_blob(&raw, None, false).unwrap();
        assert_eq!(
            encoded,
            [&[0x00u8][..], &raw[..]].concat(),
            "identity encode must be self-describing (PREFIX_UNCOMPRESSED + raw)"
        );
        let hash = fauna_core::data::ContentHash::of_raw(&raw);
        bs.put(&hash, &encoded).await.unwrap();

        // Resolver mirrors the live BackupService: no key, no compression.
        let resolver = ChunkResolver::new(Some(bs), None, false);

        let result = resolver
            .relay_for_folder(&hash, 2, Vec::new(), RelayCache::Transient)
            .await
            .unwrap();
        assert_eq!(
            result,
            Some(raw.to_vec()),
            "raw chunk must not be mis-stripped"
        );
    }

    /// Drive one relay round-trip against one announced holder: spawn the
    /// relay, read the ask the holder receives (its `request_id` is the causal
    /// barrier — no settle-sleep), answer it with `answer` (`None` is a
    /// decline), and return what the relay resolved to. `Some(..)` arrives iff
    /// `answer` is bytes hashing to `hash` (the resolver's address check).
    async fn relay_round_trip(
        resolver: &Arc<ChunkResolver>,
        ws: &WsState,
        hash: fauna_core::data::ContentHash,
        folder_id: i64,
        cache: RelayCache,
        answer: Option<Vec<u8>>,
    ) -> Option<Vec<u8>> {
        let (seat, mut holder_rx) = announced_seat(ws);
        let relay = Arc::clone(resolver);
        // spawn-ok(test)
        let handle = tokio::spawn(async move {
            relay
                .relay_for_folder(&hash, folder_id, vec![seat], cache)
                .await
        });
        let request_id = next_ask(&mut holder_rx).await.expect("holder gets an ask");
        assert_eq!(
            resolver
                .answer_announced(request_id, &TEST_ACTOR, answer)
                .await,
            AnnouncedAnswer::Taken
        );
        handle.await.unwrap().unwrap()
    }

    /// Phase 5 gate 3: a **transient** relay serves the holder's bytes to the
    /// requester and leaves the blob store untouched; the **stored** relay
    /// (the historical arm) caches them. Same holder, same bytes, the policy
    /// alone decides — pinned side by side so a future refactor that folds the
    /// two arms together fails here rather than in a residency e2e.
    #[tokio::test]
    async fn a_transient_relay_serves_without_writing_the_store_and_a_stored_one_caches() {
        let dir = tempfile::tempdir().unwrap();
        let bs: Arc<dyn BlobStoreBackend> = Arc::new(DiskBlobStore::new(dir.path()).unwrap());
        let ws = WsState::new();
        let resolver = Arc::new(ChunkResolver::new(Some(bs.clone()), None, false));

        let data = b"bytes that never rest on the nest".to_vec();
        let hash = fauna_core::data::ContentHash::of_raw(&data);

        let served = relay_round_trip(
            &resolver,
            &ws,
            hash,
            1,
            RelayCache::Transient,
            Some(data.clone()),
        )
        .await;
        assert_eq!(served, Some(data.clone()), "the requester is served");
        assert!(
            !bs.exists(&hash).await.unwrap(),
            "a transient relay must leave no copy in the blob store"
        );

        let served = relay_round_trip(
            &resolver,
            &ws,
            hash,
            1,
            RelayCache::Store,
            Some(data.clone()),
        )
        .await;
        assert_eq!(served, Some(data.clone()));
        assert!(
            bs.exists(&hash).await.unwrap(),
            "the stored arm caches the relayed bytes (the full-residency re-hydration path)"
        );
    }

    /// A seat answering with bytes that are not the preimage of the requested
    /// content address is lying (or buggy). The bytes must be discarded —
    /// treated as not-found — and above all must NOT be cached: an unverified
    /// `put` would seed the GC-probed blob store with a blob whose bytes
    /// mismatch its key, indistinguishable from at-rest corruption (which
    /// fail-closes the sweep, `backup/gc.rs`).
    #[tokio::test]
    async fn fetched_bytes_that_mismatch_their_address_are_discarded_not_cached() {
        let dir = tempfile::tempdir().unwrap();
        let bs: Arc<dyn BlobStoreBackend> = Arc::new(DiskBlobStore::new(dir.path()).unwrap());
        let ws = WsState::new();
        let resolver = Arc::new(ChunkResolver::new(Some(bs.clone()), None, false));

        // The `Store` arm, so the address check is the only thing standing
        // between the lie and the store.
        let requested = fauna_core::data::ContentHash::from_digest_raw([0x5A; 32]);
        let served = relay_round_trip(
            &resolver,
            &ws,
            requested,
            2,
            RelayCache::Store,
            Some(b"lying bytes".to_vec()),
        )
        .await;
        assert_eq!(served, None, "mismatched bytes read as not-found");
        assert!(
            !bs.exists(&requested).await.unwrap(),
            "nothing may be cached under an address the bytes do not hash to"
        );
    }

    /// **`StoreIfAttributed` rests bytes on the answer's
    /// attribution, and only on it.** The arm the route takes when the hinted
    /// folder is `full` but its owner holds a `metadata_only` folder somewhere:
    /// attributed to the hinted folder → rest; anything else → serve, never
    /// rest — it falls to *do not cache*, never to `Store`. The other two arms
    /// ignore attribution.
    #[test]
    fn each_relay_cache_arm_rests_on_exactly_its_rule() {
        for (cache, attributed, rests) in [
            (RelayCache::Store, true, true),
            (RelayCache::Store, false, true),
            (RelayCache::StoreIfAttributed, true, true),
            (RelayCache::StoreIfAttributed, false, false),
            (RelayCache::Transient, true, false),
            (RelayCache::Transient, false, false),
        ] {
            assert_eq!(
                cache.rests(attributed),
                rests,
                "{cache:?} with attributed = {attributed}"
            );
        }
    }

    /// An announced seat's answer is attributed to the folder its ask named,
    /// so the `StoreIfAttributed` arm rests it — the re-hydration cache a
    /// `full` folder keeps through an announced seat.
    #[tokio::test]
    async fn an_announced_answer_is_attributed_so_store_if_attributed_rests_it() {
        let dir = tempfile::tempdir().unwrap();
        let bs: Arc<dyn BlobStoreBackend> = Arc::new(DiskBlobStore::new(dir.path()).unwrap());
        let ws = WsState::new();
        let resolver = Arc::new(ChunkResolver::new(Some(bs.clone()), None, false));

        let data = b"a chunk the asked folder accounts for".to_vec();
        let hash = fauna_core::data::ContentHash::of_raw(&data);
        let served = relay_round_trip(
            &resolver,
            &ws,
            hash,
            1,
            RelayCache::StoreIfAttributed,
            Some(data.clone()),
        )
        .await;
        assert_eq!(served, Some(data));
        assert!(bs.exists(&hash).await.unwrap());
    }

    /// **The relay asks every candidate, not the first one it is handed.**
    ///
    /// Two seats of one folder is the ordinary shape, not a corner: the seat
    /// *requesting* a chunk announces exactly like the seat holding it, so "a
    /// seat of this folder" cannot mean "the holder". Until 2026-09-02 the
    /// relay took the first match and stopped, which made a metadata-only
    /// folder's only content path a coin flip.
    ///
    /// One seat here holds nothing (it declines) and the other holds the
    /// bytes. A detached responder answers whichever seat is asked rather than
    /// the test assuming an order.
    ///
    /// Red-verified 2026-09-02: with the candidate list truncated to its first
    /// entry, the relay returns `None` whenever the walk asks the empty seat
    /// first and this fails on the served-bytes assertion.
    #[tokio::test]
    async fn a_relay_passes_over_a_seat_that_holds_nothing_and_asks_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let bs: Arc<dyn BlobStoreBackend> = Arc::new(DiskBlobStore::new(dir.path()).unwrap());
        let ws = WsState::new();
        let (empty_seat, mut empty_rx) = announced_seat(&ws);
        let (holder_seat, mut holder_rx) = announced_seat(&ws);
        let resolver = Arc::new(ChunkResolver::new(Some(bs.clone()), None, false));

        let data = b"bytes only the second seat holds".to_vec();
        let hash = fauna_core::data::ContentHash::of_raw(&data);

        let relay = Arc::clone(&resolver);
        // spawn-ok(test)
        let handle = tokio::spawn(async move {
            relay
                .relay_for_folder(
                    &hash,
                    1,
                    vec![empty_seat, holder_seat],
                    RelayCache::Transient,
                )
                .await
        });

        // A detached responder answers whichever seat is asked — a decline
        // from the empty one, the bytes from the holder — for as long as the
        // relay keeps asking. It is a responder rather than an inline wait so
        // that a relay which asks only ONE seat still COMPLETES (returning
        // `None`) and fails the assertion below; an inline "wait for the
        // holder's ask" would instead hang forever on red.
        let responder = Arc::clone(&resolver);
        let answer = data.clone();
        // spawn-ok(test)
        tokio::spawn(async move {
            loop {
                let (request_id, bytes) = tokio::select! {
                    Some(request_id) = next_ask(&mut empty_rx) => (request_id, None),
                    Some(request_id) = next_ask(&mut holder_rx) => {
                        (request_id, Some(answer.clone()))
                    }
                    else => break,
                };
                responder
                    .answer_announced(request_id, &TEST_ACTOR, bytes)
                    .await;
            }
        });

        assert_eq!(
            handle.await.unwrap().unwrap(),
            Some(data),
            "a seat that holds nothing must be passed over, not taken as the answer"
        );
        assert!(
            !bs.exists(&hash).await.unwrap(),
            "Transient still means transient — passing over a seat may not start caching"
        );
    }

    /// `count` announced seats that are connected and will never answer, and a
    /// count of the asks they receive.
    ///
    /// ⚠ The receivers are **held alive** by the returned tasks, never dropped.
    /// A dropped receiver makes the ask's push fail and the seat is written off
    /// *immediately* — which would make both tests below pass just as well
    /// against the serial walk they exist to refute. "Silent" has to mean a
    /// live seat that says nothing, which is the real shape: a requester
    /// blocked on its own in-flight download.
    fn silent_seats(
        ws: &WsState,
        count: usize,
    ) -> (
        Vec<AnnouncedSeat>,
        Arc<std::sync::atomic::AtomicUsize>,
        Vec<tokio::task::JoinHandle<()>>,
    ) {
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut seats = Vec::new();
        let mut tasks = Vec::new();
        for _ in 0..count {
            let (seat, mut rx) = announced_seat(ws);
            seats.push(seat);
            let asked = Arc::clone(&asked);
            // spawn-ok(test)
            tasks.push(tokio::spawn(async move {
                while next_ask(&mut rx).await.is_some() {
                    asked.fetch_add(1, Ordering::Relaxed);
                    // Deliberately no answer: this seat is asked and never
                    // answers, so it costs a full fetch deadline.
                }
            }));
        }
        (seats, asked, tasks)
    }

    /// The relay asks several seats at once, so its cost is one deadline per
    /// WINDOW rather than one per silent seat.
    ///
    /// Structural, not a stopwatch: it asserts that **more than one** seat has
    /// been asked *before a single fetch deadline has elapsed*. A serial walk
    /// cannot do that at any speed — its second seat is not asked until the
    /// first times out — so the assertion separates the two shapes by
    /// construction and not by measured duration (convention 14). The budget it
    /// polls within is half one deadline, which a race clears instantly and a
    /// serial walk misses by a whole deadline.
    ///
    /// ⚠ The threshold is the literal 2, **not** `RELAY_FETCH_CONCURRENCY`.
    /// Measured 2026-09-02: the first draft asserted
    /// `asked >= RELAY_FETCH_CONCURRENCY`, which makes the constant both the
    /// thing under test and the bar it is tested against — so mutating it to `1`
    /// moved the bar down with it and the test passed against the very serial
    /// walk it exists to refute. A pin whose expectation is derived from its
    /// subject cannot fail.
    #[tokio::test]
    async fn a_relay_read_is_bounded_by_the_window_not_the_seat_count() {
        const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);
        let ws = WsState::new();
        let (seats, asked, _tasks) = silent_seats(&ws, 9);
        let resolver =
            Arc::new(ChunkResolver::new(None, None, false).with_fetch_timeout(FETCH_TIMEOUT));

        let hash = fauna_core::data::ContentHash::of_raw(b"nobody holds this");
        let relay = Arc::clone(&resolver);
        // spawn-ok(test)
        let handle = tokio::spawn(async move {
            relay
                .relay_for_folder(&hash, 1, seats, RelayCache::Transient)
                .await
        });

        const CONCURRENT_ASKS: usize = 2;
        let deadline = tokio::time::Instant::now() + FETCH_TIMEOUT / 2;
        let mut seen = 0;
        while tokio::time::Instant::now() < deadline {
            seen = asked.load(Ordering::Relaxed);
            if seen >= CONCURRENT_ASKS {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        handle.abort();

        assert!(
            seen >= CONCURRENT_ASKS,
            "the relay asked {seen} seat(s) within half a fetch deadline, wanted at least \
             {CONCURRENT_ASKS}: it is walking the seats one at a time, so a folder with N \
             silent seats costs N deadlines per chunk"
        );
    }

    /// However many seats announced the folder, one relay read asks at most
    /// `RELAY_MAX_CANDIDATE_SEATS` of them.
    ///
    /// Purely structural — it counts asks, with no timing in the assertion at
    /// all. The concurrency window above bounds the deadline; this bounds the
    /// work.
    #[tokio::test]
    async fn a_relay_read_asks_no_more_seats_than_the_candidate_cap() {
        let ws = WsState::new();
        let over_cap = RELAY_MAX_CANDIDATE_SEATS + 6;
        let (seats, asked, _tasks) = silent_seats(&ws, over_cap);
        let resolver = ChunkResolver::new(None, None, false)
            .with_fetch_timeout(std::time::Duration::from_millis(50));

        let hash = fauna_core::data::ContentHash::of_raw(b"nobody holds this either");
        let served = resolver
            .relay_for_folder(&hash, 1, seats, RelayCache::Transient)
            .await
            .unwrap();

        assert_eq!(
            served, None,
            "no seat holds it, so the relay serves nothing"
        );
        assert_eq!(
            asked.load(Ordering::Relaxed),
            RELAY_MAX_CANDIDATE_SEATS,
            "one read asked more seats than the cap — {over_cap} had announced"
        );
    }

    /// A racer that LOSES leaves no `pending` entry behind.
    ///
    /// The cap test above cannot show this and neither can any all-silent one:
    /// with no winner every racer runs to completion and takes its own timeout
    /// arm, which cleans up after itself. The leak needs a seat that is still
    /// in flight when another seat wins — the loop breaks, the stream drops, and
    /// a dropped future runs none of its arms. So: two silent seats and a holder,
    /// all three inside one `RELAY_FETCH_CONCURRENCY` window, so the holder is
    /// guaranteed to be racing the other two.
    ///
    /// The fetch deadline is left at the production 30 s on purpose. If the two
    /// abandoned entries were merely *slow* to clear rather than swept, a short
    /// timeout would hide it by letting them expire during the assertion.
    #[tokio::test]
    async fn a_losing_racer_leaves_no_pending_entry_behind() {
        let ws = WsState::new();
        let (mut seats, _asked, _tasks) = silent_seats(&ws, RELAY_FETCH_CONCURRENCY - 1);
        let (holder_seat, mut holder_rx) = announced_seat(&ws);
        seats.push(holder_seat);
        let resolver = Arc::new(ChunkResolver::new(None, None, false));

        let data = b"the seat that answers first".to_vec();
        let hash = fauna_core::data::ContentHash::of_raw(&data);

        let responder = Arc::clone(&resolver);
        let answer = data.clone();
        // spawn-ok(test)
        tokio::spawn(async move {
            while let Some(request_id) = next_ask(&mut holder_rx).await {
                responder
                    .answer_announced(request_id, &TEST_ACTOR, Some(answer.clone()))
                    .await;
            }
        });

        assert_eq!(
            resolver
                .relay_for_folder(&hash, 1, seats, RelayCache::Transient)
                .await
                .unwrap(),
            Some(data),
            "the holder was racing two silent seats and must still win"
        );
        assert!(
            resolver.pending.lock().await.is_empty(),
            "the read is settled but `pending` still holds the abandoned racers' slots — a \
             losing racer is DROPPED mid-await and runs none of its own cleanup arms, so they \
             have to be swept explicitly or a busy folder leaks one entry per passed-over seat \
             per chunk"
        );
    }

    /// A seat's decline is an answer, not a hang: with no other seat to ask,
    /// the relay reads as not-found and rests nothing.
    #[tokio::test]
    async fn a_seat_declining_relays_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let bs: Arc<dyn BlobStoreBackend> = Arc::new(DiskBlobStore::new(dir.path()).unwrap());
        let ws = WsState::new();
        let resolver = Arc::new(ChunkResolver::new(Some(bs.clone()), None, false));

        let hash = fauna_core::data::ContentHash::from_digest_raw([0xFF; 32]);
        let served = relay_round_trip(&resolver, &ws, hash, 1, RelayCache::Store, None).await;
        assert_eq!(served, None);
        assert!(!bs.exists(&hash).await.unwrap(), "a decline rests nothing");
    }

    /// The content-reachability truth table (`file-sync.md` § Content
    /// reachability): the nest holding the bytes, or a connection that
    /// announced the folder's ROW — whoever's it is, since the announce
    /// admitted only owners and members — and nothing else: not a connection
    /// that announced another row, not a revoked one, no seat's place.
    #[tokio::test]
    async fn folder_content_reachable_is_nest_held_content_or_an_announced_seat_of_the_row() {
        let owner = [0x0e; 32];
        let member = [0x3a; 32];
        let folder = |metadata_only: bool| crate::db::FolderRow {
            id: 5,
            name: "photos".into(),
            actor_id: owner.to_vec(),
            nest_content_residency: metadata_only.then(|| "metadata_only".into()),
            ..Default::default()
        };
        let announce = |ws: &WsState, actor: [u8; 32], ids: Vec<i64>| {
            let (conn, rx) = ws.subscribe(actor);
            std::mem::forget(rx);
            assert!(ws.announce_serving(
                &actor,
                conn.conn_id,
                crate::ws::ServingAnnounce {
                    device_id: [0xdd; 32],
                    folder_ids: ids,
                    ..Default::default()
                },
            ));
            conn
        };

        let none = ForeignSeats::default();
        let empty = WsState::new();
        assert!(
            folder_content_reachable(&empty, &none, &folder(false)),
            "a full folder's bytes rest on the nest: reachable with no seat"
        );
        assert!(
            !folder_content_reachable(&empty, &none, &folder(true)),
            "a metadata-only folder with no seat has no holder"
        );

        let ws = WsState::new();
        announce(&ws, member, vec![6]);
        assert!(
            !folder_content_reachable(&ws, &none, &folder(true)),
            "a connection that announced another row holds nothing of this one"
        );
        let conn = announce(&ws, member, vec![6, 5]);
        assert!(
            folder_content_reachable(&ws, &none, &folder(true)),
            "a member's connection that announced the row holds it"
        );
        conn.revoke();
        assert!(
            !folder_content_reachable(&ws, &none, &folder(true)),
            "a revoked connection holds nothing"
        );

        // The foreign-seat rows (§ Relay serving → *A member on another nest*,
        // step (6)): a live lease of THIS row holds it; another row's lease,
        // a lapsed lease and a withdrawn one hold nothing.
        let foreign = ForeignSeats::default();
        let now = std::time::Instant::now();
        foreign
            .lease_at(foreign_seat(6, [0x71; 32], [0xf6; 32]), now)
            .unwrap();
        assert!(
            !folder_content_reachable(&empty, &foreign, &folder(true)),
            "a foreign seat of another row holds nothing of this one"
        );
        let seat = foreign_seat(5, [0x71; 32], [0xf6; 32]);
        foreign.lease_at(seat.clone(), now).unwrap();
        assert!(
            folder_content_reachable(&empty, &foreign, &folder(true)),
            "a foreign seat with a live lease holds the row"
        );
        assert!(
            !foreign.has_live_for_folder_at(5, now + FOREIGN_SEAT_LEASE),
            "a lapsed lease holds nothing, swept or not"
        );
        foreign.withdraw(5, &seat.member, &seat.device, &[0x99; 32]);
        assert!(
            folder_content_reachable(&empty, &foreign, &folder(true)),
            "another nest's *no longer serving* withdraws nothing"
        );
        foreign.withdraw(5, &seat.member, &seat.device, &seat.origin_nest_id);
        assert!(
            !folder_content_reachable(&empty, &foreign, &folder(true)),
            "the leasing nest's *no longer serving* ends it"
        );
    }

    fn foreign_seat(folder_id: i64, member: [u8; 32], device: [u8; 32]) -> ForeignSeat {
        ForeignSeat {
            folder_id,
            channel_id: [0xc4; 32],
            member,
            device,
            origin_nest_id: [0x0f; 32],
            nest_url: "https://member.example".into(),
        }
    }

    /// The foreign-seat lease (`federation.md` § … → *Relay serving across
    /// nests*): renewing moves the expiry, a lapsed seat is no candidate, and
    /// the table is bounded per folder and whole — a renewal of a seat already
    /// held always passes.
    #[test]
    fn a_foreign_seat_lease_renews_lapses_and_is_bounded() {
        let seats = ForeignSeats::default();
        let t0 = std::time::Instant::now();
        let seat = foreign_seat(5, [0x71; 32], [0xf6; 32]);
        seats.lease_at(seat.clone(), t0).unwrap();
        let half = t0 + FOREIGN_SEAT_LEASE / 2;
        seats.lease_at(seat.clone(), half).unwrap();
        assert_eq!(
            seats.live_for_folder_at(5, t0 + FOREIGN_SEAT_LEASE),
            vec![seat.clone()],
            "a renewal at half the lease keeps the seat past the first expiry"
        );
        assert!(
            seats
                .live_for_folder_at(5, half + FOREIGN_SEAT_LEASE)
                .is_empty(),
            "an unrenewed lease lapses by itself"
        );

        let seats = ForeignSeats::default();
        for i in 0..FOREIGN_SEATS_PER_FOLDER {
            seats
                .lease_at(foreign_seat(5, [0x71; 32], [i as u8; 32]), t0)
                .unwrap();
        }
        assert_eq!(
            seats.lease_at(foreign_seat(5, [0x72; 32], [0xff; 32]), t0),
            Err(LeaseRefused::FolderFull)
        );
        seats
            .lease_at(foreign_seat(5, [0x71; 32], [0; 32]), t0)
            .expect("a held seat always renews");
        seats
            .lease_at(foreign_seat(6, [0x72; 32], [0xff; 32]), t0)
            .expect("another folder has room");
        assert!(
            seats
                .lease_at(
                    foreign_seat(5, [0x72; 32], [0xff; 32]),
                    t0 + FOREIGN_SEAT_LEASE
                )
                .is_ok(),
            "lapsed seats free their slots"
        );

        let seats = ForeignSeats::default();
        for i in 0..FOREIGN_SEATS_MAX {
            seats
                .lease_at(foreign_seat(i as i64, [0x71; 32], [0xf6; 32]), t0)
                .unwrap();
        }
        assert_eq!(
            seats.lease_at(foreign_seat(-1, [0x71; 32], [0xf6; 32]), t0),
            Err(LeaseRefused::TableFull)
        );
    }

    /// A forwarded seat that reports *not pushed* refills the window at once:
    /// the walk does not wait out the fetch deadline on it, and leaves no
    /// pending slot behind (step (3)).
    #[tokio::test]
    async fn a_forwarded_seat_not_pushed_is_passed_over_at_once() {
        let resolver = ChunkResolver::new(None, None, false);
        let forwarded = || AnnouncedSeat {
            actor: TEST_ACTOR,
            via: SeatVia::Forwarded(Box::new(|_, _| Box::pin(std::future::ready(false)))),
            folder_ref: "foreign:aa".into(),
        };
        let hash = fauna_core::data::ContentHash::from_digest_raw([0xAB; 32]);
        let started = std::time::Instant::now();
        let served = resolver
            .relay_for_folder(
                &hash,
                5,
                vec![forwarded(), forwarded()],
                RelayCache::Transient,
            )
            .await
            .unwrap();
        assert_eq!(served, None);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "an unpushed forwarded ask settles at once, not at the 30 s deadline"
        );
        assert!(
            resolver.pending.lock().await.is_empty(),
            "an unpushed ask leaves no pending slot"
        );
    }
}
