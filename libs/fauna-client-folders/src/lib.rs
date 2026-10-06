//! Typed-call wrapper for the `fauna.folders.*` WS-RPC kinds — the
//! folder management surface clients hit from the Devices/Peers page:
//! the folder list (`list`), the per-set enrolled-member roster
//! (`members.{add,list,remove}`), the per-set syncing-device list with
//! progress (`devices`), selective-sync path edits and config (`update`),
//! the exclusive-write lease
//! (`lease.{acquire,release}`), creation (`create`), and deletion
//! (`delete`) — the core of the `fauna.folders.*` namespace (sharing,
//! content-key, token and place kinds sit beside them; the
//! sibling `fauna.sync.conflicts.*` cluster lives in `fauna-client-sync`).
//!
//! These are the WS-RPC twins of the legacy `/api/v1/file-sets[/...]` HTTP
//! (Track B14). The Linux app is the first consumer (the Devices-page
//! folder row CRUD); the other 5 clients
//! lift this crate rather than reimplement the kind-composition (priority
//! #1/#2). It also gives the eventual `FolderWizardMachine` HTTP→WS-RPC
//! migration its `create` seam to land on.
//!
//! Pattern: same shape as `fauna-client-snapshots` — a thin
//! `FoldersClient<R: RpcRequester>`, one async method per kind, no state
//! machine, wasm-clean (no `fauna-client` dependency).

use fauna_protocol::RpcRequester;
use fauna_protocol::folders::{
    ActorMembersListRemoteRequest, ActorMembersListReply, ActorMembersListRequest,
    ContentKeyGetReply, ContentKeyGetRequest, ContentKeyPutReply, ContentKeyPutRequest,
    FolderCreateReply, FolderCreateRequest, FolderDeleteReply, FolderDeleteRequest,
    FolderDevicesReply, FolderDevicesRequest, FolderSetWebPaywallReply, FolderSetWebPaywallRequest,
    FolderShareReply, FolderShareRequest, FolderUpdateReply, FolderUpdateRequest, FoldersListReply,
    FoldersListRequest, KIND_FOLDERS_CONTENT_KEY_GET, KIND_FOLDERS_CONTENT_KEY_PUT,
    KIND_FOLDERS_CREATE, KIND_FOLDERS_DELETE, KIND_FOLDERS_DEVICES, KIND_FOLDERS_LEASE_ACQUIRE,
    KIND_FOLDERS_LEASE_RELEASE, KIND_FOLDERS_LEAVE, KIND_FOLDERS_LIST, KIND_FOLDERS_MEMBERS_EVICT,
    KIND_FOLDERS_MEMBERS_LIST, KIND_FOLDERS_MEMBERS_LIST_ACTORS,
    KIND_FOLDERS_MEMBERS_LIST_ACTORS_REMOTE, KIND_FOLDERS_MEMBERS_REMOVE,
    KIND_FOLDERS_MEMBERS_SET_ACCESS, KIND_FOLDERS_PLACES_SET, KIND_FOLDERS_READ_TOKEN_GET,
    KIND_FOLDERS_SET_WEB_PAYWALL, KIND_FOLDERS_SHARE, KIND_FOLDERS_UPDATE,
    KIND_FOLDERS_WRITE_TOKEN_GET, LeaseAcquireReply, LeaseAcquireRequest, LeaseReleaseReply,
    LeaseReleaseRequest, MemberEvictReply, MemberEvictRequest, MemberLeaveReply,
    MemberLeaveRequest, MemberRemoveReply, MemberRemoveRequest, MembersListReply,
    MembersListRequest, PlaceFlags, PlacesSetReply, PlacesSetRequest, ReadTokenGetReply,
    ReadTokenGetRequest, WriteTokenGetReply, WriteTokenGetRequest,
};

pub use fauna_protocol::folders;

/// The by-name set funnel (`fauna_protocol::folders::addressed`), re-exported
/// beside the folders client that sends through it.
pub use fauna_protocol::folders::addressed;

// The owner-side "Shared with" roster derivation (`role == "member"` filter +
// the `folder-shared-badge` count it feeds) — ungated, pure data shaping over
// an already-fetched `actor_members_list` reply.
pub mod roster;
pub use roster::member_actors;

// The recipient-side decline recipe (`folder-share-decline-button`) — the roster
// drop a decline owes on top of the durable-inbox ack. Ungated (no `mls`): it is
// two plain RPCs over the thin clients, so the web SPA and every native app run
// the identical code (priority #2).
pub mod public_follow;
// The follow's two WRITE recipes (follow / unfollow), composed once over
// `public_follow`'s reads + `fauna_client_config`'s persistence. `mls`-gated only
// to reuse `ConversationsClient::actor_by_handle` rather than re-derive its
// not-found mapping; every app that can follow already links that stack.
#[cfg(feature = "mls")]
pub mod follow_ops;
pub mod recipient;
pub use recipient::{
    AcceptShareError, DeclineShareError, FolderWelcomeJoin, accept_folder_share,
    decline_folder_share,
};

// The periodic-reconcile cadence — one constant every device ticks at. Ungated
// (no `mls`): the always-resident engines and the Windows
// bearer-only on-demand hydration host all schedule their re-pull on it.
pub mod cadence;
pub use cadence::DEFAULT_RESCAN_INTERVAL;

pub mod custody;
// The one create and one delete every production set gesture routes through:
// the set nonce minted into custody before the nest sees the set, and custody
// retired before the delete (`mls-group-key-material.md` § M2 → *Custody shape
// of the set nonce*). Ungated, transport-generic.
pub mod set_lifecycle;
pub use set_lifecycle::{
    ReseedTargetPrep, SetCustodyCut, SetLifecycleError, create_set, create_set_with_owner_root,
    delete_set, prepare_reseed_targets, prepare_reseed_targets_logged, record_nonce,
};
// The real MLS-engine-backed `FolderGroupCrypto` adapter — gated behind the
// optional `mls` feature so the base crate stays fauna-mls-free (wasm-light). It
// adds only an `impl FolderGroupCrypto for Arc<MlsEngine>` (no public items), so
// there's nothing to re-export — enabling `mls` makes the impl available to any
// consumer with an `Arc<MlsEngine>` in hand (fauna-ffi / fauna-wasm).
#[cfg(feature = "mls")]
mod mls_adapter;
// The one public item the adapter module does carry: the share leg's M2
// roster consult over `Arc<MlsEngine>` (`p2p.md` § Cross-user shared-set
// transfer — the serve admission's membership seam).
#[cfg(all(feature = "mls", feature = "p2p-share"))]
pub use mls_adapter::MlsSetMembership;
// The share leg's roster adapter (`fauna_peer_share::SetMembership` over
// `FolderGroupCrypto`) — part of the gated `p2p-share` plane; the module's
// own docs carry the fail-closed rule.
#[cfg(feature = "p2p-share")]
pub mod peer_share_adapter;
// The engine content-key binding resolver — pure over folder-keys custody and a
// folder list, and fauna-mls-free (the custody channel id is
// `fauna_core::folder_keys::channel_id_for_group`), so the bearer-only desktop
// sync agent resolves its own content keys without linking MLS.
pub mod engine_binding;
#[cfg(feature = "mail-settings")]
pub use engine_binding::CustodyServedSets;
pub use engine_binding::{
    DeclassificationAnchor, EngineKeyBinding, EngineKeyBindingError, FolderChannelOwners,
    custody_channel_for, custody_served, custody_serves_any, owned_custody_channel,
    resolve_engine_key_binding, resolve_engine_key_bindings, resolve_foreign_engine_key_bindings,
    retired_serve_custody,
};
// The impure companion to `engine_binding`: read the folder-keys custody +
// list folders + resolve every set's engine keys. The single content-key **produce** path,
// credential-agnostic — the share host's pump on the seed arm, the desktop sync
// agent on the seedless (`BackupKey`) arm.
pub mod engine_keys;
pub use engine_keys::{
    EngineKeysError, adoption_markers_or_none, engine_keys_from, resolve_engine_keys,
};
// The read half of the folder-key custody seam once custody is plane-only
// (`fauna.state.folder-keys`): what an engine build reads custody through,
// implemented over the account's store and over a capability host's throwaway
// replica, in the crates that own each (`on-demand-files.md` § Shared sets on
// a capability host, decision 1′).
pub mod key_reader;
pub use key_reader::{
    ADOPTION_MARKERS_META_KEY, CustodyNotices, FolderKeyReader, FolderKeyStore,
    MemoryFolderKeyStore, UnreadableFolderKeys, decode_adoption_markers, decode_refetch_request,
    encode_adoption_markers, encode_refetch_request, refetch_request_meta_key,
};
/// The custody seam's crossing of web's account port.
pub mod port;
// The launch-time removal resume as the shared `PostRestoreHook` body (the one
// hook every tokio leg hands to `fauna_client_mls_sync::launcher` and linux
// drives itself). `mls`-gated (it builds a `FoldersAuthor`) + native-gated
// (the hook trait lives in the native-only launcher module).
#[cfg(all(feature = "mls", not(target_arch = "wasm32")))]
pub mod launch_resume;
#[cfg(all(feature = "mls", not(target_arch = "wasm32")))]
pub use launch_resume::FolderRemovalResume;
// The member content-key custody-ingest seam (Phase 0 — the read leg): the
// nest-backed `FolderCustodySink` the conversations session drives at join +
// rotation-commit receipt so a member can decrypt a shared set's content, and
// the sibling `FolderKeyResolver` the Media machine drives. `mls`-gated only
// (generic over `RpcRequester`, like every other `Nest*` seam — native
// `Arc<NestClient>`, wasm `WsRpcClient`).
// The set names a folder grant id is matched over, read from custody + the
// folder list; its `fauna_client_capabilities::OwnedSetNames` impl is
// `mls`-gated for the capabilities dep.
pub mod owned_set_names;
pub use owned_set_names::{CustodyOwnedSetNames, read_owned_set_names};
#[cfg(feature = "mls")]
pub mod custody_ingest;
#[cfg(feature = "mls")]
pub use custody_ingest::{
    FolderCustodyObserver, NestFolderCustodySink, NestFolderKeyResolver, record_signing,
};
// Leave a shared set: nest roster self-drop + local MLS-group forget. `mls`-gated
// (needs `ConversationsSession` to forget the group locally) + native-gated (like
// `launch_resume` above): `ConversationsSession::folder_home_url`/`leave_folder`
// are themselves `not(target_arch = "wasm32")` in `fauna-conversations`, so this
// module cannot build on wasm while `mls` is on regardless of its own generic
// `RpcRequester` bound — no caller reaches it from web today (only linux/tui).
#[cfg(all(feature = "mls", not(target_arch = "wasm32")))]
pub mod leave;
#[cfg(all(feature = "mls", not(target_arch = "wasm32")))]
pub use leave::leave_share;
// Assemble the owner-side shared-folder author (thin `fauna.folders.*` client +
// identity + the conversations rail's shared `MlsEngine` as the
// group-crypto seam). `mls`-gated + native-gated for the same reason as `leave`
// above: `ConversationsSession::engine`/`backend` need it. Every native call
// site — `fauna-ffi`, linux, tui — hand-copied this ceremony before this.
#[cfg(all(feature = "mls", not(target_arch = "wasm32")))]
pub mod build_author;
#[cfg(all(feature = "mls", not(target_arch = "wasm32")))]
pub use build_author::build_folders_author;
pub mod orchestration;
#[cfg(feature = "mls")]
pub mod served_reseal;
#[cfg(feature = "mls")]
pub use served_reseal::served_set_converge;
// (The sentinel-gated pre-bind re-seal driver that once lived beside
// `orchestration` — `reseal::run_pending_reseal` — was retired 2026-09-25: the
// pass is the sync agent's ungated, idempotent
// `SyncEngine::reseal_pending_under_current` on every engine start, and no
// custody sentinel marks a set as owing it. `mls-group-key-material.md` § M2.)
// The S8 session-start seal-backfill SWEEP: D1 over the folder plane, then D3
// over each owned set's snapshot tags, best-effort throughout. The two passes
// were already shared; the sequencing around them (owner-only skip, per-set
// isolation) was hand-written once per client, so it is written once here
// instead. Transport-generic like every other seam here — `run_sweep` is what a
// client's post-auth hook calls (native `Arc<NestClient>`, wasm `WsRpcClient`).
pub mod seal_backfill;
// The `WebdavKeysBlob` reconciler — (re-)provision the actor's MSEK-sealed
// served-set content-key blob from the current served flags + custody. MLS-gated
// (the seal is `fauna_mls::wrapped_blob::seal_webdav_keys_blob`); the per-app
// glue calls it right after `serve_enable`/`serve_disable`/a served-set rotation.
// A third-party principal's folder read twins over a served set — renewed by
// the serve reconcile, revoked by the serve-off tail (`webdav-server.md`
// § Key model → *A principal's read* rule (4)). MLS-gated like the grant
// mint it rebuilds wraps with.
#[cfg(feature = "mls")]
pub mod principal_grants;
#[cfg(feature = "mls")]
pub use principal_grants::{FolderGrantError, PrincipalFolderGrants};
#[cfg(feature = "mls")]
pub mod webdav_provision;
#[cfg(feature = "mls")]
pub use webdav_provision::{
    FollowStep, ServedBlobFollower, WebdavProvisionError, can_serve_webdav, owner_can_serve_webdav,
    reconcile_webdav_keys_blob,
};

