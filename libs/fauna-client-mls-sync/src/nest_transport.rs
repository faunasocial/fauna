//! The native [`MlsReplicaTransport`] adapter over `Arc<NestClient>` (moved
//! here from `fauna-client-conversations`, which re-exports it, so a crate below
//! that one — the recovery crate's sweep retry — reaches the replica plane
//! through the same one adapter).

use std::sync::Arc;

use async_trait::async_trait;
use fauna_client::NestClient;
use fauna_protocol::mls_replica::ReplicaBase;

use crate::store::{MlsReplicaTransport, MlsTransportError, PutOutcome};

/// Native [`MlsReplicaTransport`] seam impl — the `fauna.mls.{get,put}` sealed
/// replica plane (cross-device MLS group-state sync, `docs/goal/behavior/devices.md`
/// § Cross-device MLS group-state sync) over `Arc<NestClient>`.
///
/// The seam keeps `MlsStateSync` / `FaunaCommitGate` non-generic (the dyn-erased
/// transport, design § "dyn-erasure"); this adapter is the one native place the
/// seam meets the wire, delegating to this crate's
/// `rpc_transport_{get,put}` helpers (which own the encode + the
/// `fauna.mls.conflict` → [`PutOutcome::Conflict`] classification). Shared by
/// every native app, exactly as `fauna_client_conversations::NestConversationsRpc`
/// is — wired once per client via
/// `MlsStateSync::new(Box::new(NestMlsReplicaTransport::new(nest)), …)`, and by the
/// sweep retry's parked arm (`fauna_client_recovery::ceremony::retry_sweep_as_successor`).
/// The wasm twin is `fauna_client_conversations::WsMlsReplicaTransport` over
/// `WsRpcClient`.
pub struct NestMlsReplicaTransport {
    nest: Arc<NestClient>,
}

impl NestMlsReplicaTransport {
    pub fn new(nest: Arc<NestClient>) -> Self {
        Self { nest }
    }
}

#[async_trait]
impl MlsReplicaTransport for NestMlsReplicaTransport {
    async fn get(&self, path: String) -> Result<Option<Vec<u8>>, MlsTransportError> {
        crate::store::rpc_transport_get(&self.nest, path).await
    }

    async fn get_hash(&self, path: String) -> Result<Option<[u8; 32]>, MlsTransportError> {
        crate::store::rpc_transport_get_hash(&self.nest, path).await
    }

    async fn put(
        &self,
        path: String,
        blob: Vec<u8>,
        base: ReplicaBase,
    ) -> Result<PutOutcome, MlsTransportError> {
        crate::store::rpc_transport_put(&self.nest, path, blob, base).await
    }
}
