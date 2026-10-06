//! WS-RPC production impl of the page seam, over
//! `fauna_client_bridges::AtprotoSettingsClient` (the shared
//! `fauna.bridges.atproto.*` typed-call surface). This is the
//! directive-correct (`no-http-ws-rpc-everywhere`) consumer — no HTTP.
//!
//! Mirrors `fauna_labeler_catalog_machine::nest_api::ws_rpc`: a generic
//! [`WsRpcAtprotoSettingsNest<R>`] holds the kind-composition + wire→snapshot
//! transcription + error mapping once (priority #2); the per-target concrete
//! trait impls (native `Arc<NestClient>`, wasm `WsRpcClient`) and the
//! `build_atproto_settings_machine` constructors live in the `cfg`-gated
//! submodules below and just delegate.

use std::sync::Arc;

use fauna_client_bridges::{AtprotoSettingsClient, BridgesClient};
use fauna_client_capabilities::rpc::CapabilitiesClient;
use fauna_protocol::{RpcErrorClass, RpcRequester};

use super::{
    AtprotoSettingsApiError, AtprotoSettingsNestApi, AuthoringDelegationState, CredentialListing,
    DeletePresenceOutcome, IntegrationStatus, LinkSummary, NestConsentRow, NestCredentialRow,
    NestIdentitySummary, RevokeOutcome,
};
use crate::machine::AtprotoSettingsMachine;
use crate::observer::AtprotoSettingsObserver;
use crate::snapshots::AtprotoSessionRow;
use fauna_protocol::atproto_pds::AtprotoGrantInfo;

/// Generic WS-RPC seam over any [`RpcRequester`]. Native binds
/// `R = Arc<NestClient>`, wasm `R = WsRpcClient`; the per-target trait impls
/// below delegate to these inherent methods so the logic is written once.
///
/// Carries two typed-call surfaces over the one connection: the
/// `fauna.bridges.atproto.*` page kinds, and the plain `fauna.bridges.*`
/// surface the consume-side link summary is read from (the machine composes
/// that existing surface; it does not re-own link state).
pub struct WsRpcAtprotoSettingsNest<R: RpcRequester> {
    atproto: AtprotoSettingsClient<R>,
    bridges: BridgesClient<R>,
    /// `fauna.capabilities.{mint,revoke}` — the consent-time grant's deposit
    /// and withdrawal (`crate::consent_grant`).
    capabilities: CapabilitiesClient<R>,
}

// `AtprotoSettingsNestApi` requires `Debug`, but the client isn't `Debug`; the
// requester carries no renderable state, so a name-only impl satisfies the bound.
impl<R: RpcRequester> std::fmt::Debug for WsRpcAtprotoSettingsNest<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WsRpcAtprotoSettingsNest")
    }
}

