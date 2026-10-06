//! The in-memory fake for the machine's nest seam:
//! [`FakeAtprotoSettingsNestApi`] records every call and returns scripted
//! responses. The account-plane seams have their own doubles beside their
//! traits (`crate::credentials::FakeCredentialStore`,
//! `fauna_client_atproto::identity_store::InMemoryAtprotoIdentityStore`).

#![cfg(any(test, feature = "test-helpers"))]

use std::sync::Mutex;

use super::{
    AtprotoSettingsApiError, AtprotoSettingsNestApi, AuthoringDelegationState, CredentialListing,
    DeletePresenceOutcome, IntegrationStatus, LinkSummary, NestIdentitySummary, RevokeOutcome,
};
use crate::snapshots::AtprotoSessionRow;
use fauna_protocol::atproto_pds::AtprotoGrantInfo;

/// One recorded seam call, so a test can assert *what* was sent, not just what
/// came back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeCall {
    ListAppCredentials,
    ProvisionAppCredential {
        credential_id: String,
        label: String,
        verifier: String,
        dm_allowed: bool,
    },
    RevokeAppCredential {
        credential_id: String,
    },
    ListSessions,
    ListGrants,
    RevokeSession {
        session_id: Vec<u8>,
    },
    SetExternalAppsEnabled {
        enabled: bool,
    },
    GetIntegrationStatus,
    SetIntegrationLevel {
        target_level: String,
        did_method: String,
        user_rotation_pub_did_key: String,
        history_backfill: bool,
    },
    BlueskyLinkStatus,
    FetchAuthoringDelegation,
    FetchAuthoringKey,
    ProvisionAuthoringDelegation {
        cert: Vec<u8>,
    },
    RevokeAuthoringDelegation,
    DeletePresence,
    RecordTombstone {
        prev_cid: String,
    },
    RequestTombstone,
    ListPendingConsents,
    ResolveConsent {
        consent_id: Vec<u8>,
        approved: bool,
    },
    MintGrant {
        grant_blob: Vec<u8>,
    },
    RevokeGrant {
        grant_id: [u8; 16],
    },
}

#[derive(Debug, Default)]
struct FakeState {
    listing: CredentialListingState,
    sessions: Vec<AtprotoSessionRow>,
    grants: Vec<AtprotoGrantInfo>,
    status: IntegrationStatusState,
    link: Option<LinkSummary>,
    calls: Vec<FakeCall>,
    fail_list: Option<AtprotoSettingsApiError>,
    fail_list_sessions: Option<AtprotoSettingsApiError>,
    fail_list_grants: Option<AtprotoSettingsApiError>,
    fail_provision: Option<AtprotoSettingsApiError>,
    fail_revoke: Option<AtprotoSettingsApiError>,
    fail_set_enabled: Option<AtprotoSettingsApiError>,
    fail_status: Option<AtprotoSettingsApiError>,
    fail_set_level: Option<AtprotoSettingsApiError>,
    fail_link_status: Option<AtprotoSettingsApiError>,
    revoke_outcome: Option<RevokeOutcome>,
    // D10. Modelled as real state rather than canned replies so a test drives
    // the actual ceremony: `fetch_authoring_key` mints `k_pub` here on first
    // call, and `provision` stores the cert the machine really signed — which
    // is what lets the ordering and re-mint tests mean anything.
    delegation: AuthoringDelegationState,
    minted_k_pub: Option<[u8; 32]>,
    fail_fetch_delegation: Option<AtprotoSettingsApiError>,
    fail_fetch_authoring_key: Option<AtprotoSettingsApiError>,
    fail_provision_delegation: Option<AtprotoSettingsApiError>,
    fail_revoke_delegation: Option<AtprotoSettingsApiError>,
    fail_delete_presence: Option<AtprotoSettingsApiError>,
    fail_record_tombstone: Option<AtprotoSettingsApiError>,
    fail_request_tombstone: Option<AtprotoSettingsApiError>,
    // F4 rung 2. Real state, not canned replies, for the same reason the
    // delegation is: `resolve_consent` must actually consume the row, so a test
    // asserting "the card is gone after approving" is asserting the mechanism
    // rather than a scripted second answer.
    consents: Vec<super::NestConsentRow>,
    fail_list_consents: Option<AtprotoSettingsApiError>,
    fail_resolve_consent: Option<AtprotoSettingsApiError>,
    fail_mint_grant: Option<AtprotoSettingsApiError>,
    /// Answer `resolve_consent` with `false` (answered elsewhere) whatever
    /// the row's state — the race a deposited grant is withdrawn on.
    resolve_answers_gone: bool,
}

