//! WS-RPC transport for the client-side `__mls` replica store: [`MlsReplicaClient`]
//! is the typed `fauna.mls.{get,put}` call surface that seals on save and unseals
//! on load, over the object-safe [`MlsReplicaTransport`] seam (so tests inject a
//! fake nest and each web/native leg passes a thin concrete adapter over its real
//! transport). The **first and only** client consumer of the replica plane.
//!
//! **Why a `dyn` seam and not `R: RpcRequester` generics.** [`FaunaCommitGate`]
//! (`crate::gate_impl`) must implement the object-safe, native-`Send`
//! `fauna_conversations::CommitGate`, and a generic `R`'s `RpcRequester::request`
//! future (a deliberate AFIT with no `Send` bound — wasm transports are `!Send`)
//! is not provably `Send` for all `R`. The established fix is a concrete adapter
//! per transport behind an object-safe seam — exactly `ConversationsRpc` /
//! `OutboundMailSink` (`fauna_conversations::backend`). The wire encode + the
//! `fauna.mls.conflict` classification stay in this crate as the generic
//! [`rpc_transport_get`] / [`rpc_transport_put`] helpers, so each leg's adapter
//! is a two-line delegation (like `conv_rpc_error` shared by the
//! `ConversationsRpc` arms).
//!
//! Structurally `fauna_client_drafts::DraftsClient` (the `path` plane) **plus
//! an optimistic-concurrency merge-retry**: the replica carries user-irrecoverable data (own-message
//! plaintext history; live ratchet state), so `save` takes the [`ReplicaBase`]
//! precondition from day one. On a `fauna.mls.conflict` the client re-`get`s →
//! unseals → **merges** → reseals → retries, per path family:
//!
//! * **`provider`** — the three-way [`merge_provider_replicas`] against the
//!   caller-supplied last-synced base (the wrapper's dedup baseline does
//!   double duty as the merge ancestor).
//! * **`history/<hex>`** — the two-way commutative [`merge_history_slices`]
//!   (watermark = max; no base needed).
//!
//! There is **no HTTP** here.
//!
//! [`FaunaCommitGate`]: crate::gate_impl::FaunaCommitGate

use async_trait::async_trait;
use fauna_conversations::store::history::{ChannelHistorySlice, merge_history_slices};
use fauna_core::MaybeSendSync;
use fauna_core::crypto::BackupKey;
use fauna_core::identity::ActorKeypair;
use fauna_mls::error::MlsError;
use fauna_mls::state_replica::{ProviderReplica, merge_provider_replicas};
use fauna_protocol::mls_replica::{
    GetMlsReplicaReply, GetMlsReplicaRequest, KIND_GET, KIND_PUT, MAX_MLS_REPLICA_BYTES,
    PutMlsReplicaReply, PutMlsReplicaRequest, ReplicaBase,
};
use fauna_protocol::{RpcErrorClass, RpcRequester};
use serde_bytes::ByteBuf;
use zeroize::Zeroizing;

use crate::seal::{ReplicaSealError, backup_key_from_seed, seal_replica, unseal_replica};

/// The replica path for the openMLS `provider` snapshot.
pub const PATH_PROVIDER: &str = "provider";

/// The replica path for one channel's history slice.
pub fn history_path(channel_hex: &str) -> String {
    format!("history/{channel_hex}")
}

/// Max merge-retry rounds in the `save_*_cas` methods before giving up. A
/// conflict clears in a single round unless two of the user's devices are
/// hot-writing the same replica path; the bound guards a pathological live-lock.
/// Mirrors `fauna_client_config`'s `MAX_CAS_RETRIES`.
const MAX_CAS_RETRIES: usize = 8;

/// Outcome of an [`MlsReplicaTransport::put`]: the nest stored the blob, or its
/// CAS precondition failed (`fauna.mls.conflict` — another device's write landed
/// between the load and this put). Classified inside the transport adapter (via
/// [`rpc_transport_put`]) so the CAS retry loop above needs no `RpcErrorClass`
/// bound on a generic transport error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutOutcome {
    /// The blob was stored; the put's `base` matched.
    Stored,
    /// The nest rejected `fauna.mls.conflict` — re-load, merge, retry.
    Conflict,
}

/// Concrete transport failure crossing the [`MlsReplicaTransport`] seam — a
/// disconnect, deadline, or server rejection *other than* the conflict
/// classified into [`PutOutcome::Conflict`]. Carries the rendered message the
/// CAS loop and per-app glue log (mirrors `ConvRpcError`'s message-carrying
/// variants) plus the transience class the launch-retry loop
/// (`orchestration::restore_and_wire_with_retry`) branches on; the adapter
/// classifies from its concrete transport error via [`RpcErrorClass`].
#[derive(Debug)]
pub struct MlsTransportError {
    /// Rendered message from the adapter's concrete transport error.
    pub message: String,
    /// `true` when the request never got an answer from the nest (disconnect,
    /// deadline, framing — [`RpcErrorClass::is_rejection`] `== false`), so
    /// retrying the same call can succeed once the nest is reachable. `false`
    /// when the nest answered and refused — e.g. any **refusal of the
    /// kind** — where a retry cannot change
    /// the answer.
    pub transient: bool,
}

impl MlsTransportError {
    /// A transport-level fault: the nest never answered — retryable.
    pub fn fault(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: true,
        }
    }

    /// A nest-answered rejection: retrying cannot change the answer.
    pub fn rejection(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: false,
        }
    }

    /// Classify a concrete transport error at the adapter boundary: a nest
    /// rejection is permanent, everything else is a retryable fault.
    fn classify<E: RpcErrorClass + core::fmt::Display>(e: &E) -> Self {
        fauna_protocol::classify_rpc_error(e, Self::fault, Self::rejection)
    }
}

impl core::fmt::Display for MlsTransportError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for MlsTransportError {}

