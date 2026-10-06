//! Trait abstraction for the page's nest-side surface.
//!
//! The page reads + gestures go through [`AtprotoSettingsNestApi`]. Production
//! code uses the WS-RPC [`ws_rpc::WsRpcAtprotoSettingsNest`] (over
//! `fauna_client_bridges::AtprotoSettingsClient`); tests use
//! [`FakeAtprotoSettingsNestApi`] (gated under `#[cfg(any(test, feature =
//! "test-helpers"))]`). Mirrors `fauna_labeler_catalog_machine::nest_api`.
//!
//! Transport: every read/gesture rides the authenticated WS-RPC connection —
//! the page runs inside an already-logged-in session, so the seam is
//! constructed with the session's connected requester (`Arc<NestClient>`
//! native / `WsRpcClient` wasm) and needs no per-call URL or token. There is no
//! HTTP impl (the `no-http-ws-rpc-everywhere` directive).
//!
//! **Where the secrets rest is a separate seam**, not a method here: the
//! machine holds the account-plane credential store
//! (`crate::credentials::AtprotoCredentialStore`) alongside this one. The split
//! is load-bearing rather than cosmetic — the credential secret must *never*
//! reach the nest seam, and two seams make that a type-level fact instead of a
//! review comment.

pub mod fake;
#[cfg(feature = "rpc-glue")]
pub mod ws_rpc;

#[cfg(any(test, feature = "test-helpers"))]
pub use fake::{FakeAtprotoSettingsNestApi, FakeCall};
#[cfg(feature = "rpc-glue")]
pub use ws_rpc::build_atproto_settings_machine;

use crate::snapshots::AtprotoSessionRow;
use fauna_protocol::atproto_pds::AtprotoGrantInfo;

fauna_core::declare_api_error!(
    /// Failure of a page-level nest call. Mirrors
    /// `fauna_labeler_catalog_machine::LabelerCatalogApiError`.
    AtprotoSettingsApiError {
        /// The row named by a gesture no longer exists.
        NotFound,
        /// Transport fault / 5xx / policy refusal — the detail carries the
        /// namespaced `RpcError` message the handler emitted.
        Transient,
    }
);

/// One credential row exactly as the **nest** knows it.
///
/// Deliberately distinct from [`crate::snapshots::AppCredentialRow`], which
/// additionally carries `revealable` — a fact only the local `fauna.state.atproto`
/// custody knows. Keeping the seam type free of that field means no seam
/// implementation can set it, wrongly or otherwise; the machine is the one
/// place the two sources are joined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NestCredentialRow {
    pub credential_id: String,
    pub label: String,
    pub dm_allowed: bool,
    /// Epoch milliseconds (the `fauna.bridges.atproto.*` wire convention).
    pub created_at_millis: i64,
    pub last_used_at_millis: Option<i64>,
}

/// One `fauna.bridges.atproto.list_app_credentials` read. The kill-switch flag
/// rides along with the rows because the nest sends them together — the
/// settings page renders the toggle and the list from a single fetch, so the
/// two can never disagree on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialListing {
    pub credentials: Vec<NestCredentialRow>,
    pub external_apps_enabled: bool,
}

