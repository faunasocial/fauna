//! WS-RPC production impl of the page seam. A generic
//! [`WsRpcConnectedAppsNest<R>`] holds the calls + error mapping once
//! (priority #2); the per-target trait impls (native `Arc<NestClient>`, wasm
//! `WsRpcClient`) and `build_connected_apps_machine` delegate to it. Mirrors
//! `fauna_labeler_catalog_machine::nest_api::ws_rpc`.
//!
//! The mail app passwords are not a nest list: they are the session's
//! `MailSettingsMachine`, which this seam asks rather than folding the mail
//! custody a second time.

use std::sync::Arc;

use fauna_atproto_settings_machine::NestConsentRow;
use fauna_client_bridges::AtprotoSettingsClient;
use fauna_client_capabilities::rpc::CapabilitiesClient;
use fauna_client_mail_settings::{
    DispatchError, MailCredentialSummary, MailSettingsAction, MailSettingsMachine,
};
use fauna_client_nostr::NostrBunkerClient;
use fauna_core::secret::SecretString;
use fauna_protocol::atproto_pds::{AtprotoGrantInfo, AtprotoSessionInfo};
use fauna_protocol::nostr::BunkerAppEntry;
use fauna_protocol::oauth_consent::BlockedClient;
use fauna_protocol::principals::PrincipalInfo;
use fauna_protocol::{RpcErrorClass, RpcRequester};

use super::{ConnectedAppsApiError, ConnectedAppsNestApi};
use crate::client::ConnectedAppsClient;
use crate::machine::ConnectedAppsMachine;
use crate::observer::ConnectedAppsObserver;

/// Generic WS-RPC seam over any cloneable [`RpcRequester`].
pub struct WsRpcConnectedAppsNest<R: RpcRequester> {
    apps: ConnectedAppsClient<R>,
    atproto: AtprotoSettingsClient<R>,
    bunker: NostrBunkerClient<R>,
    /// `fauna.capabilities.{mint,revoke}` — the consent-time grant's deposit
    /// and withdrawal.
    capabilities: CapabilitiesClient<R>,
    /// The session's mail-settings machine. `None` on a session that built
    /// none: the roster then simply carries no mail rows.
    mail: Option<Arc<MailSettingsMachine>>,
}

impl<R: RpcRequester> std::fmt::Debug for WsRpcConnectedAppsNest<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WsRpcConnectedAppsNest")
    }
}