/// Object-safe transport seam for the `fauna.mls.{get,put}` replica plane —
/// the type-erased boundary that keeps [`MlsReplicaClient`] / `MlsStateSync` /
/// `FaunaCommitGate` non-generic (see the module doc). Each client leg
/// implements it concretely over its own transport (native `Arc<NestClient>`,
/// wasm `WsRpcClient`) by delegating to [`rpc_transport_get`] /
/// [`rpc_transport_put`], exactly as `NestConversationsRpc` / the wasm arm
/// implement `ConversationsRpc`.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait MlsReplicaTransport: MaybeSendSync {
    /// Raw `fauna.mls.get` of a path's sealed blob (opaque bytes), or `None`
    /// when nothing is stored at `path`.
    async fn get(&self, path: String) -> Result<Option<Vec<u8>>, MlsTransportError>;

    /// The `blake3` of the sealed blob stored at `path` (the [`ReplicaBase::Hash`]
    /// digest), or `None` when nothing is stored — the cheap tip probe. A leg
    /// delegates to [`rpc_transport_get_hash`] so a hash-aware nest answers
    /// without the bytes; this default fetches and hashes, which is always
    /// correct and is what a test double or an un-updated leg gets.
    async fn get_hash(&self, path: String) -> Result<Option<[u8; 32]>, MlsTransportError> {
        Ok(self.get(path).await?.map(|b| *blake3::hash(&b).as_bytes()))
    }

    /// Raw `fauna.mls.put` of an already-sealed blob under the CAS `base`.
    /// Classifies the nest's `fauna.mls.conflict` rejection into
    /// [`PutOutcome::Conflict`]; every other rejection/fault is an error.
    async fn put(
        &self,
        path: String,
        blob: Vec<u8>,
        base: ReplicaBase,
    ) -> Result<PutOutcome, MlsTransportError>;
}

/// The `fauna.mls.get` wire call, generic over the raw transport — the one
/// place the request/reply types are encoded. A leg's concrete
/// [`MlsReplicaTransport::get`] is a one-line delegation to this (the generic
/// fn is instantiated with the leg's *concrete* `R`, so its future's `Send`ness
/// is checked there, not proven for all `R`).
pub async fn rpc_transport_get<R: RpcRequester>(
    nest: &R,
    path: String,
) -> Result<Option<Vec<u8>>, MlsTransportError>
where
    R::Error: RpcErrorClass + core::fmt::Display,
{
    let reply: GetMlsReplicaReply = nest
        .request(
            KIND_GET,
            GetMlsReplicaRequest {
                path,
                hash_only: false,
                extra: Default::default(),
            },
        )
        .await
        .map_err(|e| MlsTransportError::classify(&e))?;
    Ok(reply.blob.map(|b| b.into_vec()))
}

/// The `fauna.mls.get` **hash-only probe**, generic over the raw transport —
/// the once-per-sweep "did another device write since I last looked?" question
/// behind the mid-session adoption of a sibling-joined group. The nest answers
/// with the stored blob's `blake3` and no bytes. `None` when nothing is stored.
pub async fn rpc_transport_get_hash<R: RpcRequester>(
    nest: &R,
    path: String,
) -> Result<Option<[u8; 32]>, MlsTransportError>
where
    R::Error: RpcErrorClass + core::fmt::Display,
{
    let reply: GetMlsReplicaReply = nest
        .request(
            KIND_GET,
            GetMlsReplicaRequest {
                path,
                hash_only: true,
                extra: Default::default(),
            },
        )
        .await
        .map_err(|e| MlsTransportError::classify(&e))?;
    // Every nest answers the probe with the digest (the client's
    // hash-the-blob-yourself fallback for an older nest left with the
    // compat-remnant sweep); `None` is "nothing stored".
    Ok(reply
        .hash
        .and_then(|h| <[u8; 32]>::try_from(h.as_slice()).ok()))
}

/// The `fauna.mls.put` wire call + the `fauna.mls.conflict` classification,
/// generic over the raw transport — the one place the CAS-retry signal is
/// recognized (transport-agnostically via `RpcErrorClass`). A leg's concrete
/// [`MlsReplicaTransport::put`] is a one-line delegation to this.
pub async fn rpc_transport_put<R: RpcRequester>(
    nest: &R,
    path: String,
    blob: Vec<u8>,
    base: ReplicaBase,
) -> Result<PutOutcome, MlsTransportError>
where
    R::Error: RpcErrorClass + core::fmt::Display,
{
    let put: Result<PutMlsReplicaReply, R::Error> = nest
        .request(
            KIND_PUT,
            PutMlsReplicaRequest {
                path,
                blob: ByteBuf::from(blob),
                base,
                extra: Default::default(),
            },
        )
        .await;
    match put {
        Ok(_) => Ok(PutOutcome::Stored),
        Err(e) if is_mls_conflict(&e) => Ok(PutOutcome::Conflict),
        Err(e) => Err(MlsTransportError::classify(&e)),
    }
}

/// Failure from a [`MlsReplicaClient`] load/save. Distinguishes a transport
/// failure from a seal/encode failure and the two replica-specific hard stops
/// (over-size blob, exhausted CAS retries), so per-app glue can render an
/// actionable message.
#[derive(Debug)]
pub enum MlsReplicaClientError {
    /// The `fauna.mls.{get,put}` WS-RPC call failed (disconnect, deadline,
    /// server rejection other than the conflict the retry loop handles, …).
    Transport(MlsTransportError),
    /// Sealing/unsealing the blob failed (wrong `BackupKey`, tampered/truncated
    /// bytes, corrupt zstd).
    Seal(ReplicaSealError),
    /// Encoding a `ProviderReplica` / `ChannelHistorySlice` to its canonical
    /// bytes, or decoding a loaded blob back, failed (schema drift / corruption).
    /// A present-but-undecodable blob is a hard error, never masked as "no
    /// replica" (which would let the next save clobber the user's real state).
    Codec(String),
    /// The sealed blob exceeds [`MAX_MLS_REPLICA_BYTES`] — the nest would reject
    /// it (`fauna.mls.too_large`) and the WS frame would drop it, so the client
    /// surfaces it **before** the put rather than that path silently failing to
    /// sync. A `history/<hex>` slice hitting this needs the deferred chunked
    /// history path (design § 2).
    TooLarge { path: String, size: usize },
    /// A `save_*_cas` hit `fauna.mls.conflict` on every one of its bounded
    /// retries — another of the user's devices kept winning the race. Surfaced
    /// so a pathological live-lock fails loudly rather than dropping the write.
    ConflictRetriesExhausted,
    /// The `MlsEngine` refused the operation. In practice this is
    /// `MlsError::Retired` reaching a restore: the engine was quiesced by an
    /// account hand-over while a launch retry or a mid-session resync was still
    /// in flight, and a retired engine mutates no group state
    /// (`account-data-plane.md` § Multi-instance concurrency). Surfaced rather
    /// than swallowed so the caller stalls instead of populating groups the
    /// engine can no longer persist.
    Engine(MlsError),
}