/// One pending OAuth consent request, exactly as
/// `fauna.bridges.atproto.list_pending_consents` reported it.
///
/// Deliberately distinct from [`crate::snapshots::ConsentCardRow`], for the same
/// reason [`NestCredentialRow`] is: this one carries the raw `scopes`, the card
/// row carries their *wording*. Keeping the wording out of the seam means no
/// seam implementation can supply it — the machine derives it, from the one
/// owner (`fauna_bridge_atproto::authz::describe_scope`), which is what keeps
/// the browser page and this card saying the same thing about the same grant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NestConsentRow {
    /// The nest-issued consent id, raw bytes — the handle `resolve_consent`
    /// takes.
    pub consent_id: Vec<u8>,
    /// The binding code the user compares against their browser. Minted by the
    /// nest, so this value and the one the browser page renders have one origin
    /// and structurally cannot differ.
    pub code: String,
    /// The requesting `client_id`, a URL, rendered **verbatim**. Nothing derives
    /// an origin from it: that string is parsed exactly once, by the component
    /// that dials it.
    pub client_id: String,
    /// The client name from its *resolved* metadata document, when it published
    /// one — never a self-asserted string this side accepted unchecked.
    pub client_name: Option<String>,
    /// The requested scopes, wire spellings, in the order the row holds them.
    pub scopes: Vec<String>,
    /// The permission sets behind those scopes, frozen when PAR expanded them.
    /// Empty for the ordinary request that named none.
    pub sets: Vec<NestConsentSet>,
    /// The X25519 holder key the ceremony attested — what an approve that
    /// mints the consent-time grant wraps it to. `None` when none was attested.
    pub holder_x25519: Option<Vec<u8>>,
    /// The Ed25519 writer key the ceremony attested — `None` → the app is
    /// read-only over its record kinds.
    pub writer_ed25519: Option<Vec<u8>>,
    /// The client document's kind manifest, the compact JWS verbatim — the
    /// approving device verifies it before it admits a kind or wraps a key.
    pub fauna_manifest: Option<String>,
}

impl From<fauna_protocol::atproto_pds::PendingConsentRow> for NestConsentRow {
    /// The wire's pending row → the seam row: a pure transcription, the wire's
    /// scopes as their wire spellings. Their *wording* is the machine's to
    /// derive (`consent_card_row`), which is why the two are distinct types.
    fn from(c: fauna_protocol::atproto_pds::PendingConsentRow) -> Self {
        Self {
            consent_id: c.consent_id,
            code: c.code,
            client_id: c.client_id,
            client_name: c.client_name,
            scopes: c.scopes,
            sets: c
                .sets
                .into_iter()
                .map(|s| NestConsentSet {
                    nsid: s.nsid,
                    title: s.title,
                    details: s.details,
                    members: s.members,
                })
                .collect(),
            holder_x25519: c.holder_x25519.map(|b| b.into_vec()),
            writer_ed25519: c.writer_ed25519.map(|b| b.into_vec()),
            fauna_manifest: c.fauna_manifest,
        }
    }
}

impl NestConsentRow {
    /// The fields the consent-time grant reads, back on the wire shape
    /// `fauna_client_capabilities::ext_consent` takes.
    pub fn to_pending(&self) -> fauna_protocol::atproto_pds::PendingConsentRow {
        fauna_protocol::atproto_pds::PendingConsentRow {
            consent_id: self.consent_id.clone(),
            code: self.code.clone(),
            client_id: self.client_id.clone(),
            client_name: self.client_name.clone(),
            scopes: self.scopes.clone(),
            holder_x25519: self.holder_x25519.clone().map(Into::into),
            writer_ed25519: self.writer_ed25519.clone().map(Into::into),
            fauna_manifest: self.fauna_manifest.clone(),
            ..Default::default()
        }
    }
}

/// One permission set a consent request named, as the nest reported it.
///
/// Raw on purpose, exactly like [`NestConsentRow`]'s `scopes`: the members are
/// wire spellings the machine words through `describe_scope`, and `title` /
/// `details` are **attacker-authored** — a Lexicon record published by whatever
/// DID the NSID's authority names. Both the wording and the control-strip fence
/// belong to the machine's composition pass, so no seam implementation can
/// supply either (`atproto-pds-full.md:334`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NestConsentSet {
    /// The set's NSID — the identity, carried and rendered **verbatim**.
    pub nsid: String,
    /// The set's declared title, raw.
    pub title: Option<String>,
    /// The set's declared description, raw.
    pub details: Option<String>,
    /// The expansion, wire spellings, in document order.
    pub members: Vec<String>,
}

/// Outcome of `fauna.bridges.atproto.revoke_app_credential`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevokeOutcome {
    /// `false` when no such credential existed — the call is idempotent.
    pub revoked: bool,
    /// Sessions killed by the nest-side cascade. Already done when this
    /// returns; not a promise of future work.
    pub sessions_revoked: u32,
}