impl<R> WsRpcAtprotoSettingsNest<R>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    pub fn new(nest: R) -> Self
    where
        R: Clone,
    {
        Self {
            atproto: AtprotoSettingsClient::new(nest.clone()),
            bridges: BridgesClient::new(nest.clone()),
            capabilities: CapabilitiesClient::new(nest),
        }
    }

    async fn do_list_app_credentials(&self) -> Result<CredentialListing, AtprotoSettingsApiError> {
        let reply = self.atproto.list_app_credentials().await.map_err(map_err)?;
        Ok(CredentialListing {
            credentials: reply
                .credentials
                .into_iter()
                .map(|c| NestCredentialRow {
                    credential_id: c.credential_id,
                    label: c.label,
                    dm_allowed: c.dm_allowed,
                    created_at_millis: c.created_at,
                    last_used_at_millis: c.last_used_at,
                })
                .collect(),
            external_apps_enabled: reply.external_apps_enabled,
        })
    }

    async fn do_provision_app_credential(
        &self,
        credential_id: String,
        label: String,
        verifier: String,
        dm_allowed: bool,
    ) -> Result<(), AtprotoSettingsApiError> {
        self.atproto
            .provision_app_credential(credential_id, label, verifier, dm_allowed)
            .await
            .map(|_| ())
            .map_err(map_err)
    }

    async fn do_revoke_app_credential(
        &self,
        credential_id: String,
    ) -> Result<RevokeOutcome, AtprotoSettingsApiError> {
        let reply = self
            .atproto
            .revoke_app_credential(credential_id)
            .await
            .map_err(map_err)?;
        Ok(RevokeOutcome {
            revoked: reply.revoked,
            sessions_revoked: reply.sessions_revoked,
        })
    }

    async fn do_list_sessions(&self) -> Result<Vec<AtprotoSessionRow>, AtprotoSettingsApiError> {
        let reply = self.atproto.list_sessions().await.map_err(map_err)?;
        Ok(reply
            .sessions
            .into_iter()
            .map(|s| AtprotoSessionRow {
                session_id_hex: hex::encode(&s.session_id),
                plane: s.plane,
                credential_id: s.credential_id,
                client_note: s.client_note,
                created_at_millis: s.created_at,
                last_refreshed_at_millis: s.last_refreshed_at,
                // A row that came back from `list_sessions` HAS a live
                // session, so its expiry is real. `None` here is reserved for
                // the row `refresh` synthesizes from a grant with no session.
                expires_at_millis: Some(s.expires_at),
                // Joined by `refresh`, which reads both lists — this
                // transcription stays a pure per-row map.
                grant: None,
                // Likewise decided by `refresh`: a suspended row has no
                // session to arrive on this path at all.
                suspended: false,
            })
            .collect())
    }

    async fn do_list_grants(&self) -> Result<Vec<AtprotoGrantInfo>, AtprotoSettingsApiError> {
        let reply = self.atproto.list_grants().await.map_err(map_err)?;
        Ok(reply.grants)
    }

    async fn do_revoke_session(
        &self,
        session_id: Vec<u8>,
    ) -> Result<bool, AtprotoSettingsApiError> {
        self.atproto
            .revoke_session(session_id)
            .await
            .map(|r| r.revoked)
            .map_err(map_err)
    }

    async fn do_set_external_apps_enabled(
        &self,
        enabled: bool,
    ) -> Result<(), AtprotoSettingsApiError> {
        self.atproto
            .set_external_apps_enabled(enabled)
            .await
            .map(|_| ())
            .map_err(map_err)
    }

    async fn do_get_integration_status(
        &self,
    ) -> Result<IntegrationStatus, AtprotoSettingsApiError> {
        let reply = self
            .atproto
            .get_integration_status()
            .await
            .map_err(map_err)?;
        Ok(IntegrationStatus {
            level: reply.level,
            hosted_allowed: reply.hosted_allowed,
            handle_domain: reply.handle_domain,
            handle_preview: reply.handle_preview,
            identity: reply.identity.map(|i| NestIdentitySummary {
                handle: i.handle,
                method: i.method,
                status: i.status,
                tombstone_requested: i.tombstone_requested,
                did: i.did,
            }),
        })
    }

    async fn do_delete_presence(&self) -> Result<DeletePresenceOutcome, AtprotoSettingsApiError> {
        self.atproto
            .delete_presence()
            .await
            .map(|r| DeletePresenceOutcome {
                level: r.level,
                newly_deleted: r.newly_deleted,
            })
            .map_err(map_err)
    }

    async fn do_record_tombstone(&self, prev_cid: String) -> Result<bool, AtprotoSettingsApiError> {
        self.atproto
            .record_tombstone(prev_cid)
            .await
            .map(|r| r.newly_tombstoned)
            .map_err(map_err)
    }

    async fn do_request_tombstone(&self) -> Result<bool, AtprotoSettingsApiError> {
        self.atproto
            .request_tombstone()
            .await
            .map(|r| r.newly_requested)
            .map_err(map_err)
    }

    async fn do_set_integration_level(
        &self,
        target_level: String,
        did_method: String,
        user_rotation_pub_did_key: String,
        history_backfill: bool,
    ) -> Result<String, AtprotoSettingsApiError> {
        self.atproto
            .set_integration_level(
                target_level,
                did_method,
                user_rotation_pub_did_key,
                history_backfill,
            )
            .await
            .map(|r| r.level)
            .map_err(map_err)
    }

    async fn do_bluesky_link_status(&self) -> Result<Option<LinkSummary>, AtprotoSettingsApiError> {
        let reply = self.bridges.list().await.map_err(map_err)?;
        Ok(reply
            .bridges
            .into_iter()
            .find(|b| b.id == "bluesky")
            .filter(|b| b.linked)
            .map(|b| LinkSummary {
                display: b.identity.map(|i| i.display).unwrap_or_default(),
            }))
    }

    async fn do_fetch_authoring_delegation(
        &self,
    ) -> Result<AuthoringDelegationState, AtprotoSettingsApiError> {
        let reply = self
            .atproto
            .fetch_authoring_delegation()
            .await
            .map_err(map_err)?;
        Ok(AuthoringDelegationState {
            k_pub: reply.k_pub.and_then(|b| k_pub_array(b.into_vec())),
            cert: reply.cert.map(|b| b.into_vec()),
            last_used_at: reply.last_used_at,
        })
    }

    async fn do_fetch_authoring_key(&self) -> Result<[u8; 32], AtprotoSettingsApiError> {
        let reply = self.atproto.fetch_authoring_key().await.map_err(map_err)?;
        k_pub_array(reply.k_pub).ok_or_else(|| AtprotoSettingsApiError::Transient {
            detail: "the nest returned an authoring sub-key that is not 32 bytes".into(),
        })
    }

    async fn do_provision_authoring_delegation(
        &self,
        cert: Vec<u8>,
    ) -> Result<(), AtprotoSettingsApiError> {
        self.atproto
            .provision_authoring_delegation(cert)
            .await
            .map_err(map_err)
            .map(|_| ())
    }

    async fn do_revoke_authoring_delegation(&self) -> Result<bool, AtprotoSettingsApiError> {
        self.atproto
            .revoke_authoring_delegation()
            .await
            .map_err(map_err)
            .map(|r| r.revoked)
    }

    async fn do_list_pending_consents(
        &self,
    ) -> Result<Vec<NestConsentRow>, AtprotoSettingsApiError> {
        let reply = self
            .atproto
            .list_pending_consents()
            .await
            .map_err(map_err)?;
        // A pure transcription — the wire's order (oldest first) is kept, and
        // the scopes cross as their wire spellings. Their *wording* is the
        // machine's to derive; see `NestConsentRow`.
        Ok(reply
            .consents
            .into_iter()
            .map(NestConsentRow::from)
            .collect())
    }

    async fn do_resolve_consent(
        &self,
        consent_id: Vec<u8>,
        approved: bool,
    ) -> Result<bool, AtprotoSettingsApiError> {
        self.atproto
            .resolve_consent(consent_id, approved)
            .await
            .map(|r| r.resolved)
            .map_err(map_err)
    }

    async fn do_mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), AtprotoSettingsApiError> {
        self.capabilities
            .mint(grant_blob)
            .await
            .map(|_| ())
            .map_err(map_err)
    }

    async fn do_revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), AtprotoSettingsApiError> {
        self.capabilities
            .revoke(grant_id)
            .await
            .map(|_| ())
            .map_err(map_err)
    }
}