#[derive(Debug)]
struct IntegrationStatusState {
    level: String,
    hosted_allowed: bool,
    handle_domain: String,
    handle_preview: String,
    identity: Option<NestIdentitySummary>,
}

impl Default for IntegrationStatusState {
    fn default() -> Self {
        // A gate-passing box at level off — the common fixture. Tests that
        // pin the gated shape script it explicitly via `set_status`.
        Self {
            level: "off".into(),
            hosted_allowed: true,
            handle_domain: "example.com".into(),
            handle_preview: "alice.example.com".into(),
            identity: None,
        }
    }
}

#[derive(Debug)]
struct CredentialListingState {
    credentials: Vec<super::NestCredentialRow>,
    external_apps_enabled: bool,
}

impl Default for CredentialListingState {
    fn default() -> Self {
        // Default ON — the nest's `atproto_account_settings.external_apps_enabled`
        // column defaults to 1, and a fake that started OFF would let a test pass
        // against a default the product does not have.
        Self {
            credentials: Vec::new(),
            external_apps_enabled: true,
        }
    }
}

/// Scriptable in-memory [`AtprotoSettingsNestApi`].
#[derive(Debug, Default)]
pub struct FakeAtprotoSettingsNestApi {
    state: Mutex<FakeState>,
}

impl FakeAtprotoSettingsNestApi {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_credentials(&self, rows: Vec<super::NestCredentialRow>) {
        self.state.lock().unwrap().listing.credentials = rows;
    }

    pub fn set_external_apps_enabled(&self, enabled: bool) {
        self.state.lock().unwrap().listing.external_apps_enabled = enabled;
    }

    pub fn set_sessions(&self, rows: Vec<AtprotoSessionRow>) {
        self.state.lock().unwrap().sessions = rows;
    }

    /// Pre-seed the live OAuth grants. A grant joins a session by
    /// `hex(grant_id) == session_id_hex`, exactly as the nest's one-transaction
    /// write makes true.
    pub fn set_grants(&self, rows: Vec<AtprotoGrantInfo>) {
        self.state.lock().unwrap().grants = rows;
    }

    /// Pre-seed the live consent requests this caller may answer.
    pub fn set_pending_consents(&self, rows: Vec<super::NestConsentRow>) {
        self.state.lock().unwrap().consents = rows;
    }

    /// Read back which requests are still live — the assertion surface for
    /// "answering really consumed the row".
    pub fn pending_consents(&self) -> Vec<super::NestConsentRow> {
        self.state.lock().unwrap().consents.clone()
    }

    pub fn fail_list_consents(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_list_consents = Some(e);
    }

    pub fn fail_resolve_consent(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_resolve_consent = Some(e);
    }

    pub fn fail_mint_grant(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_mint_grant = Some(e);
    }

    /// Make every `resolve_consent` answer `false` — another device answered.
    pub fn resolve_answers_gone(&self) {
        self.state.lock().unwrap().resolve_answers_gone = true;
    }

    /// Pre-seed what the nest holds for the authoring delegation.
    pub fn set_delegation(&self, state: AuthoringDelegationState) {
        self.state.lock().unwrap().delegation = state;
    }

    /// Read back what the nest ended up holding — the assertion surface for
    /// "the ceremony actually stored a cert".
    pub fn delegation(&self) -> AuthoringDelegationState {
        self.state.lock().unwrap().delegation.clone()
    }