impl From<MlsError> for MlsReplicaClientError {
    fn from(e: MlsError) -> Self {
        Self::Engine(e)
    }
}

impl core::fmt::Display for MlsReplicaClientError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Engine(e) => write!(f, "mls engine refused: {e}"),
            Self::Transport(e) => write!(f, "mls replica transport: {e}"),
            Self::Seal(e) => write!(f, "mls replica seal/unseal: {e}"),
            Self::Codec(e) => write!(f, "mls replica encode/decode: {e}"),
            Self::TooLarge { path, size } => write!(
                f,
                "mls replica blob for {path} is {size} bytes, over the {MAX_MLS_REPLICA_BYTES}-byte cap"
            ),
            Self::ConflictRetriesExhausted => {
                write!(f, "mls replica save conflicted on every retry")
            }
        }
    }
}

impl MlsReplicaClientError {
    /// `true` iff retrying the same operation can succeed once the nest is
    /// reachable — exactly a [`MlsTransportError`] whose adapter classified it
    /// as a transport fault. A nest-answered rejection (any refusal of the
    /// kind), a seal/codec failure (wrong `BackupKey`,
    /// schema drift), an over-cap blob, and CAS exhaustion are all permanent:
    /// retrying re-asks a question whose answer cannot change. The launch-retry
    /// loop (`orchestration::restore_and_wire_with_retry`) branches on this.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transport(e) if e.transient)
    }
}

impl std::error::Error for MlsReplicaClientError {}

pub(crate) type ClientResult<T> = Result<T, MlsReplicaClientError>;

/// The CAS base for a loaded (or absent) blob: `Hash(blake3(sealed))` of the
/// exact bytes the nest content-addresses on, or `Absent` when nothing is
/// stored. Never a re-seal (a fresh AEAD nonce would change the bytes + hash).
pub(crate) fn base_of(sealed: &Option<Vec<u8>>) -> ReplicaBase {
    match sealed {
        Some(bytes) => ReplicaBase::Hash(*blake3::hash(bytes).as_bytes()),
        None => ReplicaBase::Absent,
    }
}

/// Typed `fauna.mls.{get,put}` call surface that seals/unseals a replica blob
/// under the owner's `BackupKey`. One instance per actor on each Fauna app;
/// cheap to hold (a transport handle + the derived key).
///
/// Like `DraftsClient`, replica blobs are owner-only
/// (no signing), so this retains only the derived `BackupKey`, not the keypair.
pub struct MlsReplicaClient {
    nest: Box<dyn MlsReplicaTransport>,
    key: BackupKey,
}

impl MlsReplicaClient {
    /// Build over a transport adapter and the user's identity keypair. The at-rest
    /// `BackupKey` is derived from the keypair's seed (BLAKE3 `derive_key`); every
    /// device in the fleet derives the same key and so can unseal what another
    /// device sealed.
    pub fn new(nest: Box<dyn MlsReplicaTransport>, keypair: &ActorKeypair) -> Self {
        let key = backup_key_from_seed(keypair.secret_bytes());
        Self { nest, key }
    }

    /// This actor's at-rest `BackupKey` — the key every seal/unseal on this
    /// client uses. Crate-internal so the succession re-seal
    /// ([`crate::succession`]) can offer a *predecessor's* key to the same
    /// primitives without the key itself leaving this crate.
    pub(crate) fn key(&self) -> &BackupKey {
        &self.key
    }

    /// Raw `fauna.mls.get` of a path's sealed blob (opaque bytes), or `None`.
    pub(crate) async fn get_blob(&self, path: &str) -> ClientResult<Option<Vec<u8>>> {
        self.nest
            .get(path.to_string())
            .await
            .map_err(MlsReplicaClientError::Transport)
    }

    /// Raw `fauna.mls.put` of an already-sealed blob under `base`. Enforces
    /// [`MAX_MLS_REPLICA_BYTES`] client-side first. Returns the
    /// [`PutOutcome`] so `save_*_cas` can retry on a classified conflict.
    pub(crate) async fn put_blob(
        &self,
        path: &str,
        sealed: Vec<u8>,
        base: ReplicaBase,
    ) -> ClientResult<PutOutcome> {
        if sealed.len() > MAX_MLS_REPLICA_BYTES {
            return Err(MlsReplicaClientError::TooLarge {
                path: path.to_string(),
                size: sealed.len(),
            });
        }
        self.nest
            .put(path.to_string(), sealed, base)
            .await
            .map_err(MlsReplicaClientError::Transport)
    }

    // ── provider path (three-way merge) ─────────────────────────────

    /// The content hash of the `provider` blob currently at the path, or `None`
    /// when nothing is stored — the tip probe a running device sends once per
    /// receive sweep to learn whether another device wrote since it last looked
    /// (`MlsStateSync::adopt_sibling_groups`).
    pub async fn provider_tip_hash(&self) -> ClientResult<Option<[u8; 32]>> {
        self.nest
            .get_hash(PATH_PROVIDER.to_string())
            .await
            .map_err(MlsReplicaClientError::Transport)
    }

    /// Fetch + unseal the `provider` snapshot together with its CAS base.
    /// `Ok((None, Absent))` on first run (no blob yet). A present-but-undecodable
    /// blob is a hard [`MlsReplicaClientError`], never masked as absent.
    pub async fn load_provider_with_base(
        &self,
    ) -> ClientResult<(Option<ProviderReplica>, ReplicaBase)> {
        let sealed = self.get_blob(PATH_PROVIDER).await?;
        let base = base_of(&sealed);
        match sealed {
            Some(bytes) => {
                let plain: Zeroizing<Vec<u8>> =
                    unseal_replica(&bytes, &self.key).map_err(MlsReplicaClientError::Seal)?;
                let replica = ProviderReplica::from_bytes(&plain)
                    .map_err(|e| MlsReplicaClientError::Codec(e.to_string()))?;
                Ok((Some(replica), base))
            }
            None => Ok((None, base)),
        }
    }

    // ── history/<hex> path (two-way commutative merge) ──────────────

    /// Fetch + unseal a channel's `history/<hex>` slice with its CAS base.
    /// `Ok((None, Absent))` when the channel has no slice yet.
    pub async fn load_history_with_base(
        &self,
        channel_hex: &str,
    ) -> ClientResult<(Option<ChannelHistorySlice>, ReplicaBase)> {
        let path = history_path(channel_hex);
        let sealed = self.get_blob(&path).await?;
        let base = base_of(&sealed);
        match sealed {
            Some(bytes) => {
                let plain: Zeroizing<Vec<u8>> =
                    unseal_replica(&bytes, &self.key).map_err(MlsReplicaClientError::Seal)?;
                let slice = ChannelHistorySlice::from_bytes(&plain)
                    .map_err(|e| MlsReplicaClientError::Codec(e.to_string()))?;
                Ok((Some(slice), base))
            }
            None => Ok((None, base)),
        }
    }
}