/// Typed `fauna.folders.*` call surface, generic over the WS-RPC
/// transport (`R: RpcRequester`): native call sites pass `Arc<NestClient>`,
/// the wasm SPA passes its `WsRpcClient`. The kind-composition logic is
/// written once here and shared across native + wasm (priority #2). Errors
/// propagate as the transport's `R::Error`.
/// Report of one [`FoldersClient::backfill_sealed_fields`] pass (S8 D1): how
/// many seal-only stamps were submitted per plane, plus the fail-closed and
/// failure tallies. All-zero = converged, the steady state after one pass.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SealBackfillReport {
    /// Sets whose `name_sealed` was stamped.
    pub names: usize,
    /// Sets whose selective-sync pair (`include_paths_sealed` /
    /// `exclude_paths_sealed`) gained at least one stamp.
    pub selective_sync: usize,
    /// Sets whose `retention_policy_sealed` was stamped.
    pub retention: usize,
    /// Bound rows whose M2 content keys this custody could not resolve — their
    /// audience-root fields (name / retention) were skipped **fail-closed**
    /// rather than sealed under a root no roster member could open.
    /// Non-zero is not an error: another keyed client of the roster
    /// converges them.
    pub bound_skipped: usize,
    /// Rows the custody resolved to a DIFFERENT set identity than the row's own
    /// (`keys.mls_group_id` ≠ the row's, both-absent counting as equal) —
    /// skipped fail-closed, **all** fields including the owner-only pair
    /// (identity is in doubt, so nothing about the row's audience is trusted).
    /// The defense-in-depth guard: name-keyed custody can shadow —
    /// an owned row eclipses a same-named member row in the roster `find` — and
    /// a sealed label must never ride an eclipsing set's root.
    pub identity_mismatch: usize,
    /// Per-row update submissions that failed (transient fault / refused) —
    /// skipped, the pass reruns at the next start.
    pub update_failures: usize,
}

impl SealBackfillReport {
    /// Total sets that received at least one stamp this pass.
    pub fn stamped(&self) -> usize {
        self.names + self.selective_sync + self.retention
    }
}

/// What [`FoldersClient::ensure_place`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsurePlaceOutcome {
    /// The device already held a place; nothing was written.
    AlreadyPlaced,
    /// The device held no place and was enrolled at the default point.
    Enrolled,
}

pub struct FoldersClient<R: RpcRequester> {
    nest: R,
    /// This connection's label custody — what makes [`Self::update`] a **keyed**
    /// writer of `folders.retention_policy_sealed` (path-sealing S6-e).
    /// Default-empty, so a client built without [`Self::with_label_custody`]
    /// behaves exactly as this crate did before sealing.
    custody: fauna_core::label_custody::LabelCustody,
    /// The owner's identity key — what makes [`Self::set_audience`] an
    /// **attesting** declassify. `None` on a client built without
    /// [`Self::with_audience_attestor`].
    attestor: Option<std::sync::Arc<fauna_core::identity::ActorKeypair>>,
}

impl<R: RpcRequester> FoldersClient<R> {
    pub fn new(nest: R) -> Self {
        Self {
            nest,
            custody: fauna_core::label_custody::LabelCustody::default(),
            attestor: None,
        }
    }

    /// Wire the owner's identity key, so a `→public` [`Self::set_audience`]
    /// carries the owner's signed attestation (`encryption-at-rest.md`
    /// § Readable classes → *The declassification is owner-ATTESTED*).
    ///
    /// Without it the flip still lands on the nest, but **no verifying seat
    /// unseals the folder** — it reads as public and rests sealed. Every
    /// embedder whose UI offers the audience control must wire this.
    pub fn with_audience_attestor(
        mut self,
        keypair: std::sync::Arc<fauna_core::identity::ActorKeypair>,
    ) -> Self {
        self.attestor = Some(keypair);
        self
    }

    /// Wire this connection's label custody, making [`Self::update`] seal the
    /// retention policy on the way out (path-sealing S6-e).
    ///
    /// Without it the update still succeeds and the nest simply *clears* the
    /// sealed column beside the plaintext write — the ratified pair-moves-together
    /// degrade, leaving an S8 backfill row rather than a seal that opens to the
    /// policy it replaced.
    pub fn with_label_custody(mut self, custody: fauna_core::label_custody::LabelCustody) -> Self {
        self.custody = custody;
        self
    }

    /// The wired custody, read-only. Exists so a *consumer* can pin its own
    /// wiring at the construction seam (a custody-shape pin in
    /// `fauna-core` cannot observe a caller quietly downgrading to
    /// `owner_only` — the regression pin must read the custody this client will
    /// actually seal with).
    pub fn label_custody(&self) -> &fauna_core::label_custody::LabelCustody {
        &self.custody
    }

    /// Seal a set's retention policy under the audience root, for
    /// [`Self::update`]'s write half.
    ///
    /// **Label-audience, not owner-only** — `seal_retention_policy` takes a
    /// `LabelRoot`, so a bound set's M2 content-key generation is the right root
    /// here, unlike the selective-sync pair whose funnel takes a `BackupKey`. The
    /// nest ships this field's plaintext to a roster member today, so sealing it
    /// owner-only would narrow disclosure under cover of hardening.
    ///
    /// `None` = nothing to seal: a keyless client, a set this reader holds no
    /// seal root for, or a **bound** set whose keys the resolver cannot produce
    /// — `label_seal_root()` bails for that last one (reachable since
    /// a bound-but-unresolvable resolve keeps its
    /// `mls_group_id`), so the user's save records plaintext-only rather than
    /// sealing under an owner root the roster could not open. **Best-effort by
    /// design** — a derivation failure must not fail the user's save; the row
    /// lands plaintext-only for S8, exactly like the device-label and
    /// selective-sync seams.
    async fn seal_retention(&self, name: &str, policy: &str) -> Option<fauna_protocol::ByteBuf> {
        let (keys, _) = self.custody.keys_for(name).await;
        let root = keys.label_seal_root().ok().flatten()?;
        fauna_core::label_custody::seal_retention_policy(&root, name, policy)
            .ok()
            .map(fauna_protocol::ByteBuf::from)
    }

    /// Render every row's sealed labels **sealed-first** — the set's `name`
    /// (from `name_sealed`), then its `retention_policy` (the read half of
    /// [`Self::seal_retention`]) — applied to both list projections, so every
    /// app keeps reading `FolderSummary::name` once the nest stops resting the
    /// plaintext (`path-sealing.md` § the set-name plane).
    ///
    /// Per-row custody resolution, because each row is its own set and a bound
    /// set's audience root differs per set. The salt is the row's own `name_hash`,
    /// which the nest projects beside each seal on both arms precisely so this
    /// call still works once the plaintext `name` scrubs. The name renders
    /// first and a row whose name this reader cannot open is **omitted** — the
    /// ratified degrade, never a blank name (a reserved `__` or public-audience
    /// set carries no seal and keeps its plaintext).
    ///
    /// The ONLY safe skip is *"nothing on this page is sealed"* — deliberately not
    /// "custody is empty", because a keyless reader meeting a sealed-only row must
    /// reach `Omit` rather than show that row's blank plaintext as its name or
    /// policy (the S3 bug `LabelCustody::is_keyless` was deleted over).
    ///
    /// Custody resolves by that same hash (`keys_for_hash`), never the
    /// plaintext: a scrubbed row's `name` is blank, and the keys must be in
    /// hand before the name can be opened at all.
    async fn render_sealed_labels(&self, sets: &mut Vec<fauna_protocol::folders::FolderSummary>) {
        if sets
            .iter()
            .all(|s| s.name_sealed.is_none() && s.retention_policy_sealed.is_none())
        {
            return;
        }
        let mut rendered = Vec::with_capacity(sets.len());
        for mut set in std::mem::take(sets) {
            let name_hash = set.name_hash.as_deref().map(|b| &b[..]);
            let (keys, _) = self
                .custody
                .keys_for_hash(&fauna_core::label_custody::set_name_label_salt(
                    name_hash, &set.name,
                ))
                .await;
            let name = match fauna_core::label_custody::render_set_name(
                &keys,
                set.name_sealed.as_deref().map(|b| &b[..]),
                &set.name,
                name_hash,
            ) {
                fauna_core::path_crypto::SealedLabelRender::Sealed(name)
                | fauna_core::path_crypto::SealedLabelRender::Plaintext(name) => name,
                fauna_core::path_crypto::SealedLabelRender::Omit => continue,
            };
            // The policy's salt falls back to the *wire* plaintext, never the
            // rendered name: both are the same set, and the wire hash wins
            // whenever it is present anyway.
            set.retention_policy = fauna_core::label_custody::render_retention_policy(
                &keys,
                set.retention_policy_sealed.as_deref().map(|b| &b[..]),
                set.retention_policy.as_deref(),
                &set.name,
                name_hash,
            );
            set.name = name;
            rendered.push(set);
        }
        *sets = rendered;
    }

    /// Borrow the underlying transport — for the sibling `fauna.bridges.*` kinds
    /// that ride the same connection (the `webdav_provision` reconciler's
    /// `provision_webdav_keys_blob`).
    pub fn requester(&self) -> &R {
        &self.nest
    }

    /// `fauna.folders.list` — every folder the bearer actor **owns**, with
    /// the cached stat columns and the selective-sync `include_paths` /
    /// `exclude_paths`. The WS-RPC twin of `GET /api/v1/file-sets`.
    /// Replay-safe pure read. This is the owner-scoped contract the sync engines
    /// and the owner-side author flow rely on; for the folders management UI
    /// that also surfaces sets shared *with* the caller, use
    /// [`Self::list_owned_and_shared`].
    pub async fn list(&self) -> Result<FoldersListReply, R::Error> {
        let mut reply = self.list_wire().await?;
        self.render_sealed_labels(&mut reply.folders).await;
        Ok(reply)
    }