    /// Fix the `k_pub` the mint ceremony will be handed, so a test can assert
    /// the cert was signed over *that* key.
    pub fn set_minted_k_pub(&self, k_pub: [u8; 32]) {
        let mut s = self.state.lock().unwrap();
        s.minted_k_pub = Some(k_pub);
    }

    pub fn fail_fetch_delegation(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_fetch_delegation = Some(e);
    }

    pub fn fail_fetch_authoring_key(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_fetch_authoring_key = Some(e);
    }

    pub fn fail_provision_delegation(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_provision_delegation = Some(e);
    }

    pub fn fail_revoke_delegation(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_revoke_delegation = Some(e);
    }

    pub fn fail_list(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_list = Some(e);
    }

    pub fn fail_list_sessions(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_list_sessions = Some(e);
    }

    pub fn fail_list_grants(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_list_grants = Some(e);
    }

    pub fn fail_provision(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_provision = Some(e);
    }

    pub fn fail_revoke(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_revoke = Some(e);
    }

    pub fn fail_set_enabled(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_set_enabled = Some(e);
    }

    pub fn set_revoke_outcome(&self, outcome: RevokeOutcome) {
        self.state.lock().unwrap().revoke_outcome = Some(outcome);
    }

    /// Script the whole integration-status read at once.
    pub fn set_status(
        &self,
        level: &str,
        hosted_allowed: bool,
        handle_domain: &str,
        handle_preview: &str,
        identity: Option<NestIdentitySummary>,
    ) {
        let mut s = self.state.lock().unwrap();
        s.status = IntegrationStatusState {
            level: level.into(),
            hosted_allowed,
            handle_domain: handle_domain.into(),
            handle_preview: handle_preview.into(),
            identity,
        };
    }

    pub fn set_link(&self, link: Option<LinkSummary>) {
        self.state.lock().unwrap().link = link;
    }

    pub fn fail_status(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_status = Some(e);
    }

    pub fn fail_set_level(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_set_level = Some(e);
    }

    pub fn fail_link_status(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_link_status = Some(e);
    }

    /// Script the next `delete_presence` calls to fail — the transport window
    /// § Errors & edge cases requires the card to survive.
    pub fn fail_delete_presence(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_delete_presence = Some(e);
    }

    /// End a scripted `fail_delete_presence` — the transient has passed, so a
    /// retry from the still-open card can succeed.
    pub fn clear_delete_presence_failure(&self) {
        self.state.lock().unwrap().fail_delete_presence = None;
    }

    pub fn fail_record_tombstone(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_record_tombstone = Some(e);
    }

    /// End a scripted `fail_record_tombstone` — the transient has passed.
    pub fn clear_record_tombstone_failure(&self) {
        self.state.lock().unwrap().fail_record_tombstone = None;
    }

    /// Script the next `request_tombstone` calls to fail — the crash/transport
    /// window the consent-first ordering exists for.
    pub fn fail_request_tombstone(&self, e: AtprotoSettingsApiError) {
        self.state.lock().unwrap().fail_request_tombstone = Some(e);
    }

    /// End a scripted `fail_request_tombstone` — the transient has passed.
    pub fn clear_request_tombstone_failure(&self) {
        self.state.lock().unwrap().fail_request_tombstone = None;
    }

    pub fn calls(&self) -> Vec<FakeCall> {
        self.state.lock().unwrap().calls.clone()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl AtprotoSettingsNestApi for FakeAtprotoSettingsNestApi {
    async fn list_app_credentials(&self) -> Result<CredentialListing, AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::ListAppCredentials);
        if let Some(e) = s.fail_list.clone() {
            return Err(e);
        }
        Ok(CredentialListing {
            credentials: s.listing.credentials.clone(),
            external_apps_enabled: s.listing.external_apps_enabled,
        })
    }

