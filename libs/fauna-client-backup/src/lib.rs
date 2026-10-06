//! Typed-call wrapper for the `fauna.backup.*` WS-RPC kinds — the nest-side
//! segment-backup plane clients speak to their **source** nest (the one that
//! runs the in-process backup coordinator) and to each **destination** nest
//! (the one that holds the custody).
//!
//! Two call surfaces live here because they are the same kind family, but they
//! are spoken to *different* nests and the split is load-bearing:
//!
//! - **Source-nest kinds** — [`BackupClient::status`] (the uniform 7-client
//!   Backups-page projection), [`BackupClient::nest_key_grant`] /
//!   [`BackupClient::nest_key_revoke`] (the `NestBackupKey` the nest seals this
//!   owner's segments with), and [`BackupClient::destination_register`] /
//!   [`BackupClient::destination_remove`] / [`BackupClient::destination_list`]
//!   (the nest-readable twin of the client-sealed `fauna.state.backup`
//!   destination list, which the nest cannot read).
//! - **Destination-nest kinds** — [`BackupClient::writer_grant_register`] /
//!   [`BackupClient::writer_grant_revoke`] / [`BackupClient::writer_grant_list`],
//!   spoken over the destination's *own* authenticated connection rather than
//!   the federation channel, which is what keeps revocation operable with the
//!   source nest fully hostile (`docs/goal/architecture/message-segment-store.md`
//!   § Cross-location backup protocol).
//!
//! The type does not enforce which nest a transport points at — the caller
//! holds that context (`enroll_backup_destination` takes both and names them
//! `nest` / `destination`). Each method's doc says which nest it belongs to.
//!
//! Pattern: same shape as `fauna-client-snapshots` / `fauna-client-bridges` — a
//! thin `BackupClient<R: RpcRequester>`, one async method per kind, no state
//! machine, wasm-clean (no `fauna-client` dependency), so linux, the native FFI
//! and the wasm SPA all ride one copy of the kind names and payload shapes
//! (priority #2). Before this crate the kind-name constants lived privately in
//! `fauna_client_config::backup_enroll`, reachable only by the enroll sequence;
//! the Backups-page status repoint needed a second consumer, which is what
//! turned them into a shared surface.

pub mod audit;
pub mod audit_clock;
mod cursor;
pub mod custodian;
pub mod generations;
#[cfg(not(target_arch = "wasm32"))]
pub mod native_store;
pub mod reseed;
#[cfg(feature = "local-clock")]
pub mod row_text;
pub mod seat_carry;
pub mod trust;

use fauna_protocol::RpcRequester;
use fauna_protocol::backup::{
    AttachFolderReply, AttachFolderRequest, BackupStatusReply, BackupStatusRequest,
    CustodianCheckinReply, CustodianCheckinRequest, CustodyListReply, CustodyListRequest,
    CustodyMaterializeReply, CustodyMaterializeRequest, DestinationListReply,
    DestinationListRequest, DestinationRegisterReply, DestinationRegisterRequest,
    DestinationRemoveReply, DestinationRemoveRequest, DetachFolderReply, DetachFolderRequest,
    GenerationListReply, GenerationListRequest, GenerationRestoreReply, GenerationRestoreRequest,
    NestKeyGrantReply, NestKeyGrantRequest, NestKeyRevokeReply, NestKeyRevokeRequest,
    WriterGrantListReply, WriterGrantListRequest, WriterGrantRegisterReply,
    WriterGrantRegisterRequest, WriterGrantRevokeReply, WriterGrantRevokeRequest,
};
use fauna_protocol::segments::{
    KIND_SEGMENTS_COUNTER_FLOOR, KIND_SEGMENTS_LIST, SegmentsCounterFloorReply,
    SegmentsCounterFloorRequest, SegmentsListReply, SegmentsListRequest,
};

pub use fauna_protocol::backup;

/// `fauna.backup.nest_key.grant` — grant this owner's `NestBackupKey` to the
/// source nest.
pub const KIND_NEST_KEY_GRANT: &str = "fauna.backup.nest_key.grant";

/// `fauna.backup.nest_key.revoke` — withdraw the grant, freezing the source
/// nest's ability to seal new segments for this owner.
pub const KIND_NEST_KEY_REVOKE: &str = "fauna.backup.nest_key.revoke";

/// `fauna.backup.status` — the source nest's per-destination backup projection.
pub const KIND_STATUS: &str = "fauna.backup.status";

/// `fauna.backup.destination.register` on the **source** nest — tell it where
/// to back this owner up (the nest-readable twin of the `fauna.state.backup` row).
pub const KIND_DESTINATION_REGISTER: &str = "fauna.backup.destination.register";

/// `fauna.backup.destination.remove` on the **source** nest — the deregister
/// twin of [`KIND_DESTINATION_REGISTER`].
pub const KIND_DESTINATION_REMOVE: &str = "fauna.backup.destination.remove";

/// `fauna.backup.custodian.checkin` on the **source** nest — a client-device
/// custodian acking its pull progress so the nest can project its status row
/// (`docs/goal/behavior/backup-destinations.md` § Third destination kind).
pub const KIND_CUSTODIAN_CHECKIN: &str = "fauna.backup.custodian.checkin";

/// `fauna.backup.destination.list` on the **source** nest — read back the
/// registry, the read-side twin of register/remove.
pub const KIND_DESTINATION_LIST: &str = "fauna.backup.destination.list";

/// `fauna.backup.destination.attach_folder` on the **source** nest — give one
/// of this owner's ordinary folders a destination place on a registered
/// destination (`docs/goal/behavior/backup-destinations.md` § Ordinary-folder
/// coverage).
pub const KIND_DESTINATION_ATTACH_FOLDER: &str = "fauna.backup.destination.attach_folder";

