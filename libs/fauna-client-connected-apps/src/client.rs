//! Typed calls for the two USER-class kind families this page is the first
//! app surface of: `fauna.principals.*` (the roster read and its one verb) and
//! `fauna.oauth.consent.*` (the typed-code claim, the same-device handoff's
//! open and the per-client block).
//! Transport-agnostic over [`RpcRequester`], wasm-clean; errors propagate as the
//! transport's `R::Error`.

use fauna_protocol::RpcRequester;
use fauna_protocol::oauth_consent::{
    BlockClientReply, BlockClientRequest, ListBlockedClientsReply, ListBlockedClientsRequest,
    LookupConsentCodeReply, LookupConsentCodeRequest, OpenHandoffReply, OpenHandoffRequest,
};
use fauna_protocol::principals::{
    ListPrincipalsReply, ListPrincipalsRequest, RevokePrincipalReply, RevokePrincipalRequest,
};

pub struct ConnectedAppsClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> ConnectedAppsClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.principals.list` — the roster read, oldest first.
    pub async fn list_principals(&self) -> Result<ListPrincipalsReply, R::Error> {
        self.nest
            .request("fauna.principals.list", ListPrincipalsRequest::default())
            .await
    }

    /// `fauna.principals.revoke` — the one verb. Idempotent (`revoked: false`
    /// when the row is already gone).
    pub async fn revoke_principal(
        &self,
        principal_id: Vec<u8>,
    ) -> Result<RevokePrincipalReply, R::Error> {
        self.nest
            .request(
                "fauna.principals.revoke",
                RevokePrincipalRequest {
                    principal_id,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.oauth.consent.lookup_code` — claim the typed-code request whose
    /// code the user typed. `consent: None` is the one answer for every miss.
    pub async fn lookup_code(&self, user_code: String) -> Result<LookupConsentCodeReply, R::Error> {
        self.nest
            .request(
                "fauna.oauth.consent.lookup_code",
                LookupConsentCodeRequest {
                    user_code,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.oauth.consent.open_handoff` — open the same-device handoff's
    /// pending request for the PAR handle a `fauna://consent/<request_uri>`
    /// route carried, assigned to the caller. `consent: None` is the one
    /// answer for every miss.
    pub async fn open_handoff(&self, request_uri: String) -> Result<OpenHandoffReply, R::Error> {
        self.nest
            .request(
                "fauna.oauth.consent.open_handoff",
                OpenHandoffRequest {
                    request_uri,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.oauth.consent.block_client` — set (`true`) or lift (`false`) the
    /// caller's block on one client. Idempotent both ways.
    pub async fn block_client(
        &self,
        client_id: String,
        blocked: bool,
    ) -> Result<BlockClientReply, R::Error> {
        self.nest
            .request(
                "fauna.oauth.consent.block_client",
                BlockClientRequest {
                    client_id,
                    blocked,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.oauth.consent.list_blocked_clients` — the blocks, oldest first.
    pub async fn list_blocked_clients(&self) -> Result<ListBlockedClientsReply, R::Error> {
        self.nest
            .request(
                "fauna.oauth.consent.list_blocked_clients",
                ListBlockedClientsRequest::default(),
            )
            .await
    }
}