/// The optimistic-concurrency (CAS) + merge writes — the path that makes the
/// per-path merge actually govern `__mls` reconciliation. The nest's
/// `fauna.mls.conflict` rejection arrives pre-classified as
/// [`PutOutcome::Conflict`] (the transport adapter recognizes it via
/// `RpcErrorClass` inside [`rpc_transport_put`]), so the retry loop here
/// is transport-agnostic by construction.
impl MlsReplicaClient {
    /// Persist `current` `provider` with optimistic concurrency, never clobbering
    /// a concurrent device's write. Loads the current nest state up front and at
    /// the start of every round; whenever it differs from the running ancestor
    /// (`last_synced` first, then each just-loaded side — the correct common
    /// ancestor), it three-way-merges via [`merge_provider_replicas`] **before**
    /// the put, so a store that had already advanced past `last_synced` is merged
    /// in rather than overwritten by a coincidentally-matching CAS base. A
    /// `fauna.mls.conflict` (a write landing between load and put) re-loads and
    /// retries, bounded by [`MAX_CAS_RETRIES`]. Returns the replica actually
    /// stored (a single active writer stores its own state on the first attempt).
    pub async fn save_provider_cas(
        &self,
        current: &ProviderReplica,
        last_synced: &ProviderReplica,
    ) -> ClientResult<ProviderReplica> {
        self.save_provider_cas_reporting_hash(current, last_synced)
            .await
            .map(|(stored, _)| stored)
    }

    /// [`Self::save_provider_cas`], also returning the `blake3` of the sealed
    /// bytes that landed — the path's new tip hash, which the caller records so
    /// its next tip probe can tell its own write from a sibling's.
    pub async fn save_provider_cas_reporting_hash(
        &self,
        current: &ProviderReplica,
        last_synced: &ProviderReplica,
    ) -> ClientResult<(ProviderReplica, [u8; 32])> {
        let mut to_store = current.clone();
        let mut ancestor = last_synced.clone();
        let (mut theirs_opt, mut cas_base) = self.load_provider_with_base().await?;
        for _ in 0..MAX_CAS_RETRIES {
            // Merge only when the nest has advanced past our running ancestor.
            if let Some(theirs) = theirs_opt.as_ref().filter(|t| *t != &ancestor) {
                let out = merge_provider_replicas(&ancestor, &to_store, theirs);
                // The one consumer of `conflicted_keys`, and deliberately the
                // only one: a key both sides changed to different values that
                // the merge could NOT reconcile as same-leaf progress (two
                // devices at different points of one stream, or one lagging a
                // commit the other folded — `reconciled_keys`, silent) means
                // two writers advanced this account's single MLS device leaf,
                // the violation the engine role lock (native) and the Web Locks
                // election (web) exist to prevent — so a conflict here comes
                // from a writer neither can reach, another device above all.
                // Theirs wins and this device's values are discarded, which is
                // lossy for ratchet state; the ruling on why this is reported
                // rather than surfaced or acted on is
                // `devices.md` § Cross-device MLS group-state sync →
                // *A provider CAS conflict is reported, not repaired*.
                //
                // Loud because it is the ONLY record: nothing persists the
                // event. The winning values stay at the path — the caller's
                // baseline is its own export, so this device's next flush folds
                // them forward rather than taking the path back
                // (`the_conflict_winner_persists_past_the_losers_next_flush_unreported`)
                // — but nothing in the data marks them as a collision's
                // outcome, so a later look at the replica shows nothing. `warn`
                // clears `fauna-log`'s `info` ring filter, so the line reaches
                // every app's Settings -> Logs page. The COUNT only, never the
                // keys: openMLS storage keys carry the group id (serialised, not
                // raw — `fauna_mls::state_replica::group_id_needle`, which is why
                // a raw-bytes search of a key finds nothing) and that page is
                // user-readable and copy-to-clipboard.
                if !out.conflicted_keys.is_empty() {
                    tracing::warn!(
                        conflicted_keys = out.conflicted_keys.len(),
                        "mls-sync: two writers changed the same provider-replica keys since this \
                         device's last sync — the other writer's values won and this device's \
                         were discarded. Two devices advancing one MLS leaf concurrently is the \
                         single-writer violation the device-owned-epoch invariant forbids; this \
                         line is the only record that it happened"
                    );
                }
                if out.reconciled_keys > 0 {
                    // Same-leaf progress on both sides, resolved to the side
                    // further along — the two-device steady state, not a
                    // signal; kept out of the ring on purpose.
                    tracing::debug!(
                        reconciled_keys = out.reconciled_keys,
                        "mls-sync: provider-replica keys both devices moved were reconciled to \
                         the side further along"
                    );
                }
                to_store = out.merged;
                ancestor = theirs.clone();
            }
            let plain = Zeroizing::new(
                to_store
                    .to_bytes()
                    .map_err(|e| MlsReplicaClientError::Codec(e.to_string()))?,
            );
            let sealed = seal_replica(&plain, &self.key).map_err(MlsReplicaClientError::Seal)?;
            let tip = *blake3::hash(&sealed).as_bytes();
            match self.put_blob(PATH_PROVIDER, sealed, cas_base).await? {
                PutOutcome::Stored => return Ok((to_store, tip)),
                PutOutcome::Conflict => {
                    let (t, b) = self.load_provider_with_base().await?;
                    theirs_opt = t;
                    cas_base = b;
                }
            }
        }
        Err(MlsReplicaClientError::ConflictRetriesExhausted)
    }

