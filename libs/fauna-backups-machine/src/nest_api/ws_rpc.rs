//! WS-RPC production impl of the page seam, over
//! `fauna_client_snapshots::SnapshotsClient` + `fauna_client_folders::FoldersClient`
//! (the shared `fauna.filesync.snapshot.*` / `fauna.folders.*` typed-call
//! surfaces). This is the directive-correct (`no-http-ws-rpc-everywhere`)
//! consumer — no HTTP.
//!
//! Mirrors `fauna_devices_machine::nest_api::ws_rpc`: a generic
//! [`WsRpcBackupsNest<R>`] holds the kind composition + error mapping once
//! (priority #2); the per-target concrete trait impls (native
//! `Arc<NestClient>`, wasm `WsRpcClient`) and the `build_backups_machine`
//! constructors live in the `cfg`-gated submodules below and just delegate.

use std::sync::Arc;

use async_trait::async_trait;
use fauna_client_folders::FoldersClient;
use fauna_client_snapshots::SnapshotsClient;
use fauna_core::label_custody::LabelCustody;
use fauna_protocol::filesync::{
    SnapshotCheckReply, SnapshotGetReply, SnapshotPruneSetPolicyReply, SnapshotSummaryRow,
};
use fauna_protocol::folders::FolderSummary as WireFolderSummary;
use fauna_protocol::{RpcErrorClass, RpcRequester};

use super::{BackupsApiError, BackupsNestApi};

/// Generic WS-RPC seam over any [`RpcRequester`]. Native binds
/// `R = Arc<NestClient>`, wasm `R = WsRpcClient`; the per-target trait impls
/// below delegate to these inherent methods so the logic is written once.
pub struct WsRpcBackupsNest<R: RpcRequester> {
    snapshots: SnapshotsClient<R>,
    folders: FoldersClient<R>,
}

// `BackupsNestApi` requires `Debug`, but the clients aren't `Debug`; the
// requester carries no renderable state, so a name-only impl satisfies the bound.
impl<R: RpcRequester> std::fmt::Debug for WsRpcBackupsNest<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WsRpcBackupsNest")
    }
}

impl<R> WsRpcBackupsNest<R>
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    /// Build the seam with the reader's **label custody** wired into the
    /// `SnapshotsClient` — the ratified construction input (§ Snapshot-list
    /// shape). A keyless custody still renders: the sealed-plane reads degrade
    /// per the path-sealing consumer-wiring rule rather than bypassing it, so
    /// this is never an "optional extra".
    ///
    /// The same custody renders the folder selector's set names: since schema
    /// 114 a sealed set's row rests no plaintext name, and a custody-less
    /// `FoldersClient::list` omits every such set.
    pub fn new(nest: R, custody: LabelCustody) -> Self {
        Self {
            snapshots: SnapshotsClient::new(nest.clone()).with_label_custody(custody.clone()),
            folders: FoldersClient::new(nest).with_label_custody(custody),
        }
    }

    async fn do_list_folders(&self) -> Result<Vec<WireFolderSummary>, BackupsApiError> {
        // `list`, not `list_owned_and_shared`: every snapshot verb is
        // owner-scoped, so a shared-with-me row would render affordances that
        // must then fail (§ Snapshot-list shape, *Selector source*).
        let reply = self.folders.list().await.map_err(map_err)?;
        Ok(reply.folders)
    }

    async fn do_list_snapshots(
        &self,
        folder: &str,
    ) -> Result<Vec<SnapshotSummaryRow>, BackupsApiError> {
        // `limit = 0` → the server default page size.
        let reply = self
            .snapshots
            .list(None, Some(folder.to_string()), 0)
            .await
            .map_err(map_err)?;
        Ok(reply.rows)
    }

    async fn do_create_snapshot(
        &self,
        folder: &str,
        device_id: Option<Vec<u8>>,
    ) -> Result<(), BackupsApiError> {
        // `tags: vec![]` — manual creates are untagged (§ *Create* ruling).
        self.snapshots
            .create_folder(folder, Vec::new(), device_id)
            .await
            .map_err(map_err)?;
        Ok(())
    }

    async fn do_delete_snapshot(&self, snapshot_id: i64) -> Result<(), BackupsApiError> {
        self.snapshots.delete(snapshot_id).await.map_err(map_err)?;
        Ok(())
    }

    async fn do_undelete_snapshot(&self, snapshot_id: i64) -> Result<(), BackupsApiError> {
        self.snapshots
            .undelete(snapshot_id)
            .await
            .map_err(map_err)?;
        Ok(())
    }

    async fn do_delete_snapshot_immediate(
        &self,
        snapshot_id: i64,
        confirm_id: &str,
        acknowledge: &str,
    ) -> Result<(), BackupsApiError> {
        self.snapshots
            .delete_immediate(snapshot_id, confirm_id, acknowledge)
            .await
            .map_err(map_err)?;
        Ok(())
    }

    async fn do_prune_set_policy(
        &self,
        folder: &str,
        dry_run: bool,
    ) -> Result<SnapshotPruneSetPolicyReply, BackupsApiError> {
        self.snapshots
            .prune_set_policy(folder, dry_run)
            .await
            .map_err(map_err)
    }

    async fn do_check(
        &self,
        folder: &str,
        verify_content: bool,
    ) -> Result<SnapshotCheckReply, BackupsApiError> {
        self.snapshots
            .check(folder, verify_content)
            .await
            .map_err(map_err)
    }

    /// The sealed-plane detail read. `self.snapshots` was constructed with the
    /// reader's [`LabelCustody`], so a sealed set's rows come back **opened**
    /// here — the consumer-wiring rule is satisfied once, on the seam, for every
    /// app (`behavior/path-sealing.md` § THE CONSUMER-WIRING RULE).
    async fn do_get_snapshot(&self, snapshot_id: i64) -> Result<SnapshotGetReply, BackupsApiError> {
        self.snapshots.get(snapshot_id).await.map_err(map_err)
    }
}

