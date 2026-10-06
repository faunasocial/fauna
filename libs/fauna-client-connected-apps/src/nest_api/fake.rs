//! In-memory [`ConnectedAppsNestApi`] for the machine's tests.

#![cfg(any(test, feature = "test-helpers"))]

use std::sync::Mutex;

use fauna_atproto_settings_machine::NestConsentRow;
use fauna_client_mail_settings::MailCredentialSummary;
use fauna_core::secret::SecretString;
use fauna_protocol::atproto_pds::{AtprotoGrantInfo, AtprotoSessionInfo};
use fauna_protocol::nostr::BunkerAppEntry;
use fauna_protocol::oauth_consent::BlockedClient;
use fauna_protocol::principals::PrincipalInfo;

use super::{ConnectedAppsApiError, ConnectedAppsNestApi};

/// Every seam call, in order, with its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeCall {
    ListPrincipals,
    RevokePrincipal(Vec<u8>),
    ListPendingConsents,
    ResolveConsent(Vec<u8>, bool),
    MintGrant(Vec<u8>),
    RevokeGrant([u8; 16]),
    LookupCode(String),
    OpenHandoff(String),
    BlockClient(String, bool),
    ListBlockedClients,
    ListSessions,
    ListGrants,
    RevokeSession(Vec<u8>),
    ListBunkerApps,
    RevokeBunkerApp(i64),
    ListMailCredentials,
    RevokeMailCredential(String),
    RevealMailSecret(String),
}

#[derive(Debug, Default)]
struct Inner {
    calls: Vec<FakeCall>,
    principals: Vec<PrincipalInfo>,
    consents: Vec<NestConsentRow>,
    codes: Vec<(String, NestConsentRow)>,
    handoffs: Vec<(String, NestConsentRow)>,
    drop_opened_from_list: bool,
    blocked: Vec<BlockedClient>,
    sessions: Vec<AtprotoSessionInfo>,
    grants: Vec<AtprotoGrantInfo>,
    bunker: Vec<BunkerAppEntry>,
    mail: Vec<MailCredentialSummary>,
    bunker_unavailable: bool,
    fail_all: bool,
    fail_revokes: bool,
    fail_mail: bool,
}

/// A scripted nest: set rows, drive the machine, read [`Self::calls`].
/// Gestures mutate the rows the way the nest would, so a re-read after a
/// gesture sees its effect.
#[derive(Debug, Default)]
pub struct FakeConnectedAppsNestApi {
    inner: Mutex<Inner>,
}

fn transient() -> ConnectedAppsApiError {
    ConnectedAppsApiError::Transient {
        detail: "scripted failure".into(),
    }
}

fn unavailable() -> ConnectedAppsApiError {
    ConnectedAppsApiError::Unavailable {
        detail: "fauna.protocol.unknown_kind".into(),
    }
}

impl FakeConnectedAppsNestApi {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn calls(&self) -> Vec<FakeCall> {
        self.inner.lock().unwrap().calls.clone()
    }
    pub fn set_principals(&self, rows: Vec<PrincipalInfo>) {
        self.inner.lock().unwrap().principals = rows;
    }
    pub fn set_consents(&self, rows: Vec<NestConsentRow>) {
        self.inner.lock().unwrap().consents = rows;
    }
    /// A typed code that claims `row` onto the pending list when looked up.
    pub fn add_code(&self, code: &str, row: NestConsentRow) {
        self.inner.lock().unwrap().codes.push((code.into(), row));
    }
    /// A PAR handle that opens `row` onto the pending list, once.
    pub fn add_handoff(&self, request_uri: &str, row: NestConsentRow) {
        self.inner
            .lock()
            .unwrap()
            .handoffs
            .push((request_uri.into(), row));
    }
    /// An opened handoff's row is answered but missing from the next re-list
    /// (a failed or racing read), so the machine must paint it from the reply.
    pub fn drop_opened_from_list(&self) {
        self.inner.lock().unwrap().drop_opened_from_list = true;
    }
    pub fn set_blocked(&self, rows: Vec<BlockedClient>) {
        self.inner.lock().unwrap().blocked = rows;
    }
    pub fn set_sessions(&self, rows: Vec<AtprotoSessionInfo>) {
        self.inner.lock().unwrap().sessions = rows;
    }
    pub fn set_grants(&self, rows: Vec<AtprotoGrantInfo>) {
        self.inner.lock().unwrap().grants = rows;
    }
    pub fn set_bunker(&self, rows: Vec<BunkerAppEntry>) {
        self.inner.lock().unwrap().bunker = rows;
    }
    /// The mail app passwords. A password's secret reads back as
    /// `secret-<credential_id>`.
    pub fn set_mail_credentials(&self, rows: Vec<MailCredentialSummary>) {
        self.inner.lock().unwrap().mail = rows;
    }
    /// Answer the bunker roster as a nest whose Nostr link is not custodial.
    pub fn bunker_unavailable(&self) {
        self.inner.lock().unwrap().bunker_unavailable = true;
    }
    /// Every call fails transiently from now on.
    pub fn fail_all(&self) {
        self.inner.lock().unwrap().fail_all = true;
    }
    /// The mail app-password read fails transiently from now on.
    pub fn fail_mail(&self) {
        self.inner.lock().unwrap().fail_mail = true;
    }
    /// Every revoke fails transiently from now on.
    pub fn fail_revokes(&self) {
        self.inner.lock().unwrap().fail_revokes = true;
    }

