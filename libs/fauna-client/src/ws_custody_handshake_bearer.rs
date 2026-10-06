//! [`WsCustodyHandshakeBearer`] — a [`BearerSource`] that mints its bearer
//! over the pre-identity `fauna.auth.custody_handshake` kind, signing with
//! the CUSTODIAN's device-principal key and presenting the custody-grant
//! witness inline. The custody sibling of
//! [`crate::ws_device_handshake_bearer::WsDeviceHandshakeBearer`], for the
//! process pulling a custodied account's sealed planes from ITS OWNER'S
//! nest — where the custodian holds no account credential at all (W8.6 (account-data-plane.md § Workstreams),
//! `account-data-plane.md` § Replica posture → *The custody grant +
//! ceremony*).
//!
//! The minted session's actor is the custodian key itself and its whole
//! authority is `fauna.sync.changes.list` with per-request custody-row
//! re-checks nest-side — `fauna.capabilities.revoke` severs a LIVE session
//! at its next request, so this bearer needs (and has) no revocation
//! plumbing of its own: it just keeps failing typed and the pull loop
//! reports and retries.

use std::sync::Arc;

use async_trait::async_trait;

use fauna_nest_http::{ApiError, BearerSource};

use crate::auth_client::{AuthClient, pinned_http_client};
use crate::client::NestClient;
use crate::token_cache::TokenCache;
use crate::ws_challenge_bearer::map_anon_err;

/// [`BearerSource`] minting over `fauna.auth.custody_handshake`. Holds the
/// custodian's device key + the owner-signed witness — never any identity
/// keypair (the custodian cannot act as anyone; the nest's allowlist and the
/// per-request row re-check are the authority model).
pub struct WsCustodyHandshakeBearer {
    nest_url: String,
    owner_actor_id: [u8; 32],
    custodian_signing_key: ed25519_dalek::SigningKey,
    witness: fauna_core::encoding::EmbedAsBytes,
    token_cache: TokenCache,
}

impl WsCustodyHandshakeBearer {
    pub fn new(
        nest_url: impl Into<String>,
        owner_actor_id: [u8; 32],
        custodian_signing_key: ed25519_dalek::SigningKey,
        witness: fauna_core::encoding::EmbedAsBytes,
    ) -> Self {
        Self {
            nest_url: nest_url.into().trim_end_matches('/').to_string(),
            owner_actor_id,
            custodian_signing_key,
            witness,
            token_cache: TokenCache::default(),
        }
    }

    /// One `fauna.auth.custody_handshake` round trip — the
    /// [`TokenCache::bearer`] `fetch` callback.
    async fn fetch_mint(&self) -> Result<fauna_anon_client::MintedBearer, ApiError> {
        fauna_anon_client::mint_bearer_over_custody_handshake(
            &self.nest_url,
            self.owner_actor_id,
            &self.custodian_signing_key,
            &self.witness,
        )
        .await
        .map_err(|e| map_anon_err(e, &self.nest_url))
    }
}

#[async_trait]
impl BearerSource for WsCustodyHandshakeBearer {
    async fn bearer(&self) -> Result<String, ApiError> {
        self.token_cache.bearer(|| self.fetch_mint()).await
    }

    async fn bearer_with_expiry(&self) -> Result<(String, Option<u64>), ApiError> {
        let (token, expires_at) = self
            .token_cache
            .bearer_with_expiry(|| self.fetch_mint())
            .await?;
        Ok((token, Some(expires_at)))
    }

    async fn notify_401(&self) {
        self.token_cache.clear().await;
    }

    /// The cache is the holder, so its set is this source's set
    /// (`docs/goal/behavior/devices.md` § The client's own session).
    async fn own_token_ids(&self) -> Vec<String> {
        self.token_cache.own_token_ids().await
    }

    async fn current_token_id(&self) -> Option<String> {
        self.token_cache.current_token_id().await
    }
}

/// A [`NestClient`] whose whole auth lifecycle is the custody handshake —
/// what the custodian's pull loop dials an OWNER's nest with (URL from the
/// `custodies-held` row's `owner_nest_url`; witness from the same row). The
/// session's actor is the custodian key: bearer-only, no identity keypair
/// anywhere in the client.
pub fn custody_nest_client(
    nest_url: &str,
    owner_actor_id: [u8; 32],
    custodian_signing_key: ed25519_dalek::SigningKey,
    witness: fauna_core::encoding::EmbedAsBytes,
) -> Arc<NestClient> {
    let custodian_key = custodian_signing_key.verifying_key().to_bytes();
    let bearer = Arc::new(WsCustodyHandshakeBearer::new(
        nest_url,
        owner_actor_id,
        custodian_signing_key,
        witness,
    ));
    let auth = Arc::new(AuthClient::bearer_only(
        nest_url.to_string(),
        custodian_key,
        bearer,
        pinned_http_client(nest_url),
    ));
    NestClient::with_auth(auth)
}