/// One `fauna.bridges.atproto.get_integration_status` read, exactly as the
/// nest reported it. Distinct from the snapshot fields the machine derives
/// from it (the gate *reason* text, the derived panel flags) — the seam stays
/// a transcription, the machine is where interpretation lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationStatus {
    /// Wire spelling of the current level.
    pub level: String,
    /// Nest-computed real-domain gate verdict.
    pub hosted_allowed: bool,
    /// The domain the verdict was computed from (names the greyed-rung
    /// reason).
    pub handle_domain: String,
    /// The would-be ATProto handle; empty when it does not derive.
    pub handle_preview: String,
    /// The hosted identity summary, when a row exists (any status).
    pub identity: Option<NestIdentitySummary>,
}

/// The nest's identity summary inside [`IntegrationStatus`].
///
/// `Default` exists for the fixtures: this record grows as the identity model
/// does, and struct-update fixtures (`..Default::default()`) let two branches
/// each add a field without colliding on the grown axis. The defaults are the
/// neutral ones — a pending, un-minted, un-retiring identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NestIdentitySummary {
    pub handle: String,
    /// `"plc"` or `"web"`.
    pub method: String,
    /// `"pending"`, `"active"`, `"deactivated"`, `"deleted"` (the presence
    /// sweep is owed or has run) or `"tombstoned"` (terminal).
    pub status: String,
    /// The user opted into permanently retiring this identity and no tombstone
    /// has been published yet. What the converge pass reads to know it still
    /// owes the act — and what a *second* device reads to know a retirement is
    /// already in flight.
    pub tombstone_requested: bool,
    /// The minted DID, once one exists (`None` while `pending`). Never
    /// rendered (the raw DID string stays hidden, `ui/atproto.md`) — carried
    /// for the genesis-seniority custody check, which resolves it from the
    /// public directory.
    pub did: Option<String>,
}

/// The consume-side Bluesky link as `fauna.bridges.list` reports it; `None`
/// when not linked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSummary {
    /// The linked identity's display string ("@alice.bsky.social").
    pub display: String,
}

/// What `fauna.bridges.atproto.delete_presence` reports back — deliberately
/// modest, because at reply time only the *decision* is durable
/// (`fauna_protocol::atproto_pds::DeletePresenceReply`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletePresenceOutcome {
    /// The integration level in force afterwards — always `"off"`. Carried
    /// rather than assumed: nest's answer is the truth a racing client
    /// converges on, exactly as for `set_integration_level`.
    pub level: String,
    /// Whether this call is what moved the identity into its deleted state.
    /// `false` is a success (already deleted — a retry, or a second device).
    pub newly_deleted: bool,
}