    fn record(&self, call: FakeCall) -> std::sync::MutexGuard<'_, Inner> {
        let mut g = self.inner.lock().unwrap();
        g.calls.push(call);
        g
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl ConnectedAppsNestApi for FakeConnectedAppsNestApi {
    async fn list_principals(&self) -> Result<Vec<PrincipalInfo>, ConnectedAppsApiError> {
        let g = self.record(FakeCall::ListPrincipals);
        if g.fail_all {
            return Err(transient());
        }
        Ok(g.principals.clone())
    }
    async fn revoke_principal(&self, principal_id: Vec<u8>) -> Result<bool, ConnectedAppsApiError> {
        let mut g = self.record(FakeCall::RevokePrincipal(principal_id.clone()));
        if g.fail_all || g.fail_revokes {
            return Err(transient());
        }
        let before = g.principals.len();
        g.principals.retain(|p| p.principal_id != principal_id);
        Ok(g.principals.len() != before)
    }
    async fn list_pending_consents(&self) -> Result<Vec<NestConsentRow>, ConnectedAppsApiError> {
        let g = self.record(FakeCall::ListPendingConsents);
        if g.fail_all {
            return Err(transient());
        }
        Ok(g.consents.clone())
    }
    async fn resolve_consent(
        &self,
        consent_id: Vec<u8>,
        approved: bool,
    ) -> Result<bool, ConnectedAppsApiError> {
        let mut g = self.record(FakeCall::ResolveConsent(consent_id.clone(), approved));
        if g.fail_all {
            return Err(transient());
        }
        let before = g.consents.len();
        g.consents.retain(|c| c.consent_id != consent_id);
        Ok(g.consents.len() != before)
    }
    async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), ConnectedAppsApiError> {
        let g = self.record(FakeCall::MintGrant(grant_blob));
        if g.fail_all {
            return Err(transient());
        }
        Ok(())
    }
    async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), ConnectedAppsApiError> {
        let g = self.record(FakeCall::RevokeGrant(grant_id));
        if g.fail_all || g.fail_revokes {
            return Err(transient());
        }
        Ok(())
    }
    async fn lookup_code(
        &self,
        user_code: String,
    ) -> Result<Option<NestConsentRow>, ConnectedAppsApiError> {
        let mut g = self.record(FakeCall::LookupCode(user_code.clone()));
        if g.fail_all {
            return Err(transient());
        }
        let norm = |s: &str| {
            s.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .map(|c| c.to_ascii_uppercase())
                .collect::<String>()
        };
        let pos = g
            .codes
            .iter()
            .position(|(c, _)| norm(c) == norm(&user_code));
        Ok(pos.map(|i| {
            let (_, row) = g.codes.remove(i);
            g.consents.push(row.clone());
            row
        }))
    }
    async fn open_handoff(
        &self,
        request_uri: String,
    ) -> Result<Option<NestConsentRow>, ConnectedAppsApiError> {
        let mut g = self.record(FakeCall::OpenHandoff(request_uri.clone()));
        if g.fail_all {
            return Err(transient());
        }
        // Single use, like the PAR handle it stands for: a second open misses.
        let pos = g.handoffs.iter().position(|(h, _)| *h == request_uri);
        Ok(pos.map(|i| {
            let (_, row) = g.handoffs.remove(i);
            if !g.drop_opened_from_list {
                g.consents.push(row.clone());
            }
            row
        }))
    }
    async fn block_client(
        &self,
        client_id: String,
        blocked: bool,
    ) -> Result<bool, ConnectedAppsApiError> {
        let mut g = self.record(FakeCall::BlockClient(client_id.clone(), blocked));
        if g.fail_all {
            return Err(transient());
        }
        g.blocked.retain(|b| b.client_id != client_id);
        if blocked {
            g.blocked.push(BlockedClient {
                client_id,
                blocked_at: 1_700_000_000_000,
                extra: Default::default(),
            });
        }
        Ok(blocked)
    }
    async fn list_blocked_clients(&self) -> Result<Vec<BlockedClient>, ConnectedAppsApiError> {
        let g = self.record(FakeCall::ListBlockedClients);
        if g.fail_all {
            return Err(transient());
        }
        Ok(g.blocked.clone())
    }
    async fn list_sessions(&self) -> Result<Vec<AtprotoSessionInfo>, ConnectedAppsApiError> {
        let g = self.record(FakeCall::ListSessions);
        if g.fail_all {
            return Err(transient());
        }
        Ok(g.sessions.clone())
    }
    async fn list_grants(&self) -> Result<Vec<AtprotoGrantInfo>, ConnectedAppsApiError> {
        let g = self.record(FakeCall::ListGrants);
        if g.fail_all {
            return Err(transient());
        }
        Ok(g.grants.clone())
    }
    async fn revoke_session(&self, session_id: Vec<u8>) -> Result<bool, ConnectedAppsApiError> {
        let mut g = self.record(FakeCall::RevokeSession(session_id.clone()));
        if g.fail_all || g.fail_revokes {
            return Err(transient());
        }
        let before = g.sessions.len() + g.grants.len();
        g.sessions.retain(|s| s.session_id != session_id);
        g.grants.retain(|gr| gr.grant_id != session_id);
        Ok(g.sessions.len() + g.grants.len() != before)
    }
    async fn list_bunker_apps(&self) -> Result<Vec<BunkerAppEntry>, ConnectedAppsApiError> {
        let g = self.record(FakeCall::ListBunkerApps);
        if g.fail_all {
            return Err(transient());
        }
        if g.bunker_unavailable {
            return Err(unavailable());
        }
        Ok(g.bunker.clone())
    }
    async fn revoke_bunker_app(&self, connection_id: i64) -> Result<bool, ConnectedAppsApiError> {
        let mut g = self.record(FakeCall::RevokeBunkerApp(connection_id));
        if g.fail_all || g.fail_revokes {
            return Err(transient());
        }
        let before = g.bunker.len();
        g.bunker.retain(|b| b.id != connection_id);
        Ok(g.bunker.len() != before)
    }
    async fn list_mail_credentials(
        &self,
    ) -> Result<Vec<MailCredentialSummary>, ConnectedAppsApiError> {
        let g = self.record(FakeCall::ListMailCredentials);
        if g.fail_all || g.fail_mail {
            return Err(transient());
        }
        Ok(g.mail.clone())
    }
    async fn revoke_mail_credential(
        &self,
        credential_id: String,
    ) -> Result<bool, ConnectedAppsApiError> {
        let mut g = self.record(FakeCall::RevokeMailCredential(credential_id.clone()));
        if g.fail_all || g.fail_revokes {
            return Err(transient());
        }
        let before = g.mail.len();
        g.mail.retain(|c| c.credential_id != credential_id);
        Ok(g.mail.len() != before)
    }
    async fn reveal_mail_secret(
        &self,
        credential_id: String,
    ) -> Result<SecretString, ConnectedAppsApiError> {
        let g = self.record(FakeCall::RevealMailSecret(credential_id.clone()));
        if g.fail_all {
            return Err(transient());
        }
        if !g.mail.iter().any(|c| c.credential_id == credential_id) {
            return Err(ConnectedAppsApiError::NotFound {
                detail: format!("unknown credential {credential_id}"),
            });
        }
        Ok(SecretString::new(format!("secret-{credential_id}")))
    }
}
