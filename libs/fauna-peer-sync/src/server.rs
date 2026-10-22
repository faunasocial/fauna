//! The serve side: an admitted peer walks this replica's relay plane and
//! pulls blocks it lacks — the peer analog of the nest's feed serve
//! (`account-data-plane.md` § The peer leg; the wire kinds are
//! `fauna_protocol::peer_sync` + the reused `fauna.sync.changes.list`).
//!
//! # The allowlist (wormability rule 3)
//!
//! A connection's serve set is **exactly** [`allowlisted_kinds`]:
//! `fauna.peer.node_info` (the pre-witness probe), the admission exchange,
//! `fauna.sync.changes.list`, `fauna.peer.sync.blocks.pull`,
//! `fauna.peer.sync.chunks.pull` (a sibling's file bodies), and the folder
//! row pull `fauna.peer.sync.changes.list` + `fauna.peer.sync.manifests.get`
//! (a sibling's folder rows and the manifests they name). Nothing
//! else is mapped, so `PeerChannel::serve` answers every other kind
//! `fauna.protocol.unknown_kind` — no config, capability-mint, admin, or
//! key-material kind exists on this dispatcher, and class-2 state rides only
//! as sealed *content* inside the transfer kinds.
//!
//! # The two feed arms
//!
//! `fauna.sync.changes.list` serves the relay plane's two item classes:
//! `state-entry` (class-2 — sealed envelopes with per-device origin
//! coordinates) and `record-cid` (class-1 — a content scope's coordinate
//! rows, entry-less: the coordinates ARE the payload, and the bytes travel
//! separately by want-list pull). A record-cid request names its content
//! scope explicitly and may carry the one-writer scalar cursor instead of a
//! frontier vector ("an omitted frontier is `{nest: since}`" — the feed
//! contract). The file-row request (no `item_class` — the sync engine's own file
//! pull) stays nest-mediated and is refused loudly.
//!
//! # The store-serving half
//!
//! The feed serve and the store owner thread live in
//! [`crate::store_serve`] — one implementation the custodian-NEST serve door
//! calls too, so the two serve sides cannot drift. This module keeps only the
//! peer channel's own half: admission, the block and chunk pulls, metering.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::Result;
use fauna_core::data::ContentHash;
use fauna_core::device_endpoints::DeviceEndpoints;
use fauna_core::encoding::EmbedAsBytes;
use fauna_peer_channel::{HandlerFactory, PeerHandlers, PeerNode, base_peer_handlers, from_value};
use fauna_protocol::peer_row::{LocalShareChange, PeerChangeRow, serves_held_row};
use fauna_protocol::peer_sync::{
    ERR_NOT_ADMITTED, ERR_OVER_QUOTA, ERR_UNSUPPORTED, ERR_WITNESS_REFUSED, KIND_PEER_SYNC_ADMIT,
    KIND_PEER_SYNC_BLOCKS_PULL, KIND_PEER_SYNC_CHANGES_LIST, KIND_PEER_SYNC_CHUNKS_PULL,
    KIND_PEER_SYNC_MANIFESTS_GET, PeerSyncAdmitReply, PeerSyncAdmitRequest, PeerSyncBlock,
    PeerSyncBlocksPullReply, PeerSyncBlocksPullRequest, PeerSyncChangesListReply,
    PeerSyncChangesListRequest, PeerSyncChunk, PeerSyncChunksPullReply, PeerSyncChunksPullRequest,
    PeerSyncManifest, PeerSyncManifestsGetReply, PeerSyncManifestsGetRequest,
};
use fauna_protocol::sync::SyncChangesListRequest;
use fauna_protocol::{RpcError, Value};
use fauna_transport::{EndpointKey, PeerTransport};

use crate::admission::{
    AdmissionVerdict, AdmittedConnection, admitted_verdict, claimed_account, evaluate_witness,
};
use crate::discovery::peer_sync_enabled;
use crate::quota::{MeteredPlane, QuotaConfig, QuotaLedger, metered_handler_factory};

/// Injected clock (epoch seconds) — verdict validity and quota windows never
/// read the wall clock directly, so tier-1 tests drive both (convention 14).
pub type NowFn = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The serve side's custody-revocation store, as a predicate over the grant
/// id: `true` = revoked. A fleet replica answers it from the synced
/// grant-event log (T13's two-stores rule); the runtime refreshes the backing
/// snapshot each pump pass, which IS the ratified honest bound ("a fleet
/// replica that has not yet synced the revoke may serve for one config-sync
/// convergence window"). Consulted at the admit door AND on every request of
/// the connection the grant admitted, so a revoke severs a live connection at
/// its next request.
///
/// **A predicate that cannot answer yet answers `true`.** Both views here are
/// read live from a snapshot the evaluator refreshes; before its first
/// successful refresh it knows nothing, and "nothing known" must refuse —
/// an empty set would admit every withdrawn witness (the runtime's
/// `fauna_sync_engine::peer_leg::WithdrawalSnapshot` owns that posture).
pub type CustodyRevocationFn = Arc<dyn Fn(&[u8]) -> bool + Send + Sync>;

/// The serve side's **removed-device view**, as a predicate over
/// `(account, device key)`: `true` = that account's fleet carries a `Removed`
/// row for that device. The device twin of [`CustodyRevocationFn`], and
/// keyed by account because the serve side is a registry — one node answers
/// for its own account and for every account it holds custody of, and a
/// global device-key predicate would conflate their fleets.
///
/// A fleet replica answers it from the merged `fauna.state.device-set` rows
/// of the served account (`fauna_core::generation::FleetView`'s exclusion
/// set); the runtime refreshes the backing snapshot each pump pass, which IS
/// the honest bound here exactly as it is for custody revocation — and, as
/// there, a view whose snapshot has never derived answers `true`.
///
/// **It answers `false` on an absent row, and that is load-bearing twice.**
/// (a) *Version skew:* a device enrolled before the plane-side device-set
/// existed has no row at all, so refusing on "not present" would sever every
/// such sibling; refusing on `Removed` **only** keeps the check additive.
/// (b) *The custodied account:* a custodian evaluating an owner device's
/// `DeviceAuthorization` for a custodied account cannot read that account's
/// sealed `state-fleet` rows, so its only exclusion set there is the
/// owner-signed list on the custody grants it holds
/// ([`crate::admission::CustodiedExclusions`]). A device a held grant lists
/// is refused; one minted before the removal lists nothing, and an account
/// whose grants list nothing admits — failing closed
/// would sever custody serving wholesale, exactly as `custody_revoked: None`
/// would. The bound is stated in `account-sync-plane.md` § The admission
/// seam → *Validity and severance*; the owner's own siblings, which hold the
/// rows, sever at their next evaluation regardless.
pub type DeviceRemovedFn = Arc<dyn Fn(&[u8; 32], &[u8; 32]) -> bool + Send + Sync>;