    /// Seal + put `current` at the `provider` path, **replacing** the current
    /// occupant without merging it — [`crate::sync::MlsStateSync::publish_provider`]'s
    /// body, and the reasoning is there. The CAS base is still honoured (a
    /// conflict re-reads the base and puts again, bounded by
    /// [`MAX_CAS_RETRIES`]): the precondition guards against a torn write, not
    /// against replacement, which is the point. Reads the base as raw bytes on
    /// purpose — the occupant may be sealed under a key this client does not
    /// hold (a predecessor's), and it must not need to open it to replace it.
    pub async fn publish_provider(&self, current: &ProviderReplica) -> ClientResult<()> {
        let plain = Zeroizing::new(
            current
                .to_bytes()
                .map_err(|e| MlsReplicaClientError::Codec(e.to_string()))?,
        );
        let sealed = seal_replica(&plain, &self.key).map_err(MlsReplicaClientError::Seal)?;
        let mut cas_base = base_of(&self.get_blob(PATH_PROVIDER).await?);
        for _ in 0..MAX_CAS_RETRIES {
            match self
                .put_blob(PATH_PROVIDER, sealed.clone(), cas_base)
                .await?
            {
                PutOutcome::Stored => return Ok(()),
                PutOutcome::Conflict => {
                    cas_base = base_of(&self.get_blob(PATH_PROVIDER).await?);
                }
            }
        }
        Err(MlsReplicaClientError::ConflictRetriesExhausted)
    }

    /// Persist a channel's `history/<hex>` slice with optimistic concurrency,
    /// never dropping a concurrent device's own-message plaintext. Loads the
    /// current slice up front and each round and folds it in via the commutative
    /// [`merge_history_slices`] (message union, watermark = max — no ancestor
    /// needed) **before** the put, so a store that advanced past our baseline is
    /// unioned rather than overwritten. A `fauna.mls.conflict` re-loads and
    /// retries, bounded by [`MAX_CAS_RETRIES`].
    pub async fn save_history_cas(
        &self,
        current: &ChannelHistorySlice,
    ) -> ClientResult<ChannelHistorySlice> {
        let path = history_path(&current.channel_id_hex);
        let mut to_store = current.clone();
        let (mut theirs_opt, mut cas_base) =
            self.load_history_with_base(&current.channel_id_hex).await?;
        for _ in 0..MAX_CAS_RETRIES {
            if let Some(theirs) = &theirs_opt {
                to_store = merge_history_slices(&to_store, theirs);
            }
            let plain = Zeroizing::new(to_store.to_bytes().map_err(MlsReplicaClientError::Codec)?);
            let sealed = seal_replica(&plain, &self.key).map_err(MlsReplicaClientError::Seal)?;
            match self.put_blob(&path, sealed, cas_base).await? {
                PutOutcome::Stored => return Ok(to_store),
                PutOutcome::Conflict => {
                    let (t, b) = self.load_history_with_base(&current.channel_id_hex).await?;
                    theirs_opt = t;
                    cas_base = b;
                }
            }
        }
        Err(MlsReplicaClientError::ConflictRetriesExhausted)
    }
}