/// `fauna.backup.destination.detach_folder` on the **source** nest — the
/// detach twin of [`KIND_DESTINATION_ATTACH_FOLDER`].
pub const KIND_DESTINATION_DETACH_FOLDER: &str = "fauna.backup.destination.detach_folder";

/// `fauna.backup.writer_grant.register` on the **destination** — authorize a
/// source nest to write this owner's segment-backup custody there.
pub const KIND_WRITER_GRANT_REGISTER: &str = "fauna.backup.writer_grant.register";

/// `fauna.backup.writer_grant.revoke` on the **destination** — withdraw a
/// writer's authorization.
pub const KIND_WRITER_GRANT_REVOKE: &str = "fauna.backup.writer_grant.revoke";

/// `fauna.backup.writer_grant.list` on the **destination** — read the writers
/// this owner has authorized (the trust-facet read).
pub const KIND_WRITER_GRANT_LIST: &str = "fauna.backup.writer_grant.list";

/// `fauna.backup.custody.list` on the **destination** — the live custody it
/// holds for this owner (the audit loop's source-untrusted read).
pub const KIND_CUSTODY_LIST: &str = "fauna.backup.custody.list";

/// `fauna.backup.generation.list` on the **destination** — the superseded
/// custody generations it is still retaining inside the grace window `T`.
pub const KIND_GENERATION_LIST: &str = "fauna.backup.generation.list";

/// `fauna.backup.generation.restore` on the **destination** — promote one
/// retained generation back to live for its path.
pub const KIND_GENERATION_RESTORE: &str = "fauna.backup.generation.restore";

/// `fauna.backup.custody.materialize` on the **nest being seeded** — flip one
/// custody set it holds from destination posture to live source posture.
pub const KIND_CUSTODY_MATERIALIZE: &str = "fauna.backup.custody.materialize";

