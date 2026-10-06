//! Trait abstraction for the Backups page's nest-side surface.
//!
//! The page reads + write gestures go through [`BackupsNestApi`]. Production
//! code uses the WS-RPC [`ws_rpc::WsRpcBackupsNest`] (over the **custody-wired**
//! `fauna-client-snapshots` `SnapshotsClient` + `fauna-client-folders`); tests
//! use [`FakeBackupsNestApi`] (gated under
//! `#[cfg(any(test, feature = "test-helpers"))]`). Mirrors
//! `fauna_devices_machine::nest_api`.
//!
//! Transport: every read/write rides the authenticated WS-RPC connection — the
//! page runs inside an already-logged-in session, so the seam is constructed
//! with the session's connected requester (`Arc<NestClient>` native /
//! `WsRpcClient` wasm) and needs no per-call URL or token. There is no HTTP impl
//! (the `no-http-ws-rpc-everywhere` directive).
//!
//! **Custody lives on the seam, not on the machine** — the ratified section
//! wires the `SnapshotsClient` with the reader's `LabelCustody` at construction,
//! so the sealed-plane reads (`get`, `diff`) render under it without the machine
//! ever holding a key. That is the one structural difference from
//! `fauna-devices-machine`, whose seam is key-free and whose machine holds the
//! custody instead.

pub mod fake;
#[cfg(feature = "rpc-glue")]
pub mod ws_rpc;

#[cfg(any(test, feature = "test-helpers"))]
pub use fake::{FakeBackupsNestApi, FakeCall};
#[cfg(feature = "rpc-glue")]
pub use ws_rpc::build_backups_machine;

use async_trait::async_trait;

use fauna_protocol::filesync::{
    SnapshotCheckReply, SnapshotGetReply, SnapshotPruneSetPolicyReply, SnapshotSummaryRow,
};
use fauna_protocol::folders::FolderSummary as WireFolderSummary;

fauna_core::declare_api_error!(
    /// Failure of a page-level nest call. `detail` carries the nest's error
    /// text. Mirrors `fauna_devices_machine::DevicesApiError`; the WS-RPC impl
    /// keys the variant off the `RpcError.code` suffix.
    BackupsApiError {
        /// Concurrent destructive op / conflicting state.
        Conflict,
        /// Invalid request (bad set name / unknown snapshot shape).
        BadRequest,
        /// Folder or snapshot not found / not owned.
        NotFound,
        /// The nest serves no backup service (`backup_unavailable`), or the
        /// kind is not served by it. Distinct from `Transient` because
        /// retrying cannot help: the page renders it as a standing condition.
        Unavailable,
        /// Transport fault / 5xx — retryable.
        Transient,
    }
);