/// `true` if `e` is the `fauna.mls.conflict` server rejection (vs. any other
/// rejection or a transport fault) — the signal [`rpc_transport_put`] classifies
/// into [`PutOutcome::Conflict`] for `save_*_cas` to retry on.
fn is_mls_conflict<E: RpcErrorClass>(e: &E) -> bool {
    e.as_rpc_error()
        .is_some_and(|r| r.code == fauna_protocol::mls_replica::CODE_CONFLICT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_tracing::capture_tracing_at_info;
    use fauna_client_testkit::block_on;
    use fauna_protocol::RpcError;
    use fauna_protocol::mls_replica::CODE_CONFLICT;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// Error type for the fake — `Rpc(RpcError)` so `RpcErrorClass` can surface
    /// the `fauna.mls.conflict` code the CAS retry loop matches (mirrors
    /// `fauna_client_config`'s test `FakeError`), plus `Transport` so the
    /// launch-retry transience classification is testable (a fault that never
    /// reached the nest: `is_rejection == false`).
    #[derive(Debug)]
    enum FakeError {
        Rpc(RpcError),
        Transport(&'static str),
    }
    impl core::fmt::Display for FakeError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Rpc(e) => write!(f, "rpc {}", e.code),
                Self::Transport(msg) => write!(f, "transport {msg}"),
            }
        }
    }
    impl RpcErrorClass for FakeError {
        fn is_rejection(&self) -> bool {
            matches!(self, Self::Rpc(_))
        }
        fn as_rpc_error(&self) -> Option<&RpcError> {
            match self {
                Self::Rpc(e) => Some(e),
                Self::Transport(_) => None,
            }
        }
    }

    /// Stateful in-memory fake nest: a `path → sealed blob` map **enforcing the
    /// same CAS contract as the real nest handler** — `put` checks `base`
    /// against the stored blob's `blake3` (`Absent` ⇒ empty, `Hash` ⇒ match,
    /// `None` ⇒ blind) and returns `fauna.mls.conflict` on a mismatch; `get`
    /// returns the stored bytes. Exercises the *real* seal → put → get → unseal
    /// and merge-retry loop with no HTTP and no tokio. Mirrors `FakeConfigNest`
    /// but keyed per path (MLS replica is a `path` plane).
    #[derive(Default)]
    struct FakeMlsNest {
        stored: Mutex<HashMap<String, Vec<u8>>>,
        puts: Mutex<u32>,
    }

    struct SharedNest(Arc<FakeMlsNest>);

    impl RpcRequester for SharedNest {
        type Error = FakeError;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            let reply = match kind {
                KIND_PUT => {
                    let req: PutMlsReplicaRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode put");
                    let mut map = self.0.stored.lock().unwrap();
                    let current = map.get(&req.path).map(|b| *blake3::hash(b).as_bytes());
                    let ok = match &req.base {
                        ReplicaBase::Absent => current.is_none(),
                        ReplicaBase::Hash(h) => current.as_ref() == Some(h),
                    };
                    if !ok {
                        return Err(FakeError::Rpc(RpcError::new(
                            CODE_CONFLICT,
                            "error.mls.conflict",
                        )));
                    }
                    *self.0.puts.lock().unwrap() += 1;
                    map.insert(req.path, req.blob.into_vec());
                    fauna_protocol::encode_canonical(&PutMlsReplicaReply {
                        ok: true,
                        extra: Default::default(),
                    })
                }
                KIND_GET => {
                    let req: GetMlsReplicaRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode get");
                    let blob = self
                        .0
                        .stored
                        .lock()
                        .unwrap()
                        .get(&req.path)
                        .cloned()
                        .map(ByteBuf::from);
                    // A current nest carries the digest on every reply.
                    let hash = blob
                        .as_ref()
                        .map(|b| ByteBuf::from(blake3::hash(b).as_bytes().to_vec()));
                    fauna_protocol::encode_canonical(&GetMlsReplicaReply {
                        blob,
                        hash,
                        extra: Default::default(),
                    })
                }
                other => panic!("unexpected kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    /// The transport seam over the fake — exactly the two-line delegation a
    /// slice-5 leg adapter writes, so these tests also cover the shared
    /// [`rpc_transport_get`]/[`rpc_transport_put`] wire encode + the
    /// `fauna.mls.conflict` classification.
    #[async_trait]
    impl MlsReplicaTransport for SharedNest {
        async fn get(&self, path: String) -> Result<Option<Vec<u8>>, MlsTransportError> {
            rpc_transport_get(self, path).await
        }
        async fn put(
            &self,
            path: String,
            blob: Vec<u8>,
            base: ReplicaBase,
        ) -> Result<PutOutcome, MlsTransportError> {
            rpc_transport_put(self, path, blob, base).await
        }
    }

    fn keypair(seed: u8) -> ActorKeypair {
        ActorKeypair::from_secret([seed; 32])
    }

    fn client(nest: Arc<FakeMlsNest>, seed: u8) -> MlsReplicaClient {
        MlsReplicaClient::new(Box::new(SharedNest(nest)), &keypair(seed))
    }

    /// A requester whose every call fails with the injected error — drives the
    /// transience classification in `rpc_transport_{get,put}`.
    struct FailingNest(fn() -> FakeError);

    impl RpcRequester for FailingNest {
        type Error = FakeError;
        async fn request<Req, Reply>(
            &self,
            _kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            Err((self.0)())
        }
    }

    /// The launch-retry transience contract at the adapter boundary: a fault
    /// that never got an answer (`is_rejection == false`) is retryable; a
    /// nest-answered rejection — e.g. any refusal of the kind — is permanent
    /// (the client must fall back to single-device, not retry forever).
    #[test]
    fn transport_fault_is_transient_nest_rejection_is_permanent() {
        let fault = FailingNest(|| FakeError::Transport("socket closed"));
        let e = block_on(rpc_transport_get(&fault, "provider".into())).unwrap_err();
        assert!(e.transient, "no answer from the nest ⇒ retryable: {e}");
        let e = block_on(rpc_transport_put(
            &fault,
            "provider".into(),
            vec![1],
            ReplicaBase::Absent,
        ))
        .unwrap_err();
        assert!(e.transient, "no answer from the nest ⇒ retryable: {e}");

        let rejected =
            FailingNest(|| FakeError::Rpc(RpcError::new("fauna.rpc.unknown_kind", "error.rpc")));
        let e = block_on(rpc_transport_get(&rejected, "provider".into())).unwrap_err();
        assert!(!e.transient, "nest answered + refused ⇒ permanent: {e}");
        let e = block_on(rpc_transport_put(
            &rejected,
            "provider".into(),
            vec![1],
            ReplicaBase::Absent,
        ))
        .unwrap_err();
        assert!(!e.transient, "nest answered + refused ⇒ permanent: {e}");
    }

    // ── provider path fixtures (a ProviderReplica captured from a real engine) ──

    use fauna_mls::engine::MlsEngine;
    use fauna_mls::types::ChannelId;

    /// A `ProviderReplica` for a fresh group `seed` created with one peer — a
    /// realistic non-empty snapshot with group secrets in the KV.
    fn provider_replica(seed: u8) -> ProviderReplica {
        let engine = MlsEngine::new_in_memory(keypair(seed)).unwrap();
        let peer = MlsEngine::new_in_memory(ActorKeypair::from_secret([200 - seed; 32])).unwrap();
        let peer_kps = peer.generate_key_packages(1).unwrap();
        let _ = engine.create_group(&peer_kps).unwrap();
        ProviderReplica::from_engine(&engine)
    }

    #[test]
    fn provider_save_then_load_round_trips() {
        let nest = Arc::new(FakeMlsNest::default());
        let c = client(nest.clone(), 5);
        let replica = provider_replica(5);
        let (_, base) = block_on(c.load_provider_with_base()).unwrap();
        assert_eq!(base, ReplicaBase::Absent, "empty nest ⇒ Absent");
        block_on(c.save_provider_cas(&replica, &ProviderReplica::default())).unwrap();
        let (loaded, _) = block_on(c.load_provider_with_base()).unwrap();
        // ProviderReplica has no Debug (it holds group secrets), so compare its
        // byte-stable canonical encoding rather than the value directly.
        assert_eq!(
            loaded.map(|r| r.to_bytes().unwrap()),
            Some(replica.to_bytes().unwrap()),
            "provider round-trips through seal+CAS"
        );
    }

    #[test]
    fn provider_load_on_empty_is_none_absent() {
        let nest = Arc::new(FakeMlsNest::default());
        let c = client(nest, 5);
        let (loaded, base) = block_on(c.load_provider_with_base()).unwrap();
        assert!(loaded.is_none());
        assert_eq!(base, ReplicaBase::Absent);
    }

    /// A present-but-undecryptable blob (wrong key = a different identity) is a
    /// hard error, never silently "no replica" — which would let the next save
    /// clobber the user's real, just-unreadable state.
    #[test]
    fn provider_wrong_key_errors_rather_than_masking() {
        let nest = Arc::new(FakeMlsNest::default());
        let a = client(nest.clone(), 1);
        let replica = provider_replica(1);
        block_on(a.save_provider_cas(&replica, &ProviderReplica::default())).unwrap();

        let b = client(nest, 2); // different identity → different BackupKey
        // The Ok payload (ProviderReplica) has no Debug, so match rather than
        // `expect_err`.
        let result = block_on(b.load_provider_with_base());
        assert!(
            matches!(result, Err(MlsReplicaClientError::Seal(_))),
            "wrong key must Seal-error, never mask as absent"
        );
    }

    /// The crux: two devices of ONE actor write `provider` concurrently off the
    /// same stale (empty) base. Device A stores first (wins). Device B, on its
    /// stale Absent base, conflicts → re-loads, three-way-merges, retries. Both
    /// devices' groups survive in the final replica — no group dropped. With a
    /// blind LWW put, B would clobber A's group.
    #[test]
    fn provider_concurrent_writes_merge_no_group_dropped() {
        let nest = Arc::new(FakeMlsNest::default());
        let a = client(nest.clone(), 4);
        let b = client(nest.clone(), 4); // same seed ⇒ same identity + BackupKey

        // Two disjoint provider states (different groups → different KV keys).
        let replica_a = provider_replica(4);
        let replica_b = provider_replica(40);

        // Both observe the empty base.
        let (_, base_a) = block_on(a.load_provider_with_base()).unwrap();
        let (_, base_b) = block_on(b.load_provider_with_base()).unwrap();
        assert_eq!(base_a, ReplicaBase::Absent);
        assert_eq!(base_b, ReplicaBase::Absent);

        block_on(a.save_provider_cas(&replica_a, &ProviderReplica::default())).unwrap();
        // B is stale → conflict → reload + 3-way merge + retry, transparently.
        block_on(b.save_provider_cas(&replica_b, &ProviderReplica::default())).unwrap();

        let (final_opt, _) = block_on(a.load_provider_with_base()).unwrap();
        let final_replica = final_opt.unwrap();
        // Both devices' groups are present in the merged replica. ChannelId is
        // not Ord, so compare the sorted hex forms.
        let hexes = |cs: Vec<ChannelId>| {
            let mut v: Vec<String> = cs.iter().map(|c| c.to_string()).collect();
            v.sort();
            v
        };
        let mut want = replica_a.channel_ids();
        want.extend(replica_b.channel_ids());
        assert_eq!(
            hexes(final_replica.channel_ids()),
            hexes(want),
            "both concurrent groups survive the merge"
        );
    }

    /// Two devices of ONE actor that both advanced **the same group** off one
    /// shared ancestor — the shape `provider_concurrent_writes_merge_no_group_dropped`
    /// deliberately cannot produce (its two devices touch disjoint groups, so
    /// their keys never collide). All of an account's devices share one MLS
    /// leaf, so two concurrent `self_update`s write *the same* provider KV keys
    /// with different bytes: the genuine both-sides-changed conflict.
    ///
    /// Returns `(base, mine, theirs)` — the merge ancestor and the two diverged
    /// snapshots.
    fn diverged_over_one_group() -> (ProviderReplica, ProviderReplica, ProviderReplica) {
        let origin = MlsEngine::new_in_memory(keypair(7)).unwrap();
        let peer = MlsEngine::new_in_memory(ActorKeypair::from_secret([193; 32])).unwrap();
        let peer_kps = peer.generate_key_packages(1).unwrap();
        let (channel, _welcome) = origin.create_group(&peer_kps).unwrap();
        let base = ProviderReplica::from_engine(&origin);

        // Each side restores the shared ancestor and advances the SAME group's
        // own leaf independently — fresh key material per commit, so the two
        // snapshots differ on the keys they both rewrote.
        let advance = |replica: &ProviderReplica| {
            let engine = MlsEngine::new_in_memory(keypair(7)).unwrap();
            let verdict = replica.restore_into(&engine).unwrap();
            assert!(
                verdict.is_clean(),
                "same identity ⇒ the snapshot is seatable"
            );
            engine.self_update(&channel).unwrap();
            engine.merge_pending_commit(&channel).unwrap();
            ProviderReplica::from_engine(&engine)
        };
        let mine = advance(&base);
        let theirs = advance(&base);
        (base, mine, theirs)
    }

    /// **The signal is consumed.** A genuine both-sides-changed key means two
    /// writers advanced one MLS device leaf — the violation the engine role lock
    /// (native) and the Web Locks election (web) exist to prevent, arriving from
    /// a writer neither can reach. `merge_provider_replicas` resolves it
    /// theirs-wins and reports the keys; this device's own values for them are
    /// discarded. Nothing else on any plane records that it happened, so the
    /// merge must say so out loud — the same posture `devices.md` § Cross-device
    /// MLS group-state sync states for `FutureEpochCommit`.
    ///
    /// Asserted through a subscriber filtered at `info`, the filter
    /// `fauna-log`'s ring really runs: a line that passes it is a line the user
    /// (and support) can read on every app's Settings → Logs page.
    #[test]
    fn provider_cas_conflict_is_reported_never_silent() {
        let (base, mine, theirs) = diverged_over_one_group();
        let nest = Arc::new(FakeMlsNest::default());
        let sibling = client(nest.clone(), 7);
        let me = client(nest.clone(), 7);

        // The sibling device's state is what the path already holds.
        block_on(sibling.save_provider_cas(&theirs, &ProviderReplica::default())).unwrap();

        // This device saves off the shared ancestor → three-way merge → conflict.
        let (merged, lines) =
            capture_tracing_at_info(|| block_on(me.save_provider_cas(&mine, &base)).unwrap());

        let warned: Vec<&String> = lines
            .iter()
            .filter(|l| l.starts_with("[WARN]") && l.contains("two writers"))
            .collect();
        assert_eq!(
            warned.len(),
            1,
            "the two-writer collision warns exactly once at the default info filter; got {lines:?}"
        );

        // The report is real, not a false positive: theirs genuinely won keys
        // this device had changed.
        assert_ne!(
            merged.to_bytes().unwrap(),
            mine.to_bytes().unwrap(),
            "theirs won at least one key this device had changed"
        );
    }

    /// A conflict-free merge stays quiet — the severity of the line above only
    /// means something if the ordinary disjoint-groups merge does not raise it.
    #[test]
    fn a_disjoint_merge_does_not_warn() {
        let nest = Arc::new(FakeMlsNest::default());
        let a = client(nest.clone(), 4);
        let b = client(nest.clone(), 4);
        block_on(a.save_provider_cas(&provider_replica(4), &ProviderReplica::default())).unwrap();

        let (_stored, lines) = capture_tracing_at_info(|| {
            block_on(b.save_provider_cas(&provider_replica(40), &ProviderReplica::default()))
                .unwrap()
        });
        assert!(
            !lines.iter().any(|l| l.starts_with("[WARN]")),
            "disjoint groups merge by union — no conflict, no warning; got {lines:?}"
        );
    }

    /// **Why the log is the ONLY record — and what the loser's next flush does.**
    /// The merged replica is never restored into the running engine, so this
    /// device's next flush exports the same state it just lost the conflict
    /// with. Its ancestor is its OWN last export — `MlsStateSync::
    /// save_provider_folded_if_changed` keeps `provider_base` at what the engine
    /// authored, never at a merge result the engine has not adopted — so the
    /// path is seen as advanced past the ancestor and merged again: only one
    /// side changed since that ancestor, the sibling's winning values persist,
    /// and nothing is reported a second time. The collision has still left no
    /// trace in the data: the path holds a plausible replica, not a marker.
    ///
    /// Until 2026-09-01 the ancestor was the merge result, so this flush met a
    /// path equal to its ancestor and took it back wholesale — which also
    /// dropped every key a sibling had saved on a conflict-FREE merge. Pinned here so the ruling in `devices.md`
    /// rests on a demonstrated property rather than a reading, and so a future
    /// session that changes it must come past this test.
    #[test]
    fn the_conflict_winner_persists_past_the_losers_next_flush_unreported() {
        let (base, mine, theirs) = diverged_over_one_group();
        let nest = Arc::new(FakeMlsNest::default());
        let sibling = client(nest.clone(), 7);
        let me = client(nest.clone(), 7);

        block_on(sibling.save_provider_cas(&theirs, &ProviderReplica::default())).unwrap();
        let merged = block_on(me.save_provider_cas(&mine, &base)).unwrap();

        // The next autosave: the engine is unchanged, so it re-exports `mine`;
        // the ancestor is this device's own last export, `mine` itself.
        let (again, lines) =
            capture_tracing_at_info(|| block_on(me.save_provider_cas(&mine, &mine)).unwrap());

        assert_eq!(
            again.to_bytes().unwrap(),
            merged.to_bytes().unwrap(),
            "the sibling's winning values stay at the path"
        );
        assert_ne!(
            again.to_bytes().unwrap(),
            mine.to_bytes().unwrap(),
            "the loser does NOT take the path back"
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("[WARN]")),
            "only one side changed since the ancestor — no second report; got {lines:?}"
        );
    }

    // ── history path fixtures ───────────────────────────────────────

    use fauna_conversations::address::{Rail, TypedAddress};
    use fauna_conversations::keying::ThreadKey;
    use fauna_conversations::message::{MessageBadges, MessageId, MessageSnapshot};
    use fauna_conversations::store::threads::ThreadStore;
    use fauna_conversations::thread::ThreadFlavor;
    use fauna_core::render::RenderDocument;

    const CH: &str = "aa11";

    fn addr(name: &str) -> TypedAddress {
        TypedAddress::Email {
            email_address: format!("{name}@example.com"),
        }
    }

    fn msg(id: &str, body: &str, is_own: bool, ts: i64) -> MessageSnapshot {
        MessageSnapshot {
            message_id: MessageId(id.to_string()),
            sender: addr(if is_own { "me" } else { "peer" }),
            sender_display: String::new(),
            body: body.to_string(),
            document: RenderDocument::default(),
            timestamp_ms: ts,
            subject_line: None,
            badges: MessageBadges::default(),
            reply_to: None,
            reactions: vec![],
            deleted: false,
            is_own,
            legal_takedown_ref: None,
            labels: vec![],
            plane_ref: None,
            can_delete: false,
        }
    }

    fn slice_with(messages: Vec<MessageSnapshot>, watermark: i64) -> ChannelHistorySlice {
        let store = ThreadStore::new();
        let id = store.find_or_create(
            ThreadKey::Channel {
                rail: Rail::FaunaMls,
                channel_id_hex: CH.to_string(),
            },
            vec![addr("me"), addr("peer")],
            ThreadFlavor::OneToOne,
            Some("peer".to_string()),
        );
        for m in messages {
            store.append_message(&id, m);
        }
        store.snapshot_channel_slice(&id, CH, watermark).unwrap()
    }

    #[test]
    fn history_save_then_load_round_trips() {
        let nest = Arc::new(FakeMlsNest::default());
        let c = client(nest, 7);
        let slice = slice_with(vec![msg(&format!("conv:{CH}:1"), "hi", false, 10)], 1);
        block_on(c.save_history_cas(&slice)).unwrap();
        let (loaded, _) = block_on(c.load_history_with_base(CH)).unwrap();
        assert_eq!(loaded, Some(slice));
    }

    /// Two devices append different own messages concurrently; the CAS conflict
    /// resolves by the commutative history union (watermark = max) — neither
    /// device's own-message plaintext is lost (own history is user-irrecoverable).
    #[test]
    fn history_concurrent_writes_union_no_message_dropped() {
        let nest = Arc::new(FakeMlsNest::default());
        let a = client(nest.clone(), 8);
        let b = client(nest.clone(), 8);

        let shared = msg(&format!("conv:{CH}:1"), "shared", false, 10);
        let slice_a = slice_with(
            vec![
                shared.clone(),
                msg(&format!("conv:{CH}:2"), "from A", true, 20),
            ],
            2,
        );
        let slice_b = slice_with(
            vec![shared, msg(&format!("conv:{CH}:3"), "from B", true, 30)],
            3,
        );

        block_on(a.save_history_cas(&slice_a)).unwrap();
        block_on(b.save_history_cas(&slice_b)).unwrap(); // conflict → union

        let (final_opt, _) = block_on(a.load_history_with_base(CH)).unwrap();
        let ids: Vec<String> = final_opt
            .unwrap()
            .messages
            .iter()
            .map(|m| m.message_id.0.clone())
            .collect();
        for want in [
            format!("conv:{CH}:1"),
            format!("conv:{CH}:2"),
            format!("conv:{CH}:3"),
        ] {
            assert!(
                ids.contains(&want),
                "{want} dropped from the union: {ids:?}"
            );
        }
    }

    /// An over-`MAX_MLS_REPLICA_BYTES` sealed blob is rejected client-
    /// side **before** the put (no wasted round-trip), not silently frame-dropped.
    #[test]
    fn oversize_history_slice_is_too_large_before_put() {
        let nest = Arc::new(FakeMlsNest::default());
        let c = client(nest.clone(), 9);
        // An incompressible body large enough that even after zstd the sealed
        // blob exceeds MAX_MLS_REPLICA_BYTES. blake3 XOF → a base64-alphabet
        // string (~6 bits entropy/char, so zstd cannot shrink it below the cap).
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut reader = {
            let mut h = blake3::Hasher::new();
            h.update(b"mls-replica-oversize-fixture");
            h.finalize_xof()
        };
        let mut raw = vec![0u8; 4 * 1024 * 1024];
        reader.fill(&mut raw);
        let body: String = raw
            .iter()
            .map(|b| ALPHABET[(b & 0x3f) as usize] as char)
            .collect();
        let slice = slice_with(vec![msg(&format!("conv:{CH}:1"), &body, true, 1)], 1);
        let err = block_on(c.save_history_cas(&slice)).expect_err("over-cap must reject");
        assert!(
            matches!(err, MlsReplicaClientError::TooLarge { .. }),
            "got {err}"
        );
        assert_eq!(*nest.puts.lock().unwrap(), 0, "no put reached the nest");
    }
}
