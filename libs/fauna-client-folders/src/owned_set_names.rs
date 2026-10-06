//! The set names a folder grant id is matched over, read from the owner's
//! folder-key custody plus the nest's owner-scoped folder list and folded by
//! [`crate::custody::owned_set_names_from`]. The answer behind
//! `fauna_client_capabilities::OwnedSetNames` — what the Nests page's trust
//! facet names a web-paywall grant from, and the Connected apps consent seam a
//! principal's folder grant from (`webdav-server.md` § Key model → *A
//! principal's read* rule (1)).

use std::sync::Arc;

use fauna_protocol::RpcRequester;

use crate::key_reader::FolderKeyStore;

/// The set name of every folder the owner owns, read fresh: custody, then the
/// owner-scoped folder list over `nest`. `None` when either cannot be read now.
pub async fn read_owned_set_names<R: RpcRequester>(
    keys: &dyn FolderKeyStore,
    nest: R,
) -> Option<Vec<String>> {
    let custody = crate::key_reader::FolderKeyReader::load(keys)
        .await
        .inspect_err(|e| tracing::warn!(target: "fauna_client_folders", error = %e, "owned set names: custody unreadable"))
        .ok()?;
    let rows = crate::FoldersClient::new(nest)
        .list_wire()
        .await
        .inspect_err(|e| tracing::warn!(target: "fauna_client_folders", error = %e, "owned set names: folder list unreadable"))
        .ok()?
        .folders;
    Some(crate::custody::owned_set_names_from(rows, &custody))
}

/// `fauna_client_capabilities::OwnedSetNames` over [`read_owned_set_names`].
/// `R` is the host's nest transport (native `Arc<NestClient>`, wasm
/// `WsRpcClient`).
pub struct CustodyOwnedSetNames<R> {
    /// The account's folder-key custody.
    pub keys: Arc<dyn FolderKeyStore>,
    /// The nest the owner's folder rows are listed from.
    pub nest: R,
}

#[cfg(all(feature = "mls", not(target_arch = "wasm32")))]
#[async_trait::async_trait]
impl fauna_client_capabilities::OwnedSetNames
    for CustodyOwnedSetNames<Arc<fauna_client::NestClient>>
{
    async fn owned_set_names(&self) -> Option<Vec<String>> {
        read_owned_set_names(&*self.keys, self.nest.clone()).await
    }
}

/// The SPA's twin: the same read over the browser session (`WsRpcClient`).
#[cfg(all(feature = "mls", target_arch = "wasm32"))]
#[async_trait::async_trait(?Send)]
impl fauna_client_capabilities::OwnedSetNames
    for CustodyOwnedSetNames<fauna_rpc_wasm::WsRpcClient>
{
    async fn owned_set_names(&self) -> Option<Vec<String>> {
        read_owned_set_names(&*self.keys, self.nest.clone()).await
    }
}
