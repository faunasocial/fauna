//! Trait abstraction for the engine's nest-side **control plane**.
//!
//! The engine has two planes. The byte plane is HTTP
//! ([`crate::nest_client::SyncClient`] — chunks, manifests, blobs, segments) and
//! the crate's stateful-wiremock test modules already drive it end to end. The
//! control plane is WS-RPC, a handful of kinds wide
//! (`fauna.sync.conflicts.{report,resolve}`, `fauna.folders.update`,
//! `fauna.web.files.prune_sealed`, `fauna.folders.deposits.{list,retire}`),
//! and until this
//! seam existed had **no test double at all** — it went straight through the
//! concrete `Arc<fauna_client::NestClient>` the engine holds.
//!
//! That gap was not cosmetic. [`crate::engine::SyncEngine::auto_resolve_conflict`]
//! branches on whether the resolved report reached the nest: success yields
//! `ResolvedApply::KeepLocal` (local content wins, and the row is re-pointed to
//! `Synced` at the LOCAL content's hash), failure degrades to
//! `ResolvedApply::Unresolved`. A plain-HTTP mock cannot answer a WS-RPC call, so
//! **every in-crate test of that path necessarily landed on `Unresolved`** and
//! the `KeepLocal` state — a state real deployments reach constantly — could not
//! be asserted at all. `create_arm_local_conflict_test.rs`'s module doc says so
//! outright.
//!
//! Production code uses [`ws_rpc::WsRpcSyncControl`]; tests use
//! [`fake::FakeSyncControl`] (gated `#[cfg(any(test, feature = "test-helpers"))]`)
//! to fixture replies without a transport. Mirrors the same `nest_api/{mod,
//! ws_rpc, fake}.rs` split six machine crates already use —
//! `fauna-devices-machine`, `fauna-folders-machine`, `fauna-media-machine`,
//! `fauna-labeler-catalog-machine`, `fauna-atproto-settings-machine`,
//! `fauna-onboarding-machine` (priority #3: same concepts and architecture
//! everywhere).
//!
//! **Why a narrow domain trait rather than doubling `RpcRequester` directly.**
//! `fauna_protocol::RpcRequester` is AFIT with generic `Req`/`Reply` per method,
//! so it is not dyn-safe — an `Arc<dyn RpcRequester>` does not exist. The six
//! crates above all resolve this the same way: a narrow, `async_trait`-boxed
//! trait whose methods carry concrete types, implemented once generically over
//! `R: RpcRequester` for production. This seam is that, for this crate.
//!
//! **Native-only, deliberately.** Unlike the machine-crate seams there is no
//! `?Send` wasm arm: this crate is native-only by construction (`notify`,
//! `rusqlite`, `walkdir`, blocking `std::fs`), so the dual-arm
//! `#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]` those seams carry
//! would be dead configuration here. `MaybeSendSync` is still the supertrait so
//! the bound reads identically to its siblings if this crate ever grows one.

pub mod fake;
pub mod ws_rpc;

#[cfg(any(test, feature = "test-helpers"))]
pub use fake::{FakeSyncControl, RecordedControlCall};
pub use ws_rpc::WsRpcSyncControl;

use async_trait::async_trait;
use fauna_protocol::folders::{
    ConflictReportRequest, ConflictResolveRequest, FolderDepositsListReply,
    FolderDepositsListRequest, FolderDepositsRetireRequest, FolderUpdateRequest,
};
use fauna_protocol::web::WebFilesPruneSealedRequest;

/// A control-plane call failed. The engine treats every variant the same way —
/// fail closed — so this carries a message rather than a taxonomy: the callers
/// (`auto_resolve_conflict`'s `fallback_unresolved!`, `stamp_sealed_set_name`'s
/// deferred retry) branch on `Err` at all, never on which `Err`.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SyncControlError(pub String);

/// The engine's WS-RPC control-plane surface.
///
/// Both methods take an **already-built wire request**. That split is
/// load-bearing rather than incidental: the engine seals the conflict's path and
/// details under its own `label_seal_root` before the request is transported
/// (path-sealing S6-a — a conflict row must render for exactly the audience its
/// file rows do), and that sealing needs engine state this seam does not and
/// should not hold. Keeping the seam purely transport means substituting it in a
/// test cannot silently skip sealing — the fake sees the same sealed bytes the
/// nest would.
#[async_trait]
pub trait SyncControlApi: fauna_core::MaybeSendSync + std::fmt::Debug {
    /// `fauna.sync.conflicts.report` — report a conflict, with candidates and
    /// (for the auto-resolve shape) its resolution. Returns the nest's conflict
    /// row id.
    ///
    /// With a resolution attached the nest lands the conflict resolved AND
    /// transactionally retains the loser + records the winner head row
    /// (`file-sync.md` § Conflicts), so an `Err` here means **nothing landed**
    /// and the caller must fail closed.
    async fn report_conflict(&self, req: ConflictReportRequest) -> Result<i64, SyncControlError>;

    /// `fauna.sync.conflicts.resolve` — today only the candidate-free resolve
    /// (`winning_manifest_hash: None`) a cured `catchup_failed` skip sends for
    /// its nest row (`conflicts.md` § Skipped catch-up changes reach the review
    /// list). Returns the reply's `resolved` flag; an `Err` leaves the resolve
    /// owed for the next catch-up pass.
    async fn resolve_conflict(&self, req: ConflictResolveRequest)
    -> Result<bool, SyncControlError>;

    /// `fauna.folders.update` — the sealed set-name stamp
    /// (`file-sync.md` § Sealed names & paths). Returns the reply's `ok` flag;
    /// a transport failure is a deferred retry, not a hard error, at the call
    /// site.
    async fn update_folder(&self, req: FolderUpdateRequest) -> Result<bool, SyncControlError>;

    /// `fauna.web.files.prune_sealed` — declare one folder's **complete** live
    /// website path set, so the nest drops the sealed `web_files` rows no longer
    /// in it.
    ///
    /// Returns how many rows went. An `Err` is **not** fatal at the call site:
    /// the re-record walk it follows has already landed, and failing to prune
    /// leaves exactly the pre-fix residual (a stale row keeps serving)
    /// rather than anything new — so the caller logs and proceeds. What must
    /// never happen is the opposite: sending a set the walk did not fully
    /// produce, which would delete live content.
    async fn prune_sealed_web_files(
        &self,
        req: WebFilesPruneSealedRequest,
    ) -> Result<u32, SyncControlError>;

    /// `fauna.folders.deposits.list` — one page of the owner's parked
    /// third-party deposits, sealed as they rest (`file-sync.md` § Third-party
    /// deposit ingress). An `Err` defers adoption to the next pass.
    async fn list_deposits(
        &self,
        req: FolderDepositsListRequest,
    ) -> Result<FolderDepositsListReply, SyncControlError>;

    /// `fauna.folders.deposits.retire` — drop one parked deposit once its
    /// adopted change row is durable. Returns the reply's `retired` flag
    /// (`false`: another seat retired it first).
    async fn retire_deposit(
        &self,
        req: FolderDepositsRetireRequest,
    ) -> Result<bool, SyncControlError>;
}