/// A wire `Vec<u8>` narrowed to the 32-byte Ed25519 public key the cert must
/// name. A wrong length is dropped rather than padded — a cert minted over a
/// truncated key would be refused by the nest's check 3 anyway, and silently
/// reshaping key material is how the wrong key gets signed over.
fn k_pub_array(bytes: Vec<u8>) -> Option<[u8; 32]> {
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

fauna_core::map_rpc_error! {
    /// Map a transport `R::Error` onto [`AtprotoSettingsApiError`], keyed on the
    /// WS-RPC `RpcError.code` suffix. A transport fault (the request never reached
    /// a server rejection) is `Transient`. Mirrors
    /// `fauna_labeler_catalog_machine::nest_api::ws_rpc::map_err`.
    fn map_err(e) -> AtprotoSettingsApiError {
        "not_found" => NotFound,
    }
}

// ── Native (`Arc<NestClient>`) ──────────────────────────────────────────────
#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use async_trait::async_trait;
    use fauna_client::NestClient;
    use fauna_core::identity::ActorKeypair;

    #[async_trait]
    impl AtprotoSettingsNestApi for WsRpcAtprotoSettingsNest<Arc<NestClient>> {
        async fn list_app_credentials(&self) -> Result<CredentialListing, AtprotoSettingsApiError> {
            self.do_list_app_credentials().await
        }
        async fn provision_app_credential(
            &self,
            credential_id: String,
            label: String,
            verifier: String,
            dm_allowed: bool,
        ) -> Result<(), AtprotoSettingsApiError> {
            self.do_provision_app_credential(credential_id, label, verifier, dm_allowed)
                .await
        }
        async fn revoke_app_credential(
            &self,
            credential_id: String,
        ) -> Result<RevokeOutcome, AtprotoSettingsApiError> {
            self.do_revoke_app_credential(credential_id).await
        }
        async fn list_sessions(&self) -> Result<Vec<AtprotoSessionRow>, AtprotoSettingsApiError> {
            self.do_list_sessions().await
        }

        async fn list_grants(&self) -> Result<Vec<AtprotoGrantInfo>, AtprotoSettingsApiError> {
            self.do_list_grants().await
        }
        async fn revoke_session(
            &self,
            session_id: Vec<u8>,
        ) -> Result<bool, AtprotoSettingsApiError> {
            self.do_revoke_session(session_id).await
        }
        async fn set_external_apps_enabled(
            &self,
            enabled: bool,
        ) -> Result<(), AtprotoSettingsApiError> {
            self.do_set_external_apps_enabled(enabled).await
        }
        async fn get_integration_status(
            &self,
        ) -> Result<IntegrationStatus, AtprotoSettingsApiError> {
            self.do_get_integration_status().await
        }
        async fn set_integration_level(
            &self,
            target_level: String,
            did_method: String,
            user_rotation_pub_did_key: String,
            history_backfill: bool,
        ) -> Result<String, AtprotoSettingsApiError> {
            self.do_set_integration_level(
                target_level,
                did_method,
                user_rotation_pub_did_key,
                history_backfill,
            )
            .await
        }
        async fn delete_presence(&self) -> Result<DeletePresenceOutcome, AtprotoSettingsApiError> {
            self.do_delete_presence().await
        }
        async fn record_tombstone(
            &self,
            prev_cid: String,
        ) -> Result<bool, AtprotoSettingsApiError> {
            self.do_record_tombstone(prev_cid).await
        }
        async fn request_tombstone(&self) -> Result<bool, AtprotoSettingsApiError> {
            self.do_request_tombstone().await
        }
        async fn bluesky_link_status(
            &self,
        ) -> Result<Option<LinkSummary>, AtprotoSettingsApiError> {
            self.do_bluesky_link_status().await
        }
        async fn fetch_authoring_delegation(
            &self,
        ) -> Result<AuthoringDelegationState, AtprotoSettingsApiError> {
            self.do_fetch_authoring_delegation().await
        }
        async fn fetch_authoring_key(&self) -> Result<[u8; 32], AtprotoSettingsApiError> {
            self.do_fetch_authoring_key().await
        }
        async fn provision_authoring_delegation(
            &self,
            cert: Vec<u8>,
        ) -> Result<(), AtprotoSettingsApiError> {
            self.do_provision_authoring_delegation(cert).await
        }
        async fn revoke_authoring_delegation(&self) -> Result<bool, AtprotoSettingsApiError> {
            self.do_revoke_authoring_delegation().await
        }
        async fn list_pending_consents(
            &self,
        ) -> Result<Vec<NestConsentRow>, AtprotoSettingsApiError> {
            self.do_list_pending_consents().await
        }
        async fn resolve_consent(
            &self,
            consent_id: Vec<u8>,
            approved: bool,
        ) -> Result<bool, AtprotoSettingsApiError> {
            self.do_resolve_consent(consent_id, approved).await
        }
        async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), AtprotoSettingsApiError> {
            self.do_mint_grant(grant_blob).await
        }
        async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), AtprotoSettingsApiError> {
            self.do_revoke_grant(grant_id).await
        }
    }

    /// Build an [`AtprotoSettingsMachine`] over `nest`'s authenticated WS-RPC
    /// connection. The native entry the linux app + `fauna-ffi` call.
    ///
    /// `keypair` is the caller's actor keypair — the identity the D10
    /// authoring delegation is signed under. The machine persists nothing of
    /// its own: the identity custody and the minted credential
    /// secrets are account-plane kinds, reached through the seams every host
    /// wires after construction (`set_identity_store`, `set_credential_store`).
    ///
    /// `alerts` is the app-wide critical-alerts registry the S4-C custody
    /// alarm posts to; `None` on platforms whose shell doesn't render the
    /// banner yet.
    pub fn build_atproto_settings_machine(
        nest: Arc<NestClient>,
        keypair: ActorKeypair,
        observer: Arc<dyn AtprotoSettingsObserver>,
        alerts: Option<Arc<fauna_client_alerts::CriticalAlerts>>,
    ) -> Arc<AtprotoSettingsMachine> {
        // The D10 authoring delegation is signed under the account's identity
        // key, which IS this actor keypair: the nest's check 2 refuses a cert
        // whose grantor is not the calling actor. Wired here rather than per
        // shell so every native app is identity-capable at once — a machine
        // built without it refuses `authorize_external_apps` outright
        // (`error_no_identity`), which is the correct degradation for a shell
        // that genuinely has no key, but the wrong default for one that does.
        let api: Arc<dyn AtprotoSettingsNestApi> = Arc::new(WsRpcAtprotoSettingsNest::new(nest));
        AtprotoSettingsMachine::new_with_identity(
            observer,
            api,
            Arc::new(crate::custody::DirectoryGenesisVerifier),
            alerts,
            Some(keypair),
        )
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub use native::build_atproto_settings_machine;

// ── Wasm (`WsRpcClient`) ────────────────────────────────────────────────────
#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;
    use async_trait::async_trait;
    use fauna_core::identity::ActorKeypair;
    use fauna_rpc_wasm::WsRpcClient;

    #[async_trait(?Send)]
    impl AtprotoSettingsNestApi for WsRpcAtprotoSettingsNest<WsRpcClient> {
        async fn list_app_credentials(&self) -> Result<CredentialListing, AtprotoSettingsApiError> {
            self.do_list_app_credentials().await
        }
        async fn provision_app_credential(
            &self,
            credential_id: String,
            label: String,
            verifier: String,
            dm_allowed: bool,
        ) -> Result<(), AtprotoSettingsApiError> {
            self.do_provision_app_credential(credential_id, label, verifier, dm_allowed)
                .await
        }
        async fn revoke_app_credential(
            &self,
            credential_id: String,
        ) -> Result<RevokeOutcome, AtprotoSettingsApiError> {
            self.do_revoke_app_credential(credential_id).await
        }
        async fn list_sessions(&self) -> Result<Vec<AtprotoSessionRow>, AtprotoSettingsApiError> {
            self.do_list_sessions().await
        }

        async fn list_grants(&self) -> Result<Vec<AtprotoGrantInfo>, AtprotoSettingsApiError> {
            self.do_list_grants().await
        }
        async fn revoke_session(
            &self,
            session_id: Vec<u8>,
        ) -> Result<bool, AtprotoSettingsApiError> {
            self.do_revoke_session(session_id).await
        }
        async fn set_external_apps_enabled(
            &self,
            enabled: bool,
        ) -> Result<(), AtprotoSettingsApiError> {
            self.do_set_external_apps_enabled(enabled).await
        }
        async fn get_integration_status(
            &self,
        ) -> Result<IntegrationStatus, AtprotoSettingsApiError> {
            self.do_get_integration_status().await
        }
        async fn set_integration_level(
            &self,
            target_level: String,
            did_method: String,
            user_rotation_pub_did_key: String,
            history_backfill: bool,
        ) -> Result<String, AtprotoSettingsApiError> {
            self.do_set_integration_level(
                target_level,
                did_method,
                user_rotation_pub_did_key,
                history_backfill,
            )
            .await
        }
        async fn delete_presence(&self) -> Result<DeletePresenceOutcome, AtprotoSettingsApiError> {
            self.do_delete_presence().await
        }
        async fn record_tombstone(
            &self,
            prev_cid: String,
        ) -> Result<bool, AtprotoSettingsApiError> {
            self.do_record_tombstone(prev_cid).await
        }
        async fn request_tombstone(&self) -> Result<bool, AtprotoSettingsApiError> {
            self.do_request_tombstone().await
        }
        async fn bluesky_link_status(
            &self,
        ) -> Result<Option<LinkSummary>, AtprotoSettingsApiError> {
            self.do_bluesky_link_status().await
        }
        async fn fetch_authoring_delegation(
            &self,
        ) -> Result<AuthoringDelegationState, AtprotoSettingsApiError> {
            self.do_fetch_authoring_delegation().await
        }
        async fn fetch_authoring_key(&self) -> Result<[u8; 32], AtprotoSettingsApiError> {
            self.do_fetch_authoring_key().await
        }
        async fn provision_authoring_delegation(
            &self,
            cert: Vec<u8>,
        ) -> Result<(), AtprotoSettingsApiError> {
            self.do_provision_authoring_delegation(cert).await
        }
        async fn revoke_authoring_delegation(&self) -> Result<bool, AtprotoSettingsApiError> {
            self.do_revoke_authoring_delegation().await
        }
        async fn list_pending_consents(
            &self,
        ) -> Result<Vec<NestConsentRow>, AtprotoSettingsApiError> {
            self.do_list_pending_consents().await
        }
        async fn resolve_consent(
            &self,
            consent_id: Vec<u8>,
            approved: bool,
        ) -> Result<bool, AtprotoSettingsApiError> {
            self.do_resolve_consent(consent_id, approved).await
        }
        async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), AtprotoSettingsApiError> {
            self.do_mint_grant(grant_blob).await
        }
        async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), AtprotoSettingsApiError> {
            self.do_revoke_grant(grant_id).await
        }
    }

    /// Wasm twin of the native builder — the entry the web SPA calls through
    /// `libs/fauna-wasm`.
    pub fn build_atproto_settings_machine(
        nest: WsRpcClient,
        keypair: ActorKeypair,
        observer: Arc<dyn AtprotoSettingsObserver>,
        alerts: Option<Arc<fauna_client_alerts::CriticalAlerts>>,
    ) -> Arc<AtprotoSettingsMachine> {
        // Identity wiring, same reasoning as the native twin above.
        let api: Arc<dyn AtprotoSettingsNestApi> = Arc::new(WsRpcAtprotoSettingsNest::new(nest));
        AtprotoSettingsMachine::new_with_identity(
            observer,
            api,
            Arc::new(crate::custody::DirectoryGenesisVerifier),
            alerts,
            Some(keypair),
        )
    }
}
#[cfg(target_arch = "wasm32")]
pub use wasm::build_atproto_settings_machine;
