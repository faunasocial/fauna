//! UniFFI façade for the `fauna.conversations.keypackage.{upload,count}` kinds —
//! the MLS key-package pool surface the Encryption-settings page drives.
//!
//! [`FfiConversationsClient`] wraps `fauna_client_conversations::ConversationsClient`
//! (which wraps the shared `NestClient`); it is the native-client twin of the
//! key-package pool the Rust-native Linux app reaches via `ConversationsClient`
//! directly — letting Apple / Windows / Android publish + count their own MLS key
//! packages over WS-RPC instead of the legacy `POST /api/v1/keypackage/{actor}` +
//! `GET /api/v1/keypackage/{actor}/count` HTTP twins (deleted at T8). No MLS
//! state client-side (`docs/goal/ui/conversations.md` rule #2) — the local engine
//! generates the raw key-package bytes; this just uploads them and reads the
//! nest's count.
//!
//! Scope is deliberately the pool only: the `keypackage.fetch` (FIFO consume) and
//! `welcome` / `channel` / `group` kinds are owned by the shared
//! `ConversationsManager` rail backend (`crate::conversations`), not this seam.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_conversations::ConversationsClient;
use fauna_client_conversations::conversations::{KeypackageCountReply, KeypackageUploadReply};

use crate::{FfiError, stringify};

/// FFI mirror of [`fauna_protocol::conversations::KeypackageCountReply`] — the
/// non-destructive count of remaining non-expired key packages for an actor. The
/// protocol type's forward-compat `extra` overflow has no counterpart here
/// (clients only consume `count`).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiKeypackageCountReply {
    pub count: u64,
}

impl From<KeypackageCountReply> for FfiKeypackageCountReply {
    fn from(r: KeypackageCountReply) -> Self {
        FfiKeypackageCountReply { count: r.count }
    }
}

/// FFI mirror of [`fauna_protocol::conversations::KeypackageUploadReply`] — how
/// many key packages this upload stored (the `extra` overflow is dropped).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiKeypackageUploadReply {
    pub stored: u64,
}

impl From<KeypackageUploadReply> for FfiKeypackageUploadReply {
    fn from(r: KeypackageUploadReply) -> Self {
        FfiKeypackageUploadReply { stored: r.stored }
    }
}

/// UniFFI handle for the `fauna.conversations.keypackage.{upload,count}` kinds.
/// Construct via [`crate::nest_client::FfiNestClient::conversations`]; the methods
/// are exposed to Swift as `async throws` and Kotlin as `suspend fun`. Thin
/// wrapper over the shared `fauna_client_conversations::ConversationsClient`.
#[derive(uniffi::Object)]
pub struct FfiConversationsClient {
    nest: Arc<NestClient>,
}

impl FfiConversationsClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> ConversationsClient<Arc<NestClient>> {
        ConversationsClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiConversationsClient {
    /// `fauna.conversations.keypackage.count` — non-destructive count of the
    /// remaining non-expired key packages for `actor_id` (hex `[u8; 32]`). Pure
    /// read; the Encryption-settings page passes its own actor id to surface the
    /// local pool depth.
    pub async fn keypackage_count(
        &self,
        actor_id: String,
    ) -> Result<FfiKeypackageCountReply, FfiError> {
        let reply = self
            .client()
            .keypackage_count(actor_id)
            .await
            .map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.conversations.keypackage.upload` — publish one or more MLS key
    /// packages on behalf of the calling actor (the connection's actor is
    /// implicit). `packages` carries the raw key-package bytes the local MLS
    /// engine generated. `last_resort` is `false` for the consumable one-time
    /// pool (the Encryption-settings top-up) and `true` only for the single
    /// reusable last-resort package published at onboarding.
    pub async fn keypackage_upload(
        &self,
        packages: Vec<Vec<u8>>,
        last_resort: bool,
    ) -> Result<FfiKeypackageUploadReply, FfiError> {
        let reply = self
            .client()
            .keypackage_upload(packages, last_resort)
            .await
            .map_err(stringify)?;
        Ok(reply.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_reply_mirror_maps_count() {
        let proto = KeypackageCountReply {
            count: 7,
            extra: Default::default(),
        };
        let ffi: FfiKeypackageCountReply = proto.into();
        assert_eq!(ffi, FfiKeypackageCountReply { count: 7 });
    }

    #[test]
    fn upload_reply_mirror_maps_stored() {
        let proto = KeypackageUploadReply {
            stored: 10,
            extra: Default::default(),
        };
        let ffi: FfiKeypackageUploadReply = proto.into();
        assert_eq!(ffi, FfiKeypackageUploadReply { stored: 10 });
    }
}
