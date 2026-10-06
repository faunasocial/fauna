//! Typed-call wrapper for the **user-class** `fauna.bridges.atproto.*` kinds —
//! the ATProto login-plane surface a Fauna app drives from the Bluesky
//! bridge-settings page (`docs/goal/behavior/atproto-pds-full.md` § Client
//! surface): app-credential mint / list / revoke, the session registry, and
//! the per-account external-apps kill-switch.
//!
//! Scope is deliberately the *client* half of the namespace. The bridge-class
//! kinds (`fetch_app_credential_verifiers`, `record_session`,
//! `refresh_session`, `end_session`, `fetch_session_secret_blob`) are called
//! by the attested
//! `atproto.pds` bridge over its own service-user connection and are refused to
//! clients by `bridge_method_allowlist` — they have no business on a client call
//! surface and are not wrapped here.
//!
//! Pattern: the sibling of [`crate::BridgesClient`] / [`crate::MailAccountClient`]
//! — a thin `struct { nest: R }`, one async method per kind, no state. The
//! state machine that drives it lives in `fauna-atproto-settings-machine`
//! (the same two-layer split as `fauna-client-labelers` ↔
//! `fauna-labeler-catalog-machine`, and `MailAccountClient` ↔
//! `fauna-client-mail-settings`). Generic over `R: RpcRequester` so native
//! (`Arc<NestClient>`) and wasm (`WsRpcClient`) share one implementation
//! (priority #2).

use fauna_protocol::RpcRequester;
use fauna_protocol::atproto_pds::{
    DeletePresenceReply, DeletePresenceRequest, FetchAuthoringDelegationReply,
    FetchAuthoringDelegationRequest, FetchAuthoringKeyReply, FetchAuthoringKeyRequest,
    GetIntegrationStatusReply, GetIntegrationStatusRequest, ListAppCredentialsReply,
    ListAppCredentialsRequest, ListGrantsReply, ListGrantsRequest, ListPendingConsentsReply,
    ListPendingConsentsRequest, ListSessionsReply, ListSessionsRequest,
    ProvisionAppCredentialReply, ProvisionAppCredentialRequest, ProvisionAuthoringDelegationReply,
    ProvisionAuthoringDelegationRequest, RecordTombstoneReply, RecordTombstoneRequest,
    RequestTombstoneReply, RequestTombstoneRequest, ResolveConsentReply, ResolveConsentRequest,
    RevokeAppCredentialReply, RevokeAppCredentialRequest, RevokeAuthoringDelegationReply,
    RevokeAuthoringDelegationRequest, RevokeSessionReply, RevokeSessionRequest,
    SetExternalAppsEnabledReply, SetExternalAppsEnabledRequest, SetIntegrationLevelReply,
    SetIntegrationLevelRequest,
};

pub use fauna_protocol::atproto_pds;

