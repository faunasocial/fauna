//! WS-RPC production impl of the [`SyncControlApi`] seam.
//!
//! Behavior-preserving transport extraction: the two `self.nest_client.request(..)`
//! calls that used to sit inline in `engine.rs` now live here, byte-identical —
//! same kinds, same request types, same reply types, same error text. The engine
//! still builds every request (including all path/details sealing); this only
//! carries it.
//!
//! Generic over `R: RpcRequester` so the `Arc<NestClient>` the engine already
//! holds satisfies it directly through `fauna-protocol`'s blanket
//! `impl RpcRequester for Arc<T>` — no rewrapping at the construction site, and
//! no second concrete arm to keep in step (contrast the machine crates, which
//! need native + wasm arms; this crate is native-only — see the module doc).

use async_trait::async_trait;
use fauna_protocol::RpcRequester;
use fauna_protocol::folders::{
    ConflictReportReply, ConflictReportRequest, ConflictResolveReply, ConflictResolveRequest,
    FolderDepositsListReply, FolderDepositsListRequest, FolderDepositsRetireReply,
    FolderDepositsRetireRequest, FolderUpdateReply, FolderUpdateRequest,
    KIND_FOLDERS_DEPOSITS_LIST, KIND_FOLDERS_DEPOSITS_RETIRE, KIND_FOLDERS_UPDATE,
};

use fauna_protocol::web::{WebFilesPruneSealedReply, WebFilesPruneSealedRequest};

use super::{SyncControlApi, SyncControlError};

/// WS-RPC kind the engine reports conflicts under (with candidate versions).
/// See `docs/goal/behavior/file-sync.md` § Conflicts.
const CONFLICTS_REPORT_KIND: &str = "fauna.sync.conflicts.report";

/// WS-RPC kind a cured skip's candidate-free resolve rides.
/// See `docs/goal/behavior/conflicts.md` § Skipped catch-up changes reach the
/// review list.
const CONFLICTS_RESOLVE_KIND: &str = "fauna.sync.conflicts.resolve";

/// WS-RPC kind the complete-set declaration rides
/// (`SyncEngine::converge_corpus_to_website`). See
/// `docs/goal/behavior/web-content-hosting.md` § Content model.
const WEB_FILES_PRUNE_SEALED_KIND: &str = "fauna.web.files.prune_sealed";

/// The production control plane: any [`RpcRequester`] (natively
/// `Arc<fauna_client::NestClient>`).
pub struct WsRpcSyncControl<R: RpcRequester> {
    nest: R,
}

// `SyncControlApi` requires `Debug`, but `NestClient` carries no renderable
// state worth printing, so a name-only impl satisfies the bound — the same
// choice `WsRpcFolderNest` makes for the same reason.
impl<R: RpcRequester> std::fmt::Debug for WsRpcSyncControl<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WsRpcSyncControl")
    }
}