impl<R> WsRpcConnectedAppsNest<R>
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    pub fn new(nest: R, mail: Option<Arc<MailSettingsMachine>>) -> Self {
        Self {
            apps: ConnectedAppsClient::new(nest.clone()),
            atproto: AtprotoSettingsClient::new(nest.clone()),
            capabilities: CapabilitiesClient::new(nest.clone()),
            bunker: NostrBunkerClient::new(nest),
            mail,
        }
    }

    async fn do_list_principals(&self) -> Result<Vec<PrincipalInfo>, ConnectedAppsApiError> {
        Ok(self
            .apps
            .list_principals()
            .await
            .map_err(map_roster_err)?
            .principals)
    }
    async fn do_revoke_principal(&self, id: Vec<u8>) -> Result<bool, ConnectedAppsApiError> {
        Ok(self
            .apps
            .revoke_principal(id)
            .await
            .map_err(map_err)?
            .revoked)
    }
    async fn do_list_pending_consents(&self) -> Result<Vec<NestConsentRow>, ConnectedAppsApiError> {
        let reply = self
            .atproto
            .list_pending_consents()
            .await
            .map_err(map_err)?;
        Ok(reply
            .consents
            .into_iter()
            .map(NestConsentRow::from)
            .collect())
    }
    async fn do_resolve_consent(
        &self,
        id: Vec<u8>,
        approved: bool,
    ) -> Result<bool, ConnectedAppsApiError> {
        let reply = self
            .atproto
            .resolve_consent(id, approved)
            .await
            .map_err(map_err)?;
        Ok(reply.resolved)
    }
    async fn do_mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), ConnectedAppsApiError> {
        self.capabilities
            .mint(grant_blob)
            .await
            .map(|_| ())
            .map_err(map_roster_err)
    }
    async fn do_revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), ConnectedAppsApiError> {
        self.capabilities
            .revoke(grant_id)
            .await
            .map(|_| ())
            .map_err(map_roster_err)
    }
    async fn do_lookup_code(
        &self,
        code: String,
    ) -> Result<Option<NestConsentRow>, ConnectedAppsApiError> {
        let reply = self.apps.lookup_code(code).await.map_err(map_err)?;
        Ok(reply.consent.map(NestConsentRow::from))
    }
    async fn do_open_handoff(
        &self,
        request_uri: String,
    ) -> Result<Option<NestConsentRow>, ConnectedAppsApiError> {
        let reply = self.apps.open_handoff(request_uri).await.map_err(map_err)?;
        Ok(reply.consent.map(NestConsentRow::from))
    }
    async fn do_block_client(
        &self,
        client_id: String,
        blocked: bool,
    ) -> Result<bool, ConnectedAppsApiError> {
        let reply = self
            .apps
            .block_client(client_id, blocked)
            .await
            .map_err(map_err)?;
        Ok(reply.blocked)
    }
    async fn do_list_blocked_clients(&self) -> Result<Vec<BlockedClient>, ConnectedAppsApiError> {
        Ok(self
            .apps
            .list_blocked_clients()
            .await
            .map_err(map_err)?
            .clients)
    }
    async fn do_list_sessions(&self) -> Result<Vec<AtprotoSessionInfo>, ConnectedAppsApiError> {
        Ok(self
            .atproto
            .list_sessions()
            .await
            .map_err(map_err)?
            .sessions)
    }
    async fn do_list_grants(&self) -> Result<Vec<AtprotoGrantInfo>, ConnectedAppsApiError> {
        Ok(self.atproto.list_grants().await.map_err(map_err)?.grants)
    }
    async fn do_revoke_session(&self, id: Vec<u8>) -> Result<bool, ConnectedAppsApiError> {
        Ok(self
            .atproto
            .revoke_session(id)
            .await
            .map_err(map_err)?
            .revoked)
    }
    async fn do_list_bunker_apps(&self) -> Result<Vec<BunkerAppEntry>, ConnectedAppsApiError> {
        self.bunker.list().await.map_err(map_err)
    }
    async fn do_revoke_bunker_app(&self, id: i64) -> Result<bool, ConnectedAppsApiError> {
        self.bunker.revoke(id).await.map_err(map_err)
    }
    /// Re-read the mail machine's rows and hand them over, under the same gate
    /// the mail-settings page lists them on
    /// (`credential_management_reachable`). Only the rows: this runs on every
    /// visit and every consent push, so it makes no nest call and never heals —
    /// that is the mail page's own hydrate.
    async fn do_list_mail_credentials(
        &self,
    ) -> Result<Vec<MailCredentialSummary>, ConnectedAppsApiError> {
        let Some(mail) = &self.mail else {
            return Ok(Vec::new());
        };
        mail.refresh_credentials().await.map_err(map_mail_err)?;
        let snapshot = mail.snapshot();
        Ok(if snapshot.credential_management_reachable {
            snapshot.credentials
        } else {
            Vec::new()
        })
    }
    async fn do_revoke_mail_credential(&self, id: String) -> Result<bool, ConnectedAppsApiError> {
        let Some(mail) = &self.mail else {
            return Ok(false);
        };
        mail.dispatch(MailSettingsAction::RevokeCredential { credential_id: id })
            .await
            .map_err(map_mail_err)?;
        Ok(true)
    }
    async fn do_reveal_mail_secret(
        &self,
        id: String,
    ) -> Result<SecretString, ConnectedAppsApiError> {
        let Some(mail) = &self.mail else {
            return Err(ConnectedAppsApiError::NotFound {
                detail: "no mail machine in this session".into(),
            });
        };
        mail.reveal_credential_secret(id)
            .await
            .map_err(map_mail_err)
    }
}

/// A mail-machine failure as this page's error. The text is the
/// `DispatchError`'s own — it never contains a secret.
fn map_mail_err(e: DispatchError) -> ConnectedAppsApiError {
    let detail = e.to_string();
    match e {
        DispatchError::UnknownCredential(_) => ConnectedAppsApiError::NotFound { detail },
        _ => ConnectedAppsApiError::Transient { detail },
    }
}

fauna_core::map_rpc_error! {
    /// Map a transport `R::Error` onto [`ConnectedAppsApiError`], keyed on the
    /// WS-RPC `RpcError.code` suffix. `unknown_kind` is a surface the nest was built without (the nostr bunker) and a
    /// permission refusal is a surface this account does not have — both let
    /// the page render what it can, without an error.
    fn map_err(e) -> ConnectedAppsApiError {
        "not_found" => NotFound,
        "unknown_kind" | "forbidden" | "permission_denied" => Unavailable,
    }
}

fauna_core::map_rpc_error! {
    /// [`map_err`] for the principal roster, which every nest serves: its
    /// `unknown_kind` is an ordinary refusal the page surfaces (`Transient`),
    /// never an absent surface.
    fn map_roster_err(e) -> ConnectedAppsApiError {
        "not_found" => NotFound,
        "forbidden" | "permission_denied" => Unavailable,
    }
}