/// Typed `fauna.backup.*` call surface, generic over the WS-RPC transport
/// (`R: RpcRequester`): native call sites pass `Arc<NestClient>`, the wasm SPA
/// passes its `WsRpcClient`. Errors propagate as the transport's `R::Error`.
pub struct BackupClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> BackupClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// Borrow the underlying transport — lets a caller that already built a
    /// `BackupClient` reuse the same connection for a non-backup kind rather
    /// than opening a second one (the enroll sequence's `fauna.nest.info`
    /// read).
    pub fn transport(&self) -> &R {
        &self.nest
    }

    /// Give the transport back. `R` is not required to be `Clone` (a native
    /// `Arc<NestClient>` is, a wasm `WsRpcClient` need not be), so a caller
    /// that must hand the *same* connection to another typed client that
    /// takes it by value unwraps rather than
    /// reconnecting.
    pub fn into_inner(self) -> R {
        self.nest
    }

    // ── source-nest kinds ────────────────────────────────────────────────────

    /// `fauna.backup.status` (**source** nest) — the uniform Backups-page
    /// projection: an `enrolled` flag plus one row per registered destination
    /// (`last_upload_time`, `backlog_count`), computed by the nest's in-process
    /// coordinator from the state its own passes advance
    /// (`docs/goal/behavior/backup-destinations.md` § Per-destination status read).
    ///
    /// This is the **replacement** for the pre-flip source-side FFI computation
    /// (the deleted client coordinator's `destination_status()`): it is live with every
    /// app asleep, and it is the only status source web and mobile can have,
    /// since neither runs a local coordinator.
    ///
    /// `enrolled == false` means precisely "this actor has granted no
    /// `NestBackupKey`" — the nest has no coordinator for them, so the
    /// destination list is empty regardless of what the client's own config
    /// holds. A client whose config *does* hold destinations in that state is
    /// pre-enrollment and self-heals by re-issuing the grant + registrations
    /// (`fauna_client_config::reconcile_backup_enrollment`). Replay-safe pure
    /// read.
    pub async fn status(&self) -> Result<BackupStatusReply, R::Error> {
        self.nest
            .request(KIND_STATUS, BackupStatusRequest::default())
            .await
    }

    /// `fauna.backup.nest_key.grant` (**source** nest) — hand the nest this
    /// owner's `NestBackupKey` so its coordinator can seal this owner's
    /// segments. Idempotent (a re-grant replaces) and inert on its own: a
    /// stored key with zero registered destinations backs nothing up.
    ///
    /// `nest_backup_key` must be exactly 32 bytes; the nest rejects any other
    /// length as malformed.
    pub async fn nest_key_grant(
        &self,
        nest_backup_key: Vec<u8>,
    ) -> Result<NestKeyGrantReply, R::Error> {
        self.nest
            .request(
                KIND_NEST_KEY_GRANT,
                NestKeyGrantRequest {
                    nest_backup_key: nest_backup_key.into(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.backup.nest_key.revoke` (**source** nest) — withdraw the grant.
    /// The reply's `revoked` is `false` when there was nothing stored, so the
    /// call is idempotent rather than an error on a double-revoke.
    pub async fn nest_key_revoke(&self) -> Result<NestKeyRevokeReply, R::Error> {
        self.nest
            .request(KIND_NEST_KEY_REVOKE, NestKeyRevokeRequest::default())
            .await
    }

    /// `fauna.backup.destination.register` (**source** nest) — record where to
    /// back this owner up. The nest holds the destination list only as client-sealed
    /// ciphertext, so without this call a destination the client records in
    /// its own state is invisible to the in-process coordinator. Idempotent on
    /// `destination_id`.
    ///
    /// `destination_nest_id` is the destination's 32-byte identity, hex-encoded
    /// — the nest refuses anything else rather than storing a row that can
    /// never match a real `nest_id`.
    pub async fn destination_register(
        &self,
        destination_id: String,
        destination_nest_url: String,
        destination_nest_id: String,
    ) -> Result<DestinationRegisterReply, R::Error> {
        self.nest
            .request(
                KIND_DESTINATION_REGISTER,
                DestinationRegisterRequest {
                    destination_id,
                    destination_nest_url,
                    destination_nest_id,
                    kind: fauna_core::data::DESTINATION_KIND_NEST.to_string(),
                    // Struct-update so a future per-kind field lands here as its
                    // default rather than as a merge conflict — the standing
                    // convention for a wire type that carries a `Default`.
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.backup.destination.register` (**source** nest) for a **client
    /// device custodian** — the third destination kind
    /// (`docs/goal/behavior/backup-destinations.md` § Third destination kind).
    ///
    /// Step 2 of the custodian's three-step enrollment, and the only nest
    /// mutation in it: there is no destination server, so none of the peer-nest
    /// sequence's `fauna.nest.info` read, `NestBackupKey` grant or
    /// destination-side writer grant applies. Idempotent on `destination_id`
    /// like its nest sibling, and inert until the config write that follows it —
    /// so a crash mid-enrollment leaves only this row, and the local store is
    /// derived state a fresh pull rebuilds.
    ///
    /// A custodian has **no address**, which is why this is a separate verb
    /// rather than more arguments: passing a URL and a nest id here would be
    /// meaningless, and the nest must not dial a row registered this way.
    ///
    /// `capacity_cap_bytes` rides the registration because the registry is the
    /// only copy the custodian's *host* can read on desktop: the pull is hosted
    /// by the bearer-only sync agent, which cannot open the client-sealed
    /// destination row (`docs/goal/architecture/apps/sync-agent.md`
    /// § Credential model), and reads its assignment back through
    /// [`Self::destination_list`] instead (§ Control plane split — *policy
    /// through the nest, never over local IPC*). `None` = uncapped.
    pub async fn destination_register_custodian(
        &self,
        destination_id: String,
        custodian_device_id: String,
        capacity_cap_bytes: Option<u64>,
    ) -> Result<DestinationRegisterReply, R::Error> {
        self.nest
            .request(
                KIND_DESTINATION_REGISTER,
                DestinationRegisterRequest {
                    destination_id,
                    kind: fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE.to_string(),
                    custodian_device_id: Some(custodian_device_id),
                    capacity_cap_bytes,
                    ..Default::default()
                },
            )
            .await
    }

    /// This device's own custodian assignment, read back from the source nest's
    /// destination registry — **every** custodian host's discovery path: the
    /// desktop sync agent's, and the phones' in-process host
    /// (`fauna-ffi`'s `build_custodian_host`), through the shared
    /// [`fauna_core::data::custodian_assignment_for`] rule. The registry, not the
    /// at-rest config, so a cap the owner raises on another device reaches this
    /// one (`docs/goal/architecture/apps/sync-agent.md` § Control plane split).
    /// `Ok(None)` means this device is not enrolled as a custodian — the
    /// ordinary state on most devices, not an error.
    ///
    /// It is also what keeps a held copy safe after a box loss: the rebuilt
    /// nest's registry is empty, so no host is built and nothing pulls from it
    /// (a pull reads a path its source no longer lists as deleted) until the
    /// owner's re-seed has put the corpus back and re-enrolled this device.
    pub async fn custodian_assignment(
        &self,
        this_device_id: &str,
    ) -> Result<Option<fauna_core::data::CustodianAssignment>, R::Error> {
        let reply = self.destination_list().await?;
        Ok(fauna_core::data::custodian_assignment_for(
            reply.destinations.iter().map(|d| d.custodian_row()),
            this_device_id,
        ))
    }

    /// `fauna.backup.custodian.checkin` (**source** nest) — a custodian device
    /// acking how far it has pulled, so the nest can project this destination's
    /// status row for the owner's other devices
    /// (`docs/goal/architecture/message-segment-store.md` § Client-device
    /// custodian → *Check-in*).
    ///
    /// Written after each pull pass on the device's own connection. `cap_state`
    /// is [`fauna_protocol::backup::CAP_STATE_OK`] or
    /// [`fauna_protocol::backup::CAP_STATE_REACHED`] — a custodian that has
    /// stopped pulling at its cap must not read as an ordinary lag.
    // The parameters ARE the wire request's fields — one call surface per
    // `CustodianCheckinRequest` field, mirroring every other typed call in this
    // crate. Grouping them into a local struct would put a second shape beside
    // the wire type it already mirrors, so the arity rides an allow (the
    // codebase's standing idiom for wire-shaped call surfaces) rather than a
    // parallel type that can drift from the protocol.
    #[allow(clippy::too_many_arguments)]
    pub async fn custodian_checkin(
        &self,
        destination_id: String,
        high_water: u64,
        held_bytes: u64,
        cap_state: String,
        audit_state: Option<String>,
        last_audit_passed_at: Option<u64>,
        device_id: Option<String>,
    ) -> Result<CustodianCheckinReply, R::Error> {
        self.nest
            .request(
                KIND_CUSTODIAN_CHECKIN,
                CustodianCheckinRequest {
                    destination_id,
                    high_water,
                    held_bytes,
                    cap_state,
                    audit_state,
                    last_audit_passed_at,
                    device_id,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.backup.destination.remove` (**source** nest) — drop one registry
    /// row. Idempotent; removing an unknown `destination_id` is not an error.
    ///
    /// This does **not** revoke the destination-side writer grant — that is a
    /// distinct trust-facet action, not implied by removing a destination from
    /// this owner's list.
    pub async fn destination_remove(
        &self,
        destination_id: String,
    ) -> Result<DestinationRemoveReply, R::Error> {
        self.nest
            .request(
                KIND_DESTINATION_REMOVE,
                DestinationRemoveRequest {
                    destination_id,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.backup.destination.list` (**source** nest) — read back this
    /// owner's registry rows. Replay-safe pure read.
    pub async fn destination_list(&self) -> Result<DestinationListReply, R::Error> {
        self.nest
            .request(KIND_DESTINATION_LIST, DestinationListRequest::default())
            .await
    }

    /// `fauna.backup.destination.attach_folder` (**source** nest) — attach one
    /// of this owner's ordinary folders to a registered destination. Idempotent
    /// on `(destination_id, folder_id)`; the reply's `folder_set` is the
    /// canonical `__folder/<source-nest-hex>/<folder-id>` name the caller
    /// records verbatim as the config row's `folder_name` — the naming rule
    /// lives on the nest, never re-derived client-side.
    pub async fn destination_attach_folder(
        &self,
        destination_id: String,
        folder_id: i64,
    ) -> Result<AttachFolderReply, R::Error> {
        self.nest
            .request(
                KIND_DESTINATION_ATTACH_FOLDER,
                AttachFolderRequest {
                    destination_id,
                    folder_id,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.backup.destination.detach_folder` (**source** nest) — remove a
    /// folder's destination place. Idempotent; the destination-side teardown is
    /// the coordinator's, on its next pass.
    pub async fn destination_detach_folder(
        &self,
        destination_id: String,
        folder_id: i64,
    ) -> Result<DetachFolderReply, R::Error> {
        self.nest
            .request(
                KIND_DESTINATION_DETACH_FOLDER,
                DetachFolderRequest {
                    destination_id,
                    folder_id,
                    extra: Default::default(),
                },
            )
            .await
    }

    // ── destination-nest kinds ───────────────────────────────────────────────

    /// `fauna.backup.writer_grant.register` (**destination** nest) — authorize
    /// `writer_nest_id` to write this owner's segment-backup custody here.
    /// Idempotent (re-registering the same writer refreshes rather than
    /// duplicating) and inert alone: an authorized-but-unconfigured writer
    /// backs nothing up.
    ///
    /// Spoken over the destination's own authenticated connection, never the
    /// federation channel — that separation is what keeps revocation operable
    /// with the source nest fully hostile.
    pub async fn writer_grant_register(
        &self,
        writer_nest_id: String,
    ) -> Result<WriterGrantRegisterReply, R::Error> {
        self.nest
            .request(
                KIND_WRITER_GRANT_REGISTER,
                WriterGrantRegisterRequest {
                    writer_nest_id,
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.backup.writer_grant.register { succeeds }` (**destination**
    /// nest) — hand the seat `succeeds` holds to `writer_nest_id`, the
    /// rotated box's carry (`segment-backup-protocol.md` § Cross-location
    /// backup protocol → *The writer seat*). A separate method rather than an
    /// optional field on [`Self::writer_grant_register`] so the carry cannot
    /// issue a registration that names no predecessor: without `succeeds` the
    /// successor is a second box.
    ///
    /// Spoken over the destination's own authenticated connection, like every
    /// writer-grant kind; the caller has verified the rotation chain linking
    /// `succeeds` to `writer_nest_id` first ([`crate::seat_carry`]).
    pub async fn writer_grant_succeed(
        &self,
        writer_nest_id: String,
        succeeds: String,
    ) -> Result<WriterGrantRegisterReply, R::Error> {
        self.nest
            .request(
                KIND_WRITER_GRANT_REGISTER,
                WriterGrantRegisterRequest {
                    writer_nest_id,
                    succeeds: Some(succeeds),
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.auth.rotation_chain` (**source** nest) — the box's full rotation
    /// chain, oldest hop first; empty when it never rotated. Read by the seat
    /// carry, which verifies it ([`fauna_protocol::nest_rotation::verify_chain`])
    /// before naming a predecessor.
    pub async fn rotation_chain(
        &self,
    ) -> Result<fauna_protocol::nest_rotation::RotationChainReply, R::Error> {
        self.nest
            .request(
                fauna_protocol::nest_rotation::ROTATION_CHAIN_KIND,
                fauna_protocol::nest_rotation::RotationChainRequest::default(),
            )
            .await
    }

    /// `fauna.backup.writer_grant.revoke` (**destination** nest) — withdraw a
    /// writer's authorization, freezing its writes to this owner's custody.
    /// The reply's `revoked` is `false` when there was no such grant.
    pub async fn writer_grant_revoke(
        &self,
        writer_nest_id: String,
    ) -> Result<WriterGrantRevokeReply, R::Error> {
        self.nest
            .request(
                KIND_WRITER_GRANT_REVOKE,
                WriterGrantRevokeRequest {
                    writer_nest_id,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.backup.writer_grant.list` (**destination** nest) — the writers
    /// this owner has authorized here. Backs the Nests-page trust-facet rows.
    /// Replay-safe pure read.
    pub async fn writer_grant_list(&self) -> Result<WriterGrantListReply, R::Error> {
        self.nest
            .request(KIND_WRITER_GRANT_LIST, WriterGrantListRequest::default())
            .await
    }

    /// `fauna.backup.custody.list` (**destination** nest) — the **live**
    /// latest-per-path custody this destination is holding for the owner, each
    /// row carrying the destination's own `updated_at` receipt clock.
    ///
    /// This is the audit loop's central observable, and the reason it is a
    /// *destination* kind is the whole point of auditing: `status()` above is
    /// the **source** nest reporting on its own uploads, and the source is the
    /// custody writer. Only the party holding the bytes can say what is
    /// actually there. Replay-safe pure read.
    ///
    /// Tombstoned paths are excluded nest-side — a compacted-out path is the
    /// absence of custody, so a destination that has dropped everything returns
    /// an empty list rather than reading as healthy.
    /// `cursor` resumes past a previous reply's `next_cursor` (`None` = first
    /// page); walk to an **absent** `next_cursor` — [`crate::audit`]'s
    /// `read_full_custody` owns that loop, so callers rarely page by hand.
    pub async fn custody_list(&self, cursor: Option<String>) -> Result<CustodyListReply, R::Error> {
        self.nest
            .request(
                KIND_CUSTODY_LIST,
                CustodyListRequest {
                    cursor,
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.backup.generation.list` (**destination** nest) — the superseded
    /// custody generations this destination is still retaining for the owner
    /// inside the grace window `T`, newest supersede first, plus `grace_secs`
    /// (the window itself, so no client hard-codes a constant the nest owns).
    ///
    /// The recovery counterpart to [`Self::custody_list`]: that one reports what
    /// is live, this one reports what can still be rolled *back* to. It is a
    /// destination kind for the same reason the whole plane is — the source nest
    /// is the custody writer, so it is exactly the party a rogue-source recovery
    /// must route around (`message-segment-store.md` § Custody grace window (T)).
    /// Owner-scoped by the authenticated actor. Replay-safe pure read.
    /// `cursor` resumes past a previous reply's `next_cursor` (`None` = first
    /// page); walk to an **absent** `next_cursor` —
    /// [`crate::generations::list_retained_generations`] owns that loop, so
    /// callers rarely page by hand.
    pub async fn generation_list(
        &self,
        cursor: Option<String>,
    ) -> Result<GenerationListReply, R::Error> {
        self.nest
            .request(
                KIND_GENERATION_LIST,
                GenerationListRequest {
                    cursor,
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.backup.generation.restore` (**destination** nest) — promote one
    /// retained generation back to live for its path, addressed by
    /// `(folder_name, path_hash, manifest_hash)` exactly as
    /// [`Self::generation_list`] reports it.
    ///
    /// Non-destructive by construction: the live generation it displaces is
    /// retained by the same machinery a supersede uses, so a mistaken restore is
    /// itself undoable within `T`.
    ///
    /// A `restored: false` reply is **not** an error — it means no such retained
    /// generation exists for this owner (an unknown manifest, or one already
    /// reclaimed past `T`). Callers should prefer
    /// [`generations::restore_generation`](crate::generations::restore_generation),
    /// which lifts that distinction into a typed outcome.
    /// `fauna.segments.list` (**source** nest) — the source's saved segment
    /// counter for one `(serve tag, scope)`, the generation its backup ledger
    /// carries ([`fauna_protocol::segments::SegmentsListReply::next_segment_id`]).
    ///
    /// The audit's one read of the *source*: asked only to settle an observed
    /// ledger regression at a destination — which party went backwards
    /// (`audit::SourceLedgerVouch`). The listed segments themselves are
    /// discarded here; the custodian pull is the reader of those.
    pub async fn segments_next_id(
        &self,
        kind_tag: String,
        scope_hex: String,
    ) -> Result<u32, R::Error> {
        let reply: SegmentsListReply = self
            .nest
            .request(
                KIND_SEGMENTS_LIST,
                SegmentsListRequest {
                    kind: kind_tag,
                    actor_id: scope_hex,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.next_segment_id)
    }

    /// `fauna.segments.counter_floor` (**source** nest) — raise the source's
    /// saved segment counter for one `(serve tag, scope)` to at least `floor`,
    /// answering the counter it then stands at. Monotonic and idempotent on
    /// the nest, so a repeat is harmless.
    ///
    /// The audit's one write to the *source*: made when a pass accepts a
    /// source regression, with the generation this device had pinned
    /// (`audit::SourceLedgerVouch::floor_counter`).
    pub async fn segments_counter_floor(
        &self,
        kind_tag: String,
        scope_hex: String,
        floor: u32,
    ) -> Result<u32, R::Error> {
        let reply: SegmentsCounterFloorReply = self
            .nest
            .request(
                KIND_SEGMENTS_COUNTER_FLOOR,
                SegmentsCounterFloorRequest {
                    kind: kind_tag,
                    actor_id: scope_hex,
                    floor,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.next_segment_id)
    }

    pub async fn generation_restore(
        &self,
        folder_name: String,
        path_hash: String,
        manifest_hash: String,
    ) -> Result<GenerationRestoreReply, R::Error> {
        self.nest
            .request(
                KIND_GENERATION_RESTORE,
                GenerationRestoreRequest {
                    folder_name,
                    path_hash,
                    manifest_hash,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.backup.custody.materialize` (the nest being **seeded**) — phase 3
    /// of the re-seed ceremony: flip one custody set this nest holds from
    /// destination posture (opaque sealed chunks it cannot read) to live source
    /// posture (an account it serves).
    ///
    /// Spoken to the TARGET, not to whoever delivered the bytes, and that is the
    /// ceremony's whole shape: after phase 2 the target holds an ordinary backup
    /// destination's corpus, and the party holding the bytes is the only one that
    /// can reconstitute them. It works identically whether the corpus arrived
    /// from a surviving source nest's own coordinator or from this device
    /// re-sealing its custodian store, because delivered custody is byte-identical
    /// either way (`backup-destinations.md` § Third destination kind -> *Re-seed*).
    ///
    /// **Not replay-safe as a write, but safe to retry**: the empty-target rule
    /// makes every repeat a `fauna.backup.target_not_empty` refusal before a byte
    /// is read, so a reconnect-retry can neither double-write nor silently
    /// succeed twice. Callers should surface that code as "this nest already
    /// holds an account for you", not as a failure to retry harder.
    ///
    /// `folder_display_name` names a covered folder for the folder-mirror arm —
    /// custody deliberately never carried it, so the driving device supplies it,
    /// with one page of the owner's re-home signatures (`signer_key` +
    /// `signatures`, `writer-signed-change-records.md` ruling (7)(a)). Leave all
    /// three unset for a segment set (`__mail`).
    pub async fn custody_materialize(
        &self,
        req: CustodyMaterializeRequest,
    ) -> Result<CustodyMaterializeReply, R::Error> {
        self.nest.request(KIND_CUSTODY_MATERIALIZE, req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::block_on;
    use fauna_protocol::backup::BackupDestinationStatusItem;
    use std::sync::Mutex;

    /// Records the kinds requested and replays a canned reply, so each test
    /// asserts the kind string and payload shape without a nest. Encodes
    /// through the real dag-cbor pipeline (`encode_canonical`/`decode_strict`)
    /// rather than a JSON stand-in, so a wire-shape mismatch fails here.
    #[derive(Default)]
    struct MockNest {
        kinds: Mutex<Vec<&'static str>>,
        payloads: Mutex<Vec<Vec<u8>>>,
        reply: Option<Vec<u8>>,
    }

    impl MockNest {
        fn with_reply<T: serde::Serialize>(reply: &T) -> Self {
            Self {
                kinds: Mutex::new(Vec::new()),
                payloads: Mutex::new(Vec::new()),
                reply: Some(
                    fauna_protocol::encode_canonical(reply)
                        .expect("encode reply")
                        .to_vec(),
                ),
            }
        }

        fn kinds(&self) -> Vec<&'static str> {
            self.kinds.lock().unwrap().clone()
        }

        /// Decode the request the client actually put on the wire.
        ///
        /// Kind-only assertions cannot see a wrong *field* — and for the
        /// destination-register kind, which now carries two different
        /// destination kinds down one RPC, the field is the whole distinction.
        fn last_request<T: serde::de::DeserializeOwned>(&self) -> T {
            let payloads = self.payloads.lock().unwrap();
            let bytes = payloads.last().expect("a request was sent");
            fauna_protocol::decode_strict(bytes).expect("request decodes")
        }
    }

    impl RpcRequester for &MockNest {
        type Error = String;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            self.kinds.lock().unwrap().push(kind);
            // Round-trip the payload so a request that cannot ride the wire
            // fails here rather than silently at a real nest, and keep the
            // bytes so a test can assert what was actually sent.
            let encoded = fauna_protocol::encode_canonical(&payload).map_err(|e| e.to_string())?;
            self.payloads.lock().unwrap().push(encoded.to_vec());
            let bytes = self
                .reply
                .as_ref()
                .ok_or_else(|| "no canned reply".to_string())?;
            fauna_protocol::decode_strict(bytes).map_err(|e| e.to_string())
        }
    }

    #[test]
    fn status_requests_the_status_kind_and_decodes_rows() {
        let nest = MockNest::with_reply(&BackupStatusReply {
            enrolled: true,
            destinations: vec![BackupDestinationStatusItem {
                destination_id: "dest-1".into(),
                last_upload_time: Some(1_700_000_000),
                backlog_count: 3,
                ..Default::default()
            }],
            extra: Default::default(),
        });
        let reply = block_on(BackupClient::new(&nest).status()).unwrap();

        assert_eq!(nest.kinds().as_slice(), &["fauna.backup.status"]);
        assert!(reply.enrolled);
        assert_eq!(reply.destinations.len(), 1);
        assert_eq!(reply.destinations[0].destination_id, "dest-1");
        assert_eq!(reply.destinations[0].last_upload_time, Some(1_700_000_000));
        assert_eq!(reply.destinations[0].backlog_count, 3);
    }

    /// The not-enrolled shape the reconcile path keys on: no coordinator, so
    /// no rows — regardless of what the client's own config holds.
    #[test]
    fn status_not_enrolled_carries_no_rows() {
        let nest = MockNest::with_reply(&BackupStatusReply {
            enrolled: false,
            destinations: Vec::new(),
            extra: Default::default(),
        });
        let reply = block_on(BackupClient::new(&nest).status()).unwrap();

        assert!(!reply.enrolled);
        assert!(reply.destinations.is_empty());
    }

    #[test]
    fn destination_register_sends_the_source_side_kind() {
        let nest = MockNest::with_reply(&DestinationRegisterReply {
            ok: true,
            extra: Default::default(),
        });
        block_on(BackupClient::new(&nest).destination_register(
            "dest-1".into(),
            "wss://dest.example".into(),
            "aa".repeat(32),
        ))
        .unwrap();

        assert_eq!(
            nest.kinds().as_slice(),
            &["fauna.backup.destination.register"]
        );
    }

    #[test]
    fn nest_registration_states_the_nest_kind_on_the_wire() {
        let nest = MockNest::with_reply(&DestinationRegisterReply {
            ok: true,
            extra: Default::default(),
        });
        block_on(BackupClient::new(&nest).destination_register(
            "dest-1".into(),
            "wss://dest.example".into(),
            "aa".repeat(32),
        ))
        .unwrap();

        let req: DestinationRegisterRequest = nest.last_request();
        assert_eq!(req.kind, fauna_core::data::DESTINATION_KIND_NEST);
        assert_eq!(req.custodian_device_id, None);
    }

    #[test]
    fn custodian_registration_carries_the_device_and_no_address() {
        let nest = MockNest::with_reply(&DestinationRegisterReply {
            ok: true,
            extra: Default::default(),
        });
        block_on(BackupClient::new(&nest).destination_register_custodian(
            "dest-ipad".into(),
            "dev-abc".into(),
            Some(64 << 30),
        ))
        .unwrap();

        assert_eq!(
            nest.kinds().as_slice(),
            &["fauna.backup.destination.register"],
            "a custodian registers through the same idempotent kind as a nest"
        );
        let req: DestinationRegisterRequest = nest.last_request();
        assert_eq!(req.kind, fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE);
        assert_eq!(req.custodian_device_id.as_deref(), Some("dev-abc"));
        // The cap rides the registration because the registry is what this
        // device's *host* reads back: on desktop the pull runs in the
        // bearer-only sync agent, which cannot open the at-rest row.
        assert_eq!(req.capacity_cap_bytes, Some(64 << 30));
        // The property that matters: a custodian has no address, so the nest
        // gets nothing it could try to federate-dial.
        assert!(req.destination_nest_url.is_empty());
        assert!(req.destination_nest_id.is_empty());
    }

    #[test]
    fn a_device_reads_its_own_assignment_off_the_registry() {
        // The bearer-only host's discovery path (`apps/sync-agent.md`
        // § Control plane split — destinations are read from nest rows, never
        // over local IPC). One `destination.list` call, and the shared matcher
        // picks this device's row out of the owner's whole destination set.
        let nest = MockNest::with_reply(&fauna_protocol::backup::DestinationListReply {
            destinations: vec![
                fauna_protocol::backup::DestinationItem {
                    destination_id: "peer-nest".into(),
                    destination_nest_url: "https://d1.example".into(),
                    ..Default::default()
                },
                fauna_protocol::backup::DestinationItem {
                    destination_id: "dest-laptop".into(),
                    kind: fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE.into(),
                    custodian_device_id: Some("dev-abc".into()),
                    capacity_cap_bytes: Some(64 << 30),
                    ..Default::default()
                },
                fauna_protocol::backup::DestinationItem {
                    destination_id: "dest-ipad".into(),
                    kind: fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE.into(),
                    custodian_device_id: Some("dev-other".into()),
                    capacity_cap_bytes: Some(1 << 30),
                    ..Default::default()
                },
            ],
            extra: Default::default(),
        });

        let found = block_on(BackupClient::new(&nest).custodian_assignment("dev-abc"))
            .unwrap()
            .expect("this device is enrolled");
        assert_eq!(nest.kinds().as_slice(), &["fauna.backup.destination.list"]);
        assert_eq!(found.destination_id, "dest-laptop");
        assert_eq!(found.capacity_cap_bytes, Some(64 << 30));

        // A device that is not enrolled gets `None`, not an error — the
        // ordinary state on most of the owner's devices.
        assert!(
            block_on(BackupClient::new(&nest).custodian_assignment("dev-unenrolled"))
                .unwrap()
                .is_none()
        );
    }

    /// The regression fence behind the re-seed's safety (`backup-destinations.md`
    /// § Re-seed): a rebuilt nest answers an empty registry, and a device holding
    /// the lost nest's copy must read that as *not assigned* — so no host is
    /// built and no pull can read the empty source as "everything was deleted"
    /// and put the held corpus on the grace clock before the owner restores it.
    #[test]
    fn a_rebuilt_nests_empty_registry_assigns_no_custodian() {
        let nest = MockNest::with_reply(&fauna_protocol::backup::DestinationListReply {
            destinations: vec![],
            extra: Default::default(),
        });
        assert!(
            block_on(BackupClient::new(&nest).custodian_assignment("dev-abc"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn custodian_checkin_sends_its_own_kind_with_the_cap_state() {
        let nest = MockNest::with_reply(&CustodianCheckinReply {
            ok: true,
            extra: Default::default(),
        });
        block_on(BackupClient::new(&nest).custodian_checkin(
            "dest-ipad".into(),
            4_096,
            12_345,
            fauna_protocol::backup::CAP_STATE_REACHED.into(),
            None,
            None,
            Some("dev-ipad".into()),
        ))
        .unwrap();

        assert_eq!(nest.kinds().as_slice(), &["fauna.backup.custodian.checkin"]);
        let req: CustodianCheckinRequest = nest.last_request();
        assert_eq!(req.high_water, 4_096);
        assert_eq!(req.held_bytes, 12_345);
        assert_eq!(req.cap_state, fauna_protocol::backup::CAP_STATE_REACHED);
        assert_eq!(
            req.device_id.as_deref(),
            Some("dev-ipad"),
            "the check-in names the device speaking — the nest authenticates the \
             owner, not the device, so without this it cannot refuse a check-in \
             against a row registered to a different one"
        );
    }

    /// A cap-reached custodian's check-in must SAY cap-reached. Reporting off
    /// the plan is what stops the two halves drifting apart — a stopped
    /// custodian reporting `CAP_STATE_OK` renders as ordinary lag, and the user
    /// is never told their backup stopped.
    #[test]
    fn checking_in_off_the_plan_carries_both_halves_the_plan_decided() {
        use crate::custodian::{CapState, HeldGeneration, plan_reclaim};

        let held = vec![HeldGeneration {
            path: "a/seg-1.dat".into(),
            manifest_hash: "aa".into(),
            size_bytes: 900,
            stored_at: 0,
            deleted: false,
        }];
        // Live bytes alone exceed the cap: nothing may be reclaimed, so the
        // custodian stops pulling.
        let plan = plan_reclaim(&held, Some(500), 0);
        assert_eq!(plan.cap_state, CapState::Reached);

        let nest = MockNest::with_reply(&CustodianCheckinReply {
            ok: true,
            extra: Default::default(),
        });
        block_on(plan.check_in(
            &BackupClient::new(&nest),
            "dest-ipad",
            4_096,
            None,
            Some("dev-ipad"),
        ))
        .unwrap();

        assert_eq!(nest.kinds().as_slice(), &["fauna.backup.custodian.checkin"]);
        let req: CustodianCheckinRequest = nest.last_request();
        assert_eq!(req.high_water, 4_096);
        assert_eq!(
            req.held_bytes, plan.held_bytes,
            "held_bytes comes from the plan, not a second count"
        );
        assert_eq!(
            req.cap_state,
            fauna_protocol::backup::CAP_STATE_REACHED,
            "a custodian that stopped at its cap must not read as ordinary lag"
        );
    }

    /// **A failing self-audit is REPORTED, and it does not drag the pass clock
    /// forward with it.**
    ///
    /// Two things this pins, both of which make a rotted store look healthy when
    /// broken. (1) The check-in still goes out: withholding it would silence the
    /// row, and silence is read by the 30-day intermittency rule as a sleeping
    /// device — the wrong alarm, thirty days late, for the one failure the audit
    /// exists to catch. (2) `last_audit_passed_at` stays at the previous *pass*,
    /// so the row reads "verified then, rotten since" rather than "verified just
    /// now".
    ///
    /// Mutation check: build the verdict with `SelfAudit::passed(now)` on the
    /// failure path — the wire then carries `ok` plus a fresh timestamp, i.e. the
    /// rotted store reporting its healthiest-ever row.
    #[test]
    fn a_failing_self_audit_is_reported_and_keeps_the_last_pass_timestamp() {
        use crate::custodian::{CapState, ReclaimPlan, SelfAudit};

        let plan = ReclaimPlan {
            reclaim: Vec::new(),
            held_bytes: 4_096,
            cap_state: CapState::Ok,
        };
        let nest = MockNest::with_reply(&CustodianCheckinReply {
            ok: true,
            extra: Default::default(),
        });

        // Passed a day ago; failing now.
        block_on(plan.check_in(
            &BackupClient::new(&nest),
            "dest-ipad",
            4_096,
            Some(SelfAudit::failed(Some(86_400))),
            Some("dev-ipad"),
        ))
        .unwrap();

        let req: CustodianCheckinRequest = nest.last_request();
        assert_eq!(
            req.audit_state.as_deref(),
            Some(fauna_protocol::backup::AUDIT_STATE_FAILED),
            "a failed audit must reach the owner's nest, not be withheld into silence"
        );
        assert_eq!(
            req.last_audit_passed_at,
            Some(86_400),
            "the failure must not advance the last-PASSED clock"
        );
        assert_eq!(
            req.cap_state,
            fauna_protocol::backup::CAP_STATE_OK,
            "cap state and audit state are independent — a rotted store is not a full one"
        );
    }

    /// A custodian that has never audited reports **nothing**, not a failure —
    /// otherwise every device enrolled before its first audit interval renders
    /// as failing.
    #[test]
    fn a_never_audited_custodian_sends_no_audit_fields() {
        use crate::custodian::{CapState, ReclaimPlan};

        let nest = MockNest::with_reply(&CustodianCheckinReply {
            ok: true,
            extra: Default::default(),
        });
        block_on(
            ReclaimPlan {
                reclaim: Vec::new(),
                held_bytes: 0,
                cap_state: CapState::Ok,
            }
            .check_in(
                &BackupClient::new(&nest),
                "dest-ipad",
                0,
                None,
                Some("dev-ipad"),
            ),
        )
        .unwrap();

        let req: CustodianCheckinRequest = nest.last_request();
        assert_eq!(req.audit_state, None);
        assert_eq!(req.last_audit_passed_at, None);
    }

    #[test]
    fn writer_grant_register_sends_the_destination_side_kind() {
        let nest = MockNest::with_reply(&WriterGrantRegisterReply {
            ok: true,
            extra: Default::default(),
        });
        block_on(BackupClient::new(&nest).writer_grant_register("bb".repeat(32))).unwrap();

        assert_eq!(
            nest.kinds().as_slice(),
            &["fauna.backup.writer_grant.register"]
        );
        let req: WriterGrantRegisterRequest = nest.last_request();
        assert_eq!(
            req.succeeds, None,
            "a plain registration names no predecessor"
        );
    }

    #[test]
    fn writer_grant_succeed_always_names_the_predecessor() {
        let nest = MockNest::with_reply(&WriterGrantRegisterReply {
            ok: true,
            extra: Default::default(),
        });
        block_on(BackupClient::new(&nest).writer_grant_succeed("bb".repeat(32), "aa".repeat(32)))
            .unwrap();

        assert_eq!(
            nest.kinds().as_slice(),
            &["fauna.backup.writer_grant.register"]
        );
        let req: WriterGrantRegisterRequest = nest.last_request();
        assert_eq!(req.writer_nest_id, "bb".repeat(32));
        assert_eq!(req.succeeds.as_deref(), Some("aa".repeat(32).as_str()));
    }

    #[test]
    fn generation_list_sends_the_destination_side_kind_and_decodes_the_window() {
        let nest = MockNest::with_reply(&GenerationListReply {
            generations: vec![fauna_protocol::backup::GenerationItem {
                folder_name: "__mail".into(),
                path: None,
                path_hash: "aa".repeat(32),
                manifest_hash: "bb".repeat(32),
                size_bytes: 4096,
                superseded_at: 1_700_000_000,
                extra: Default::default(),
            }],
            grace_secs: 30 * 24 * 60 * 60,
            next_cursor: None,
            extra: Default::default(),
        });
        let reply = block_on(BackupClient::new(&nest).generation_list(None)).unwrap();

        assert_eq!(nest.kinds().as_slice(), &["fauna.backup.generation.list"]);
        assert_eq!(reply.grace_secs, 30 * 24 * 60 * 60);
        assert_eq!(reply.generations.len(), 1);
        assert_eq!(
            reply.generations[0].path, None,
            "a path-less row survives the wire"
        );
    }

    #[test]
    fn generation_restore_sends_the_destination_side_kind() {
        let nest = MockNest::with_reply(&GenerationRestoreReply {
            restored: true,
            extra: Default::default(),
        });
        let reply = block_on(BackupClient::new(&nest).generation_restore(
            "__mail".into(),
            "aa".repeat(32),
            "bb".repeat(32),
        ))
        .unwrap();

        assert_eq!(
            nest.kinds().as_slice(),
            &["fauna.backup.generation.restore"]
        );
        assert!(reply.restored);
    }

    /// A transport failure propagates as `R::Error` rather than being
    /// swallowed — the page renders it, and the reconcile path distinguishes
    /// "not enrolled" from "could not ask".
    #[test]
    fn transport_error_propagates() {
        let nest = MockNest::default();
        let err = block_on(BackupClient::new(&nest).status()).unwrap_err();
        assert_eq!(err, "no canned reply");
    }
}