impl<R> WsRpcSyncControl<R>
where
    R: RpcRequester,
    R::Error: std::fmt::Display,
{
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// The kind composition + error mapping, written once (priority #2). The
    /// trait impl below is on the CONCRETE `Arc<NestClient>` and just delegates
    /// here — see that module's note on why it cannot be generic.
    async fn do_report_conflict(
        &self,
        req: ConflictReportRequest,
    ) -> Result<i64, SyncControlError> {
        let reply: ConflictReportReply = self
            .nest
            .request(CONFLICTS_REPORT_KIND, req)
            .await
            .map_err(|e| SyncControlError(format!("conflict report failed: {e}")))?;
        Ok(reply.id)
    }

    async fn do_resolve_conflict(
        &self,
        req: ConflictResolveRequest,
    ) -> Result<bool, SyncControlError> {
        let reply: ConflictResolveReply = self
            .nest
            .request(CONFLICTS_RESOLVE_KIND, req)
            .await
            .map_err(|e| SyncControlError(format!("conflict resolve failed: {e}")))?;
        Ok(reply.resolved)
    }

    /// Rides the sealed set-name stamp (`SyncEngine::stamp_sealed_set_name`).
    /// See `docs/goal/behavior/file-sync.md` § Sealed names & paths.
    async fn do_update_folder(&self, req: FolderUpdateRequest) -> Result<bool, SyncControlError> {
        let reply: FolderUpdateReply = self
            .nest
            .request(KIND_FOLDERS_UPDATE, req)
            .await
            .map_err(|e| SyncControlError(e.to_string()))?;
        Ok(reply.ok)
    }

    async fn do_prune_sealed_web_files(
        &self,
        req: WebFilesPruneSealedRequest,
    ) -> Result<u32, SyncControlError> {
        let reply: WebFilesPruneSealedReply = self
            .nest
            .request(WEB_FILES_PRUNE_SEALED_KIND, req)
            .await
            .map_err(|e| SyncControlError(format!("sealed web_files prune failed: {e}")))?;
        Ok(reply.dropped)
    }

    async fn do_list_deposits(
        &self,
        req: FolderDepositsListRequest,
    ) -> Result<FolderDepositsListReply, SyncControlError> {
        self.nest
            .request(KIND_FOLDERS_DEPOSITS_LIST, req)
            .await
            .map_err(|e| SyncControlError(format!("deposits list failed: {e}")))
    }

    async fn do_retire_deposit(
        &self,
        req: FolderDepositsRetireRequest,
    ) -> Result<bool, SyncControlError> {
        let reply: FolderDepositsRetireReply = self
            .nest
            .request(KIND_FOLDERS_DEPOSITS_RETIRE, req)
            .await
            .map_err(|e| SyncControlError(format!("deposit retire failed: {e}")))?;
        Ok(reply.retired)
    }
}

// The trait impl is on the CONCRETE `WsRpcSyncControl<Arc<NestClient>>`, not on
// a generic `R: RpcRequester`, and that is forced rather than stylistic:
// `RpcRequester::request` is an `async fn` in trait (AFIT), so for a generic `R`
// the compiler cannot know its future is `Send` — and `#[async_trait]` boxes as
// `Pin<Box<dyn Future + Send>>`, which fails to compile ("`<R as
// RpcRequester>::request` is an `async fn` in trait, which does not
// automatically imply that its future is `Send`"). Naming the concrete type
// resolves it, because `NestClient::request`'s future genuinely is `Send` (it
// encodes the payload before the first await — see `fauna_client`'s own
// `RpcRequester` impl). Same reason `fauna-folders-machine`'s seam splits
// generic inherent methods from per-target concrete impls.
#[async_trait]
impl SyncControlApi for WsRpcSyncControl<std::sync::Arc<fauna_client::NestClient>> {
    async fn report_conflict(&self, req: ConflictReportRequest) -> Result<i64, SyncControlError> {
        self.do_report_conflict(req).await
    }

    async fn resolve_conflict(
        &self,
        req: ConflictResolveRequest,
    ) -> Result<bool, SyncControlError> {
        self.do_resolve_conflict(req).await
    }

    async fn update_folder(&self, req: FolderUpdateRequest) -> Result<bool, SyncControlError> {
        self.do_update_folder(req).await
    }

    async fn prune_sealed_web_files(
        &self,
        req: WebFilesPruneSealedRequest,
    ) -> Result<u32, SyncControlError> {
        self.do_prune_sealed_web_files(req).await
    }

    async fn list_deposits(
        &self,
        req: FolderDepositsListRequest,
    ) -> Result<FolderDepositsListReply, SyncControlError> {
        self.do_list_deposits(req).await
    }

    async fn retire_deposit(
        &self,
        req: FolderDepositsRetireRequest,
    ) -> Result<bool, SyncControlError> {
        self.do_retire_deposit(req).await
    }
}