fauna_core::map_rpc_error! {
    /// Map a transport `R::Error` onto [`BackupsApiError`], keyed on the WS-RPC
    /// `RpcError.code` suffix. A transport fault (the request never reached a
    /// server rejection) is `Transient`. Mirrors
    /// `fauna_devices_machine::nest_api::ws_rpc::map_err`, plus the
    /// `backup_unavailable` / `unknown_kind` arm this page needs: both mean
    /// *retrying cannot help*, which is what separates them from `Transient` —
    /// rendering either as a retryable blip would leave the user clicking a
    /// button that can never work.
    fn map_err(e) -> BackupsApiError {
        "conflict" => Conflict,
        "not_found" => NotFound,
        "invalid_request" | "malformed" => BadRequest,
        "backup_unavailable" | "unknown_kind" => Unavailable,
    }
}

// ── Native (`Arc<NestClient>`) ──────────────────────────────────────────────
#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use crate::machine::BackupsMachine;
    use crate::observer::BackupsObserver;
    use fauna_client::NestClient;

    #[async_trait]
    impl BackupsNestApi for WsRpcBackupsNest<Arc<NestClient>> {
        async fn list_folders(&self) -> Result<Vec<WireFolderSummary>, BackupsApiError> {
            self.do_list_folders().await
        }
        async fn list_snapshots(
            &self,
            folder: &str,
        ) -> Result<Vec<SnapshotSummaryRow>, BackupsApiError> {
            self.do_list_snapshots(folder).await
        }
        async fn create_snapshot(
            &self,
            folder: &str,
            device_id: Option<Vec<u8>>,
        ) -> Result<(), BackupsApiError> {
            self.do_create_snapshot(folder, device_id).await
        }
        async fn delete_snapshot(&self, snapshot_id: i64) -> Result<(), BackupsApiError> {
            self.do_delete_snapshot(snapshot_id).await
        }
        async fn undelete_snapshot(&self, snapshot_id: i64) -> Result<(), BackupsApiError> {
            self.do_undelete_snapshot(snapshot_id).await
        }
        async fn delete_snapshot_immediate(
            &self,
            snapshot_id: i64,
            confirm_id: &str,
            acknowledge: &str,
        ) -> Result<(), BackupsApiError> {
            self.do_delete_snapshot_immediate(snapshot_id, confirm_id, acknowledge)
                .await
        }
        async fn prune_set_policy(
            &self,
            folder: &str,
            dry_run: bool,
        ) -> Result<SnapshotPruneSetPolicyReply, BackupsApiError> {
            self.do_prune_set_policy(folder, dry_run).await
        }
        async fn check(
            &self,
            folder: &str,
            verify_content: bool,
        ) -> Result<SnapshotCheckReply, BackupsApiError> {
            self.do_check(folder, verify_content).await
        }
        async fn get_snapshot(
            &self,
            snapshot_id: i64,
        ) -> Result<SnapshotGetReply, BackupsApiError> {
            self.do_get_snapshot(snapshot_id).await
        }
    }

    /// Build the page machine over the session's connected native requester.
    /// The client's login glue supplies the reader's [`LabelCustody`] (it holds
    /// the keypair the builder does not) and, via
    /// [`BackupsMachine::set_device_id`], the shell's stable sync device id.
    pub fn build_backups_machine(
        nest: Arc<NestClient>,
        observer: Arc<dyn BackupsObserver>,
        custody: LabelCustody,
    ) -> Arc<BackupsMachine> {
        let api: Arc<dyn BackupsNestApi> = Arc::new(WsRpcBackupsNest::new(nest, custody));
        BackupsMachine::new(observer, api)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::build_backups_machine;

// ── wasm (`WsRpcClient`) ────────────────────────────────────────────────────
#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;
    use crate::machine::BackupsMachine;
    use crate::observer::BackupsObserver;
    use fauna_rpc_wasm::WsRpcClient;

    #[async_trait(?Send)]
    impl BackupsNestApi for WsRpcBackupsNest<WsRpcClient> {
        async fn list_folders(&self) -> Result<Vec<WireFolderSummary>, BackupsApiError> {
            self.do_list_folders().await
        }
        async fn list_snapshots(
            &self,
            folder: &str,
        ) -> Result<Vec<SnapshotSummaryRow>, BackupsApiError> {
            self.do_list_snapshots(folder).await
        }
        async fn create_snapshot(
            &self,
            folder: &str,
            device_id: Option<Vec<u8>>,
        ) -> Result<(), BackupsApiError> {
            self.do_create_snapshot(folder, device_id).await
        }
        async fn delete_snapshot(&self, snapshot_id: i64) -> Result<(), BackupsApiError> {
            self.do_delete_snapshot(snapshot_id).await
        }
        async fn undelete_snapshot(&self, snapshot_id: i64) -> Result<(), BackupsApiError> {
            self.do_undelete_snapshot(snapshot_id).await
        }
        async fn delete_snapshot_immediate(
            &self,
            snapshot_id: i64,
            confirm_id: &str,
            acknowledge: &str,
        ) -> Result<(), BackupsApiError> {
            self.do_delete_snapshot_immediate(snapshot_id, confirm_id, acknowledge)
                .await
        }
        async fn prune_set_policy(
            &self,
            folder: &str,
            dry_run: bool,
        ) -> Result<SnapshotPruneSetPolicyReply, BackupsApiError> {
            self.do_prune_set_policy(folder, dry_run).await
        }
        async fn check(
            &self,
            folder: &str,
            verify_content: bool,
        ) -> Result<SnapshotCheckReply, BackupsApiError> {
            self.do_check(folder, verify_content).await
        }
        async fn get_snapshot(
            &self,
            snapshot_id: i64,
        ) -> Result<SnapshotGetReply, BackupsApiError> {
            self.do_get_snapshot(snapshot_id).await
        }
    }

    /// Build the page machine over the browser WS-RPC transport.
    pub fn build_backups_machine(
        nest: WsRpcClient,
        observer: Arc<dyn BackupsObserver>,
        custody: LabelCustody,
    ) -> Arc<BackupsMachine> {
        let api: Arc<dyn BackupsNestApi> = Arc::new(WsRpcBackupsNest::new(nest, custody));
        BackupsMachine::new(observer, api)
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::build_backups_machine;