// ── Native (`Arc<NestClient>`) ──────────────────────────────────────────────
#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use fauna_client::NestClient;

    #[async_trait::async_trait]
    impl ConnectedAppsNestApi for WsRpcConnectedAppsNest<Arc<NestClient>> {
        async fn list_principals(&self) -> Result<Vec<PrincipalInfo>, ConnectedAppsApiError> {
            self.do_list_principals().await
        }
        async fn revoke_principal(&self, id: Vec<u8>) -> Result<bool, ConnectedAppsApiError> {
            self.do_revoke_principal(id).await
        }
        async fn list_pending_consents(
            &self,
        ) -> Result<Vec<NestConsentRow>, ConnectedAppsApiError> {
            self.do_list_pending_consents().await
        }
        async fn resolve_consent(
            &self,
            id: Vec<u8>,
            approved: bool,
        ) -> Result<bool, ConnectedAppsApiError> {
            self.do_resolve_consent(id, approved).await
        }
        async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), ConnectedAppsApiError> {
            self.do_mint_grant(grant_blob).await
        }
        async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), ConnectedAppsApiError> {
            self.do_revoke_grant(grant_id).await
        }
        async fn lookup_code(
            &self,
            code: String,
        ) -> Result<Option<NestConsentRow>, ConnectedAppsApiError> {
            self.do_lookup_code(code).await
        }
        async fn open_handoff(
            &self,
            request_uri: String,
        ) -> Result<Option<NestConsentRow>, ConnectedAppsApiError> {
            self.do_open_handoff(request_uri).await
        }
        async fn block_client(
            &self,
            client_id: String,
            blocked: bool,
        ) -> Result<bool, ConnectedAppsApiError> {
            self.do_block_client(client_id, blocked).await
        }
        async fn list_blocked_clients(&self) -> Result<Vec<BlockedClient>, ConnectedAppsApiError> {
            self.do_list_blocked_clients().await
        }
        async fn list_sessions(&self) -> Result<Vec<AtprotoSessionInfo>, ConnectedAppsApiError> {
            self.do_list_sessions().await
        }
        async fn list_grants(&self) -> Result<Vec<AtprotoGrantInfo>, ConnectedAppsApiError> {
            self.do_list_grants().await
        }
        async fn revoke_session(&self, id: Vec<u8>) -> Result<bool, ConnectedAppsApiError> {
            self.do_revoke_session(id).await
        }
        async fn list_bunker_apps(&self) -> Result<Vec<BunkerAppEntry>, ConnectedAppsApiError> {
            self.do_list_bunker_apps().await
        }
        async fn revoke_bunker_app(&self, id: i64) -> Result<bool, ConnectedAppsApiError> {
            self.do_revoke_bunker_app(id).await
        }
        async fn list_mail_credentials(
            &self,
        ) -> Result<Vec<MailCredentialSummary>, ConnectedAppsApiError> {
            self.do_list_mail_credentials().await
        }
        async fn revoke_mail_credential(&self, id: String) -> Result<bool, ConnectedAppsApiError> {
            self.do_revoke_mail_credential(id).await
        }
        async fn reveal_mail_secret(
            &self,
            id: String,
        ) -> Result<SecretString, ConnectedAppsApiError> {
            self.do_reveal_mail_secret(id).await
        }
    }

    /// Build a [`ConnectedAppsMachine`] over `nest`'s authenticated WS-RPC
    /// connection and the session's mail-settings machine (`None` leaves the
    /// roster without mail rows).
    pub fn build_connected_apps_machine(
        nest: Arc<NestClient>,
        observer: Arc<dyn ConnectedAppsObserver>,
        mail: Option<Arc<MailSettingsMachine>>,
    ) -> Arc<ConnectedAppsMachine> {
        let api: Arc<dyn ConnectedAppsNestApi> = Arc::new(WsRpcConnectedAppsNest::new(nest, mail));
        ConnectedAppsMachine::new(observer, api)
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub use native::build_connected_apps_machine;

// ── Wasm (`WsRpcClient`) ────────────────────────────────────────────────────
#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;
    use fauna_rpc_wasm::WsRpcClient;

    #[async_trait::async_trait(?Send)]
    impl ConnectedAppsNestApi for WsRpcConnectedAppsNest<WsRpcClient> {
        async fn list_principals(&self) -> Result<Vec<PrincipalInfo>, ConnectedAppsApiError> {
            self.do_list_principals().await
        }
        async fn revoke_principal(&self, id: Vec<u8>) -> Result<bool, ConnectedAppsApiError> {
            self.do_revoke_principal(id).await
        }
        async fn list_pending_consents(
            &self,
        ) -> Result<Vec<NestConsentRow>, ConnectedAppsApiError> {
            self.do_list_pending_consents().await
        }
        async fn resolve_consent(
            &self,
            id: Vec<u8>,
            approved: bool,
        ) -> Result<bool, ConnectedAppsApiError> {
            self.do_resolve_consent(id, approved).await
        }
        async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), ConnectedAppsApiError> {
            self.do_mint_grant(grant_blob).await
        }
        async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), ConnectedAppsApiError> {
            self.do_revoke_grant(grant_id).await
        }
        async fn lookup_code(
            &self,
            code: String,
        ) -> Result<Option<NestConsentRow>, ConnectedAppsApiError> {
            self.do_lookup_code(code).await
        }
        async fn open_handoff(
            &self,
            request_uri: String,
        ) -> Result<Option<NestConsentRow>, ConnectedAppsApiError> {
            self.do_open_handoff(request_uri).await
        }
        async fn block_client(
            &self,
            client_id: String,
            blocked: bool,
        ) -> Result<bool, ConnectedAppsApiError> {
            self.do_block_client(client_id, blocked).await
        }
        async fn list_blocked_clients(&self) -> Result<Vec<BlockedClient>, ConnectedAppsApiError> {
            self.do_list_blocked_clients().await
        }
        async fn list_sessions(&self) -> Result<Vec<AtprotoSessionInfo>, ConnectedAppsApiError> {
            self.do_list_sessions().await
        }
        async fn list_grants(&self) -> Result<Vec<AtprotoGrantInfo>, ConnectedAppsApiError> {
            self.do_list_grants().await
        }
        async fn revoke_session(&self, id: Vec<u8>) -> Result<bool, ConnectedAppsApiError> {
            self.do_revoke_session(id).await
        }
        async fn list_bunker_apps(&self) -> Result<Vec<BunkerAppEntry>, ConnectedAppsApiError> {
            self.do_list_bunker_apps().await
        }
        async fn revoke_bunker_app(&self, id: i64) -> Result<bool, ConnectedAppsApiError> {
            self.do_revoke_bunker_app(id).await
        }
        async fn list_mail_credentials(
            &self,
        ) -> Result<Vec<MailCredentialSummary>, ConnectedAppsApiError> {
            self.do_list_mail_credentials().await
        }
        async fn revoke_mail_credential(&self, id: String) -> Result<bool, ConnectedAppsApiError> {
            self.do_revoke_mail_credential(id).await
        }
        async fn reveal_mail_secret(
            &self,
            id: String,
        ) -> Result<SecretString, ConnectedAppsApiError> {
            self.do_reveal_mail_secret(id).await
        }
    }

    /// Build a [`ConnectedAppsMachine`] over the SPA's browser `WsRpcClient`
    /// and the session's mail-settings machine.
    pub fn build_connected_apps_machine(
        nest: WsRpcClient,
        observer: Arc<dyn ConnectedAppsObserver>,
        mail: Option<Arc<MailSettingsMachine>>,
    ) -> Arc<ConnectedAppsMachine> {
        let api: Arc<dyn ConnectedAppsNestApi> = Arc::new(WsRpcConnectedAppsNest::new(nest, mail));
        ConnectedAppsMachine::new(observer, api)
    }
}
#[cfg(target_arch = "wasm32")]
pub use wasm::build_connected_apps_machine;

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{CapturingRequester, block_on};

    #[test]
    fn a_roster_unknown_kind_is_a_surfaced_error() {
        let seam = WsRpcConnectedAppsNest::new(
            Arc::new(CapturingRequester::rejecting_code(
                "fauna.protocol.unknown_kind",
                "fauna.principals.list",
            )),
            None,
        );
        let err = block_on(seam.do_list_principals()).unwrap_err();
        assert!(
            matches!(err, ConnectedAppsApiError::Transient { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn a_bunker_unknown_kind_is_unavailable_not_an_error() {
        let seam = WsRpcConnectedAppsNest::new(
            Arc::new(CapturingRequester::rejecting_code(
                "fauna.protocol.unknown_kind",
                "fauna.nostr.bunker.list_apps",
            )),
            None,
        );
        let err = block_on(seam.do_list_bunker_apps()).unwrap_err();
        assert!(
            matches!(err, ConnectedAppsApiError::Unavailable { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn a_transport_fault_is_transient() {
        let seam =
            WsRpcConnectedAppsNest::new(Arc::new(CapturingRequester::transport_fault()), None);
        let err = block_on(seam.do_list_blocked_clients()).unwrap_err();
        assert!(
            matches!(err, ConnectedAppsApiError::Transient { .. }),
            "{err:?}"
        );
    }
}
