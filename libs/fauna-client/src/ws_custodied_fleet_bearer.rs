//! [`WsCustodiedFleetBearer`] — a [`BearerSource`] that mints its bearer over
//! the pre-identity `fauna.auth.custodied_fleet_handshake` kind, signing with
//! an OWNER-FLEET device's principal key and presenting its root-signed
//! `DeviceAuthorization` inline. The fleet twin of
//! [`crate::ws_custody_handshake_bearer::WsCustodyHandshakeBearer`]: where that
//! one lets a custodian pull FROM an owner's nest, this one lets the owner's
//! own device read back what a CUSTODIAN nest holds for it — the
//! custodian-NEST serve door (`account-replica-posture.md` § Replica posture
//! → *The custody grant + ceremony*, the custodian-NEST serve door bullet).
//!
//! The minted session's actor is the device key and its whole authority is the
//! door's two reads (`fauna.sync.changes.list`, `fauna.segments.list`) plus the
//! segment byte pair, each re-derived nest-side from rows on every request — so
//! a removal, an expiry or the host's remove severs a LIVE session at its next
//! request, and this bearer needs no revocation plumbing of its own.

use std::sync::Arc;

use async_trait::async_trait;

use fauna_nest_http::{ApiError, BearerSource};

use crate::auth_client::{AuthClient, pinned_http_client};
use crate::client::NestClient;
use crate::token_cache::TokenCache;
use crate::ws_challenge_bearer::map_anon_err;

/// [`BearerSource`] minting over `fauna.auth.custodied_fleet_handshake`. Holds
/// the device's principal key and its `DeviceAuthorization` — never an
/// identity keypair (the session reads; it can act as no one).
pub struct WsCustodiedFleetBearer {
    nest_url: String,
    owner_actor_id: [u8; 32],
    device_signing_key: ed25519_dalek::SigningKey,
    authorization: fauna_core::encoding::EmbedAsBytes,
    token_cache: TokenCache,
}

impl WsCustodiedFleetBearer {
    pub fn new(
        nest_url: impl Into<String>,
        owner_actor_id: [u8; 32],
        device_signing_key: ed25519_dalek::SigningKey,
        authorization: fauna_core::encoding::EmbedAsBytes,
    ) -> Self {
        Self {
            nest_url: nest_url.into().trim_end_matches('/').to_string(),
            owner_actor_id,
            device_signing_key,
            authorization,
            token_cache: TokenCache::default(),
        }
    }

    /// One `fauna.auth.custodied_fleet_handshake` round trip — the
    /// [`TokenCache::bearer`] `fetch` callback.
    async fn fetch_mint(&self) -> Result<fauna_anon_client::MintedBearer, ApiError> {
        fauna_anon_client::mint_bearer_over_custodied_fleet_handshake(
            &self.nest_url,
            self.owner_actor_id,
            &self.device_signing_key,
            &self.authorization,
        )
        .await
        .map_err(|e| map_anon_err(e, &self.nest_url))
    }
}

#[async_trait]
impl BearerSource for WsCustodiedFleetBearer {
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

/// A [`NestClient`] whose whole auth lifecycle is the custodied-fleet
/// handshake — what an owner device dials a CUSTODIAN nest with to read back a
/// held copy (the URL from the owner's `custodian-endpoints` row; the cert is
/// the device's own). The session's actor is the device key: bearer-only, no
/// identity keypair anywhere in the client.
pub fn custodied_fleet_nest_client(
    nest_url: &str,
    owner_actor_id: [u8; 32],
    device_signing_key: ed25519_dalek::SigningKey,
    authorization: fauna_core::encoding::EmbedAsBytes,
) -> Arc<NestClient> {
    let device_key = device_signing_key.verifying_key().to_bytes();
    let bearer = Arc::new(WsCustodiedFleetBearer::new(
        nest_url,
        owner_actor_id,
        device_signing_key,
        authorization,
    ));
    let auth = Arc::new(AuthClient::bearer_only(
        nest_url.to_string(),
        device_key,
        bearer,
        pinned_http_client(nest_url),
    ));
    NestClient::with_auth(auth)
}