/// The serve side's **file-body source**: one stored chunk body of a file-sync
/// folder, by the folder's `FolderRef` wire string and the chunk's store key —
/// `None` when this device holds no body for it. The host answers it through
/// the one serve core (`fauna_sync_engine::engine::SyncEngine::serve_chunk`,
/// reached through the host's relay seat), so the peer leg is that core's
/// further consumer, never a second serve path (`file-sync.md` § Relay
/// serving). It never fetches on the asker's behalf.
pub type FileChunkFn = Arc<
    dyn Fn(
            String,
            [u8; 32],
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Vec<u8>>> + Send>>
        + Send
        + Sync,
>;

/// The serve side's **folder row source**: a file-sync folder's change rows
/// with `seq` strictly greater than `since`, in seq order, pending rows on the
/// tail, at most `max_rows` — by the folder's `FolderRef` wire string. The
/// host answers it from the folder's own state DB
/// (`fauna_sync_engine::peer_share_store::SyncDbShareStore::changes_since`);
/// a folder no engine here runs answers an empty page. The rows may include
/// ones this side must not serve — the server applies the provenance ruling
/// ([`fauna_protocol::peer_row::serves_held_row`]), so a host never encodes it.
pub type FolderChangesFn = Arc<
    dyn Fn(
            String,
            i64,
            u32,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = anyhow::Result<Vec<LocalShareChange>>> + Send>,
        > + Send
        + Sync,
>;

/// The serve side's **folder manifest source**: canonical `ChunkManifest`
/// bytes the folder's replica retained, by the folder's `FolderRef` wire
/// string and the manifest's content hash — `None` when it retains none.
/// Self-verifying bytes only (the receiver re-hashes them); the share leg's
/// re-derive fallback needs the M2 keys and stays there.
pub type FolderManifestFn = Arc<
    dyn Fn(
            String,
            [u8; 32],
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Vec<u8>>> + Send>>
        + Send
        + Sync,
>;

/// The kinds this dispatcher serves — the whole allowlist (rule 3). Public so
/// the compliance test asserts the *set*, not a sample.
pub const fn allowlisted_kinds() -> [&'static str; 7] {
    [
        fauna_protocol::peer::KIND_PEER_NODE_INFO,
        KIND_PEER_SYNC_ADMIT,
        "fauna.sync.changes.list",
        KIND_PEER_SYNC_BLOCKS_PULL,
        KIND_PEER_SYNC_CHUNKS_PULL,
        KIND_PEER_SYNC_CHANGES_LIST,
        KIND_PEER_SYNC_MANIFESTS_GET,
    ]
}

/// The block-pull reply's byte budget (blocks over it are `deferred`), sized
/// under the peer channel's 1 MiB frame (`fauna_peer_channel::MAX_FRAME_LEN`).
/// The feed page's own bounds live with the feed serve
/// ([`crate::store_serve`]). A Rust constant — never a knob.
const MAX_BLOCK_BYTES_PER_PULL: usize = 700 * 1024;

pub use crate::store_serve::ServeStoreHandle;

// ── The server ───────────────────────────────────────────────────────────────

/// Everything the serve side needs beyond the store.
pub struct PeerSyncServerConfig {
    /// The human label answered on `fauna.peer.node_info`.
    pub display_name: String,
    /// This device's own admission witness — presented back in every admit
    /// reply, so one round trip yields mutual (independently evaluated)
    /// admission.
    pub own_witness: EmbedAsBytes,
    /// Which witness kind [`Self::own_witness`] is — named in the reply so
    /// the other side dispatches its verification by name, never by shape
    /// (the sides' kinds may differ; a custodian's own witness is a custody
    /// grant while a fleet listener's is a `DeviceAuthorization`).
    pub own_witness_kind: String,
    /// The custody-revocation store this serve side answers from. **`None`
    /// fail-closed-refuses every custody-grant witness**: T13 requires a
    /// fleet replica to evaluate custody admission against the synced
    /// grant-event log, so a serve side with no revocation view cannot
    /// perform the evaluation at all — wiring this predicate is what
    /// *enables* custody serving (the W8.5 (account-data-plane.md § Workstreams) runtime leg), never an optional
    /// hardening on top of it.
    pub custody_revoked: Option<CustodyRevocationFn>,
    /// The removed-device view this serve side answers from ([`DeviceRemovedFn`]).
    ///
    /// **`None` admits** — the opposite posture to `custody_revoked`'s, and
    /// deliberately so: a custody witness cannot be evaluated *at all*
    /// without a revocation view, whereas a `DeviceAuthorization` witness is
    /// fully evaluated by its verifier and this view only ever *withdraws*
    /// one. Failing closed here would refuse every sibling of every build
    /// that has not wired a view — the whole same-account peer leg. The
    /// production wiring always passes `Some` (`fauna-sync-engine`'s
    /// `peer_leg::ensure_bound`).
    pub device_removed: Option<DeviceRemovedFn>,
    /// Where `fauna.peer.sync.chunks.pull` reads file bodies ([`FileChunkFn`]).
    /// `None` — a host running no file-sync engines — answers every want
    /// `missing`, which sends the puller to the nest: a refusal would cost the
    /// same and say less.
    pub file_chunks: Option<FileChunkFn>,
    /// Where `fauna.peer.sync.changes.list` reads a folder's rows
    /// ([`FolderChangesFn`]). `None` — no file-sync engines here — answers an
    /// empty page, never an error, as the chunk door answers `missing`.
    pub folder_changes: Option<FolderChangesFn>,
    /// Where `fauna.peer.sync.manifests.get` reads a folder's retained
    /// manifests ([`FolderManifestFn`]). `None` answers every hash `missing`.
    pub folder_manifests: Option<FolderManifestFn>,
    pub quotas: QuotaConfig,
    pub now: NowFn,
}

/// One served account's serve-side state: its store, and the witness this
/// node answers a dialer of that account WITH (the admit reply's mutual
/// half). For the own account that is the machine's `DeviceAuthorization`;
/// for a custodied account it is THE CUSTODY GRANT itself — an owner-fleet
/// device dialing its custodian verifies exactly that the peer is entitled
/// to hold the account's data, and the custodian's own cert (a different
/// account's!) could never verify for it (the admit seam's "sides' kinds
/// may differ" sentence, made concrete).
#[derive(Clone)]
pub struct ServedAccount {
    pub store: ServeStoreHandle,
    pub reply_witness: EmbedAsBytes,
    pub reply_witness_kind: String,
}

/// The peer-sync serve side — one node serving its OWN account's store and,
/// on a machine holding custody, every custodied account's store (W8.5 P1:
/// the registry lives inside the server; the single-account
/// `verdict.account != self.account` refusal generalized to registry lookup,
/// it did not weaken).
pub struct PeerSyncServer {
    /// The served accounts: this account's own store plus every custodied
    /// store. The own entry is inserted at construction and never leaves;
    /// the custodied entries are pump-fed truth ([`Self::set_custodied`] —
    /// a custody accepted this pass serves on the next, exactly like
    /// endpoint facts).
    served: RwLock<HashMap<[u8; 32], ServedAccount>>,
    /// This machine's OWN account — the only account custody-grant
    /// witnesses may claim (a custodian serves only the custodied account's
    /// FLEET, so custody-of-custody chains are refused at admission).
    account: [u8; 32],
    config: PeerSyncServerConfig,
    ledger: QuotaLedger,
    /// This device's own dial identity + candidates, answered in every admit
    /// reply (T13 step 4). Pump-fed, exactly like the served set.
    own_endpoints: RwLock<Option<DeviceEndpoints>>,
    /// What dialers carried in their own half, keyed by the channel-proven
    /// key — drained by the pump into `fauna.state.custodian-endpoints`.
    observed_endpoints: Mutex<HashMap<[u8; 32], DeviceEndpoints>>,
}

impl PeerSyncServer {
    pub fn new(store: ServeStoreHandle, account: [u8; 32], config: PeerSyncServerConfig) -> Self {
        let ledger = QuotaLedger::new(config.quotas.clone());
        let own = ServedAccount {
            store,
            reply_witness: config.own_witness.clone(),
            reply_witness_kind: config.own_witness_kind.clone(),
        };
        Self {
            served: RwLock::new(HashMap::from([(account, own)])),
            account,
            config,
            ledger,
            own_endpoints: RwLock::new(None),
            observed_endpoints: Mutex::new(HashMap::new()),
        }
    }

    /// Publish this device's current dial identity + candidates, answered in
    /// every admit reply (T13 step 4's responder half). Pump-fed truth, like
    /// [`Self::set_custodied`]: the admit path only ever *reads* this snapshot,
    /// so a re-exchange costs the handler no I/O (the ack-path rule).
    pub fn set_own_endpoints(&self, endpoints: Option<DeviceEndpoints>) {
        *self.own_endpoints.write().unwrap() = endpoints;
    }

    /// Take everything dialers carried since the last drain, keyed by the
    /// **channel-proven** key (never the value's self-named `node_id` — see
    /// [`crate::discovery::bind_carried_endpoints`]).
    ///
    /// Draining rather than reading keeps the write-back on the pump's own
    /// path: the admit handler records a fact, the pump turns it into a sealed
    /// registry row through the proper writer door.
    pub fn drain_observed_endpoints(&self) -> HashMap<[u8; 32], DeviceEndpoints> {
        std::mem::take(&mut *self.observed_endpoints.lock().unwrap())
    }

    /// Replace the CUSTODIED serve set (the own account's entry is
    /// untouchable). The pump feeds this each pass from the machine's
    /// `custodies-held` rows; a custody revoked owner-side stops being
    /// dialed by the owner regardless, and dropping its entry here stops
    /// this side serving it at the next admission evaluation.
    pub fn set_custodied(&self, custodied: HashMap<[u8; 32], ServedAccount>) {
        let mut served = self.served.write().unwrap();
        let own = served
            .get(&self.account)
            .cloned()
            .expect("the own entry is constructed in and never removed");
        served.clear();
        served.insert(self.account, own);
        for (account, entry) in custodied {
            if account != self.account {
                served.insert(account, entry);
            }
        }
    }

    /// The served account set — what admission may route to (own +
    /// custodied). A snapshot; the registry moves only between pump passes.
    fn is_served(&self, account: &[u8; 32]) -> bool {
        self.served.read().unwrap().contains_key(account)
    }

    /// The serve-side entry for one admitted account.
    fn served_entry(&self, account: &[u8; 32]) -> Option<ServedAccount> {
        self.served.read().unwrap().get(account).cloned()
    }

    /// The per-connection handler factory for [`PeerNode::start_with`] — the
    /// shared rule-8 door ([`metered_handler_factory`]) over this plane's
    /// serve set. Sides admit independently: the verdict slot lives in
    /// [`Self::connection_handlers`], minted fresh per connection.
    pub fn handler_factory(self: &Arc<Self>) -> HandlerFactory {
        metered_handler_factory(self, Self::connection_handlers)
    }

    /// The serve set for one connection — exactly [`allowlisted_kinds`].
    fn connection_handlers(self: &Arc<Self>, peer: EndpointKey) -> PeerHandlers {
        let verdict: Arc<Mutex<Option<AdmittedConnection>>> = Arc::new(Mutex::new(None));

        let admit_server = Arc::clone(self);
        let admit_slot = Arc::clone(&verdict);
        let list_server = Arc::clone(self);
        let list_slot = Arc::clone(&verdict);
        let pull_server = Arc::clone(self);
        let pull_slot = Arc::clone(&verdict);
        let chunks_server = Arc::clone(self);
        let chunks_slot = Arc::clone(&verdict);
        let rows_server = Arc::clone(self);
        let rows_slot = Arc::clone(&verdict);
        let manifests_server = Arc::clone(self);
        let manifests_slot = Arc::clone(&verdict);

        base_peer_handlers(self.config.display_name.clone())
            .on(KIND_PEER_SYNC_ADMIT, move |req| {
                let server = Arc::clone(&admit_server);
                let slot = Arc::clone(&admit_slot);
                async move { server.handle_admit(&peer, &slot, req.payload).await }
            })
            .on("fauna.sync.changes.list", move |req| {
                let server = Arc::clone(&list_server);
                let slot = Arc::clone(&list_slot);
                async move { server.handle_changes_list(&peer, &slot, req.payload).await }
            })
            .on(KIND_PEER_SYNC_BLOCKS_PULL, move |req| {
                let server = Arc::clone(&pull_server);
                let slot = Arc::clone(&pull_slot);
                async move { server.handle_blocks_pull(&peer, &slot, req.payload).await }
            })
            .on(KIND_PEER_SYNC_CHUNKS_PULL, move |req| {
                let server = Arc::clone(&chunks_server);
                let slot = Arc::clone(&chunks_slot);
                async move { server.handle_chunks_pull(&peer, &slot, req.payload).await }
            })
            .on(KIND_PEER_SYNC_CHANGES_LIST, move |req| {
                let server = Arc::clone(&rows_server);
                let slot = Arc::clone(&rows_slot);
                async move {
                    server
                        .handle_folder_changes_list(&peer, &slot, req.payload)
                        .await
                }
            })
            .on(KIND_PEER_SYNC_MANIFESTS_GET, move |req| {
                let server = Arc::clone(&manifests_server);
                let slot = Arc::clone(&manifests_slot);
                async move {
                    server
                        .handle_folder_manifests_get(&peer, &slot, req.payload)
                        .await
                }
            })
    }

    /// Meter one request and read the connection's verdict, refusing when
    /// absent, expired, or naming an account this node does not serve. The
    /// scope-less preflight — what `blocks.pull` runs, its scope enforcement
    /// being per **block** below (a scope-bounded `Named` verdict must reach
    /// that check, never be refused wholesale up here). Returns the verdict
    /// AND its account's serve store — the multi-account routing (a verdict
    /// for a custodied account serves from the custodied store).
    fn admitted_connection(
        &self,
        peer: &EndpointKey,
        slot: &Mutex<Option<AdmittedConnection>>,
    ) -> Result<(AdmissionVerdict, ServeStoreHandle), RpcError> {
        let now = (self.config.now)();
        let admitted = admitted_verdict(
            peer,
            slot,
            &self.ledger,
            now,
            over_quota,
            || not_admitted("no admission verdict on this connection"),
            // The seam's validity rule: a connection outliving its witness's
            // expiry re-presents before continuing past it.
            || not_admitted("the admission witness has expired — re-present it"),
        )?;
        let verdict = admitted.verdict;
        // Device removal, per REQUEST — the same shape the served-account
        // check below has: severance is per request, never per
        // connection. That is also why removal
        // cannot merely bound the *next* handshake — a fleet cert never
        // expires, so a live connection that outlived the removal would
        // otherwise keep pulling until the peer hung up. Binds to the
        // `DeviceAuthorization` arm alone, via the key the slot recorded at
        // admission.
        if let (Some(device_key), Some(removed)) =
            (&admitted.device_key, &self.config.device_removed)
            && removed(&verdict.account, device_key)
        {
            return Err(not_admitted(
                "the device is removed from this account's fleet",
            ));
        }
        // Custody revocation, per REQUEST — the custody twin of the check
        // above, since the property binds both arms: a custodian whose grant
        // the owner revoked would otherwise keep pulling over a connection it
        // never closes, until the grant's own `expires_at`. Binds to the
        // grant id the slot recorded at admission; with no revocation view at
        // all it refuses, as the admit door does (a custody verdict cannot
        // exist without one, so that arm is belt-and-braces).
        if let Some(grant_id) = &admitted.custody_grant_id
            && self
                .config
                .custody_revoked
                .as_ref()
                .is_none_or(|revoked| revoked(grant_id))
        {
            return Err(not_admitted("the custody grant is revoked"));
        }
        // The account predicate, explicit — never smuggled through a scope
        // check (`admits_scope` also enforces scope membership, which is not
        // this preflight's question). Generalized (W8.5): the verdict must
        // name a SERVED account — a custody dropped from the registry severs
        // here, at the next request, even on a live connection.
        let Some(entry) = self.served_entry(&verdict.account) else {
            return Err(not_admitted("the verdict names an unserved account"));
        };
        Ok((verdict, entry.store))
    }

    /// [`Self::admitted_connection`] plus the scope check — the preflight of
    /// the kinds whose request names the one scope they serve.
    fn admitted_for(
        &self,
        peer: &EndpointKey,
        slot: &Mutex<Option<AdmittedConnection>>,
        scope: &str,
    ) -> Result<ServeStoreHandle, RpcError> {
        let (verdict, store) = self.admitted_connection(peer, slot)?;
        if !verdict.admits_scope(&verdict.account, scope) {
            return Err(not_admitted("the verdict does not admit this scope"));
        }
        Ok(store)
    }

    async fn handle_admit(
        &self,
        peer: &EndpointKey,
        slot: &Mutex<Option<AdmittedConnection>>,
        payload: Value,
    ) -> Result<Value, RpcError> {
        let now = (self.config.now)();
        // Rule 8: the admission exchange itself is metered — a refused
        // witness spends budget exactly like an admitted one.
        if !self.ledger.try_request(peer.as_bytes(), now) {
            return Err(over_quota());
        }
        let req: PeerSyncAdmitRequest = from_value(&payload)?;
        // Multi-account routing (W8.5 P1): read which account the witness
        // CLAIMS, gate the claim against the served set, then verify against
        // exactly the claim — the peek is never believed on its own; the
        // verifier's signature/key-binding/expiry rules bind it.
        let claimed = claimed_account(&req.witness_kind, &req.witness).map_err(|e| {
            RpcError::new(ERR_WITNESS_REFUSED, "error.peer_sync.witness_refused")
                .with_details_text(e.to_string())
        })?;
        if req.witness_kind == fauna_protocol::peer_sync::WITNESS_CUSTODY_GRANT
            && claimed != self.account
        {
            // A custodian serves only the custodied account's FLEET (T13):
            // a custody grant admits its owner's planes on the owner's own
            // fleet listeners — never a custody-of-custody chain through a
            // custodied store.
            return Err(
                RpcError::new(ERR_WITNESS_REFUSED, "error.peer_sync.witness_refused")
                    .with_details_text(
                        "a custody grant admits only against the granting account's own fleet",
                    ),
            );
        }
        if !self.is_served(&claimed) {
            return Err(
                RpcError::new(ERR_WITNESS_REFUSED, "error.peer_sync.witness_refused")
                    .with_details_text("the witness names an account this node does not serve"),
            );
        }
        // One by-name dispatch for every witness kind (a kind this build does
        // not implement is refused by name, never guessed at from shape — the
        // seam's verifier rule; `evaluate_witness` owns the match).
        let evaluated = evaluate_witness(
            &req.witness_kind,
            &req.witness,
            peer.as_bytes(),
            &claimed,
            now,
        )
        .map_err(|e| {
            RpcError::new(ERR_WITNESS_REFUSED, "error.peer_sync.witness_refused")
                .with_details_text(e.to_string())
        })?;
        // The custody evaluator's second store: the witness is self-contained
        // and verified above; revocation is THIS side's own question, answered
        // from its revocation view — and a serve side with no view at all
        // cannot evaluate custody admission (fail-closed; see the config
        // field's doc).
        if let Some(grant_id) = &evaluated.custody_grant_id {
            match &self.config.custody_revoked {
                None => {
                    return Err(RpcError::new(
                        ERR_WITNESS_REFUSED,
                        "error.peer_sync.witness_refused",
                    )
                    .with_details_text(
                        "this serve side has no custody-revocation view and cannot \
                         evaluate custody admission",
                    ));
                }
                Some(revoked) if revoked(grant_id) => {
                    return Err(RpcError::new(
                        ERR_WITNESS_REFUSED,
                        "error.peer_sync.witness_refused",
                    )
                    .with_details_text("the custody grant is revoked"));
                }
                Some(_) => {}
            }
        }
        // The device evaluator's second store, the twin of the custody
        // evaluator's above: the `DeviceAuthorization` witness is
        // self-contained, root-signed and (in production) never expires —
        // "removal is the control" — so whether the account has since removed
        // the device is THIS side's own question, answered from its own
        // merged `fauna.state.device-set` state. Refused BEFORE the verdict
        // is stored, so a removed device never holds one.
        if let (Some(device_key), Some(removed)) =
            (&evaluated.device_key, &self.config.device_removed)
            && removed(&claimed, device_key)
        {
            return Err(
                RpcError::new(ERR_WITNESS_REFUSED, "error.peer_sync.witness_refused")
                    .with_details_text("the device is removed from this account's fleet"),
            );
        }
        // The mutual half answers with the CLAIMED account's reply witness:
        // the own cert on the own account, THE CUSTODY GRANT on a custodied
        // one (an owner-fleet dialer verifies its custodian by exactly that
        // grant — the sides' kinds may differ).
        let reply_entry = self
            .served_entry(&claimed)
            .ok_or_else(|| not_admitted("the served account left the registry mid-admission"))?;
        *slot.lock().unwrap() = Some(AdmittedConnection {
            verdict: evaluated.verdict,
            device_key: evaluated.device_key,
            custody_grant_id: evaluated.custody_grant_id,
        });
        // T13 step 4, responder half. Record what the dialer carried — bound
        // to the key THIS CHANNEL proved, never the value's self-named
        // `node_id` — and answer with our own current candidates. Both are
        // memory-only here: the pump drains the observation into the sealed
        // registry row, so the ack path stays free of I/O.
        if let Some(carried) =
            crate::discovery::bind_carried_endpoints(req.endpoints, peer.as_bytes())
        {
            self.observed_endpoints
                .lock()
                .unwrap()
                .insert(*peer.as_bytes(), carried);
        }
        to_value(&PeerSyncAdmitReply {
            witness_kind: reply_entry.reply_witness_kind,
            witness: reply_entry.reply_witness,
            endpoints: self.own_endpoints.read().unwrap().clone(),
            extra: Default::default(),
        })
    }

    async fn handle_changes_list(
        &self,
        peer: &EndpointKey,
        slot: &Mutex<Option<AdmittedConnection>>,
        payload: Value,
    ) -> Result<Value, RpcError> {
        let req: SyncChangesListRequest = from_value(&payload)?;
        let (item_class, scope) =
            crate::store_serve::changes_list_scope(&req).map_err(serve_refusal)?;
        let store = self.admitted_for(peer, slot, &scope)?;
        let reply = crate::store_serve::serve_changes_list(&store, item_class, scope, &req)
            .await
            .map_err(serve_refusal)?;
        to_value(&reply)
    }

    async fn handle_blocks_pull(
        &self,
        peer: &EndpointKey,
        slot: &Mutex<Option<AdmittedConnection>>,
        payload: Value,
    ) -> Result<Value, RpcError> {
        let req: PeerSyncBlocksPullRequest = from_value(&payload)?;
        // The want-list is scope-less on the wire (blocks are
        // content-addressed), so the scope check runs per block against its
        // index row below; the preflight asks only metering + verdict-exists
        // + validity + the served-account match. A scope up here would
        // over-gate: a `Named` content-scope verdict (the M2 / custody
        // shape) admits no account-state scope, yet the per-block check is
        // exactly what its bound is for.
        let (verdict, store) = self.admitted_connection(peer, slot)?;

        let mut cids = Vec::with_capacity(req.cids.len());
        for cid in &req.cids {
            let arr: [u8; 36] = cid
                .as_slice()
                .try_into()
                .map_err(|_| unsupported("a want-list CID is not the canonical 36-byte form"))?;
            cids.push(
                ContentHash::from_bytes(arr)
                    .map_err(|_| unsupported("a want-list CID does not decode as a CID"))?,
            );
        }

        let looked_up = store.blocks(cids).await.map_err(internal)?;
        let mut reply = PeerSyncBlocksPullReply::default();
        let mut budget = 0usize;
        for lookup in looked_up {
            let wire_cid = serde_bytes::ByteBuf::from(lookup.cid.as_bytes().to_vec());
            // Scope enforcement per block (the core's admission duty): a
            // block with no index row, or in a scope the verdict does not
            // admit, is "missing" — indistinguishable from not-held, which is
            // exactly what a scope-bounded verdict (the share twin's M2 /
            // custody witnesses) must see.
            let admitted = lookup
                .scope
                .as_deref()
                .is_some_and(|s| verdict.admits_scope(&verdict.account, s));
            let Some(bytes) = lookup.bytes.filter(|_| admitted) else {
                reply.missing.push(wire_cid);
                continue;
            };
            if !reply.blocks.is_empty() && budget + bytes.len() > MAX_BLOCK_BYTES_PER_PULL {
                reply.deferred.push(wire_cid);
                continue;
            }
            budget += bytes.len();
            reply.blocks.push(PeerSyncBlock {
                cid: wire_cid,
                bytes: serde_bytes::ByteBuf::from(bytes),
                extra: Default::default(),
            });
        }
        to_value(&reply)
    }

    /// `fauna.peer.sync.chunks.pull` — a sibling's file bodies, sliced to the
    /// frame. Admitted for an **own-account device** only: a sibling of this
    /// account holds the same folders, while a custodian (a custody-grant
    /// verdict) and an owner device dialing this machine as ITS custodian (a
    /// verdict for a custodied account) hold account planes, not file bodies.
    ///
    /// Every want is answered through [`PeerSyncServerConfig::file_chunks`]
    /// and the body re-derived per want, as the share leg's chunk door does —
    /// the serve core keeps no chunk cache by rule. A want past the reply's
    /// budget is `deferred` unread, so a want list costs at most one reply's
    /// worth of serving per request.
    async fn handle_chunks_pull(
        &self,
        peer: &EndpointKey,
        slot: &Mutex<Option<AdmittedConnection>>,
        payload: Value,
    ) -> Result<Value, RpcError> {
        let req: PeerSyncChunksPullRequest = from_value(&payload)?;
        self.admitted_own_device(peer, slot)?;
        let mut wants = Vec::with_capacity(req.wants.len());
        for want in &req.wants {
            let key = <[u8; 32]>::try_from(want.store_key.as_ref())
                .map_err(|_| unsupported("a store key must be exactly 32 bytes"))?;
            wants.push((key, want));
        }

        let mut reply = PeerSyncChunksPullReply::default();
        let mut budget = crate::ranged::MAX_BODY_BYTES_PER_REPLY;
        for (key, want) in wants {
            if budget == 0 {
                reply.deferred.push(want.store_key.clone());
                continue;
            }
            let body = match &self.config.file_chunks {
                Some(source) => source(req.folder.clone(), key).await,
                None => None,
            };
            let Some(body) = body else {
                reply.missing.push(want.store_key.clone());
                continue;
            };
            // budget > 0, so every slice of an incomplete body is non-empty and
            // a pull always advances (`ranged::slice_for`).
            let Some(slice) = crate::ranged::slice_for(&body, want.offset, budget) else {
                return Err(unsupported(
                    "the want's offset is past the end of the stored body",
                ));
            };
            budget -= slice.len();
            reply.chunks.push(PeerSyncChunk {
                store_key: want.store_key.clone(),
                offset: want.offset,
                bytes: serde_bytes::ByteBuf::from(slice.to_vec()),
                total_len: body.len() as u64,
                extra: Default::default(),
            });
        }
        to_value(&reply)
    }

    /// The preflight of the three **folder doors** (`chunks.pull`,
    /// `changes.list`, `manifests.get`): [`Self::admitted_connection`] — the
    /// per-request severance checks included — plus the rule that a folder is
    /// served to a device of THIS account only. A sibling of this account holds
    /// the same folders; a custodian (a custody-grant verdict) and an owner
    /// device dialing this machine as ITS custodian (a verdict for a custodied
    /// account) hold account planes, not folders.
    fn admitted_own_device(
        &self,
        peer: &EndpointKey,
        slot: &Mutex<Option<AdmittedConnection>>,
    ) -> Result<(), RpcError> {
        let (verdict, _store) = self.admitted_connection(peer, slot)?;
        let own_device = slot
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|a| a.device_key.is_some() && a.custody_grant_id.is_none());
        if verdict.account != self.account || !own_device {
            return Err(not_admitted(
                "a folder is served to a device of this account only",
            ));
        }
        Ok(())
    }

    /// `fauna.peer.sync.changes.list` — one page of a folder's change rows for
    /// a sibling (`p2p.md` § Same-account nest-down folder transfer). The serve
    /// set is the row half's: the provenance ruling over the folder's own and
    /// relayed rows, pending rows on the tail page, `more` a fact (one row past
    /// the bound is asked for). Rows walk on any path — metadata-sized; only
    /// chunk bodies keep the relay byte gate.
    async fn handle_folder_changes_list(
        &self,
        peer: &EndpointKey,
        slot: &Mutex<Option<AdmittedConnection>>,
        payload: Value,
    ) -> Result<Value, RpcError> {
        let req: PeerSyncChangesListRequest = from_value(&payload)?;
        self.admitted_own_device(peer, slot)?;
        let page = crate::ranged::MAX_ROWS_PER_PAGE;
        let rows = match &self.config.folder_changes {
            Some(source) => source(req.folder.clone(), req.since, page + 1)
                .await
                .map_err(internal)?,
            None => Vec::new(),
        };
        let more = rows.len() as u32 > page;
        let own_actor_hex = hex::encode(self.account);
        let changes = rows
            .into_iter()
            .take(page as usize)
            .filter(|row| serves_held_row(row, &own_actor_hex))
            .map(|row| PeerChangeRow {
                change: row.change,
                sequenced: row.sequenced,
                signer_cert: row.signer_cert,
                extra: Default::default(),
            })
            .collect();
        to_value(&PeerSyncChangesListReply {
            changes,
            more,
            extra: Default::default(),
        })
    }

    /// `fauna.peer.sync.manifests.get` — a folder's retained manifests for a
    /// sibling, by content hash. With no nest answering, the sibling that wrote
    /// a row is the only source of the manifest it names. Served / missing /
    /// deferred; a manifest over the reply budget is `deferred`, never
    /// `missing`. A request names at most one page of hashes
    /// ([`crate::ranged::MAX_ROWS_PER_PAGE`]) and the source is not read once
    /// the reply is full — the rest is `deferred` unread.
    async fn handle_folder_manifests_get(
        &self,
        peer: &EndpointKey,
        slot: &Mutex<Option<AdmittedConnection>>,
        payload: Value,
    ) -> Result<Value, RpcError> {
        let req: PeerSyncManifestsGetRequest = from_value(&payload)?;
        self.admitted_own_device(peer, slot)?;
        // One page of hashes per request: each is a source read (a DB open in
        // the sync agent) and a frame holds ~29k of them. The sibling client
        // asks for one at a time.
        if req.manifest_hashes.len() > crate::ranged::MAX_ROWS_PER_PAGE as usize {
            return Err(unsupported(
                "a manifests request names at most one page of hashes",
            ));
        }
        let mut hashes = Vec::with_capacity(req.manifest_hashes.len());
        for wanted in &req.manifest_hashes {
            let hash = <[u8; 32]>::try_from(wanted.as_ref())
                .map_err(|_| unsupported("a manifest hash must be exactly 32 bytes"))?;
            hashes.push((hash, wanted));
        }
        let mut reply = PeerSyncManifestsGetReply::default();
        let mut budget = crate::ranged::MAX_BODY_BYTES_PER_REPLY;
        // The reply is full once the budget is gone or a manifest has been
        // turned away for exceeding what was left: the rest is `deferred`
        // unread, as `chunks.pull` does, so the reads stay bounded by the
        // reply, not by the request.
        let mut full = false;
        for (hash, wanted) in hashes {
            if full || budget == 0 {
                reply.deferred.push(wanted.clone());
                continue;
            }
            let bytes = match &self.config.folder_manifests {
                Some(source) => source(req.folder.clone(), hash).await,
                None => None,
            };
            match bytes {
                None => reply.missing.push(wanted.clone()),
                Some(bytes) if bytes.len() > budget => {
                    full = true;
                    reply.deferred.push(wanted.clone());
                }
                Some(bytes) => {
                    budget -= bytes.len();
                    reply.manifests.push(PeerSyncManifest {
                        hash: wanted.clone(),
                        bytes: serde_bytes::ByteBuf::from(bytes),
                        extra: Default::default(),
                    });
                }
            }
        }
        to_value(&reply)
    }
}

/// Bring the peer-sync node up — the **one** door to the leg's listener.
///
/// - **Rule 7 (version brake):** refuses unless the nest advertises the
///   `peer-sync` capability — a nest release that stops advertising it stops
///   the fleet's peer leg at the next start.
/// - **Rule 5 (no listener when off):** the listener exists iff the returned
///   [`PeerNode`] is alive; dropping it aborts the accept loop and every
///   inbound channel. There is no other bind site in this crate.
pub async fn start_peer_sync_node(
    transport: Arc<dyn PeerTransport>,
    server: Arc<PeerSyncServer>,
    nest_capabilities: &[String],
) -> Result<PeerNode> {
    anyhow::ensure!(
        peer_sync_enabled(nest_capabilities),
        "the nest does not advertise the `peer-sync` capability — the fleet brake is on \
         (p2p.md § Wormability posture rule 7), so the peer leg must not come up"
    );
    Ok(PeerNode::start_with(transport, server.handler_factory()).await)
}

// ── Wire helpers + error shapes ──────────────────────────────────────────────

/// The plane-scoped wrapper over the shared round-trip: only the encode-failure
/// error is this plane's (`fauna.peer.sync.internal`).
fn to_value<T: serde::Serialize>(value: &T) -> Result<Value, RpcError> {
    fauna_peer_channel::to_value(value)
        .ok_or_else(|| internal(anyhow::anyhow!("reply encoding failed")))
}

fn not_admitted(detail: &str) -> RpcError {
    RpcError::new(ERR_NOT_ADMITTED, "error.peer_sync.not_admitted").with_details_text(detail)
}

fn over_quota() -> RpcError {
    RpcError::new(ERR_OVER_QUOTA, "error.peer_sync.over_quota")
}

fn unsupported(detail: &str) -> RpcError {
    RpcError::new(ERR_UNSUPPORTED, "error.peer_sync.unsupported").with_details_text(detail)
}

/// The shared feed serve's refusal, in this plane's vocabulary.
fn serve_refusal(r: crate::store_serve::ServeRefusal) -> RpcError {
    match r {
        crate::store_serve::ServeRefusal::Unsupported(detail) => unsupported(detail),
        crate::store_serve::ServeRefusal::Internal(e) => internal(e),
    }
}

fn internal(e: anyhow::Error) -> RpcError {
    RpcError::new("fauna.peer.sync.internal", "error.peer_sync.internal")
        .with_details_text(e.to_string())
}

// The serve door against verdict shapes no wire producer mints yet (the M2 /
// custody `Named` verdicts) — in-crate because the slot is hand-set; every
// wire-reachable behavior is pinned in `fauna-sync-engine`'s
// `peer_leg_convergence.rs` instead.
impl MeteredPlane for PeerSyncServer {
    const PLANE: &'static str = "peer-sync";

    fn now_secs(&self) -> u64 {
        (self.config.now)()
    }

    fn ledger(&self) -> &QuotaLedger {
        &self.ledger
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::{AdmissionVerdict, AdmittedScopes};
    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_account_store::store::AccountStore;
    use fauna_account_store::types::WriterId;

    const ACCOUNT: [u8; 32] = [0xAA; 32];

    /// A server over a store holding one record + block in each of two
    /// content scopes.
    async fn server_with_two_scoped_blocks() -> (
        tempfile::TempDir,
        Arc<PeerSyncServer>,
        (ContentHash, String),
        (ContentHash, String),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store = AccountStore::open(
            SqliteBackend::open(dir.path()).unwrap(),
            &hex::encode(ACCOUNT),
            WriterId([0x0D; 32]),
        )
        .await
        .unwrap();
        let in_scope = format!("content:post:{}", "aa".repeat(32));
        let out_scope = format!("content:post:{}", "bb".repeat(32));
        let (cid_in, _) = store
            .stage_local_record(&in_scope, "post", b"a block inside the named scope")
            .await
            .unwrap();
        let (cid_out, _) = store
            .stage_local_record(&out_scope, "post", b"a block outside the named scope")
            .await
            .unwrap();
        let server = Arc::new(PeerSyncServer::new(
            ServeStoreHandle::spawn(store),
            ACCOUNT,
            PeerSyncServerConfig {
                display_name: "serve-door".into(),
                // Never presented on this path — handle_blocks_pull is below
                // the admit exchange.
                own_witness: EmbedAsBytes {
                    envelope: vec![],
                    bytes: vec![],
                    signer_auth: None,
                },
                own_witness_kind: WITNESS_DEVICE_AUTHORIZATION.to_string(),
                custody_revoked: None,
                device_removed: None,
                file_chunks: None,
                folder_changes: None,
                folder_manifests: None,
                quotas: QuotaConfig::default(),
                now: Arc::new(|| 5_000),
            },
        ));
        (dir, server, (cid_in, in_scope), (cid_out, out_scope))
    }

    /// The sync plane serves through the shared rule-8 door
    /// ([`metered_handler_factory`]) rather than a local copy of it: with a
    /// one-connection window, the same peer's second dial is refused outright.
    /// Pins the wiring — a re-rolled local factory would pass every other test
    /// in this file.
    #[tokio::test]
    async fn the_sync_plane_meters_connections_through_the_shared_door() {
        let dir = tempfile::tempdir().unwrap();
        let store = AccountStore::open(
            SqliteBackend::open(dir.path()).unwrap(),
            &hex::encode(ACCOUNT),
            WriterId([0x0D; 32]),
        )
        .await
        .unwrap();
        let server = Arc::new(PeerSyncServer::new(
            ServeStoreHandle::spawn(store),
            ACCOUNT,
            PeerSyncServerConfig {
                display_name: "meter-door".into(),
                own_witness: EmbedAsBytes {
                    envelope: vec![],
                    bytes: vec![],
                    signer_auth: None,
                },
                own_witness_kind: WITNESS_DEVICE_AUTHORIZATION.to_string(),
                custody_revoked: None,
                device_removed: None,
                file_chunks: None,
                folder_changes: None,
                folder_manifests: None,
                quotas: QuotaConfig {
                    conns_per_window: 1,
                    ..QuotaConfig::default()
                },
                now: Arc::new(|| 5_000),
            },
        ));

        let factory = server.handler_factory();
        let peer = EndpointKey::from_bytes([0xEE; 32]);
        assert!(factory(peer, fauna_transport::PathKind::Lan).is_some());
        assert!(
            factory(peer, fauna_transport::PathKind::Lan).is_none(),
            "the second connection in-window is refused by the shared meter"
        );
    }

    fn pull_payload(cids: &[&ContentHash]) -> Value {
        to_value(&PeerSyncBlocksPullRequest {
            cids: cids
                .iter()
                .map(|c| serde_bytes::ByteBuf::from(c.as_bytes().to_vec()))
                .collect(),
            extra: Default::default(),
        })
        .unwrap()
    }

    /// The preflight must not over-gate on the account-state scope: a
    /// scope-bounded `Named` verdict (the M2-membership / custody-grant
    /// shape) reaches the per-block check, which is the enforcement the
    /// verdict's bound is FOR — in-scope blocks serve, out-of-scope blocks
    /// are "missing", indistinguishable from not-held.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_named_scope_verdict_pulls_blocks_in_its_scope_and_only_there() {
        let (_dir, server, (cid_in, in_scope), (cid_out, _)) =
            server_with_two_scoped_blocks().await;
        let slot = Mutex::new(Some(AdmittedConnection {
            verdict: AdmissionVerdict {
                account: ACCOUNT,
                scopes: AdmittedScopes::Named(vec![in_scope]),
                expires_at: None,
            },
            device_key: None,
            custody_grant_id: None,
        }));
        let peer = EndpointKey::from_bytes([0x77; 32]);

        let reply = server
            .handle_blocks_pull(&peer, &slot, pull_payload(&[&cid_in, &cid_out]))
            .await
            .expect(
                "a scope-bounded verdict reaches the per-block check, never a wholesale refusal",
            );
        let reply: PeerSyncBlocksPullReply = from_value(&reply).unwrap();
        assert_eq!(
            reply.blocks.len(),
            1,
            "exactly the in-scope block serves, got {reply:?}"
        );
        assert_eq!(reply.blocks[0].cid.as_slice(), cid_in.as_bytes());
        assert_eq!(reply.missing.len(), 1);
        assert_eq!(reply.missing[0].as_slice(), cid_out.as_bytes());
    }

    // ── the admit door's custody arm ─────────────────────────────────────

    use fauna_core::custody_grant::{
        CUSTODY_GRANT_ID_LEN, CustodyGrant, CustodyScopeSet, sign_custody_grant,
    };
    use fauna_core::data::Timestamp;
    use fauna_core::identity::ActorKeypair;
    use fauna_protocol::peer_sync::{WITNESS_CUSTODY_GRANT, WITNESS_DEVICE_AUTHORIZATION};

    /// An empty-store server for the account of `ActorKeypair::from_secret([9;32])`,
    /// with the given custody-revocation view and no removed-device view.
    async fn admit_server(
        custody_revoked: Option<CustodyRevocationFn>,
    ) -> (tempfile::TempDir, Arc<PeerSyncServer>, ActorKeypair) {
        admit_server_with(custody_revoked, None).await
    }

    /// [`admit_server`] with both evaluator views wired.
    async fn admit_server_with(
        custody_revoked: Option<CustodyRevocationFn>,
        device_removed: Option<DeviceRemovedFn>,
    ) -> (tempfile::TempDir, Arc<PeerSyncServer>, ActorKeypair) {
        admit_server_serving(custody_revoked, device_removed, None).await
    }

    /// [`admit_server_with`] plus a file-body source.
    async fn admit_server_serving(
        custody_revoked: Option<CustodyRevocationFn>,
        device_removed: Option<DeviceRemovedFn>,
        file_chunks: Option<FileChunkFn>,
    ) -> (tempfile::TempDir, Arc<PeerSyncServer>, ActorKeypair) {
        admit_server_sourced(
            custody_revoked,
            device_removed,
            FolderSources {
                chunks: file_chunks,
                ..Default::default()
            },
        )
        .await
    }

    /// The three folder doors' host sources.
    #[derive(Default)]
    struct FolderSources {
        chunks: Option<FileChunkFn>,
        changes: Option<FolderChangesFn>,
        manifests: Option<FolderManifestFn>,
    }

    /// [`admit_server_with`] plus the folder doors' sources.
    async fn admit_server_sourced(
        custody_revoked: Option<CustodyRevocationFn>,
        device_removed: Option<DeviceRemovedFn>,
        sources: FolderSources,
    ) -> (tempfile::TempDir, Arc<PeerSyncServer>, ActorKeypair) {
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let account = owner.actor_id().0;
        let dir = tempfile::tempdir().unwrap();
        let store = AccountStore::open(
            SqliteBackend::open(dir.path()).unwrap(),
            &hex::encode(account),
            WriterId([0x0D; 32]),
        )
        .await
        .unwrap();
        let server = Arc::new(PeerSyncServer::new(
            ServeStoreHandle::spawn(store),
            account,
            PeerSyncServerConfig {
                display_name: "admit-door".into(),
                own_witness: EmbedAsBytes {
                    envelope: vec![],
                    bytes: vec![],
                    signer_auth: None,
                },
                own_witness_kind: WITNESS_DEVICE_AUTHORIZATION.to_string(),
                custody_revoked,
                device_removed,
                file_chunks: sources.chunks,
                folder_changes: sources.changes,
                folder_manifests: sources.manifests,
                quotas: QuotaConfig::default(),
                now: Arc::new(|| 5_000),
            },
        ));
        (dir, server, owner)
    }

    // ── fauna.peer.sync.changes.list / manifests.get ────────────────────────

    /// A folder-row source for folder `"7"`: the rows a host's store would
    /// answer, honouring `since` on the sequenced rows and `max_rows`, pending
    /// rows (seq 0) on the tail — `SyncDbShareStore::changes_since`'s shape.
    fn folder_rows(rows: Vec<LocalShareChange>) -> FolderChangesFn {
        Arc::new(move |folder: String, since: i64, max_rows: u32| {
            let page: Vec<LocalShareChange> = if folder == "7" {
                let mut out: Vec<LocalShareChange> = rows
                    .iter()
                    .filter(|r| r.sequenced && r.change.seq > since)
                    .cloned()
                    .collect();
                out.extend(rows.iter().filter(|r| !r.sequenced).cloned());
                out.truncate(max_rows as usize);
                out
            } else {
                Vec::new()
            };
            Box::pin(async move { Ok(page) })
        })
    }

    fn own_row(seq: i64, sequenced: bool, actor_hex: &str) -> LocalShareChange {
        LocalShareChange {
            change: fauna_protocol::sync::SyncChange {
                seq,
                path: Some(format!("f{seq}.txt")),
                author_actor_id: Some(actor_hex.to_string()),
                ..Default::default()
            },
            sequenced,
            locally_authored: true,
            signer_cert: None,
        }
    }

    fn rows_payload(folder: &str, since: i64) -> Value {
        to_value(&PeerSyncChangesListRequest {
            folder: folder.into(),
            since,
            extra: Default::default(),
        })
        .unwrap()
    }

    fn manifests_payload(folder: &str, hashes: &[[u8; 32]]) -> Value {
        to_value(&PeerSyncManifestsGetRequest {
            folder: folder.into(),
            manifest_hashes: hashes
                .iter()
                .map(|h| serde_bytes::ByteBuf::from(h.to_vec()))
                .collect(),
            extra: Default::default(),
        })
        .unwrap()
    }

    /// A manifest source retaining `bytes` under `hash` in folder `"7"`.
    fn one_manifest(entries: Vec<([u8; 32], Vec<u8>)>) -> FolderManifestFn {
        Arc::new(move |folder: String, wanted: [u8; 32]| {
            let hit = (folder == "7")
                .then(|| entries.iter().find(|(h, _)| *h == wanted))
                .flatten()
                .map(|(_, b)| b.clone());
            Box::pin(async move { hit })
        })
    }

    async fn admitted_sibling(
        server: &PeerSyncServer,
        owner: &ActorKeypair,
    ) -> (EndpointKey, Mutex<Option<AdmittedConnection>>) {
        let device = [0x0Au8; 32];
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(device);
        server
            .handle_admit(&peer, &slot, device_admit_payload(owner, device))
            .await
            .expect("a sibling admits");
        (peer, slot)
    }

    /// A sibling pages a folder's rows: the page stops at the one bound both
    /// legs read with `more` as a fact, the next page resumes from the cursor,
    /// the pending rows ride the tail page unsequenced, and the provenance
    /// ruling filters — an unsigned row of another writer is never served.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_sibling_pulls_a_folders_row_page_with_pending_rows_on_the_tail() {
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let me = hex::encode(owner.actor_id().0);
        let page = crate::ranged::MAX_ROWS_PER_PAGE as i64;
        let mut rows: Vec<LocalShareChange> =
            (1..=page + 3).map(|s| own_row(s, true, &me)).collect();
        rows.push(own_row(0, false, &me));
        // An unsigned row relayed from another writer: nothing vouches for it.
        let mut relayed = own_row(page + 4, true, &"ab".repeat(32));
        relayed.locally_authored = false;
        rows.push(relayed);
        let (_dir, server, owner) = admit_server_sourced(
            None,
            None,
            FolderSources {
                changes: Some(folder_rows(rows)),
                ..Default::default()
            },
        )
        .await;
        let (peer, slot) = admitted_sibling(&server, &owner).await;

        let reply = server
            .handle_folder_changes_list(&peer, &slot, rows_payload("7", 0))
            .await
            .expect("an admitted sibling pages the folder");
        let reply: PeerSyncChangesListReply = from_value(&reply).unwrap();
        assert_eq!(reply.changes.len(), page as usize);
        assert!(reply.more, "a full page with rows behind it says so");
        assert!(reply.changes.iter().all(|c| c.sequenced));

        let reply = server
            .handle_folder_changes_list(&peer, &slot, rows_payload("7", page))
            .await
            .unwrap();
        let reply: PeerSyncChangesListReply = from_value(&reply).unwrap();
        assert!(!reply.more, "the tail page is the last");
        let seqs: Vec<i64> = reply.changes.iter().map(|c| c.change.seq).collect();
        assert_eq!(
            seqs,
            vec![page + 1, page + 2, page + 3, 0],
            "own rows to the tail, the pending row last, the unsigned relay withheld"
        );
        assert!(
            !reply.changes[3].sequenced,
            "a pending row serves unsequenced"
        );

        // A folder this host runs no engine for is an empty page, not an error.
        let reply = server
            .handle_folder_changes_list(&peer, &slot, rows_payload("8", 0))
            .await
            .unwrap();
        let reply: PeerSyncChangesListReply = from_value(&reply).unwrap();
        assert!(reply.changes.is_empty() && !reply.more);
    }

    /// The manifest door: a retained manifest is served, an unheld one is
    /// `missing`, and one past the reply budget is `deferred`, never `missing`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_sibling_is_served_retained_manifests_missing_and_deferred() {
        let small = ([0x11u8; 32], vec![1u8; 64]);
        let huge = (
            [0x22u8; 32],
            vec![2u8; crate::ranged::MAX_BODY_BYTES_PER_REPLY],
        );
        let absent = [0x33u8; 32];
        let (_dir, server, owner) = admit_server_sourced(
            None,
            None,
            FolderSources {
                manifests: Some(one_manifest(vec![small.clone(), huge.clone()])),
                ..Default::default()
            },
        )
        .await;
        let (peer, slot) = admitted_sibling(&server, &owner).await;
        let reply = server
            .handle_folder_manifests_get(
                &peer,
                &slot,
                manifests_payload("7", &[small.0, absent, huge.0]),
            )
            .await
            .expect("an admitted sibling fetches manifests");
        let reply: PeerSyncManifestsGetReply = from_value(&reply).unwrap();
        assert_eq!(reply.manifests.len(), 1);
        assert_eq!(reply.manifests[0].bytes.as_ref(), small.1.as_slice());
        assert_eq!(
            reply.missing,
            vec![serde_bytes::ByteBuf::from(absent.to_vec())]
        );
        assert_eq!(
            reply.deferred,
            vec![serde_bytes::ByteBuf::from(huge.0.to_vec())]
        );

        // No manifest source at all answers missing, never an error.
        let (_d2, bare, owner2) = admit_server_sourced(None, None, FolderSources::default()).await;
        let (peer2, slot2) = admitted_sibling(&bare, &owner2).await;
        let reply = bare
            .handle_folder_manifests_get(&peer2, &slot2, manifests_payload("7", &[small.0]))
            .await
            .unwrap();
        let reply: PeerSyncManifestsGetReply = from_value(&reply).unwrap();
        assert_eq!(reply.missing.len(), 1);
    }

    /// A counting manifest source answering every hash with `len` bytes.
    fn counting_manifests(
        len: usize,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    ) -> FolderManifestFn {
        Arc::new(move |_folder: String, _wanted: [u8; 32]| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move { Some(vec![7u8; len]) })
        })
    }

    /// A counting manifest source answering the FIRST hash with a small
    /// manifest and every later one with a full-budget manifest, so the reply
    /// is stopped by a manifest turned away for size (`full`), never by the
    /// budget reaching zero.
    fn small_then_oversized_manifests(
        calls: Arc<std::sync::atomic::AtomicUsize>,
    ) -> FolderManifestFn {
        Arc::new(move |_folder: String, _wanted: [u8; 32]| {
            let nth = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let len = if nth == 0 {
                8
            } else {
                crate::ranged::MAX_BODY_BYTES_PER_REPLY
            };
            Box::pin(async move { Some(vec![7u8; len]) })
        })
    }

    fn distinct_hashes(n: usize) -> Vec<[u8; 32]> {
        (0..n)
            .map(|i| {
                let mut h = [0u8; 32];
                h[..8].copy_from_slice(&(i as u64).to_be_bytes());
                h
            })
            .collect()
    }

    /// The reply budget bounds the READS, not just the answer: once one
    /// manifest has spent it, the rest of the request is `deferred` unread
    /// (`chunks.pull`'s rule), so a request costs at most about one reply's
    /// worth of source calls (each a DB open in the sync agent).
    #[tokio::test(flavor = "multi_thread")]
    async fn the_manifest_door_stops_reading_the_source_once_the_reply_budget_is_spent() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (_dir, server, owner) = admit_server_sourced(
            None,
            None,
            FolderSources {
                manifests: Some(counting_manifests(
                    crate::ranged::MAX_BODY_BYTES_PER_REPLY,
                    Arc::clone(&calls),
                )),
                ..Default::default()
            },
        )
        .await;
        let (peer, slot) = admitted_sibling(&server, &owner).await;
        let n = crate::ranged::MAX_ROWS_PER_PAGE as usize;
        let reply = server
            .handle_folder_manifests_get(&peer, &slot, manifests_payload("7", &distinct_hashes(n)))
            .await
            .expect("a request at the cap is answered");
        let reply: PeerSyncManifestsGetReply = from_value(&reply).unwrap();
        assert!(
            calls.load(std::sync::atomic::Ordering::SeqCst) <= 2,
            "the source is read at most until the budget is gone"
        );
        assert_eq!(reply.manifests.len(), 1);
        assert_eq!(reply.deferred.len(), n - 1);
        assert!(reply.missing.is_empty());
    }

    /// The `full` arm of the stop is witnessed on its own: after one small
    /// manifest the budget is NOT zero, so only a manifest turned away for
    /// exceeding what is left can stop the reads. Without that arm one small
    /// manifest followed by 63 large ones costs 64 reads instead of 2.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_manifest_door_stops_reading_at_the_first_oversized_manifest_with_budget_left() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (_dir, server, owner) = admit_server_sourced(
            None,
            None,
            FolderSources {
                manifests: Some(small_then_oversized_manifests(Arc::clone(&calls))),
                ..Default::default()
            },
        )
        .await;
        let (peer, slot) = admitted_sibling(&server, &owner).await;
        let n = crate::ranged::MAX_ROWS_PER_PAGE as usize;
        let reply = server
            .handle_folder_manifests_get(&peer, &slot, manifests_payload("7", &distinct_hashes(n)))
            .await
            .expect("a request at the cap is answered");
        let reply: PeerSyncManifestsGetReply = from_value(&reply).unwrap();
        assert!(
            calls.load(std::sync::atomic::Ordering::SeqCst) <= 2,
            "the source is read at most until the first oversized manifest"
        );
        assert_eq!(reply.manifests.len(), 1);
        assert_eq!(reply.deferred.len(), n - 1);
        assert!(reply.missing.is_empty());
    }

    /// A request names at most one page of hashes — a frame holds ~29k, each a
    /// source call — and an over-cap request is refused before any read.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_manifest_door_refuses_a_request_over_the_page_bound_without_reading() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (_dir, server, owner) = admit_server_sourced(
            None,
            None,
            FolderSources {
                manifests: Some(counting_manifests(8, Arc::clone(&calls))),
                ..Default::default()
            },
        )
        .await;
        let (peer, slot) = admitted_sibling(&server, &owner).await;
        let n = crate::ranged::MAX_ROWS_PER_PAGE as usize + 1;
        let err = server
            .handle_folder_manifests_get(&peer, &slot, manifests_payload("7", &distinct_hashes(n)))
            .await
            .expect_err("an over-cap request is refused");
        assert_eq!(err.code, ERR_UNSUPPORTED, "{err:?}");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// Folder rows and manifests go to this account's own devices only — a
    /// custodian and a custodied account's device are refused at both doors —
    /// and a removal severs a live sibling at its next row or manifest request.
    #[tokio::test(flavor = "multi_thread")]
    async fn folder_rows_and_manifests_are_refused_to_a_custodian_a_custodied_fleet_and_a_removed_device()
     {
        let device = [0x0Au8; 32];
        let removed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&removed);
        let view: DeviceRemovedFn = Arc::new(move |_acct: &[u8; 32], key: &[u8; 32]| {
            *key == device && flag.load(std::sync::atomic::Ordering::SeqCst)
        });
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let me = hex::encode(owner.actor_id().0);
        let (_dir, server, owner) = admit_server_sourced(
            Some(Arc::new(|_: &[u8]| false)),
            Some(view),
            FolderSources {
                changes: Some(folder_rows(vec![own_row(1, true, &me)])),
                manifests: Some(one_manifest(vec![([0x11; 32], vec![1])])),
                ..Default::default()
            },
        )
        .await;

        async fn both_refused(
            server: &PeerSyncServer,
            peer: &EndpointKey,
            slot: &Mutex<Option<AdmittedConnection>>,
            why: &str,
        ) {
            let err = server
                .handle_folder_changes_list(peer, slot, rows_payload("7", 0))
                .await
                .expect_err(why);
            assert_eq!(err.code, ERR_NOT_ADMITTED, "{why}: {err:?}");
            let err = server
                .handle_folder_manifests_get(peer, slot, manifests_payload("7", &[[0x11; 32]]))
                .await
                .expect_err(why);
            assert_eq!(err.code, ERR_NOT_ADMITTED, "{why}: {err:?}");
        }

        // No admission at all.
        let slot = Mutex::new(None);
        both_refused(
            &server,
            &EndpointKey::from_bytes([0x44; 32]),
            &slot,
            "unadmitted",
        )
        .await;

        // A custodian.
        let custodian = [0xC5u8; 32];
        let (payload, _) = custody_admit_payload(&owner, custodian);
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(custodian);
        server.handle_admit(&peer, &slot, payload).await.unwrap();
        both_refused(&server, &peer, &slot, "a custodian holds no folders").await;

        // A custodied account's device.
        let other = ActorKeypair::from_secret([21u8; 32]);
        let (_cdir, handle, _cid, _scope) = custodied_store(&hex::encode(other.actor_id().0)).await;
        server.set_custodied(HashMap::from([(other.actor_id().0, served(handle))]));
        let foreign = [0x0Fu8; 32];
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(foreign);
        server
            .handle_admit(&peer, &slot, device_admit_payload(&other, foreign))
            .await
            .unwrap();
        both_refused(&server, &peer, &slot, "another account's device").await;

        // A sibling, served, then removed mid-connection.
        let (peer, slot) = admitted_sibling(&server, &owner).await;
        server
            .handle_folder_changes_list(&peer, &slot, rows_payload("7", 0))
            .await
            .expect("an enrolled sibling is served");
        removed.store(true, std::sync::atomic::Ordering::SeqCst);
        both_refused(&server, &peer, &slot, "a removed sibling").await;
    }

    // ── fauna.peer.sync.chunks.pull ─────────────────────────────────────────

    /// A file-body source holding `body` under `key` in folder `"7"`.
    fn one_body(key: [u8; 32], body: Vec<u8>) -> FileChunkFn {
        Arc::new(move |folder: String, wanted: [u8; 32]| {
            let hit = (folder == "7" && wanted == key).then(|| body.clone());
            Box::pin(async move { hit })
        })
    }

    fn chunks_payload(folder: &str, wants: &[([u8; 32], u64)]) -> Value {
        to_value(&PeerSyncChunksPullRequest {
            folder: folder.into(),
            wants: wants
                .iter()
                .map(|(k, offset)| fauna_protocol::peer_sync::PeerSyncChunkWant {
                    store_key: serde_bytes::ByteBuf::from(k.to_vec()),
                    offset: *offset,
                    extra: Default::default(),
                })
                .collect(),
            extra: Default::default(),
        })
        .unwrap()
    }

    /// A sibling of this account is served its folder's chunk body, sliced to
    /// the reply budget and resumable from the served offset; a key this
    /// device holds no body for is `missing` (the nest's to serve).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_sibling_pulls_a_folders_chunk_in_slices() {
        let key = [0xA1u8; 32];
        let body: Vec<u8> = (0..(crate::ranged::MAX_BODY_BYTES_PER_REPLY + 100))
            .map(|i| i as u8)
            .collect();
        let (_dir, server, owner) =
            admit_server_serving(None, None, Some(one_body(key, body.clone()))).await;
        let device = [0x0Au8; 32];
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(device);
        server
            .handle_admit(&peer, &slot, device_admit_payload(&owner, device))
            .await
            .expect("a sibling admits");

        let gone = [0xB2u8; 32];
        let reply = server
            .handle_chunks_pull(
                &peer,
                &slot,
                chunks_payload("7", &[(gone, 0), (key, 0), (gone, 0)]),
            )
            .await
            .expect("an admitted sibling pulls");
        let reply: PeerSyncChunksPullReply = from_value(&reply).unwrap();
        assert_eq!(reply.chunks.len(), 1);
        // The budget went to the body: the want after it is deferred unread.
        assert_eq!(
            reply.deferred,
            vec![serde_bytes::ByteBuf::from(gone.to_vec())]
        );
        assert_eq!(reply.chunks[0].total_len, body.len() as u64);
        let first = reply.chunks[0].bytes.len();
        assert_eq!(first, crate::ranged::MAX_BODY_BYTES_PER_REPLY);
        assert_eq!(
            reply.missing,
            vec![serde_bytes::ByteBuf::from(gone.to_vec())]
        );

        let reply = server
            .handle_chunks_pull(&peer, &slot, chunks_payload("7", &[(key, first as u64)]))
            .await
            .unwrap();
        let reply: PeerSyncChunksPullReply = from_value(&reply).unwrap();
        assert_eq!(reply.chunks[0].bytes.as_ref(), &body[first..]);

        // Another folder's want is not this body's, however the key reads.
        let reply = server
            .handle_chunks_pull(&peer, &slot, chunks_payload("8", &[(key, 0)]))
            .await
            .unwrap();
        let reply: PeerSyncChunksPullReply = from_value(&reply).unwrap();
        assert!(reply.chunks.is_empty() && reply.missing.len() == 1);
    }

    /// File bodies go to this account's own devices only: a custodian and an
    /// owner device of a custodied account are refused at the chunk door,
    /// whatever they may pull elsewhere.
    #[tokio::test(flavor = "multi_thread")]
    async fn file_bodies_are_refused_to_a_custodian_and_to_a_custodied_accounts_fleet() {
        let key = [0xA1u8; 32];
        let (_dir, server, owner) = admit_server_serving(
            Some(Arc::new(|_: &[u8]| false)),
            None,
            Some(one_body(key, vec![1, 2, 3])),
        )
        .await;

        let custodian = [0xC5u8; 32];
        let (payload, _) = custody_admit_payload(&owner, custodian);
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(custodian);
        server.handle_admit(&peer, &slot, payload).await.unwrap();
        let err = server
            .handle_chunks_pull(&peer, &slot, chunks_payload("7", &[(key, 0)]))
            .await
            .expect_err("a custodian holds no file bodies");
        assert_eq!(err.code, ERR_NOT_ADMITTED);

        let other = ActorKeypair::from_secret([21u8; 32]);
        let (_cdir, handle, _cid, _scope) = custodied_store(&hex::encode(other.actor_id().0)).await;
        server.set_custodied(HashMap::from([(other.actor_id().0, served(handle))]));
        let device = [0x0Fu8; 32];
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(device);
        server
            .handle_admit(&peer, &slot, device_admit_payload(&other, device))
            .await
            .unwrap();
        let err = server
            .handle_chunks_pull(&peer, &slot, chunks_payload("7", &[(key, 0)]))
            .await
            .expect_err("another account's device holds none of this account's folders");
        assert_eq!(err.code, ERR_NOT_ADMITTED);
    }

    /// No admission, no bytes; and a host with no file-body source answers
    /// every want missing rather than refusing.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_chunk_door_needs_admission_and_degrades_to_missing() {
        let key = [0xA1u8; 32];
        let (_dir, server, owner) = admit_server_serving(None, None, None).await;
        let device = [0x0Au8; 32];
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(device);
        let err = server
            .handle_chunks_pull(&peer, &slot, chunks_payload("7", &[(key, 0)]))
            .await
            .expect_err("unadmitted");
        assert_eq!(err.code, ERR_NOT_ADMITTED);

        server
            .handle_admit(&peer, &slot, device_admit_payload(&owner, device))
            .await
            .unwrap();
        let reply = server
            .handle_chunks_pull(&peer, &slot, chunks_payload("7", &[(key, 0)]))
            .await
            .unwrap();
        let reply: PeerSyncChunksPullReply = from_value(&reply).unwrap();
        assert!(reply.chunks.is_empty());
        assert_eq!(reply.missing.len(), 1);
    }

    fn custody_admit_payload(owner: &ActorKeypair, custodian: [u8; 32]) -> (Value, Vec<u8>) {
        let grant = CustodyGrant {
            grant_id: vec![0x1D; CUSTODY_GRANT_ID_LEN],
            owner: owner.actor_id(),
            custodian_key: custodian,
            scopes: CustodyScopeSet::Account,
            minted_at: Timestamp(1_000),
            expires_at: Timestamp(9_000),
            removed_devices: Vec::new(),
        };
        let witness = sign_custody_grant(owner, &grant).unwrap();
        let payload = to_value(&PeerSyncAdmitRequest {
            witness_kind: WITNESS_CUSTODY_GRANT.to_string(),
            witness,
            endpoints: None,
            extra: Default::default(),
        })
        .unwrap();
        (payload, grant.grant_id)
    }

    /// The wired-and-live custody path: a valid witness admits over the real
    /// admit door and leaves the single-principal verdict in the slot.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_custody_witness_admits_when_a_revocation_view_is_wired() {
        let (_dir, server, owner) = admit_server(Some(Arc::new(|_: &[u8]| false))).await;
        let custodian = [0xC5u8; 32];
        let (payload, _) = custody_admit_payload(&owner, custodian);
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(custodian);

        server
            .handle_admit(&peer, &slot, payload)
            .await
            .expect("a live custody witness admits");
        let verdict = slot.lock().unwrap().clone().expect("verdict stored");
        assert_eq!(
            verdict.verdict.scopes,
            AdmittedScopes::AllOfAccountSinglePrincipal
        );
        assert_eq!(verdict.verdict.expires_at, Some(9_000));
        assert_eq!(
            verdict.device_key, None,
            "a custody grant records no fleet device key — the removal view is \
             not its question"
        );
    }

    fn sample_endpoints(node_id: [u8; 32], port: u16) -> DeviceEndpoints {
        DeviceEndpoints {
            node_id,
            lan_addrs: vec![format!("192.168.1.9:{port}")],
            public_addrs: Vec::new(),
            relay_url: None,
        }
    }

    /// T13 step 4, responder half: the admit reply answers with THIS device's
    /// pump-fed candidates (the only way a non-fleet custodian can learn
    /// them — it cannot read the fleet-only `device-endpoints` kind), and what
    /// the dialer carried is left for the pump to seal into a registry row.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_admit_reply_carries_this_devices_own_endpoints() {
        let (_dir, server, owner) = admit_server(Some(Arc::new(|_: &[u8]| false))).await;
        let custodian = [0xC5u8; 32];
        let (payload, _) = custody_admit_payload(&owner, custodian);
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(custodian);

        // Nothing published yet ⇒ the slot stays absent (byte-identical to
        // the pre-W8.4 shape).
        let reply: PeerSyncAdmitReply = from_value(
            &server
                .handle_admit(&peer, &slot, payload.clone())
                .await
                .expect("admits"),
        )
        .unwrap();
        assert_eq!(reply.endpoints, None, "no facts yet ⇒ nothing carried");

        // Once the pump publishes them, every reply answers this pass's value.
        let own = sample_endpoints([0x0D; 32], 4433);
        server.set_own_endpoints(Some(own.clone()));
        let reply: PeerSyncAdmitReply = from_value(
            &server
                .handle_admit(&peer, &slot, payload)
                .await
                .expect("admits"),
        )
        .unwrap();
        assert_eq!(reply.endpoints, Some(own));
    }

    /// The dialer's half is recorded for the pump — and ONLY when the value
    /// names the key the channel proved. A peer naming a third node leaves no
    /// observation, so it can never move that node's registry row.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_dialers_carried_endpoints_are_observed_under_the_proven_key() {
        let (_dir, server, owner) = admit_server(Some(Arc::new(|_: &[u8]| false))).await;
        let custodian = [0xC5u8; 32];
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(custodian);

        // Names itself — observed under the proven key.
        let (mut payload, _) = custody_admit_payload(&owner, custodian);
        let mut req: PeerSyncAdmitRequest = from_value(&payload).unwrap();
        req.endpoints = Some(sample_endpoints(custodian, 5555));
        payload = to_value(&req).unwrap();
        server.handle_admit(&peer, &slot, payload).await.unwrap();
        let observed = server.drain_observed_endpoints();
        assert_eq!(
            observed.get(&custodian),
            Some(&sample_endpoints(custodian, 5555))
        );
        assert!(
            server.drain_observed_endpoints().is_empty(),
            "draining takes the observations — the pump seals each one once"
        );

        // Names a THIRD node over the same channel — refused, nothing recorded.
        let (mut payload, _) = custody_admit_payload(&owner, custodian);
        let mut req: PeerSyncAdmitRequest = from_value(&payload).unwrap();
        req.endpoints = Some(sample_endpoints([0xEEu8; 32], 6666));
        payload = to_value(&req).unwrap();
        server.handle_admit(&peer, &slot, payload).await.unwrap();
        assert!(
            server.drain_observed_endpoints().is_empty(),
            "a carried node_id that is not the proven peer must leave no trace"
        );
    }

    /// T13's nest/fleet two-stores rule, serve side: the evaluator answers
    /// revocation from its own view — a revoked grant id is refused at the
    /// admission evaluation even though the witness itself still verifies.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_revoked_custody_grant_is_refused_at_the_admit_door() {
        let revoked_id = vec![0x1D; CUSTODY_GRANT_ID_LEN];
        let view = move |id: &[u8]| id == revoked_id.as_slice();
        let (_dir, server, owner) = admit_server(Some(Arc::new(view))).await;
        let custodian = [0xC5u8; 32];
        let (payload, _) = custody_admit_payload(&owner, custodian);
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(custodian);

        let err = server
            .handle_admit(&peer, &slot, payload)
            .await
            .expect_err("a revoked grant must be refused");
        assert_eq!(err.code, ERR_WITNESS_REFUSED, "{err:?}");
        assert!(slot.lock().unwrap().is_none(), "no verdict on refusal");
    }

    /// Property (a) on the custody arm (`account-sync-plane.md` § The admission
    /// seam → *Validity and severance*, "three properties bind both arms"): a
    /// grant revoked while its custodian holds a live connection severs at
    /// that connection's NEXT REQUEST — not only at a next handshake the
    /// custodian need never offer. The grant's own `expires_at` is otherwise
    /// the only thing that would end it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_custody_grant_revoked_mid_connection_severs_at_the_next_request() {
        let revoked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let view = {
            let revoked = Arc::clone(&revoked);
            move |_: &[u8]| revoked.load(std::sync::atomic::Ordering::SeqCst)
        };
        let (_dir, server, owner) = admit_server(Some(Arc::new(view))).await;
        let custodian = [0xC5u8; 32];
        let (payload, _) = custody_admit_payload(&owner, custodian);
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(custodian);
        server
            .handle_admit(&peer, &slot, payload)
            .await
            .expect("a live grant admits");
        server
            .admitted_connection(&peer, &slot)
            .expect("and serves while it stays live");

        revoked.store(true, std::sync::atomic::Ordering::SeqCst);
        let Err(err) = server.admitted_connection(&peer, &slot) else {
            panic!("the revocation must reach the live connection");
        };
        assert_eq!(err.code, ERR_NOT_ADMITTED, "{err:?}");
        assert!(
            format!("{:?}", err.details).contains("revoked"),
            "refused by the revocation view, not by anything else: {err:?}"
        );
    }

    /// Fail-closed: a serve side with NO revocation view cannot evaluate
    /// custody admission at all — `custody_revoked: None` refuses the kind
    /// rather than serving without the second store (the config field's doc
    /// owns the reasoning). Device-authorization admission is untouched by
    /// this gate.
    #[tokio::test(flavor = "multi_thread")]
    async fn without_a_revocation_view_custody_witnesses_are_refused() {
        let (_dir, server, owner) = admit_server(None).await;
        let custodian = [0xC5u8; 32];
        let (payload, _) = custody_admit_payload(&owner, custodian);
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(custodian);

        let err = server
            .handle_admit(&peer, &slot, payload)
            .await
            .expect_err("no revocation view = no custody evaluation");
        assert_eq!(err.code, ERR_WITNESS_REFUSED, "{err:?}");
        assert!(
            format!("{:?}", err.details).contains("revocation"),
            "the refusal names the missing view, not the witness: {err:?}"
        );
    }

    /// The account predicate the old scope check smuggled in stays explicit:
    /// a verdict naming another account is refused at the preflight.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_verdict_for_another_account_cannot_pull_blocks() {
        let (_dir, server, (cid_in, _), _) = server_with_two_scoped_blocks().await;
        let slot = Mutex::new(Some(AdmittedConnection {
            verdict: AdmissionVerdict {
                account: [0xBB; 32],
                scopes: AdmittedScopes::AllOfAccount,
                expires_at: None,
            },
            device_key: None,
            custody_grant_id: None,
        }));
        let peer = EndpointKey::from_bytes([0x78; 32]);

        let err = server
            .handle_blocks_pull(&peer, &slot, pull_payload(&[&cid_in]))
            .await
            .expect_err("another account's verdict must be refused");
        assert_eq!(err.code, ERR_NOT_ADMITTED, "{err:?}");
    }

    // ── W8.5 P1: the multi-account registry ──────────────────────────────

    use fauna_core::data::{Capability, DeviceAuthorization};
    use fauna_core::encoding::sign_envelope;

    /// A signed `DeviceAuthorization` witness for `account` covering `device`.
    fn device_witness(account: &ActorKeypair, device: [u8; 32]) -> EmbedAsBytes {
        let cert = DeviceAuthorization {
            actor_id: account.actor_id(),
            device_key: device,
            capabilities: vec![Capability::RenewBearer],
            created_at: Timestamp(1_000),
            expires_at: None,
        };
        let (bytes, env) = sign_envelope(account, &cert).unwrap();
        EmbedAsBytes::from_signed(bytes, env)
    }

    fn device_admit_payload(account: &ActorKeypair, device: [u8; 32]) -> Value {
        to_value(&PeerSyncAdmitRequest {
            witness_kind: WITNESS_DEVICE_AUTHORIZATION.to_string(),
            witness: device_witness(account, device),
            endpoints: None,
            extra: Default::default(),
        })
        .unwrap()
    }

    /// A custodied store registered via `set_custodied`, holding one staged
    /// content record — so a served block PROVES store routing (the own
    /// store never held it).
    async fn custodied_store(
        owner_hex: &str,
    ) -> (tempfile::TempDir, ServeStoreHandle, ContentHash, String) {
        let dir = tempfile::tempdir().unwrap();
        let store = AccountStore::open(
            SqliteBackend::open(dir.path()).unwrap(),
            owner_hex,
            WriterId([0x0E; 32]),
        )
        .await
        .unwrap();
        let scope = format!("content:post:{}", "cc".repeat(32));
        let (cid, _) = store
            .stage_local_record(&scope, "post", b"a block only the custodied store holds")
            .await
            .unwrap();
        (dir, ServeStoreHandle::spawn(store), cid, scope)
    }

    /// Wrap a custodied handle in its serve entry — the tests answer with an
    /// (unpresented on these hand-slotted paths) empty witness.
    fn served(handle: ServeStoreHandle) -> ServedAccount {
        ServedAccount {
            store: handle,
            reply_witness: EmbedAsBytes {
                envelope: vec![],
                bytes: vec![],
                signer_auth: None,
            },
            reply_witness_kind: WITNESS_CUSTODY_GRANT.to_string(),
        }
    }

    // ── the admit door's removed-device arm ──────────────────────────────

    /// A view excluding exactly `(account, device)` — the shape the pump
    /// feeds in production (own-account snapshot behind an account guard).
    fn removed_view(account: [u8; 32], device: [u8; 32]) -> DeviceRemovedFn {
        Arc::new(move |acct: &[u8; 32], key: &[u8; 32]| *acct == account && *key == device)
    }

    /// The finding this arm exists for: the fleet cert never expires
    /// ("removal is the control"), so without this view a removed device
    /// keeps an `AllOfAccount` verdict at every sibling, forever.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_removed_device_is_refused_at_the_admit_door() {
        let device = [0x0Fu8; 32];
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let (_dir, server, _own) =
            admit_server_with(None, Some(removed_view(owner.actor_id().0, device))).await;
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(device);

        let err = server
            .handle_admit(&peer, &slot, device_admit_payload(&owner, device))
            .await
            .expect_err("a removed device must not be admitted");
        assert_eq!(err.code, ERR_WITNESS_REFUSED, "{err:?}");
        assert!(slot.lock().unwrap().is_none(), "no verdict on refusal");
    }

    /// **Refuse on the `Removed` row only.** A device the view does not
    /// exclude still admits — which is what keeps the check additive across
    /// version skew: a device enrolled before the plane-side device-set
    /// existed has no row at all, and refusing "not present" would sever
    /// every such sibling.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_device_with_no_removal_row_still_admits() {
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let (_dir, server, _own) =
            admit_server_with(None, Some(removed_view(owner.actor_id().0, [0xEEu8; 32]))).await;
        let device = [0x0Fu8; 32];
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(device);

        server
            .handle_admit(&peer, &slot, device_admit_payload(&owner, device))
            .await
            .expect("an unexcluded device admits");
        let entry = slot.lock().unwrap().clone().expect("verdict stored");
        assert_eq!(
            entry.device_key,
            Some(device),
            "the `DeviceAuthorization` arm records the key the per-request re-check binds to"
        );
    }

    /// **The view is keyed by SERVED ACCOUNT, never by device key alone.**
    /// The serve side is a registry (own account + every custodied one), so
    /// a global device-key predicate would let one account's fleet exclusion
    /// refuse another account's device of the same key — and, worse, read as
    /// if it had answered the right question.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_removal_in_one_account_does_not_sever_another_accounts_fleet() {
        let device = [0x0Fu8; 32];
        let own_account = ActorKeypair::from_secret([9u8; 32]);
        // The view excludes this device for the node's OWN account only.
        let (_dir, server, _own) =
            admit_server_with(None, Some(removed_view(own_account.actor_id().0, device))).await;
        let owner = ActorKeypair::from_secret([21u8; 32]);
        let owner_hex = hex::encode(owner.actor_id().0);
        let (_cdir, handle, _cid, _scope) = custodied_store(&owner_hex).await;
        server.set_custodied(HashMap::from([(owner.actor_id().0, served(handle))]));

        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(device);
        server
            .handle_admit(&peer, &slot, device_admit_payload(&owner, device))
            .await
            .expect("the custodied account's own fleet is not governed by our exclusions");
        assert_eq!(
            slot.lock()
                .unwrap()
                .clone()
                .expect("verdict")
                .verdict
                .account,
            owner.actor_id().0
        );
    }

    /// **Severance is per REQUEST, not per connection.** A cert with no
    /// expiry means a connection admitted before the removal would otherwise
    /// keep pulling indefinitely; the preflight re-checks the view on every
    /// request, exactly as it re-checks served-account membership.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_removal_severs_an_already_admitted_connection_at_its_next_request() {
        let device = [0x0Fu8; 32];
        let owner = ActorKeypair::from_secret([21u8; 32]);
        let owner_hex = hex::encode(owner.actor_id().0);
        // The view flips mid-connection, as the pump's snapshot refresh does.
        let removed: Arc<std::sync::atomic::AtomicBool> =
            Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&removed);
        let owner_account = owner.actor_id().0;
        let view: DeviceRemovedFn = Arc::new(move |acct: &[u8; 32], key: &[u8; 32]| {
            *acct == owner_account
                && *key == device
                && flag.load(std::sync::atomic::Ordering::SeqCst)
        });
        let (_dir, server, _own) = admit_server_with(None, Some(view)).await;
        let (_cdir, handle, cid, _scope) = custodied_store(&owner_hex).await;
        server.set_custodied(HashMap::from([(owner.actor_id().0, served(handle))]));

        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(device);
        server
            .handle_admit(&peer, &slot, device_admit_payload(&owner, device))
            .await
            .expect("admitted while still enrolled");
        server
            .handle_blocks_pull(&peer, &slot, pull_payload(&[&cid]))
            .await
            .expect("and serving");

        removed.store(true, std::sync::atomic::Ordering::SeqCst);
        let err = server
            .handle_blocks_pull(&peer, &slot, pull_payload(&[&cid]))
            .await
            .expect_err("the live connection must be severed at its next request");
        assert_eq!(err.code, ERR_NOT_ADMITTED, "{err:?}");
    }

    /// The whole custodian serve path, hand-slotted: an owner-fleet device's
    /// `DeviceAuthorization` for a CUSTODIED account admits, and its pulls
    /// route to the custodied store — the block only that store holds
    /// serves, which no single-account server could have answered.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_custodied_accounts_fleet_admits_and_serves_from_the_custodied_store() {
        let (_dir, server, _own) = admit_server(None).await;
        let owner = ActorKeypair::from_secret([21u8; 32]);
        let owner_hex = hex::encode(owner.actor_id().0);
        let (_cdir, handle, cid, _scope) = custodied_store(&owner_hex).await;
        server.set_custodied(HashMap::from([(owner.actor_id().0, served(handle))]));

        let device = [0x0Fu8; 32];
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(device);
        server
            .handle_admit(&peer, &slot, device_admit_payload(&owner, device))
            .await
            .expect("the custodied account's fleet admits");
        let verdict = slot.lock().unwrap().clone().expect("verdict stored");
        assert_eq!(verdict.verdict.account, owner.actor_id().0);

        let reply = server
            .handle_blocks_pull(&peer, &slot, pull_payload(&[&cid]))
            .await
            .expect("the pull routes to the custodied store");
        let reply: PeerSyncBlocksPullReply = from_value(&reply).unwrap();
        assert_eq!(
            reply.blocks.len(),
            1,
            "the custodied store's block serves: {reply:?}"
        );
    }

    /// An account this node does not serve is refused at admission — the
    /// registry is the gate, and the refusal names it before any
    /// verification runs against the wrong expectation.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_witness_for_an_unserved_account_is_refused_at_admission() {
        let (_dir, server, _own) = admit_server(None).await;
        let stranger = ActorKeypair::from_secret([22u8; 32]);
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes([0x10; 32]);

        let err = server
            .handle_admit(&peer, &slot, device_admit_payload(&stranger, [0x10; 32]))
            .await
            .expect_err("an unserved account's witness must be refused");
        assert_eq!(err.code, ERR_WITNESS_REFUSED, "{err:?}");
        assert!(
            format!("{:?}", err.details).contains("does not serve"),
            "the refusal names the registry, not the signature: {err:?}"
        );
    }

    /// T13: a custodian serves only the custodied account's FLEET — a
    /// custody-grant witness claiming a custodied account (a
    /// custody-of-custody chain) is refused by name at admission, even
    /// with a live revocation view and a verifying signature.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_custody_grant_admits_only_against_the_granting_accounts_own_fleet() {
        let (_dir, server, _own) = admit_server(Some(Arc::new(|_: &[u8]| false))).await;
        let owner = ActorKeypair::from_secret([21u8; 32]);
        let owner_hex = hex::encode(owner.actor_id().0);
        let (_cdir, handle, _cid, _scope) = custodied_store(&owner_hex).await;
        server.set_custodied(HashMap::from([(owner.actor_id().0, served(handle))]));

        // A custody grant SIGNED BY the custodied owner — valid in itself,
        // but presented at a custodian's listener it would chain custody.
        let custodian2 = [0xC6u8; 32];
        let (payload, _) = custody_admit_payload(&owner, custodian2);
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(custodian2);

        let err = server
            .handle_admit(&peer, &slot, payload)
            .await
            .expect_err("custody-of-custody must be refused");
        assert_eq!(err.code, ERR_WITNESS_REFUSED, "{err:?}");
        assert!(
            format!("{:?}", err.details).contains("own fleet"),
            "the refusal names the chain rule: {err:?}"
        );
    }

    /// Dropping a custodied entry severs at the NEXT REQUEST, live
    /// connections included — the registry lookup runs in every preflight,
    /// so a custody the pump stopped serving refuses without waiting for
    /// re-admission.
    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_a_custodied_entry_severs_at_the_next_request() {
        let (_dir, server, _own) = admit_server(None).await;
        let owner = ActorKeypair::from_secret([21u8; 32]);
        let owner_hex = hex::encode(owner.actor_id().0);
        let (_cdir, handle, cid, _scope) = custodied_store(&owner_hex).await;
        server.set_custodied(HashMap::from([(owner.actor_id().0, served(handle))]));

        let device = [0x0Fu8; 32];
        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(device);
        server
            .handle_admit(&peer, &slot, device_admit_payload(&owner, device))
            .await
            .expect("admits while served");

        server.set_custodied(HashMap::new());
        let err = server
            .handle_blocks_pull(&peer, &slot, pull_payload(&[&cid]))
            .await
            .expect_err("the dropped custody refuses on the live connection");
        assert_eq!(err.code, ERR_NOT_ADMITTED, "{err:?}");
    }

    // ── the custodied account's exclusion list (the held grants') ────────

    /// A custody grant `owner` signed for this node, naming `removed` in its
    /// removed-device exclusion list — what a `custodies-held` row carries.
    fn held_grant(owner: &ActorKeypair, grant_byte: u8, removed: &[[u8; 32]]) -> EmbedAsBytes {
        sign_custody_grant(
            owner,
            &CustodyGrant {
                grant_id: vec![grant_byte; CUSTODY_GRANT_ID_LEN],
                owner: owner.actor_id(),
                custodian_key: [0xC5u8; 32],
                scopes: CustodyScopeSet::Account,
                minted_at: Timestamp(1_000),
                expires_at: Timestamp(9_000),
                removed_devices: fauna_core::custody_grant::canonical_removed_devices(
                    removed.iter().copied(),
                ),
            },
        )
        .unwrap()
    }

    /// The residual this closes: a custodian cannot read the custodied
    /// account's sealed device-set rows, so the owner-signed list on the
    /// grant it holds is its exclusion set for that account. A removed owner
    /// device presenting a perfectly valid (never-expiring) fleet cert is
    /// refused at the custodian's admit door, and a device the list does not
    /// name still admits.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_held_grants_exclusion_list_refuses_a_removed_owner_device_at_the_admit_door() {
        let removed = [0x0Fu8; 32];
        let honest = [0x0Au8; 32];
        let owner = ActorKeypair::from_secret([21u8; 32]);
        let exclusions = crate::admission::CustodiedExclusions::default();
        exclusions.replace(crate::admission::CustodiedExclusions::derive([
            &held_grant(&owner, 0x1D, &[removed]),
        ]));
        let (_dir, server, _own) = admit_server_with(None, Some(Arc::new(exclusions.view()))).await;
        let (_cdir, handle, _cid, _scope) = custodied_store(&hex::encode(owner.actor_id().0)).await;
        server.set_custodied(HashMap::from([(owner.actor_id().0, served(handle))]));

        let slot = Mutex::new(None);
        let err = server
            .handle_admit(
                &EndpointKey::from_bytes(removed),
                &slot,
                device_admit_payload(&owner, removed),
            )
            .await
            .expect_err("a device the held grant lists must not be admitted");
        assert_eq!(err.code, ERR_WITNESS_REFUSED, "{err:?}");
        assert!(slot.lock().unwrap().is_none(), "no verdict on refusal");

        let slot = Mutex::new(None);
        server
            .handle_admit(
                &EndpointKey::from_bytes(honest),
                &slot,
                device_admit_payload(&owner, honest),
            )
            .await
            .expect("an owner device the list does not name admits");
    }

    /// A grant minted BEFORE the removal carries an empty list and admits the
    /// device (the honest bound: until expiry or re-mint). Once a grant minted
    /// after the removal is held, the refreshed map severs the device's LIVE
    /// connection at its next request — and the map is the union over the
    /// held grants, so the older, empty-listed grant cannot mask the newer.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_re_minted_grants_list_severs_a_live_connection_at_its_next_request() {
        let device = [0x0Fu8; 32];
        let owner = ActorKeypair::from_secret([21u8; 32]);
        let before = held_grant(&owner, 0x1D, &[]);
        let exclusions = crate::admission::CustodiedExclusions::default();
        exclusions.replace(crate::admission::CustodiedExclusions::derive([&before]));
        let (_dir, server, _own) = admit_server_with(None, Some(Arc::new(exclusions.view()))).await;
        let (_cdir, handle, cid, _scope) = custodied_store(&hex::encode(owner.actor_id().0)).await;
        server.set_custodied(HashMap::from([(owner.actor_id().0, served(handle))]));

        let slot = Mutex::new(None);
        let peer = EndpointKey::from_bytes(device);
        server
            .handle_admit(&peer, &slot, device_admit_payload(&owner, device))
            .await
            .expect("a grant minted before the removal admits the device");
        server
            .handle_blocks_pull(&peer, &slot, pull_payload(&[&cid]))
            .await
            .expect("and serves it");

        // The owner revokes and re-grants; the pump's next pass holds both.
        let after = held_grant(&owner, 0x1E, &[device]);
        exclusions.replace(crate::admission::CustodiedExclusions::derive([
            &before, &after,
        ]));
        let err = server
            .handle_blocks_pull(&peer, &slot, pull_payload(&[&cid]))
            .await
            .expect_err("the live connection must be severed at its next request");
        assert_eq!(err.code, ERR_NOT_ADMITTED, "{err:?}");
    }

    /// The map is keyed by the SIGNING account: a list binds only the account
    /// that signed it (another account's device of the same key admits), an
    /// account with no held grant admits as before (absent admits — the
    /// additive-across-skew rule), and a witness whose signature does not
    /// verify contributes nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_exclusion_map_binds_only_the_signing_account() {
        let device = [0x0Fu8; 32];
        let owner = ActorKeypair::from_secret([21u8; 32]);
        let other = ActorKeypair::from_secret([22u8; 32]);
        let mut tampered = held_grant(&other, 0x1F, &[device]);
        let last = tampered.bytes.len() - 1;
        tampered.bytes[last] ^= 0x01;
        let map = crate::admission::CustodiedExclusions::derive([
            &held_grant(&owner, 0x1D, &[device]),
            &tampered,
        ]);
        assert_eq!(map.len(), 1, "the tampered witness contributes nothing");
        let exclusions = crate::admission::CustodiedExclusions::default();
        exclusions.replace(map);
        let view = exclusions.view();
        assert!(view(&owner.actor_id().0, &device));
        assert!(!view(&other.actor_id().0, &device));
        assert!(!view(&[0x77u8; 32], &device));
    }
}