// `MaybeSendSync` supertrait + dual `async_trait` arm so the one seam serves
// native (`Arc<NestClient>`, `Send + Sync`) and wasm (the single-threaded
// `Rc`-based `WsRpcClient`, `!Send`) — the identical pattern on
// `fauna_labeler_catalog_machine::LabelerCatalogNestApi`.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait AtprotoSettingsNestApi: fauna_core::MaybeSendSync + std::fmt::Debug {
    /// `fauna.bridges.atproto.list_app_credentials` — the credential rows plus
    /// the account's kill-switch state.
    async fn list_app_credentials(&self) -> Result<CredentialListing, AtprotoSettingsApiError>;

    /// `fauna.bridges.atproto.provision_app_credential` — store the
    /// client-computed PHC verifier. The secret itself is **not** a parameter
    /// and never crosses this seam (D3 custody split).
    async fn provision_app_credential(
        &self,
        credential_id: String,
        label: String,
        verifier: String,
        dm_allowed: bool,
    ) -> Result<(), AtprotoSettingsApiError>;

    /// `fauna.bridges.atproto.revoke_app_credential` — idempotent; cascades to
    /// the sessions minted from the credential.
    async fn revoke_app_credential(
        &self,
        credential_id: String,
    ) -> Result<RevokeOutcome, AtprotoSettingsApiError>;

    /// `fauna.bridges.atproto.list_sessions` — live sessions only.
    async fn list_sessions(&self) -> Result<Vec<AtprotoSessionRow>, AtprotoSettingsApiError>;

    /// `fauna.bridges.atproto.list_grants` — the connected-apps registry read:
    /// live (unrevoked, unexpired) OAuth grants for the calling actor.
    ///
    /// The richer half of the row [`list_sessions`](Self::list_sessions)
    /// carries the liveness of. Returned as the wire record rather than a
    /// snapshot row because the machine composes the two into one row (and
    /// strips attacker text) rather than handing either through.
    async fn list_grants(&self) -> Result<Vec<AtprotoGrantInfo>, AtprotoSettingsApiError>;

    /// `fauna.bridges.atproto.revoke_session` — `false` when already gone.
    /// Also the grant revocation: a `grant_id` *is* a session-family id, so the
    /// nest cascades to `atproto_oauth_grants` in the same transaction and
    /// there is deliberately no `revoke_grant` companion.
    async fn revoke_session(&self, session_id: Vec<u8>) -> Result<bool, AtprotoSettingsApiError>;

    /// `fauna.bridges.atproto.set_external_apps_enabled` — the per-account
    /// kill-switch.
    async fn set_external_apps_enabled(&self, enabled: bool)
    -> Result<(), AtprotoSettingsApiError>;

    /// `fauna.bridges.atproto.get_integration_status` — the depth selector's
    /// one read: level + gate verdict + identity summary.
    async fn get_integration_status(&self) -> Result<IntegrationStatus, AtprotoSettingsApiError>;

    /// `fauna.bridges.atproto.set_integration_level` — the one call per
    /// confirmed transition. Returns the level actually in force afterward
    /// (nest's truth, which a racing client converges on). The mint
    /// parameters are read nest-side only when the transition mints.
    async fn set_integration_level(
        &self,
        target_level: String,
        did_method: String,
        user_rotation_pub_did_key: String,
        history_backfill: bool,
    ) -> Result<String, AtprotoSettingsApiError>;

    /// `fauna.bridges.atproto.delete_presence` — the delete ceremony's confirm
    /// (`atproto-delete-confirm`). Records the durable decision and tears down
    /// the account-side halves; the bridge sweeps the projected records on its
    /// own convergent pass. Returns the level in force afterwards (always
    /// `"off"`) and whether *this* call is what moved the identity into its
    /// deleted state.
    ///
    /// `newly_deleted = false` is a **success** — a retry, or a second device —
    /// never an error for a caller to surface (`atproto-pds-bridge.md`
    /// § Disable & revocation layer 2, the sweep's shape).
    async fn delete_presence(&self) -> Result<DeletePresenceOutcome, AtprotoSettingsApiError>;

    /// `fauna.bridges.atproto.record_tombstone` — report a published PLC
    /// tombstone so nest can move the identity to its terminal state. `true`
    /// when this call is what moved it.
    async fn record_tombstone(&self, prev_cid: String) -> Result<bool, AtprotoSettingsApiError>;

    /// `fauna.bridges.atproto.request_tombstone` — record the durable opt-in
    /// *intent* nest-side. `true` when this call is what recorded it.
    ///
    /// Never called on its own: the machine's `request_tombstone` gesture
    /// writes the user's consent client-side FIRST (the account plane
    /// custody's `tombstone_consents` — the record the converge pass requires and the
    /// nest cannot author), then this. The ceremony
    /// UI that invokes the gesture is still gated on net-new element IDs; the
    /// converge pass itself needs no UI at all, which is why it landed first.
    async fn request_tombstone(&self) -> Result<bool, AtprotoSettingsApiError>;

    /// The consume-side Bluesky link summary (`fauna.bridges.list`, provider
    /// `"bluesky"`). The machine composes this existing surface; it does not
    /// re-own link state.
    async fn bluesky_link_status(&self) -> Result<Option<LinkSummary>, AtprotoSettingsApiError>;

    // ── D10 delegated authoring (`atproto-pds-full.md` § D10) ────────────
    //
    // One method per kind, deliberately: the *ceremony* (fetch → sign →
    // provision) is composed in the machine, where the identity key lives, so
    // no seam implementation can be handed a signing key or reorder the steps.

    /// `fauna.bridges.atproto.fetch_authoring_delegation` — the page-load
    /// status read. Returns `(k_pub, cert)`, each absent until that stage of
    /// the ceremony has run. **Mints nothing.**
    async fn fetch_authoring_delegation(
        &self,
    ) -> Result<AuthoringDelegationState, AtprotoSettingsApiError>;

    /// `fauna.bridges.atproto.fetch_authoring_key` — the sub-key's public
    /// half, **minting it on first call**. Step 1 of the mint ceremony.
    async fn fetch_authoring_key(&self) -> Result<[u8; 32], AtprotoSettingsApiError>;

    /// `fauna.bridges.atproto.provision_authoring_delegation` — step 3: upload
    /// the identity-signed cert. The nest re-verifies it and stores nothing on
    /// refusal.
    async fn provision_authoring_delegation(
        &self,
        cert: Vec<u8>,
    ) -> Result<(), AtprotoSettingsApiError>;

    /// `fauna.bridges.atproto.revoke_authoring_delegation` — `false` when
    /// there was nothing to revoke (a no-op success).
    async fn revoke_authoring_delegation(&self) -> Result<bool, AtprotoSettingsApiError>;

    // ── The OAuth consent ceremony's app half (F4 rung 2) ────────────────

    /// `fauna.bridges.atproto.list_pending_consents` — the live requests this
    /// caller may answer, oldest first.
    ///
    /// The push (`fauna.atproto.consent_requested`) is a *nudge to re-read*,
    /// never the card's only source: it is best-effort like every nudge here, so
    /// an app that was closed when it fired finds the card only through this —
    /// and an unassigned request (a PAR with no `login_hint`) fans out to nobody
    /// at all.
    async fn list_pending_consents(&self) -> Result<Vec<NestConsentRow>, AtprotoSettingsApiError>;

    /// `fauna.bridges.atproto.resolve_consent` — approve (`true`) or decline
    /// (`false`) one request. Returns whether **this** call is what resolved it.
    ///
    /// A `false` return is not an error: nest decides ownership, replay and
    /// expiry in the UPDATE's own `WHERE` clause, so "another device answered",
    /// "it expired" and "not yours" are one indistinguishable answer by
    /// construction. The caller re-lists rather than guessing which.
    async fn resolve_consent(
        &self,
        consent_id: Vec<u8>,
        approved: bool,
    ) -> Result<bool, AtprotoSettingsApiError>;

    /// `fauna.capabilities.mint` — deposit the consent-time grant an approve
    /// of a records consent prepared (`crate::consent_grant`), before the
    /// resolve.
    async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), AtprotoSettingsApiError>;

    /// `fauna.capabilities.revoke` — withdraw a deposited consent grant whose
    /// resolve did not land.
    async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), AtprotoSettingsApiError>;
}

/// What the nest currently holds for the account's authoring delegation — the
/// raw shape of the status read, before the machine verifies the cert and turns
/// it into a renderable row.
///
/// Both fields absent is the never-ran-the-ceremony case; `k_pub` present with
/// `cert` absent is a real interrupted-ceremony state that authorizes nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthoringDelegationState {
    pub k_pub: Option<[u8; 32]>,
    /// The stored cert's canonical embed-as-bytes wire, **verbatim**. The
    /// machine verifies it under the account's own identity key rather than
    /// trusting the nest's copy.
    pub cert: Option<Vec<u8>>,
    /// Epoch-millis of the last external-app write that applied under this
    /// delegation; `None` = never used. **Advisory only** — unlike `cert`, nothing signs this, so it is not
    /// proof of use and not proof of non-use (D10 § Audit).
    pub last_used_at: Option<i64>,
}