/// Typed `fauna.bridges.atproto.*` user-class call surface. Caller-scoped by
/// construction — no `actor_id` parameter anywhere: every kind is self-scoped,
/// the connection knows its caller, and the handlers resolve *that* actor's
/// rows. Errors propagate as the transport's `R::Error` (native
/// `NestClientError`, wasm rpc-wasm error); the namespaced `RpcError`s the
/// handlers emit surface through that channel.
pub struct AtprotoSettingsClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> AtprotoSettingsClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.bridges.atproto.list_app_credentials` — every app-credential row
    /// for the calling actor (label / `dm_allowed` / created / last-used; never
    /// verifier material), **plus** the account's `external_apps_enabled`
    /// kill-switch state, which rides along deliberately so the settings page
    /// renders the toggle from the same fetch it lists rows with. Replay-safe
    /// read.
    pub async fn list_app_credentials(&self) -> Result<ListAppCredentialsReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.atproto.list_app_credentials",
                ListAppCredentialsRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.atproto.provision_app_credential` — store the
    /// PHC-serialized Argon2id `verifier` the *client* computed at mint. The
    /// nest never sees the secret (D3 custody split); the recoverable copy is
    /// the caller's to persist into its `fauna.state.atproto` row.
    pub async fn provision_app_credential(
        &self,
        credential_id: impl Into<String>,
        label: impl Into<String>,
        verifier: impl Into<String>,
        dm_allowed: bool,
    ) -> Result<ProvisionAppCredentialReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.atproto.provision_app_credential",
                ProvisionAppCredentialRequest {
                    credential_id: credential_id.into(),
                    label: label.into(),
                    verifier: verifier.into(),
                    dm_allowed,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.bridges.atproto.revoke_app_credential` — idempotent (`revoked:
    /// false` when no such row existed). Cascades nest-side: every session
    /// minted from the credential is revoked and the bridge is nudged
    /// (`sessions_changed`) to drop cached state immediately, so the reply's
    /// `sessions_revoked` is a fact about work already done, not a promise.
    pub async fn revoke_app_credential(
        &self,
        credential_id: impl Into<String>,
    ) -> Result<RevokeAppCredentialReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.atproto.revoke_app_credential",
                RevokeAppCredentialRequest {
                    credential_id: credential_id.into(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.bridges.atproto.list_sessions` — live (unrevoked, unexpired)
    /// sessions for the calling actor. Replay-safe read.
    pub async fn list_sessions(&self) -> Result<ListSessionsReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.atproto.list_sessions",
                ListSessionsRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.atproto.list_grants` — live OAuth grants for the calling
    /// actor: the richer half of the connected-apps row (F4 slice 8a).
    /// Replay-safe read.
    ///
    /// Revoking one of these is [`Self::revoke_session`] with its `grant_id` —
    /// the grant id *is* the session-family id, so there is no `revoke_grant`
    /// to call.
    pub async fn list_grants(&self) -> Result<ListGrantsReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.atproto.list_grants",
                ListGrantsRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.atproto.revoke_session` — kill one live session by id.
    /// Idempotent (`revoked: false` when already gone).
    ///
    /// Takes an OAuth grant's `grant_id` too: revoking there cascades to the
    /// grant row in the same transaction (F4 slice 8a).
    pub async fn revoke_session(
        &self,
        session_id: Vec<u8>,
    ) -> Result<RevokeSessionReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.atproto.revoke_session",
                RevokeSessionRequest {
                    session_id,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.bridges.atproto.set_external_apps_enabled` — the per-account
    /// kill-switch (default ON). OFF suspends the whole external-app plane
    /// **non-destructively**: rows are kept and stay individually revocable,
    /// and a flip emits `sessions_changed` so the bridge's cached flag updates
    /// immediately rather than at token expiry.
    pub async fn set_external_apps_enabled(
        &self,
        enabled: bool,
    ) -> Result<SetExternalAppsEnabledReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.atproto.set_external_apps_enabled",
                SetExternalAppsEnabledRequest {
                    enabled,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.bridges.atproto.get_integration_status` — the caller's current
    /// integration level, the real-domain gate verdict (nest-computed, so a
    /// stale client-side domain cache can never flip it), and their hosted
    /// identity summary, in one self-scoped fetch. Replay-safe read; the
    /// `atproto` page machine renders from this.
    pub async fn get_integration_status(&self) -> Result<GetIntegrationStatusReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.atproto.get_integration_status",
                GetIntegrationStatusRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.atproto.set_integration_level` — the depth selector's
    /// one transition call: nest composes every per-rung effect server-side
    /// (`docs/goal/ui/atproto.md` § Transition semantics) and replies with the
    /// level actually in force, so a client that raced another device
    /// converges on nest's truth. The mint parameters are read only when the
    /// transition enters a hosted level with no identity row yet; pass empty
    /// strings / `false` otherwise.
    pub async fn set_integration_level(
        &self,
        target_level: impl Into<String>,
        did_method: impl Into<String>,
        user_rotation_pub_did_key: impl Into<String>,
        history_backfill: bool,
    ) -> Result<SetIntegrationLevelReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.atproto.set_integration_level",
                SetIntegrationLevelRequest {
                    target_level: target_level.into(),
                    did_method: did_method.into(),
                    user_rotation_pub_did_key: user_rotation_pub_did_key.into(),
                    history_backfill,
                    extra: Default::default(),
                },
            )
            .await
    }

    // ── D10 delegated authoring (§ D10 → Mint ceremony) ──────────────────
    //
    // The ceremony is ordered and the order is load-bearing: fetch `K_pub`
    // first (which mints it), sign the cert over *that* key, then provision.
    // A cert minted against a guessed or stale key is refused by the nest's
    // check 3, so there is no shortcut past the fetch.

    /// `fauna.bridges.atproto.fetch_authoring_key` — the account's authoring
    /// sub-key public half, **minting the sub-key on the first call**
    /// (provision-on-read, first-write-wins). The secret never leaves the nest.
    ///
    /// Because this mints, it is a *ceremony* call, not a page-load read — use
    /// [`Self::fetch_authoring_delegation`] to render status.
    pub async fn fetch_authoring_key(&self) -> Result<FetchAuthoringKeyReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.atproto.fetch_authoring_key",
                FetchAuthoringKeyRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.atproto.fetch_authoring_delegation` — read back the
    /// stored delegation (cert bytes verbatim, plus `k_pub` when a sub-key
    /// exists) so the page can render its status row. A pure read: it mints
    /// nothing, so opening the page never creates a sub-key.
    ///
    /// Verify the returned cert with
    /// [`crate::atproto_delegation::parse_delegation_cert`] rather than
    /// trusting it — that is what makes this read authoritative about what the
    /// account itself signed.
    pub async fn fetch_authoring_delegation(
        &self,
    ) -> Result<FetchAuthoringDelegationReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.atproto.fetch_authoring_delegation",
                FetchAuthoringDelegationRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.atproto.provision_authoring_delegation` — upload the
    /// identity-signed cert built by
    /// [`crate::atproto_delegation::build_authoring_delegation_cert`]. The nest
    /// runs the five-check verify (§ Mint ceremony) and refuses without
    /// storing anything on failure.
    pub async fn provision_authoring_delegation(
        &self,
        cert: Vec<u8>,
    ) -> Result<ProvisionAuthoringDelegationReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.atproto.provision_authoring_delegation",
                ProvisionAuthoringDelegationRequest {
                    cert,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.bridges.atproto.revoke_authoring_delegation` — destroy the
    /// sub-key and its cert (§ Revocation). Already-published posts stay
    /// verifiable forever from the cert embedded in their own wire; what stops
    /// is *future* authoring. Idempotent: revoking an absent delegation is a
    /// no-op success, since the desired end-state already holds.
    pub async fn revoke_authoring_delegation(
        &self,
    ) -> Result<RevokeAuthoringDelegationReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.atproto.revoke_authoring_delegation",
                RevokeAuthoringDelegationRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.atproto.delete_presence` — the "separate, stronger
    /// action" beside the reversible step-down (`atproto-pds-bridge.md`
    /// § Disable & revocation layer 2), driven by `atproto-delete-confirm`.
    ///
    /// Takes no parameters, deliberately: every choice the flow offers is a
    /// choice about *whether* to proceed, which the client's confirm ceremony
    /// settles before calling. Idempotent — `newly_deleted = false` is a retry
    /// or a second device, and a **success**, not an error to surface.
    ///
    /// The reply is modest on purpose. At the instant it is sent only the
    /// *decision* is durable: nest has recorded the tombstone and stepped the
    /// account down to `off`, and the bridge sweeps the projected records and
    /// announces the deletion on its own convergent pass. A caller that renders
    /// "your records are gone" off this reply is claiming something nest is in
    /// no position to say (`atproto-pds-bridge.md` § Disable & revocation, the
    /// honest caveat).
    pub async fn delete_presence(&self) -> Result<DeletePresenceReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.atproto.delete_presence",
                DeletePresenceRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.atproto.request_tombstone` — record the durable opt-in
    /// intent for the "also permanently retire this identity" tick, inside the
    /// delete ceremony (S5 slice 5b). Wire-enforced precondition: the presence
    /// must already be recorded deleted. Idempotent; `newly_requested = false`
    /// is a retry or a second device, and a success.
    ///
    /// Callers write the user's consent into the account plane custody's
    /// `tombstone_consents` BEFORE this call (the settings machine's
    /// `request_tombstone` gesture owns that ordering) — the converge pass
    /// publishes only when the nest's intent and that client-authored record
    /// agree.
    pub async fn request_tombstone(&self) -> Result<RequestTombstoneReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.atproto.request_tombstone",
                RequestTombstoneRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.atproto.record_tombstone` — report that the PLC directory
    /// accepted the tombstone this client signed, so nest can move the identity
    /// to its terminal state (S5 slice 5b).
    ///
    /// Testimony, not proof: nest holds no key that could sign the operation
    /// and never reads the directory, so this call is the only way it learns the
    /// outcome. Idempotent — a client that crashed between submitting and
    /// reporting re-reports, and a log found *already* tombstoned is reported
    /// the same way, with an empty `prev_cid` because this client chained
    /// nothing itself.
    pub async fn record_tombstone(
        &self,
        prev_cid: String,
    ) -> Result<RecordTombstoneReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.atproto.record_tombstone",
                RecordTombstoneRequest {
                    prev_cid,
                    extra: Default::default(),
                },
            )
            .await
    }

    // ── The OAuth consent ceremony's app half (F4 rung 2) ────────────────
    //
    // Only the user's half of the ceremony is wrapped. Opening a consent
    // request is the nest's own `/oauth/authorize`, never a client kind: a user
    // who could *open* one could push an approval card naming any client they
    // liked, which is exactly the phishing shape the binding code exists to
    // make visible (`atproto-pds-full.md` § F4 detail).

    /// `fauna.bridges.atproto.list_pending_consents` — the live consent requests
    /// this caller may answer, oldest first, including the unassigned ones (a
    /// PAR that carried no `login_hint`, which any account on this nest may
    /// claim by approving).
    ///
    /// **Not optional, and not merely a fallback.** The own-device
    /// `fauna.atproto.consent_requested` push is best-effort like every nudge in
    /// this system, so an app that was closed when it fired has no other way to
    /// find the card at all — and an unassigned request fans out to nobody by
    /// construction, so this read is the *only* way one is ever seen. Replay-safe
    /// read.
    pub async fn list_pending_consents(&self) -> Result<ListPendingConsentsReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.atproto.list_pending_consents",
                ListPendingConsentsRequest::default(),
            )
            .await
    }

    /// `fauna.bridges.atproto.resolve_consent` — the approval (or decline)
    /// itself. D3 rung 2's whole trust root is that this call arrives over the
    /// caller's own authed WS-RPC connection: the grant is Ed25519-rooted and the
    /// browser never holds a Fauna secret.
    ///
    /// `resolved: false` is a normal answer, not an error — there was nothing
    /// live to resolve: already answered (possibly on the user's other device),
    /// expired, or not this caller's row. Nest decides that with the UPDATE's own
    /// `WHERE` clause, so the three are indistinguishable by construction and the
    /// app re-lists rather than guessing which.
    pub async fn resolve_consent(
        &self,
        consent_id: Vec<u8>,
        approved: bool,
    ) -> Result<ResolveConsentReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.atproto.resolve_consent",
                ResolveConsentRequest {
                    consent_id,
                    approved,
                    ..Default::default()
                },
            )
            .await
    }
}