    /// [`Self::list`] as the nest sent it — **no label render**, so no row is
    /// omitted: a sealed set's `name` is the empty sentinel beside its
    /// `name_hash` + `name_sealed` (`path-sealing.md` § the set-name plane).
    /// For a caller that addresses sets by id or hash and never shows a name,
    /// or that renders with custody it holds itself — never for a display
    /// list, which must not show a blank name. [`Self::list`] on a client built
    /// without [`Self::with_label_custody`] drops every sealed set instead.
    pub async fn list_wire(&self) -> Result<FoldersListReply, R::Error> {
        self.nest
            .request(
                KIND_FOLDERS_LIST,
                FoldersListRequest {
                    include_shared_with_me: None,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.folders.list` with the **member-visible projection** (B3): the
    /// caller's owned sets unioned with sets they are a roster member of — each
    /// shared-with-me set a `role == "member"` summary carrying the owner's
    /// handle and the set's `mls_group_id`. Replay-safe pure read.
    ///
    /// ⚠ A `role == "member"` row is only *rostered* nest-side (the nest cannot
    /// observe an MLS join). The caller MUST filter member rows to sets it has
    /// actually joined — `MlsEngine::has_group(ChannelId::from_group_id(
    /// mls_group_id))` — before rendering, else a stranger's un-accepted knock
    /// appears unbidden. See `FolderSummary::role`.
    pub async fn list_owned_and_shared(&self) -> Result<FoldersListReply, R::Error> {
        let mut reply = self.list_owned_and_shared_wire().await?;
        self.render_sealed_labels(&mut reply.folders).await;
        Ok(reply)
    }

    /// [`Self::list_owned_and_shared`] with **no label render** — the
    /// member-visible twin of [`Self::list_wire`], under the same contract.
    pub async fn list_owned_and_shared_wire(&self) -> Result<FoldersListReply, R::Error> {
        self.nest
            .request(
                KIND_FOLDERS_LIST,
                FoldersListRequest {
                    include_shared_with_me: Some(true),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.folders.create` — create a new folder owned by the bearer
    /// actor. The WS-RPC twin of `POST /api/v1/file-sets`. A name already in
    /// use returns `fauna.folders.conflict`; an invalid mode returns
    /// `fauna.folders.invalid_request`. This is the `create` seam the
    /// `FolderWizardMachine` HTTP→WS-RPC migration lands on.
    ///
    /// Crate-private on purpose: every production create goes through
    /// [`crate::set_lifecycle::create_set`], which mints the set nonce into
    /// custody first — the compiler, not a grep, keeps a bare create out.
    ///
    /// A sealed create leaves by hash alone ([`addressed`]), so the nest's
    /// reply echoes no name; the caller's own is put back on it.
    pub(crate) async fn create(
        &self,
        req: FolderCreateRequest,
    ) -> Result<FolderCreateReply, R::Error> {
        let name = req.name.clone();
        let mut reply: FolderCreateReply = self
            .nest
            .request(KIND_FOLDERS_CREATE, addressed(req))
            .await?;
        if reply.name.is_empty() {
            reply.name = name;
        }
        Ok(reply)
    }

    /// `fauna.folders.share` — transition an owner-only set to a **cross-user
    /// shared** set by binding it to a **client-created** MLS group (`req.group_id`
    /// = hex of the raw openMLS group id from [`fauna_mls::engine::MlsEngine::group_id_bytes`]).
    /// The nest derives the 32-byte `ChannelId` via `ChannelId::from_group_id`,
    /// first-binder-wins-claims that channel (registering **the owner** on the
    /// roster), and stamps `mls_group_id`; the reply echoes the derived
    /// `channel_id` (hex) the caller threads into [`orchestration::FoldersAuthor::bind_set`]
    /// and the per-member `welcome.deliver`. Owner-scoped: a non-owner / unknown set
    /// folds to `fauna.folders.not_found`; a channel already claimed by another
    /// actor (a removed member's re-bind, an outsider) returns
    /// `fauna.folders.already_claimed`. The set must already exist (`create`).
    /// This is the thin wire call; the full create-group → share → bind →
    /// deliver-welcome sequence is orchestrated above it (5d(b-pre)).
    pub async fn share(&self, req: FolderShareRequest) -> Result<FolderShareReply, R::Error> {
        self.nest.request(KIND_FOLDERS_SHARE, addressed(req)).await
    }

    /// `fauna.folders.members.list` — the enrolled-device roster for one
    /// folder (`device_id`, `label`, `flags`). The WS-RPC twin of
    /// `GET /api/v1/file-sets/{name}/members`. Replay-safe pure read.
    pub async fn members_list(
        &self,
        name: impl Into<String>,
    ) -> Result<MembersListReply, R::Error> {
        self.nest
            .request(
                KIND_FOLDERS_MEMBERS_LIST,
                addressed(MembersListRequest {
                    name: name.into(),
                    ..Default::default()
                }),
            )
            .await
    }

    /// `fauna.folders.members.list_actors` — the *actor* (user) roster of a
    /// shared folder: who the set is shared with, the owner-side "Shared with"
    /// list (`docs/goal/ui/folders.md` § Sharing). Each entry carries the
    /// actor's hex id, its nest-resolved `handle` (empty when unknown), and a
    /// `role` of `"owner"` or `"member"`. Owner/member-gated (same read gate as
    /// `content_key.get`); an owner-only (unshared) set returns
    /// `fauna.folders.not_shared`. Replay-safe pure read. (Distinct from
    /// `members_list`, which is the enrolled *device* roster.)
    pub async fn actor_members_list(
        &self,
        name: impl Into<String>,
    ) -> Result<ActorMembersListReply, R::Error> {
        self.nest
            .request(
                KIND_FOLDERS_MEMBERS_LIST_ACTORS,
                addressed(ActorMembersListRequest {
                    name: name.into(),
                    ..Default::default()
                }),
            )
            .await
    }

    /// `fauna.folders.members.list_actors_remote` — the same actor roster for a
    /// **cross-nest** member's set: the caller's own nest relays the read to the
    /// set's home nest (`fauna.federation.folder.actors.fetch`), addressed by
    /// channel, never by name (`federation.md` § Cross-nest…, *The cross-nest
    /// writer roster read*). Ids-only — every `handle` is empty — and stamped
    /// with the home nest's `caller_access`. A distinct kind, so an old nest
    /// fails `unknown_kind` rather than answering a same-named own set.
    pub async fn actor_members_list_remote(
        &self,
        channel_id_hex: impl Into<String>,
        nest_url: impl Into<String>,
    ) -> Result<ActorMembersListReply, R::Error> {
        self.nest
            .request(
                KIND_FOLDERS_MEMBERS_LIST_ACTORS_REMOTE,
                ActorMembersListRemoteRequest {
                    channel_id: channel_id_hex.into(),
                    nest_url: nest_url.into(),
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.folders.update` — partial update; every `None` field is left
    /// unchanged server-side. For a selective-sync path save, pass only
    /// Set a folder's audience — `private` | `shared` | `public`
    /// (`folders.md` § Target re-model owns the model).
    ///
    /// **Needs no MLS engine and no content key, in EVERY direction** — the
    /// bound `→shared` flip-back included. The flip is one
    /// `folders.update` write: the back-catalogue is moved by each device's own
    /// engine at its next catch-up (`SyncEngine::converge_corpus_to_audience`,
    /// which dispatches the sealed direction on bound-ness), on every seat, the
    /// members' included, and nothing is staged for any direction — a per-actor
    /// custody sentinel could never reach a member's engine, so the projection
    /// is the cross-device *signal*.
    ///
    /// **It is not the cross-device *authority*: the `→public` flip carries the
    /// owner's signature.** A seat unseals only on an attestation it verifies
    /// against the owner's identity, never on the nest's report of the audience
    /// (`encryption-at-rest.md` § Readable classes → *The declassification is
    /// owner-ATTESTED*). This is the gesture every app's owner confirm lands
    /// in, so it is where the attestation is minted — over the folder id and
    /// the last served attestation, both read from one `folders.list` first.
    /// Re-sending `public` on an already-public folder re-mints, which is how a
    /// folder created public, or declassified before attestations existed, is
    /// healed. The sealed directions send no attestation: sealing is always
    /// safe, and the nest keeps the last one so the next mint counts above it.
    ///
    /// The nest validates what it accepts: to `public` from any audience, to
    /// `private` only while unbound, to `shared` only while bound — plus the
    /// cross-toggle refusals (WebDAV-serve ⊕ public, paywall ⊕ public, each
    /// refused whichever side moves second). A refused flip returns the error;
    /// it never partially applies.
    pub async fn set_audience(&self, name: &str, audience: &str) -> Result<(), R::Error> {
        let audience_attestation = match (&self.attestor, audience) {
            (Some(keypair), fauna_protocol::folders::AUDIENCE_PUBLIC) => {
                // Unrendered, matched by hash: a sealed set rests no plaintext
                // name (schema 114), and a miss would send the flip unattested.
                let listed = self.list_wire().await?;
                listed
                    .folders
                    .iter()
                    .find(|fs| fs.is_named(name))
                    .map(|fs| {
                        fauna_protocol::folders::AudienceAttestation::mint(
                            keypair,
                            fs.id,
                            name,
                            fauna_core::data::Timestamp::now_millis_or_zero(),
                            fs.audience_attestation.as_ref(),
                        )
                    })
            }
            _ => None,
        };
        self.update(fauna_protocol::folders::FolderUpdateRequest {
            name: name.to_string(),
            audience: Some(audience.to_string()),
            audience_attestation,
            ..Default::default()
        })
        .await
        .map(|_| ())
    }

    /// Set a folder's **content residency** — `"full"` (the nest keeps chunk
    /// bytes; the default) or `"metadata_only"` (bytes never rest on the nest:
    /// seats skip byte upload, the nest drops its copy, content moves between
    /// seats by transient relay while a holder is online). Folders re-model
    /// phase 5; `file-sync.md` § Content residency owns the model.
    ///
    /// Keyless, like [`Self::set_audience`]: a plain `folders.update` on the
    /// request's own `residency` field (never folded into the sent-whole
    /// `nest_place` record). **The flip to `metadata_only` is consent-gated in
    /// the app** — the nest deletes its chunk bytes for the folder on that
    /// write, so every app arms `folder-residency-confirm` naming exactly that
    /// and calls this only once answered. The nest refuses it on reserved rails
    /// and, both directions, against any serving surface (website ⊕ webdav ⊕
    /// paywall); a refusal returns the error and never partially applies.
    pub async fn set_residency(&self, name: &str, residency: &str) -> Result<(), R::Error> {
        self.update(fauna_protocol::folders::FolderUpdateRequest {
            name: name.to_string(),
            residency: Some(residency.to_string()),
            ..Default::default()
        })
        .await
        .map(|_| ())
    }

    /// Turn a folder's **exclusive editing** on or off — "one device at a time
    /// may write to this folder" (`file-sync.md` § Exclusive editing owns the
    /// mechanism; `folder-exclusive-editing-toggle` is the owner's control).
    ///
    /// Keyless, like [`Self::set_residency`]: a plain `folders.update` on the
    /// request's own `exclusive_editing` field, so no other property rides the
    /// write. Turning it OFF does not revoke a lease a device is holding — the
    /// holder's own release or the TTL ends it (the nest's ruling, commented at
    /// its handler). The nest refuses the flag on a reserved rail.
    pub async fn set_exclusive_editing(&self, name: &str, on: bool) -> Result<(), R::Error> {
        self.update(fauna_protocol::folders::FolderUpdateRequest {
            name: name.to_string(),
            exclusive_editing: Some(on),
            ..Default::default()
        })
        .await
        .map(|_| ())
    }

    /// Flip a folder's `website_enabled` (schema v41, `DEFAULT 0`) — "serve this
    /// folder as your website".
    ///
    /// **The only door to a website folder.** Phase 2 slice e retired the create
    /// wizard's mode step (a folder has no type), which left no way to make one
    /// at all until this slice; `folders.md` § Implementation status today
    /// records that gap as accepted and explicitly not to be patched by
    /// re-adding a mode control.
    ///
    /// Orthogonal to audience: this publishes the folder's head (the nest
    /// re-keyed the former `mode == "web"` fan-out to this flag), while the
    /// audience decides who may read what is published. Enabling it on a folder
    /// that is neither `public` nor paywalled is allowed and inert — nothing is
    /// readable until one of those lands. Keyless, as [`Self::set_audience`].
    pub async fn set_website_enabled(&self, name: &str, enabled: bool) -> Result<(), R::Error> {
        self.update(fauna_protocol::folders::FolderUpdateRequest {
            name: name.to_string(),
            website_enabled: Some(enabled),
            ..Default::default()
        })
        .await
        .map(|_| ())
    }

    /// `include_paths` / `exclude_paths` and leave `mode` / `retention_policy`
    /// `None`. The WS-RPC twin of `PUT /api/v1/file-sets/{name}`. An unknown
    /// set returns `fauna.folders.not_found`.
    /// ⚠ **The retention seal is minted here, not by the caller.** When `req`
    /// carries a `retention_policy` and no `retention_policy_sealed`, this mints
    /// one from the connection's custody (S6-e) — so every caller of this one
    /// method is a keyed writer without each app repeating the derivation. A
    /// caller that supplies its own seal is left alone (the S8 backfill shape).
    pub async fn update(
        &self,
        mut req: FolderUpdateRequest,
    ) -> Result<FolderUpdateReply, R::Error> {
        if req.retention_policy_sealed.is_none()
            && let Some(policy) = req.retention_policy.clone()
        {
            req.retention_policy_sealed = self.seal_retention(&req.name, &policy).await;
        }
        self.nest.request(KIND_FOLDERS_UPDATE, addressed(req)).await
    }

    /// The S8 D1 **folder-plane seal backfill**: walk the owner's own sets and
    /// stamp every missing sealed sibling from the still-resting dual-write
    /// plaintext, through the nest's seal-only update arm (`(None, Some(sealed))`
    /// stamps in place; nothing plaintext is ever sent back). Client-driven by
    /// necessity — no server-side backfill is possible for client-keyed seals
    /// (`file-sync.md` § Sealed names & paths → *Migration*).
    ///
    /// Per-field predicates, all keyed on the **nest-observed NULL** (idempotent
    /// with no local marker; the steady state is one list read and zero writes):
    ///
    /// - `name_sealed` missing → stamp under the audience root (convergent, so a
    ///   re-stamp is byte-identical and free). Reserved `__` rails never seal.
    /// - `include_paths_sealed` / `exclude_paths_sealed` missing beside a
    ///   non-empty plaintext list → stamp under the **owner's** `BackupKey`
    ///   (their audience is owner-only — S6-c; random nonce, hence
    ///   only-if-missing).
    /// - `retention_policy_sealed` missing beside a plaintext policy → stamp
    ///   under the audience root (random nonce, only-if-missing).
    ///
    /// ⚠ **The guard is derived from the ROW, not from custody:** a bound
    /// row (`mls_group_id` present) whose M2 content keys this custody cannot
    /// resolve skips the two audience-root fields **fail-closed** — an
    /// owner-root seal there would be unopenable by every roster member and,
    /// post-flip, member-visible loss.
    /// `label_seal_root` reaches the same refusal on its own for a
    /// resolver-carrying custody (a bound-but-unresolvable resolve keeps its
    /// `mls_group_id`, so the bail arm fires); the row-level check stays as the
    /// authority here because this pass also runs under a resolver-LESS custody
    /// in tests, and because the row is simply the stronger witness. The
    /// owner-only selective-sync pair still stamps on such rows — the owner
    /// root is that pair's *correct* audience.
    ///
    /// ⚠ **Row-identity guard:** the custody is name-keyed and
    /// can resolve a *different* set than the row in hand (an owned row
    /// eclipses a same-named member row in the roster `find`; a resolver
    /// regression could reach a colliding foreign record). Before anything
    /// stamps, the resolved `keys.mls_group_id` must equal the row's own
    /// (both-absent = equal); a mismatch skips the whole row fail-closed and
    /// counts in [`SealBackfillReport::identity_mismatch`].
    ///
    /// Reads the raw wire list deliberately (not [`Self::list`], whose
    /// `render_sealed_labels` rewrites `retention_policy` in place — this pass's
    /// predicates must see the resting truth). Per-row update failures are
    /// counted and skipped, never fatal: the pass reruns at the next start.
    pub async fn backfill_sealed_fields(&self) -> Result<SealBackfillReport, R::Error> {
        let mut report = SealBackfillReport::default();
        let reply: FoldersListReply = self
            .nest
            .request(
                KIND_FOLDERS_LIST,
                FoldersListRequest {
                    include_shared_with_me: None,
                    extra: Default::default(),
                },
            )
            .await?;
        for row in &reply.folders {
            if fauna_core::sync::is_reserved_folder_name(&row.name) {
                continue;
            }
            let (keys, _) = self
                .custody
                .keys_for_row(&row.name, row.name_hash.as_ref().map(|b| &b[..]))
                .await;
            // row-identity guard, see the doc comment. Corrupt
            // row hex counts as a mismatch: identity unknowable ⇒ nothing
            // about the row's audience is trusted.
            let row_group = match row.mls_group_id.as_deref() {
                Some(hex_id) => match hex::decode(hex_id.trim()) {
                    Ok(raw) => Some(raw),
                    Err(_) => {
                        report.identity_mismatch += 1;
                        continue;
                    }
                },
                None => None,
            };
            if keys.mls_group_id != row_group {
                report.identity_mismatch += 1;
                continue;
            }
            let bound = row.mls_group_id.is_some();
            let audience_root = if bound && keys.content_keys.is_none() {
                report.bound_skipped += 1;
                None
            } else {
                keys.label_seal_root().ok().flatten()
            };

            // A scrubbed row (blank plaintext name) has no name left to seal,
            // and its name is no salt to seal the retention policy under — both
            // name-salted stamps skip it. Its seals were minted at the keyed
            // create or rename that scrubbed it.
            let named = !row.name.is_empty();
            let name_sealed = match (&row.name_sealed, &audience_root) {
                (None, Some(root)) if named => {
                    fauna_core::label_custody::seal_set_name(root, &row.name)
                        .ok()
                        .flatten()
                        .map(fauna_protocol::ByteBuf::from)
                }
                _ => None,
            };
            let retention_policy_sealed = match (
                &row.retention_policy,
                &row.retention_policy_sealed,
                &audience_root,
            ) {
                (Some(policy), None, Some(root)) if named => {
                    fauna_core::label_custody::seal_retention_policy(root, &row.name, policy)
                        .ok()
                        .map(fauna_protocol::ByteBuf::from)
                }
                _ => None,
            };
            let owner = self.custody.owner_key();
            let include_paths_sealed = match (&row.include_paths, &row.include_paths_sealed, &owner)
            {
                (Some(paths), None, Some(owner)) if !paths.is_empty() => {
                    fauna_core::label_custody::seal_include_paths(owner, row.id, paths)
                        .ok()
                        .map(fauna_protocol::ByteBuf::from)
                }
                _ => None,
            };
            let exclude_paths_sealed = match (&row.exclude_paths, &row.exclude_paths_sealed, &owner)
            {
                (Some(paths), None, Some(owner)) if !paths.is_empty() => {
                    fauna_core::label_custody::seal_exclude_paths(owner, row.id, paths)
                        .ok()
                        .map(fauna_protocol::ByteBuf::from)
                }
                _ => None,
            };

            if name_sealed.is_none()
                && retention_policy_sealed.is_none()
                && include_paths_sealed.is_none()
                && exclude_paths_sealed.is_none()
            {
                continue;
            }
            let stamped_name = name_sealed.is_some();
            let stamped_selective =
                include_paths_sealed.is_some() || exclude_paths_sealed.is_some();
            let stamped_retention = retention_policy_sealed.is_some();
            let req = FolderUpdateRequest {
                name: row.name.clone(),
                name_sealed,
                include_paths_sealed,
                exclude_paths_sealed,
                retention_policy_sealed,
                ..Default::default()
            };
            match self
                .nest
                .request::<_, FolderUpdateReply>(KIND_FOLDERS_UPDATE, addressed(req))
                .await
            {
                Ok(_) => {
                    report.names += usize::from(stamped_name);
                    report.selective_sync += usize::from(stamped_selective);
                    report.retention += usize::from(stamped_retention);
                }
                // Best-effort per row (an owner-scope refusal, a transient
                // fault): count and continue — the pass reruns next start, and
                // failing the whole walk over one row would starve the rest.
                Err(_) => report.update_failures += 1,
            }
        }
        Ok(report)
    }

    /// `fauna.folders.set_web_paywall` — paywall a `web`-mode folder to a
    /// subscription tier (`Some(tier)`) or clear it (`None`). A dedicated kind
    /// rather than a `update` field because clear-vs-leave-unchanged would need a
    /// nested `Option` dag-cbor cannot round-trip (`monetization.md` § Pillar 2).
    /// Both the set and the tier are the caller's; the nest gates the setter to
    /// website-enabled folders and refuses an unknown tier. The
    /// [`FoldersAuthor::paywall_set`](crate::orchestration::FoldersAuthor)
    /// orchestration drives this after content-keying the set.
    pub async fn set_web_paywall(
        &self,
        name: impl Into<String>,
        tier: Option<String>,
    ) -> Result<FolderSetWebPaywallReply, R::Error> {
        self.nest
            .request(
                KIND_FOLDERS_SET_WEB_PAYWALL,
                addressed(FolderSetWebPaywallRequest {
                    name: name.into(),
                    tier,
                    ..Default::default()
                }),
            )
            .await
    }

    /// `fauna.folders.delete` — remove a folder the bearer owns. The
    /// WS-RPC twin of `DELETE /api/v1/file-sets/{name}`. **Non-cascading:**
    /// deleting a set that still has snapshots surfaces as
    /// `fauna.folders.internal` (the DB FK refuses the row drop); callers
    /// surface that to the user rather than swallow it. A concurrent
    /// destructive op returns `fauna.folders.conflict`.
    ///
    /// Crate-private on purpose: every production delete goes through
    /// [`crate::set_lifecycle::delete_set`], which retires the set's custody
    /// first.
    pub(crate) async fn delete(
        &self,
        name: impl Into<String>,
    ) -> Result<FolderDeleteReply, R::Error> {
        self.nest
            .request(
                KIND_FOLDERS_DELETE,
                addressed(FolderDeleteRequest {
                    name: name.into(),
                    ..Default::default()
                }),
            )
            .await
    }

    /// `fauna.folders.devices` — the devices syncing one folder, each with
    /// its sync progress (`label`, `last_change_at`, `change_count`). The
    /// WS-RPC twin of `GET /api/v1/file-sets/{name}/devices`. Replay-safe pure
    /// read. (Distinct from `members.list`: members are the *enrolled* roster
    /// with places; devices are the ones with recorded sync activity.)
    pub async fn devices(&self, name: impl Into<String>) -> Result<FolderDevicesReply, R::Error> {
        self.nest
            .request(
                KIND_FOLDERS_DEVICES,
                addressed(FolderDevicesRequest {
                    name: name.into(),
                    ..Default::default()
                }),
            )
            .await
    }

    /// `fauna.folders.members.remove` — unenroll one device from a folder.
    /// The WS-RPC twin of `DELETE /api/v1/file-sets/{name}/members/{device_id}`.
    /// An unknown set returns `fauna.folders.not_found`.
    pub async fn members_remove(
        &self,
        req: MemberRemoveRequest,
    ) -> Result<MemberRemoveReply, R::Error> {
        self.nest
            .request(KIND_FOLDERS_MEMBERS_REMOVE, addressed(req))
            .await
    }

    /// `fauna.folders.places.set` — enrol a device into a folder, or change
    /// what its place *does* (`originates` / `accepts` / `applies_deletes`;
    /// `docs/goal/behavior/folders.md` § Target re-model → places). The one
    /// add/edit door onto a device place; every one of the eight flag points
    /// is writable. An unknown set returns `fauna.folders.not_found`.
    pub async fn places_set(&self, req: PlacesSetRequest) -> Result<PlacesSetReply, R::Error> {
        self.nest
            .request(KIND_FOLDERS_PLACES_SET, addressed(req))
            .await
    }

    /// **A local presence writes the place it needs** (`file-sync.md` § 4): the
    /// one shared enrol every gesture that gives a device a local presence on
    /// `name` calls — a desktop bind (agent-side, `SetLocationFolder`) and the
    /// apple/android on-demand toggle turned ON.
    ///
    /// Reads the roster ([`Self::members_list`]); if `device_id_hex` already
    /// holds a place, writes nothing and returns
    /// [`EnsurePlaceOutcome::AlreadyPlaced`] — the enrol fills a gap, it never
    /// resets a place the user chose. Otherwise sends
    /// [`Self::places_set`] at [`PlaceFlags::default_place`] and returns
    /// [`EnsurePlaceOutcome::Enrolled`]. Device ids compare case-insensitively
    /// (both are hex).
    ///
    /// Read-then-write, not atomic: a place the user edits between the two
    /// round trips could be overwritten by the default — a window of one RPC,
    /// on a device that held no place a moment earlier. Callers treat an error
    /// as best-effort (the engine's `Absent` default covers the gap until the
    /// next gesture).
    pub async fn ensure_place(
        &self,
        name: impl Into<String>,
        device_id_hex: &str,
    ) -> Result<EnsurePlaceOutcome, R::Error> {
        let name = name.into();
        let roster = self.members_list(name.clone()).await?;
        if roster
            .members
            .iter()
            .any(|m| m.device_id.eq_ignore_ascii_case(device_id_hex))
        {
            return Ok(EnsurePlaceOutcome::AlreadyPlaced);
        }
        self.places_set(PlacesSetRequest {
            name,
            device_id: device_id_hex.to_ascii_lowercase(),
            flags: PlaceFlags::default_place(),
            ..Default::default()
        })
        .await?;
        Ok(EnsurePlaceOutcome::Enrolled)
    }

    /// `fauna.folders.lease.acquire` — acquire an exclusive-access lease for
    /// conflict-free writes on a folder, for one device. The WS-RPC twin of
    /// `POST /api/v1/file-sets/{name}/lease`. `acquired == false` when another
    /// device already holds the lease.
    pub async fn lease_acquire(
        &self,
        req: LeaseAcquireRequest,
    ) -> Result<LeaseAcquireReply, R::Error> {
        self.nest
            .request(KIND_FOLDERS_LEASE_ACQUIRE, addressed(req))
            .await
    }

    /// `fauna.folders.lease.release` — release a folder's exclusive-access
    /// lease. The WS-RPC twin of `DELETE /api/v1/file-sets/{name}/lease`.
    /// `device_id` (hex, required) scopes the release to the holder that
    /// acquired it: a device frees only its own lease, never
    /// another actor's. There is no holder-blind form — the device-id-less
    /// owner force-release was retired 2026-09-24 (compat-remnant sweep); a
    /// crash-stuck lease lapses on the nest's TTL.
    pub async fn lease_release(
        &self,
        name: impl Into<String>,
        device_id: impl Into<String>,
    ) -> Result<LeaseReleaseReply, R::Error> {
        self.nest
            .request(
                KIND_FOLDERS_LEASE_RELEASE,
                addressed(LeaseReleaseRequest {
                    name: name.into(),
                    device_id: device_id.into(),
                    ..Default::default()
                }),
            )
            .await
    }

    /// `fauna.folders.write_token.get` — a cross-nest **writer** asks its own
    /// nest to relay a short-lived byte-plane write token from the set's home
    /// nest, so it can POST sealed chunks/manifests DIRECT to the home nest.
    /// Channel-keyed (a foreign set's `name` only resolves on its home nest);
    /// `nest_url` is the home nest URL from the `ForeignFolder` record. Returns
    /// `(token, expires_at)` — `expires_at` is absolute Unix seconds.
    pub async fn write_token_get(
        &self,
        nest_url: impl Into<String>,
        channel_id: impl Into<String>,
    ) -> Result<WriteTokenGetReply, R::Error> {
        self.nest
            .request(
                KIND_FOLDERS_WRITE_TOKEN_GET,
                WriteTokenGetRequest {
                    nest_url: nest_url.into(),
                    channel_id: channel_id.into(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.folders.read_token.get` — the read-scoped twin of
    /// [`Self::write_token_get`]: any cross-nest **member** asks its own nest
    /// to relay a short-lived byte-plane read token from the set's home nest,
    /// which admits it at the store-miss relay arm of the chunk route and
    /// re-checks the membership at every request. A reader has no other
    /// bearer for that plane — the write-token mint refuses it.
    ///
    /// Its engine consumer is a cross-nest reader's byte plane, which takes it
    /// in place of the write-token bearer
    /// (`fauna_sync_engine::write_token_bearer::folder_read_token_bearer`,
    /// `on-demand-files.md` § Shared sets on a capability host, decision 3).
    pub async fn read_token_get(
        &self,
        nest_url: impl Into<String>,
        channel_id: impl Into<String>,
    ) -> Result<ReadTokenGetReply, R::Error> {
        self.nest
            .request(
                KIND_FOLDERS_READ_TOKEN_GET,
                ReadTokenGetRequest {
                    nest_url: nest_url.into(),
                    channel_id: channel_id.into(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// The set's change rows exactly as the nest serves them —
    /// `fauna.sync.changes.list` from the start, unverified: the served-era
    /// adoption's sweep reads the rows readers exempt while the set is served
    /// (`writer-signed-change-records.md` ruling (7)(b)). Owner-scoped; one
    /// reply carries the whole log. Addressed by the set's hash like every
    /// other set request — the nest holds no plaintext name for a sealed set.
    pub async fn changes_raw(
        &self,
        name: &str,
    ) -> Result<Vec<fauna_protocol::sync::SyncChange>, R::Error> {
        let reply: fauna_protocol::sync::SyncChangesListReply = self
            .nest
            .request(
                "fauna.sync.changes.list",
                addressed(fauna_protocol::sync::SyncChangesListRequest {
                    folder: Some(name.to_string()),
                    since: 0,
                    ..Default::default()
                }),
            )
            .await?;
        Ok(reply.changes)
    }

    /// `fauna.folders.served_rows.adopt` — one page of the owner's in-place
    /// signatures over the set's WebDAV pseudo-device rows (ruling (7)(b)).
    pub async fn served_rows_adopt(
        &self,
        req: fauna_protocol::folders::ServedRowsAdoptRequest,
    ) -> Result<fauna_protocol::folders::ServedRowsAdoptReply, R::Error> {
        self.nest
            .request(
                fauna_protocol::folders::KIND_FOLDERS_SERVED_ROWS_ADOPT,
                addressed(req),
            )
            .await
    }

    /// `fauna.folders.members.evict` — evict a **cross-user** member (an
    /// `ActorId`, not a device) from a shared set's `actor_channels` roster, the
    /// metadata-confidentiality half of rotate-on-removal (shared folders
    /// Slice 3, F1/OBS-1). Owner-scoped; idempotent (`evicted == false` if the
    /// member was already absent). Distinct from [`Self::members_remove`], which
    /// unenrolls one of the owner's own sync *devices*. The `FoldersAuthor`
    /// orchestration drives this alongside the content-key rotation + envelope
    /// re-publish on a member removal.
    pub async fn members_evict(
        &self,
        req: MemberEvictRequest,
    ) -> Result<MemberEvictReply, R::Error> {
        self.nest
            .request(KIND_FOLDERS_MEMBERS_EVICT, addressed(req))
            .await
    }

    /// `fauna.folders.leave` — a **recipient** voluntarily leaves a set shared
    /// *with* them, self-dropping their own `actor_channels` row. The recipient
    /// counterpart to [`Self::members_evict`]: **self-scoped** (drops only the
    /// authenticated caller) and addressed by the raw `mls_group_id` the member
    /// holds in their member-visible `FolderSummary` — not the owner-only `name`.
    /// No `ownerSecret`; idempotent (`left == false` if the caller was not a
    /// member). The client pairs this with `MlsEngine::forget_group` to locally
    /// forget the MLS group (the recipient-side leave primitive).
    pub async fn leave(
        &self,
        group_id_hex: impl Into<String>,
    ) -> Result<MemberLeaveReply, R::Error> {
        self.leave_with_home(group_id_hex, None::<String>).await
    }

    /// [`Self::leave`] for a **foreign** (cross-nest) set: `home_nest_url` (from
    /// the member's own `ForeignFolder` record / `FolderSummary.home_nest_url`)
    /// rides the additive `nest_url`, so the caller's own nest relays
    /// `fauna.federation.channel.leave` to the set's home nest — killing all
    /// future federated fetches (generations already held are not revoked, exact
    /// parity with same-nest leave). `None` ⇒ the plain same-nest self-drop.
    pub async fn leave_with_home(
        &self,
        group_id_hex: impl Into<String>,
        home_nest_url: Option<impl Into<String>>,
    ) -> Result<MemberLeaveReply, R::Error> {
        self.nest
            .request(
                KIND_FOLDERS_LEAVE,
                MemberLeaveRequest::new(group_id_hex, home_nest_url),
            )
            .await
    }

    /// `fauna.folders.content_key.put` — the **owner** publishes the sealed M2
    /// content-key envelope (the full generation bundle) for a shared set.
    /// Owner-scoped; upserts keyed by the set's derived `ChannelId` (re-published
    /// on every membership change). Opaque ciphertext nest-side.
    pub async fn content_key_put(
        &self,
        req: ContentKeyPutRequest,
    ) -> Result<ContentKeyPutReply, R::Error> {
        self.nest
            .request(KIND_FOLDERS_CONTENT_KEY_PUT, addressed(req))
            .await
    }

    /// `fauna.folders.content_key.get` — any **readable** member (owner or
    /// roster member) fetches the latest sealed M2 envelope, fed verbatim to
    /// `MlsEngine::open_content_key_envelope`. A non-member or owner-only set
    /// folds to `fauna.folders.not_found` (ST-RES-1); a readable-but-unpublished
    /// set returns `fauna.folders.not_published`.
    pub async fn content_key_get(
        &self,
        req: ContentKeyGetRequest,
    ) -> Result<ContentKeyGetReply, R::Error> {
        self.nest
            .request(KIND_FOLDERS_CONTENT_KEY_GET, addressed(req))
            .await
    }

    /// `fauna.folders.members.set_access` — the **owner** grants or edits a
    /// member's `reader`/`writer` access (+ optional byte cap) on their shared
    /// set (multi-writer Phase 1; `ui/folders.md` § Sharing). Owner-scoped +
    /// claimant-gated nest-side, exactly like `content_key.put`. Role
    /// transitions never rotate the content key.
    pub async fn members_set_access(
        &self,
        req: fauna_protocol::folders::MemberSetAccessRequest,
    ) -> Result<fauna_protocol::folders::MemberSetAccessReply, R::Error> {
        self.nest
            .request(KIND_FOLDERS_MEMBERS_SET_ACCESS, addressed(req))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{MockRequester, RecordingRequester, block_on};

    // ── ensure_place: a local presence writes the place it needs ────────────

    /// Serves a CONFIGURED roster and records every request.
    struct RosterRequester {
        members: Vec<fauna_protocol::folders::FolderMember>,
        calls: std::sync::Mutex<Vec<(&'static str, Vec<u8>)>>,
    }

    impl RpcRequester for RosterRequester {
        type Error = std::convert::Infallible;

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
            self.calls.lock().unwrap().push((kind, bytes.to_vec()));
            let reply = match kind {
                "fauna.folders.members.list" => {
                    fauna_protocol::encode_canonical(&MembersListReply {
                        members: self.members.clone(),
                        extra: Default::default(),
                    })
                }
                "fauna.folders.places.set" => fauna_protocol::encode_canonical(&PlacesSetReply {
                    ok: true,
                    ..Default::default()
                }),
                other => panic!("unexpected kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    fn roster_client(
        members: Vec<fauna_protocol::folders::FolderMember>,
    ) -> FoldersClient<std::sync::Arc<RosterRequester>> {
        FoldersClient::new(std::sync::Arc::new(RosterRequester {
            members,
            calls: std::sync::Mutex::new(Vec::new()),
        }))
    }

    fn places_sets(
        client: &FoldersClient<std::sync::Arc<RosterRequester>>,
    ) -> Vec<PlacesSetRequest> {
        client
            .requester()
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| *k == "fauna.folders.places.set")
            .map(|(_, b)| fauna_protocol::decode_strict(b).expect("decode places.set"))
            .collect()
    }

    #[test]
    fn ensure_place_enrols_a_placeless_device_at_the_default_point() {
        let other = fauna_protocol::folders::FolderMember {
            device_id: "bb".repeat(32),
            label: "desk".into(),
            flags: PlaceFlags::archive_place(),
            ..Default::default()
        };
        let client = roster_client(vec![other]);
        let outcome = block_on(client.ensure_place("docs", &"AA".repeat(32))).unwrap();
        assert_eq!(outcome, EnsurePlaceOutcome::Enrolled);
        let sets = places_sets(&client);
        assert_eq!(sets.len(), 1);
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &sets[0], "docs"
        ));
        assert_eq!(
            sets[0].device_id,
            "aa".repeat(32),
            "sent in canonical lowercase"
        );
        assert_eq!(sets[0].flags, PlaceFlags::default_place());
    }

    #[test]
    fn ensure_place_writes_nothing_for_a_device_that_holds_a_place() {
        let mine = fauna_protocol::folders::FolderMember {
            device_id: "aa".repeat(32),
            label: "laptop".into(),
            flags: PlaceFlags::archive_place(),
            ..Default::default()
        };
        let client = roster_client(vec![mine]);
        let outcome = block_on(client.ensure_place("docs", &"AA".repeat(32))).unwrap();
        assert_eq!(outcome, EnsurePlaceOutcome::AlreadyPlaced);
        assert!(
            places_sets(&client).is_empty(),
            "never rewrites a chosen place"
        );
    }

    // ── S8 D1: the seal-backfill pass ───────────────────────────────────────

    /// A requester that serves a CONFIGURED list reply and records every
    /// request — the backfill pass's whole world (list once, stamp N times).
    struct BackfillRequester {
        list: FoldersListReply,
        calls: std::sync::Mutex<Vec<(&'static str, Vec<u8>)>>,
    }

    impl RpcRequester for BackfillRequester {
        type Error = std::convert::Infallible;

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
            self.calls.lock().unwrap().push((kind, bytes.to_vec()));
            let reply = match kind {
                "fauna.folders.list" => fauna_protocol::encode_canonical(&self.list),
                "fauna.folders.update" => fauna_protocol::encode_canonical(&FolderUpdateReply {
                    ok: true,
                    extra: Default::default(),
                }),
                other => panic!("unexpected kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    fn owner_key() -> fauna_core::crypto::BackupKey {
        fauna_core::crypto::BackupKey::from_bytes([7u8; 32])
    }

    fn backfill_client(
        rows: Vec<fauna_protocol::folders::FolderSummary>,
        custody: fauna_core::label_custody::LabelCustody,
    ) -> FoldersClient<std::sync::Arc<BackfillRequester>> {
        let req = std::sync::Arc::new(BackfillRequester {
            list: FoldersListReply {
                folders: rows,
                extra: Default::default(),
            },
            calls: std::sync::Mutex::new(Vec::new()),
        });
        FoldersClient::new(req).with_label_custody(custody)
    }

    fn updates(
        client: &FoldersClient<std::sync::Arc<BackfillRequester>>,
    ) -> Vec<FolderUpdateRequest> {
        client
            .requester()
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| *k == "fauna.folders.update")
            .map(|(_, b)| fauna_protocol::decode_strict(b).expect("decode update"))
            .collect()
    }

    // ── the set-name render on list ─────────────────────────────────────────

    /// Both list projections render `name` **sealed-first** from `name_sealed`
    /// under the row's own `name_hash`, so every app keeps reading `.name` once
    /// the nest's plaintext scrubs: a scrubbed row this reader can open shows
    /// its sealed name, a scrubbed row it cannot open is omitted (never shown
    /// blank), and an unsealed row — a reserved or public set — keeps its
    /// plaintext.
    #[test]
    fn list_renders_the_sealed_name_and_omits_an_unopenable_scrubbed_row() {
        let root = fauna_core::path_crypto::LabelRoot::owner_of(&owner_key());
        let sealed = |name: &str| {
            fauna_protocol::ByteBuf::from(
                fauna_core::label_custody::seal_set_name(&root, name)
                    .unwrap()
                    .unwrap(),
            )
        };
        let hash = |name: &str| {
            fauna_protocol::ByteBuf::from(fauna_core::path_crypto::set_name_hash(name).to_vec())
        };
        let foreign_root = fauna_core::path_crypto::LabelRoot::owner_of(
            &fauna_core::crypto::BackupKey::from_bytes([9u8; 32]),
        );
        let rows = vec![
            fauna_protocol::folders::FolderSummary {
                id: 1,
                name: String::new(),
                name_sealed: Some(sealed("Tax returns")),
                name_hash: Some(hash("Tax returns")),
                ..Default::default()
            },
            fauna_protocol::folders::FolderSummary {
                id: 2,
                name: String::new(),
                name_sealed: Some(fauna_protocol::ByteBuf::from(
                    fauna_core::label_custody::seal_set_name(&foreign_root, "theirs")
                        .unwrap()
                        .unwrap(),
                )),
                name_hash: Some(hash("theirs")),
                ..Default::default()
            },
            fauna_protocol::folders::FolderSummary {
                id: 3,
                name: "__config".into(),
                name_hash: Some(hash("__config")),
                ..Default::default()
            },
        ];
        for shared in [false, true] {
            let client = backfill_client(
                rows.clone(),
                fauna_core::label_custody::LabelCustody::owner_only(owner_key()),
            );
            let reply = if shared {
                block_on(client.list_owned_and_shared())
            } else {
                block_on(client.list())
            }
            .unwrap();
            let shown: Vec<(i64, &str)> = reply
                .folders
                .iter()
                .map(|s| (s.id, s.name.as_str()))
                .collect();
            assert_eq!(shown, vec![(1, "Tax returns"), (3, "__config")]);
        }
    }

    // ── exclusive editing ───────────────────────────────────────────────────

    /// The toggle's write is its own field and nothing else: an update that
    /// also carried a mode, an audience or a residency would silently re-write
    /// a property the owner did not touch.
    #[test]
    fn set_exclusive_editing_sends_only_its_own_field() {
        let client = backfill_client(vec![], fauna_core::label_custody::LabelCustody::default());
        block_on(client.set_exclusive_editing("db", true)).unwrap();
        block_on(client.set_exclusive_editing("db", false)).unwrap();
        let sent = updates(&client);
        let expect = |on| {
            addressed(FolderUpdateRequest {
                name: "db".into(),
                exclusive_editing: Some(on),
                ..Default::default()
            })
        };
        assert_eq!(sent, vec![expect(true), expect(false)]);
    }

    // ── the attesting declassify ────────────────────────────────────────────

    fn attesting_client(
        rows: Vec<fauna_protocol::folders::FolderSummary>,
        keypair: &std::sync::Arc<fauna_core::identity::ActorKeypair>,
    ) -> FoldersClient<std::sync::Arc<BackfillRequester>> {
        backfill_client(rows, fauna_core::label_custody::LabelCustody::default())
            .with_audience_attestor(std::sync::Arc::clone(keypair))
    }

    /// The owner confirm's landing point signs: the `→public` update carries an
    /// attestation a seat verifies against the owner's identity — for THIS
    /// folder's id and name. Re-confirming an already-public folder (the heal
    /// for a born-public or pre-attestation one) counts above what the nest
    /// served; the sealed directions send nothing.
    #[test]
    fn set_audience_public_mints_an_attestation_every_seat_can_verify() {
        use fauna_protocol::folders::{AUDIENCE_PUBLIC, AttestationMemory, FolderSummary};
        let owner = std::sync::Arc::new(fauna_core::identity::ActorKeypair::from_secret([3; 32]));
        let row = FolderSummary {
            id: 41,
            name: "site".into(),
            ..Default::default()
        };

        let client = attesting_client(vec![row.clone()], &owner);
        block_on(client.set_audience("site", AUDIENCE_PUBLIC)).unwrap();
        let sent = updates(&client).pop().expect("one update");
        assert_eq!(sent.audience.as_deref(), Some(AUDIENCE_PUBLIC));
        let first = sent
            .audience_attestation
            .expect("the flip must be attested");

        let served = FolderSummary {
            audience: AUDIENCE_PUBLIC.into(),
            audience_attestation: Some(first.clone()),
            ..row.clone()
        };
        let trusted = owner.actor_id();
        let judged = |fs: &FolderSummary, name: &str| {
            fs.judge_declassification(name, Some(&trusted), AttestationMemory::default())
                .0
        };
        assert!(
            judged(&served, "site"),
            "an honest flip must still arm the seat"
        );
        assert!(
            !judged(&served, "diary"),
            "…and only under the name it was signed for"
        );

        // Re-confirm while public: a fresh mint, strictly above the served one.
        let pinned = FolderSummary {
            audience_attestation: Some(fauna_protocol::folders::AudienceAttestation {
                counter: u64::MAX - 5,
                ..first
            }),
            ..served
        };
        let client = attesting_client(vec![pinned], &owner);
        block_on(client.set_audience("site", AUDIENCE_PUBLIC)).unwrap();
        let again = updates(&client)
            .pop()
            .unwrap()
            .audience_attestation
            .unwrap();
        assert_eq!(again.counter, u64::MAX - 4);

        // The flip-back is unattested — sealing is always safe.
        let client = attesting_client(vec![row.clone()], &owner);
        block_on(client.set_audience("site", "private")).unwrap();
        assert_eq!(updates(&client).pop().unwrap().audience_attestation, None);

        // No signer wired: the flip lands bare, and verifying seats seal it.
        let bare = backfill_client(
            vec![row],
            fauna_core::label_custody::LabelCustody::default(),
        );
        block_on(bare.set_audience("site", AUDIENCE_PUBLIC)).unwrap();
        assert_eq!(updates(&bare).pop().unwrap().audience_attestation, None);
    }

    /// One pass over three rows: a fully-unsealed unbound set is stamped on
    /// every plane (name exact-bytes — convergent; the three random-nonce
    /// fields by seal/open round-trip, the S6-d convention — and the request
    /// carries NO plaintext); an already-stamped set and a reserved rail are
    /// both untouched. Second pass on the stamped shape = zero writes.
    #[test]
    fn backfill_stamps_missing_fields_and_is_idempotent() {
        let unsealed = fauna_protocol::folders::FolderSummary {
            id: 5,
            name: "docs".into(),
            include_paths: Some(vec!["Documents/2026 taxes".into()]),
            exclude_paths: Some(vec!["Documents/cache".into()]),
            retention_policy: Some("{\"keep_last\":3}".into()),
            ..Default::default()
        };
        let already_stamped = fauna_protocol::folders::FolderSummary {
            id: 6,
            name: "photos".into(),
            name_sealed: Some(fauna_protocol::ByteBuf::from(vec![1u8; 8])),
            ..Default::default()
        };
        let reserved = fauna_protocol::folders::FolderSummary {
            id: 7,
            name: "__config".into(),
            ..Default::default()
        };
        let custody = fauna_core::label_custody::LabelCustody::owner_only(owner_key());
        let client = backfill_client(vec![unsealed, already_stamped, reserved], custody);

        let report = block_on(client.backfill_sealed_fields()).unwrap();
        assert_eq!(
            (report.names, report.selective_sync, report.retention),
            (1, 1, 1)
        );
        assert_eq!(report.bound_skipped, 0);
        assert_eq!(report.update_failures, 0);

        let stamps = updates(&client);
        assert_eq!(stamps.len(), 1, "one stamp for the one unsealed set");
        let req = &stamps[0];
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            req, "docs"
        ));
        // Stamp-only: the request must send back NO plaintext (the seal-only
        // `(None, Some(sealed))` nest arm stamps in place).
        assert_eq!(req.include_paths, None);
        assert_eq!(req.exclude_paths, None);
        assert_eq!(req.retention_policy, None);
        // Name: convergent ⇒ exact bytes by recomputing the derivation.
        let root = fauna_core::path_crypto::LabelRoot::owner_of(&owner_key());
        assert_eq!(
            req.name_sealed.as_ref().map(|b| b.to_vec()),
            fauna_core::label_custody::seal_set_name(&root, "docs").unwrap()
        );
        // Random-nonce fields: seal/open round-trip under the reader custody.
        let keys = fauna_core::file_download::FileDownloadKeys::owner(owner_key());
        assert_eq!(
            fauna_core::label_custody::render_include_paths(
                &keys,
                req.include_paths_sealed.as_deref().map(|b| &b[..]),
                None,
                5,
            ),
            Some(vec!["Documents/2026 taxes".to_string()]),
        );
        assert_eq!(
            fauna_core::label_custody::render_exclude_paths(
                &keys,
                req.exclude_paths_sealed.as_deref().map(|b| &b[..]),
                None,
                5,
            ),
            Some(vec!["Documents/cache".to_string()]),
        );
        assert_eq!(
            fauna_core::label_custody::render_retention_policy(
                &keys,
                req.retention_policy_sealed.as_deref().map(|b| &b[..]),
                None,
                "docs",
                None,
            ),
            Some("{\"keep_last\":3}".to_string()),
        );

        // Idempotence: re-list with the stamped shape ⇒ zero writes.
        let converged = fauna_protocol::folders::FolderSummary {
            id: 5,
            name: "docs".into(),
            include_paths: Some(vec!["Documents/2026 taxes".into()]),
            exclude_paths: Some(vec!["Documents/cache".into()]),
            retention_policy: Some("{\"keep_last\":3}".into()),
            name_sealed: req.name_sealed.clone(),
            include_paths_sealed: req.include_paths_sealed.clone(),
            exclude_paths_sealed: req.exclude_paths_sealed.clone(),
            retention_policy_sealed: req.retention_policy_sealed.clone(),
            ..Default::default()
        };
        let client2 = backfill_client(
            vec![converged],
            fauna_core::label_custody::LabelCustody::owner_only(owner_key()),
        );
        let report2 = block_on(client2.backfill_sealed_fields()).unwrap();
        assert_eq!(report2, SealBackfillReport::default());
        assert!(updates(&client2).is_empty(), "converged ⇒ no writes");
    }

    struct StubResolver {
        set: &'static str,
        /// `None` = bound-but-unresolvable — the cell: the set IS
        /// bound and this custody cannot produce its content keys.
        keys: Option<fauna_core::folder_keys::FolderContentKeys>,
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl fauna_core::folder_keys::FolderKeyResolver for StubResolver {
        async fn resolve(
            &self,
            name_hash: &[u8; 32],
        ) -> anyhow::Result<fauna_core::folder_keys::ResolvedCustody> {
            use fauna_core::folder_keys::{ResolvedCustody, ResolvedFolderKeys};
            Ok(
                if *name_hash == fauna_core::path_crypto::set_name_hash(self.set) {
                    ResolvedCustody::ContentKeyed(ResolvedFolderKeys {
                        mls_group_id: Some(b"raw-group-id".to_vec()),
                        content_keys: self.keys.clone(),
                        home_nest_url: None,
                        home_nest_actor_id: None,
                    })
                } else {
                    ResolvedCustody::owner_only()
                },
            )
        }
    }

    /// A bound row with resolvable keys: the name stamps under the roster's
    /// CONTENT root (exact bytes — convergent), never the owner's, while the
    /// owner-only selective-sync pair still stamps under the owner key.
    #[test]
    fn backfill_seals_a_bound_sets_name_under_the_content_root() {
        let content = fauna_core::folder_keys::FolderContentKeys::genesis([4u8; 32], 1_000);
        let row = fauna_protocol::folders::FolderSummary {
            id: 9,
            name: "shared".into(),
            mls_group_id: Some(hex::encode(b"raw-group-id")),
            include_paths: Some(vec!["Sync/in".into()]),
            ..Default::default()
        };
        let custody = fauna_core::label_custody::LabelCustody::new(
            Some(std::sync::Arc::new(StubResolver {
                set: "shared",
                keys: Some(content.clone()),
            })),
            Some(owner_key()),
        );
        let client = backfill_client(vec![row], custody);

        let report = block_on(client.backfill_sealed_fields()).unwrap();
        assert_eq!((report.names, report.selective_sync), (1, 1));
        assert_eq!(report.bound_skipped, 0);

        let stamps = updates(&client);
        let content_root = fauna_core::path_crypto::LabelRoot::content_key(
            *content.current_key(),
            content.current_version(),
        );
        assert_eq!(
            stamps[0].name_sealed.as_ref().map(|b| b.to_vec()),
            fauna_core::label_custody::seal_set_name(&content_root, "shared").unwrap(),
            "bound set's name seals under the roster's content root"
        );
        let owner_root = fauna_core::path_crypto::LabelRoot::owner_of(&owner_key());
        assert_ne!(
            stamps[0].name_sealed.as_ref().map(|b| b.to_vec()),
            fauna_core::label_custody::seal_set_name(&owner_root, "shared").unwrap(),
            "and NOT under the owner root no roster member could open"
        );
    }

    /// A SCRUBBED bound row (blank plaintext `name`, its `name_hash` beside
    /// it): custody resolves by the hash, so the row's identity matches and
    /// its owner-only selective-sync pair still stamps — while nothing is
    /// sealed with the blank name as plaintext or salt. Keyed by the blank
    /// plaintext, custody read owner-only and the row skipped as an identity
    /// mismatch.
    #[test]
    fn backfill_resolves_a_scrubbed_bound_row_by_its_hash_and_seals_nothing_by_name() {
        let content = fauna_core::folder_keys::FolderContentKeys::genesis([4u8; 32], 1_000);
        let row = fauna_protocol::folders::FolderSummary {
            id: 9,
            name: String::new(),
            name_hash: Some(fauna_protocol::ByteBuf::from(
                fauna_core::path_crypto::set_name_hash("shared").to_vec(),
            )),
            mls_group_id: Some(hex::encode(b"raw-group-id")),
            include_paths: Some(vec!["Sync/in".into()]),
            retention_policy: Some("{\"keep_last\":3}".into()),
            ..Default::default()
        };
        let custody = fauna_core::label_custody::LabelCustody::new(
            Some(std::sync::Arc::new(StubResolver {
                set: "shared",
                keys: Some(content),
            })),
            Some(owner_key()),
        );
        let client = backfill_client(vec![row], custody);

        let report = block_on(client.backfill_sealed_fields()).unwrap();
        assert_eq!(report.identity_mismatch, 0, "the hash finds the bound set");
        assert_eq!(
            (report.names, report.retention, report.selective_sync),
            (0, 0, 1)
        );
        let stamps = updates(&client);
        assert_eq!(stamps.len(), 1);
        assert!(stamps[0].name_sealed.is_none());
        assert!(stamps[0].retention_policy_sealed.is_none());
    }

    /// A bound row whose keys this custody canNOT resolve — the production
    /// shape since: the resolver answers bound-but-unresolvable
    /// (`content_keys: None`), identity matches the row, and the audience-root
    /// fields (name / retention) are skipped **fail-closed** — the
    /// guard — while the owner-only selective-sync pair still stamps (its
    /// audience is the owner regardless of the set's bound-ness).
    #[test]
    fn backfill_skips_a_bound_sets_audience_fields_fail_closed() {
        let row = fauna_protocol::folders::FolderSummary {
            id: 11,
            name: "shared".into(),
            mls_group_id: Some(hex::encode(b"raw-group-id")),
            include_paths: Some(vec!["Sync/in".into()]),
            retention_policy: Some("{\"keep_last\":3}".into()),
            ..Default::default()
        };
        let custody = fauna_core::label_custody::LabelCustody::new(
            Some(std::sync::Arc::new(StubResolver {
                set: "shared",
                keys: None, // bound, but this holder cannot resolve the keys
            })),
            Some(owner_key()),
        );
        let client = backfill_client(vec![row], custody);

        let report = block_on(client.backfill_sealed_fields()).unwrap();
        assert_eq!(report.bound_skipped, 1);
        assert_eq!((report.names, report.retention), (0, 0));
        assert_eq!(report.selective_sync, 1);

        let stamps = updates(&client);
        assert_eq!(stamps.len(), 1);
        assert_eq!(stamps[0].name_sealed, None, "no owner-root name seal");
        assert_eq!(stamps[0].retention_policy_sealed, None);
        assert!(stamps[0].include_paths_sealed.is_some());
    }

    /// The row-identity guard: when the name-keyed custody
    /// resolves to a DIFFERENT identity than the row in hand — here a
    /// resolver-less custody meeting a bound row, the strongest divergence —
    /// the whole row is skipped fail-closed, the owner-only pair included
    /// (identity in doubt ⇒ nothing about the row's audience is trusted).
    #[test]
    fn backfill_skips_everything_on_a_custody_row_identity_mismatch() {
        let row = fauna_protocol::folders::FolderSummary {
            id: 11,
            name: "shared".into(),
            mls_group_id: Some(hex::encode(b"raw-group-id")),
            include_paths: Some(vec!["Sync/in".into()]),
            retention_policy: Some("{\"keep_last\":3}".into()),
            ..Default::default()
        };
        let custody = fauna_core::label_custody::LabelCustody::owner_only(owner_key());
        let client = backfill_client(vec![row], custody);

        let report = block_on(client.backfill_sealed_fields()).unwrap();
        assert_eq!(report.identity_mismatch, 1);
        assert_eq!(report.stamped(), 0);
        assert!(
            updates(&client).is_empty(),
            "an identity-mismatched row sends no update at all"
        );
    }

    /// The cell at the interactive-save site: a bound set whose
    /// keys the resolver cannot produce, with an owner key in hand, refuses to
    /// seal the retention policy — the save records plaintext-only, never an
    /// owner-root seal stamped under the owner root no roster member could
    /// open.
    #[test]
    fn seal_retention_refuses_a_bound_set_with_unresolvable_keys() {
        let custody = fauna_core::label_custody::LabelCustody::new(
            Some(std::sync::Arc::new(StubResolver {
                set: "shared",
                keys: None,
            })),
            Some(owner_key()),
        );
        let client = FoldersClient::new(RecordingRequester::new(reply)).with_label_custody(custody);

        let sealed = block_on(client.seal_retention("shared", "{\"keep_last\":3}"));

        assert_eq!(
            sealed, None,
            "bound + owner key + unresolvable keys must not be stamped under \
             the owner root"
        );

        // Do-not-cheat: the same site with an ordinary unbound owned set (the
        // resolver positively answers unbound) still seals under the owner
        // root.
        let custody = fauna_core::label_custody::LabelCustody::new(
            Some(std::sync::Arc::new(StubResolver {
                set: "someone-elses",
                keys: None,
            })),
            Some(owner_key()),
        );
        let client = FoldersClient::new(RecordingRequester::new(reply)).with_label_custody(custody);
        let sealed = block_on(client.seal_retention("mine", "{\"keep_last\":3}"));
        assert!(
            sealed.is_some(),
            "an unbound owned set still seals its policy under the owner root"
        );
    }

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = FoldersClient::new(MockRequester);
    }

    // ── Wire-contract tests ─────────────────────────────────────────────────
    //
    // The construction-only `MockRequester` above can't catch a wrong kind
    // string or a request that no longer serializes to the shape the nest
    // handler decodes. These tests pin both: each `FoldersClient` method must
    // send its exact `fauna.folders.*` kind and a payload that round-trips
    // back to the typed request. No nest-side conformance test routes through
    // this adapter's literal kind strings, so an adapter-method kind rename
    // would otherwise break the Devices/Peers folder CRUD silently. The
    // pattern mirrors the `RecordingRequester` in `fauna-client-events` /
    // `-snapshots` / `-sync` (transport-free, so it runs on every target
    // including wasm); real end-to-end round-trip conformance lives in
    // `tests/e2e-unified/tests/test_folders.py` (real router dispatch, tier_3).

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        // Answer with a reply the requested `Reply` type decodes — one arm
        // per kind, each the minimal valid shape.
        match kind {
            "fauna.folders.list" => fauna_protocol::encode_canonical(&FoldersListReply {
                folders: vec![],
                extra: Default::default(),
            }),
            "fauna.folders.create" => fauna_protocol::encode_canonical(&FolderCreateReply {
                id: 1,
                name: "photos".into(),
                retention_policy: None,
                extra: Default::default(),
            }),
            // Struct-update rather than a hand-listed field set: this is a
            // growing wire type with a `Default` impl, and hand-listing is what
            // let `flags` (folders phase 2 slice b) land with this fixture
            // uncompiled. Prefer `..Default::default()` for any wire type that
            // is still gaining fields — two branches growing the same struct
            // then merge cleanly instead of colliding on the grown axis.
            "fauna.folders.members.list" => fauna_protocol::encode_canonical(&MembersListReply {
                members: vec![],
                extra: Default::default(),
            }),
            "fauna.folders.members.list_actors" | "fauna.folders.members.list_actors_remote" => {
                fauna_protocol::encode_canonical(&ActorMembersListReply {
                    members: vec![],
                    ..Default::default()
                })
            }
            "fauna.folders.share" => fauna_protocol::encode_canonical(&FolderShareReply {
                ok: true,
                folder: "photos".into(),
                channel_id: "cd".repeat(32),
                ..Default::default()
            }),
            "fauna.folders.update" => fauna_protocol::encode_canonical(&FolderUpdateReply {
                ok: true,
                extra: Default::default(),
            }),
            "fauna.folders.delete" => fauna_protocol::encode_canonical(&FolderDeleteReply {
                ok: true,
                extra: Default::default(),
            }),
            "fauna.folders.devices" => fauna_protocol::encode_canonical(&FolderDevicesReply {
                devices: vec![],
                extra: Default::default(),
            }),
            "fauna.folders.members.remove" => {
                fauna_protocol::encode_canonical(&MemberRemoveReply {
                    ok: true,
                    extra: Default::default(),
                })
            }
            "fauna.folders.leave" => fauna_protocol::encode_canonical(&MemberLeaveReply {
                ok: true,
                channel_id: "cd".repeat(32),
                left: true,
                extra: Default::default(),
            }),
            "fauna.folders.lease.acquire" => fauna_protocol::encode_canonical(&LeaseAcquireReply {
                acquired: true,
                extra: Default::default(),
            }),
            "fauna.folders.lease.release" => fauna_protocol::encode_canonical(&LeaseReleaseReply {
                released: true,
                extra: Default::default(),
            }),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    fn client() -> (
        std::sync::Arc<RecordingRequester>,
        FoldersClient<std::sync::Arc<RecordingRequester>>,
    ) {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = FoldersClient::new(rec.clone());
        (rec, client)
    }

    #[test]
    fn list_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.list()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.folders.list");
        let req: FoldersListRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(
            req.include_shared_with_me, None,
            "the owner-scoped list does not opt into member visibility"
        );
    }

    #[test]
    fn list_owned_and_shared_sets_the_flag() {
        let (rec, c) = client();
        block_on(c.list_owned_and_shared()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.folders.list");
        let req: FoldersListRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(
            req.include_shared_with_me,
            Some(true),
            "the member-visible projection opts in via include_shared_with_me"
        );
    }

    #[test]
    fn create_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.create(FolderCreateRequest {
            name: "photos".into(),
            retention_policy: Some("{}".into()),
            // Growing wire type: struct-update fixture (survives concurrent field-adds).
            ..Default::default()
        }))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.folders.create");
        let req: FolderCreateRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.name, "photos");
        assert_eq!(req.retention_policy.as_deref(), Some("{}"));
    }

    #[test]
    fn share_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.share(FolderShareRequest {
            name: "photos".into(),
            group_id: "ab".repeat(8),
            ..Default::default()
        }))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.folders.share");
        let req: FolderShareRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &req, "photos"
        ));
        assert_eq!(req.group_id, "ab".repeat(8));
    }

    /// Every by-name request leaves with its hash address beside the name
    /// (the S5b hash-sender batch); a reserved `__` set leaves by name alone.
    #[test]
    fn by_name_requests_carry_the_set_name_hash() {
        let want = fauna_core::path_crypto::set_name_hash("photos").to_vec();
        let (rec, c) = client();
        block_on(c.members_list("photos")).expect("infallible mock");
        let (_, payload) = rec.recorded();
        let req: MembersListRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.name_hash.as_deref(), Some(&want));

        let (rec, c) = client();
        block_on(c.lease_release("photos", "ab".repeat(16))).expect("infallible mock");
        let (_, payload) = rec.recorded();
        let req: LeaseReleaseRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.name_hash.as_deref(), Some(&want));

        let (rec, c) = client();
        block_on(c.devices("__backup")).expect("infallible mock");
        let (_, payload) = rec.recorded();
        let req: FolderDevicesRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.name_hash, None, "a reserved set is addressed by name");
    }

    #[test]
    fn members_list_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.members_list("photos")).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.folders.members.list");
        let req: MembersListRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &req, "photos"
        ));
    }

    #[test]
    fn actor_members_list_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.actor_members_list("photos")).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.folders.members.list_actors");
        let req: ActorMembersListRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &req, "photos"
        ));
    }

    #[test]
    fn actor_members_list_remote_composes_kind_and_payload() {
        let (rec, c) = client();
        let channel = "cd".repeat(32);
        block_on(c.actor_members_list_remote(channel.clone(), "https://home.example"))
            .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.folders.members.list_actors_remote");
        let req: ActorMembersListRemoteRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.channel_id, channel);
        assert_eq!(req.nest_url, "https://home.example");
    }

    #[test]
    fn leave_composes_kind_and_payload() {
        let (rec, c) = client();
        let reply = block_on(c.leave("ab".repeat(16))).expect("infallible mock");
        assert!(reply.ok && reply.left);
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.folders.leave");
        // Addressed by the raw group id (not a set name).
        let req: MemberLeaveRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.group_id, "ab".repeat(16));
    }

    #[test]
    fn update_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.update(FolderUpdateRequest {
            name: "photos".into(),
            retention_policy: None,
            include_paths: Some(vec!["/home/me/pics".into()]),
            exclude_paths: Some(vec!["/home/me/pics/tmp".into()]),
            webdav_enabled: None,
            ..Default::default()
        }))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.folders.update");
        let req: FolderUpdateRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &req, "photos"
        ));
        assert_eq!(
            req.exclude_paths.as_deref(),
            Some(&["/home/me/pics/tmp".into()][..])
        );
    }

    #[test]
    fn delete_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.delete("photos")).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.folders.delete");
        let req: FolderDeleteRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &req, "photos"
        ));
    }

    #[test]
    fn devices_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.devices("photos")).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.folders.devices");
        let req: FolderDevicesRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &req, "photos"
        ));
    }

    #[test]
    fn members_remove_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.members_remove(MemberRemoveRequest {
            name: "photos".into(),
            device_id: "ab".repeat(32),
            ..Default::default()
        }))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.folders.members.remove");
        let req: MemberRemoveRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &req, "photos"
        ));
        assert_eq!(req.device_id, "ab".repeat(32));
    }

    #[test]
    fn lease_acquire_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.lease_acquire(LeaseAcquireRequest {
            name: "photos".into(),
            device_id: "cd".repeat(32),
            ..Default::default()
        }))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.folders.lease.acquire");
        let req: LeaseAcquireRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &req, "photos"
        ));
        assert_eq!(req.device_id, "cd".repeat(32));
    }

    #[test]
    fn lease_release_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.lease_release("photos", "ab".repeat(32))).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.folders.lease.release");
        let req: LeaseReleaseRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &req, "photos"
        ));
        assert_eq!(
            req.device_id,
            "ab".repeat(32),
            "the release names its device"
        );
    }
}