    async fn provision_app_credential(
        &self,
        credential_id: String,
        label: String,
        verifier: String,
        dm_allowed: bool,
    ) -> Result<(), AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::ProvisionAppCredential {
            credential_id: credential_id.clone(),
            label: label.clone(),
            verifier,
            dm_allowed,
        });
        if let Some(e) = s.fail_provision.clone() {
            return Err(e);
        }
        // Model the nest actually storing the row, so a subsequent refresh
        // lists it exactly as production would.
        s.listing.credentials.push(super::NestCredentialRow {
            credential_id,
            label,
            dm_allowed,
            created_at_millis: 1_700_000_000_000,
            last_used_at_millis: None,
        });
        Ok(())
    }

    async fn revoke_app_credential(
        &self,
        credential_id: String,
    ) -> Result<RevokeOutcome, AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::RevokeAppCredential {
            credential_id: credential_id.clone(),
        });
        if let Some(e) = s.fail_revoke.clone() {
            return Err(e);
        }
        let existed = s
            .listing
            .credentials
            .iter()
            .any(|c| c.credential_id == credential_id);
        s.listing
            .credentials
            .retain(|c| c.credential_id != credential_id);
        // The nest-side cascade kills the sessions minted from the credential.
        let killed = s
            .sessions
            .iter()
            .filter(|x| x.credential_id.as_deref() == Some(credential_id.as_str()))
            .count() as u32;
        s.sessions
            .retain(|x| x.credential_id.as_deref() != Some(credential_id.as_str()));
        Ok(s.revoke_outcome.unwrap_or(RevokeOutcome {
            revoked: existed,
            sessions_revoked: killed,
        }))
    }

    async fn list_sessions(&self) -> Result<Vec<AtprotoSessionRow>, AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::ListSessions);
        if let Some(e) = s.fail_list_sessions.clone() {
            return Err(e);
        }
        Ok(s.sessions.clone())
    }

    async fn list_grants(&self) -> Result<Vec<AtprotoGrantInfo>, AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::ListGrants);
        if let Some(e) = s.fail_list_grants.clone() {
            return Err(e);
        }
        Ok(s.grants.clone())
    }

    async fn revoke_session(&self, session_id: Vec<u8>) -> Result<bool, AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::RevokeSession {
            session_id: session_id.clone(),
        });
        let hex_id = hex::encode(&session_id);
        let existed = s.sessions.iter().any(|x| x.session_id_hex == hex_id);
        s.sessions.retain(|x| x.session_id_hex != hex_id);
        // The nest cascades a session revoke to its grant row in one
        // transaction (`grant_id` *is* the family id), so the fake must too —
        // otherwise a test could "revoke" and still see the grant half live,
        // a state the real registry cannot produce.
        s.grants.retain(|g| hex::encode(&g.grant_id) != hex_id);
        Ok(existed)
    }

    async fn set_external_apps_enabled(
        &self,
        enabled: bool,
    ) -> Result<(), AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::SetExternalAppsEnabled { enabled });
        if let Some(e) = s.fail_set_enabled.clone() {
            return Err(e);
        }
        s.listing.external_apps_enabled = enabled;
        Ok(())
    }

    async fn get_integration_status(&self) -> Result<IntegrationStatus, AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::GetIntegrationStatus);
        if let Some(e) = s.fail_status.clone() {
            return Err(e);
        }
        Ok(IntegrationStatus {
            level: s.status.level.clone(),
            hosted_allowed: s.status.hosted_allowed,
            handle_domain: s.status.handle_domain.clone(),
            handle_preview: s.status.handle_preview.clone(),
            identity: s.status.identity.clone(),
        })
    }

    async fn set_integration_level(
        &self,
        target_level: String,
        did_method: String,
        user_rotation_pub_did_key: String,
        history_backfill: bool,
    ) -> Result<String, AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::SetIntegrationLevel {
            target_level: target_level.clone(),
            did_method,
            user_rotation_pub_did_key,
            history_backfill,
        });
        if let Some(e) = s.fail_set_level.clone() {
            return Err(e);
        }
        // Model the nest's composed effects at the fidelity the machine
        // observes on its refresh: the level persists; entering a hosted
        // level with no identity records a pending mint intent; leaving
        // hosted deactivates but retains it; the consume-side link drops on a
        // hosted entry (the one-backing rule) and on Linked → down.
        let target = fauna_protocol::atproto::IntegrationLevel::from_wire(&target_level)
            .expect("tests script ratified levels");
        let current = fauna_protocol::atproto::IntegrationLevel::from_wire(&s.status.level)
            .expect("fake state holds ratified levels");
        if target.is_hosted() {
            s.link = None;
            match &mut s.status.identity {
                // Reactivation, at the fidelity the summary carries (it has
                // no DID field — nest's pending-vs-active split on re-entry
                // is pinned by the conformance suite, not here).
                Some(id) => id.status = "active".into(),
                None => {
                    s.status.identity = Some(NestIdentitySummary {
                        handle: s.status.handle_preview.clone(),
                        method: "plc".into(),
                        status: "pending".into(),
                        tombstone_requested: false,
                        did: None,
                    })
                }
            }
        } else if current.is_hosted() {
            if let Some(id) = &mut s.status.identity {
                id.status = "deactivated".into();
            }
        } else if current == fauna_protocol::atproto::IntegrationLevel::Linked && target < current {
            s.link = None;
        }
        s.status.level = target_level.clone();
        Ok(target_level)
    }

    /// Models `delete_presence_handler`'s real effects rather than a canned
    /// reply, because the ceremony's whole point is the cascade: an account
    /// with no identity is the handler's `no_hosted_identity` refusal (a client
    /// that offered the button there misread its own snapshot), and a real call
    /// revokes every session, destroys the D10 authoring credential, records
    /// the tombstone and drops the level to `off`. A fake that only flipped a
    /// status would let a page that leaves stale connected-app rows on screen
    /// pass its own test.
    async fn delete_presence(&self) -> Result<DeletePresenceOutcome, AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::DeletePresence);
        if let Some(e) = s.fail_delete_presence.clone() {
            return Err(e);
        }
        let Some(id) = s.status.identity.as_mut() else {
            return Err(AtprotoSettingsApiError::Transient {
                detail: "fauna.bridges.atproto.no_hosted_identity".into(),
            });
        };
        // Idempotent: confirming twice destroys the same presence once.
        let newly_deleted = id.status != "deleted" && id.status != "tombstoned";
        if newly_deleted {
            id.status = "deleted".into();
        }
        s.sessions.clear();
        s.delegation = AuthoringDelegationState::default();
        s.status.level = "off".into();
        Ok(DeletePresenceOutcome {
            level: "off".into(),
            newly_deleted,
        })
    }

    /// Models nest's real precondition rather than always answering `true`:
    /// only a `'deleted'` identity that recorded the opt-in may be retired, and
    /// a report for anything else is the refusal the nest kind raises. A fake
    /// that always succeeded here would let the converge pass's ordering bug
    /// pass its own test.
    async fn record_tombstone(&self, prev_cid: String) -> Result<bool, AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::RecordTombstone { prev_cid });
        if let Some(e) = s.fail_record_tombstone.clone() {
            return Err(e);
        }
        match &mut s.status.identity {
            Some(id) if id.status == "tombstoned" => Ok(false),
            Some(id) if id.status == "deleted" && id.tombstone_requested => {
                id.status = "tombstoned".into();
                Ok(true)
            }
            // `Transient` is what the real seam maps a policy refusal to; the
            // detail carries the namespaced code, as it does on the wire.
            _ => Err(AtprotoSettingsApiError::Transient {
                detail: "fauna.bridges.atproto.tombstone_not_authorized".into(),
            }),
        }
    }

    /// Models nest's real precondition (`request_tombstone_handler`): only a
    /// `'deleted'` (or already-`'tombstoned'`) did:plc identity may record the
    /// opt-in — the wire-enforced "unreachable outside the delete ceremony"
    /// rule. A fake that always succeeded would let a gesture that skipped the
    /// ceremony pass its own test.
    async fn request_tombstone(&self) -> Result<bool, AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::RequestTombstone);
        if let Some(e) = s.fail_request_tombstone.clone() {
            return Err(e);
        }
        match &mut s.status.identity {
            Some(id)
                if id.method == "plc"
                    && id.did.is_some()
                    && (id.status == "deleted" || id.status == "tombstoned") =>
            {
                let newly = !id.tombstone_requested;
                id.tombstone_requested = true;
                Ok(newly)
            }
            _ => Err(AtprotoSettingsApiError::Transient {
                detail: "fauna.bridges.atproto.not_tombstoneable".into(),
            }),
        }
    }

    async fn bluesky_link_status(&self) -> Result<Option<LinkSummary>, AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::BlueskyLinkStatus);
        if let Some(e) = s.fail_link_status.clone() {
            return Err(e);
        }
        Ok(s.link.clone())
    }

    async fn fetch_authoring_delegation(
        &self,
    ) -> Result<AuthoringDelegationState, AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::FetchAuthoringDelegation);
        if let Some(e) = s.fail_fetch_delegation.clone() {
            return Err(e);
        }
        Ok(s.delegation.clone())
    }

    async fn fetch_authoring_key(&self) -> Result<[u8; 32], AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::FetchAuthoringKey);
        if let Some(e) = s.fail_fetch_authoring_key.clone() {
            return Err(e);
        }
        // Mint-on-read, first-write-wins: the first call fixes `k_pub` and
        // every later one returns that same key, exactly as the nest does.
        let k = *s.minted_k_pub.get_or_insert([0xA5; 32]);
        s.delegation.k_pub = Some(k);
        Ok(k)
    }

    async fn provision_authoring_delegation(
        &self,
        cert: Vec<u8>,
    ) -> Result<(), AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls
            .push(FakeCall::ProvisionAuthoringDelegation { cert: cert.clone() });
        if let Some(e) = s.fail_provision_delegation.clone() {
            return Err(e);
        }
        s.delegation.cert = Some(cert);
        Ok(())
    }

    async fn revoke_authoring_delegation(&self) -> Result<bool, AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::RevokeAuthoringDelegation);
        if let Some(e) = s.fail_revoke_delegation.clone() {
            return Err(e);
        }
        // Revoke destroys the whole row — sub-key and cert alike (D10 §
        // Revocation) — so a later fetch reads back empty, not cert-less.
        let had = s.delegation.k_pub.is_some() || s.delegation.cert.is_some();
        s.delegation = AuthoringDelegationState::default();
        s.minted_k_pub = None;
        Ok(had)
    }

    async fn list_pending_consents(
        &self,
    ) -> Result<Vec<super::NestConsentRow>, AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::ListPendingConsents);
        if let Some(e) = s.fail_list_consents.clone() {
            return Err(e);
        }
        Ok(s.consents.clone())
    }

    async fn resolve_consent(
        &self,
        consent_id: Vec<u8>,
        approved: bool,
    ) -> Result<bool, AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::ResolveConsent {
            consent_id: consent_id.clone(),
            approved,
        });
        if let Some(e) = s.fail_resolve_consent.clone() {
            return Err(e);
        }
        // The nest's `WHERE` clause, modelled: an id with no live row is simply
        // unmatched — `false`, not an error — and a matched row is consumed, so
        // a replayed answer can never resolve twice.
        if s.resolve_answers_gone {
            return Ok(false);
        }
        let before = s.consents.len();
        s.consents.retain(|c| c.consent_id != consent_id);
        Ok(s.consents.len() != before)
    }

    async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), AtprotoSettingsApiError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeCall::MintGrant { grant_blob });
        match s.fail_mint_grant.clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), AtprotoSettingsApiError> {
        self.state
            .lock()
            .unwrap()
            .calls
            .push(FakeCall::RevokeGrant { grant_id });
        Ok(())
    }
}