// `MaybeSendSync` supertrait + dual `async_trait` arm so the one seam serves
// native (`Arc<NestClient>`, `Send + Sync`) and wasm (the single-threaded
// `Rc`-based `WsRpcClient`, `!Send`) — the identical pattern on
// `fauna_devices_machine::DevicesNestApi`.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait BackupsNestApi: fauna_core::MaybeSendSync + std::fmt::Debug {
    /// `fauna.folders.list` — the sets the bearer **owns**
    /// (`include_shared_with_me` unset: every snapshot verb is owner-scoped, so
    /// a shared-with-me set would render affordances that must fail).
    ///
    /// Returns the **wire** rows, not [`crate::BackupFolderRow`]: the machine
    /// applies the reserved-name filter and the name ordering, so those rulings
    /// live in one place and a fake can feed the machine an unfiltered,
    /// unordered list to prove them.
    async fn list_folders(&self) -> Result<Vec<WireFolderSummary>, BackupsApiError>;

    /// `fauna.filesync.snapshot.list` in **folder mode** — the selected set's
    /// rows, newest first, soft-deleted and deletion-pending rows included (the
    /// lifecycle fields are what make them renderable).
    ///
    /// Returns the **wire** rows: `last_backed_up` and per-row `integrity` are
    /// machine-side *derivations*, not field copies, and both would be lost by
    /// transcribing here.
    async fn list_snapshots(
        &self,
        folder: &str,
    ) -> Result<Vec<SnapshotSummaryRow>, BackupsApiError>;

    /// `fauna.filesync.snapshot.create_folder` — a manual snapshot of the set.
    ///
    /// ⚠ `tags` is deliberately **not** a parameter: manual creates are untagged
    /// (§ Snapshot-list shape, *Create* ruling — a tag is a retention shield, so
    /// auto-tagging silently exempts manual snapshots from retention forever;
    /// windows' `["manual"]` is the live instance this retires). `device_id` is
    /// the shell's stable sync device id when it has one, for row provenance.
    async fn create_snapshot(
        &self,
        folder: &str,
        device_id: Option<Vec<u8>>,
    ) -> Result<(), BackupsApiError>;

    /// `fauna.filesync.snapshot.delete` — queues the 48 h pending action; the
    /// row then renders `DeletionPending`.
    async fn delete_snapshot(&self, snapshot_id: i64) -> Result<(), BackupsApiError>;

    /// `fauna.filesync.snapshot.undelete` — recover a soft-deleted snapshot
    /// inside its 30-day window; the row returns `Active` on the next list
    /// read. The nest answers `not_soft_deleted` for any other row state.
    async fn undelete_snapshot(&self, snapshot_id: i64) -> Result<(), BackupsApiError>;

    /// `fauna.filesync.snapshot.delete_immediate` — the modal-gated hard delete
    /// (§ Architectural rules, rule 4).
    ///
    /// `confirm_id` and `acknowledge` are the modal's two exact-match inputs,
    /// carried verbatim: the nest re-checks them, so the friction bar is
    /// enforced on both sides rather than trusting the client's own predicate.
    async fn delete_snapshot_immediate(
        &self,
        snapshot_id: i64,
        confirm_id: &str,
        acknowledge: &str,
    ) -> Result<(), BackupsApiError>;

    /// `fauna.filesync.snapshot.prune_set_policy` — apply the set's **resting**
    /// retention policy, preview (`dry_run`) then execute.
    ///
    /// ⚠ There is deliberately **no policy parameter** (§ Architectural rules,
    /// rule 5): a client-supplied policy is exactly the drift this kind exists
    /// to end.
    async fn prune_set_policy(
        &self,
        folder: &str,
        dry_run: bool,
    ) -> Result<SnapshotPruneSetPolicyReply, BackupsApiError>;

    /// `fauna.filesync.snapshot.get` — one snapshot's file list, the
    /// **sealed-plane read** behind `snapshot-detail-files`.
    ///
    /// Belongs to this seam rather than to each app because it is the one call
    /// on the page that needs label custody: the seam's `SnapshotsClient` is
    /// constructed with the reader's `LabelCustody`, so a sealed set's rows
    /// arrive opened here and no client ever has to wire custody a second time
    /// (`behavior/path-sealing.md` § THE CONSUMER-WIRING RULE; § User actions
    /// puts this read on "the machine's custody-wired `SnapshotsClient::get`").
    ///
    /// Returns the **wire** reply, like every other read on this trait — the
    /// machine transcribes it into [`crate::SnapshotDetail`].
    async fn get_snapshot(&self, snapshot_id: i64) -> Result<SnapshotGetReply, BackupsApiError>;

    /// `fauna.filesync.snapshot.check` — integrity scan over the set.
    ///
    /// Returns the **wire** reply so the machine calls the shared
    /// `SnapshotCheckReply::is_ok()` predicate itself and derives each row's
    /// implication from `structured_errors` — the two things adopting apps kept
    /// re-coding.
    async fn check(
        &self,
        folder: &str,
        verify_content: bool,
    ) -> Result<SnapshotCheckReply, BackupsApiError>;
}
