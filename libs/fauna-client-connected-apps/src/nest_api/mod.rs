//! Everything the page reads and writes, as one seam. Production is the WS-RPC
//! [`ws_rpc::WsRpcConnectedAppsNest`] over the typed clients
//! (`ConnectedAppsClient`, `fauna_client_bridges::AtprotoSettingsClient`,
//! `fauna_client_nostr::NostrBunkerClient`) plus the session's
//! `MailSettingsMachine` for the mail app passwords; tests use
//! [`FakeConnectedAppsNestApi`].
//!
//! Rows cross the seam RAW — the wire's own types, the ATProto settings
//! machine's raw [`NestConsentRow`], the mail machine's own
//! [`MailCredentialSummary`] — so no seam implementation can supply a wording
//! or a revoke verb: both are the machine's.

pub mod fake;
#[cfg(feature = "rpc-glue")]
pub mod ws_rpc;

#[cfg(any(test, feature = "test-helpers"))]
pub use fake::{FakeCall, FakeConnectedAppsNestApi};
#[cfg(feature = "rpc-glue")]
pub use ws_rpc::build_connected_apps_machine;

use fauna_atproto_settings_machine::NestConsentRow;
use fauna_client_mail_settings::MailCredentialSummary;
use fauna_core::secret::SecretString;
use fauna_protocol::atproto_pds::{AtprotoGrantInfo, AtprotoSessionInfo};
use fauna_protocol::nostr::BunkerAppEntry;
use fauna_protocol::oauth_consent::BlockedClient;
use fauna_protocol::principals::PrincipalInfo;

fauna_core::declare_api_error!(
    /// Failure of one nest call on this page.
    ConnectedAppsApiError {
        /// The row a gesture named no longer exists.
        NotFound,
        /// The nest does not answer this kind (`unknown_kind` — a nest built without the `nostr` feature),
        /// or the surface behind it is off for this account. The page renders
        /// what it can read without an error — additive evolution
        /// (`connected-apps.md` § Errors & edge cases).
        Unavailable,
        /// Transport fault / 5xx / any other refusal — retryable.
        Transient,
    }
);

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait ConnectedAppsNestApi: fauna_core::MaybeSendSync + std::fmt::Debug {
    /// `fauna.principals.list`.
    async fn list_principals(&self) -> Result<Vec<PrincipalInfo>, ConnectedAppsApiError>;
    /// `fauna.principals.revoke` — `false` when already gone.
    async fn revoke_principal(&self, principal_id: Vec<u8>) -> Result<bool, ConnectedAppsApiError>;
    /// `fauna.bridges.atproto.list_pending_consents`.
    async fn list_pending_consents(&self) -> Result<Vec<NestConsentRow>, ConnectedAppsApiError>;
    /// `fauna.bridges.atproto.resolve_consent` — `false` when nothing live was
    /// there to resolve.
    async fn resolve_consent(
        &self,
        consent_id: Vec<u8>,
        approved: bool,
    ) -> Result<bool, ConnectedAppsApiError>;
    /// `fauna.capabilities.mint` — deposit the consent-time grant an approve
    /// of a records consent prepared, before the resolve.
    async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), ConnectedAppsApiError>;
    /// `fauna.capabilities.revoke` — withdraw a deposited consent grant whose
    /// resolve did not land.
    async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), ConnectedAppsApiError>;
    /// `fauna.oauth.consent.lookup_code` — `None` for every miss.
    async fn lookup_code(
        &self,
        user_code: String,
    ) -> Result<Option<NestConsentRow>, ConnectedAppsApiError>;
    /// `fauna.oauth.consent.open_handoff` — `None` for every miss.
    async fn open_handoff(
        &self,
        request_uri: String,
    ) -> Result<Option<NestConsentRow>, ConnectedAppsApiError>;
    /// `fauna.oauth.consent.block_client`.
    async fn block_client(
        &self,
        client_id: String,
        blocked: bool,
    ) -> Result<bool, ConnectedAppsApiError>;
    /// `fauna.oauth.consent.list_blocked_clients`.
    async fn list_blocked_clients(&self) -> Result<Vec<BlockedClient>, ConnectedAppsApiError>;
    /// `fauna.bridges.atproto.list_sessions`.
    async fn list_sessions(&self) -> Result<Vec<AtprotoSessionInfo>, ConnectedAppsApiError>;
    /// `fauna.bridges.atproto.list_grants`.
    async fn list_grants(&self) -> Result<Vec<AtprotoGrantInfo>, ConnectedAppsApiError>;
    /// `fauna.bridges.atproto.revoke_session` — cascades to an OAuth grant
    /// whose id it is.
    async fn revoke_session(&self, session_id: Vec<u8>) -> Result<bool, ConnectedAppsApiError>;
    /// `fauna.nostr.bunker.list`.
    async fn list_bunker_apps(&self) -> Result<Vec<BunkerAppEntry>, ConnectedAppsApiError>;
    /// `fauna.nostr.bunker.revoke`.
    async fn revoke_bunker_app(&self, connection_id: i64) -> Result<bool, ConnectedAppsApiError>;
    /// The mail app passwords, re-read through the mail-settings machine.
    /// Empty when the account has no credential management to show (mail,
    /// calendar, contacts and served files all off) or the session holds no
    /// mail machine.
    async fn list_mail_credentials(
        &self,
    ) -> Result<Vec<MailCredentialSummary>, ConnectedAppsApiError>;
    /// The mail machine's `RevokeCredential`: both on-nest blobs and the
    /// custody row's soft-revoke marker.
    async fn revoke_mail_credential(
        &self,
        credential_id: String,
    ) -> Result<bool, ConnectedAppsApiError>;
    /// One mail app password's secret, read on demand from the account's own
    /// mail custody. `NotFound` when no such credential exists.
    async fn reveal_mail_secret(
        &self,
        credential_id: String,
    ) -> Result<SecretString, ConnectedAppsApiError>;
}
