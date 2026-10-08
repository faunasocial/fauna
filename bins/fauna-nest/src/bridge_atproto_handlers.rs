//! WS-RPC handlers for the `fauna.bridges.atproto.*` F1 auth-core surface
//! (`docs/goal/behavior/atproto-pds-full.md` § Detailed design → *WS-RPC kind
//! surface* / *F1 detail*): the attested `atproto.pds` bridge's verifier
//! fetch + session registry, and the client-facing app-credential
//! mint/list/revoke + session list/revoke + external-apps kill-switch.
//!
//! Custody split: the nest stores PHC verifier strings and session-registry
//! rows only — the credential secret never reaches it, and Argon2id runs
//! bridge-side. Caller-class enforcement uses `bridge_method_allowlist`
//! (`BridgeAtprotoPds` for the bridge kinds; User/Admin, self-scoped, for
//! the client kinds).

use std::sync::Arc;
use std::time::Duration;

use serde_bytes::ByteBuf;

use fauna_protocol::atproto::IntegrationLevel;
use fauna_protocol::atproto_pds::{
    AppCredentialInfo, AppCredentialVerifierRow, AtprotoConsentRequestedPush, AtprotoGrantInfo,
    AtprotoIdentitySummary, AtprotoSessionInfo, DeletePresenceReply, DeletePresenceRequest,
    DeliverPermissionSetReply, DeliverPermissionSetRequest, EndSessionReply, EndSessionRequest,
    FetchAppCredentialVerifiersReply, FetchAppCredentialVerifiersRequest,
    FetchAuthoringDelegationReply, FetchAuthoringDelegationRequest, FetchAuthoringKeyReply,
    FetchAuthoringKeyRequest, FetchPreferencesReply, FetchPreferencesRequest,
    GetIntegrationStatusReply, GetIntegrationStatusRequest, ListAppCredentialsReply,
    ListAppCredentialsRequest, ListGrantsReply, ListGrantsRequest, ListPendingConsentsReply,
    ListPendingConsentsRequest, ListSessionsReply, ListSessionsRequest, PREFERENCES_MAX_BYTES,
    PendingConsentRow, ProvisionAppCredentialReply, ProvisionAppCredentialRequest,
    ProvisionAuthoringDelegationReply, ProvisionAuthoringDelegationRequest, RecordBlobReply,
    RecordBlobRequest, RecordSessionReply, RecordSessionRequest, RecordTombstoneReply,
    RecordTombstoneRequest, RefreshSessionReply, RefreshSessionRequest, RequestTombstoneReply,
    RequestTombstoneRequest, ResolveConsentReply, ResolveConsentRequest, RevokeAppCredentialReply,
    RevokeAppCredentialRequest, RevokeAuthoringDelegationReply, RevokeAuthoringDelegationRequest,
    RevokeSessionReply, RevokeSessionRequest, SetExternalAppsEnabledReply,
    SetExternalAppsEnabledRequest, SetIntegrationLevelReply, SetIntegrationLevelRequest,
    StorePreferencesReply, StorePreferencesRequest, refresh_status,
};
#[cfg(feature = "bluesky")]
use fauna_protocol::atproto_pds::{
    EXTERNAL_WRITE_BATCH_MAX, EXTERNAL_WRITE_RECORD_MAX_BYTES, ExternalWrite, ExternalWriteRefusal,
    ExternalWriteResult, IngestExternalWriteReply, IngestExternalWriteRequest,
    external_write_action,
};
use fauna_protocol::bridge_atproto::{
    FetchAtprotoIssuerJwksReply, FetchAtprotoIssuerJwksRequest, FetchProfileReply,
    FetchProfileRequest, FetchPublicPostsReply, FetchPublicPostsRequest, IssuerJwk, PublicPostItem,
    PublicPostsCursor, public_post_item_kind,
};
use fauna_protocol::{RpcError, Value, decode_strict as decode};

use crate::bridge_routing_handlers::{malformed, require_class};
use crate::db::atproto_identities::TombstoneRecordOutcome;
use crate::db::atproto_pds::{
    ConsentRequestRow, ConsentStartKind, CredentialWriteError, RefreshOutcome,
};
use crate::db::bridge_service_users::{BridgeRole, BridgeStatus};
use crate::routes::AppState;
use crate::rpc_errors::internal;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers ─────────────────────────────────────────────────────

use crate::rpc_errors::encode_reply;

fn rate_limited() -> RpcError {
    crate::rpc_errors::rate_limited_ns("bridges")
}

/// The account's external-apps kill-switch is OFF — the whole external-app
/// plane is suspended (F1 detail). Distinct code so the bridge maps it to
/// the same uniform `AuthenticationRequired` it sends for a wrong secret
/// (no enumeration signal ever leaves the box).
fn external_apps_disabled() -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.atproto.disabled",
        "error.bridges.atproto.disabled",
    );
    e.details = Some(Box::new(Value::String(
        "external apps are disabled for this account".into(),
    )));
    e
}

/// `(actor_id, credential_id)` is already taken. Refusing beats upserting:
/// a second device whose collision-avoid has not seen the first device's
/// mint would otherwise silently retire that credential's secret. The client
/// surfaces provision errors, so the race degrades to a visible retry.
fn credential_exists() -> RpcError {
    RpcError::new(
        "fauna.bridges.atproto.credential_exists",
        "error.bridges.atproto.credential_exists",
    )
    .with_details_text("an app credential with that id already exists")
}

fn parse_actor_id(bytes: &[u8]) -> Result<[u8; 32], RpcError> {
    crate::rpc_errors::require_bytes32("actor_id", bytes).map_err(malformed)
}

/// The session planes F1/F4 record. Anything else is a malformed request.
const VALID_PLANES: &[&str] = &["app_credential", "oauth"];

/// Resolve an XRPC login identifier to a local account. Today: the bare
/// Fauna handle, with a first-label fallback for the `handle.domain` form
/// third-party login boxes send. DID resolution activates when the mirror
/// chain's identity table (S2) lands — until then a DID resolves to no
/// account. A wrong fallback resolution is harmless: the fetched verifier
/// rows still require the credential secret to verify, and failures are
/// uniform.
pub(crate) async fn resolve_identifier(
    state: &Arc<AppState>,
    identifier: &str,
) -> anyhow::Result<Option<[u8; 32]>> {
    let identifier = identifier.trim();
    if identifier.is_empty() {
        return Ok(None);
    }
    // A DID identifier resolves against the stored `did` column — the same
    // value the account's session `sub` carries since slice 4d, so an app can
    // log back in with the DID it was handed. Looked up, never parsed for an
    // actor id: the placeholder DID that could be decoded back into one is
    // exactly what 4d removed.
    if identifier.starts_with("did:") {
        return state.db.resolve_actor_by_atproto_did(identifier).await;
    }
    if let Some(actor) = state.db.resolve_handle(identifier).await? {
        return Ok(Some(actor));
    }
    if let Some((first_label, rest)) = identifier.split_once('.')
        && !first_label.is_empty()
        && !rest.is_empty()
    {
        return state.db.resolve_handle(first_label).await;
    }
    Ok(None)
}

/// Whether this deployment actually runs the out-of-process ATProto PDS bridge —
/// i.e. an **approved** `atproto.pds` bridge service user exists.
///
/// This is the deployment-level "is ATProto hosting on?" signal for surfaces
/// *outside* the atproto wire path that must react to it: the apex ACME order's
/// `pds.<apex>` SAN ([`crate::acme_http01::pds_san_included`]) and the admin
/// expected-DNS matrix's `pds.<primary>` row ([`crate::dns_handlers`]). Both need
/// to know whether anything is listening on `pds.<apex>` before telling an admin
/// to publish that record / letting the name into the all-or-nothing ACME order.
///
/// Approval is the right signal because the `atproto.pds` role is **never**
/// auto-approved by any enable toggle — it always takes the manual admin approval
/// card (`BridgeRole::AtprotoPds` docs; `mail-bridge-lifecycle.md` § Onboarding
/// auto-approval), so an approved row means an admin deliberately stood the
/// bridge up. (The cleaner `atproto_enabled` deployment state arrives with mirror
/// S4 — `atproto-pds-full.md` § Implementation status today; this is its
/// today-queryable equivalent, not a placeholder to rip out.)
///
/// Fail-safe: a DB error reads as `false`, so a transient failure omits the SAN /
/// the advisory row rather than risking the apex order.
pub(crate) async fn atproto_pds_bridge_approved(state: &AppState) -> bool {
    match state
        .db
        .list_bridge_service_users(Some(BridgeRole::AtprotoPds), Some(BridgeStatus::Approved))
        .await
    {
        Ok(rows) => !rows.is_empty(),
        Err(e) => {
            tracing::warn!(
                target: "bridge_rpc",
                error = %format!("{e:#}"),
                "atproto_pds_bridge_approved: list failed; treating as not-approved"
            );
            false
        }
    }
}

/// Nudge every approved `atproto.pds` bridge that an account's external-app
/// state changed (session/credential revoke, kill-switch flip) — the
/// `sessions_changed` push (nudge + poll-fallback: delivery is best-effort
/// by design, the bridge re-fetches on its own schedule too).
/// `external_apps_enabled` is `Some(new value)` only on a kill-switch flip
/// (the bridge updates its cached flag directly from the nudge); `None` on
/// session/credential revokes.
pub(crate) async fn notify_atproto_sessions_changed(
    state: &Arc<AppState>,
    actor_id: &[u8; 32],
    external_apps_enabled: Option<bool>,
) {
    let bridges = match state.db.list_approved_bridge_service_users().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                target: "bridge_rpc",
                error = %format!("{e:#}"),
                "sessions_changed: failed to list approved bridges; skipping push (bridge re-fetches on poll)"
            );
            return;
        }
    };
    for bridge in bridges {
        if bridge.role != BridgeRole::AtprotoPds {
            continue;
        }
        state.ws.notify_push(
            &bridge.ed25519_pubkey,
            fauna_protocol::PushEvent::BridgeAtprotoSessionsChanged(
                fauna_protocol::atproto_pds::BridgeAtprotoSessionsChangedPush {
                    actor_id: actor_id.to_vec(),
                    external_apps_enabled,
                    extra: Default::default(),
                },
            ),
        );
    }
}

/// Nudge every approved `atproto.pds` bridge that the admin rotated the
/// **nest's** OAuth issuer key set, through either arm (TP5 / S2d leg 1) — the
/// `issuer_key_rotated` push. Hint-less: the bridge answers by re-fetching
/// `fetch_issuer_jwks`, which IS the state.
///
/// Best-effort by design (nudge + poll-fallback, the `sessions_changed`
/// pattern) — but the latency it buys matters here. Under the forced arm the leaked
/// `kid` leaves the nest's served set at once, and a verifier keeps honouring
/// it until the verifier re-reads: this push is what makes that seconds rather
/// than the refresh loop's ticker (`authorization-server.md` § The issuer →
/// *Two rotation arms*, "what the forced arm does not bound").
pub(crate) async fn notify_atproto_issuer_key_rotated(state: &Arc<AppState>) {
    let bridges = match state.db.list_approved_bridge_service_users().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                target: "bridge_rpc",
                error = %format!("{e:#}"),
                "issuer_key_rotated: failed to list approved bridges; skipping push \
                 (bridge re-fetches on reconnect/poll/unknown-kid)"
            );
            return;
        }
    };
    for bridge in bridges {
        if bridge.role != BridgeRole::AtprotoPds {
            continue;
        }
        state.ws.notify_push(
            &bridge.ed25519_pubkey,
            fauna_protocol::PushEvent::BridgeAtprotoIssuerKeyRotated(
                fauna_protocol::atproto_pds::BridgeAtprotoIssuerKeyRotatedPush {
                    extra: Default::default(),
                },
            ),
        );
    }
}

/// Nudge every approved `atproto.pds` bridge that projection-relevant content
/// landed for a local account — a public post was created, a post was deleted
/// (tombstone journal row written), or a profile was set — via a
/// `fauna.bridges.atproto.projection_ready` push (S3, `atproto-pds-bridge.md`
/// § Where logic lives; the `notify_bridges_outbound_ready` pattern). The
/// bridge answers by pulling `fetch_public_posts` / `fetch_profile` from its
/// stored cursor. `actor_hint` scopes the pull to one user when known; `None`
/// means "sweep the roster". Best-effort: a disconnected bridge's emit is a
/// no-op (`notify_push` drops it); its periodic poll is the correctness
/// backstop.
pub(crate) async fn notify_bridges_atproto_projection_ready(
    state: &Arc<AppState>,
    actor_hint: Option<[u8; 32]>,
) {
    let bridges = match state.db.list_approved_bridge_service_users().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                target: "bridge_rpc",
                error = %format!("{e:#}"),
                "projection_ready: failed to list approved bridges; skipping push (bridge catches up on its next poll)"
            );
            return;
        }
    };
    for bridge in bridges {
        if bridge.role != BridgeRole::AtprotoPds {
            continue;
        }
        state.ws.notify_push(
            &bridge.ed25519_pubkey,
            fauna_protocol::PushEvent::BridgeAtprotoProjectionReady(
                fauna_protocol::atproto_pds::BridgeAtprotoProjectionReadyPush {
                    actor_id: actor_hint.map(|a| ByteBuf::from(a.to_vec())),
                    ..Default::default()
                },
            ),
        );
    }
}

// ── Bridge-class handlers ───────────────────────────────────────

fn fetch_app_credential_verifiers_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &bridge_actor,
                "fauna.bridges.atproto.fetch_app_credential_verifiers",
            )
            .await?;
            let req: FetchAppCredentialVerifiersRequest = decode(&payload).map_err(malformed)?;
            let resolved = resolve_identifier(&state, &req.identifier)
                .await
                .map_err(internal)?;
            // Per-caller window keyed on the resolved target (zero-actor for
            // unresolved identifiers, so unknown-identifier floods share one
            // bucket) plus the identifier string.
            let target = resolved.unwrap_or([0u8; 32]);
            if !state
                .bridge_rate_limit
                .check(&bridge_actor, &target, &req.identifier)
            {
                return Err(rate_limited());
            }
            let Some(actor) = resolved else {
                return encode_reply(&FetchAppCredentialVerifiersReply {
                    actor_id: None,
                    external_apps_enabled: true,
                    login_did: None,
                    verifiers: Vec::new(),
                    extra: Default::default(),
                });
            };
            let enabled = state
                .db
                .get_atproto_external_apps_enabled(&actor)
                .await
                .map_err(internal)?;
            // The DID the account may open a session as: its real one, and only
            // while the identity is ACTIVE. Two rules in one field, both held
            // here so the bridge never interprets identity status (slice 4d).
            //
            // `pending` has no DID to serve a repo under. `deactivated` /
            // `deleted` is the layer-2 step-down, which
            // `atproto-pds-bridge.md` § Disable & revocation ratifies as
            // suspending the account's ENTIRE ATProto presence, live app
            // sessions included — revoking those sessions is not enough on its
            // own, since nothing stopped the app logging straight back in.
            let login_did = state
                .db
                .get_atproto_identity(&actor)
                .await
                .map_err(internal)?
                .filter(|row| row.status == "active")
                .and_then(|row| row.did);
            let verifiers = state
                .db
                .list_atproto_app_credentials(&actor)
                .await
                .map_err(internal)?
                .into_iter()
                .map(|c| AppCredentialVerifierRow {
                    credential_id: c.credential_id,
                    verifier: c.verifier,
                    dm_allowed: c.dm_allowed,
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&FetchAppCredentialVerifiersReply {
                actor_id: Some(ByteBuf::from(actor.to_vec())),
                external_apps_enabled: enabled,
                login_did,
                verifiers,
                extra: Default::default(),
            })
        })
    })
}

fn record_session_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &bridge_actor,
                "fauna.bridges.atproto.record_session",
            )
            .await?;
            let req: RecordSessionRequest = decode(&payload).map_err(malformed)?;
            let actor = parse_actor_id(&req.actor_id)?;
            if !VALID_PLANES.contains(&req.plane.as_str()) {
                return Err(malformed(format!("unknown session plane {:?}", req.plane)));
            }
            // Defense-in-depth: the bridge already refuses createSession on
            // its cached flag; re-check here so a stale cache can never
            // register a session for a disabled account.
            if !state
                .db
                .get_atproto_external_apps_enabled(&actor)
                .await
                .map_err(internal)?
            {
                return Err(external_apps_disabled());
            }
            state
                .db
                .insert_atproto_session(
                    &actor,
                    &req.session_id,
                    &req.plane,
                    req.credential_id.as_deref(),
                    req.client_note.as_deref(),
                    req.expires_at,
                )
                .await
                .map_err(malformed)?;
            if let Some(credential_id) = &req.credential_id {
                state
                    .db
                    .touch_atproto_credential_last_used(&actor, credential_id)
                    .await
                    .map_err(internal)?;
            }
            encode_reply(&RecordSessionReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn refresh_session_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &bridge_actor,
                "fauna.bridges.atproto.refresh_session",
            )
            .await?;
            let req: RefreshSessionRequest = decode(&payload).map_err(malformed)?;
            let actor = parse_actor_id(&req.actor_id)?;
            let status = rotate_session(
                &state,
                &actor,
                &req.session_id,
                &req.presented_jti,
                &req.new_jti,
                req.new_expires_at,
            )
            // `internal`, not `malformed`: `malformed` tells a caller its
            // payload was bad, and a storage failure is not the caller's fault.
            .await
            .map_err(internal)?;
            encode_reply(&RefreshSessionReply {
                status: status.to_string(),
                extra: Default::default(),
            })
        })
    })
}

fn end_session_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(&state, &bridge_actor, "fauna.bridges.atproto.end_session").await?;
            let req: EndSessionRequest = decode(&payload).map_err(malformed)?;
            let actor = parse_actor_id(&req.actor_id)?;
            let ended = end_oauth_session(&state, &actor, &req.session_id)
                .await
                .map_err(internal)?;
            encode_reply(&EndSessionReply {
                ended,
                extra: Default::default(),
            })
        })
    })
}

/// `app.bsky.actor.getPreferences` — read the account's opaque preferences
/// payload. The kill-switch is *not* re-checked here: D8 (the bridge's single
/// authorization decision point) already denies every authenticated call for a
/// disabled account, so the bridge never reaches this kind for one — and
/// re-enforcing it nest-side would be the second decision point D8 exists to
/// prevent.
fn fetch_preferences_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &bridge_actor,
                "fauna.bridges.atproto.fetch_preferences",
            )
            .await?;
            let req: FetchPreferencesRequest = decode(&payload).map_err(malformed)?;
            let actor = parse_actor_id(&req.actor_id)?;
            let preferences = state
                .db
                .get_atproto_preferences(&actor)
                .await
                .map_err(internal)?;
            encode_reply(&FetchPreferencesReply {
                preferences: preferences.map(ByteBuf::from),
                extra: Default::default(),
            })
        })
    })
}

/// `app.bsky.actor.putPreferences` — overwrite the account's opaque
/// preferences payload. The blob is passed straight to storage (D2: the nest
/// invents no Fauna concept); the only policy is the hard size cap
/// ([`PREFERENCES_MAX_BYTES`]), the source-of-truth re-check behind the
/// bridge's own pre-check.
fn store_preferences_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &bridge_actor,
                "fauna.bridges.atproto.store_preferences",
            )
            .await?;
            let req: StorePreferencesRequest = decode(&payload).map_err(malformed)?;
            let actor = parse_actor_id(&req.actor_id)?;
            if req.preferences.len() > PREFERENCES_MAX_BYTES {
                return Err(malformed(format!(
                    "preferences payload is {} bytes, over the {PREFERENCES_MAX_BYTES}-byte cap",
                    req.preferences.len()
                )));
            }
            state
                .db
                .set_atproto_preferences(&actor, &req.preferences)
                .await
                .map_err(internal)?;
            encode_reply(&StorePreferencesReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.atproto.record_blob` — tie an uploaded blob's ATProto CID to
/// the Fauna media `ContentHash` its bytes landed under (F2.4 slice 1).
///
/// The bytes themselves never come through here: they reach the nest over the
/// sanctioned bulk-binary carve-out (`POST /api/v1/blob`), which is the one
/// non-WS-RPC internal surface and the only one built to stream megabytes. What
/// that route *cannot* know is which account uploaded — it authenticates as the
/// bridge's own service user and keeps its `uploader` for audit only — so the
/// account attribution arrives here, on the side holding the authenticated
/// `fauna_actor`. Two legs, each carrying what its own surface actually knows.
///
/// The kill-switch is not re-checked, for the same reason `fetch_preferences`
/// does not: D8 is the single authorization decision point and has already
/// denied every authenticated call for a disabled account.
fn record_blob_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(&state, &bridge_actor, "fauna.bridges.atproto.record_blob").await?;
            let req: RecordBlobRequest = decode(&payload).map_err(malformed)?;
            let actor = parse_actor_id(&req.actor_id)?;
            // A `ContentHash` is a blake3 digest — fixed 32 bytes. Anything else
            // is a bridge bug, and storing it would leave a row naming media
            // that can never resolve.
            let media_ref: [u8; 32] = req.media_ref.as_slice().try_into().map_err(|_| {
                malformed(format!(
                    "media_ref is {} bytes, want a 32-byte blake3 content hash",
                    req.media_ref.len()
                ))
            })?;
            if req.cid.is_empty() {
                return Err(malformed("record_blob carries no cid"));
            }
            state
                .db
                .upsert_atproto_blob(&actor, &req.cid, &media_ref)
                .await
                .map_err(internal)?;
            encode_reply(&RecordBlobReply {
                ok: true,
                // Spelled here, not bridge-side: this is where the `ContentHash`
                // type and its canonical base32 form are owned, and the bridge
                // matches this string against what the outbound extraction
                // produces. A hand-rolled Go copy that drifted by a character
                // would re-fetch and re-hash every image forever, silently.
                fauna_cid: fauna_cbor::Cid::from_digest_raw(media_ref).to_base32(),
                extra: Default::default(),
            })
        })
    })
}

// ── User-class handlers (self-scoped; Admin ⊇ User) ─────────────

fn provision_app_credential_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.atproto.provision_app_credential",
            )
            .await?;
            let req: ProvisionAppCredentialRequest = decode(&payload).map_err(malformed)?;
            let credential_id = req.credential_id.trim();
            if credential_id.is_empty() {
                return Err(malformed(
                    "provision_app_credential: credential_id is empty",
                ));
            }
            if req.label.trim().is_empty() {
                return Err(malformed("provision_app_credential: label is empty"));
            }
            // The client computes the verifier; hold it to the one shape the
            // bridge can verify (self-describing Argon2id PHC). A malformed
            // verifier would otherwise only surface as uniform login failure.
            if !req.verifier.starts_with("$argon2id$") {
                return Err(malformed(
                    "provision_app_credential: verifier is not an Argon2id PHC string",
                ));
            }
            state
                .db
                .put_atproto_app_credential(
                    &actor_id,
                    credential_id,
                    req.label.trim(),
                    &req.verifier,
                    req.dm_allowed,
                )
                .await
                .map_err(|e| match e {
                    CredentialWriteError::Duplicate => credential_exists(),
                    CredentialWriteError::Other(e) => malformed(e),
                })?;
            encode_reply(&ProvisionAppCredentialReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn list_app_credentials_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.atproto.list_app_credentials",
            )
            .await?;
            let _req: ListAppCredentialsRequest = decode(&payload).map_err(malformed)?;
            let credentials = state
                .db
                .list_atproto_app_credentials(&actor_id)
                .await
                .map_err(internal)?
                .into_iter()
                .map(|c| AppCredentialInfo {
                    credential_id: c.credential_id,
                    label: c.label,
                    dm_allowed: c.dm_allowed,
                    created_at: c.created_at,
                    last_used_at: c.last_used_at,
                    extra: Default::default(),
                })
                .collect();
            let external_apps_enabled = state
                .db
                .get_atproto_external_apps_enabled(&actor_id)
                .await
                .map_err(internal)?;
            encode_reply(&ListAppCredentialsReply {
                credentials,
                external_apps_enabled,
                extra: Default::default(),
            })
        })
    })
}

fn revoke_app_credential_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.atproto.revoke_app_credential",
            )
            .await?;
            let req: RevokeAppCredentialRequest = decode(&payload).map_err(malformed)?;
            let (revoked, sessions_revoked) = state
                .db
                .revoke_atproto_app_credential(&actor_id, &req.credential_id)
                .await
                .map_err(internal)?;
            if revoked {
                notify_atproto_sessions_changed(&state, &actor_id, None).await;
            }
            encode_reply(&RevokeAppCredentialReply {
                revoked,
                sessions_revoked,
                extra: Default::default(),
            })
        })
    })
}

fn list_sessions_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.atproto.list_sessions").await?;
            let _req: ListSessionsRequest = decode(&payload).map_err(malformed)?;
            let sessions = state
                .db
                .list_atproto_sessions(&actor_id)
                .await
                .map_err(internal)?
                .into_iter()
                .map(|s| AtprotoSessionInfo {
                    session_id: s.session_id,
                    plane: s.plane,
                    credential_id: s.credential_id,
                    client_note: s.client_note,
                    created_at: s.created_at,
                    last_refreshed_at: s.last_refreshed_at,
                    expires_at: s.expires_at,
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&ListSessionsReply {
                sessions,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.atproto.list_grants` — the connected-apps registry read
/// (F4 slice 8a, USER class, self-scoped).
///
/// Self-scoped by construction: the grants returned are the caller's own,
/// because `actor_id` is the authenticated connection's, never a request
/// field. The kill-switch is deliberately not consulted — a disabled account
/// must still be able to SEE and revoke what it once granted; the switch
/// suspends the external plane, it does not hide the audit surface.
fn list_grants_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.atproto.list_grants").await?;
            let _req: ListGrantsRequest = decode(&payload).map_err(malformed)?;
            let grants = state
                .db
                .list_atproto_oauth_grants(&actor_id)
                .await
                .map_err(internal)?
                .into_iter()
                .map(|g| AtprotoGrantInfo {
                    grant_id: g.grant_id,
                    client_id: g.client_id,
                    client_name: g.client_name,
                    scopes: g.scopes,
                    sets: g.sets,
                    created_at: g.created_at,
                    last_used_at: g.last_used_at,
                    expires_at: g.expires_at,
                    suspended: g.suspended,
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&ListGrantsReply {
                grants,
                extra: Default::default(),
            })
        })
    })
}

fn revoke_session_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.atproto.revoke_session").await?;
            let req: RevokeSessionRequest = decode(&payload).map_err(malformed)?;
            let revoked = state
                .db
                .revoke_atproto_session(&actor_id, &req.session_id)
                .await
                .map_err(internal)?;
            if revoked {
                notify_atproto_sessions_changed(&state, &actor_id, None).await;
            }
            encode_reply(&RevokeSessionReply {
                revoked,
                extra: Default::default(),
            })
        })
    })
}

fn set_external_apps_enabled_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.atproto.set_external_apps_enabled",
            )
            .await?;
            let req: SetExternalAppsEnabledRequest = decode(&payload).map_err(malformed)?;
            state
                .db
                .set_atproto_external_apps_enabled(&actor_id, req.enabled)
                .await
                .map_err(internal)?;
            // OFF also closes the account's live third-party principal
            // sessions on the nest, 4401, after the write: every dispatch on
            // them re-reads the flag and would refuse anyway, but a refused
            // socket left open is still a Push recipient once the event door
            // exists — the roster revoke's reasoning
            // (`principals_handlers::revoke_handler`). ON closes nothing: the
            // client dials again with its token, or refreshes.
            if !req.enabled {
                state.ws.disconnect_account_principals(&actor_id);
            }
            // Both directions nudge: OFF must reach the bridge immediately
            // (live access tokens start refusing per-request), and ON
            // restores service without waiting for a poll.
            notify_atproto_sessions_changed(&state, &actor_id, Some(req.enabled)).await;
            encode_reply(&SetExternalAppsEnabledReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── OAuth consent ceremony (F4, D3 rung 2) ──────────────────────

/// How long an unanswered consent request stays live, in milliseconds.
///
/// This bounds a **human** — unlocking a phone, finding the Fauna app, reading
/// the code off the browser and comparing it — which is why it is minutes and
/// not seconds. Ten is comfortably past that walk while short enough that a
/// forgotten card does not sit on the page.
///
/// ⚠ It is deliberately **its own constant**, not a reuse of any F4 freshness
/// number (the DPoP window, the assertion cap, the PAR TTL). Those bound
/// cryptographic artifacts; this bounds a person. Sharing one would mean a
/// future change to either silently moved the other — the trap an earlier review
/// records, in the one direction F4 has already declined to follow.
pub(crate) const CONSENT_REQUEST_TTL_MILLIS: i64 = 10 * 60 * 1000;

/// Project a stored row into the shape apps render. `logo_uri` is absent by
/// construction — nothing stores it here, so nothing can leak the user's
/// address by loading it (§ F4 detail's ceremony ruling).
pub(crate) fn pending_consent_row(row: &ConsentRequestRow) -> PendingConsentRow {
    PendingConsentRow {
        consent_id: row.consent_id.clone(),
        code: row.code.clone(),
        client_id: row.client_id.clone(),
        client_name: row.client_name.clone(),
        scopes: split_scopes(&row.scopes),
        sets: row.sets.clone(),
        created_at: row.created_at,
        expires_at: row.expires_at,
        holder_x25519: row
            .binding
            .attested
            .holder_x25519
            .map(|k| serde_bytes::ByteBuf::from(k.to_vec())),
        writer_ed25519: row
            .binding
            .attested
            .writer_ed25519
            .map(|k| serde_bytes::ByteBuf::from(k.to_vec())),
        fauna_manifest: row.binding.fauna_manifest.clone(),
        install: None,
        extra: Default::default(),
    }
}

/// [`pending_consent_row`], with an install card's install section filled
/// from the verified install the plugin runner holds for it
/// (`crate::plugin_runner::PendingInstall`) — the projection every surface
/// that shows a card to an app goes through.
pub(crate) fn project_consent_row(state: &AppState, row: &ConsentRequestRow) -> PendingConsentRow {
    let mut out = pending_consent_row(row);
    if row.start == ConsentStartKind::Install {
        out.install = state.plugins.pending_info(&row.consent_id).map(Box::new);
    }
    out
}

/// The scope set's one spelling, both directions: space-delimited, exactly as
/// the OAuth `scope` parameter carries it. Storing the joined form and
/// splitting on read means nothing re-encodes what the user approved.
fn split_scopes(scopes: &str) -> Vec<String> {
    scopes
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>()
}

/// Open a pending consent request and fan it out to the named account's own
/// clients — **the one owner of that act**, called by the nest's own
/// `/oauth/authorize` (`crate::oauth_as_routes`).
///
/// The *rules* here are the interesting part:
///
/// * **The `login_hint` is resolved here and never reported back.** `None` from
///   an unknown identifier and `None` from an absent hint are deliberately the
///   same value: the authorize page is anonymous and attacker-reachable, so a
///   distinguishable answer would make the OAuth flow a handle-enumeration
///   oracle.
/// * **The binding code is minted by the party that also stores the row the
///   approval card renders**, so the browser and the card cannot be shown
///   different codes.
/// * **Own-device fan-out only.** An unassigned request reaches nobody by
///   construction — there is no account to reach — and is found through
///   `list_pending_consents` instead.
///
/// The caller validates its own inputs (every endpoint has already validated
/// the request against the client's document before it gets here).
///
/// # One owner for every start (`authorization-server.md` § Consent)
///
/// The four starts reach one card, so they open rows through this one
/// function and differ only in the [`ConsentStart`] they name:
///
/// * **Browser** — as above: the hint may be absent or unresolved, and the row
///   is then unassigned. Always opens a row.
/// * **Typed code** — no hint at all: the row is unassigned and listed to
///   nobody until the account that types its code claims it, so there is
///   nobody to notify. Always opens a row.
/// * **Push** — TP9's three rules, all decided here so no endpoint can hold a
///   second opinion about them. An unresolved hint and a client the account
///   has blocked (rule (c)) both open **nothing** and answer `None` — the
///   caller's reply is the same either way, so neither is an oracle. A row
///   replaces the client's earlier live push to the same account (rule (b),
///   in the storage layer). And it notifies only a client this account has
///   approved before (rule (a)); anything else lands in the pending list
///   alone.
/// * **Handoff** — the account is the caller of
///   `fauna.oauth.consent.open_handoff`, never a resolved hint: the user who
///   opened the route is the user who answers. TP9's rules do not apply — the
///   user asked for this card, so it notifies, a repeat is the user opening
///   it again, and the block governs the quiet push only. Always opens a row.
#[allow(clippy::too_many_arguments)] // the request's facts, flat, as every start holds them
pub(crate) async fn open_consent_request(
    state: &Arc<AppState>,
    start: ConsentStart<'_>,
    client_id: &str,
    client_name: Option<&str>,
    scopes: &[String],
    sets: &[fauna_protocol::atproto_pds::ConsentSetInfo],
    binding: &crate::db::atproto_pds::ConsentBinding,
) -> anyhow::Result<Option<ConsentRequestRow>> {
    let (kind, actor, notify) = match start {
        ConsentStart::Browser { login_hint } => {
            let actor = match login_hint {
                Some(hint) => resolve_identifier(state, hint).await?,
                None => None,
            };
            (ConsentStartKind::Browser, actor, actor.is_some())
        }
        ConsentStart::TypedCode => (ConsentStartKind::TypedCode, None, false),
        ConsentStart::Push {
            login_hint,
            dpop_jkt,
            authenticated,
        } => {
            let Some(actor) = resolve_identifier(state, login_hint).await? else {
                return Ok(None);
            };
            if state.db.oauth_client_blocked(&actor, client_id).await? {
                return Ok(None);
            }
            // Rule (a). What counts as "this client" is what the request
            // PROVED: an authenticated client proved its `client_id`, so any
            // prior grant to that client counts; a public client proved only
            // its DPoP key, so only a grant bound to that key does — otherwise
            // anyone could use an approved public client's name to put a
            // notification in front of the user and collect the tokens.
            let key = (!authenticated).then_some(dpop_jkt);
            let approved_before = state
                .db
                .atproto_oauth_client_approved_before(&actor, client_id, key)
                .await?;
            (ConsentStartKind::Push, Some(actor), approved_before)
        }
        ConsentStart::Handoff { actor } => (ConsentStartKind::Handoff, Some(actor), true),
        // No fan-out from here: the card's install section is held by the
        // plugin runner only once this row's id exists, so
        // `fauna.plugins.install` pushes the card itself after holding it.
        ConsentStart::Install { admin } => (ConsentStartKind::Install, Some(admin), false),
    };
    let row = state
        .db
        .open_atproto_consent_request_for(
            kind,
            actor,
            client_id,
            client_name,
            &scopes.join(" "),
            sets,
            binding,
            CONSENT_REQUEST_TTL_MILLIS,
        )
        .await?;

    if let (true, Some(actor)) = (notify, actor) {
        state.ws.notify_push(
            &actor,
            fauna_protocol::PushEvent::AtprotoConsentRequested(AtprotoConsentRequestedPush {
                consent: project_consent_row(state, &row),
                extra: Default::default(),
            }),
        );
    }
    Ok(Some(row))
}

/// Which consent start is opening a row, with what that start carries.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ConsentStart<'a> {
    /// `/oauth/authorize`, with the PAR's `login_hint` if it carried one.
    Browser { login_hint: Option<&'a str> },
    /// `/oauth/device_authorization`.
    TypedCode,
    /// `/oauth/bc-authorize`.
    Push {
        login_hint: &'a str,
        /// The thumbprint of the key the request was proved under.
        dpop_jkt: &'a str,
        /// Whether the client authenticated (`private_key_jwt`) — i.e. whether
        /// its `client_id` is proved rather than merely named.
        authenticated: bool,
    },
    /// `fauna.oauth.consent.open_handoff`, by the account that called it.
    Handoff { actor: [u8; 32] },
    /// `fauna.plugins.install`, by the admin that called it — the card is
    /// theirs to answer, and nobody else's.
    Install { admin: [u8; 32] },
}

/// How a consent request stands right now — **the one owner of that reading**,
/// read by the nest's own `/oauth/authorize` long-poll
/// (`crate::oauth_as_routes`).
///
/// Four states and no fifth. `Expired` covers a swept, evicted or never-issued
/// id alike, because all three mean the same thing to every caller: this
/// ceremony cannot proceed. A **resolved** request reports its resolution even
/// past its expiry — the user did answer, and what may still be minted from that
/// answer is the redeeming caller's rule, not one to lose here.
#[derive(Debug, Clone)]
pub(crate) enum ConsentState {
    Pending,
    Approved(Box<ApprovedConsent>),
    Denied,
    Expired,
}

/// An approved consent, read back from the row the user was actually shown.
///
/// ⚠ `scopes` and `sets` come from that row and **never** from the reader's own
/// copy of the request. It is what stops a recorded grant ever being wider than
/// the card — the same rule in both directions of this seam.
#[derive(Debug, Clone)]
pub(crate) struct ApprovedConsent {
    pub actor_id: [u8; 32],
    /// The account's real DID, and only while its ATProto identity is ACTIVE.
    /// `None` means approved by an account that cannot complete an ATProto
    /// flow — the caller decides what that costs, and never interprets status.
    pub login_did: Option<String>,
    pub scopes: Vec<String>,
    pub sets: Vec<fauna_protocol::atproto_pds::ConsentSetInfo>,
}

pub(crate) async fn read_consent_state(
    state: &Arc<AppState>,
    consent_id: &[u8],
) -> anyhow::Result<ConsentState> {
    let Some(row) = state.db.get_atproto_consent_request(consent_id).await? else {
        return Ok(ConsentState::Expired);
    };
    match (row.resolved_at, row.approved) {
        (Some(_), Some(true)) => {
            let actor = actor_id_from_slice(row.actor_id.as_deref().unwrap_or_default())?;
            let login_did = state
                .db
                .get_atproto_identity(&actor)
                .await?
                .filter(|r| r.status == "active")
                .and_then(|r| r.did);
            Ok(ConsentState::Approved(Box::new(ApprovedConsent {
                actor_id: actor,
                login_did,
                scopes: split_scopes(&row.scopes),
                sets: row.sets.clone(),
            })))
        }
        (Some(_), _) => Ok(ConsentState::Denied),
        (None, _) if row.expires_at <= fauna_core::data::Timestamp::now_millis() as i64 => {
            Ok(ConsentState::Expired)
        }
        (None, _) => Ok(ConsentState::Pending),
    }
}

/// A stored actor id, as the fixed-width array every caller wants.
fn actor_id_from_slice(bytes: &[u8]) -> anyhow::Result<[u8; 32]> {
    <[u8; 32]>::try_from(bytes)
        .map_err(|_| anyhow::anyhow!("stored actor id is not 32 bytes: {} bytes", bytes.len()))
}

/// `fauna.bridges.atproto.deliver_permission_set` — BRIDGE class. The
/// bridge→nest half of the permission-set request call
/// ([`crate::oauth_as_permission_sets`]): the answer to one
/// `permission_set_requested` push, handed to the PAR waiting under its
/// `request_id`.
///
/// Bridge-only because the bytes it carries feed an authorization decision as
/// VERIFIED — only the attested PDS host ran the chain that verified them. An
/// answer nobody waits for (the deadline already refused, a shed waiter, an id
/// this nest never minted) is `accepted: false`, which is information for the
/// bridge's log and never an error: a late answer is ordinary.
fn deliver_permission_set_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &bridge_actor,
                "fauna.bridges.atproto.deliver_permission_set",
            )
            .await?;
            let req: DeliverPermissionSetRequest = decode(&payload).map_err(malformed)?;
            let accepted = state
                .oauth_as
                .permission_sets
                .deliver(&req.request_id, req.record.map(ByteBuf::into_vec));
            encode_reply(&DeliverPermissionSetReply {
                accepted,
                extra: Default::default(),
            })
        })
    })
}

/// Whether a grant was recorded — the account's external-apps kill-switch
/// being OFF is a `false`, not an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrantRecorded {
    Yes,
    /// The account is not accepting external application access. Refused as an
    /// authorization failure by every caller, never as a server error: nothing
    /// is broken; this account has turned the plane off.
    ExternalAppsDisabled,
    /// The holder key the client attested at PAR may not be this principal's
    /// (`third-party.md` § The principal model, rule 2) — nothing was recorded.
    /// The client's error, answered as such.
    HolderKeyRefused(crate::db::third_party_principals::HolderKeyRefused),
}

/// Record a grant and open the session family its refresh tokens rotate
/// through — **the one owner of that act**, called by the nest's own
/// `/oauth/token` ([`crate::oauth_as_routes`]).
///
/// One call, one transaction, two rows: the connected-apps grant row and the
/// OAuth session family. See [`crate::db::CacheDb::record_atproto_oauth_grant`]
/// for why they are written together rather than by two kinds.
///
/// The kill-switch re-check is the same defence-in-depth `record_session`
/// carries, and it matters more here: this is the *first* write of a grant the
/// user's app will list as connected, so registering one for an account whose
/// external-app plane is OFF would show a live connection the account has
/// disabled.
#[allow(clippy::too_many_arguments)]
///
/// `issuer` marks WHICH authorization server minted the grant — the
/// nest-hosted AS's own routes pass
/// [`crate::db::atproto_pds::OAUTH_GRANT_ISSUER_NEST`], the one issuer
/// standing since the bridge AS retired. The forced session-secret rotation reads the
/// mark to end exactly the refresh families its re-mint killed.
pub(crate) async fn record_oauth_grant(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    grant_id: &[u8],
    client_id: &str,
    client_name: Option<&str>,
    scopes: &[String],
    sets: &[fauna_protocol::atproto_pds::ConsentSetInfo],
    dpop_jkt: &str,
    session_expires_at: i64,
    grant_expires_at: Option<i64>,
    issuer: &str,
    principal: &crate::db::third_party_principals::PrincipalAttestation,
) -> anyhow::Result<GrantRecorded> {
    if !state.db.get_atproto_external_apps_enabled(actor).await? {
        return Ok(GrantRecorded::ExternalAppsDisabled);
    }
    let recorded = state
        .db
        .record_atproto_oauth_grant(
            actor,
            grant_id,
            client_id,
            client_name,
            // Space-joined, the one spelling the OAuth `scope` parameter uses
            // and the same one the consent row stores — nothing re-encodes it.
            &scopes.join(" "),
            sets,
            dpop_jkt,
            session_expires_at,
            grant_expires_at,
            issuer,
            principal,
        )
        .await;
    match recorded {
        Ok(()) => Ok(GrantRecorded::Yes),
        Err(e) => match e.downcast_ref::<crate::db::third_party_principals::HolderKeyRefused>() {
            Some(refused) => Ok(GrantRecorded::HolderKeyRefused(*refused)),
            None => Err(e),
        },
    }
}

/// Rotate a session family's refresh token — **the one owner of that act**,
/// shared by the bridge-class kind `fauna.bridges.atproto.refresh_session` and
/// by the nest's own `/oauth/token` refresh grant.
///
/// Returns one of [`fauna_protocol::atproto_pds::refresh_status`]'s values. The
/// kill-switch OFF answer is `disabled` rather than an error, so both callers'
/// uniform-failure mapping stays one code path.
///
/// ⚠ This is the **plane-agnostic** rotation the app-credential plane uses too,
/// deliberately: a replayed OAuth refresh token must kill its family exactly as
/// a replayed app-password one does, and a second implementation of that rule
/// is how the two would come to disagree.
///
/// On `REUSE_DETECTED` this also fires the `sessions_changed` nudge, from the
/// one rotation owner so both callers get it for free. **Not** because a
/// user-facing surface reads it — the nudge reaches only approved
/// `atproto.pds` bridges (see [`notify_atproto_sessions_changed`]), and a
/// `None` flag is a no-op there today. The reason is pattern completeness:
/// every other family termination in this plane already nudges from its one
/// owner, and leaving this the sole silent exception is how a future
/// bridge-side session cache — or a future user-facing surface built on this
/// push — would end up missing exactly the termination that is a
/// token-theft signal. Never on `ROTATED`: a successful rotation is the
/// common path and changes nothing any nudge consumer renders — that would
/// be a push on every refresh of every connected app.
pub(crate) async fn rotate_session(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    session_id: &[u8],
    presented_jti: &[u8],
    new_jti: &[u8],
    new_expires_at: i64,
) -> anyhow::Result<&'static str> {
    if !state.db.get_atproto_external_apps_enabled(actor).await? {
        return Ok(refresh_status::DISABLED);
    }
    Ok(
        match state
            .db
            .refresh_atproto_session(actor, session_id, presented_jti, new_jti, new_expires_at)
            .await?
        {
            RefreshOutcome::Rotated => refresh_status::ROTATED,
            RefreshOutcome::ReuseDetected => {
                notify_atproto_sessions_changed(state, actor, None).await;
                refresh_status::REUSE_DETECTED
            }
            RefreshOutcome::Invalid => refresh_status::INVALID,
        },
    )
}

/// End a session family — **the one owner of that act**, shared by the
/// bridge-class kind `fauna.bridges.atproto.end_session` and by the nest's own
/// `/oauth/revoke`.
///
/// Because the grant id **is** the session-family id, this is the whole of
/// revocation: the storage layer marks the session row and cascades to its
/// grant row in one transaction. There is deliberately no `revoke_grant`
/// spelling — one operation with one name, so the user's app, this endpoint and
/// the reuse family-kill cannot come to disagree about what revocation means.
///
/// The nudge fires for the same reason the user-driven revoke's does: it
/// reaches approved `atproto.pds` bridges (never the user's own app —
/// `connected-apps.md`'s surface is unbuilt, and reads on open rather than
/// on push), so a bridge's cached session state does not lag the nest's
/// canonical revocation until its next poll.
pub(crate) async fn end_oauth_session(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    session_id: &[u8],
) -> anyhow::Result<bool> {
    let ended = state.db.revoke_atproto_session(actor, session_id).await?;
    if ended {
        notify_atproto_sessions_changed(state, actor, None).await;
    }
    Ok(ended)
}

/// End **every live grant the nest-hosted AS minted**, across every account —
/// the forced session-secret rotation's other half, and its one owner.
///
/// Re-minting that secret kills every refresh family MACed under it, so leaving
/// the grant rows alone would list N dead connections in each user's
/// connected-apps surface — real scopes, `revoked_at` NULL, a `last_used_at`
/// reporting use of a credential that cannot be used — for up to 180 days. That
/// is the state `authorization-server.md` § The issuer → *Grants recorded while
/// the flow is unhonoured are ended at the re-point* rejects, reached through a
/// second door.
///
/// **Here rather than at the calling handler, and that placement is the point.**
/// A loop over [`end_oauth_session`] written at the call site would be a second
/// revocation spelling in a plane whose whole discipline is that there is one —
/// and it would nudge connected-apps once per GRANT, so an account holding
/// three would be pushed at three times for one act. This shares that owner's
/// revocation call and collapses the nudge to **one per affected actor**.
///
/// Only `issuer = 'nest'` rows — the families this rotation's secret MACed
/// (§ The issuer → *Two HS256 secrets, not one*). The retired bridge AS's
/// NULL-marked grants were ended at its retirement and the mark is `NOT NULL`
/// since schema 102, so today that is every grant; the filter keeps this
/// response naming what it kills.
///
/// Answers how many grants were actually ended, which is what the reply reports:
/// a grant already revoked is not this response's to claim, and neither is one
/// already past its `expires_at` — the count means *connected-apps rows that
/// disappeared*, so an invisible row would inflate it in the one direction that
/// makes a compromise response sound bigger than it was.
pub(crate) async fn end_nest_issued_oauth_sessions(state: &Arc<AppState>) -> anyhow::Result<u64> {
    // ONE transaction, not a read followed by a loop of per-grant endings.
    // Both shapes share the revocation owner's row writes, but only this one is
    // atomic — and atomicity is what makes the caller's error honest: a failure
    // here ends NOTHING, so "the secret was replaced, the grants were not"
    // describes the whole outcome, with no half-ended set the admin's count
    // cannot name. The storage layer writes the same
    // `revoke_session_row`/`revoke_grant_row` pair `revoke_atproto_session`
    // does, so this is one act's bulk selection rather than a second spelling.
    let ended = state.db.end_nest_minted_oauth_grants().await?;
    // One push per actor, after the endings rather than between them: a surface
    // refreshed mid-sweep would show a half-ended list, and an account holding
    // several grants would be pushed at once per grant — the nudge names an
    // actor, so the extra frames carry no extra information.
    for actor in &ended.actors {
        notify_atproto_sessions_changed(state, actor, None).await;
    }
    Ok(ended.ended)
}

/// `fauna.bridges.atproto.list_pending_consents` — USER class, self-scoped.
/// The poll-fallback for the own-device push, and the only way an unassigned
/// request (a PAR with no `login_hint`) is ever found.
fn list_pending_consents_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.atproto.list_pending_consents",
            )
            .await?;
            let _req: ListPendingConsentsRequest = decode(&payload).map_err(malformed)?;
            let rows = state
                .db
                .list_pending_atproto_consent_requests(&actor_id)
                .await
                .map_err(internal)?;
            encode_reply(&ListPendingConsentsReply {
                consents: rows
                    .iter()
                    .map(|row| project_consent_row(&state, row))
                    .collect(),
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.atproto.resolve_consent` — USER class, self-scoped. The
/// approval itself: D3 rung 2's trust root is that this call arrives over the
/// caller's own authed WS-RPC connection, so the grant is Ed25519-rooted and the
/// browser never holds a Fauna secret.
///
/// Authorization is the storage layer's `WHERE` clause, not a check here: a row
/// belonging to another account, an already-answered row and an expired one are
/// all simply unmatched, so there is no read-then-write window and an approval
/// cannot be replayed into a second grant.
fn resolve_consent_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.atproto.resolve_consent").await?;
            let req: ResolveConsentRequest = decode(&payload).map_err(malformed)?;
            let row = state
                .db
                .get_atproto_consent_request(&req.consent_id)
                .await
                .map_err(internal)?;
            // The card's folder choice (`authorization-server.md` § Scope
            // grammar → *The folder plane's qualifier is the user's*): required
            // to approve a row carrying a bare folder-plane scope, refused on
            // every other answer. A row that is gone is left to the resolve's
            // own uniform `resolved: false`.
            if let Some(row) = &row {
                let bare = row.scopes.split_whitespace().any(|s| {
                    fauna_bridge_atproto::fauna_scope::user_qualified_bare_arm(s).is_some()
                });
                match (req.approved && bare, req.folder) {
                    (true, None) => {
                        return Err(malformed(
                            "approving this request needs the folder the card chose",
                        ));
                    }
                    (false, Some(_)) => {
                        return Err(malformed(
                            "a folder is only chosen when approving a request for one",
                        ));
                    }
                    _ => {}
                }
                if row.start == ConsentStartKind::Install {
                    return resolve_install(&state, &actor_id, &req).await;
                }
            }
            // The chosen folder must be a folder row of the RESOLVING account;
            // anything else resolves nothing — the uniform answer, saying
            // nothing about whose folder an id is.
            if let Some(folder) = req.folder {
                let owned = state
                    .db
                    .get_folder_by_id(folder)
                    .await
                    .map_err(internal)?
                    .is_some_and(|f| f.actor_id.as_slice() == actor_id.as_slice());
                if !owned {
                    return encode_reply(&ResolveConsentReply {
                        resolved: false,
                        extra: Default::default(),
                    });
                }
            }
            let resolved = state
                .db
                .resolve_atproto_consent_request_choosing(
                    &req.consent_id,
                    &actor_id,
                    req.approved,
                    req.folder,
                )
                .await
                .map_err(internal)?;
            if resolved.is_some() {
                // Wake the nest's own `/oauth/authorize` long-poll holding on
                // this ceremony. Best-effort: a lost wake costs one fallback
                // poll interval, never a stuck ceremony.
                state.oauth_as.consent_wakes.wake(&req.consent_id);
            }
            encode_reply(&ResolveConsentReply {
                resolved: resolved.is_some(),
                extra: Default::default(),
            })
        })
    })
}

/// `resolve_consent` on an admin's install card (`third-party.md` § The
/// runner contract → *The install-approval leg*): approving is the ADMIN's
/// act — the card is assigned to the admin who opened it, and an approval by
/// a caller without the Admin class refuses even so — and it mints the
/// hosted plugin from the module the install verified, then starts it. A
/// decline discards the verified module. A card whose module this nest no
/// longer holds (it restarted since the install) is declined and refused:
/// the admin installs again.
async fn resolve_install(
    state: &Arc<AppState>,
    actor_id: &[u8; 32],
    req: &ResolveConsentRequest,
) -> Result<bytes::Bytes, RpcError> {
    if req.approved {
        require_class(state, actor_id, "fauna.plugins.install").await?;
    }
    let held = req.approved && state.plugins.is_pending(&req.consent_id);
    let resolved = state
        .db
        .resolve_atproto_consent_request(&req.consent_id, actor_id, held)
        .await
        .map_err(internal)?;
    let Some(_row) = resolved else {
        return encode_reply(&ResolveConsentReply {
            resolved: false,
            extra: Default::default(),
        });
    };
    let install = state.plugins.take_pending(&req.consent_id);
    if req.approved {
        use crate::plugin_runner::InstallError;
        let install = install.ok_or_else(|| {
            crate::rpc_errors::invalid_request_ns("plugins", InstallError::Expired)
        })?;
        state
            .plugins
            .complete_install(state, install)
            .await
            .map_err(|e| match e {
                InstallError::Other(e)
                    if e.downcast_ref::<crate::db::third_party_principals::InstallRefused>()
                        .is_some() =>
                {
                    crate::rpc_errors::conflict_ns("plugins", e)
                }
                InstallError::Other(e) => internal(e),
                e => crate::rpc_errors::invalid_request_ns("plugins", e),
            })?;
    }
    encode_reply(&ResolveConsentReply {
        resolved: true,
        extra: Default::default(),
    })
}

// ── Authoring delegation (D10) ──────────────────────────────────

/// The nest's Ed25519 signing-key bytes — the root the authoring sub-key's
/// at-rest KEK derives from (`crate::atproto_authoring_key`).
#[cfg(feature = "bluesky")]
fn nest_key_bytes(state: &Arc<AppState>) -> Result<[u8; 32], RpcError> {
    state
        .nest_signing_key
        .as_ref()
        .map(|k| k.to_bytes())
        .ok_or_else(|| internal("nest signing key not configured"))
}

/// `fauna.bridges.atproto.fetch_authoring_key` — USER-class, self-scoped:
/// return the caller's delegated authoring sub-key public key, minting the
/// sub-key nest-side on the first call (`atproto-pds-full.md` D10 § Mint
/// ceremony). Only `K_pub` crosses the wire; the secret half never leaves
/// the nest process.
fn fetch_authoring_key_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.atproto.fetch_authoring_key",
            )
            .await?;
            let _req: FetchAuthoringKeyRequest = decode(&payload).map_err(malformed)?;
            let k_pub = crate::atproto_authoring_key::mint_or_fetch(&state.db, &actor_id)
                .await
                .map_err(internal)?;
            encode_reply(&FetchAuthoringKeyReply {
                k_pub: k_pub.to_vec(),
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.atproto.fetch_authoring_delegation` — USER-class,
/// self-scoped: read back the caller's current authoring delegation so the
/// AT Protocol page can render its status row (D10 § Mint ceremony).
///
/// **Mints nothing.** `fetch_authoring_key` mints `K` on first call, so it
/// cannot double as the page-load status read without creating a sub-key for
/// every user who merely opens the page. This one only reads: an account that
/// never ran the ceremony gets two `None`s and no row is created.
///
/// The cert crosses **verbatim**, not as a nest-side summary, so the client
/// re-verifies the envelope under its own identity key — the nest is not
/// trusted to describe a structure it only stores.
fn fetch_authoring_delegation_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.atproto.fetch_authoring_delegation",
            )
            .await?;
            let _req: FetchAuthoringDelegationRequest = decode(&payload).map_err(malformed)?;
            let row = state
                .db
                .get_atproto_authoring_key(&actor_id)
                .await
                .map_err(internal)?;
            let (k_pub, cert, last_used_at) = match row {
                Some(r) => (
                    Some(serde_bytes::ByteBuf::from(r.k_pub)),
                    r.cert.map(serde_bytes::ByteBuf::from),
                    r.last_used_at,
                ),
                None => (None, None, None),
            };
            encode_reply(&FetchAuthoringDelegationReply {
                k_pub,
                cert,
                // Advisory only — the client renders it as a hint, never as
                // proof of (non-)use. See the wire field's docs.
                last_used_at,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.atproto.provision_authoring_delegation` — USER-class,
/// self-scoped: store the identity-signed delegation cert authorizing the
/// caller's authoring sub-key, after the full D10 verification matrix.
/// Validate-then-write, so a refused cert changes nothing at all.
fn provision_authoring_delegation_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.atproto.provision_authoring_delegation",
            )
            .await?;
            let req: ProvisionAuthoringDelegationRequest = decode(&payload).map_err(malformed)?;
            // The sub-key must already exist: the client fetches (and thereby
            // mints) K first, then signs a cert naming that exact K_pub.
            let row = state
                .db
                .get_atproto_authoring_key(&actor_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| {
                    malformed("no authoring sub-key minted; call fetch_authoring_key first")
                })?;
            let k_pub: [u8; 32] = row
                .k_pub
                .as_slice()
                .try_into()
                .map_err(|_| internal("stored authoring k_pub is not 32 bytes"))?;
            // MICROSECONDS — the cert's `expires_at` is a `Timestamp`, whose
            // unit is microseconds. This read `now_epoch_millis()` until
            // 2026-07-29, which made check 5 dead: every real expiry compared
            // ~1000x larger than "now", so an already-expired cert provisioned
            // successfully.
            let now = fauna_core::data::Timestamp::now().0;
            crate::atproto_authoring_key::verify_delegation_cert(&req.cert, &actor_id, &k_pub, now)
                .map_err(malformed)?;
            state
                .db
                .set_atproto_authoring_key_cert(&actor_id, &req.cert)
                .await
                .map_err(internal)?;
            encode_reply(&ProvisionAuthoringDelegationReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.atproto.revoke_authoring_delegation` — USER-class,
/// self-scoped: destroy the caller's authoring sub-key (D10 § Revocation).
/// The secret, pubkey, and cert all go; already-published posts stay
/// verifiable forever from the cert embedded in their own wire.
fn revoke_authoring_delegation_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.atproto.revoke_authoring_delegation",
            )
            .await?;
            let _req: RevokeAuthoringDelegationRequest = decode(&payload).map_err(malformed)?;
            let revoked = state
                .db
                .delete_atproto_authoring_key(&actor_id)
                .await
                .map_err(internal)?;
            if revoked {
                notify_atproto_sessions_changed(&state, &actor_id, None).await;
            }
            encode_reply(&RevokeAuthoringDelegationReply {
                revoked,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.atproto.set_integration_level` — the depth selector's one
/// transition kind (`docs/goal/ui/atproto.md` § Transition semantics, § Where
/// logic lives). USER-class, self-scoped: the caller moves *their own*
/// integration to the requested level, and nest composes every per-rung effect
/// server-side so the client makes **one call per confirmed transition**.
///
/// **Atomicity, honestly.** The effects span more than one store (the identity
/// row, the session registry, the consume-side provider, a supervisor flag), so
/// a single SQL transaction cannot cover them. What the
/// client-state-recoverability invariant actually demands is that no crash
/// strands the user somewhere they cannot leave — which this gets from two
/// properties instead: **every effect is individually idempotent**, and **the
/// level is written LAST**. A crash mid-transition therefore leaves the OLD
/// level in force over partially-applied effects, and the retry replays the
/// same plan and converges. The level a client observes is never a level whose
/// effects were skipped.
///
/// Effect order is deliberate: unlink → mint/reactivate → deactivate → suspend
/// the plane → boot the bridge → write the level. Teardown precedes buildup so
/// the one-backing rule can never be transiently violated.
fn set_integration_level_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.atproto.set_integration_level",
            )
            .await?;
            let req: SetIntegrationLevelRequest = decode(&payload).map_err(malformed)?;
            let target = IntegrationLevel::from_wire(&req.target_level)
                .ok_or_else(|| malformed(format!("unknown level: {}", req.target_level)))?;

            let current = state
                .db
                .get_atproto_integration_level(&actor_id)
                .await
                .map_err(internal)?;
            let identity = state
                .db
                .get_atproto_identity(&actor_id)
                .await
                .map_err(internal)?;
            let actor_hex = hex::encode(actor_id);
            let bluesky_provider = state
                .bridge
                .providers
                .as_ref()
                .and_then(|registry| registry.get("bluesky"));
            let has_external_link = match &bluesky_provider {
                Some(p) => p
                    .status(&state, &actor_hex)
                    .await
                    .map(|s| s.linked)
                    .unwrap_or(false),
                // No consume-side provider on this build — nothing to unlink.
                None => false,
            };

            let plan = fauna_protocol::atproto::TransitionPlan::for_move(
                current,
                target,
                fauna_protocol::atproto::LevelContext {
                    has_external_link,
                    // A retired identity is not a restorable one: its DID no
                    // longer resolves anywhere, so there is nothing to
                    // reactivate and the plan must say *mint* — which is also
                    // what makes the client's card describe a new identity and
                    // send the mint parameters for one.
                    has_identity: identity.as_ref().is_some_and(|i| i.status != "tombstoned"),
                },
            );

            // A mint needs its parameters up front: validating here means a
            // rejected request has changed nothing at all.
            if plan.mints_identity {
                match req.did_method.as_str() {
                    "plc" => {
                        fauna_protocol::atproto::decode_did_key(&req.user_rotation_pub_did_key)
                            .map_err(|e| malformed(format!("user rotation pubkey: {e}")))?;
                    }
                    "web" => {
                        if !req.user_rotation_pub_did_key.is_empty() {
                            return Err(malformed(
                                "did:web has no rotation keys; pubkey must be empty",
                            ));
                        }
                    }
                    other => {
                        return Err(malformed(format!(
                            "entering a hosted level needs did_method plc|web, got: {other:?}"
                        )));
                    }
                }
            }

            // ── effects, teardown before buildup ──
            if plan.unlinks_external
                && let Some(p) = &bluesky_provider
            {
                // The external account itself is untouched — it keeps existing
                // on its own PDS, merely no longer connected.
                p.unlink(&state, &actor_hex)
                    .await
                    .map_err(crate::bridges_ui_handlers::bridge_error_to_rpc)?;
            }
            if plan.suspends_login_plane {
                let revoked = state
                    .db
                    .revoke_all_atproto_sessions(&actor_id)
                    .await
                    .map_err(internal)?;
                // Credential and grant rows are KEPT and individually
                // revocable, so stepping back up restores usability — this
                // call kills only the session, deliberately leaving the
                // grant row at rest. `list_atproto_oauth_grants` derives
                // `suspended` from the now-dead paired session, so the
                // connected-apps read reflects the teardown with no second
                // write here.
                tracing::info!(
                    target: "bridge_rpc",
                    revoked,
                    "suspended the atproto login plane on step-down"
                );
                notify_atproto_sessions_changed(&state, &actor_id, Some(false)).await;
            }
            if plan.deactivates_identity {
                // Layer-2 deactivation: reversible. The DID and its sealed key
                // blob are retained, so re-entering a hosted level restores the
                // same identity. The bridge observes the status on its roster
                // read and stops serving the repo / announces `#account`.
                state
                    .db
                    .set_atproto_identity_active(&actor_id, false)
                    .await
                    .map_err(internal)?;
                // D10 § Revocation: un-host / deactivation cascades the same
                // destroy as an explicit `revoke_authoring_delegation` — the
                // server's delegated authoring credential must not outlive the
                // hosting it was granted for. Unlike the DID this is NOT
                // retained across the round trip: re-entering a hosted level
                // re-mints a fresh K and the client re-provisions a fresh cert
                // (the deliberate asymmetry — a server-held signing credential
                // is re-consented, not silently restored). Posts already
                // published stay verifiable from their embedded cert.
                // Idempotent, like every effect here, so the retry converges.
                state
                    .db
                    .delete_atproto_authoring_key(&actor_id)
                    .await
                    .map_err(internal)?;
            }
            if plan.mints_identity {
                // A previously RETIRED identity is archived out of the way
                // first, so the mint below inserts rather than colliding with a
                // row that can never become live again. The archive is
                // append-only: the destroyed DID, its provenance and its public
                // key halves survive this, because a DID that no longer resolves
                // leaves that record as the only thing anyone can still say
                // about it. Idempotent (a no-op when there is nothing retired),
                // so a retried transition converges.
                let archived = state
                    .db
                    .archive_retired_atproto_identity(&actor_id)
                    .await
                    .map_err(internal)?;
                if archived {
                    tracing::info!(
                        target: "bridge_rpc",
                        "archived a retired atproto identity; minting a fresh one for this actor"
                    );
                }
                state
                    .db
                    .upsert_atproto_identity_intent(
                        &actor_id,
                        &req.did_method,
                        &req.user_rotation_pub_did_key,
                    )
                    .await
                    .map_err(internal)?;
                state
                    .db
                    .set_atproto_history_backfill(&actor_id, req.history_backfill)
                    .await
                    .map_err(internal)?;
            }
            if plan.reactivates_identity {
                state
                    .db
                    .set_atproto_identity_active(&actor_id, true)
                    .await
                    .map_err(internal)?;
            }
            if target.is_hosted() && !current.is_hosted() {
                // First user to reach a hosted level boots the PDS bridge; the
                // flag write is idempotent and later users are a no-op.
                match crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path) {
                    Some(dir) => {
                        if let Err(e) = crate::mail_enable::set_atproto_enabled(&dir, true).await {
                            // Non-fatal: the durable flag is what the
                            // run-script reads at the next boot, and the
                            // reconcile derives "should be up" from any user
                            // sitting at a hosted level.
                            tracing::warn!(
                                target: "bridge_rpc",
                                error = %e,
                                "could not raise the atproto-enabled flag"
                            );
                        }
                    }
                    None => tracing::warn!(
                        target: "bridge_rpc",
                        "no data dir for the atproto-enabled flag"
                    ),
                }
            }

            // Written LAST — see the atomicity note above.
            state
                .db
                .set_atproto_integration_level(&actor_id, target)
                .await
                .map_err(internal)?;

            // Nudge the bridge so a layer-2 status change lands on the network
            // NOW rather than on the ≤30 s projection poll. § Disable &
            // revocation says projection stops "immediately", and withdrawing
            // consent to publish is exactly the direction that must not idle: a
            // step-down keeps serving the repo until the bridge next looks.
            // Both directions nudge (the reactivation case is the same argument
            // inverted — the user asked to be visible again), matching the
            // sessions_changed precedent above. Best-effort by construction: the
            // poll remains the correctness backstop, so a dropped push only
            // costs latency (`notify_bridges_atproto_projection_ready`).
            if plan.deactivates_identity || plan.reactivates_identity {
                notify_bridges_atproto_projection_ready(&state, Some(actor_id)).await;
            }

            encode_reply(&SetIntegrationLevelReply {
                level: target.as_str().into(),
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.atproto.delete_presence` — the wire behind
/// `atproto-delete-presence` (`docs/goal/ui/atproto.md` § User actions).
/// USER-class, self-scoped: the "separate, stronger action" beside the
/// reversible step-down (`atproto-pds-bridge.md` § Disable & revocation layer
/// 2).
///
/// What this handler does is record a DECISION and tear down the account-side
/// halves; the destruction itself is the bridge's, on its own convergent pass
/// (it sweeps every projected record with real `deleteRecord` commits,
/// announces `#account(deleted)`, then purges the repo). Splitting it that way
/// is not laziness about latency — the records must be deleted by the single
/// per-user repo writer, signed with the bridge-custodied key nest cannot read.
///
/// Two things are deliberately NOT destroyed. The **identity** survives: the DID
/// row and its sealed key blob are retained, which is what makes the section's
/// "still reversible in identity terms" literally true and lets a later
/// re-enable restore the same DID rather than mint a second one. And the
/// **projection floor** survives, because it is the record of what the user
/// consented to publish — the boundary a later re-enable is measured against,
/// not part of the presence being destroyed.
///
/// The identity's *terminal* destruction — the PLC tombstone — is deliberately
/// unreachable from here: it must be signed with the user's senior rotation key,
/// which lives only in their client (§ State & data shape), so it is an explicit
/// opt-in sub-flow the client performs directly against the PLC directory, never
/// implicit in this sweep (`../ui/atproto.md` § Don't do these).
///
/// The level lands on `off`. `ui/atproto.md` § Transition semantics calls the
/// delete action "orthogonal to level" — meaning it is not one of the selector's
/// transitions — but leaving the selector parked on a hosted rung whose presence
/// has just been destroyed would offer to restore something and then not, so the
/// level follows the presence down.
fn delete_presence_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.atproto.delete_presence").await?;
            let _req: DeletePresenceRequest = decode(&payload).map_err(malformed)?;

            // There must be something to delete. A missing row is the caller's
            // mistake, not a silent success: a client that renders the button
            // with no identity has misread its own status snapshot.
            if state
                .db
                .get_atproto_identity(&actor_id)
                .await
                .map_err(internal)?
                .is_none()
            {
                return Err(no_hosted_identity());
            }

            // ── effects, in the same discipline as the transition kind:
            // every one idempotent, the level written LAST, so a crash leaves
            // the old level over partially-applied effects and the retry
            // converges (no SQL transaction can span these four stores).

            // The login plane goes first and unconditionally. Whatever the
            // current level, a presence that is being destroyed must not keep
            // answering external app sessions while the sweep runs.
            let revoked = state
                .db
                .revoke_all_atproto_sessions(&actor_id)
                .await
                .map_err(internal)?;
            notify_atproto_sessions_changed(&state, &actor_id, Some(false)).await;

            // D10 § Revocation: the server-held delegated authoring credential
            // must not outlive the hosting it was granted for — the same
            // cascade a step-down performs.
            state
                .db
                .delete_atproto_authoring_key(&actor_id)
                .await
                .map_err(internal)?;

            // The durable tombstone. This is the row the bridge converges on,
            // and the reason the sweep needs no `deleting` state and no
            // completion call back into nest.
            let newly_deleted = state
                .db
                .mark_atproto_identity_deleted(&actor_id)
                .await
                .map_err(internal)?;

            state
                .db
                .set_atproto_integration_level(&actor_id, IntegrationLevel::Off)
                .await
                .map_err(internal)?;

            tracing::info!(
                target: "bridge_rpc",
                revoked,
                newly_deleted,
                "recorded a Bluesky presence deletion; the bridge sweeps the repo"
            );

            // Same argument as the step-down nudge, only stronger: withdrawing
            // consent to publish must not idle behind the ≤30 s poll, and here
            // the user has asked for the records to be *destroyed*. Best-effort
            // — the poll stays the correctness backstop.
            notify_bridges_atproto_projection_ready(&state, Some(actor_id)).await;

            encode_reply(&DeletePresenceReply {
                level: IntegrationLevel::Off.as_str().into(),
                newly_deleted,
                extra: Default::default(),
            })
        })
    })
}

/// The identity is not one a PLC tombstone can retire — either it never minted
/// a DID, or it is did:web, whose custody *is* domain custody and which has no
/// operation log to tombstone (`atproto-pds-bridge.md` § Identity: DID method).
fn not_tombstoneable() -> RpcError {
    RpcError::new(
        "fauna.bridges.atproto.not_tombstoneable",
        "error.bridges.atproto.not_tombstoneable",
    )
    .with_details_text(
        "only a minted did:plc identity whose presence has been deleted can be retired",
    )
}

/// A client reported a published PLC tombstone for an identity this nest holds
/// no opt-in for. Distinct from [`not_tombstoneable`], which refuses the *ask*:
/// this refuses the *report*, and it is a divergence rather than a user error —
/// the client believes it destroyed a DID whose row here is still restorable.
fn tombstone_not_authorized() -> RpcError {
    RpcError::new(
        "fauna.bridges.atproto.tombstone_not_authorized",
        "error.bridges.atproto.tombstone_not_authorized",
    )
    .with_details_text(
        "no tombstone opt-in stands for this identity; nothing was recorded as retired",
    )
}

/// `fauna.bridges.atproto.request_tombstone` — record the "also permanently
/// retire this identity" opt-in taken inside the delete-presence ceremony
/// (`atproto-pds-bridge.md` § Disable & revocation layer 2). USER-class,
/// self-scoped, idempotent.
///
/// This kind exists because the tombstone is the one identity act that spans a
/// client crash. Signing it needs the senior rotation key, which lives only in
/// the user's client, and submitting it must wait for the presence sweep to
/// finish — so the client performs it on a later converge pass, not in the
/// gesture that confirms it. The intent therefore has to be durable *here*: a
/// client that died between the confirm and the submit would otherwise leave the
/// user believing an identity was retired when nothing was ever published.
///
/// The precondition — the presence must already be recorded deleted — is what
/// makes `ui/atproto.md` § Don't do these ("never a step-down, never implicit")
/// true on the wire rather than a rule each of the seven apps is trusted to
/// keep. It also encodes the ordering the act depends on, for a reason that is
/// not about our own bookkeeping: a tombstoned DID stops resolving, and a relay
/// that cannot resolve a DID cannot verify that DID's commits — so retiring
/// before the sweep has published its deletes would strand exactly the
/// tombstones the delete flow exists to propagate.
fn request_tombstone_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.atproto.request_tombstone").await?;
            let _req: RequestTombstoneRequest = decode(&payload).map_err(malformed)?;

            let identity = state
                .db
                .get_atproto_identity(&actor_id)
                .await
                .map_err(internal)?
                .ok_or_else(no_hosted_identity)?;

            // Refuse the un-retirable cases with their own error rather than
            // reporting a silent no-op: a client that asked has misread its own
            // status snapshot, and "nothing happened" is the one answer that
            // would let it go on believing the retirement is under way.
            if identity.method != "plc" || identity.did.is_none() {
                return Err(not_tombstoneable());
            }
            if identity.status != "deleted" && identity.status != "tombstoned" {
                return Err(not_tombstoneable());
            }

            let newly_requested = state
                .db
                .request_atproto_tombstone(&actor_id)
                .await
                .map_err(internal)?;

            tracing::info!(
                target: "bridge_rpc",
                newly_requested,
                "recorded a PLC tombstone opt-in; the client signs and submits it"
            );

            encode_reply(&RequestTombstoneReply {
                newly_requested,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.atproto.record_tombstone` — the client reporting that the PLC
/// directory accepted the tombstone it signed (S5 slice 5b). USER-class,
/// self-scoped, idempotent.
///
/// Nest records testimony, not a verified fact, and the asymmetry is inherent
/// rather than a shortcut: it holds no key that could sign the operation and
/// makes no connection of its own to the directory, so it has no independent
/// view of the outcome and does not pretend to one. The client sends this for a
/// log it found *already* tombstoned as well, which is what lets a crash between
/// submitting and reporting converge instead of leaving the row a state behind
/// the network.
///
/// Nothing is torn down here. Everything a retirement destroys was destroyed by
/// the delete-presence sweep this kind's precondition requires; what changes is
/// only that the identity stops being restorable.
///
/// Testimony is not the same as permission, which is why the report of an act
/// this nest never authorized is **refused** rather than answered with a
/// success. The two are not distinguishable by "did a row change?" — both
/// change nothing — and the difference matters: an accepted-but-unauthorized
/// report leaves the row `'deleted'`, and a `'deleted'` row still reactivates,
/// so the user could later be re-parked on a hosted rung backing a DID their
/// client says it destroyed. A client in that state has diverged from its own
/// status snapshot; saying so loudly is more useful to it than a silent no-op.
fn record_tombstone_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.atproto.record_tombstone").await?;
            let req: RecordTombstoneRequest = decode(&payload).map_err(malformed)?;

            if state
                .db
                .get_atproto_identity(&actor_id)
                .await
                .map_err(internal)?
                .is_none()
            {
                return Err(no_hosted_identity());
            }

            let newly_tombstoned = match state
                .db
                .mark_atproto_identity_tombstoned(&actor_id)
                .await
                .map_err(internal)?
            {
                TombstoneRecordOutcome::Recorded => true,
                TombstoneRecordOutcome::AlreadyRetired => false,
                TombstoneRecordOutcome::NotAuthorized => {
                    tracing::warn!(
                        target: "bridge_rpc",
                        prev_cid = %req.prev_cid,
                        "a client reported a PLC tombstone this nest never authorized; \
                         refusing rather than leaving a restorable row behind a retired DID"
                    );
                    return Err(tombstone_not_authorized());
                }
            };

            tracing::info!(
                target: "bridge_rpc",
                newly_tombstoned,
                prev_cid = %req.prev_cid,
                "recorded a completed PLC tombstone; the identity is retired"
            );

            encode_reply(&RecordTombstoneReply {
                newly_tombstoned,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.atproto.get_integration_status` — the USER-class,
/// self-scoped read the `atproto` page machine renders from: current level,
/// real-domain gate verdict, and the caller's hosted identity summary in one
/// fetch (`docs/goal/ui/atproto.md` § State & data shape).
///
/// The gate verdict is computed here, with the same predicate the identities
/// roster applies (`fetch_atproto_identities_handler`), so the greyed rung a
/// user sees and the roster the bridge serves can never disagree — and the
/// client needs no domain plumbing of its own (a stale client-side domain
/// cache must not flip the verdict).
fn get_integration_status_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.atproto.get_integration_status",
            )
            .await?;
            let _req: GetIntegrationStatusRequest = decode(&payload).map_err(malformed)?;

            let level = state
                .db
                .get_atproto_integration_level(&actor_id)
                .await
                .map_err(internal)?;

            let domain = state.handle_domain();
            let hosted_allowed =
                fauna_provisioning::probe::resolve_handle_domain(&domain).is_public_dns_name;

            // Derived at read time, never stored — a rename or domain change
            // re-derives on the next fetch. Empty when the handle does not
            // derive (reserved label, non-derivable domain): the client renders
            // no preview line rather than a wrong one.
            let handle = state
                .db
                .get_handle(&actor_id)
                .await
                .map_err(internal)?
                .unwrap_or_default();
            let handle_preview = fauna_protocol::atproto::derive_atproto_handle(&handle, &domain)
                .unwrap_or_default();

            let identity = state
                .db
                .get_atproto_identity(&actor_id)
                .await
                .map_err(internal)?
                .map(|row| AtprotoIdentitySummary {
                    // The summary's handle is the same read-time derivation;
                    // for an existing identity it equals the preview.
                    handle: handle_preview.clone(),
                    method: row.method,
                    status: row.status,
                    tombstone_requested: row.tombstone_requested,
                    did: row.did,
                    extra: Default::default(),
                });
            // Test-hooks seam: the nest stops NAMING this actor's identity
            // (`AppState::atproto_identity_withheld`) — row and DID untouched.
            #[cfg(feature = "test-hooks")]
            let identity = if state
                .atproto_identity_withheld
                .lock()
                .expect("atproto_identity_withheld mutex poisoned")
                .contains(&actor_id)
            {
                None
            } else {
                identity
            };

            encode_reply(&GetIntegrationStatusReply {
                level: level.as_str().into(),
                hosted_allowed,
                handle_domain: domain,
                handle_preview,
                identity,
                extra: Default::default(),
            })
        })
    })
}

// ── Bridge-class: S3 projection reads ───────────────────────────

/// One page of a user's public projection stream — servable public posts
/// (the `public_outbox_page` filter: `post/%`, not gated) interleaved with
/// post-delete tombstone journal rows, oldest-first by `(created_at, id)`
/// (S3, `atproto-pds-bridge.md` § Where logic lives). Post payloads are the
/// stored bytes verbatim (segment-first via `load_post_body`); tombstone
/// payloads are the journal row's bare canonical `Tombstone`. A post row
/// whose body no longer resolves (deleted mid-page; mirror divergence) is
/// warned and skipped — the cursor still advances past it, and its deletion
/// arrives as a journal row.
fn fetch_atproto_public_posts_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &bridge_actor,
                "fauna.bridges.atproto.fetch_public_posts",
            )
            .await?;
            let req: FetchPublicPostsRequest = decode(&payload).map_err(malformed)?;
            let actor_id = parse_actor_id(&req.actor_id)?;
            let after = match &req.cursor {
                Some(c) => {
                    let id = fauna_core::hex32::decode(&c.post_id)
                        .map_err(|_| malformed("cursor post_id must be 32 bytes of hex"))?;
                    Some((c.created_at_micros, id))
                }
                None => None,
            };
            let limit = req
                .limit
                .clamp(1, crate::db::atproto_projection::MAX_PUBLIC_PROJECTION_PAGE);

            // The history-backfill opt-in, made operative: the stream starts at
            // this user's projection floor, so a post predating their consent
            // to publish is never served to the bridge at all
            // (`atproto-pds-bridge.md` § Projection & backfill). Enforced here,
            // at the source of truth, rather than trusted to the bridge's
            // cursor — that store is explicitly re-derivable, and a wipe must
            // not resurrect unconsented history.
            let floor_micros = state
                .db
                .get_atproto_projection_floor(&actor_id)
                .await
                .map_err(internal)?;

            let rows = state
                .db
                .list_public_projection_page(&actor_id, after, limit, floor_micros)
                .await
                .map_err(internal)?;

            // Keyset resume point: the last ROW scanned (not the last item
            // emitted — a skipped unresolvable post must not wedge the
            // cursor). `Some` only on a full page; `None` = exhausted.
            let next_cursor = if rows.len() as u32 == limit {
                rows.last().map(|r| PublicPostsCursor {
                    created_at_micros: r.created_at,
                    post_id: hex::encode(r.id),
                })
            } else {
                None
            };

            let mut items = Vec::with_capacity(rows.len());
            for row in rows {
                let (kind, bytes, deleted_post_id) = if row.is_tombstone {
                    // Decode the bare canonical `Tombstone` here so the Go
                    // bridge never has to decode dag-cbor: surface the deleted
                    // post's digest as lowercase hex (same shape as a live
                    // item's `post_id`), which the projection loop resolves
                    // against its PostId→AT-URI map.
                    let ts: fauna_core::data::Tombstone = fauna_core::encoding::canonical_decode(
                        &row.inline_payload,
                    )
                    .map_err(|e| internal(format!("decode tombstone journal payload: {e:#}")))?;
                    let deleted = hex::encode(ts.post_id.digest());
                    (
                        public_post_item_kind::TOMBSTONE,
                        row.inline_payload,
                        Some(deleted),
                    )
                } else {
                    match crate::segments::post::load_post_body(
                        &state.post_segments,
                        &state.db,
                        &row.id,
                    )
                    .await
                    {
                        Ok(Some(body)) => (public_post_item_kind::POST, body, None),
                        Ok(None) => {
                            tracing::warn!(
                                target: "bridge_rpc",
                                post_id = %hex::encode(row.id),
                                "fetch_public_posts: post row has no resolvable body; skipping item"
                            );
                            continue;
                        }
                        Err(e) => return Err(internal(format!("load post body: {e:#}"))),
                    }
                };
                items.push(PublicPostItem {
                    post_id: hex::encode(row.id),
                    created_at_micros: row.created_at,
                    kind: kind.into(),
                    payload: ByteBuf::from(bytes),
                    deleted_post_id,
                });
            }
            encode_reply(&FetchPublicPostsReply { items, next_cursor })
        })
    })
}

/// The target user's current profile payload (the `fauna.profile.set`
/// at-rest bytes) for the `app.bsky.actor.profile` record at rkey `self`.
/// Bridge-class twin of the User-class `fauna.profile.get` (which the
/// bridge cannot call); absent profile is a normal `None`, never an error —
/// most users project posts before ever setting a profile.
fn fetch_atproto_profile_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(&state, &bridge_actor, "fauna.bridges.atproto.fetch_profile").await?;
            let req: FetchProfileRequest = decode(&payload).map_err(malformed)?;
            let actor_id = parse_actor_id(&req.actor_id)?;
            let profile = state
                .db
                .get_latest_profile_payload(&actor_id)
                .await
                .map_err(internal)?;
            encode_reply(&FetchProfileReply {
                profile: profile.map(ByteBuf::from),
            })
        })
    })
}

// ── F2: the external write path ─────────────────────────────────

/// The account has no hosted ATProto identity with a DID, so no write can be
/// addressed (`at://<did>/…` is unbuildable). A bridge that authenticated an
/// XRPC session for this account should never reach here; treated as a
/// account-state error rather than a per-write refusal because it disqualifies
/// the whole batch, not one row.
///
/// Not feature-gated: `delete_presence` (S5 slice 5) raises the same error for
/// the same reason on a build without `bluesky`, and two spellings of "you have
/// no hosted identity" would be one spelling too many for a client matching on
/// the code.
fn no_hosted_identity() -> RpcError {
    RpcError::new(
        "fauna.bridges.atproto.no_hosted_identity",
        "error.bridges.atproto.no_hosted_identity",
    )
    .with_details_text("the account has no active hosted ATProto identity")
}

/// Build the D6-sub-typed result for a refused write.
#[cfg(feature = "bluesky")]
fn refused(sub_type: &str, message: impl Into<String>) -> ExternalWriteResult {
    ExternalWriteResult {
        refusal: Some(ExternalWriteRefusal {
            sub_type: sub_type.to_string(),
            message: message.into(),
            extra: Default::default(),
        }),
        ..Default::default()
    }
}

/// `fauna.bridges.atproto.ingest_external_write` — one XRPC write batch
/// (`atproto-pds-full.md` § F2 detail).
///
/// **This slice implements the journal and refuse arms only.** The round-trip
/// arm (reverse-translate → build → sign with the D10 sub-key → ingest) is
/// F2.2 slice 3; until it lands, a round-trip-classified write refuses. The
/// two refusal reasons are deliberately distinguished, because only one of
/// them is temporary:
///
/// - the account has no D10 authoring delegation → **fauna-surface**, naming
///   the Fauna-app authorization step. This is the *designed permanent*
///   answer (D10 § Mint ceremony), correct today and unchanged by slice 3.
/// - a delegation exists but nothing can sign with it yet → **deferred**,
///   saying "not yet". Slice 3 replaces exactly this branch; the `deferred`
///   sub-type is what stops a later session reading it as policy.
///
/// Refusals are per-write data, not a batch failure: a mixed `applyWrites`
/// tells the caller precisely which rows to fix. A *malformed* write (missing
/// rkey on a delete, a record over the cap, an unknown action) fails the whole
/// call instead — that is a bridge bug, not a user-actionable condition, and
/// must not be dressed up as a D6 refusal the XRPC client would surface.
#[cfg(feature = "bluesky")]
fn ingest_external_write_handler() -> RpcHandler {
    use fauna_bridge_atproto::membership::{
        RefusalSubType, RoundTrip, WriteAction, WriteDisposition, classify_external_write,
    };
    use fauna_bridge_atproto::record_refs::MAX_RECORD_REFS;

    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &bridge_actor,
                "fauna.bridges.atproto.ingest_external_write",
            )
            .await?;
            let req: IngestExternalWriteRequest = decode(&payload).map_err(malformed)?;
            let actor = parse_actor_id(&req.actor_id)?;

            if req.writes.len() > EXTERNAL_WRITE_BATCH_MAX {
                return Err(malformed(format!(
                    "external write batch carries {} writes, over the {EXTERNAL_WRITE_BATCH_MAX} cap",
                    req.writes.len()
                )));
            }

            // The second rate-limit layer (§ Wire & process topology): the
            // bridge's per-IP window cannot bound one *account*, since every
            // caller behind a shared egress IP shares that bucket and the
            // authenticated actor is only known here. Keyed per (bridge,
            // account) like every other bridge kind's window.
            if !state
                .bridge_rate_limit
                .check(&bridge_actor, &actor, "ingest_external_write")
            {
                return Err(rate_limited());
            }

            // Defense-in-depth: the bridge refuses the whole external-app
            // plane on its cached flag, but a stale cache must never author
            // content for an account whose kill-switch is OFF.
            if !state
                .db
                .get_atproto_external_apps_enabled(&actor)
                .await
                .map_err(internal)?
            {
                return Err(external_apps_disabled());
            }

            // Every reply row needs `at://<did>/…`, so resolve the DID once.
            let did = state
                .db
                .get_atproto_identity(&actor)
                .await
                .map_err(internal)?
                .and_then(|row| row.did)
                .ok_or_else(no_hosted_identity)?;

            // Parse and classify the whole batch *before* applying any of it.
            // A malformed action is a bridge bug that fails the call, so
            // discovering one at write 3 must not leave writes 1–2 already
            // applied. Knowing every disposition up front also lets the D10
            // signer load once, and only when a round-trip actually needs it.
            let mut classified = Vec::with_capacity(req.writes.len());
            for (i, w) in req.writes.iter().enumerate() {
                let action = match w.action.as_str() {
                    external_write_action::CREATE => WriteAction::Create,
                    external_write_action::UPDATE => WriteAction::Update,
                    external_write_action::DELETE => WriteAction::Delete,
                    other => {
                        return Err(malformed(format!("write {i}: unknown action {other:?}")));
                    }
                };
                // A cap the receiver does not enforce is not a cap. The bridge
                // is trusted, but `resolved_targets` is the one field on this
                // wire whose size a *record* influences, so it is bounded here
                // too — and an over-cap map is a bridge bug, hence the whole
                // call fails rather than this row refusing.
                if w.resolved_targets.len() > MAX_RECORD_REFS {
                    return Err(malformed(format!(
                        "write {i}: {} resolved targets, over the {MAX_RECORD_REFS} cap",
                        w.resolved_targets.len()
                    )));
                }
                classified.push((action, classify_external_write(&w.collection, action)));
            }

            // Whether the account authorized external-app authoring is a pure
            // *account-state* question, answerable from the row alone — so it
            // decides the fauna-surface refusal on its own. Only actually
            // *signing* needs the nest signing key (the root of K's at-rest
            // KEK), so an unauthorized account never depends on a nest
            // internal to be told, correctly, to go authorize in a client.
            let needs_signer = classified
                .iter()
                .any(|(_, d)| matches!(d, WriteDisposition::RoundTrip(_)));
            // A *lapsed* delegation is the same user-visible state as one never
            // provisioned, and — like that case — is decided from the DB row
            // ALONE, before the nest signing key is touched, so an account
            // whose grant expired is still told where to re-authorize even on a
            // nest holding no signing key. Keep that ordering (pinned by
            // `a_post_without_a_delegation_refuses_fauna_surface`).
            //
            // The wall clock is the only sound operand here: step 5 of the
            // chain verify compares the cert against the *record's* own
            // `created_at`, which this arm adopts verbatim from the external
            // app, so a lapsed app could otherwise author forever by
            // back-dating (`delegation_is_live`; pinned by
            // `a_lapsed_delegation_refuses_even_when_the_record_backdates_...`).
            let now_micros = fauna_core::data::Timestamp::now().0;
            let has_delegation = needs_signer
                && state
                    .db
                    .get_atproto_authoring_key(&actor)
                    .await
                    .map_err(internal)?
                    .is_some_and(|row| {
                        row.cert.is_some_and(|cert| {
                            crate::atproto_authoring_key::delegation_is_live(&cert, now_micros)
                        })
                    });
            let signer = if has_delegation {
                let nest_key = nest_key_bytes(&state)?;
                crate::atproto_authoring_key::load_signer(&state.db, &nest_key, &actor)
                    .await
                    .map_err(internal)?
            } else {
                None
            };

            // A BATCH IS ALL OR NOTHING (decided 2026-07-29, F2.3 piece 2).
            // Every refusal this handler can raise is decidable before any
            // write is applied — the classifier's own verdict, the
            // no-delegation account state, and the capability scoping are all
            // pure functions of the batch plus the account row — so they are
            // decided here, up front, and a batch containing ANY refusal
            // applies none of it.
            //
            // `applyWrites` is the reason. The lexicon has no shape for "3 of
            // your 5 writes happened": its reply is a results array or an
            // error. So applying the good rows and refusing the rest would
            // leave real Fauna posts behind for a call the caller was told
            // failed — the same "the answer names something that did not
            // happen" failure the first-emit ruling rejects deferral for.
            // Per-write refusal DATA is unchanged and still positional, which
            // is what lets the bridge name exactly which row to fix.
            let mut refusals: Vec<Option<ExternalWriteResult>> = classified
                .iter()
                .map(|(_, disposition)| match disposition {
                    WriteDisposition::Refuse(r) => Some(refused(
                        match r.sub_type {
                            RefusalSubType::Policy => "policy",
                            RefusalSubType::FaunaSurface => "fauna_surface",
                            RefusalSubType::Deferred => "deferred",
                        },
                        r.message.clone(),
                    )),
                    // The designed *permanent* answer for an unauthorized
                    // account (D10 § Mint ceremony) — not a "not yet".
                    WriteDisposition::RoundTrip(_) if signer.is_none() => Some(refused(
                        "fauna_surface",
                        "this account has not authorized external-app posting; \
                         enable it on the AT Protocol page in a Fauna app",
                    )),
                    // A delegation exists but does not cover THIS write's
                    // capability — production-reachable, since provisioning
                    // accepts any subset of the enumerated authoring set.
                    WriteDisposition::RoundTrip(rt)
                        if !crate::atproto_authoring_key::delegation_grants(
                            &signer
                                .as_ref()
                                .expect("the signer-is-none arm above precedes this one")
                                .1,
                            &match rt {
                                RoundTrip::Post => fauna_core::data::Capability::Post,
                                RoundTrip::Profile => fauna_core::data::Capability::UpdateProfile,
                            },
                        ) =>
                    {
                        Some(refused(
                            "fauna_surface",
                            "the existing external-app authorization does not cover \
                             this write; re-authorize external apps on the Bluesky \
                             page in a Fauna app",
                        ))
                    }
                    _ => None,
                })
                .collect();

            // PRE-FLIGHT — the second half of all-or-nothing, and the half a
            // `WriteDisposition` cannot see.
            // Mapping dispositions above catches every refusal
            // *derivable from (collection, action) plus the account row*, but
            // three refusals are only knowable by looking at the record itself
            // or at stored state: an unparseable post, an unparseable profile,
            // and a delete of a post this actor does not own. Raised inside the
            // apply arms — where they used to be decided — each would fire with
            // rows `0..i` already applied, and nothing rolls those back: the
            // bridge then fails the whole call while the projection loop
            // carries the applied rows onto the firehose anyway (no `post_map`
            // row is written, and "already mapped" is exactly its idempotency
            // test). The delete arm was worst: a user's post destroyed AND
            // withdrawn from the network on a call answered as failed.
            //
            // So they are decided here, before anything is applied. Parsing is
            // pure, so the arms re-run it on the same bytes and cannot reach a
            // different verdict; the delete check has ONE owner
            // (`routes::check_post_delete_authorization`) that the arm still
            // runs as its guarantee.
            // INPUT SHAPE — the third pass, and the one that makes the property
            // self-enforcing rather than dependent on the bridge having checked
            // first (see `check_write_input_shape`).
            // Runs over EVERY row, journal included, and before the round-trip
            // pre-flight below: these are the cheapest checks and a malformed
            // row should not first cost a stored-state read.
            for (i, (action, disposition)) in classified.iter().enumerate() {
                if refusals[i].is_some() {
                    continue;
                }
                check_write_input_shape(i, &req.writes[i], *action, disposition)?;
            }

            for (i, (action, disposition)) in classified.iter().enumerate() {
                if refusals[i].is_some() {
                    continue;
                }
                let WriteDisposition::RoundTrip(rt) = disposition else {
                    continue;
                };
                refusals[i] =
                    preflight_round_trip(&state, &actor, &did, i, &req.writes[i], *action, *rt)
                        .await?;
            }

            // BLOB REFS — the fourth concern (F2.4 slice 2): a record
            // referencing a blob this account has not uploaded here refuses,
            // before anything is applied. See `preflight_blob_refs` for the
            // ruling and its scope.
            for (i, (action, disposition)) in classified.iter().enumerate() {
                if refusals[i].is_some() {
                    continue;
                }
                refusals[i] =
                    preflight_blob_refs(&state, &actor, &req.writes[i], *action, disposition)
                        .await?;
            }

            if refusals.iter().any(Option::is_some) {
                // Nothing is applied. A row that refused carries its own
                // reason; every other row is told the batch was abandoned, so
                // no result is ambiguous — an empty result with neither a
                // refusal nor an rkey would read downstream as a malformed
                // reply rather than as "not applied".
                let results = refusals
                    .into_iter()
                    .map(|r| {
                        r.unwrap_or_else(|| {
                            refused(
                                "policy",
                                "not applied: another write in this batch was refused, \
                                 and a batch applies either wholly or not at all",
                            )
                        })
                    })
                    .collect();
                return encode_reply(&IngestExternalWriteReply {
                    results,
                    extra: Default::default(),
                });
            }

            // `referenced_at` is already stamped — `preflight_blob_refs` above
            // does it per ref, immediately BEFORE resolving that ref, and this
            // loop used to repeat the walk here instead. Merging the two is not
            // tidying: the stamp is the row's immunity token against the F2.4
            // sweep, so it has to be acquired before the verdict that depends on
            // the row surviving, and it must cover exactly the refs the verdict
            // covered. Two loops maintained that agreement in parallel over the
            // same row set (`commits_caller_record`) and the same walk; one loop
            // makes it identity. See `preflight_blob_refs` for the ordering
            // argument and what a refused batch's stamps cost.
            let mut results = Vec::with_capacity(req.writes.len());
            for (i, (action, disposition)) in classified.into_iter().enumerate() {
                let w = &req.writes[i];
                results.push(match disposition {
                    // Unreachable: `refusals` above owns every refusing
                    // disposition, and a batch carrying one returned there
                    // without applying anything. The arm stays so the match is
                    // total, and refuses rather than inventing a result — but
                    // the refusal MESSAGES have exactly one home, up there.
                    WriteDisposition::Refuse(r) => refused(
                        match r.sub_type {
                            RefusalSubType::Policy => "policy",
                            RefusalSubType::FaunaSurface => "fauna_surface",
                            RefusalSubType::Deferred => "deferred",
                        },
                        r.message,
                    ),
                    WriteDisposition::RoundTrip(RoundTrip::Post) => {
                        let (kp, cert) = signer
                            .as_ref()
                            .expect("a refusing batch returned before this loop");
                        match action {
                            WriteAction::Create => {
                                round_trip_post_create(&state, &actor, &did, i, w, kp, cert).await?
                            }
                            WriteAction::Delete => {
                                round_trip_post_delete(&state, &actor, &did, i, w, kp, cert).await?
                            }
                            // Unreachable: the classifier policy-refuses a
                            // post update before it can reach this arm.
                            WriteAction::Update => refused(
                                "policy",
                                "posts are immutable; delete the post and create a new one",
                            ),
                        }
                    }
                    WriteDisposition::RoundTrip(RoundTrip::Profile) => {
                        let (kp, cert) = signer
                            .as_ref()
                            .expect("a refusing batch returned before this loop");
                        match action {
                            WriteAction::Create | WriteAction::Update => {
                                round_trip_profile_update(&state, &actor, &did, i, w, kp, cert)
                                    .await?
                            }
                            // Unreachable: the classifier fauna-surface-refuses
                            // a profile delete before it can reach this arm.
                            WriteAction::Delete => refused(
                                "fauna_surface",
                                "the profile record is derived from your Fauna profile; \
                                 edit or clear it in a Fauna app",
                            ),
                        }
                    }
                    WriteDisposition::Journal => {
                        journal_write(&state, &actor, &did, i, w, action).await?
                    }
                });
            }

            // The delegation row's ADVISORY `last_used_at` (D10 § Audit,
            // `atproto-pds-full.md`). Stamped HERE — after the batch applied,
            // never at signer load — because "last used" honestly means an
            // external app *authored*: a batch that refused authored nothing,
            // and the all-or-nothing gate above has already returned in that
            // case. Gated on `signer.is_some()` so a batch that only journalled
            // (no round-trip row, so no delegated authoring) does not stamp.
            //
            // Failure is LOGGED, never propagated: the writes have already
            // landed, so returning an error here would tell the caller a
            // successful batch failed — and this value is advisory, explicitly
            // not forensics. Under-reporting it is the honest failure direction.
            if signer.is_some() {
                let now_millis = (fauna_core::data::Timestamp::now().0 / 1000) as i64;
                if let Err(e) = state
                    .db
                    .touch_atproto_authoring_key_last_used(&actor, now_millis)
                    .await
                {
                    tracing::warn!(
                        "atproto: advisory last_used_at stamp failed after a successful \
                         delegated batch (the writes DID apply): {e}"
                    );
                }
            }

            encode_reply(&IngestExternalWriteReply {
                results,
                extra: Default::default(),
            })
        })
    })
}

/// The record bytes a write must carry, within the per-record cap.
///
/// **ONE owner of both requirements, TWO callers** — the up-front pass
/// ([`check_write_input_shape`]) runs it over the whole batch before anything is
/// applied, and each apply arm runs it again for the *value*, which is what keeps
/// the two from drifting into disagreeing verdicts (the pattern
/// `routes::check_post_delete_authorization` established for the delete rule).
///
/// Both requirements are pure functions of the row, which is why they belong
/// up-front: raised from inside an apply arm they fire with rows `0..i` already
/// applied and nothing rolls those back.
#[cfg(feature = "bluesky")]
fn require_record(index: usize, w: &ExternalWrite) -> Result<&[u8], RpcError> {
    let record = w.record.as_ref().ok_or_else(|| {
        malformed(format!(
            "write {index}: {} carries no record",
            external_write_action_label(w)
        ))
    })?;
    if record.len() > EXTERNAL_WRITE_RECORD_MAX_BYTES {
        return Err(malformed(format!(
            "write {index}: record is {} bytes, over the {EXTERNAL_WRITE_RECORD_MAX_BYTES}-byte cap",
            record.len()
        )));
    }
    Ok(record)
}

/// The rkey a write must address. `what` names the caller's requirement so the
/// message stays as specific as the hand-written ones it replaces.
///
/// One owner, two callers — see [`require_record`].
#[cfg(feature = "bluesky")]
fn require_rkey<'a>(index: usize, w: &'a ExternalWrite, what: &str) -> Result<&'a str, RpcError> {
    w.rkey
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .ok_or_else(|| malformed(format!("write {index}: {what}")))
}

/// The record CID a journal write must carry. One owner, two callers — see
/// [`require_record`].
#[cfg(feature = "bluesky")]
fn require_cid(index: usize, w: &ExternalWrite) -> Result<&str, RpcError> {
    w.cid.as_deref().filter(|c| !c.is_empty()).ok_or_else(|| {
        malformed(format!(
            "write {index}: {} carries no cid",
            external_write_action_label(w)
        ))
    })
}

/// The action as the wire spelled it, for a message. Kept beside the accessors so
/// all three phrase themselves the same way.
#[cfg(feature = "bluesky")]
fn external_write_action_label(w: &ExternalWrite) -> &str {
    &w.action
}

/// Every input-shape requirement that is a **pure function of the row**, decided
/// for the whole batch before anything is applied.
///
/// This is the third pass all-or-nothing needs, and the reason it exists is:
/// `:223` forbids an *outcome* — "rows applied, call
/// answered as failed, projection carries them onto the network" — and **any**
/// mid-loop `Err` in the apply loop produces it, not only a `refused(…)`. The
/// disposition pass catches refusals derivable from `(collection, action)` plus
/// the account row; [`preflight_round_trip`] catches the ones only the record or
/// stored state can reveal; this catches the ones that are just *shape*.
///
/// It was previously true only because a **different process** validated first —
/// the Go bridge caps the record at the identical constant and always supplies an
/// rkey and CID before the single `ingest_external_write`. That made the exposure
/// narrow rather than a live bug, but it left the guarantee resting on an unstated
/// precondition, which is exactly the false-premise class that produced
/// this finding. A guarantee that holds because someone else checked is not a
/// guarantee this code can state.
///
/// Journal rows are covered here and are *not* covered by
/// [`preflight_round_trip`], which returns early on a non-`RoundTrip`
/// disposition — the journal arm's four shape errors were the largest part of the
/// gap.
#[cfg(feature = "bluesky")]
fn check_write_input_shape(
    index: usize,
    w: &ExternalWrite,
    action: fauna_bridge_atproto::membership::WriteAction,
    disposition: &fauna_bridge_atproto::membership::WriteDisposition,
) -> Result<(), RpcError> {
    use fauna_bridge_atproto::membership::{RoundTrip, WriteAction, WriteDisposition};

    match disposition {
        WriteDisposition::RoundTrip(rt) => match (rt, action) {
            // A round-trip post delete addresses an existing record by rkey and
            // carries no record of its own.
            (RoundTrip::Post, WriteAction::Delete) => {
                require_rkey(index, w, "delete carries no rkey")?;
            }
            // A round-trip create lets the NEST derive the rkey (it is a
            // deterministic TID over the Fauna post's own creation instant), so
            // only the record is required here.
            _ => {
                require_record(index, w)?;
            }
        },
        WriteDisposition::Journal => match action {
            WriteAction::Create | WriteAction::Update => {
                require_rkey(index, w, "journal writes require an rkey")?;
                require_record(index, w)?;
                require_cid(index, w)?;
            }
            WriteAction::Delete => {
                require_rkey(index, w, "journal writes require an rkey")?;
            }
        },
        // A refusing disposition applies nothing, so its shape is irrelevant —
        // and checking it would turn a clean policy refusal into a malformed-call
        // error, losing the per-row reason the caller needs.
        _ => {}
    }
    Ok(())
}

/// Decide, WITHOUT applying anything, whether a round-trip write will refuse.
///
/// The batch's all-or-nothing rule (`atproto-pds-full.md` § F2 detail) needs
/// every refusal knowable before the first row is applied. Dispositions cover
/// the refusals derivable from `(collection, action)` plus the account row;
/// this covers the three that need the record's own bytes or stored state.
///
/// Returning `Ok(None)` means "this row has no refusal to raise" — the arm may
/// still fail with a genuine `RpcError`, which fails the whole call and is a
/// different thing from a per-row refusal.
#[cfg(feature = "bluesky")]
async fn preflight_round_trip(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    did: &str,
    index: usize,
    w: &ExternalWrite,
    action: fauna_bridge_atproto::membership::WriteAction,
    round_trip: fauna_bridge_atproto::membership::RoundTrip,
) -> Result<Option<ExternalWriteResult>, RpcError> {
    use fauna_bridge_atproto::membership::{RoundTrip, WriteAction};

    match (round_trip, action) {
        // An unparseable record — the reviewer's own repro is an ordinary app
        // sending `createdAt: "not-a-timestamp"`, which the bridge's structural
        // validation cannot catch (it does no Lexicon validation, and the
        // record is perfectly good dag-cbor).
        (RoundTrip::Post, WriteAction::Create) => {
            let record = w.record.as_deref().map(Vec::as_slice).unwrap_or(&[]);
            if let Err(e) = fauna_bridge_atproto::reverse_translate::parse_post_record(record) {
                return Ok(Some(refused(
                    "policy",
                    format!("record is not a usable app.bsky.feed.post: {e}"),
                )));
            }
        }
        (RoundTrip::Profile, WriteAction::Create | WriteAction::Update) => {
            let record = w.record.as_deref().map(Vec::as_slice).unwrap_or(&[]);
            if let Err(e) = fauna_bridge_atproto::reverse_translate::parse_profile_record(record) {
                return Ok(Some(refused(
                    "policy",
                    format!("record is not a usable app.bsky.actor.profile: {e}"),
                )));
            }
        }
        // A delete of somebody else's post. The authorship rule has one owner
        // in `routes`; this asks it, and `delete_post_core` still enforces it.
        (RoundTrip::Post, WriteAction::Delete) => {
            let Some(rkey) = w.rkey.as_deref() else {
                return Ok(None); // the arm raises the malformed-batch error
            };
            let at_uri = format!("at://{did}/{}/{rkey}", w.collection);
            // No resolved target means this delete journals, which the
            // authorship rule does not govern.
            let Some(post_id) = resolved_post_id(w, &at_uri) else {
                return Ok(None);
            };
            let digest = crate::db::posts::cid_to_digest(&post_id);
            let probe = fauna_core::data::Tombstone {
                author: fauna_core::identity::ActorId(*actor),
                post_id,
                created_at: fauna_core::data::Timestamp::now(),
            };
            match crate::routes::check_post_delete_authorization(state, *actor, &probe, digest)
                .await
            {
                Ok(_) => {}
                Err(crate::routes::PostDeleteError::NotAuthor) => {
                    return Ok(Some(refused(
                        "policy",
                        "that post belongs to another account and cannot be deleted from here",
                    )));
                }
                Err(crate::routes::PostDeleteError::Internal(msg)) => {
                    return Err(internal(msg));
                }
            }
        }
        // A post update never reaches an arm (the classifier refuses it), and
        // a profile delete is refused as fauna-surface.
        (RoundTrip::Post, WriteAction::Update) | (RoundTrip::Profile, WriteAction::Delete) => {}
    }
    let _ = index;
    Ok(None)
}

/// Which rows carry **caller-authored blob refs** — the row set the blob
/// pre-flight covers and the `referenced_at` stamp pass walks. ONE owner for the
/// set, so refusal coverage and stamp coverage cannot drift: a row whose
/// dangling ref we would refuse is exactly a row whose resolvable refs we must
/// stamp.
///
/// **The predicate this evolved from was `commits_caller_record`, and the
/// difference is the whole of the inbound-picture change.** Slices 2 and 3 used
/// "commits the caller's record bytes" as the proxy for "carries caller-authored
/// refs", and profile rows sat outside it on both counts: a profile round-trip
/// commits the *projection's* rendering (`reproject_record`), and the pictures
/// in that rendering are projection-stored, with no `atproto_blobs` row to check
/// or stamp. Covering profiles then would have refused every "change display
/// name, keep avatar" write from a pictured account.
///
/// The ratified inbound-picture design (`atproto-pds-full.md` § F2 detail,
/// *a picture crosses INBOUND by resolution, never by trust*) separates the two
/// questions, so this predicate keeps the one it is actually about. A profile
/// write still commits the projection's rendering — that half is unchanged — but
/// the caller's record now *authors* the picture choice, so its refs must be
/// gated and stamped like any other. What made the old exemption safe is what
/// replaces it: an echoed projection-stored ref no longer needs an
/// `atproto_blobs` row, because the bridge resolves it and sends the answer as
/// `resolved_media` (see [`resolve_external_blob_ref`]).
#[cfg(feature = "bluesky")]
fn carries_caller_blob_refs(
    action: fauna_bridge_atproto::membership::WriteAction,
    disposition: &fauna_bridge_atproto::membership::WriteDisposition,
) -> bool {
    use fauna_bridge_atproto::membership::{RoundTrip, WriteAction, WriteDisposition};
    matches!(
        (disposition, action),
        (
            WriteDisposition::RoundTrip(RoundTrip::Post),
            WriteAction::Create
        ) | (
            WriteDisposition::RoundTrip(RoundTrip::Profile),
            WriteAction::Create | WriteAction::Update
        ) | (
            WriteDisposition::Journal,
            WriteAction::Create | WriteAction::Update
        )
    )
}

/// The refusal for a blob ref that resolves to nothing (F2.4 slice 2's ruling,
/// `atproto-pds-full.md` § F2 detail). One spelling, shared by the pre-flight
/// and the media arm.
#[cfg(feature = "bluesky")]
fn refuse_unuploaded_blob(cid: &str) -> ExternalWriteResult {
    refused(
        "policy",
        format!(
            "record references blob {cid}, which this account has not uploaded \
             to this PDS; upload it with com.atproto.repo.uploadBlob first"
        ),
    )
}

/// Resolve one blob CID a record references to the Fauna media it landed as —
/// the ONE owner of "is this ref resolvable", asked by the pre-flight for the
/// verdict and by the media arm for the value.
///
/// `None` means the ref is not resolvable as Fauna media: this account never
/// uploaded those bytes here (`atproto_blobs` has no row — the `uploadBlob`
/// ledger is per-account, as blob refs are per-repo), or the media path no
/// longer holds their metadata. The two collapse deliberately, because the
/// remedy is identical: re-upload, which converges by CID (the upsert).
///
/// The `MediaItem` takes `media_type` and `size_bytes` from the nest's own
/// `blob_metadata` row — for these bytes, the type the upload leg SNIFFED —
/// never from the record's declared `mimeType`/`size`: what 7 apps render a
/// blob as must be what the bytes are, not what a caller said they are (the
/// sniffed-not-declared ruling). Dimensions stay `None` here; the caller
/// merges the record's declared `aspectRatio`, which nothing stored knows and
/// a native app declares for itself too.
#[cfg(feature = "bluesky")]
async fn resolve_uploaded_blob(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    cid: &str,
) -> Result<Option<fauna_core::data::MediaItem>, RpcError> {
    let Some(row) = state
        .db
        .get_atproto_blob(actor, cid)
        .await
        .map_err(internal)?
    else {
        return Ok(None);
    };
    let Some(meta) = state
        .db
        .get_blob_metadata(&row.media_ref)
        .await
        .map_err(internal)?
    else {
        return Ok(None);
    };
    Ok(Some(fauna_core::data::MediaItem {
        blob_hash: fauna_cbor::Cid::from_digest_raw(row.media_ref),
        media_type: meta.content_type,
        size_bytes: u64::try_from(meta.size_bytes).unwrap_or(0),
        dimensions: None,
        thumbnail: meta
            .thumbnail_hash
            .as_deref()
            .and_then(|h| <[u8; 32]>::try_from(h).ok())
            .map(fauna_cbor::Cid::from_digest_raw),
        remote_url: None,
        alt: None,
    }))
}

/// Resolve one blob CID a record references to the Fauna content it is —
/// **the** owner of "does this ref resolve", asking the two ratified resolvers
/// in turn (`atproto-pds-full.md` § F2 detail, *a picture crosses INBOUND by
/// resolution, never by trust*).
///
/// 1. **This nest's upload ledger** — the fresh-upload case. The account
///    `uploadBlob`'d these bytes here, so [`resolve_uploaded_blob`] has the row,
///    and (only here) there is a `referenced_at` stamp to acquire.
/// 2. **The bridge's answer about its own store** — the echo case. A caller
///    handing back the ref our own projection published names bytes this PDS
///    serves but this side cannot *name*: the Fauna-CID↔ATProto-CID index is the
///    bridge blob store, and the two address spaces are bridged only by holding
///    the bytes. So the bridge resolved it before calling and sent the mapping
///    as `resolved_media`.
///
/// `None` from both is the refusal condition: nothing anywhere can supply those
/// bytes later (a blob ref is repo-scoped), so committing the record would
/// publish a dangling ref forever.
///
/// **Order is by ownership, not preference, and it cannot matter:** both
/// resolvers are content-addressed over the same bytes, so where both answer
/// they answer the same `ContentHash`. Asking the ledger first simply keeps the
/// stamp beside the row it protects.
///
/// A `resolved_media` value that is not a parseable Fauna CID is treated as no
/// answer rather than trusted or panicked on — the bridge would have to be buggy
/// to send one, and the safe reading of a malformed vouch is "did not vouch".
///
/// **`allow_echo` is decided HERE, not by what the bridge chose to send**
/// ([`resolves_echoed_media`]). The bridge already scopes production to profile
/// writes, so consulting the map unconditionally would agree with it today — and
/// would make the nest's pre-flight depend on the bridge's discretion, which is
/// precisely what `check_write_input_shape` exists to refuse.
/// A later bridge change that populated `resolved_media` for a record-committing
/// row would then silently bypass the `atproto_blobs` existence check for it.
/// Enforcing the scope on this side makes that bypass unrepresentable.
#[cfg(feature = "bluesky")]
async fn resolve_external_blob_ref(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    w: &ExternalWrite,
    cid: &str,
    allow_echo: bool,
) -> Result<Option<fauna_core::data::ContentHash>, RpcError> {
    if let Some(item) = resolve_uploaded_blob(state, actor, cid).await? {
        return Ok(Some(item.blob_hash));
    }
    if !allow_echo {
        return Ok(None);
    }
    Ok(w.resolved_media
        .get(cid)
        .and_then(|fauna_cid| fauna_core::data::ContentHash::from_base32(fauna_cid).ok()))
}

/// Which rows may resolve a ref through the BRIDGE's answer rather than this
/// nest's own upload ledger.
///
/// Only the profile singleton, because only a picture has an echo case: the
/// projection publishes the account's avatar/banner, so an external app editing
/// a bio hands those very refs back and nothing but the bridge's store can say
/// what Fauna content they are. Every other row commits the CALLER's record
/// bytes, and for those F2.4 slice 2's ratified asymmetry stands — an external
/// app uploads its media here, and a ref with no `atproto_blobs` row refuses.
///
/// Widening this is a product ruling, not a tidy-up: it would let a record
/// reference already-published media without an upload. Change it here *and* in
/// the bridge's `resolveEchoedMedia`, deliberately.
#[cfg(feature = "bluesky")]
fn resolves_echoed_media(disposition: &fauna_bridge_atproto::membership::WriteDisposition) -> bool {
    use fauna_bridge_atproto::membership::{RoundTrip, WriteDisposition};
    matches!(disposition, WriteDisposition::RoundTrip(RoundTrip::Profile))
}

/// Build the Fauna media items for a post record's images — ONE owner, TWO
/// callers (the blob pre-flight for the verdict, the create arm for the
/// value), the [`require_record`] pattern, so the two cannot reach different
/// conclusions about the same record.
///
/// (Nothing deletes an `atproto_blobs` row between the two asks today; the
/// F2.4 GC slice must keep that true — sweep only rows no in-flight write has
/// resolved — or the arm's re-ask becomes a mid-loop error, the
/// shape.)
#[cfg(feature = "bluesky")]
async fn media_items_for_post(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    images: &[fauna_bridge_atproto::reverse_translate::IntermediateImage],
) -> Result<Result<Vec<fauna_core::data::MediaItem>, ExternalWriteResult>, RpcError> {
    use fauna_bridge_atproto::outbound::MAX_EMBED_IMAGES;

    // The lexicon ceiling is two-sided with outbound's `extract_projection_media`
    // cap: what we refuse inbound is exactly what we would truncate outbound.
    if images.len() > MAX_EMBED_IMAGES {
        return Ok(Err(refused(
            "policy",
            format!(
                "record embeds {} images; app.bsky.embed.images allows at most \
                 {MAX_EMBED_IMAGES}",
                images.len()
            ),
        )));
    }

    let mut items = Vec::with_capacity(images.len());
    for img in images {
        let Some(mut item) = resolve_uploaded_blob(state, actor, &img.cid).await? else {
            return Ok(Err(refuse_unuploaded_blob(&img.cid)));
        };
        if let (Some(width), Some(height)) = (img.width, img.height) {
            item.dimensions = Some(fauna_core::data::Dimensions { width, height });
        }
        items.push(item);
    }
    Ok(Ok(items))
}

/// Decide, WITHOUT applying anything, whether a row's blob refs refuse it —
/// the F2.4 slice-2 pre-flight (fourth concern of the all-or-nothing passes).
///
/// **A blob ref that resolves to nothing REFUSES** (`atproto-pds-full.md`
/// § F2 detail owns the ruling). A blob ref is repo-scoped — only THIS PDS
/// can ever serve it — so unlike a reply target on the open network, nothing
/// anywhere can supply the bytes later: journaling would commit a record
/// whose media 404s from every consumer forever (the dangling ref
/// *store-then-reference* exists to prevent), and round-tripping without the
/// image would silently alter the post while the record still claims it.
/// Refusing is the `uploadBlob` asymmetry applied again: nothing has been
/// published yet, the app learns immediately, and the remedy is literally the
/// upload it skipped.
///
/// Covers the [`carries_caller_blob_refs`] rows: round-trip post creates get the
/// full media verdict (images ceiling + per-image resolution) plus the
/// generic walk (a link-card `thumb` or a video blob is a ref even though the
/// media arm maps neither); profile round-trips and journal rows get the
/// generic walk. Runs after [`check_write_input_shape`] (the record is present)
/// and after [`preflight_round_trip`] (an unparseable post has already refused).
#[cfg(feature = "bluesky")]
async fn preflight_blob_refs(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    w: &ExternalWrite,
    action: fauna_bridge_atproto::membership::WriteAction,
    disposition: &fauna_bridge_atproto::membership::WriteDisposition,
) -> Result<Option<ExternalWriteResult>, RpcError> {
    use fauna_bridge_atproto::membership::{RoundTrip, WriteAction, WriteDisposition};

    if !carries_caller_blob_refs(action, disposition) {
        return Ok(None);
    }
    let Some(record) = w.record.as_deref() else {
        return Ok(None); // the input-shape pass already raised the malformed error
    };

    for cid in fauna_bridge_atproto::record_refs::external_record_blob_refs(record) {
        // STAMP, THEN RESOLVE — the order is load-bearing, and it is what makes
        // the F2.4 GC unable to race this write (the constraint slice 2 left for
        // slice 4). The stamp is the row's **immunity token**: the sweep deletes
        // only `referenced_at IS NULL` rows, and evaluates that predicate inside
        // its own DELETE, so once a stamp lands the row can never be swept. Do
        // it first and every later reader — this pre-flight's own verdict and
        // the apply arm's re-ask for the values — is asking about a row the
        // sweep provably cannot take. Resolve-then-stamp leaves exactly the
        // shape: the verdict says "resolvable", the sweep takes the row,
        // and the arm errors mid-loop on a batch already declared applicable.
        //
        // A stamp on a batch that then refuses is the cost, and it is the same
        // fail-safe direction slice 2 ratified for the apply loop (over-stamping
        // costs one blob's collectability; under-stamping is deleted user
        // media). It is bounded to the caller's own uploads — a row exists only
        // where this account uploaded those bytes — so a refusing caller can
        // only keep its own blobs alive, which posting them would do anyway.
        //
        // The stamp is unconditional and the resolution is two-sided: a ref the
        // BRIDGE vouched for has no row here to stamp, so the UPDATE matches
        // nothing and costs nothing. Keeping it unconditional is what keeps the
        // stamped set and the checked set identical by construction rather than
        // by a second predicate maintained alongside this one.
        state
            .db
            .stamp_atproto_blob_referenced(actor, &cid)
            .await
            .map_err(internal)?;
        if resolve_external_blob_ref(state, actor, w, &cid, resolves_echoed_media(disposition))
            .await?
            .is_none()
        {
            return Ok(Some(refuse_unuploaded_blob(&cid)));
        }
    }

    if let (WriteDisposition::RoundTrip(RoundTrip::Post), WriteAction::Create) =
        (disposition, action)
        && let Ok(parsed) = fauna_bridge_atproto::reverse_translate::parse_post_record(record)
        && let Err(refusal) = media_items_for_post(state, actor, &parsed.images).await?
    {
        return Ok(Some(refusal));
    }

    Ok(None)
}

/// Round-trip one `app.bsky.feed.post` **create** into a real Fauna post
/// (`atproto-pds-full.md` D1 + D10): reverse-translate the record, build the
/// `Post`, sign it with the delegated sub-key `K` with the identity-signed
/// cert embedded in the wire's `signer_auth`, and ingest it through the very
/// same `ingest_post_core` path a Fauna app's own post takes.
///
/// Going through that path rather than writing storage directly is the point:
/// `classify_encrypted_post` runs `verify_authoring_envelope` on the real
/// bytes, so a post this arm produces is proven acceptable to the ingest gate
/// by construction — and, being an ordinary stored post, it federates,
/// projects, and decodes on all 7 apps with no special case.
///
/// **The Fauna post adopts the record's own `createdAt`** rather than a
/// nest-stamped instant. A native app already asserts its own
/// (`fauna_client_core::post::build_post` stamps `Timestamp::now()`
/// client-side and no ingest-side skew check exists), so nest-stamping only
/// this surface would be a per-surface deviation that closes nothing; and the
/// projection emits `post.created_at` verbatim back into the record's
/// `createdAt` (`outbound::translate_post_for_projection`), so discarding the
/// submitted value would silently hand the caller back a *different* record
/// than it wrote. Should Fauna ever want a creation-skew policy, it belongs at
/// the shared ingest gate for every post, not invented here.
///
/// The reply's rkey is derived from the same `(created_at, post_id)` pair the
/// projection will later feed to `DeterministicTID`, so the AT-URI answered
/// here is the one the record actually lands at — read-your-writes by
/// construction, not by timing.
#[cfg(feature = "bluesky")]
#[allow(clippy::too_many_arguments)]
async fn round_trip_post_create(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    did: &str,
    index: usize,
    w: &ExternalWrite,
    signer: &fauna_core::identity::ActorKeypair,
    cert_bytes: &[u8],
) -> Result<ExternalWriteResult, RpcError> {
    use fauna_bridge_atproto::membership::WriteAction;
    use fauna_core::data::{Post, PostBody, Reference, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, canonical_decode, canonical_encode, sign_envelope};
    use fauna_core::identity::ActorId;

    // Re-checked here as the GUARANTEE; the up-front `check_write_input_shape`
    // pass is what makes it decidable before any row is applied.
    let record = require_record(index, w)?;

    // A record the bridge structurally validated but we cannot express is a
    // *policy* refusal for this write, not a batch failure: the caller learns
    // precisely which row to fix.
    let parsed = match fauna_bridge_atproto::reverse_translate::parse_post_record(record) {
        Ok(p) => p,
        Err(e) => {
            return Ok(refused(
                "policy",
                format!("record is not a usable app.bsky.feed.post: {e}"),
            ));
        }
    };

    // A reply or quote names its target by AT-URI; a Fauna `Reference` needs
    // the target's Fauna post id. The bridge resolved each against `post_map`
    // and sent the answers in `resolved_targets` (slice 4b).
    //
    // **A reference we cannot resolve journals the whole write** — it neither
    // refuses nor publishes standalone (`atproto-pds-full.md` § F2 detail: *A
    // reply or quote whose target is not a Fauna post JOURNALS*). Most of the
    // network is not Fauna, so this is the ordinary case, not an edge one:
    // refusing would make the account's PDS reject the network's most common
    // operation and the reply would exist nowhere at all (the record only
    // reaches the repo after this call answers), while round-tripping it
    // standalone would publish the user's words to their Fauna followers
    // stripped of the conversation they wrote them in — a silent alteration
    // against D1. Journaling is D2 applied literally, and the reversible
    // option: the record lands verbatim, reaches the repo and the firehose,
    // and can be back-filled should Fauna grow cross-network references.
    let mut references = Vec::new();
    if let Some(reply) = parsed.reply.as_ref() {
        let Some(post_id) = resolved_post_id(w, &reply.parent_uri) else {
            return journal_write(state, actor, did, index, w, WriteAction::Create).await;
        };
        references.push(Reference::Reply { post_id });
    }
    if let Some(quote) = parsed.quote.as_ref() {
        let Some(post_id) = resolved_post_id(w, &quote.uri) else {
            return journal_write(state, actor, did, index, w, WriteAction::Create).await;
        };
        references.push(Reference::Quote { post_id });
    }

    // F2.4 slice 2: the media arm. The blob pre-flight already decided
    // resolvability for the whole batch; the arm asks the same owner again for
    // the VALUES (the `require_record` pattern), so the two cannot drift.
    let media = match media_items_for_post(state, actor, &parsed.images).await? {
        Ok(items) => items,
        Err(refusal) => return Ok(refusal),
    };
    let body = if media.is_empty() {
        PostBody::Text {
            content: parsed.text,
            facets: parsed.facets,
        }
    } else if parsed.text.is_empty() {
        // Alt text crosses on the first image only — the exact inverse of
        // `outbound::extract_projection_media`, so a media post that leaves
        // Fauna and comes back keeps its alt stable.
        PostBody::Media {
            alt_text: parsed
                .images
                .first()
                .map(|i| i.alt.clone())
                .filter(|a| !a.is_empty()),
            items: media,
        }
    } else {
        PostBody::TextWithMedia {
            content: parsed.text,
            facets: parsed.facets,
            items: media,
        }
    };

    let created_at_micros = parsed.created_at_micros;
    let post = Post {
        author: ActorId(*actor),
        created_at: Timestamp(created_at_micros),
        body,
        references,
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };

    // The stored cert is already the client-uploaded embed-as-bytes wire
    // (`provision_authoring_delegation` verified it before storing); decode it
    // only to nest it inside this post's wire.
    let cert: EmbedAsBytes = canonical_decode(cert_bytes)
        .map_err(|e| internal(format!("stored delegation cert is not embed-as-bytes: {e}")))?;
    let (bytes, env) =
        sign_envelope(signer, &post).map_err(|e| internal(format!("sign delegated post: {e}")))?;
    let wire = canonical_encode(&EmbedAsBytes::from_signed(bytes, env).with_signer_auth(cert))
        .map_err(|e| internal(format!("encode delegated post: {e}")))?;

    let post_id = crate::routes::ingest_post_core(state, *actor, &bytes::Bytes::from(wire))
        .await
        .map_err(|e| match e {
            crate::routes::PostCreateError::Ingest(api) => internal(format!(
                "delegated post rejected at ingest: {}",
                api.message
            )),
            crate::routes::PostCreateError::Internal(msg) => internal(msg),
        })?;

    let rkey =
        fauna_bridge_atproto::outbound::deterministic_tid(created_at_micros as i64, &post_id);
    Ok(ExternalWriteResult {
        at_uri: Some(format!("at://{did}/{}/{rkey}", w.collection)),
        rkey: Some(rkey),
        // The bridge maps this into its `post_map` row. Without it the
        // projection loop would not recognize the post as already projected and
        // would re-project it over the caller's own record bytes.
        fauna_post_id: Some(hex::encode(post_id)),
        ..Default::default()
    })
}

/// Round-trip one `app.bsky.feed.post` **delete** into a real Fauna post
/// deletion: build the `Tombstone`, sign it with the delegated sub-key `K`
/// with the identity-signed cert embedded, and put it through the very same
/// decode-then-`delete_post_core` pipeline `fauna.posts.delete` uses.
///
/// The structure mirrors [`round_trip_post_create`] deliberately, for the same
/// reason: routing through `decode_tombstone` is the *proof*, not a formality.
/// That call runs `verify_authoring_envelope` on the real bytes, so a
/// tombstone this arm produces is provably one the ordinary delete path would
/// accept — dropping the cert makes it reject, exactly as D10 designed. Going
/// straight to `delete_post_core` would create a second, unverified delete
/// door.
///
/// **A record that maps to no Fauna post journals** rather than refusing: it
/// is a journaled record being deleted, which is the ordinary case for the
/// non-Fauna half of an account's repo.
#[cfg(feature = "bluesky")]
#[allow(clippy::too_many_arguments)]
async fn round_trip_post_delete(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    did: &str,
    index: usize,
    w: &ExternalWrite,
    signer: &fauna_core::identity::ActorKeypair,
    cert_bytes: &[u8],
) -> Result<ExternalWriteResult, RpcError> {
    use fauna_bridge_atproto::membership::WriteAction;
    use fauna_core::data::{Timestamp, Tombstone};
    use fauna_core::encoding::{EmbedAsBytes, canonical_decode, canonical_encode, sign_envelope};
    use fauna_core::identity::ActorId;

    // Every delete addresses an existing record by rkey (the bridge always
    // sends one), and that record's own AT-URI is the key the bridge resolved.
    // Re-checked here as the GUARANTEE — see the journal arm's note.
    let rkey = require_rkey(index, w, "delete carries no rkey")?;
    let at_uri = format!("at://{did}/{}/{rkey}", w.collection);

    let Some(post_id) = resolved_post_id(w, &at_uri) else {
        return journal_write(state, actor, did, index, w, WriteAction::Delete).await;
    };
    let digest = crate::db::posts::cid_to_digest(&post_id);

    let tombstone = Tombstone {
        author: ActorId(*actor),
        post_id,
        // The tombstone's own instant, not the record's: a delete carries no
        // `createdAt` to adopt, and nothing derives an rkey from this one (the
        // record being removed already has its key).
        created_at: Timestamp::now(),
    };
    let cert: EmbedAsBytes = canonical_decode(cert_bytes)
        .map_err(|e| internal(format!("stored delegation cert is not embed-as-bytes: {e}")))?;
    let (bytes, env) = sign_envelope(signer, &tombstone)
        .map_err(|e| internal(format!("sign delegated tombstone: {e}")))?;
    let wire = canonical_encode(&EmbedAsBytes::from_signed(bytes, env).with_signer_auth(cert))
        .map_err(|e| internal(format!("encode delegated tombstone: {e}")))?;

    // The gate, on the real bytes — the same call `fauna.posts.delete` makes.
    let verified = fauna_core::encoding::decode_tombstone(&wire)
        .map_err(|e| internal(format!("delegated tombstone rejected at the gate: {e}")))?;

    match crate::routes::delete_post_core(
        state,
        *actor,
        &verified,
        digest,
        crate::routes::RenderSite::Now,
    )
    .await
    {
        // AlreadyGone is a success: a retried batch must converge, and the
        // journal arm's tombstone-an-absent-record no-op sets the precedent.
        Ok(_) => {}
        Err(crate::routes::PostDeleteError::NotAuthor) => {
            return Ok(refused(
                "policy",
                "that post belongs to another account and cannot be deleted from here",
            ));
        }
        Err(crate::routes::PostDeleteError::Internal(msg)) => {
            return Err(internal(msg));
        }
    }

    // Relay to the paired public nest, so a forwarded copy does not outlive
    // the original — the same leg `posts_delete_handler` fires, with the same
    // signed body, so the peer re-verifies the envelope itself.
    crate::routes::maybe_enqueue_delete_outbox(
        state,
        &verified.author.0,
        &bytes::Bytes::from(wire),
    )
    .await;

    Ok(ExternalWriteResult {
        at_uri: Some(at_uri),
        rkey: Some(rkey.to_string()),
        // The bridge drops its `post_map` row for this id in the same funnel
        // commit as the record.
        fauna_post_id: Some(hex::encode(digest)),
        ..Default::default()
    })
}

/// Round-trip one `app.bsky.actor.profile` write into a real Fauna profile
/// update (`atproto-pds-full.md` D1 + § F2 detail's `putRecord` routing): merge
/// the record over the account's current profile, sign the result with the
/// delegated sub-key `K` with the identity-signed cert embedded, and put it
/// through the very same `ingest_profile_core` gate `fauna.profile.set` uses.
///
/// The structure mirrors [`round_trip_post_create`] deliberately, and for the
/// same reason: sharing the gate is the *proof*. `ingest_profile_core` re-runs
/// `verify_authoring_envelope` on the real bytes under
/// [`Capability::UpdateProfile`](fauna_core::data::Capability), so a profile
/// this arm produces provably cannot be one the ordinary publish path would
/// reject — dropping the cert makes it reject. A direct content-row write would
/// be a second, unverified profile door.
///
/// **The merge, and why it is not a replacement.** An
/// `app.bsky.actor.profile` record can express only a display name, a
/// description and two blob refs; a Fauna `Profile` also carries the account's
/// `nests`, `links`, `admin_nests`, inbox mode and recovery-key mirror. Writing
/// the record as a *whole* profile would silently destroy all of those — most
/// damagingly `nests`, which is how peers find the account at all — for a user
/// who only edited their bio in another app. So the line is: fields **inside**
/// the record's translatable scope (`display_name`, `bio`) are taken from the
/// record and are authoritative, absence included, so clearing a display name
/// in bsky.app clears it in Fauna; every field **outside** that scope is
/// preserved untouched.
///
/// **Avatar and banner are INSIDE the scope, and authoritative the same way**
/// (`atproto-pds-full.md` § F2 detail, *a picture crosses INBOUND by resolution,
/// never by trust*). A record that omits or clears `avatar` clears the user's
/// picture, because otherwise a picture set anywhere would be unremovable from
/// bsky.app — and clearing destroys nothing: the ref stops being referenced,
/// the bytes stay, and any Fauna app can set it again. The *unchanged* case
/// needs no special arm: the caller echoes the ref our projection published, it
/// resolves to the `ContentHash` the profile already holds, and the assignment
/// is a no-op — content addressing, not a diff.
///
/// **Never trusted, always resolved.** Each picture ref is resolved through
/// [`resolve_external_blob_ref`]'s two owners, and a ref that resolves to
/// nothing has already refused the whole batch in [`preflight_blob_refs`] — so
/// by the time this arm runs, every picture it sets is bytes this PDS serves.
/// The arm re-asks that same owner for the value rather than carrying one
/// forward, the `media_items_for_post` pattern: two askers, one answer.
///
/// **`updated_at` is nest-stamped, unlike a post's adopted `createdAt`.** The
/// profile lexicon carries no creation instant to adopt, and nothing derives an
/// rkey from this one (the singleton's key is the constant `self`). It also
/// means the chain verify's step-5 expiry comparison here runs against an
/// instant the external app does not choose — the back-dating hole finding
/// closed for posts cannot open on this path at all.
#[cfg(feature = "bluesky")]
#[allow(clippy::too_many_arguments)]
async fn round_trip_profile_update(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    did: &str,
    index: usize,
    w: &ExternalWrite,
    signer: &fauna_core::identity::ActorKeypair,
    cert_bytes: &[u8],
) -> Result<ExternalWriteResult, RpcError> {
    use fauna_bridge_atproto::outbound::PROFILE_SELF_RKEY;
    use fauna_core::data::{InboxMode, Profile, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, canonical_decode, canonical_encode, sign_envelope};
    use fauna_core::identity::ActorId;

    // Re-checked here as the GUARANTEE; the up-front `check_write_input_shape`
    // pass is what makes it decidable before any row is applied.
    let record = require_record(index, w)?;

    // Same shape as the post arm: a record the bridge structurally validated
    // but we cannot express refuses *this write*, not the batch.
    let parsed = match fauna_bridge_atproto::reverse_translate::parse_profile_record(record) {
        Ok(p) => p,
        Err(e) => {
            return Ok(refused(
                "policy",
                format!("record is not a usable app.bsky.actor.profile: {e}"),
            ));
        }
    };

    // Read-modify-write over the current profile. An account with no profile
    // row yet gets a fresh one — there is nothing to preserve, so the empty
    // fields below are the honest starting state, not a loss.
    let current = crate::profile_handlers::latest_profile_bytes(state, actor).await?;
    let mut profile = match current {
        Some(bytes) => {
            fauna_core::encoding::decode_profile(&bytes)
                .map_err(|e| internal(format!("stored profile does not decode: {e}")))?
                .0
        }
        None => Profile {
            actor_id: ActorId(*actor),
            display_name: None,
            bio: None,
            avatar: None,
            banner: None,
            links: Vec::new(),
            nests: Vec::new(),
            admin_nests: Vec::new(),
            load_hint: None,
            inbox_mode: InboxMode::Open,
            recovery_head: None,
            updated_at: Timestamp::now(),
        },
    };
    profile.display_name = parsed.display_name;
    profile.bio = parsed.bio;
    // Authoritative, absence included — and each field independent of the other,
    // so a record carrying only a banner clears the avatar and vice versa. The
    // pre-flight has already refused any ref that resolves to nothing, so an
    // unresolvable one here would be a genuine inconsistency between the two
    // asks; it is reported rather than silently dropping the user's picture.
    profile.avatar = resolve_profile_picture(state, actor, w, parsed.avatar.as_deref()).await?;
    profile.banner = resolve_profile_picture(state, actor, w, parsed.banner.as_deref()).await?;
    profile.updated_at = Timestamp::now();

    let cert: EmbedAsBytes = canonical_decode(cert_bytes)
        .map_err(|e| internal(format!("stored delegation cert is not embed-as-bytes: {e}")))?;
    let (bytes, env) = sign_envelope(signer, &profile)
        .map_err(|e| internal(format!("sign delegated profile: {e}")))?;
    let wire = canonical_encode(&EmbedAsBytes::from_signed(bytes, env).with_signer_auth(cert))
        .map_err(|e| internal(format!("encode delegated profile: {e}")))?;

    // The gate, on the real bytes — the same call `fauna.profile.set` makes.
    crate::profile_handlers::ingest_profile_core(state, *actor, &wire).await?;

    Ok(ExternalWriteResult {
        at_uri: Some(format!("at://{did}/{}/{PROFILE_SELF_RKEY}", w.collection)),
        // The singleton's key is the constant `self`, never the caller's
        // candidate: a profile filed anywhere else is not the profile the
        // network reads, and the projection would keep re-emitting at `self`.
        rkey: Some(PROFILE_SELF_RKEY.to_string()),
        // What the repo must carry is the projection's own rendering of the
        // profile we just stored, NOT the caller's record: the projection owns
        // this collection and re-emits it, so committing the caller's bytes
        // would have the next pass overwrite them — and the `cid` answered here
        // would name bytes that do not survive.
        //
        // **The nest does not render it (F2.4 slice 3 — this closed the
        // pictured-account gap).** It cannot: a projected `avatar`/`banner`
        // blob ref carries the picture's ATProto CID, MIME and size out of the
        // *bridge's* blob store — indexed by the picture's Fauna CID, which is
        // the direction `atproto_blobs` does not answer, and with no row at all
        // for the pictures a Fauna app set — and whether a picture publishes
        // is sniffed from its bytes, which never reach this side
        // (`atproto-pds-bridge.md` § Projection & backfill owns that drop
        // rule). Rendering here would be a second implementation of the
        // projection's rendering, drifting on which CID, which MIME string,
        // which size and publishable-at-all: three of the four silent. So the
        // nest asserts the *property* and the bridge renders through the very
        // function a projection pass uses, making the two equal by identity.
        //
        // Residual, stated rather than overstated: a picture whose bytes fail
        // to fetch during the write's render is dropped for that record, and a
        // later pass that fetches successfully rewrites it — one spurious
        // #commit, self-healing, and strictly narrower than the pre-slice-3
        // behaviour where every pictured account was rewritten every time.
        reproject_record: true,
        // A profile is not a Fauna *post*, so there is no post_map row to
        // write — and none is needed: the override makes the projection
        // idempotent with this write by construction.
        ..Default::default()
    })
}

/// One profile picture field's value, resolved — the arm's re-ask of the same
/// owner the pre-flight asked for its verdict.
///
/// `None` in, `None` out: an absent ref is the user clearing that picture, which
/// is a value and not a failure.
///
/// A ref present but unresolvable is an INTERNAL error, deliberately, not a
/// refusal and never a silent `None`. [`preflight_blob_refs`] already refused
/// every unresolvable ref before anything applied, so reaching here means the
/// two asks disagreed about the same record. Silently
/// clearing the picture would be the same class of bug wearing a success
/// response: the user's avatar would vanish on a call that answered OK.
#[cfg(feature = "bluesky")]
async fn resolve_profile_picture(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    w: &ExternalWrite,
    cid: Option<&str>,
) -> Result<Option<fauna_core::data::ContentHash>, RpcError> {
    let Some(cid) = cid else {
        return Ok(None);
    };
    // The profile arm is the one disposition that may resolve an echo, so this
    // is `resolves_echoed_media`'s answer for its own row, spelled at the site
    // rather than threaded — the arm cannot be reached for any other row.
    resolve_external_blob_ref(state, actor, w, cid, true)
        .await?
        .map(Some)
        .ok_or_else(|| {
            internal(format!(
                "profile picture {cid} resolved in the pre-flight but not in the apply arm"
            ))
        })
}

/// Look up the Fauna post the bridge resolved `at_uri` to, as a `PostId`.
///
/// `resolved_targets` values are the **32-byte digest in lowercase hex** — the
/// same spelling `post_map` keys on, which is
/// `outbound::extract_projection_refs`'s `hex::encode(post_id.digest())` and
/// the `fauna_post_id` this handler answers with. The full 36-byte CID is
/// rebuilt from that digest with the dag-cbor codec, the codec every stored
/// post is encoded under.
///
/// `None` covers absent, unresolvable and malformed alike, because all three
/// mean the same thing to the caller — *no Fauna post is known to be here* —
/// and the ratified answer to that is to journal. A hex string the bridge
/// somehow mangled must not become a reference to whatever post that digest
/// happens to name, so parsing is strict.
#[cfg(feature = "bluesky")]
fn resolved_post_id(w: &ExternalWrite, at_uri: &str) -> Option<fauna_core::data::PostId> {
    let hex_digest = w.resolved_targets.get(at_uri)?;
    let mut digest = [0u8; 32];
    hex::decode_to_slice(hex_digest, &mut digest).ok()?;
    Some(fauna_cbor::Cid::from_digest_dag_cbor(digest))
}

/// Apply one journal-classified write: the record's dag-cbor lands verbatim
/// in `atproto_native_records`, keyed `(actor, collection, rkey)`.
///
/// A delete tombstones (never `DELETE`s) — re-derivability needs the
/// tombstone, and tombstoning an absent record is a no-op success, so a
/// retried batch converges on the same state.
#[cfg(feature = "bluesky")]
async fn journal_write(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    did: &str,
    index: usize,
    w: &ExternalWrite,
    action: fauna_bridge_atproto::membership::WriteAction,
) -> Result<ExternalWriteResult, RpcError> {
    use fauna_bridge_atproto::membership::WriteAction;

    // Every journal write addresses a specific record, so the bridge must
    // have supplied the rkey (it generates a TID when the XRPC caller did
    // not). Only a round-trip create lets the nest derive one.
    //
    // These four shape requirements are re-checked here as the GUARANTEE; the
    // up-front `check_write_input_shape` pass is what makes them decidable before
    // any row is applied. Same accessor on both sides, so the
    // two cannot disagree.
    let rkey = require_rkey(index, w, "journal writes require an rkey")?;

    match action {
        WriteAction::Create | WriteAction::Update => {
            let record = require_record(index, w)?;
            let cid = require_cid(index, w)?;
            state
                .db
                .put_atproto_native_record(actor, &w.collection, rkey, cid, record)
                .await
                .map_err(internal)?;
        }
        WriteAction::Delete => {
            state
                .db
                .tombstone_atproto_native_record(actor, &w.collection, rkey)
                .await
                .map_err(internal)?;
        }
    }

    // No `fauna_post_id`: a journaled record has no Fauna post identity, so the
    // bridge writes no `post_map` row for it — and must not, since that index
    // is keyed by exactly that identity.
    Ok(ExternalWriteResult {
        rkey: Some(rkey.to_string()),
        at_uri: Some(format!("at://{did}/{}/{rkey}", w.collection)),
        ..Default::default()
    })
}

// ── Registration ────────────────────────────────────────────────

/// `fauna.bridges.atproto.fetch_issuer_jwks` — the resource server's source
/// for the nest's public issuer key set, and for the issuer identifier it pins
/// (TP5 / S2d leg 1).
///
/// It hands every approved PDS bridge the same public bytes `/oauth/jwks`
/// serves the open internet, so it can *verify*; the bridge mints nothing.
/// Nothing here is secret — the reply is scoped to the PDS bridge class only
/// because a caller with no resource-server plane has no use for it, not
/// because reading it would reveal anything.
///
/// **The served set is the whole answer.** [`crate::oauth_issuer_key::serve_key_set`]
/// applies the retirement horizon lazily on this read exactly as it does for
/// `/oauth/jwks`, so a key that has left the set is a key that must stop
/// verifying — which is what bounds the forced rotation arm's window at a
/// verifier (`authorization-server.md` § The issuer → *Two rotation arms*).
/// A resource server therefore replaces its set with this one, never merges.
///
/// **`issuer` is [`crate::oauth_issuer_routes::issuer`]** — the same string the
/// discovery document publishes and the deployment's protected-resource
/// document names; `None` only on a domainless nest.
fn fetch_atproto_issuer_jwks_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &bridge_actor,
                "fauna.bridges.atproto.fetch_issuer_jwks",
            )
            .await?;
            let _req: FetchAtprotoIssuerJwksRequest = decode(&payload).map_err(malformed)?;
            // A budget of its own: this feed's caller re-fetches on an unknown
            // `kid`, which a stranger presenting forged tokens at the resource
            // server can provoke. The bridge damps that itself; this is the
            // second line, and sharing a bucket with another fetch would let
            // the provoked traffic starve it.
            if !state
                .bridge_rate_limit
                .check(&bridge_actor, &[0u8; 32], "atproto:issuer-jwks")
            {
                return Err(rate_limited());
            }

            let issuer = crate::oauth_issuer_routes::issuer(&state);

            // A lookup, never a mint: the active signer is seated at boot, and a
            // read that minted here could seal it under a seed a deployment-seed
            // rotation had just retired.
            let db = state.db.clone();
            let now = fauna_core::data::Timestamp::now_secs_or_zero();
            let keys = tokio::task::spawn_blocking(move || {
                crate::oauth_issuer_key::serve_key_set(&db.conn_blocking(), now)
            })
            .await
            .map_err(|e| internal(format!("issuer key set task: {e}")))?
            .map_err(|e| internal(format!("read issuer key set: {e}")))?;

            encode_reply(&FetchAtprotoIssuerJwksReply {
                issuer,
                keys: keys
                    .into_iter()
                    .map(|k| IssuerJwk {
                        kid: k.kid,
                        x: k.x,
                        y: k.y,
                    })
                    .collect(),
            })
        })
    })
}

pub fn register_bridge_atproto_handlers(b: &mut RpcRouterBuilder) {
    let fetch = Duration::from_secs(5);
    let provision = Duration::from_secs(30);
    b.add(
        "fauna.bridges.atproto.fetch_app_credential_verifiers",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: fetch_app_credential_verifiers_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.record_session",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: provision,
            handler: record_session_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.fetch_issuer_jwks",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: fetch_atproto_issuer_jwks_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.deliver_permission_set",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: deliver_permission_set_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.refresh_session",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: refresh_session_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.end_session",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: end_session_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.fetch_preferences",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: fetch_preferences_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.store_preferences",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: provision,
            handler: store_preferences_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.record_blob",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: provision,
            handler: record_blob_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.provision_app_credential",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: provision,
            handler: provision_app_credential_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.list_app_credentials",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: list_app_credentials_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.revoke_app_credential",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: revoke_app_credential_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.list_sessions",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: list_sessions_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.list_grants",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: list_grants_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.revoke_session",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: revoke_session_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.set_external_apps_enabled",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: provision,
            handler: set_external_apps_enabled_handler(),
        },
    );
    // F4 consent ceremony (D3 rung 2). `resolve_consent` is the only one that
    // grants anything, and it is idempotent the way that matters: the storage
    // `WHERE` clause matches an unresolved row exactly once, so a retried
    // approval answers `resolved: false` rather than minting a second grant.
    b.add(
        "fauna.bridges.atproto.list_pending_consents",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: list_pending_consents_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.resolve_consent",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: provision,
            handler: resolve_consent_handler(),
        },
    );
    // D10 authoring delegation. All three are idempotent: the fetch mints
    // first-write-wins and thereafter returns the same K_pub, provision
    // overwrites the cert with an equally-verified one, and a revoke of an
    // absent delegation is a no-op success.
    b.add(
        "fauna.bridges.atproto.fetch_authoring_key",
        RpcKindMeta {
            forbid_replay: false,
            // May mint a keypair on first call — provision weight, not fetch.
            default_deadline: provision,
            handler: fetch_authoring_key_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.fetch_authoring_delegation",
        RpcKindMeta {
            forbid_replay: false,
            // A pure read that mints nothing — fetch weight, unlike
            // `fetch_authoring_key` above, which may mint on first call.
            default_deadline: fetch,
            handler: fetch_authoring_delegation_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.provision_authoring_delegation",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: provision,
            handler: provision_authoring_delegation_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.revoke_authoring_delegation",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: revoke_authoring_delegation_handler(),
        },
    );
    // S3 projection reads — pure reads, replay-safe at fetch weight.
    b.add(
        "fauna.bridges.atproto.set_integration_level",
        RpcKindMeta {
            // Idempotent by construction (a move to the level already in force
            // is a no-op success), so a replayed retry is safe — which is what
            // makes the level-written-last crash story converge.
            forbid_replay: false,
            // A hosted entry can boot the bridge and round-trip the consume-side
            // provider's unlink; the mint itself is asynchronous.
            default_deadline: provision,
            handler: set_integration_level_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.delete_presence",
        RpcKindMeta {
            // Idempotent: the tombstone write, the session revoke and the
            // authoring-key destroy all converge, so a replayed confirm
            // destroys the same presence once.
            forbid_replay: false,
            // Account-side teardown only — the repo sweep is the bridge's own
            // pass, so this call does not wait on it.
            default_deadline: provision,
            handler: delete_presence_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.request_tombstone",
        RpcKindMeta {
            // Idempotent: the flag converges, so a replayed confirm records the
            // same opt-in once.
            forbid_replay: false,
            default_deadline: provision,
            handler: request_tombstone_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.record_tombstone",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: provision,
            handler: record_tombstone_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.get_integration_status",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: get_integration_status_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.fetch_public_posts",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: fetch_atproto_public_posts_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.fetch_profile",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: fetch_atproto_profile_handler(),
        },
    );
    // F2 — the external write path. Gated like the nostr handlers: the kind
    // is registered unconditionally in `fauna_protocol::kind` (the protocol
    // crate has no `bluesky` feature), while the handler needs the
    // `fauna-bridge-atproto` membership classifier the feature pulls in.
    #[cfg(feature = "bluesky")]
    b.add(
        "fauna.bridges.atproto.ingest_external_write",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: provision,
            handler: ingest_external_write_handler(),
        },
    );
}

#[cfg(test)]
use bytes::Bytes;
#[cfg(test)]
use fauna_protocol::encode_canonical;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    const USER: [u8; 32] = [42u8; 32];
    const BRIDGE: [u8; 32] = [77u8; 32];

    /// The history-backfill floor these DB-level assertions want: none. They
    /// ask "did this post land in the projection at all", which the floor —
    /// a *consent* filter the handler applies from the account's stored
    /// opt-in (`fetch_atproto_public_posts`) — is not the subject of.
    ///
    /// Gated with its only callers: every use is in the `bluesky`-only
    /// `external_write` module, and an ungated constant would be dead code in
    /// the default nest build (which `-D warnings` makes a hard error).
    #[cfg(feature = "bluesky")]
    const NO_PROJECTION_FLOOR: i64 = 0;

    async fn fixture_state() -> Arc<AppState> {
        crate::test_support::fixture_state()
    }

    /// Like `fixture_state` but booted with a deployment signing key: the seed
    /// rests in `nest_keypair` and the boot step has seated the issuer key set
    /// under it, as `start_server` leaves a nest before it serves. A nest the
    /// boot step never seated has no issuer key, and the feed answers an error
    /// rather than an empty set — the honest answer, pinned by
    /// `the_issuer_jwks_feed_mints_nothing_before_the_boot_mint`.
    async fn fixture_state_with_nest_key() -> Arc<AppState> {
        let seed = [0x42u8; 32];
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
        db.set_nest_keypair(&seed, &signing_key.verifying_key().to_bytes())
            .await
            .expect("seat the deployment keypair");
        crate::test_support::boot_mint(&db).await;
        Arc::new(AppState {
            nest_signing_key: Some(signing_key),
            ..AppState::for_test(db)
        })
    }

    /// User with a handle + an approved atproto.pds bridge service user.
    async fn seed_user_and_bridge(state: &Arc<AppState>) {
        state.db.create_user(&USER, "free", "test").await.unwrap();
        state.db.set_handle(&USER, "alice").await.unwrap();
        state
            .db
            .create_pending_bridge_service_user(&BRIDGE, BridgeRole::AtprotoPds, "atproto-1")
            .await
            .unwrap();
        state
            .db
            .upsert_bridge_x25519(&BRIDGE, &[88u8; 32])
            .await
            .unwrap();
        state
            .db
            .approve_bridge_service_user(&BRIDGE, None)
            .await
            .unwrap();
    }

    /// Enrol USER's ATProto identity the way the depth selector's transition
    /// does, with the history-backfill answer that decides their projection
    /// floor. Production never pages a user the bridge did not read off the
    /// identity roster, so a projection test without this is testing a shape
    /// that cannot occur — and, since the floor fails closed, would page
    /// nothing at all.
    async fn enroll_atproto_identity(state: &Arc<AppState>, history_backfill: bool) {
        state
            .db
            .upsert_atproto_identity_intent(&USER, "plc", "did:key:zDnaeUSER")
            .await
            .unwrap();
        state
            .db
            .set_atproto_history_backfill(&USER, history_backfill)
            .await
            .unwrap();
    }

    fn enc<T: serde::Serialize>(req: &T) -> Bytes {
        Bytes::from(encode_canonical(req).unwrap().to_vec())
    }

    async fn provision_credential(state: &Arc<AppState>, credential_id: &str, dm: bool) {
        let h = provision_app_credential_handler();
        let req = ProvisionAppCredentialRequest {
            credential_id: credential_id.into(),
            label: credential_id.into(),
            verifier: "$argon2id$v=19$m=65536,t=2,p=1$c2FsdA$aGFzaA".into(),
            dm_allowed: dm,
            extra: Default::default(),
        };
        let reply = h(state.clone(), USER, enc(&req)).await.expect("provision");
        let reply: ProvisionAppCredentialReply = decode(&reply).unwrap();
        assert!(reply.ok);
    }

    #[tokio::test]
    async fn provision_then_bridge_fetches_verifiers_by_handle() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        provision_credential(&state, "ivory", true).await;

        let h = fetch_app_credential_verifiers_handler();
        // Bare handle and handle.domain both resolve.
        for identifier in ["alice", "alice.fauna.example"] {
            let req = FetchAppCredentialVerifiersRequest {
                identifier: identifier.into(),
                extra: Default::default(),
            };
            let reply = h(state.clone(), BRIDGE, enc(&req)).await.expect("fetch");
            let reply: FetchAppCredentialVerifiersReply = decode(&reply).unwrap();
            assert_eq!(
                reply.actor_id.as_deref().map(|v| &v[..]),
                Some(&USER[..]),
                "{identifier}"
            );
            assert!(reply.external_apps_enabled);
            assert_eq!(reply.verifiers.len(), 1);
            assert_eq!(reply.verifiers[0].credential_id, "ivory");
            assert!(reply.verifiers[0].dm_allowed);
            assert!(reply.verifiers[0].verifier.starts_with("$argon2id$"));
        }
    }

    /// A second device that has not refreshed its listing derives the same
    /// kebab id from the same label and mints its own secret. Replacing the
    /// first device's row would silently stop its app password authenticating,
    /// with no signal anywhere — so the nest refuses instead, and the race
    /// degrades to a visible retry.
    #[tokio::test]
    async fn provision_refuses_a_duplicate_credential_id_and_keeps_the_first_verifier() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        provision_credential(&state, "ivory", true).await;

        let h = provision_app_credential_handler();
        let req = ProvisionAppCredentialRequest {
            credential_id: "ivory".into(),
            label: "ivory".into(),
            verifier: "$argon2id$v=19$m=65536,t=2,p=1$c2FsdA$c2Vjb25k".into(),
            dm_allowed: false,
            extra: Default::default(),
        };
        let err = h(state.clone(), USER, enc(&req))
            .await
            .expect_err("a duplicate credential_id must be refused");
        assert_eq!(err.code, "fauna.bridges.atproto.credential_exists");

        // The first device's verifier and its dm_allowed both survive.
        let rows = state.db.list_atproto_app_credentials(&USER).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].verifier.ends_with("aGFzaA"), "first verifier died");
        assert!(rows[0].dm_allowed, "first dm_allowed was clobbered");
    }

    #[tokio::test]
    async fn fetch_verifiers_unknown_identifier_resolves_empty() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        let h = fetch_app_credential_verifiers_handler();
        for identifier in ["bob", "did:plc:abc123", ""] {
            let req = FetchAppCredentialVerifiersRequest {
                identifier: identifier.into(),
                extra: Default::default(),
            };
            let reply = h(state.clone(), BRIDGE, enc(&req)).await.expect("fetch");
            let reply: FetchAppCredentialVerifiersReply = decode(&reply).unwrap();
            assert!(reply.actor_id.is_none(), "{identifier}");
            assert!(reply.verifiers.is_empty());
        }
    }

    /// Slice 4d: `login_did` is the DID the bridge mints the session `sub`
    /// from, and it is filled ONLY for an active identity with a minted DID.
    ///
    /// The three `None` cases are one rule with three causes, and the rule
    /// lives here because the bridge does not read identity status: no row at
    /// all (Bluesky never enabled), a `pending` row (the mint loop has not
    /// answered yet — no repo exists to serve), and a `deactivated` row (the
    /// layer-2 step-down, which `atproto-pds-bridge.md` § Disable & revocation
    /// ratifies as suspending the account's ENTIRE ATProto presence; revoking
    /// its live sessions accomplishes nothing if the next login re-issues one).
    #[tokio::test]
    async fn login_did_is_answered_only_for_an_active_minted_identity() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        provision_credential(&state, "ivory", false).await;
        let h = fetch_app_credential_verifiers_handler();
        let fetch = async |identifier: &str| -> FetchAppCredentialVerifiersReply {
            let req = FetchAppCredentialVerifiersRequest {
                identifier: identifier.into(),
                extra: Default::default(),
            };
            decode(&h(state.clone(), BRIDGE, enc(&req)).await.expect("fetch")).unwrap()
        };

        // (1) No identity row: credentials exist, but there is no ATProto
        // presence to log into.
        let reply = fetch("alice").await;
        assert_eq!(reply.actor_id.as_deref().map(|v| &v[..]), Some(&USER[..]));
        assert!(reply.login_did.is_none(), "no identity row must not log in");

        // (2) Pending — intent recorded, DID not yet minted.
        state
            .db
            .upsert_atproto_identity_intent(&USER, "plc", "did:key:zTestRotationPub")
            .await
            .unwrap();
        assert!(
            fetch("alice").await.login_did.is_none(),
            "pending must not log in"
        );

        // (3) Active + minted — the one case that logs in, and it answers the
        // stored DID verbatim rather than anything derived from the actor id.
        const DID: &str = "did:plc:7iza6de2dwap2sbkpav7c6c6";
        state
            .db
            .record_atproto_minted(&USER, DID, None)
            .await
            .unwrap();
        assert_eq!(fetch("alice").await.login_did.as_deref(), Some(DID));

        // …and that DID is itself a working login identifier, so an app can log
        // back in with the `sub` it was handed.
        let by_did = fetch(DID).await;
        assert_eq!(by_did.actor_id.as_deref().map(|v| &v[..]), Some(&USER[..]));
        assert_eq!(by_did.login_did.as_deref(), Some(DID));

        // (4) Deactivated — the DID is RETAINED (re-enabling restores the same
        // identity) but the presence is suspended, so login stops.
        state
            .db
            .set_atproto_identity_active(&USER, false)
            .await
            .unwrap();
        assert!(
            fetch("alice").await.login_did.is_none(),
            "a step-down that only revokes sessions is undone by the next login"
        );
        assert_eq!(
            state
                .db
                .get_atproto_identity(&USER)
                .await
                .unwrap()
                .unwrap()
                .did
                .as_deref(),
            Some(DID),
            "the DID must survive deactivation — it is what makes re-enable restore the same identity"
        );
    }

    #[tokio::test]
    async fn fetch_verifiers_denied_for_user_class() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        let h = fetch_app_credential_verifiers_handler();
        let req = FetchAppCredentialVerifiersRequest {
            identifier: "alice".into(),
            extra: Default::default(),
        };
        let err = h(state.clone(), USER, enc(&req)).await.expect_err("denied");
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn user_cannot_call_bridge_session_kinds() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        let h = record_session_handler();
        let req = RecordSessionRequest {
            actor_id: USER.to_vec(),
            session_id: b"sess".to_vec(),
            plane: "app_credential".into(),
            credential_id: None,
            client_note: None,
            expires_at: i64::MAX,
            extra: Default::default(),
        };
        let err = h(state.clone(), USER, enc(&req)).await.expect_err("denied");
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    /// The permission-set request call, both legs, over a real subscribed
    /// bridge connection: the PAR's resolver pushes `permission_set_requested`
    /// to the approved PDS bridge, the bridge's `deliver_permission_set`
    /// releases the waiter, and the resolver hands back exactly the bytes the
    /// bridge delivered, verbatim.
    #[tokio::test]
    async fn a_permission_set_request_round_trips_through_the_bridge_connection() {
        use fauna_bridge_atproto::permission_set::ParsedInclude;

        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        let (_conn, mut rx) = state.ws.subscribe(BRIDGE);

        // spawn-ok(test): the resolver parks on the bridge's answer, so the
        // test drives the bridge leg while it runs; the join below reaps it.
        let resolving = tokio::spawn({
            let state = state.clone();
            async move {
                crate::oauth_as_permission_sets::resolve_permission_sets(
                    &state,
                    &[ParsedInclude {
                        nsid: "com.example.calendar.appPerms".into(),
                        aud: None,
                    }],
                )
                .await
            }
        });

        let frame = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("the resolver pushes to the connected bridge")
            .expect("connection open");
        let fauna_protocol::Frame::Push(push) = fauna_protocol::decode_frame(&frame).unwrap()
        else {
            panic!("expected a Push frame")
        };
        assert_eq!(push.kind, "fauna.bridges.atproto.permission_set_requested");
        let fauna_protocol::PushEvent::BridgeAtprotoPermissionSetRequested(asked) =
            fauna_protocol::PushEvent::from_push(&push.kind, push.payload)
        else {
            panic!("the request must classify as its own variant")
        };
        assert_eq!(asked.nsid, "com.example.calendar.appPerms");
        assert_eq!(asked.request_id.len(), 16);

        let deliver = deliver_permission_set_handler();
        let reply: DeliverPermissionSetReply = decode(
            &deliver(
                state.clone(),
                BRIDGE,
                enc(&DeliverPermissionSetRequest {
                    request_id: asked.request_id.clone(),
                    nsid: asked.nsid.clone(),
                    record: Some(ByteBuf::from(b"verified dag-cbor".to_vec())),
                    extra: Default::default(),
                }),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert!(reply.accepted, "a live waiter accepts the delivery");

        let records = resolving
            .await
            .unwrap()
            .expect("the delivered set resolves the request");
        assert_eq!(records, vec![b"verified dag-cbor".to_vec()]);

        // A second answer under the same id matches nothing — the waiter is
        // gone — and says so without erroring.
        let reply: DeliverPermissionSetReply = decode(
            &deliver(
                state.clone(),
                BRIDGE,
                enc(&DeliverPermissionSetRequest {
                    request_id: asked.request_id,
                    nsid: asked.nsid,
                    record: None,
                    extra: Default::default(),
                }),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert!(!reply.accepted);
    }

    /// The bridge answering "could not resolve" refuses the PAR at once — the
    /// waiter is released with no record, and the resolver names the set.
    #[tokio::test]
    async fn a_bridge_refusal_refuses_the_request_without_waiting_out_the_deadline() {
        use fauna_bridge_atproto::permission_set::ParsedInclude;

        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        let (_conn, mut rx) = state.ws.subscribe(BRIDGE);

        // spawn-ok(test): the resolver parks on the bridge's answer, so the
        // test drives the bridge leg while it runs; the join below reaps it.
        let resolving = tokio::spawn({
            let state = state.clone();
            async move {
                crate::oauth_as_permission_sets::resolve_permission_sets(
                    &state,
                    &[ParsedInclude {
                        nsid: "com.example.calendar.appPerms".into(),
                        aud: None,
                    }],
                )
                .await
            }
        });
        let frame = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let fauna_protocol::Frame::Push(push) = fauna_protocol::decode_frame(&frame).unwrap()
        else {
            panic!("expected a Push frame")
        };
        let fauna_protocol::PushEvent::BridgeAtprotoPermissionSetRequested(asked) =
            fauna_protocol::PushEvent::from_push(&push.kind, push.payload)
        else {
            panic!("the request must classify as its own variant")
        };

        let started = std::time::Instant::now();
        let _ = deliver_permission_set_handler()(
            state.clone(),
            BRIDGE,
            enc(&DeliverPermissionSetRequest {
                request_id: asked.request_id,
                nsid: asked.nsid,
                record: None,
                extra: Default::default(),
            }),
        )
        .await
        .unwrap();
        let outcome = resolving.await.unwrap();
        assert_eq!(
            outcome,
            Err(crate::oauth_as_permission_sets::UnresolvedSet {
                nsid: "com.example.calendar.appPerms".into()
            })
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// The timeout arm: a connected bridge that never answers (a wedged
    /// bridge, a shed frame) is the deadline's to
    /// refuse, and the refusal leaves nothing registered.
    #[tokio::test]
    async fn a_silent_bridge_is_refused_at_the_deadline() {
        use fauna_bridge_atproto::permission_set::ParsedInclude;

        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        let (_conn, mut rx) = state.ws.subscribe(BRIDGE);

        let outcome = crate::oauth_as_permission_sets::resolve_permission_sets_within(
            &state,
            &[ParsedInclude {
                nsid: "com.example.calendar.appPerms".into(),
                aud: None,
            }],
            Duration::from_millis(200),
        )
        .await;
        assert_eq!(
            outcome,
            Err(crate::oauth_as_permission_sets::UnresolvedSet {
                nsid: "com.example.calendar.appPerms".into()
            })
        );
        assert!(
            rx.try_recv().is_ok(),
            "the push did go out before the deadline refused"
        );
        // Nothing waits any more: a late delivery matches nothing.
        let reply: DeliverPermissionSetReply = decode(
            &deliver_permission_set_handler()(
                state.clone(),
                BRIDGE,
                enc(&DeliverPermissionSetRequest {
                    request_id: vec![0u8; 16],
                    nsid: "com.example.calendar.appPerms".into(),
                    record: Some(ByteBuf::from(b"late".to_vec())),
                    extra: Default::default(),
                }),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert!(!reply.accepted);
    }

    #[tokio::test]
    async fn deliver_permission_set_is_bridge_only() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        let err = deliver_permission_set_handler()(
            state.clone(),
            USER,
            enc(&DeliverPermissionSetRequest {
                request_id: vec![0u8; 16],
                nsid: "com.example.calendar.appPerms".into(),
                record: Some(ByteBuf::from(b"forged".to_vec())),
                extra: Default::default(),
            }),
        )
        .await
        .expect_err("a user may not deliver a permission set");
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn record_session_registers_and_stamps_last_used() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        provision_credential(&state, "ivory", false).await;

        let h = record_session_handler();
        let req = RecordSessionRequest {
            actor_id: USER.to_vec(),
            session_id: b"sess-1".to_vec(),
            plane: "app_credential".into(),
            credential_id: Some("ivory".into()),
            client_note: Some("Ivory for Bluesky".into()),
            expires_at: crate::db::now_epoch_millis() + 1_000_000,
            extra: Default::default(),
        };
        let reply = h(state.clone(), BRIDGE, enc(&req)).await.expect("record");
        let reply: RecordSessionReply = decode(&reply).unwrap();
        assert!(reply.ok);

        let sessions = state.db.list_atproto_sessions(&USER).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].client_note.as_deref(),
            Some("Ivory for Bluesky")
        );
        let creds = state.db.list_atproto_app_credentials(&USER).await.unwrap();
        assert!(creds[0].last_used_at.is_some());

        // Unknown plane refused.
        let bad = RecordSessionRequest {
            plane: "carrier_pigeon".into(),
            ..req
        };
        assert!(h(state.clone(), BRIDGE, enc(&bad)).await.is_err());
    }

    /// **F4 slice 7 — one call writes both rows, and the OAuth session it
    /// creates rotates through the SAME registry the app plane uses.**
    ///
    /// The second half is the load-bearing part: the grant's refresh family is
    /// an ordinary `atproto_sessions` row, so rotate-on-use and the reuse
    /// family-kill apply to OAuth refresh tokens with no second implementation
    /// — which is why `/oauth/token`'s refresh grant calls the existing
    /// plane-agnostic `refresh_session` rather than growing its own path.
    #[tokio::test]
    async fn record_oauth_grant_writes_the_grant_and_its_rotatable_session() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        let far = crate::db::now_epoch_millis() + 1_000_000;

        let recorded = record_oauth_grant(
            &state,
            &USER,
            b"grant-fam-1",
            "https://app.example.com/client-metadata.json",
            Some("Example Client"),
            &["atproto".to_string(), "repo:*".to_string()],
            &[],
            "jkt-thumbprint",
            far,
            Some(far),
            crate::db::atproto_pds::OAUTH_GRANT_ISSUER_NEST,
            &crate::db::third_party_principals::UNATTESTED_DEVICE,
        )
        .await
        .expect("grant");
        assert_eq!(recorded, GrantRecorded::Yes);

        // The session half is listed like any other, on the `oauth` plane and
        // with no minting credential.
        let sessions = state.db.list_atproto_sessions(&USER).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].plane, "oauth");
        assert_eq!(sessions[0].credential_id, None);
        assert_eq!(sessions[0].session_id, b"grant-fam-1".to_vec());

        // …and it rotates through the shared registry path, family-kill
        // included, with no OAuth-specific code.
        let rh = refresh_session_handler();
        let mk = |presented: &[u8], new: &[u8]| RefreshSessionRequest {
            actor_id: USER.to_vec(),
            session_id: b"grant-fam-1".to_vec(),
            presented_jti: presented.to_vec(),
            new_jti: new.to_vec(),
            new_expires_at: far,
            extra: Default::default(),
        };
        let r: RefreshSessionReply = decode(
            &rh(state.clone(), BRIDGE, enc(&mk(b"grant-fam-1", b"j2")))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(r.status, refresh_status::ROTATED);
        let r: RefreshSessionReply = decode(
            &rh(state.clone(), BRIDGE, enc(&mk(b"grant-fam-1", b"j3")))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            r.status,
            refresh_status::REUSE_DETECTED,
            "an OAuth refresh family must die on replay exactly like an app-plane one"
        );
    }

    /// The kill-switch gates the grant write too, and answers
    /// `ExternalAppsDisabled` rather than erroring — so the token endpoint can turn it into a clean
    /// OAuth refusal. **The negative half is the point: nothing may be left
    /// behind**, or the user's app would list a connection for an account
    /// whose external-app plane is OFF.
    #[tokio::test]
    async fn kill_switch_off_refuses_a_grant_and_writes_nothing() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        state
            .db
            .set_atproto_external_apps_enabled(&USER, false)
            .await
            .unwrap();
        let far = crate::db::now_epoch_millis() + 1_000_000;

        let recorded = record_oauth_grant(
            &state,
            &USER,
            b"grant-fam-2",
            "https://app.example.com/client-metadata.json",
            None,
            &["atproto".to_string()],
            &[],
            "jkt",
            far,
            None,
            crate::db::atproto_pds::OAUTH_GRANT_ISSUER_NEST,
            &crate::db::third_party_principals::UNATTESTED_DEVICE,
        )
        .await
        .expect("reply");
        assert_eq!(recorded, GrantRecorded::ExternalAppsDisabled);
        assert!(
            state
                .db
                .list_atproto_sessions(&USER)
                .await
                .unwrap()
                .is_empty(),
            "a refused grant must leave no session behind"
        );
    }

    #[tokio::test]
    async fn kill_switch_off_refuses_record_and_refresh() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        let far = crate::db::now_epoch_millis() + 1_000_000;
        state
            .db
            .insert_atproto_session(&USER, b"sess-1", "app_credential", None, None, far)
            .await
            .unwrap();

        // Flip OFF via the user-class handler (emits the nudge too).
        let h = set_external_apps_enabled_handler();
        let req = SetExternalAppsEnabledRequest {
            enabled: false,
            extra: Default::default(),
        };
        let reply = h(state.clone(), USER, enc(&req)).await.expect("set");
        let reply: SetExternalAppsEnabledReply = decode(&reply).unwrap();
        assert!(reply.ok);

        // record_session refused with the distinct disabled code.
        let h = record_session_handler();
        let req = RecordSessionRequest {
            actor_id: USER.to_vec(),
            session_id: b"sess-2".to_vec(),
            plane: "app_credential".into(),
            credential_id: None,
            client_note: None,
            expires_at: far,
            extra: Default::default(),
        };
        let err = h(state.clone(), BRIDGE, enc(&req)).await.expect_err("off");
        assert_eq!(err.code, "fauna.bridges.atproto.disabled");

        // refresh_session reports the disabled status.
        let h = refresh_session_handler();
        let req = RefreshSessionRequest {
            actor_id: USER.to_vec(),
            session_id: b"sess-1".to_vec(),
            presented_jti: b"sess-1".to_vec(),
            new_jti: b"jti-2".to_vec(),
            new_expires_at: far,
            extra: Default::default(),
        };
        let reply = h(state.clone(), BRIDGE, enc(&req)).await.expect("reply");
        let reply: RefreshSessionReply = decode(&reply).unwrap();
        assert_eq!(reply.status, refresh_status::DISABLED);

        // Flip back ON restores refresh.
        state
            .db
            .set_atproto_external_apps_enabled(&USER, true)
            .await
            .unwrap();
        let reply = h(state.clone(), BRIDGE, enc(&req)).await.expect("reply");
        let reply: RefreshSessionReply = decode(&reply).unwrap();
        assert_eq!(reply.status, refresh_status::ROTATED);
    }

    #[tokio::test]
    async fn refresh_reuse_reports_reuse_detected() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        let far = crate::db::now_epoch_millis() + 1_000_000;
        state
            .db
            .insert_atproto_session(&USER, b"fam", "app_credential", None, None, far)
            .await
            .unwrap();
        let (_conn, mut rx) = state.ws.subscribe(BRIDGE);
        let h = refresh_session_handler();
        let mk = |presented: &[u8], new: &[u8]| RefreshSessionRequest {
            actor_id: USER.to_vec(),
            session_id: b"fam".to_vec(),
            presented_jti: presented.to_vec(),
            new_jti: new.to_vec(),
            new_expires_at: far,
            extra: Default::default(),
        };
        let r: RefreshSessionReply = decode(
            &h(state.clone(), BRIDGE, enc(&mk(b"fam", b"j2")))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(r.status, refresh_status::ROTATED);
        assert!(
            rx.try_recv().is_err(),
            "an ordinary rotation must not nudge — that would push on every refresh of every connected app"
        );
        let r: RefreshSessionReply = decode(
            &h(state.clone(), BRIDGE, enc(&mk(b"fam", b"j3")))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(r.status, refresh_status::REUSE_DETECTED);
        // The family-kill is a token-theft signal, so it — alone among the
        // three outcomes — nudges the bridge's cached session state.
        let frame = rx
            .try_recv()
            .expect("reuse detection nudges every approved atproto.pds bridge");
        let fauna_protocol::Frame::Push(push) = fauna_protocol::decode_frame(&frame).unwrap()
        else {
            panic!("expected a Push frame")
        };
        assert_eq!(push.kind, "fauna.bridges.atproto.sessions_changed");
        let fauna_protocol::PushEvent::BridgeAtprotoSessionsChanged(nudge) =
            fauna_protocol::PushEvent::from_push(&push.kind, push.payload)
        else {
            panic!("the nudge must classify as its own variant")
        };
        assert_eq!(nudge.actor_id, USER.to_vec());
        assert_eq!(
            nudge.external_apps_enabled, None,
            "this is a session termination, not a kill-switch flip"
        );
        // Family dead — even the current jti refuses now.
        let r: RefreshSessionReply = decode(
            &h(state.clone(), BRIDGE, enc(&mk(b"j2", b"j4")))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(r.status, refresh_status::INVALID);
    }

    #[tokio::test]
    async fn credential_revoke_cascades_and_lists_reflect_it() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        provision_credential(&state, "ivory", false).await;
        let far = crate::db::now_epoch_millis() + 1_000_000;
        state
            .db
            .insert_atproto_session(&USER, b"s1", "app_credential", Some("ivory"), None, far)
            .await
            .unwrap();

        // list_app_credentials sees it (and the default-ON flag).
        let h = list_app_credentials_handler();
        let req = ListAppCredentialsRequest {
            extra: Default::default(),
        };
        let reply: ListAppCredentialsReply =
            decode(&h(state.clone(), USER, enc(&req)).await.unwrap()).unwrap();
        assert_eq!(reply.credentials.len(), 1);
        assert!(reply.external_apps_enabled);

        // Revoke cascades to the session.
        let h = revoke_app_credential_handler();
        let req = RevokeAppCredentialRequest {
            credential_id: "ivory".into(),
            extra: Default::default(),
        };
        let reply: RevokeAppCredentialReply =
            decode(&h(state.clone(), USER, enc(&req)).await.unwrap()).unwrap();
        assert!(reply.revoked);
        assert_eq!(reply.sessions_revoked, 1);

        let h = list_sessions_handler();
        let req = ListSessionsRequest {
            extra: Default::default(),
        };
        let reply: ListSessionsReply =
            decode(&h(state.clone(), USER, enc(&req)).await.unwrap()).unwrap();
        assert!(reply.sessions.is_empty());
    }

    #[tokio::test]
    async fn end_session_and_user_revoke_session() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        let far = crate::db::now_epoch_millis() + 1_000_000;
        for sid in [b"s1".as_slice(), b"s2"] {
            state
                .db
                .insert_atproto_session(&USER, sid, "app_credential", None, None, far)
                .await
                .unwrap();
        }

        // Bridge ends s1 (deleteSession).
        let h = end_session_handler();
        let req = EndSessionRequest {
            actor_id: USER.to_vec(),
            session_id: b"s1".to_vec(),
            extra: Default::default(),
        };
        let reply: EndSessionReply =
            decode(&h(state.clone(), BRIDGE, enc(&req)).await.unwrap()).unwrap();
        assert!(reply.ended);
        // Idempotent.
        let reply: EndSessionReply =
            decode(&h(state.clone(), BRIDGE, enc(&req)).await.unwrap()).unwrap();
        assert!(!reply.ended);

        // User revokes s2 from the settings page.
        let h = revoke_session_handler();
        let req = RevokeSessionRequest {
            session_id: b"s2".to_vec(),
            extra: Default::default(),
        };
        let reply: RevokeSessionReply =
            decode(&h(state.clone(), USER, enc(&req)).await.unwrap()).unwrap();
        assert!(reply.revoked);
        assert!(
            state
                .db
                .list_atproto_sessions(&USER)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn preferences_round_trip_and_cap() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        let fetch = fetch_preferences_handler();
        let store = store_preferences_handler();
        let freq = FetchPreferencesRequest {
            actor_id: USER.to_vec(),
            extra: Default::default(),
        };

        // No row yet → None (the bridge answers an empty array).
        let reply: FetchPreferencesReply =
            decode(&fetch(state.clone(), BRIDGE, enc(&freq)).await.unwrap()).unwrap();
        assert!(reply.preferences.is_none());

        // Store an opaque payload, then read the exact bytes back.
        let payload = br#"[{"$type":"app.bsky.actor.defs#savedFeedsPrefV2","items":[]}]"#;
        let sreq = StorePreferencesRequest {
            actor_id: USER.to_vec(),
            preferences: payload.to_vec(),
            extra: Default::default(),
        };
        let sreply: StorePreferencesReply =
            decode(&store(state.clone(), BRIDGE, enc(&sreq)).await.unwrap()).unwrap();
        assert!(sreply.ok);
        let reply: FetchPreferencesReply =
            decode(&fetch(state.clone(), BRIDGE, enc(&freq)).await.unwrap()).unwrap();
        assert_eq!(reply.preferences, Some(ByteBuf::from(payload.to_vec())));

        // Overwrite (upsert) — the read reflects the latest write verbatim.
        let payload2 = br#"[{"$type":"app.bsky.actor.defs#personalDetailsPref"}]"#;
        let sreq2 = StorePreferencesRequest {
            actor_id: USER.to_vec(),
            preferences: payload2.to_vec(),
            extra: Default::default(),
        };
        store(state.clone(), BRIDGE, enc(&sreq2)).await.unwrap();
        let reply: FetchPreferencesReply =
            decode(&fetch(state.clone(), BRIDGE, enc(&freq)).await.unwrap()).unwrap();
        assert_eq!(reply.preferences, Some(ByteBuf::from(payload2.to_vec())));

        // Over-cap is refused — the nest is the source-of-truth authority.
        let too_big = StorePreferencesRequest {
            actor_id: USER.to_vec(),
            preferences: vec![b' '; PREFERENCES_MAX_BYTES + 1],
            extra: Default::default(),
        };
        assert!(store(state.clone(), BRIDGE, enc(&too_big)).await.is_err());
    }

    #[tokio::test]
    async fn preferences_kinds_are_bridge_only() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        // A plain user actor must not reach the bridge-only preferences kinds
        // (they are the attested PDS host's, called on the caller's behalf).
        let freq = FetchPreferencesRequest {
            actor_id: USER.to_vec(),
            extra: Default::default(),
        };
        assert!(
            fetch_preferences_handler()(state.clone(), USER, enc(&freq))
                .await
                .is_err()
        );
        let sreq = StorePreferencesRequest {
            actor_id: USER.to_vec(),
            preferences: b"[]".to_vec(),
            extra: Default::default(),
        };
        assert!(
            store_preferences_handler()(state.clone(), USER, enc(&sreq))
                .await
                .is_err()
        );
    }

    // ── record_blob (F2.4 slice 1) ───────────────────────────────────────────

    const TEST_BLOB_CID: &str = "bafkreiaha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4";

    /// The feed serves the issuer key set the JWKS serves — same `kid`s, same
    /// coordinates — because they are one read of one owner
    /// (`crate::oauth_issuer_key::serve_key_set`). A resource server verifying
    /// against a different set than the one clients discover is the failure
    /// this plane exists to prevent.
    #[tokio::test]
    async fn the_issuer_jwks_feed_serves_the_set_the_jwks_serves() {
        let state = fixture_state_with_nest_key().await;
        seed_user_and_bridge(&state).await;

        let raw = fetch_atproto_issuer_jwks_handler()(
            state.clone(),
            BRIDGE,
            enc(&FetchAtprotoIssuerJwksRequest {}),
        )
        .await
        .unwrap();
        let reply: FetchAtprotoIssuerJwksReply = decode(&raw).unwrap();

        // A booted nest serves the signer its boot step seated, the same as the
        // JWKS route: a usable set rather than an empty one a verifier would
        // cache.
        assert!(
            !reply.keys.is_empty(),
            "the feed served no key at all; a resource server fed this verifies \
             nothing the nest mints"
        );

        let db = state.db.clone();
        let served = tokio::task::spawn_blocking(move || {
            crate::oauth_issuer_key::serve_key_set(
                &db.conn_blocking(),
                fauna_core::data::Timestamp::now_secs_or_zero(),
            )
        })
        .await
        .unwrap()
        .unwrap();

        let from_feed: Vec<(String, String, String)> = reply
            .keys
            .iter()
            .map(|k| (k.kid.clone(), k.x.clone(), k.y.clone()))
            .collect();
        let from_owner: Vec<(String, String, String)> = served
            .iter()
            .map(|k| (k.kid.clone(), k.x.clone(), k.y.clone()))
            .collect();
        assert_eq!(from_feed, from_owner);
    }

    /// The feed hands the resource server the issuer the deployment's
    /// protected-resource document names — the one the discovery document
    /// publishes — so a token the nest mints verifies at the PDS
    /// (`authorization-server.md` § The issuer → *The teaching is one WS-RPC
    /// feed carrying both halves*). Red-verify by withholding it again.
    #[tokio::test]
    async fn the_feed_carries_the_issuer_the_resource_server_is_sent_to() {
        let state = fixture_state_with_nest_key().await;
        seed_user_and_bridge(&state).await;
        state
            .identity_domain
            .store(Some(Arc::new("nest.example".to_string())));

        let raw = fetch_atproto_issuer_jwks_handler()(
            state.clone(),
            BRIDGE,
            enc(&FetchAtprotoIssuerJwksRequest {}),
        )
        .await
        .unwrap();
        let reply: FetchAtprotoIssuerJwksReply = decode(&raw).unwrap();

        assert_eq!(
            reply.issuer,
            Some("https://nest.example".to_string()),
            "the feed withheld or mis-spelled the issuer the PDS pins"
        );
        assert_eq!(
            fauna_bridge_atproto::oauth_metadata::protected_resource_authorization_servers(
                "nest.example"
            ),
            vec![reply.issuer.clone().unwrap()],
            "the resource server sends clients to one issuer and is fed another"
        );
        assert!(!reply.keys.is_empty());
    }

    /// A domainless nest has no issuer identity, so the feed carries none —
    /// and still serves the keys, which are public either way.
    #[tokio::test]
    async fn a_domainless_nest_feeds_keys_and_no_issuer() {
        let state = fixture_state_with_nest_key().await;
        seed_user_and_bridge(&state).await;
        state.identity_domain.store(None);
        assert_eq!(state.web_serving_domain(), "", "precondition: domainless");

        let raw = fetch_atproto_issuer_jwks_handler()(
            state.clone(),
            BRIDGE,
            enc(&FetchAtprotoIssuerJwksRequest {}),
        )
        .await
        .unwrap();
        let reply: FetchAtprotoIssuerJwksReply = decode(&raw).unwrap();
        assert_eq!(reply.issuer, None);
        assert!(!reply.keys.is_empty());
    }

    /// The feed only looks the key set up. On a nest whose boot mint has not
    /// seated an issuer key it answers an error and mints nothing, rather than
    /// sealing a key under whatever seed the answering serving generation holds
    /// — which, inside a deployment-seed rotation's hand-off window, is the
    /// retired one.
    #[tokio::test]
    async fn the_issuer_jwks_feed_mints_nothing_before_the_boot_mint() {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
        let state = Arc::new(AppState {
            nest_signing_key: Some(ed25519_dalek::SigningKey::from_bytes(&[0x42u8; 32])),
            ..AppState::for_test(db)
        });
        seed_user_and_bridge(&state).await;

        let reply = fetch_atproto_issuer_jwks_handler()(
            state.clone(),
            BRIDGE,
            enc(&FetchAtprotoIssuerJwksRequest {}),
        )
        .await;

        let db = state.db.clone();
        let rows: i64 = tokio::task::spawn_blocking(move || {
            db.conn_blocking()
                .query_row("SELECT COUNT(*) FROM oauth_issuer_keys", [], |r| r.get(0))
                .unwrap()
        })
        .await
        .unwrap();
        assert_eq!(rows, 0, "the feed minted an issuer key on a read");
        assert!(
            reply.is_err(),
            "no key is seated, and the feed says so rather than serving an empty set"
        );
    }

    /// A caller that is not the PDS bridge is refused. The reply carries no
    /// secret, but the class gate is what keeps the plane's caller set honest —
    /// and it is the gate a widening would silently skip.
    #[tokio::test]
    async fn the_issuer_jwks_feed_refuses_a_caller_that_is_not_the_pds_bridge() {
        let state = fixture_state_with_nest_key().await;
        seed_user_and_bridge(&state).await;

        let err = fetch_atproto_issuer_jwks_handler()(
            state.clone(),
            USER,
            enc(&FetchAtprotoIssuerJwksRequest {}),
        )
        .await
        .expect_err("an ordinary user must not reach a bridge-class kind");
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn record_blob_ties_the_cid_to_the_media_ref_for_the_named_account() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        let req = RecordBlobRequest {
            actor_id: USER.to_vec(),
            cid: TEST_BLOB_CID.into(),
            media_ref: vec![5u8; 32],
            extra: Default::default(),
        };
        let raw = record_blob_handler()(state.clone(), BRIDGE, enc(&req))
            .await
            .unwrap();
        let reply: RecordBlobReply = decode(&raw).unwrap();
        assert!(reply.ok);

        // The row is the account's, not the bridge's — the whole reason this
        // leg exists separately from the byte upload (which authenticates as
        // the bridge and cannot know the account).
        let row = state
            .db
            .get_atproto_blob(&USER, TEST_BLOB_CID)
            .await
            .unwrap()
            .expect("row under the named account");
        assert_eq!(row.media_ref, [5u8; 32]);
        assert!(row.referenced_at.is_none());
        assert!(
            state
                .db
                .get_atproto_blob(&BRIDGE, TEST_BLOB_CID)
                .await
                .unwrap()
                .is_none(),
            "the bridge's own actor must hold no blob row"
        );
    }

    /// A `media_ref` that is not a 32-byte blake3 digest must be refused, not
    /// stored: a short/long ref would leave a row naming media that can never
    /// resolve, and the failure would surface much later as a media-less post.
    #[tokio::test]
    async fn record_blob_refuses_a_media_ref_that_is_not_a_content_hash() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        for bad in [vec![5u8; 31], vec![5u8; 33], vec![]] {
            let req = RecordBlobRequest {
                actor_id: USER.to_vec(),
                cid: TEST_BLOB_CID.into(),
                media_ref: bad.clone(),
                extra: Default::default(),
            };
            assert!(
                record_blob_handler()(state.clone(), BRIDGE, enc(&req))
                    .await
                    .is_err(),
                "a {}-byte media_ref must refuse",
                bad.len()
            );
        }
        let req = RecordBlobRequest {
            actor_id: USER.to_vec(),
            cid: String::new(),
            media_ref: vec![5u8; 32],
            extra: Default::default(),
        };
        assert!(
            record_blob_handler()(state.clone(), BRIDGE, enc(&req))
                .await
                .is_err(),
            "an empty cid must refuse"
        );
        assert!(
            state.db.list_atproto_blobs(&USER).await.unwrap().is_empty(),
            "a refused record_blob must write nothing"
        );
    }

    /// The `fauna_cid` this kind answers must be spelled exactly as the OUTBOUND
    /// media extraction spells the same bytes (`ContentHash::to_base32`, via
    /// `outbound::translate_post_for_projection`'s media items). That string is
    /// the provenance key the projection loop's "already published these bytes?"
    /// lookup matches on, so a one-character disagreement would make every image
    /// re-fetch and re-hash forever with nothing failing — the exact class of
    /// two-sides-green-while-disagreeing bug the cross-side pinning rule was written about.
    #[tokio::test]
    async fn the_answered_fauna_cid_is_spelled_as_the_outbound_side_spells_it() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        let bytes = b"the same image bytes, reached from either direction";
        let media_ref = fauna_core::data::ContentHash::of_raw(bytes).digest();

        let req = RecordBlobRequest {
            actor_id: USER.to_vec(),
            cid: TEST_BLOB_CID.into(),
            media_ref: media_ref.to_vec(),
            extra: Default::default(),
        };
        let raw = record_blob_handler()(state.clone(), BRIDGE, enc(&req))
            .await
            .unwrap();
        let reply: RecordBlobReply = decode(&raw).unwrap();
        assert_eq!(
            reply.fauna_cid,
            fauna_core::data::ContentHash::of_raw(bytes).to_base32(),
            "the answered fauna_cid must equal the outbound spelling of the same bytes"
        );
    }

    #[tokio::test]
    async fn record_blob_is_bridge_only() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        let req = RecordBlobRequest {
            actor_id: USER.to_vec(),
            cid: TEST_BLOB_CID.into(),
            media_ref: vec![5u8; 32],
            extra: Default::default(),
        };
        assert!(
            record_blob_handler()(state.clone(), USER, enc(&req))
                .await
                .is_err(),
            "a plain user actor must not reach the bridge-only blob kind"
        );
    }

    #[tokio::test]
    async fn provision_rejects_malformed_inputs() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        let h = provision_app_credential_handler();
        let base = ProvisionAppCredentialRequest {
            credential_id: "ivory".into(),
            label: "ivory".into(),
            verifier: "$argon2id$v=19$m=65536,t=2,p=1$c2FsdA$aGFzaA".into(),
            dm_allowed: false,
            extra: Default::default(),
        };
        let empty_id = ProvisionAppCredentialRequest {
            credential_id: "  ".into(),
            ..base.clone()
        };
        assert!(h(state.clone(), USER, enc(&empty_id)).await.is_err());
        let not_phc = ProvisionAppCredentialRequest {
            verifier: "hunter2".into(),
            ..base.clone()
        };
        assert!(h(state.clone(), USER, enc(&not_phc)).await.is_err());
        let empty_label = ProvisionAppCredentialRequest {
            label: "".into(),
            ..base
        };
        assert!(h(state.clone(), USER, enc(&empty_label)).await.is_err());
    }

    // ── S3 projection reads ─────────────────────────────────────

    /// Store a bare canonical `Post` through the production write path
    /// (`segments::post::store_post` — segment body + projection row) and
    /// return `(post_id, stored_bytes)`.
    async fn seed_public_post(
        state: &Arc<AppState>,
        content: &str,
        created_at_micros: u64,
    ) -> ([u8; 32], Vec<u8>) {
        let post = fauna_core::data::Post {
            author: fauna_core::identity::ActorId(USER),
            created_at: fauna_core::data::Timestamp(created_at_micros),
            body: fauna_core::data::PostBody::Text {
                content: content.into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let bytes = fauna_core::encoding::canonical_encode(&post).unwrap();
        let post_id: [u8; 32] = *blake3::hash(&bytes).as_bytes();
        crate::segments::post::store_post(&state.post_segments, &state.db, &post_id, &bytes, None)
            .await
            .unwrap();
        (post_id, bytes)
    }

    /// A legally taken-down post never crosses the wire the bridge reads.
    ///
    /// The DAO-level pins live in `db::atproto_projection`; this one asserts
    /// the **flow**, because a filter the handler bypasses is no filter:
    /// admin applies a real `legal_takedown` through the production
    /// transaction → `content_meta.legal_takedown_ref` is set → the
    /// `fauna.bridges.atproto.fetch_public_posts` handler the Go bridge calls
    /// stops serving it. Nest is the only gate here — the bridge publishes
    /// whatever this reply contains (a grep for `takedown`/`legal_` across
    /// `internal/atprotorepo/` + `cmd/fauna-atproto-bridge/` returns nothing),
    /// so this assertion is the whole enforcement.
    #[tokio::test]
    async fn bridge_paging_withholds_a_legally_taken_down_post() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        enroll_atproto_identity(&state, true).await;
        let (kept_id, _) = seed_public_post(&state, "ordinary post", 1_000_000).await;
        let (taken_id, _) = seed_public_post(&state, "the taken-down post", 1_500_000).await;

        // The production takedown transaction — flag + obligation + audit.
        state
            .db
            .post_legal_takedown_txn(
                &taken_id,
                &hex::encode(taken_id),
                Some("court-order-1"),
                &USER,
                &BRIDGE[..],
                "test takedown",
                2_000_000,
            )
            .await
            .expect("takedown applied");

        let h = fetch_atproto_public_posts_handler();
        let req = FetchPublicPostsRequest {
            actor_id: ByteBuf::from(USER.to_vec()),
            cursor: None,
            limit: 10,
        };
        let reply: FetchPublicPostsReply =
            decode(&h(state.clone(), BRIDGE, enc(&req)).await.unwrap()).unwrap();

        // The post's BODY never reaches the bridge (the servability predicate)…
        let posts: Vec<&str> = reply
            .items
            .iter()
            .filter(|i| i.kind == public_post_item_kind::POST)
            .map(|i| i.post_id.as_str())
            .collect();
        assert_eq!(
            posts,
            vec![hex::encode(kept_id).as_str()],
            "the taken-down post must never reach the bridge"
        );

        // …and the takedown emits a RETRACTION for it, so a post that was
        // already projected before the takedown is deleted from the Bluesky
        // repo rather than left standing (`moderation.md` § Legal takedown →
        // the off-box publish surfaces). Without the witness the predicate
        // stops only future publication.
        let retractions: Vec<&str> = reply
            .items
            .iter()
            .filter(|i| i.kind == public_post_item_kind::TOMBSTONE)
            .filter_map(|i| i.deleted_post_id.as_deref())
            .collect();
        assert_eq!(
            retractions,
            vec![hex::encode(taken_id).as_str()],
            "the takedown retracts the already-projected record"
        );
    }

    /// Post → delete → page: the stream serves the survivor's stored bytes
    /// verbatim and the delete as a tombstone item at the DELETE instant,
    /// with `next_cursor: None` once exhausted (the production flow behind
    /// `fauna.bridges.atproto.fetch_public_posts`).
    #[tokio::test]
    async fn bridge_pages_public_posts_and_delete_tombstones() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        // Opted into history ⇒ genesis floor, so this paging/tombstone test
        // sees every seeded row regardless of its timestamp.
        enroll_atproto_identity(&state, true).await;
        let (deleted_id, _) = seed_public_post(&state, "first post", 1_000_000).await;
        let (kept_id, kept_bytes) = seed_public_post(&state, "second post", 1_500_000).await;

        // Delete the first post through the shared delete core (the
        // `unrepost` in-process tombstone shape), dated after both creates.
        let tombstone = fauna_core::data::Tombstone {
            author: fauna_core::identity::ActorId(USER),
            post_id: fauna_core::data::PostId::from_digest_dag_cbor(deleted_id),
            created_at: fauna_core::data::Timestamp(2_000_000),
        };
        match crate::routes::delete_post_core(
            &state,
            USER,
            &tombstone,
            deleted_id,
            crate::routes::RenderSite::Now,
        )
        .await
        {
            Ok(crate::routes::PostDeleteOutcome::Deleted) => {}
            Ok(crate::routes::PostDeleteOutcome::AlreadyGone) => panic!("post existed"),
            Err(_) => panic!("delete failed"),
        }

        let h = fetch_atproto_public_posts_handler();
        let req = FetchPublicPostsRequest {
            actor_id: ByteBuf::from(USER.to_vec()),
            cursor: None,
            limit: 10,
        };
        let reply: FetchPublicPostsReply =
            decode(&h(state.clone(), BRIDGE, enc(&req)).await.unwrap()).unwrap();
        assert_eq!(reply.items.len(), 2);
        assert_eq!(reply.next_cursor, None, "short page ⇒ exhausted");

        // Oldest-first: the surviving post (t=1.5s), then the delete (t=2s).
        assert_eq!(reply.items[0].kind, public_post_item_kind::POST);
        assert_eq!(reply.items[0].post_id, hex::encode(kept_id));
        assert_eq!(reply.items[0].created_at_micros, 1_500_000);
        assert_eq!(
            reply.items[0].deleted_post_id, None,
            "a post item carries no deleted_post_id"
        );
        assert_eq!(
            reply.items[0].payload.as_slice(),
            &kept_bytes[..],
            "stored post bytes served verbatim"
        );

        assert_eq!(reply.items[1].kind, public_post_item_kind::TOMBSTONE);
        assert_eq!(reply.items[1].created_at_micros, 2_000_000);
        // The nest pre-decodes the deleted post's digest so the bridge
        // never touches dag-cbor: it matches the deleted post's row id and
        // the payload's `Tombstone.post_id`.
        assert_eq!(
            reply.items[1].deleted_post_id.as_deref(),
            Some(hex::encode(deleted_id).as_str())
        );
        let decoded_ts: fauna_core::data::Tombstone =
            fauna_core::encoding::canonical_decode(&reply.items[1].payload).unwrap();
        assert_eq!(decoded_ts.author.0, USER);
        assert_eq!(decoded_ts.post_id, tombstone.post_id);
        assert_eq!(
            reply.items[1].deleted_post_id.as_deref(),
            Some(hex::encode(decoded_ts.post_id.digest()).as_str())
        );

        // Cursor resume: a full first page (limit 1) hands back a cursor;
        // resuming from it serves the tombstone next.
        let first = FetchPublicPostsRequest {
            actor_id: ByteBuf::from(USER.to_vec()),
            cursor: None,
            limit: 1,
        };
        let page1: FetchPublicPostsReply =
            decode(&h(state.clone(), BRIDGE, enc(&first)).await.unwrap()).unwrap();
        assert_eq!(page1.items.len(), 1);
        let cursor = page1.next_cursor.expect("full page carries a cursor");
        let resume = FetchPublicPostsRequest {
            actor_id: ByteBuf::from(USER.to_vec()),
            cursor: Some(cursor),
            limit: 10,
        };
        let page2: FetchPublicPostsReply =
            decode(&h(state.clone(), BRIDGE, enc(&resume)).await.unwrap()).unwrap();
        assert_eq!(page2.items.len(), 1);
        assert_eq!(page2.items[0].kind, public_post_item_kind::TOMBSTONE);
    }

    #[tokio::test]
    async fn bridge_fetches_profile_present_and_absent() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;

        let h = fetch_atproto_profile_handler();
        let req = FetchProfileRequest {
            actor_id: ByteBuf::from(USER.to_vec()),
        };
        let reply: FetchProfileReply =
            decode(&h(state.clone(), BRIDGE, enc(&req)).await.unwrap()).unwrap();
        assert_eq!(reply.profile, None, "no profile set yet");

        // Seed a stored profile row (the `fauna.profile.set` at-rest shape is
        // opaque bytes to this read — serve-verbatim is the contract).
        {
            let conn = state.db.conn().await;
            crate::db::content::insert_content(
                &conn,
                &[9u8; 32],
                "profile",
                &USER,
                7_000_000,
                b"profile-at-rest-bytes",
                None,
                "fauna",
                None,
            )
            .unwrap();
        }
        let reply: FetchProfileReply =
            decode(&h(state.clone(), BRIDGE, enc(&req)).await.unwrap()).unwrap();
        assert_eq!(
            reply.profile.as_deref().map(|b| &b[..]),
            Some(&b"profile-at-rest-bytes"[..])
        );
    }

    /// The consent gate at the kind boundary (`atproto-pds-bridge.md`
    /// § Projection & backfill). Forward-only is the ratified default, so a
    /// user's pre-existing public posts must not be served to the bridge at
    /// all — the whole point of enforcing the floor here rather than trusting
    /// the bridge's (re-derivable) cursor.
    #[tokio::test]
    async fn forward_only_identity_is_served_no_history() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        seed_public_post(&state, "written long before enabling", 1_000_000).await;
        // Declined history ⇒ the floor is *now*, far above the seeded post.
        enroll_atproto_identity(&state, false).await;

        let h = fetch_atproto_public_posts_handler();
        let req = FetchPublicPostsRequest {
            actor_id: ByteBuf::from(USER.to_vec()),
            cursor: None,
            limit: 10,
        };
        let reply: FetchPublicPostsReply =
            decode(&h(state.clone(), BRIDGE, enc(&req)).await.unwrap()).unwrap();
        assert!(
            reply.items.is_empty(),
            "a post predating the user's consent must never be served to the \
             bridge; got {} item(s)",
            reply.items.len()
        );
    }

    /// No identity row = no consent to publish anything, so the stream is
    /// empty rather than unbounded. Fails CLOSED: an open default here would
    /// serve a whole back-catalogue for any actor whose row is missing.
    #[tokio::test]
    async fn projection_page_for_an_unenrolled_actor_is_empty() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;
        seed_public_post(&state, "public post", 1_000_000).await;
        // Deliberately NO enroll_atproto_identity call.

        let h = fetch_atproto_public_posts_handler();
        let req = FetchPublicPostsRequest {
            actor_id: ByteBuf::from(USER.to_vec()),
            cursor: None,
            limit: 10,
        };
        let reply: FetchPublicPostsReply =
            decode(&h(state.clone(), BRIDGE, enc(&req)).await.unwrap()).unwrap();
        assert!(
            reply.items.is_empty(),
            "an actor with no ATProto identity has consented to nothing"
        );
    }

    #[tokio::test]
    async fn projection_reads_denied_for_user_class() {
        let state = fixture_state().await;
        seed_user_and_bridge(&state).await;

        let h = fetch_atproto_public_posts_handler();
        let req = FetchPublicPostsRequest {
            actor_id: ByteBuf::from(USER.to_vec()),
            cursor: None,
            limit: 10,
        };
        let err = h(state.clone(), USER, enc(&req)).await.expect_err("denied");
        assert_eq!(err.code, "fauna.bridges.permission_denied");

        let h = fetch_atproto_profile_handler();
        let req = FetchProfileRequest {
            actor_id: ByteBuf::from(USER.to_vec()),
        };
        let err = h(state.clone(), USER, enc(&req)).await.expect_err("denied");
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    /// The D10 authoring-delegation lifecycle over the real wire: mint on
    /// first fetch (idempotent), refuse a cert naming a foreign sub-key,
    /// store a well-formed one, destroy it on revoke, and re-mint fresh
    /// afterwards (the recreatable-state claim the migration comment makes).
    #[tokio::test]
    async fn authoring_delegation_mint_provision_revoke_lifecycle() {
        use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
        use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
        use fauna_core::identity::ActorKeypair;

        // The nest signing key is the root of the sub-key's at-rest KEK, and
        // the caller's actor id must be a REAL pubkey (the cert is verified
        // under it), so this test cannot use the flat `USER` const.
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        crate::test_support::seat_deployment_seed(&db, &[5u8; 32]).await;
        let mut st = AppState::for_test(db);
        st.nest_signing_key = Some(ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]));
        let state = Arc::new(st);
        let identity = ActorKeypair::from_secret([11u8; 32]);
        let user = identity.actor_id().0;
        state.db.create_user(&user, "free", "test").await.unwrap();
        state.db.set_handle(&user, "alice").await.unwrap();

        // 1. First fetch mints; the second returns the same key.
        let fetch = fetch_authoring_key_handler();
        let first = fetch(
            state.clone(),
            user,
            enc(&FetchAuthoringKeyRequest::default()),
        )
        .await
        .expect("fetch mints");
        let first: FetchAuthoringKeyReply = decode(&first).unwrap();
        assert_eq!(first.k_pub.len(), 32);
        let k_pub: [u8; 32] = first.k_pub.clone().try_into().unwrap();

        let again = fetch(
            state.clone(),
            user,
            enc(&FetchAuthoringKeyRequest::default()),
        )
        .await
        .expect("second fetch");
        let again: FetchAuthoringKeyReply = decode(&again).unwrap();
        assert_eq!(again.k_pub, first.k_pub, "mint is first-write-wins");

        let cert_for = |device_key: [u8; 32], caps: Vec<Capability>| -> Vec<u8> {
            let da = DeviceAuthorization {
                actor_id: identity.actor_id(),
                device_key,
                capabilities: caps,
                created_at: Timestamp(1),
                expires_at: None,
            };
            let (b, e) = sign_envelope(&identity, &da).unwrap();
            canonical_encode(&EmbedAsBytes::from_signed(b, e)).unwrap()
        };

        // 2. A cert over a FOREIGN device key is refused and stores nothing.
        let provision = provision_authoring_delegation_handler();
        let req = ProvisionAuthoringDelegationRequest {
            cert: cert_for([9u8; 32], vec![Capability::Post]),
            extra: Default::default(),
        };
        provision(state.clone(), user, enc(&req))
            .await
            .expect_err("a cert over a foreign device_key must be refused");
        assert!(
            state
                .db
                .get_atproto_authoring_key(&user)
                .await
                .unwrap()
                .unwrap()
                .cert
                .is_none(),
            "a refused provision must leave the row certless"
        );

        // 2b. A cert that EXPIRED LONG AGO is refused (check 5).
        //
        // `Timestamp` is MICROSECONDS since the epoch (`fauna-core` data.rs),
        // and the check must compare it against a microsecond clock. This
        // asserts it with a real past instant rather than the pure test's
        // synthetic `now` values, because the unit only becomes observable
        // where the handler reads the actual wall clock — a millisecond clock
        // here reads ~1000x too small, so every expiry lies in its "future"
        // and check 5 silently accepts every expired cert.
        let long_expired = {
            let now_micros = crate::db::now_epoch_millis().max(0) as u64 * 1_000;
            let da = DeviceAuthorization {
                actor_id: identity.actor_id(),
                device_key: k_pub,
                capabilities: vec![Capability::Post],
                created_at: Timestamp(now_micros - 7_200_000_000),
                expires_at: Some(Timestamp(now_micros - 3_600_000_000)), // an hour ago
            };
            let (b, e) = sign_envelope(&identity, &da).unwrap();
            canonical_encode(&EmbedAsBytes::from_signed(b, e)).unwrap()
        };
        provision(
            state.clone(),
            user,
            enc(&ProvisionAuthoringDelegationRequest {
                cert: long_expired,
                extra: Default::default(),
            }),
        )
        .await
        .expect_err("a cert that expired an hour ago must be refused");
        assert!(
            state
                .db
                .get_atproto_authoring_key(&user)
                .await
                .unwrap()
                .unwrap()
                .cert
                .is_none(),
            "a refused expired provision must leave the row certless"
        );

        // 3. The well-formed cert is accepted and stored verbatim.
        let good = cert_for(k_pub, vec![Capability::Post, Capability::UpdateProfile]);
        let req = ProvisionAuthoringDelegationRequest {
            cert: good.clone(),
            extra: Default::default(),
        };
        let reply = provision(state.clone(), user, enc(&req))
            .await
            .expect("provision");
        let reply: ProvisionAuthoringDelegationReply = decode(&reply).unwrap();
        assert!(reply.ok);
        assert_eq!(
            state
                .db
                .get_atproto_authoring_key(&user)
                .await
                .unwrap()
                .unwrap()
                .cert
                .as_deref(),
            Some(&good[..])
        );

        // 4. Revoke destroys the row; a second revoke is a no-op success.
        let revoke = revoke_authoring_delegation_handler();
        let reply = revoke(
            state.clone(),
            user,
            enc(&RevokeAuthoringDelegationRequest::default()),
        )
        .await
        .expect("revoke");
        let reply: RevokeAuthoringDelegationReply = decode(&reply).unwrap();
        assert!(reply.revoked);
        assert!(
            state
                .db
                .get_atproto_authoring_key(&user)
                .await
                .unwrap()
                .is_none()
        );
        let reply = revoke(
            state.clone(),
            user,
            enc(&RevokeAuthoringDelegationRequest::default()),
        )
        .await
        .expect("second revoke");
        let reply: RevokeAuthoringDelegationReply = decode(&reply).unwrap();
        assert!(
            !reply.revoked,
            "revoking an absent delegation is a no-op success"
        );

        // 5. A post-revoke fetch mints a FRESH sub-key — recreatable state.
        let after = fetch(
            state.clone(),
            user,
            enc(&FetchAuthoringKeyRequest::default()),
        )
        .await
        .expect("re-mint");
        let after: FetchAuthoringKeyReply = decode(&after).unwrap();
        assert_ne!(
            after.k_pub, first.k_pub,
            "re-enabling must mint a fresh sub-key, never resurrect the old one"
        );
    }

    /// The page-load status read. Its load-bearing property is what it does
    /// *not* do: reading the delegation must never mint a sub-key, or every
    /// user who merely opened the AT Protocol page would get an
    /// `atproto_authoring_keys` row.
    #[tokio::test]
    async fn fetch_authoring_delegation_reads_every_state_and_mints_nothing() {
        use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
        use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
        use fauna_core::identity::ActorKeypair;

        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        crate::test_support::seat_deployment_seed(&db, &[5u8; 32]).await;
        let mut st = AppState::for_test(db);
        st.nest_signing_key = Some(ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]));
        let state = Arc::new(st);
        let identity = ActorKeypair::from_secret([21u8; 32]);
        let user = identity.actor_id().0;
        state.db.create_user(&user, "free", "test").await.unwrap();
        state.db.set_handle(&user, "alice").await.unwrap();

        // A fresh handler per call: the factory is cheap, and one boxed
        // closure cannot be moved into an async block more than once.
        async fn read_now(state: Arc<AppState>, user: [u8; 32]) -> FetchAuthoringDelegationReply {
            let bytes = fetch_authoring_delegation_handler()(
                state,
                user,
                enc(&FetchAuthoringDelegationRequest::default()),
            )
            .await
            .expect("the status read never fails on a healthy account");
            decode::<FetchAuthoringDelegationReply>(&bytes).unwrap()
        }

        // 1. Never ran the ceremony: both absent, and — the point — no row was
        // created by asking.
        let empty = read_now(state.clone(), user).await;
        assert_eq!(empty.k_pub, None);
        assert_eq!(empty.cert, None);
        assert!(
            state
                .db
                .get_atproto_authoring_key(&user)
                .await
                .unwrap()
                .is_none(),
            "reading the delegation must not mint a sub-key"
        );

        // 2. After the ceremony's first step: `k_pub` present, cert absent —
        // the interrupted state, which authorizes nothing.
        let fetch = fetch_authoring_key_handler();
        let minted = fetch(
            state.clone(),
            user,
            enc(&FetchAuthoringKeyRequest::default()),
        )
        .await
        .expect("mint");
        let minted: FetchAuthoringKeyReply = decode(&minted).unwrap();
        let k_pub: [u8; 32] = minted.k_pub.clone().try_into().unwrap();

        let mid = read_now(state.clone(), user).await;
        assert_eq!(mid.k_pub.as_ref().map(|b| b.as_slice()), Some(&k_pub[..]));
        assert_eq!(mid.cert, None);

        // 3. Provisioned: the cert comes back BYTE-IDENTICAL to what was
        // uploaded, which is what lets the client re-verify the envelope under
        // its own identity key rather than trust the nest's account of it.
        let da = DeviceAuthorization {
            actor_id: identity.actor_id(),
            device_key: k_pub,
            capabilities: vec![Capability::Post, Capability::UpdateProfile],
            created_at: Timestamp(1),
            expires_at: None,
        };
        let (b, e) = sign_envelope(&identity, &da).unwrap();
        let cert = canonical_encode(&EmbedAsBytes::from_signed(b, e)).unwrap();
        provision_authoring_delegation_handler()(
            state.clone(),
            user,
            enc(&ProvisionAuthoringDelegationRequest {
                cert: cert.clone(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("provision");

        let full = read_now(state.clone(), user).await;
        assert_eq!(full.k_pub.as_ref().map(|b| b.as_slice()), Some(&k_pub[..]));
        assert_eq!(
            full.cert.as_ref().map(|b| b.as_slice()),
            Some(&cert[..]),
            "the cert must cross verbatim, not as a nest-side summary"
        );

        // 4. After revoke the whole row is gone — not a cert-less remnant.
        revoke_authoring_delegation_handler()(
            state.clone(),
            user,
            enc(&RevokeAuthoringDelegationRequest::default()),
        )
        .await
        .expect("revoke");
        let gone = read_now(state.clone(), user).await;
        assert_eq!(gone.k_pub, None);
        assert_eq!(gone.cert, None);
    }

    /// The deployment-seed rotation's hand-off window for `K`'s mint, driven
    /// causally the way `oauth_issuer_handlers::tests::rotation_window` drives
    /// the OAuth doors: the ceremony has committed while a serving generation
    /// not yet torn down still holds the retired seed (`box-recovery.md`
    /// § Deployment-seed rotation → *The bounded hand-off window*). `K` is
    /// per-account, so it has no boot moment: an account's first fetch mints
    /// it, and when that generation answers the fetch the row must still be
    /// sealed under the seed the database holds. A row sealed under the
    /// retired copy never opens again, and the satellite walk refuses it on
    /// every later rotation.
    #[tokio::test]
    async fn a_first_authoring_key_fetch_in_the_rotation_window_seals_under_the_successor_seed() {
        use crate::test_support::{
            every_row_opens_under, seat_deployment_seed, serving_generation,
        };
        use zeroize::Zeroizing;

        let (a, b, c) = (
            Zeroizing::new([0xa1u8; 32]),
            Zeroizing::new([0xb2u8; 32]),
            Zeroizing::new([0xc3u8; 32]),
        );
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        seat_deployment_seed(&db, &a).await;
        let user = fauna_core::identity::ActorKeypair::from_secret([11u8; 32])
            .actor_id()
            .0;
        db.create_user(&user, "free", "test").await.unwrap();
        db.set_handle(&user, "alice").await.unwrap();
        let outgoing = Arc::new(serving_generation(db.clone(), &a));

        db.rotate_deployment_seed(&a, &b)
            .await
            .expect("the ceremony runs")
            .expect("and commits");

        fetch_authoring_key_handler()(outgoing, user, enc(&FetchAuthoringKeyRequest::default()))
            .await
            .expect("the outgoing generation answers the first fetch");
        assert!(
            every_row_opens_under(
                &db,
                "atproto_authoring_keys",
                "k_secret_wrapped",
                crate::nest_kek::ATPROTO_AUTHORING_CONTEXT,
                &b,
            )
            .await,
            "a first fetch answered by the outgoing generation sealed K under the retired \
             deployment seed"
        );

        db.rotate_deployment_seed(&b, &c)
            .await
            .expect("the next rotation commits — K does not wedge the walk")
            .expect("and is no rule refusal");
    }

    // ── F2: ingest_external_write ───────────────────────────────

    #[cfg(feature = "bluesky")]
    mod external_write {
        use super::*;

        const DID: &str = "did:plc:alicetestidentity00000";

        /// A user with an approved bridge, a minted DID, and the kill-switch
        /// left at its default ON.
        async fn seeded() -> Arc<AppState> {
            let state = fixture_state().await;
            seed_user_and_bridge(&state).await;
            state
                .db
                .upsert_atproto_identity_intent(&USER, "plc", "did:key:zRotation")
                .await
                .unwrap();
            state
                .db
                .record_atproto_minted(&USER, DID, None)
                .await
                .unwrap();
            state
        }

        fn write(collection: &str, action: &str, rkey: Option<&str>) -> ExternalWrite {
            ExternalWrite {
                collection: collection.into(),
                action: action.into(),
                rkey: rkey.map(str::to_string),
                record: Some(ByteBuf::from(b"dag-cbor-bytes".to_vec())),
                cid: Some("bafyfixturecid".into()),
                ..Default::default()
            }
        }

        async fn ingest(
            state: &Arc<AppState>,
            writes: Vec<ExternalWrite>,
        ) -> Result<IngestExternalWriteReply, RpcError> {
            let req = IngestExternalWriteRequest {
                actor_id: USER.to_vec(),
                writes,
                extra: Default::default(),
            };
            let bytes = ingest_external_write_handler()(state.clone(), BRIDGE, enc(&req)).await?;
            Ok(decode(&bytes).unwrap())
        }

        /// The journal arm end to end: an unmappable lexicon lands verbatim,
        /// is addressable by its AT-URI, and reads back through the same
        /// helpers the bridge materializes the MST from.
        #[tokio::test]
        async fn an_unmappable_lexicon_journals_verbatim_and_reads_back() {
            let state = seeded().await;
            let reply = ingest(
                &state,
                vec![write("app.bsky.graph.list", "create", Some("3listrkey"))],
            )
            .await
            .expect("ingest");

            assert_eq!(reply.results.len(), 1);
            let r = &reply.results[0];
            assert!(r.refusal.is_none(), "journal write must not refuse: {r:?}");
            assert_eq!(r.rkey.as_deref(), Some("3listrkey"));
            assert_eq!(
                r.at_uri.as_deref(),
                Some(format!("at://{DID}/app.bsky.graph.list/3listrkey").as_str())
            );
            assert_eq!(
                r.fauna_post_id, None,
                "a journaled record has no Fauna post identity, so the bridge \
                 must get no post_map key to write a bogus row with"
            );

            let row = state
                .db
                .get_atproto_native_record(&USER, "app.bsky.graph.list", "3listrkey")
                .await
                .unwrap()
                .expect("journaled");
            assert_eq!(
                row.record, b"dag-cbor-bytes",
                "the journal stores the bridge's bytes verbatim — re-derivability depends on it"
            );
            assert_eq!(row.cid, "bafyfixturecid");
        }

        /// D2's moved row, proven at the handler: a like is journaled, not
        /// round-trip-refused — the Fauna concept does not exist, and
        /// journaling still lands the record in the repo.
        #[tokio::test]
        async fn an_interaction_record_journals_rather_than_refusing() {
            let state = seeded().await;
            let reply = ingest(
                &state,
                vec![write("app.bsky.feed.like", "create", Some("3likerkey"))],
            )
            .await
            .expect("ingest");
            assert!(
                reply.results[0].refusal.is_none(),
                "a like must journal, got {:?}",
                reply.results[0].refusal
            );
            assert!(
                state
                    .db
                    .get_atproto_native_record(&USER, "app.bsky.feed.like", "3likerkey")
                    .await
                    .unwrap()
                    .is_some()
            );
        }

        #[tokio::test]
        async fn a_delete_tombstones_and_a_second_delete_is_still_a_success() {
            let state = seeded().await;
            ingest(
                &state,
                vec![write("app.bsky.graph.list", "create", Some("3gone"))],
            )
            .await
            .unwrap();

            for round in 0..2 {
                let reply = ingest(
                    &state,
                    vec![ExternalWrite {
                        record: None,
                        cid: None,
                        ..write("app.bsky.graph.list", "delete", Some("3gone"))
                    }],
                )
                .await
                .expect("delete");
                assert!(
                    reply.results[0].refusal.is_none(),
                    "delete round {round} must succeed"
                );
            }
            // The row is KEPT with `deleted_at` set — never `DELETE`d. The
            // repo's re-derivability needs the tombstone, so `get` still sees
            // it (that is how the op path tells "deleted" from "never
            // existed") while `list` — what the bridge materializes the MST
            // from — no longer does.
            let row = state
                .db
                .get_atproto_native_record(&USER, "app.bsky.graph.list", "3gone")
                .await
                .unwrap()
                .expect("the tombstone row survives; re-derivability needs it");
            assert!(
                row.deleted_at.is_some(),
                "the row must be tombstoned, not live"
            );
            assert!(
                state
                    .db
                    .list_atproto_native_records(&USER, "app.bsky.graph.list")
                    .await
                    .unwrap()
                    .is_empty(),
                "a tombstoned record is no longer materialized into the MST"
            );
        }

        /// Without a D10 delegation the round-trip arm gives its *designed
        /// permanent* answer — fauna-surface, naming where to authorize.
        /// Slice 3 must not change this branch.
        #[tokio::test]
        async fn a_post_without_a_delegation_refuses_fauna_surface() {
            let state = seeded().await;
            let reply = ingest(&state, vec![write("app.bsky.feed.post", "create", None)])
                .await
                .expect("ingest");

            let refusal = reply.results[0].refusal.as_ref().expect("refused");
            assert_eq!(refusal.sub_type, "fauna_surface");
            assert!(
                refusal.message.contains("Fauna app"),
                "the refusal must name where to authorize, got: {}",
                refusal.message
            );
            assert!(reply.results[0].rkey.is_none());
        }

        // ── F2.2 slice 3: the delegated round-trip arm ──────────────

        const POST_TEXT: &str = "hello from an external app";
        /// 2026-03-20T12:00:00.000000Z — the instant the record asserts.
        const RECORD_MICROS: u64 = 1_774_008_000_000_000;

        /// Encode a record the way the Go bridge does before the nest sees it
        /// (indigo's `JSONRecordToDagCBOR`). No fixture here carries a CID
        /// link, so plain JSON→dag-cbor is the exact byte shape.
        fn record_bytes(v: serde_json::Value) -> ByteBuf {
            ByteBuf::from(serde_ipld_dagcbor::to_vec(&v).expect("fixture encodes"))
        }

        fn post_record(extra: serde_json::Value) -> ByteBuf {
            let mut base = serde_json::json!({
                "$type": "app.bsky.feed.post",
                "text": POST_TEXT,
                "createdAt": "2026-03-20T12:00:00.000000Z",
            });
            let (serde_json::Value::Object(base_map), serde_json::Value::Object(extra_map)) =
                (&mut base, extra)
            else {
                panic!("both must be objects")
            };
            base_map.extend(extra_map);
            record_bytes(base)
        }

        /// A post write carrying `record`; a round-trip create sends no rkey
        /// (the nest derives it).
        fn post_write(action: &str, record: Option<ByteBuf>) -> ExternalWrite {
            ExternalWrite {
                collection: "app.bsky.feed.post".into(),
                action: action.into(),
                rkey: if action == "create" {
                    None
                } else {
                    Some("3existing".into())
                },
                record,
                cid: Some("bafyfixturecid".into()),
                ..Default::default()
            }
        }

        /// A seeded account that has completed the D10 mint ceremony: a real
        /// keypair identity (the cert is verified *under* the caller's actor
        /// id, so a flat const cannot work), a nest signing key (the root of
        /// K's at-rest KEK), an approved bridge, a minted DID, and a
        /// provisioned `Post`+`UpdateProfile` delegation.
        ///
        /// Returns the state, the account's actor id, and `K_pub`.
        async fn seeded_with_delegation() -> (Arc<AppState>, [u8; 32], [u8; 32]) {
            // The full authoring set — what the real client ceremony mints
            // (`AUTHORING_CAPABILITIES`).
            seeded_with_delegation_caps(vec![
                fauna_core::data::Capability::Post,
                fauna_core::data::Capability::UpdateProfile,
            ])
            .await
        }

        /// `seeded_with_delegation` with a caller-chosen capability set —
        /// provisioning accepts any subset of the enumerated authoring set, so
        /// a narrower cert (a non-Fauna client minting its own) is a
        /// legitimate stored state the dispatch pre-check must handle.
        async fn seeded_with_delegation_caps(
            caps: Vec<fauna_core::data::Capability>,
        ) -> (Arc<AppState>, [u8; 32], [u8; 32]) {
            use fauna_core::data::{DeviceAuthorization, Timestamp};
            use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
            use fauna_core::identity::ActorKeypair;

            let db = Arc::new(CacheDb::open_in_memory().unwrap());
            crate::test_support::seat_deployment_seed(&db, &[5u8; 32]).await;
            let mut st = AppState::for_test(db);
            st.nest_signing_key = Some(ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]));
            let state = Arc::new(st);

            let identity = ActorKeypair::from_secret([11u8; 32]);
            let user = identity.actor_id().0;
            state.db.create_user(&user, "free", "test").await.unwrap();
            state.db.set_handle(&user, "alice").await.unwrap();
            state
                .db
                .create_pending_bridge_service_user(&BRIDGE, BridgeRole::AtprotoPds, "atproto-1")
                .await
                .unwrap();
            state
                .db
                .upsert_bridge_x25519(&BRIDGE, &[88u8; 32])
                .await
                .unwrap();
            state
                .db
                .approve_bridge_service_user(&BRIDGE, None)
                .await
                .unwrap();
            state
                .db
                .upsert_atproto_identity_intent(&user, "plc", "did:key:zRotation")
                .await
                .unwrap();
            state
                .db
                .record_atproto_minted(&user, DID, None)
                .await
                .unwrap();

            // Mint K through the real kind, then provision the cert over it.
            let fetched = fetch_authoring_key_handler()(
                state.clone(),
                user,
                enc(&FetchAuthoringKeyRequest::default()),
            )
            .await
            .expect("mint");
            let fetched: FetchAuthoringKeyReply = decode(&fetched).unwrap();
            let k_pub: [u8; 32] = fetched.k_pub.try_into().unwrap();

            let da = DeviceAuthorization {
                actor_id: identity.actor_id(),
                device_key: k_pub,
                capabilities: caps,
                created_at: Timestamp(1),
                expires_at: None,
            };
            let (b, e) = sign_envelope(&identity, &da).unwrap();
            let cert = canonical_encode(&EmbedAsBytes::from_signed(b, e)).unwrap();
            provision_authoring_delegation_handler()(
                state.clone(),
                user,
                enc(&ProvisionAuthoringDelegationRequest {
                    cert,
                    extra: Default::default(),
                }),
            )
            .await
            .expect("provision");

            (state, user, k_pub)
        }

        async fn ingest_as(
            state: &Arc<AppState>,
            actor: [u8; 32],
            writes: Vec<ExternalWrite>,
        ) -> Result<IngestExternalWriteReply, RpcError> {
            let req = IngestExternalWriteRequest {
                actor_id: actor.to_vec(),
                writes,
                extra: Default::default(),
            };
            let bytes = ingest_external_write_handler()(state.clone(), BRIDGE, enc(&req)).await?;
            Ok(decode(&bytes).unwrap())
        }

        /// **The headline.** An external-app post create becomes a real,
        /// stored, delegated-signed Fauna post: it survives
        /// `classify_encrypted_post` (the ingest gate runs the D10 chain on
        /// the real bytes — reaching storage at all *is* that proof), it is
        /// signed by `K` rather than the identity key, and the one read face
        /// every app shares decodes it as valid.
        #[tokio::test]
        async fn a_delegated_post_round_trips_and_verifies_through_the_shared_read_face() {
            use fauna_core::data::Capability;
            use fauna_core::encoding::{
                AuthoringOrigin, EmbedAsBytes, canonical_decode, decode_signed_bytes,
                verify_authoring_envelope,
            };

            let (state, user, k_pub) = seeded_with_delegation().await;
            let reply = ingest_as(
                &state,
                user,
                vec![post_write(
                    "create",
                    Some(post_record(serde_json::json!({}))),
                )],
            )
            .await
            .expect("ingest");

            let r = &reply.results[0];
            assert!(
                r.refusal.is_none(),
                "the round-trip arm must publish, not refuse: {:?}",
                r.refusal
            );
            let rkey = r
                .rkey
                .as_deref()
                .expect("a published post reports its rkey");
            assert_eq!(
                r.at_uri.as_deref(),
                Some(format!("at://{DID}/app.bsky.feed.post/{rkey}").as_str())
            );

            // The post really landed, and the bytes are the ones the ingest
            // gate accepted.
            let rows = state
                .db
                .list_public_projection_page(&user, None, 16, NO_PROJECTION_FLOOR)
                .await
                .expect("list the public projection");
            assert_eq!(rows.len(), 1, "exactly one post landed");

            // The reply names the Fauna post the write became. The bridge keys
            // its `post_map` row on this, and that row is what stops the
            // projection loop re-projecting this very post over the caller's
            // own record bytes.
            assert_eq!(
                r.fauna_post_id.as_deref(),
                Some(hex::encode(rows[0].id).as_str()),
                "a round-tripped post must report the Fauna post id it became"
            );
            let stored =
                crate::segments::post::load_post_body(&state.post_segments, &state.db, &rows[0].id)
                    .await
                    .unwrap()
                    .expect("the post body is resolvable");

            // Signed by K, not by the identity key — and the cert rides along.
            let wire: EmbedAsBytes = canonical_decode(&stored).unwrap();
            let signer_auth = wire.signer_auth.clone();
            assert!(
                signer_auth.is_some(),
                "the delegation cert must travel with the post"
            );
            let (bytes, env) = wire.into_signed().unwrap();
            let post: fauna_core::data::Post = decode_signed_bytes(&bytes).unwrap();
            assert_eq!(post.author.0, user, "the post is the user's own");
            assert_eq!(post.body_text(), POST_TEXT);
            assert_ne!(
                post.author.0, k_pub,
                "the author is the account, never the sub-key"
            );
            let origin = verify_authoring_envelope(
                &post,
                &bytes,
                &env,
                signer_auth.as_deref(),
                &Capability::Post,
                post.created_at,
            )
            .expect("the chain verifies");
            match origin {
                // The signer is the account's own minted sub-key — not the
                // identity key (no server-side actor can hold that), and not
                // some other delegate.
                AuthoringOrigin::Delegated { device_key } => assert_eq!(device_key, k_pub),
                AuthoringOrigin::Direct => {
                    panic!("a Direct verdict would mean the identity key signed it")
                }
            }

            // The one read face wasm + FFI expose to all 7 apps accepts it —
            // AND reports it as delegated. Accepting it is not enough: the D10
            // audit surface (`atproto-pds-full.md` § D10 → *Audit*) is precisely
            // the promise that a user's own app can tell, from the stored bytes
            // alone, which posts an external app wrote as them. A read face that
            // answered a bare `true` here would satisfy every assertion above
            // while leaving that promise unkeepable.
            let (decoded, read_origin) =
                fauna_client_core::post::decode_post(&stored).expect("decode_post");
            assert_eq!(
                read_origin,
                Some(AuthoringOrigin::Delegated { device_key: k_pub }),
                "every app's read face must accept a delegated post AND name \
                 the sub-key that signed it"
            );
            assert_eq!(decoded.body_text(), POST_TEXT);

            // The ADVISORY companion (D10 § Audit): a batch that actually
            // applied stamps the delegation row's `last_used_at`, so the
            // AT Protocol page can hint that this delegation is in use. Asserted
            // here rather than in its own test because the stamp's whole
            // contract is "an external app authored" — which is exactly what
            // this journey establishes and a synthetic DB test cannot.
            let row = state
                .db
                .get_atproto_authoring_key(&user)
                .await
                .expect("read the delegation row")
                .expect("the delegation exists");
            assert!(
                row.last_used_at.is_some(),
                "a delegated write that APPLIED must stamp the advisory last_used_at"
            );
        }

        /// The stamp's negative half, and the reason it sits after the apply
        /// loop rather than at signer load: a batch the nest REFUSED authored
        /// nothing, so it must leave `last_used_at` alone. Stamping on attempt
        /// would make the row report use for calls that never wrote anything —
        /// turning an already-advisory hint into an actively misleading one.
        #[tokio::test]
        async fn a_refused_batch_does_not_stamp_the_advisory_last_used_at() {
            let (state, user, _k_pub) = seeded_with_delegation().await;

            // An unparseable post record: refused by the pre-flight, so the
            // all-or-nothing gate returns before anything is applied.
            let reply = ingest_as(
                &state,
                user,
                vec![post_write(
                    "create",
                    Some(ByteBuf::from(b"not a record".to_vec())),
                )],
            )
            .await
            .expect("ingest answers");
            assert!(
                reply.results[0].refusal.is_some(),
                "this batch must refuse for the test to mean anything"
            );

            let row = state
                .db
                .get_atproto_authoring_key(&user)
                .await
                .expect("read the delegation row")
                .expect("the delegation exists");
            assert_eq!(
                row.last_used_at, None,
                "a refused batch authored nothing and must not report use"
            );
        }

        /// Build a delegation cert over `k_pub` with a chosen `expires_at`.
        /// Written straight onto the row by the caller rather than provisioned
        /// through the kind, on purpose: `provision_authoring_delegation`
        /// refuses an already-past expiry (check 5), so the state this finding
        /// is about — **minted while valid, lapsed since** — is unreachable
        /// through the handler. That is the ordinary production state, not a
        /// synthetic one.
        fn cert_expiring_at(
            identity: &fauna_core::identity::ActorKeypair,
            k_pub: [u8; 32],
            expires_at: Option<u64>,
        ) -> Vec<u8> {
            use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
            use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};

            let da = DeviceAuthorization {
                actor_id: identity.actor_id(),
                device_key: k_pub,
                capabilities: vec![Capability::Post, Capability::UpdateProfile],
                created_at: Timestamp(1),
                expires_at: expires_at.map(Timestamp),
            };
            let (b, e) = sign_envelope(identity, &da).unwrap();
            canonical_encode(&EmbedAsBytes::from_signed(b, e)).unwrap()
        }

        /// **a delegation must expire against the WALL CLOCK,
        /// never against the record's own `createdAt`.**
        ///
        /// The only expiry gate anywhere on the published-post path is
        /// `verify_signed_or_delegated` step 5, which compares
        /// `cert.expires_at` against *the value's own* `created_at`
        /// (`fauna-core/src/encoding.rs`). That comparison is correct where it
        /// lives — re-verifying already-published content, where a post
        /// authorized at creation stays authorized forever
        /// (`atproto-pds-full.md` D10 § Revocation) — and is useless as an
        /// authorization gate *here*, because the round-trip create arm adopts
        /// `created_at` verbatim from the external app's record (slice 3's
        /// ruling, deliberate: the rkey is derived from it). So the value the
        /// expiry is measured against is chosen by the very party the expiry
        /// constrains, and an app whose delegation lapsed keeps authoring
        /// indefinitely by back-dating `createdAt` — silent history insertion,
        /// needing neither key nor nest compromise, and so *not* the residual
        /// D10 § Revocation accepts.
        ///
        /// The control arm is what makes this non-vacuous: the same cert,
        /// written the same way, differing only in `expires_at`, must still
        /// author. Without it a merely-malformed fixture would pass for the
        /// wrong reason.
        ///
        /// ⚠ MICROSECONDS. `Timestamp` is micros; a millisecond clock reads
        /// ~1000x too small and puts every real expiry in its own "future" —
        /// exactly how the provision-time check sat dead until it was fixed.
        #[tokio::test]
        async fn a_lapsed_delegation_refuses_even_when_the_record_backdates_its_created_at() {
            use fauna_core::data::Timestamp;
            use fauna_core::identity::ActorKeypair;

            // The cert is verified under the caller's own actor id, so this
            // must be `seeded_with_delegation`'s identity, reconstructed.
            let identity = ActorKeypair::from_secret([11u8; 32]);
            let now = Timestamp::now().0;
            let an_hour_ago = now - 3_600_000_000;
            let an_hour_hence = now + 3_600_000_000;

            // The attack's precondition, asserted rather than assumed: the
            // record's own `createdAt` really does predate the expiry, so
            // step 5 passes and only a wall-clock gate can refuse this.
            assert!(
                RECORD_MICROS < an_hour_ago,
                "the fixture record must back-date to before the expiry for \
                 this to exercise the finding at all"
            );

            // Control: a cert written by this exact route, expiring in the
            // future, still authors.
            let (state, user, k_pub) = seeded_with_delegation().await;
            state
                .db
                .set_atproto_authoring_key_cert(
                    &user,
                    &cert_expiring_at(&identity, k_pub, Some(an_hour_hence)),
                )
                .await
                .unwrap();
            let reply = ingest_as(
                &state,
                user,
                vec![post_write(
                    "create",
                    Some(post_record(serde_json::json!({}))),
                )],
            )
            .await
            .expect("ingest");
            assert!(
                reply.results[0].refusal.is_none(),
                "an unexpired delegation must still author, else this test's \
                 refusal arm proves nothing: {:?}",
                reply.results[0].refusal
            );

            // The finding: the same cert, lapsed an hour ago.
            let (state, user, k_pub) = seeded_with_delegation().await;
            state
                .db
                .set_atproto_authoring_key_cert(
                    &user,
                    &cert_expiring_at(&identity, k_pub, Some(an_hour_ago)),
                )
                .await
                .unwrap();
            let reply = ingest_as(
                &state,
                user,
                vec![post_write(
                    "create",
                    Some(post_record(serde_json::json!({}))),
                )],
            )
            .await
            .expect("ingest");

            let refusal = reply.results[0]
                .refusal
                .as_ref()
                .expect("a lapsed delegation must refuse the write");
            assert_eq!(
                refusal.sub_type, "fauna_surface",
                "a lapsed delegation is the same user-visible state as one \
                 never provisioned — the refusal names where to re-authorize"
            );
            assert!(reply.results[0].rkey.is_none(), "nothing may be authored");

            // And nothing reached storage under the account's identity.
            assert!(
                state
                    .db
                    .list_atproto_native_records(&user, "app.bsky.feed.post")
                    .await
                    .unwrap()
                    .is_empty(),
                "a refused write must leave the repo untouched"
            );
        }

        /// The ruling this slice owns: the Fauna post **adopts the record's
        /// own `createdAt`**, which is what makes the answered rkey the one
        /// the projection will later derive. Pinning both halves together is
        /// the point — the rkey is a pure function of that instant, so a
        /// nest-stamped `created_at` would answer an AT-URI the record never
        /// lands at (read-your-writes broken).
        #[tokio::test]
        async fn the_post_adopts_the_records_created_at_so_the_answered_rkey_is_the_real_one() {
            let (state, user, _) = seeded_with_delegation().await;
            let reply = ingest_as(
                &state,
                user,
                vec![post_write(
                    "create",
                    Some(post_record(serde_json::json!({}))),
                )],
            )
            .await
            .expect("ingest");

            let rows = state
                .db
                .list_public_projection_page(&user, None, 16, NO_PROJECTION_FLOOR)
                .await
                .unwrap();
            assert_eq!(
                rows[0].created_at as u64, RECORD_MICROS,
                "the stored post carries the record's asserted instant, not the nest's clock"
            );
            // The rkey the projection will emit, derived exactly as
            // `projector.go` does via `DeterministicTID`.
            let expected = fauna_bridge_atproto::outbound::deterministic_tid(
                RECORD_MICROS as i64,
                &rows[0].id,
            );
            assert_eq!(
                reply.results[0].rkey.as_deref(),
                Some(expected.as_str()),
                "the answered rkey must equal the projection's own derivation"
            );
        }

        /// `strongRef.cid` is a STRING (`format: cid`), not a dag-cbor tag-42
        /// link — but it is still *parsed* as a CID, so a fixture needs a
        /// genuinely valid one (CIDv1, raw, sha2-256). An invented base32
        /// literal fails with "Failed to parse multihash" and the write would
        /// refuse `policy` for a reason that has nothing to do with the test.
        const FIXTURE_CID: &str = "bafkreiaha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4";

        fn strong_ref(uri: &str) -> serde_json::Value {
            serde_json::json!({ "uri": uri, "cid": FIXTURE_CID })
        }

        /// Publish one top-level post and return `(its AT-URI, its Fauna post
        /// id hex)` — the real pair a bridge would hold in `post_map`, so a
        /// following reply/quote/delete resolves against something that
        /// genuinely exists rather than a hand-made id.
        async fn publish_one(state: &Arc<AppState>, user: [u8; 32]) -> (String, String) {
            let reply = ingest_as(
                state,
                user,
                vec![post_write(
                    "create",
                    Some(post_record(serde_json::json!({}))),
                )],
            )
            .await
            .expect("ingest the target post");
            let r = &reply.results[0];
            assert!(r.refusal.is_none(), "the target post must publish");
            (
                r.at_uri.clone().expect("target at_uri"),
                r.fauna_post_id.clone().expect("target fauna_post_id"),
            )
        }

        /// A reply whose parent IS a Fauna post lands as a real Fauna reply —
        /// a `Reference::Reply` to that exact post, not a top-level post.
        #[tokio::test]
        async fn a_reply_to_a_fauna_post_lands_as_a_real_reply() {
            use fauna_core::encoding::{EmbedAsBytes, canonical_decode, decode_signed_bytes};
            let (state, user, _) = seeded_with_delegation().await;
            let (parent_uri, parent_id) = publish_one(&state, user).await;

            let mut write = post_write(
                "create",
                Some(post_record(serde_json::json!({
                    "reply": {
                        "root": strong_ref(&parent_uri),
                        "parent": strong_ref(&parent_uri),
                    },
                }))),
            );
            write
                .resolved_targets
                .insert(parent_uri.clone(), parent_id.clone());

            let reply = ingest_as(&state, user, vec![write]).await.expect("ingest");
            let r = &reply.results[0];
            assert!(
                r.refusal.is_none(),
                "a reply to a Fauna post must publish: {:?}",
                r.refusal
            );

            let rows = state
                .db
                .list_public_projection_page(&user, None, 16, NO_PROJECTION_FLOOR)
                .await
                .unwrap();
            assert_eq!(rows.len(), 2, "the parent and the reply both landed");

            let reply_id = r.fauna_post_id.as_deref().expect("the reply's post id");
            let reply_row = rows
                .iter()
                .find(|row| hex::encode(row.id) == reply_id)
                .expect("the reply is in the projection");
            let stored = crate::segments::post::load_post_body(
                &state.post_segments,
                &state.db,
                &reply_row.id,
            )
            .await
            .unwrap()
            .expect("the reply body is resolvable");
            let wire: EmbedAsBytes = canonical_decode(&stored).unwrap();
            let (bytes, _) = wire.into_signed().unwrap();
            let post: fauna_core::data::Post = decode_signed_bytes(&bytes).unwrap();

            let mut parent_digest = [0u8; 32];
            hex::decode_to_slice(&parent_id, &mut parent_digest).unwrap();
            assert_eq!(
                post.references,
                vec![fauna_core::data::Reference::Reply {
                    post_id: fauna_cbor::Cid::from_digest_dag_cbor(parent_digest),
                }],
                "the reply must reference the parent it named, and nothing else"
            );
        }

        /// A quote resolves the same way, into `Reference::Quote`.
        #[tokio::test]
        async fn a_quote_of_a_fauna_post_lands_as_a_real_quote() {
            use fauna_core::encoding::{EmbedAsBytes, canonical_decode, decode_signed_bytes};
            let (state, user, _) = seeded_with_delegation().await;
            let (quoted_uri, quoted_id) = publish_one(&state, user).await;

            let mut write = post_write(
                "create",
                Some(post_record(serde_json::json!({
                    "embed": {
                        "$type": "app.bsky.embed.record",
                        "record": strong_ref(&quoted_uri),
                    },
                }))),
            );
            write
                .resolved_targets
                .insert(quoted_uri.clone(), quoted_id.clone());

            let reply = ingest_as(&state, user, vec![write]).await.expect("ingest");
            let r = &reply.results[0];
            assert!(r.refusal.is_none(), "a quote must publish: {:?}", r.refusal);

            let rows = state
                .db
                .list_public_projection_page(&user, None, 16, NO_PROJECTION_FLOOR)
                .await
                .unwrap();
            let quote_id = r.fauna_post_id.as_deref().expect("the quote's post id");
            let quote_row = rows
                .iter()
                .find(|row| hex::encode(row.id) == quote_id)
                .expect("the quote is in the projection");
            let stored = crate::segments::post::load_post_body(
                &state.post_segments,
                &state.db,
                &quote_row.id,
            )
            .await
            .unwrap()
            .unwrap();
            let wire: EmbedAsBytes = canonical_decode(&stored).unwrap();
            let (bytes, _) = wire.into_signed().unwrap();
            let post: fauna_core::data::Post = decode_signed_bytes(&bytes).unwrap();

            let mut quoted_digest = [0u8; 32];
            hex::decode_to_slice(&quoted_id, &mut quoted_digest).unwrap();
            assert_eq!(
                post.references,
                vec![fauna_core::data::Reference::Quote {
                    post_id: fauna_cbor::Cid::from_digest_dag_cbor(quoted_digest),
                }]
            );
        }

        /// **The ratified fallback** (`atproto-pds-full.md` § F2 detail): a
        /// reply whose parent is not a Fauna post JOURNALS. It must neither
        /// refuse (the record only reaches the repo after this call answers, so
        /// refusing leaves the user's reply existing nowhere) nor publish
        /// standalone (that would put the user's words in front of their Fauna
        /// followers stripped of the conversation they wrote them in).
        #[tokio::test]
        async fn a_reply_to_a_non_fauna_post_journals_rather_than_refusing_or_flattening() {
            let (state, user, _) = seeded_with_delegation().await;

            // A parent out on the wider network: nothing resolves it, so the
            // write carries no `resolved_targets` entry for it.
            let foreign = "at://did:plc:someoneelse/app.bsky.feed.post/3foreign";
            let mut write = post_write(
                "create",
                Some(post_record(serde_json::json!({
                    "reply": { "root": strong_ref(foreign), "parent": strong_ref(foreign) },
                }))),
            );
            // A journal write addresses a specific record, so the bridge sends
            // its candidate rkey (it always does on a post create).
            write.rkey = Some("3journaled".into());

            let reply = ingest_as(&state, user, vec![write]).await.expect("ingest");
            let r = &reply.results[0];
            assert!(
                r.refusal.is_none(),
                "an unresolvable reply must journal, not refuse: {:?}",
                r.refusal
            );
            assert!(
                r.fauna_post_id.is_none(),
                "a journaled record has no Fauna identity"
            );

            assert!(
                state
                    .db
                    .list_public_projection_page(&user, None, 16, NO_PROJECTION_FLOOR)
                    .await
                    .unwrap()
                    .is_empty(),
                "journaling must publish NO Fauna post — least of all a flattened top-level one"
            );
            assert!(
                state
                    .db
                    .get_atproto_native_record(&user, "app.bsky.feed.post", "3journaled")
                    .await
                    .unwrap()
                    .is_some(),
                "the record itself must land verbatim in the journal"
            );
        }

        /// A quote takes the same rule rather than a second one — one uniform
        /// fallback, per the ruling.
        #[tokio::test]
        async fn a_quote_of_a_non_fauna_post_journals_too() {
            let (state, user, _) = seeded_with_delegation().await;
            let foreign = "at://did:plc:someoneelse/app.bsky.feed.post/3foreignquote";
            let mut write = post_write(
                "create",
                Some(post_record(serde_json::json!({
                    "embed": {
                        "$type": "app.bsky.embed.record",
                        "record": strong_ref(foreign),
                    },
                }))),
            );
            write.rkey = Some("3journaledquote".into());

            let reply = ingest_as(&state, user, vec![write]).await.expect("ingest");
            assert!(reply.results[0].refusal.is_none(), "must journal");
            assert!(
                state
                    .db
                    .list_public_projection_page(&user, None, 16, NO_PROJECTION_FLOOR)
                    .await
                    .unwrap()
                    .is_empty(),
                "a quote missing its target must not publish a bare post"
            );
        }

        /// A `deleteRecord` of a round-tripped post tombstones the Fauna post.
        #[tokio::test]
        async fn a_post_delete_tombstones_the_fauna_post() {
            let (state, user, _) = seeded_with_delegation().await;
            let (at_uri, post_id) = publish_one(&state, user).await;

            // The bridge addresses the record by its own rkey and resolves that
            // record's AT-URI against `post_map`.
            let rkey = at_uri.rsplit('/').next().expect("rkey").to_string();
            let mut write = post_write("delete", None);
            write.rkey = Some(rkey);
            write
                .resolved_targets
                .insert(at_uri.clone(), post_id.clone());

            let reply = ingest_as(&state, user, vec![write]).await.expect("ingest");
            let r = &reply.results[0];
            assert!(
                r.refusal.is_none(),
                "deleting a round-tripped post must succeed: {:?}",
                r.refusal
            );
            assert_eq!(
                r.fauna_post_id.as_deref(),
                Some(post_id.as_str()),
                "the reply names the post so the bridge drops its post_map row"
            );

            // The live post is gone and an ATProto delete *witness* took its
            // place. Both halves matter: the first is the deletion, the second
            // is what lets the projection loop retract the record on the
            // firehose — a delete the bridge could never learn about would
            // leave the post live on the network forever.
            let rows = state
                .db
                .list_public_projection_page(&user, None, 16, NO_PROJECTION_FLOOR)
                .await
                .unwrap();
            assert!(
                !rows.iter().any(|row| !row.is_tombstone),
                "no live post may remain in the projection after a delete"
            );
            assert_eq!(
                rows.iter().filter(|row| row.is_tombstone).count(),
                1,
                "the delete must leave exactly one ATProto retraction witness"
            );
        }

        /// A delete of a record that maps to no Fauna post journals (the
        /// ordinary case for the non-Fauna half of a repo), and a profile
        /// write still refuses **deferred** (F2.3) — never `policy`, since the
        /// sub-type is what tells a later session "build this" rather than
        /// "this is the answer".
        #[tokio::test]
        async fn an_unmapped_delete_journals_while_the_profile_write_beside_it_round_trips() {
            let (state, user, _) = seeded_with_delegation().await;
            let reply = ingest_as(
                &state,
                user,
                vec![
                    post_write("delete", None),
                    ExternalWrite {
                        collection: "app.bsky.actor.profile".into(),
                        action: "update".into(),
                        rkey: Some("self".into()),
                        record: Some(record_bytes(serde_json::json!({
                            "$type": "app.bsky.actor.profile",
                            "displayName": "Alice",
                        }))),
                        cid: Some("bafyfixturecid".into()),
                        ..Default::default()
                    },
                ],
            )
            .await
            .expect("ingest");

            assert!(
                reply.results[0].refusal.is_none(),
                "an unmapped delete journals rather than refusing: {:?}",
                reply.results[0].refusal
            );
            // The profile beside it round-trips (F2.3 retired the `deferred`
            // arm this test used to assert), landing at the singleton key —
            // and a mixed batch still resolves each row on its own merits.
            assert!(
                reply.results[1].refusal.is_none(),
                "the profile write must round-trip: {:?}",
                reply.results[1].refusal
            );
            assert_eq!(reply.results[1].rkey.as_deref(), Some("self"));
            assert!(
                reply.results[1].reproject_record,
                "a profile write asks the projection to render the record the repo carries"
            );
        }

        /// An account that never completed the mint ceremony still gets the
        /// *permanent* fauna-surface answer even though the signing arm now
        /// exists — the two refusals must not collapse into one.
        #[tokio::test]
        async fn a_post_still_refuses_fauna_surface_when_only_the_cert_is_missing() {
            let (state, user, _) = seeded_with_delegation().await;
            // Revoke: destroys the row, so K exists nowhere and no cert does.
            revoke_authoring_delegation_handler()(
                state.clone(),
                user,
                enc(&RevokeAuthoringDelegationRequest::default()),
            )
            .await
            .expect("revoke");
            // Re-mint K *without* provisioning a cert — the certless in-between
            // state slice 2b explicitly allows.
            fetch_authoring_key_handler()(
                state.clone(),
                user,
                enc(&FetchAuthoringKeyRequest::default()),
            )
            .await
            .expect("re-mint");

            let reply = ingest_as(
                &state,
                user,
                vec![post_write(
                    "create",
                    Some(post_record(serde_json::json!({}))),
                )],
            )
            .await
            .expect("ingest");
            let refusal = reply.results[0].refusal.as_ref().expect("refused");
            assert_eq!(
                refusal.sub_type, "fauna_surface",
                "a minted-but-uncertified account has not authorized posting"
            );
        }

        /// A malformed action anywhere in the batch fails the whole call and
        /// applies **nothing** — the classify-then-apply split. Before it, a
        /// bad action at index 1 left index 0's journal row behind.
        #[tokio::test]
        async fn a_malformed_action_late_in_the_batch_applies_nothing() {
            let state = seeded().await;
            let err = ingest(
                &state,
                vec![
                    write("app.bsky.graph.list", "create", Some("3first")),
                    write("app.bsky.graph.list", "frobnicate", Some("3bad")),
                ],
            )
            .await
            .expect_err("an unknown action fails the call");
            assert!(err.code.contains("malformed") || !err.code.is_empty());
            assert!(
                state
                    .db
                    .get_atproto_native_record(&USER, "app.bsky.graph.list", "3first")
                    .await
                    .unwrap()
                    .is_none(),
                "a batch that fails validation must leave no partial writes behind"
            );
        }

        /// A post *update* is refused permanently (immutable), and the
        /// sub-type must be `policy` — not `deferred`, which a later session
        /// would read as "build it".
        #[tokio::test]
        async fn a_post_update_is_policy_refused_not_deferred() {
            let state = seeded().await;
            let reply = ingest(
                &state,
                vec![write("app.bsky.feed.post", "update", Some("3existing"))],
            )
            .await
            .expect("ingest");
            let refusal = reply.results[0].refusal.as_ref().expect("refused");
            assert_eq!(refusal.sub_type, "policy");
        }

        /// A batch is ALL OR NOTHING: one refused row means no row is applied,
        /// and the results stay positionally aligned so the caller still learns
        /// exactly which row to fix.
        ///
        /// **This supersedes the earlier "a refusal mid-batch does not roll
        /// back the rows around it" ruling** (F2.1, 2026-07-24), which was
        /// decided when `applyWrites` was not served and only single-write
        /// verbs existed — a world where "partial" could never arise. It cannot
        /// survive contact with `applyWrites`: that lexicon's reply is a
        /// results array or an error, with no shape for "3 of your 5 writes
        /// happened", so applying the good rows and failing the call would
        /// leave real Fauna posts behind for a write the caller was told did
        /// not occur. Per-write refusal DATA — the half `:394` actually
        /// ratified — is unchanged.
        #[tokio::test]
        async fn a_refused_row_rolls_back_the_whole_batch() {
            let state = seeded().await;
            let reply = ingest(
                &state,
                vec![
                    write("app.bsky.graph.list", "create", Some("3ok1")),
                    write("app.bsky.feed.post", "update", Some("3bad")),
                    write("app.bsky.feed.threadgate", "create", Some("3ok2")),
                ],
            )
            .await
            .expect("ingest");

            // Positional alignment survives, and the row that earned the
            // refusal still carries its own reason — that is what lets the
            // bridge name the offending row in the error it returns.
            assert_eq!(reply.results.len(), 3);
            assert_eq!(
                reply.results[1]
                    .refusal
                    .as_ref()
                    .map(|r| r.sub_type.as_str()),
                Some("policy"),
                "the offending row keeps its own sub-typed reason"
            );
            // Every other row is explicitly told the batch was abandoned. A
            // result with neither a refusal nor an rkey would read downstream
            // as a malformed reply rather than as "not applied".
            for i in [0, 2] {
                let r = reply.results[i]
                    .refusal
                    .as_ref()
                    .unwrap_or_else(|| panic!("row {i} must be told the batch was abandoned"));
                assert!(
                    r.message
                        .contains("another write in this batch was refused"),
                    "row {i} message: {}",
                    r.message
                );
            }
            // And neither journal row landed.
            for (coll, rkey) in [
                ("app.bsky.graph.list", "3ok1"),
                ("app.bsky.feed.threadgate", "3ok2"),
            ] {
                assert!(
                    state
                        .db
                        .get_atproto_native_record(&USER, coll, rkey)
                        .await
                        .unwrap()
                        .is_none(),
                    "{rkey} must NOT have landed: a refused batch applies nothing"
                );
            }
        }

        /// The rule above, driven by a refusal the disposition pass CANNOT
        /// see.
        ///
        /// `a_refused_row_rolls_back_the_whole_batch` refuses via the
        /// classifier (a post *update*), so it exercises only the up-front
        /// disposition map and stayed green while this defect was live. The
        /// refusing row here is an **unparseable record**: the bridge does no
        /// Lexicon validation, so `createdAt: "not-a-timestamp"` is perfectly
        /// good dag-cbor that only fails when the round-trip arm parses it —
        /// which used to happen after row 0 had already been applied, with
        /// nothing to roll it back. An ordinary external app sending one
        /// slightly-wrong row in a batch reaches this.
        #[tokio::test]
        async fn a_refusal_only_the_record_can_reveal_still_rolls_back_the_batch() {
            // A delegation must exist, or the disposition pass refuses the post
            // row `fauna_surface` up front and the test would prove nothing.
            let (state, user, _) = seeded_with_delegation().await;
            let reply = ingest_as(
                &state,
                user,
                vec![
                    write("app.bsky.graph.list", "create", Some("3ok1")),
                    // Structurally fine to the bridge (real dag-cbor, right
                    // `$type`), unparseable to the round-trip arm: nothing
                    // short of parsing the record can tell.
                    post_write(
                        "create",
                        Some(post_record(
                            serde_json::json!({"createdAt": "not-a-timestamp"}),
                        )),
                    ),
                ],
            )
            .await
            .expect("ingest");

            assert_eq!(reply.results.len(), 2);
            let refusal = reply.results[1]
                .refusal
                .as_ref()
                .expect("the unparseable row must refuse");
            assert_eq!(refusal.sub_type, "policy", "{refusal:?}");
            // The earlier row left NO trace. Before this journal row
            // was present, and — worse — the projection loop would then carry
            // it onto the firehose for a call the caller was told failed.
            assert!(
                state
                    .db
                    .get_atproto_native_record(&user, "app.bsky.graph.list", "3ok1")
                    .await
                    .unwrap()
                    .is_none(),
                "row 0 was applied even though row 1 refused — the batch is not \
                 all-or-nothing for refusals the disposition pass cannot see"
            );
        }

        /// The same rule, driven by a whole-call **`RpcError`** rather than a
        /// refusal.
        ///
        /// Both pins above drive `refused(…)`. But what `:223` forbids is an
        /// *outcome* — "rows applied, call answered as failed, projection carries
        /// them onto the network" — and **any** mid-loop `Err` produces it just as
        /// well. The refusal-vs-`RpcError` distinction the all-or-nothing note
        /// originally leaned on does no work for the property.
        ///
        /// The refusing row here is an **over-cap record**: a pure function of the
        /// row, so nothing but ordering ever made it late. It used to be raised
        /// inside the journal arm, after row 0's post had already been created and
        /// federated — for a call the caller was told was malformed. The fix
        /// decides every such shape error in `check_write_input_shape`, before any
        /// row is applied.
        ///
        /// Note this arm is only meaningful because row 0 is a **round-trip post
        /// create**, the most expensive and least reversible thing the batch can
        /// do: a journal-only version would pass against a much weaker fix.
        #[tokio::test]
        async fn a_whole_call_error_from_a_later_row_still_rolls_back_the_batch() {
            let (state, user, _) = seeded_with_delegation().await;

            // Over the per-record cap by construction, and NOT decidable from
            // `(collection, action)` plus the account row — only from the row's
            // own bytes.
            let oversize = ExternalWrite {
                record: Some(ByteBuf::from(vec![
                    b'x';
                    EXTERNAL_WRITE_RECORD_MAX_BYTES + 1
                ])),
                ..write("app.bsky.graph.list", "create", Some("3toobig"))
            };

            let err = ingest_as(
                &state,
                user,
                vec![
                    post_write("create", Some(post_record(serde_json::json!({})))),
                    oversize,
                ],
            )
            .await
            .expect_err("an over-cap row must fail the whole call");
            assert!(
                format!("{err:?}").contains("over the"),
                "the error must name the cap, not something incidental: {err:?}"
            );

            // The post from row 0 must NOT exist. Before it did — a real
            // Fauna post, federated and visible on all 7 apps, for a call the
            // caller was told failed, and the projection loop would then carry it
            // onto the firehose because no `post_map` row was written.
            let posts = state
                .db
                .list_posts_by_author(&user)
                .await
                .expect("list posts");
            assert!(
                posts.is_empty(),
                "row 0's post survived a failed batch — the batch is not \
                 all-or-nothing against a whole-call error, only against refusals"
            );
        }

        /// The control for the rule above: with no refusal in it, the same
        /// shape of batch applies every row. Without this arm, an
        /// implementation that simply never applied batches would pass.
        #[tokio::test]
        async fn a_clean_batch_applies_every_row() {
            let state = seeded().await;
            let reply = ingest(
                &state,
                vec![
                    write("app.bsky.graph.list", "create", Some("3ok1")),
                    write("app.bsky.feed.threadgate", "create", Some("3ok2")),
                ],
            )
            .await
            .expect("ingest");

            assert_eq!(reply.results.len(), 2);
            for (i, r) in reply.results.iter().enumerate() {
                assert!(r.refusal.is_none(), "row {i} refused: {:?}", r.refusal);
            }
            for (coll, rkey) in [
                ("app.bsky.graph.list", "3ok1"),
                ("app.bsky.feed.threadgate", "3ok2"),
            ] {
                assert!(
                    state
                        .db
                        .get_atproto_native_record(&USER, coll, rkey)
                        .await
                        .unwrap()
                        .is_some(),
                    "{rkey} must have landed"
                );
            }
        }

        /// The kill-switch is the account-wide off state; a stale bridge
        /// cache must never author content past it.
        #[tokio::test]
        async fn the_kill_switch_refuses_the_whole_batch() {
            let state = seeded().await;
            state
                .db
                .set_atproto_external_apps_enabled(&USER, false)
                .await
                .unwrap();

            let err = ingest(
                &state,
                vec![write("app.bsky.graph.list", "create", Some("3nope"))],
            )
            .await
            .expect_err("kill-switch refuses");
            assert_eq!(err.code, "fauna.bridges.atproto.disabled");
            assert!(
                state
                    .db
                    .get_atproto_native_record(&USER, "app.bsky.graph.list", "3nope")
                    .await
                    .unwrap()
                    .is_none(),
                "nothing may land when the plane is suspended"
            );
        }

        #[tokio::test]
        async fn the_kind_is_bridge_only() {
            let state = seeded().await;
            let req = IngestExternalWriteRequest {
                actor_id: USER.to_vec(),
                writes: vec![write("app.bsky.graph.list", "create", Some("3x"))],
                extra: Default::default(),
            };
            // The account's own user actor is NOT allowed to drive this kind:
            // a Fauna app writes through its identity-signed post kinds.
            ingest_external_write_handler()(state.clone(), USER, enc(&req))
                .await
                .expect_err("User class must be refused");
        }

        #[tokio::test]
        async fn an_over_cap_batch_is_refused_before_anything_lands() {
            let state = seeded().await;
            let writes = (0..EXTERNAL_WRITE_BATCH_MAX + 1)
                .map(|i| {
                    write(
                        "app.bsky.graph.list",
                        "create",
                        Some(Box::leak(format!("3r{i}").into_boxed_str())),
                    )
                })
                .collect();
            ingest(&state, writes).await.expect_err("over cap");
            assert!(
                state
                    .db
                    .list_atproto_native_records(&USER, "app.bsky.graph.list")
                    .await
                    .unwrap()
                    .is_empty(),
                "the cap must be checked before any row is applied"
            );
        }

        #[tokio::test]
        async fn a_journal_write_without_an_rkey_is_malformed() {
            let state = seeded().await;
            ingest(&state, vec![write("app.bsky.graph.list", "create", None)])
                .await
                .expect_err("journal writes need an rkey");
        }

        #[tokio::test]
        async fn an_unknown_action_is_malformed_not_a_silent_skip() {
            let state = seeded().await;
            ingest(
                &state,
                vec![write("app.bsky.graph.list", "frobnicate", Some("3x"))],
            )
            .await
            .expect_err("an unknown action must fail loudly");
        }

        // ── F2.3: the delegated profile round-trip ──────────────────

        /// A profile write carrying `record`. `putRecord` on the singleton is
        /// an UPDATE — the disposition is a function of (collection, action),
        /// so the verb is what routes it to the C2 sanctioned update path.
        fn profile_write(action: &str, record: Option<ByteBuf>) -> ExternalWrite {
            ExternalWrite {
                collection: "app.bsky.actor.profile".into(),
                action: action.into(),
                rkey: Some("self".into()),
                record,
                cid: Some("bafyfixturecid".into()),
                ..Default::default()
            }
        }

        fn profile_record(display_name: Option<&str>, description: Option<&str>) -> ByteBuf {
            let mut base = serde_json::json!({ "$type": "app.bsky.actor.profile" });
            let map = base.as_object_mut().unwrap();
            if let Some(dn) = display_name {
                map.insert("displayName".into(), serde_json::Value::String(dn.into()));
            }
            if let Some(d) = description {
                map.insert("description".into(), serde_json::Value::String(d.into()));
            }
            record_bytes(base)
        }

        /// Publish a Fauna profile for `identity` the way the owner's own
        /// client does — through `fauna.profile.set`'s real gate — so the
        /// round-trip below is merging over a genuinely stored profile.
        async fn publish_profile(state: &Arc<AppState>, profile: &fauna_core::data::Profile) {
            use fauna_core::identity::ActorKeypair;
            let identity = ActorKeypair::from_secret([11u8; 32]);
            let body = fauna_core::encoding::sign_and_pack(&identity, profile).unwrap();
            crate::profile_handlers::ingest_profile_core(state, identity.actor_id().0, &body)
                .await
                .expect("the owner publishes their profile");
        }

        fn base_profile(actor: [u8; 32]) -> fauna_core::data::Profile {
            use fauna_core::data::{
                InboxMode, NestEntry, NestRole, Profile, ProfileLink, Timestamp,
            };
            use fauna_core::identity::ActorId;
            Profile {
                actor_id: ActorId(actor),
                display_name: Some("Alice".into()),
                bio: Some("the bio she wrote in a Fauna app".into()),
                avatar: Some(fauna_core::data::ContentHash::of_raw(b"her avatar bytes")),
                banner: None,
                links: vec![ProfileLink {
                    label: "site".into(),
                    uri: "https://example.invalid".into(),
                }],
                nests: vec![NestEntry {
                    nest_id: vec![9u8; 32],
                    url: "https://nest.example.invalid".into(),
                    roles: vec![NestRole::Social],
                }],
                admin_nests: vec![],
                load_hint: None,
                inbox_mode: InboxMode::Open,
                recovery_head: Some(fauna_core::recovery::ChainHead::new([3u8; 32], 1)),
                updated_at: Timestamp(1),
            }
        }

        async fn stored_profile(
            state: &Arc<AppState>,
            actor: &[u8; 32],
        ) -> fauna_core::data::Profile {
            let bytes = crate::profile_handlers::latest_profile_bytes(state, actor)
                .await
                .unwrap()
                .expect("a profile is stored");
            fauna_core::encoding::decode_profile(&bytes)
                .expect("the stored profile verifies")
                .0
        }

        /// **The F2.3 headline.** An external app's `putRecord` on the profile
        /// singleton becomes a real, stored, delegated-signed Fauna profile
        /// update — and the one read face every app, the ActivityPub actor
        /// serve and the ATProto projection all share decodes it as valid,
        /// authored by the ACCOUNT and signed by `K`.
        #[tokio::test]
        async fn a_delegated_profile_update_round_trips_and_verifies_through_the_shared_read_face()
        {
            use fauna_core::data::Capability;
            use fauna_core::encoding::{
                AuthoringOrigin, EmbedAsBytes, canonical_decode, decode_signed_bytes,
                verify_authoring_envelope,
            };

            let (state, user, k_pub) = seeded_with_delegation().await;
            publish_profile(&state, &base_profile(user)).await;

            let reply = ingest_as(
                &state,
                user,
                vec![profile_write(
                    "update",
                    Some(profile_record(Some("Alice B"), Some("bio from bsky.app"))),
                )],
            )
            .await
            .expect("ingest");
            let res = &reply.results[0];
            assert!(res.refusal.is_none(), "refused: {:?}", res.refusal);
            assert_eq!(res.rkey.as_deref(), Some("self"));
            assert_eq!(
                res.at_uri,
                Some(format!("at://{DID}/app.bsky.actor.profile/self"))
            );
            assert!(
                res.fauna_post_id.is_none(),
                "a profile is not a Fauna post; there is no post_map row to write"
            );

            // Reaching storage at all is the proof the shared gate accepted it.
            let bytes = crate::profile_handlers::latest_profile_bytes(&state, &user)
                .await
                .unwrap()
                .expect("the profile was stored");
            let (profile, origin) = fauna_core::encoding::decode_profile(&bytes)
                .expect("the shared read face accepts the delegated profile");
            // The D10 audit surface end-to-end: an external app's profile edit,
            // read back through the one shared face all 7 apps use, reports the
            // sub-key that made it (`atproto-pds-full.md` § D10 -> *Audit*).
            assert!(
                matches!(
                    origin,
                    fauna_core::encoding::AuthoringOrigin::Delegated { .. }
                ),
                "an external-app profile edit must read back as DELEGATED, got {origin:?}"
            );
            assert_eq!(profile.display_name.as_deref(), Some("Alice B"));
            assert_eq!(profile.bio.as_deref(), Some("bio from bsky.app"));

            // ...and it verifies as DELEGATED under K, not as the owner's own.
            let wire: EmbedAsBytes = canonical_decode(&bytes).unwrap();
            let auth = wire.signer_auth.clone();
            let (signed, env) = wire.into_signed().unwrap();
            let decoded: fauna_core::data::Profile = decode_signed_bytes(&signed).unwrap();
            let origin = verify_authoring_envelope(
                &decoded,
                &signed,
                &env,
                auth.as_deref(),
                &Capability::UpdateProfile,
                decoded.updated_at,
            )
            .expect("the chain verifies");
            assert_eq!(origin, AuthoringOrigin::Delegated { device_key: k_pub });
        }

        /// **The data-loss guard.** An `app.bsky.actor.profile` record can
        /// express a display name, a description and two blob refs — nothing
        /// else. Treating it as a whole profile would wipe the account's
        /// `nests` (how peers find it at all), links and recovery-key mirror
        /// because someone edited their bio in another app.
        ///
        /// **The avatar left this test's scope with the inbound-picture slice**
        /// and is asserted next door instead. It used to be listed here among
        /// the un-expressible fields, which was true only while the merge
        /// ignored blob refs: the record CAN express a picture, so under the
        /// ratified design an omitted one clears rather than survives
        /// (`an_external_app_clears_a_profile_picture_by_omitting_it`, and the
        /// echo case one further down). What is preserved here is what ATProto
        /// genuinely cannot say.
        #[tokio::test]
        async fn a_profile_update_preserves_every_field_atproto_cannot_express() {
            let (state, user, _) = seeded_with_delegation().await;
            let before = base_profile(user);
            publish_profile(&state, &before).await;

            ingest_as(
                &state,
                user,
                vec![profile_write(
                    "update",
                    Some(profile_record(Some("Alice B"), Some("bio from bsky.app"))),
                )],
            )
            .await
            .expect("ingest");

            let after = stored_profile(&state, &user).await;
            // Inside the record's translatable scope: taken from the record.
            assert_eq!(after.display_name.as_deref(), Some("Alice B"));
            assert_eq!(after.bio.as_deref(), Some("bio from bsky.app"));
            // Outside it: preserved, every one.
            assert_eq!(after.nests, before.nests, "nests must survive");
            assert_eq!(after.links.len(), 1, "links must survive");
            assert_eq!(after.recovery_head, before.recovery_head);
            assert_eq!(after.actor_id.0, user);
        }

        /// The other half of the same line: a field the record *can* express
        /// is authoritative, ABSENCE INCLUDED. Clearing a display name in
        /// bsky.app clears it in Fauna — otherwise a user could never remove
        /// one from the app they set it in.
        #[tokio::test]
        async fn a_profile_update_clears_a_display_name_the_record_omits() {
            let (state, user, _) = seeded_with_delegation().await;
            publish_profile(&state, &base_profile(user)).await;

            ingest_as(
                &state,
                user,
                vec![profile_write(
                    "update",
                    Some(profile_record(None, Some("just a bio"))),
                )],
            )
            .await
            .expect("ingest");

            let after = stored_profile(&state, &user).await;
            assert_eq!(
                after.display_name, None,
                "an omitted displayName clears it; preserving it would strand the user"
            );
            assert_eq!(after.bio.as_deref(), Some("just a bio"));
            assert!(!after.nests.is_empty(), "out-of-scope fields still survive");
        }

        /// The record the repo must carry is the PROJECTION's rendering of the
        /// merged profile, not the caller's bytes: the projection owns this
        /// collection and re-emits it, so committing the caller's record would
        /// have the next pass overwrite it and the synchronously answered cid
        /// would name bytes that do not survive.
        ///
        /// **The nest asserts that property; it does not render the record**
        /// (F2.4 slice 3). It cannot: a projected `avatar`/`banner` ref carries
        /// the picture's ATProto CID/MIME/size from the *bridge's* blob store,
        /// and publishability is sniffed from bytes that never reach this side.
        /// Rendering here would be a second implementation of the projection's
        /// rendering — which is exactly the bug this arm shipped with until
        /// slice 3, and it only ever showed up for accounts that had a picture.
        #[tokio::test]
        async fn a_profile_update_asks_the_projection_to_render_the_record() {
            let (state, user, _) = seeded_with_delegation().await;
            publish_profile(&state, &base_profile(user)).await;

            let reply = ingest_as(
                &state,
                user,
                vec![profile_write(
                    "update",
                    Some(profile_record(Some("Alice B"), Some("bio from bsky.app"))),
                )],
            )
            .await
            .expect("ingest");

            assert!(
                reply.results[0].reproject_record,
                "a profile write must tell the bridge to commit the projection's own \
                 rendering — without it the bridge commits the caller's bytes and the \
                 next projection pass overwrites them"
            );
            // The Fauna side really did apply the merge the render will read.
            let stored = stored_profile(&state, &user).await;
            assert_eq!(stored.display_name.as_deref(), Some("Alice B"));
            assert_eq!(stored.bio.as_deref(), Some("bio from bsky.app"));
        }

        /// A profile *delete* is fauna-surface-refused, not journaled: the
        /// projection owns this collection, so a journal tombstone for it
        /// would collide with the record the projection keeps emitting and the
        /// repo would stop being re-derivable from Fauna state.
        #[tokio::test]
        async fn a_profile_delete_refuses_fauna_surface_even_with_a_delegation() {
            let (state, user, _) = seeded_with_delegation().await;
            publish_profile(&state, &base_profile(user)).await;

            let reply = ingest_as(&state, user, vec![profile_write("delete", None)])
                .await
                .expect("ingest");
            let refusal = reply.results[0].refusal.as_ref().expect("refused");
            assert_eq!(refusal.sub_type, "fauna_surface");
            assert!(
                !stored_profile(&state, &user).await.nests.is_empty(),
                "a refused delete must leave the profile untouched"
            );
        }

        /// The same designed-permanent answer the post arm gives, reached from
        /// the profile disposition: without a D10 delegation nothing can sign,
        /// and the refusal names where to authorize.
        #[tokio::test]
        async fn a_profile_update_without_a_delegation_refuses_fauna_surface() {
            let state = seeded().await;
            let reply = ingest(
                &state,
                vec![profile_write(
                    "update",
                    Some(profile_record(Some("Alice"), None)),
                )],
            )
            .await
            .expect("ingest");
            let refusal = reply.results[0].refusal.as_ref().expect("refused");
            assert_eq!(refusal.sub_type, "fauna_surface");
            assert!(refusal.message.contains("Fauna app"));
        }

        /// A LIVE delegation whose cert does not cover the write's capability
        /// is refused with the re-authorize remedy, never surfaced as an
        /// error: provisioning accepts any subset of the enumerated authoring
        /// set, so a Post-only cert (a non-Fauna client minting its own) is a
        /// legitimate stored state — this is the dispatch pre-check, the good
        /// error message in front of the chain verify's guarantee. Found as a
        /// tier_3 500 on 2026-07-29: the tier_3 test's own Post-only mint hit
        /// the chain verify's step-4 rejection wearing `InternalServerError`.
        #[tokio::test]
        async fn a_profile_update_under_a_post_only_cert_refuses_fauna_surface_not_an_error() {
            use fauna_core::data::Capability;
            let (state, user, _) = seeded_with_delegation_caps(vec![Capability::Post]).await;
            let reply = ingest_as(
                &state,
                user,
                vec![profile_write(
                    "update",
                    Some(profile_record(Some("Alice B"), None)),
                )],
            )
            .await
            .expect("a capability miss is a refusal row, not a failed call");
            let refusal = reply.results[0].refusal.as_ref().expect("refused");
            assert_eq!(refusal.sub_type, "fauna_surface");
            assert!(
                refusal.message.contains("re-authorize"),
                "the remedy must be named: {}",
                refusal.message
            );
            // And nothing was written: the refusal preceded the Fauna ingest.
            assert!(
                crate::profile_handlers::latest_profile_bytes(&state, &user)
                    .await
                    .unwrap()
                    .is_none(),
                "a refused profile write must store nothing"
            );
        }

        /// The symmetric pin: an UpdateProfile-only cert cannot author a POST.
        /// Together with the test above this proves the pre-check is
        /// per-write-capability, not a batch-level any-capability pass.
        #[tokio::test]
        async fn a_post_create_under_an_updateprofile_only_cert_refuses_fauna_surface() {
            use fauna_core::data::Capability;
            let (state, user, _) =
                seeded_with_delegation_caps(vec![Capability::UpdateProfile]).await;
            let reply = ingest_as(
                &state,
                user,
                vec![ExternalWrite {
                    collection: "app.bsky.feed.post".into(),
                    action: "create".into(),
                    rkey: Some("3candidate".into()),
                    record: Some(post_record(serde_json::json!({}))),
                    cid: Some("bafyfixturecid".into()),
                    ..Default::default()
                }],
            )
            .await
            .expect("a capability miss is a refusal row, not a failed call");
            let refusal = reply.results[0].refusal.as_ref().expect("refused");
            assert_eq!(refusal.sub_type, "fauna_surface");
            assert!(
                refusal.message.contains("re-authorize"),
                "{}",
                refusal.message
            );
        }

        // ── F2.4 slice 2: the media arm ─────────────────────────────

        /// Encode a record whose JSON carries dag-json CID links (`{"/": …}`)
        /// the way indigo does — a link must land as a **tag-42 CID link**,
        /// not a one-key map, or the blob refs would not decode. The link-free
        /// fixtures above keep using `record_bytes`.
        fn record_bytes_with_links(v: serde_json::Value) -> ByteBuf {
            ByteBuf::from(fauna_bridge_atproto::test_support::dag_cbor(v))
        }

        /// A genuinely valid, distinct CIDv1(raw, sha2-256) per `seed`.
        fn blob_cid(seed: u8) -> String {
            fauna_bridge_atproto::test_support::test_cid(seed)
        }

        /// Seed what F2.4 slice 1's two nest-side writes leave behind for one
        /// uploaded blob: the media path's `blob_metadata` (sniffed type,
        /// measured size) and the `atproto_blobs` ledger row tying the ATProto
        /// CID to the Fauna `ContentHash`.
        async fn seed_uploaded_blob(
            state: &Arc<AppState>,
            user: &[u8; 32],
            seed: u8,
            content_type: &str,
            size: i64,
        ) -> (String, [u8; 32]) {
            let cid = blob_cid(seed);
            let media_ref = [seed; 32];
            state
                .db
                .put_blob_metadata(&media_ref, size, content_type, None, None)
                .await
                .unwrap();
            state
                .db
                .upsert_atproto_blob(user, &cid, &media_ref)
                .await
                .unwrap();
            (cid, media_ref)
        }

        fn image_entry(cid: &str, alt: &str, aspect: Option<(u32, u32)>) -> serde_json::Value {
            let mut entry = serde_json::json!({
                "alt": alt,
                "image": {
                    "$type": "blob",
                    "ref": { "/": cid },
                    // Declared and deliberately WRONG in the fixtures that
                    // check the stored type wins.
                    "mimeType": "image/jpeg",
                    "size": 1234,
                },
            });
            if let Some((w, h)) = aspect {
                entry["aspectRatio"] = serde_json::json!({ "width": w, "height": h });
            }
            entry
        }

        fn images_post(text: &str, entries: Vec<serde_json::Value>) -> ByteBuf {
            record_bytes_with_links(serde_json::json!({
                "$type": "app.bsky.feed.post",
                "text": text,
                "createdAt": "2026-03-20T12:00:00.000000Z",
                "embed": { "$type": "app.bsky.embed.images", "images": entries },
            }))
        }

        async fn stored_post_body(
            state: &Arc<AppState>,
            user: &[u8; 32],
        ) -> fauna_core::data::Post {
            use fauna_core::encoding::{EmbedAsBytes, canonical_decode, decode_signed_bytes};
            let rows = state
                .db
                .list_public_projection_page(user, None, 16, NO_PROJECTION_FLOOR)
                .await
                .unwrap();
            assert_eq!(rows.len(), 1, "exactly one post landed");
            let stored =
                crate::segments::post::load_post_body(&state.post_segments, &state.db, &rows[0].id)
                    .await
                    .unwrap()
                    .expect("post body resolvable");
            let wire: EmbedAsBytes = canonical_decode(&stored).unwrap();
            let (bytes, _env) = wire.into_signed().unwrap();
            decode_signed_bytes(&bytes).unwrap()
        }

        /// **The slice-2 headline.** An uploaded image referenced by a record
        /// becomes a real `PostBody::Media` post: the blob hash is the Fauna
        /// `ContentHash` the upload landed under, the type and size are the
        /// nest's own STORED values (the record's declared `mimeType` lies
        /// `image/jpeg` and must lose — sniffed-not-declared), the declared
        /// aspect ratio crosses as dimensions, the first image's alt becomes
        /// the set-level `alt_text` — and the ledger row is stamped
        /// `referenced_at`, which is what stands between these bytes and the
        /// F2.4 GC.
        #[tokio::test]
        async fn an_uploaded_image_reference_becomes_a_real_media_post_and_stamps_the_row() {
            use fauna_core::data::PostBody;

            let (state, user, _) = seeded_with_delegation().await;
            let (cid, media_ref) = seed_uploaded_blob(&state, &user, 41, "image/png", 4096).await;

            let reply = ingest_as(
                &state,
                user,
                vec![post_write(
                    "create",
                    Some(images_post(
                        "",
                        vec![image_entry(&cid, "a red bird", Some((640, 480)))],
                    )),
                )],
            )
            .await
            .expect("ingest");
            assert!(
                reply.results[0].refusal.is_none(),
                "a resolvable image must publish: {:?}",
                reply.results[0].refusal
            );

            let post = stored_post_body(&state, &user).await;
            let PostBody::Media { items, alt_text } = &post.body else {
                panic!("an image-only record must land as PostBody::Media, got {post:?}");
            };
            assert_eq!(items.len(), 1);
            assert_eq!(
                items[0].blob_hash,
                fauna_cbor::Cid::from_digest_raw(media_ref),
                "the media names the Fauna ContentHash the upload landed under"
            );
            assert_eq!(
                items[0].media_type, "image/png",
                "the STORED (sniffed) type wins over the record's declared image/jpeg"
            );
            assert_eq!(
                items[0].size_bytes, 4096,
                "the stored size, not the declared 1234"
            );
            assert_eq!(
                items[0].dimensions,
                Some(fauna_core::data::Dimensions {
                    width: 640,
                    height: 480
                })
            );
            assert_eq!(alt_text.as_deref(), Some("a red bird"));

            let row = state
                .db
                .get_atproto_blob(&user, &cid)
                .await
                .unwrap()
                .expect("ledger row");
            assert!(
                row.referenced_at.is_some(),
                "the reference stamp is what stands between these bytes and the GC"
            );
        }

        #[tokio::test]
        async fn text_plus_images_becomes_text_with_media() {
            use fauna_core::data::PostBody;

            let (state, user, _) = seeded_with_delegation().await;
            let (cid, _) = seed_uploaded_blob(&state, &user, 42, "image/webp", 999).await;

            let reply = ingest_as(
                &state,
                user,
                vec![post_write(
                    "create",
                    Some(images_post(POST_TEXT, vec![image_entry(&cid, "", None)])),
                )],
            )
            .await
            .expect("ingest");
            assert!(reply.results[0].refusal.is_none());

            let post = stored_post_body(&state, &user).await;
            let PostBody::TextWithMedia { content, items, .. } = &post.body else {
                panic!("text plus images must land as TextWithMedia, got {post:?}");
            };
            assert_eq!(content, POST_TEXT);
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].media_type, "image/webp");
            assert_eq!(
                items[0].dimensions, None,
                "no declared aspectRatio must not invent dimensions"
            );
        }

        /// **The ruling's pin, in the PROOF shape.** A blob ref that resolves
        /// to nothing REFUSES — and it rolls back the whole batch: row 0 is a
        /// journal create that would otherwise land, row 1 references bytes
        /// nobody uploaded. Neither may apply, and nothing may be stamped.
        /// (Mutation check: neuter `preflight_blob_refs` and row 0's journal
        /// row survives a failed call — exactly the outcome.)
        #[tokio::test]
        async fn an_unuploaded_blob_reference_refuses_and_rolls_back_the_batch() {
            let (state, user, _) = seeded_with_delegation().await;

            let reply = ingest_as(
                &state,
                user,
                vec![
                    write("app.bsky.graph.list", "create", Some("3rollback")),
                    post_write(
                        "create",
                        Some(images_post("", vec![image_entry(&blob_cid(77), "", None)])),
                    ),
                ],
            )
            .await
            .expect("a refusal is per-row data, not a failed call");

            let refusal = reply.results[1].refusal.as_ref().expect("row 1 refuses");
            assert_eq!(refusal.sub_type, "policy");
            assert!(
                refusal.message.contains("uploadBlob"),
                "the refusal must name the remedy, got: {}",
                refusal.message
            );
            assert!(
                reply.results[0]
                    .refusal
                    .as_ref()
                    .expect("row 0 told the batch was abandoned")
                    .message
                    .contains("wholly or not at all"),
            );
            assert!(
                state
                    .db
                    .get_atproto_native_record(&user, "app.bsky.graph.list", "3rollback")
                    .await
                    .unwrap()
                    .is_none(),
                "all-or-nothing: the journal row before the refusing row must not land"
            );
        }

        /// The blob rule covers JOURNAL rows too: an unknown-lexicon record
        /// referencing an uploaded blob journals and stamps the row; one
        /// referencing bytes never uploaded refuses. Both matter — a journaled
        /// record commits to the repo, so a dangling ref there 404s on the
        /// network exactly like a post's, and an unstamped-but-referenced row
        /// is GC bait.
        #[tokio::test]
        async fn a_journal_record_referencing_blobs_stamps_or_refuses_by_existence() {
            let (state, user, _) = seeded_with_delegation().await;
            let (cid, _) = seed_uploaded_blob(&state, &user, 43, "image/png", 10).await;

            let journal_record = |c: &str| {
                record_bytes_with_links(serde_json::json!({
                    "$type": "app.bsky.graph.list",
                    "name": "pins",
                    "avatar": {
                        "$type": "blob",
                        "ref": { "/": c },
                        "mimeType": "image/png",
                        "size": 10,
                    },
                }))
            };

            let ok = ingest_as(
                &state,
                user,
                vec![ExternalWrite {
                    record: Some(journal_record(&cid)),
                    ..write("app.bsky.graph.list", "create", Some("3haveblob"))
                }],
            )
            .await
            .expect("ingest");
            assert!(
                ok.results[0].refusal.is_none(),
                "{:?}",
                ok.results[0].refusal
            );
            assert!(
                state
                    .db
                    .get_atproto_blob(&user, &cid)
                    .await
                    .unwrap()
                    .unwrap()
                    .referenced_at
                    .is_some(),
                "a journaled record's reference must stamp too — its record is \
                 on the network just as much as a post's"
            );

            let missing = ingest_as(
                &state,
                user,
                vec![ExternalWrite {
                    record: Some(journal_record(&blob_cid(78))),
                    ..write("app.bsky.graph.list", "create", Some("3noblob"))
                }],
            )
            .await
            .expect("ingest");
            let refusal = missing.results[0].refusal.as_ref().expect("refused");
            assert!(
                refusal.message.contains("uploadBlob"),
                "{}",
                refusal.message
            );
        }

        /// The lexicon ceiling, two-sided with outbound's cap: five images
        /// refuse even when every one of them is resolvable.
        #[tokio::test]
        async fn more_than_four_images_refuse_even_when_all_resolve() {
            let (state, user, _) = seeded_with_delegation().await;
            let mut entries = Vec::new();
            for seed in 50..55u8 {
                let (cid, _) = seed_uploaded_blob(&state, &user, seed, "image/png", 10).await;
                entries.push(image_entry(&cid, "", None));
            }

            let reply = ingest_as(
                &state,
                user,
                vec![post_write("create", Some(images_post("", entries)))],
            )
            .await
            .expect("ingest");
            let refusal = reply.results[0].refusal.as_ref().expect("refused");
            assert!(
                refusal.message.contains("at most"),
                "the ceiling refusal names the limit, got: {}",
                refusal.message
            );
        }

        /// **The exemption pin, EVOLVED (the inbound-picture slice).** It used
        /// to assert that a profile update keeping a projection-stored picture
        /// is not refused *because profiles are outside the blob rule's row
        /// set*. Profiles are now INSIDE that set — the caller's record authors
        /// the picture choice — so the same journey still succeeds, but for a
        /// different and stronger reason: the bridge RESOLVED the echoed ref
        /// against its own store and vouched for it as `resolved_media`.
        ///
        /// The `mutation` half is what makes that non-vacuous: drop the vouch
        /// and the identical write must refuse rather than dangle.
        #[tokio::test]
        async fn a_profile_update_keeping_a_projection_stored_picture_needs_the_bridges_vouch() {
            let (state, user, _) = seeded_with_delegation().await;
            let (uploaded_cid, _) = seed_uploaded_blob(&state, &user, 44, "image/png", 10).await;
            let echoed = blob_cid(79);
            let echoed_fauna = fauna_core::data::ContentHash::from_digest_raw([79u8; 32]);

            let record = || {
                record_bytes_with_links(serde_json::json!({
                    "$type": "app.bsky.actor.profile",
                    "displayName": "Ada",
                    // The avatar the account already has: projection-stored, so
                    // no `atproto_blobs` row exists for it — only the bridge's
                    // blob store can say what Fauna content it is.
                    "avatar": {
                        "$type": "blob",
                        "ref": { "/": echoed.clone() },
                        "mimeType": "image/png",
                        "size": 10,
                    },
                }))
            };

            let mut vouched = profile_write("update", Some(record()));
            vouched
                .resolved_media
                .insert(echoed.clone(), echoed_fauna.to_base32());
            let reply = ingest_as(&state, user, vec![vouched])
                .await
                .expect("ingest");
            assert!(
                reply.results[0].refusal.is_none(),
                "a bridge-vouched echo must not refuse: {:?}",
                reply.results[0].refusal
            );
            assert_eq!(
                stored_profile(&state, &user).await.avatar,
                Some(echoed_fauna),
                "the echoed picture resolves to the content the profile already holds"
            );

            // MUTATION: the same record with no vouch. Neither resolver knows
            // the ref, so committing it would publish a dangling reference.
            let reply = ingest_as(&state, user, vec![profile_write("update", Some(record()))])
                .await
                .expect("ingest");
            assert!(
                reply.results[0].refusal.is_some(),
                "an unvouched, un-uploaded picture ref must refuse — the caller's \
                 echo is never trusted"
            );

            // An unrelated uploaded blob is untouched by either attempt: the
            // stamp covers the refs the record names, not the ledger at large.
            assert!(
                state
                    .db
                    .get_atproto_blob(&user, &uploaded_cid)
                    .await
                    .unwrap()
                    .unwrap()
                    .referenced_at
                    .is_none(),
                "an unrelated uploaded blob must stay unstamped by a profile write"
            );
        }

        /// A post's **images** may not come from a bridge vouch — and the guard
        /// that refuses this row is the MEDIA ARM, not the echo scope.
        ///
        /// ⚠ Read this before treating it as the pin for "the nest decides
        /// which rows may use a bridge vouch": it is not, and believing it was
        /// cost the security review a turn. An image ref is
        /// resolved a second time by [`media_items_for_post`], which asks
        /// [`resolve_uploaded_blob`] **directly** and never consults
        /// `resolved_media` — so this row refuses even with the echo scope
        /// removed entirely. What it genuinely pins is that second, older
        /// guard: the media arm never grew an echo-aware path.
        ///
        /// The echo-scope property itself is pinned where
        /// [`resolves_echoed_media`] is the *sole* gate — a post's link-card
        /// `thumb` and a journal row, the two classes the media arm never maps
        /// (the pre-flight's own comment enumerates them). See
        /// `a_post_link_card_thumb_may_not_use_the_bridges_vouch` and
        /// `a_journal_record_may_not_use_the_bridges_vouch`.
        #[tokio::test]
        async fn a_post_image_may_not_resolve_a_picture_through_the_bridges_vouch() {
            let (state, user, _) = seeded_with_delegation().await;
            let echoed = blob_cid(88);
            let echoed_fauna = fauna_core::data::ContentHash::from_digest_raw([88u8; 32]);

            let record = record_bytes_with_links(serde_json::json!({
                "$type": "app.bsky.feed.post",
                "text": "an image this account never uploaded here",
                "createdAt": "2026-07-30T10:00:00.000Z",
                "embed": {
                    "$type": "app.bsky.embed.images",
                    "images": [image_entry(&echoed, "", None)],
                },
            }));
            let mut w = post_write("create", Some(record));
            // A vouch a future bridge might send for a record-committing row.
            w.resolved_media
                .insert(echoed.clone(), echoed_fauna.to_base32());

            let reply = ingest_as(&state, user, vec![w]).await.expect("ingest");
            let refusal = reply.results[0]
                .refusal
                .as_ref()
                .expect("a post's blob must come from this nest's upload ledger");
            assert!(
                refusal.message.contains("uploadBlob"),
                "the refusal must name the remedy, got: {}",
                refusal.message
            );
        }

        /// **`resolved_media` is NOT a bypass of the upload ledger** — the nest
        /// decides which rows may use a bridge vouch, never the bridge by what
        /// it chose to send ([`resolves_echoed_media`] — a point a prior verify
        /// contract pinned only vacuously).
        ///
        /// A post's link-card `thumb` is the honest witness: it is a real blob
        /// ref, so the pre-flight's generic walk sees it, but the media arm
        /// maps only `app.bsky.embed.images` — so [`resolves_echoed_media`] is
        /// the **sole** gate standing between this row and an unuploaded ref.
        /// Delete that scope decision and this test goes red; the images pin
        /// above does not, because a second guard catches it.
        ///
        /// The bridge scopes production to profile writes today, so this row
        /// cannot arise from the current bridge at all. That is exactly why it
        /// is pinned: the property must hold against a FUTURE bridge, and a
        /// test exercising only what today's producer emits would go on
        /// passing while the bypass opened.
        #[tokio::test]
        async fn a_post_link_card_thumb_may_not_use_the_bridges_vouch() {
            let (state, user, _) = seeded_with_delegation().await;
            let echoed = blob_cid(91);
            let echoed_fauna = fauna_core::data::ContentHash::from_digest_raw([91u8; 32]);

            let record = record_bytes_with_links(serde_json::json!({
                "$type": "app.bsky.feed.post",
                "text": "a link card whose thumb was never uploaded here",
                "createdAt": "2026-07-31T10:00:00.000Z",
                "embed": {
                    "$type": "app.bsky.embed.external",
                    "external": {
                        "uri": "https://example.com",
                        "title": "t",
                        "description": "d",
                        "thumb": {
                            "$type": "blob",
                            "ref": { "/": echoed.clone() },
                            "mimeType": "image/png",
                            "size": 10,
                        },
                    },
                },
            }));
            let mut w = post_write("create", Some(record));
            // A vouch a future bridge might send for a record-committing row.
            w.resolved_media
                .insert(echoed.clone(), echoed_fauna.to_base32());

            let reply = ingest_as(&state, user, vec![w]).await.expect("ingest");
            let refusal = reply.results[0].refusal.as_ref().expect(
                "a post's link-card thumb must resolve against this nest's \
                 upload ledger, never through the bridge's vouch",
            );
            // Name the ref, so a refusal raised for some unrelated reason
            // cannot stand in for this one.
            assert!(
                refusal.message.contains(&echoed) && refusal.message.contains("uploadBlob"),
                "the refusal must name the unuploaded thumb and the remedy, got: {}",
                refusal.message
            );
        }

        /// The same property on the row class with **no second guard at all**:
        /// a journal row has no media arm, so [`resolves_echoed_media`] is the
        /// only thing deciding whether the bridge's vouch is consulted.
        ///
        /// Its positive half — a journal row whose ref this account really did
        /// upload applies and stamps — is
        /// `a_journal_record_referencing_blobs_stamps_or_refuses_by_existence`;
        /// what is new here is that a *vouch* cannot stand in for that upload.
        #[tokio::test]
        async fn a_journal_record_may_not_use_the_bridges_vouch() {
            let (state, user, _) = seeded_with_delegation().await;
            let echoed = blob_cid(92);
            let echoed_fauna = fauna_core::data::ContentHash::from_digest_raw([92u8; 32]);

            let record = record_bytes_with_links(serde_json::json!({
                "$type": "app.bsky.graph.list",
                "name": "pins",
                "avatar": {
                    "$type": "blob",
                    "ref": { "/": echoed.clone() },
                    "mimeType": "image/png",
                    "size": 10,
                },
            }));
            let mut w = ExternalWrite {
                record: Some(record),
                ..write("app.bsky.graph.list", "create", Some("3vouched"))
            };
            w.resolved_media
                .insert(echoed.clone(), echoed_fauna.to_base32());

            let reply = ingest_as(&state, user, vec![w]).await.expect("ingest");
            let refusal = reply.results[0]
                .refusal
                .as_ref()
                .expect("a journal row's blob ref must come from this nest's upload ledger");
            assert!(
                refusal.message.contains(&echoed) && refusal.message.contains("uploadBlob"),
                "the refusal must name the unuploaded ref and the remedy, got: {}",
                refusal.message
            );
            assert!(
                state
                    .db
                    .get_atproto_native_record(&user, "app.bsky.graph.list", "3vouched")
                    .await
                    .unwrap()
                    .is_none(),
                "a refused journal row must not land"
            );
        }

        /// **The headline inbound-picture assertion.** An external app that
        /// uploads an image and then sets it as its avatar ends up with a real
        /// Fauna profile picture — resolved through this nest's own upload
        /// ledger, with no bridge vouch needed and no caller-declared value
        /// trusted.
        ///
        /// The stamp is asserted in the same breath because it is what stops
        /// the F2.4 sweep collecting the bytes out from under the profile
        /// (the unreferenced-blob class: bytes stored with no reference the GC oracle
        /// walks are bytes already scheduled for deletion).
        #[tokio::test]
        async fn an_external_app_sets_a_profile_picture_from_its_own_upload() {
            let (state, user, _) = seeded_with_delegation().await;
            let (cid, media_ref) = seed_uploaded_blob(&state, &user, 51, "image/png", 640).await;

            let record = record_bytes_with_links(serde_json::json!({
                "$type": "app.bsky.actor.profile",
                "displayName": "Ada",
                "avatar": {
                    "$type": "blob",
                    "ref": { "/": cid.clone() },
                    "mimeType": "image/png",
                    "size": 640,
                },
            }));
            let reply = ingest_as(&state, user, vec![profile_write("update", Some(record))])
                .await
                .expect("ingest");
            assert!(
                reply.results[0].refusal.is_none(),
                "a freshly uploaded picture must resolve nest-side: {:?}",
                reply.results[0].refusal
            );

            let profile = stored_profile(&state, &user).await;
            assert_eq!(
                profile.avatar,
                Some(fauna_core::data::ContentHash::from_digest_raw(media_ref)),
                "the avatar is the Fauna content the upload landed as"
            );
            assert_eq!(profile.banner, None, "no banner ref means no banner");

            assert!(
                state
                    .db
                    .get_atproto_blob(&user, &cid)
                    .await
                    .unwrap()
                    .unwrap()
                    .referenced_at
                    .is_some(),
                "the picture's ledger row must be stamped, or the sweep collects \
                 the bytes the profile now points at"
            );
        }

        /// Absence is authoritative, exactly like `display_name`: a record with
        /// no `avatar` CLEARS the picture. Otherwise a picture set anywhere
        /// would be unremovable from bsky.app.
        ///
        /// And clearing destroys nothing — the bytes and their ledger row
        /// survive, so any Fauna app can set the same picture again.
        #[tokio::test]
        async fn an_external_app_clears_a_profile_picture_by_omitting_it() {
            let (state, user, _) = seeded_with_delegation().await;
            let (cid, _) = seed_uploaded_blob(&state, &user, 52, "image/png", 640).await;

            let with_picture = record_bytes_with_links(serde_json::json!({
                "$type": "app.bsky.actor.profile",
                "displayName": "Ada",
                "avatar": {
                    "$type": "blob",
                    "ref": { "/": cid.clone() },
                    "mimeType": "image/png",
                    "size": 640,
                },
            }));
            ingest_as(
                &state,
                user,
                vec![profile_write("update", Some(with_picture))],
            )
            .await
            .expect("ingest");
            assert!(stored_profile(&state, &user).await.avatar.is_some());

            // Now the same account, edited in an app that removed the picture.
            let reply = ingest_as(
                &state,
                user,
                vec![profile_write(
                    "update",
                    Some(profile_record(Some("Ada"), None)),
                )],
            )
            .await
            .expect("ingest");
            assert!(reply.results[0].refusal.is_none());
            assert_eq!(
                stored_profile(&state, &user).await.avatar,
                None,
                "omitting the avatar clears it — absence is authoritative"
            );

            assert!(
                state
                    .db
                    .get_atproto_blob(&user, &cid)
                    .await
                    .unwrap()
                    .is_some(),
                "clearing a picture destroys no bytes and no ledger row"
            );
        }

        /// The two fields are independent — a record carrying only a banner
        /// clears the avatar and sets the banner, never the other way round.
        /// (`atproto-pds-bridge.md:112`'s per-field independence, read inbound.)
        #[tokio::test]
        async fn avatar_and_banner_are_independent_fields() {
            let (state, user, _) = seeded_with_delegation().await;
            let (avatar_cid, avatar_ref) =
                seed_uploaded_blob(&state, &user, 53, "image/png", 100).await;
            let (banner_cid, banner_ref) =
                seed_uploaded_blob(&state, &user, 54, "image/png", 200).await;

            let both = record_bytes_with_links(serde_json::json!({
                "$type": "app.bsky.actor.profile",
                "avatar": {
                    "$type": "blob", "ref": { "/": avatar_cid }, "mimeType": "image/png", "size": 100,
                },
                "banner": {
                    "$type": "blob", "ref": { "/": banner_cid.clone() }, "mimeType": "image/png", "size": 200,
                },
            }));
            ingest_as(&state, user, vec![profile_write("update", Some(both))])
                .await
                .expect("ingest");
            let profile = stored_profile(&state, &user).await;
            assert_eq!(
                profile.avatar,
                Some(fauna_core::data::ContentHash::from_digest_raw(avatar_ref))
            );
            assert_eq!(
                profile.banner,
                Some(fauna_core::data::ContentHash::from_digest_raw(banner_ref))
            );

            // Banner only: the avatar clears, the banner stays.
            let banner_only = record_bytes_with_links(serde_json::json!({
                "$type": "app.bsky.actor.profile",
                "banner": {
                    "$type": "blob", "ref": { "/": banner_cid }, "mimeType": "image/png", "size": 200,
                },
            }));
            ingest_as(
                &state,
                user,
                vec![profile_write("update", Some(banner_only))],
            )
            .await
            .expect("ingest");
            let profile = stored_profile(&state, &user).await;
            assert_eq!(profile.avatar, None, "the omitted avatar cleared");
            assert_eq!(
                profile.banner,
                Some(fauna_core::data::ContentHash::from_digest_raw(banner_ref)),
                "the banner the record still carries is untouched"
            );
        }

        /// A picture neither resolver knows refuses the WHOLE batch, before
        /// anything applies — the all-or-nothing rule, and the reason a
        /// dangling ref can never be committed.
        #[tokio::test]
        async fn an_unresolvable_picture_refuses_the_whole_batch() {
            let (state, user, _) = seeded_with_delegation().await;
            let (good_cid, _) = seed_uploaded_blob(&state, &user, 55, "image/png", 10).await;

            let good = record_bytes_with_links(serde_json::json!({
                "$type": "app.bsky.actor.profile",
                "displayName": "Ada",
                "avatar": {
                    "$type": "blob", "ref": { "/": good_cid }, "mimeType": "image/png", "size": 10,
                },
            }));
            let bad = record_bytes_with_links(serde_json::json!({
                "$type": "app.bsky.actor.profile",
                "displayName": "Ada again",
                "avatar": {
                    "$type": "blob", "ref": { "/": blob_cid(99) }, "mimeType": "image/png", "size": 10,
                },
            }));
            let reply = ingest_as(
                &state,
                user,
                vec![
                    profile_write("update", Some(good)),
                    profile_write("update", Some(bad)),
                ],
            )
            .await
            .expect("ingest");

            assert!(
                reply.results[1].refusal.is_some(),
                "the unresolvable picture refuses"
            );
            assert!(
                reply.results[0].refusal.is_some(),
                "and its batch-mate is abandoned with it — all or nothing"
            );
            assert!(
                crate::profile_handlers::latest_profile_bytes(&state, &user)
                    .await
                    .unwrap()
                    .is_none(),
                "nothing was applied"
            );
        }
    }

    // ── The consent ceremony (F4 slice 6a, D3 rung 2) ────────────────────────

    mod consent {
        use super::*;

        async fn create(state: &Arc<AppState>, login_hint: Option<&str>) -> ConsentRequestRow {
            open_consent_request(
                state,
                ConsentStart::Browser { login_hint },
                "https://app.example/client-metadata.json",
                Some("Example App"),
                &["atproto".into(), "repo:app.bsky.feed.post".into()],
                &[],
                &crate::db::atproto_pds::ConsentBinding::default(),
            )
            .await
            .expect("open consent")
            .expect("the browser start always opens a row")
        }

        /// The owner's reading, flattened to what these tests assert on.
        struct Answer {
            status: &'static str,
            actor_id: Option<[u8; 32]>,
            login_did: Option<String>,
            scopes: Vec<String>,
        }

        async fn fetch(state: &Arc<AppState>, consent_id: &[u8]) -> Answer {
            let flat = |status| Answer {
                status,
                actor_id: None,
                login_did: None,
                scopes: Vec::new(),
            };
            match read_consent_state(state, consent_id)
                .await
                .expect("read consent")
            {
                ConsentState::Approved(approved) => Answer {
                    status: "approved",
                    actor_id: Some(approved.actor_id),
                    login_did: approved.login_did,
                    scopes: approved.scopes,
                },
                ConsentState::Denied => flat("denied"),
                ConsentState::Expired => flat("expired"),
                ConsentState::Pending => flat("pending"),
            }
        }

        async fn resolve(
            state: &Arc<AppState>,
            actor: [u8; 32],
            consent_id: &[u8],
            approved: bool,
        ) -> ResolveConsentReply {
            let req = ResolveConsentRequest {
                consent_id: consent_id.to_vec(),
                approved,
                ..Default::default()
            };
            let out = resolve_consent_handler()(state.clone(), actor, enc(&req))
                .await
                .expect("resolve consent");
            decode(&out).unwrap()
        }

        async fn list(state: &Arc<AppState>, actor: [u8; 32]) -> ListPendingConsentsReply {
            let out = list_pending_consents_handler()(
                state.clone(),
                actor,
                enc(&ListPendingConsentsRequest {
                    extra: Default::default(),
                }),
            )
            .await
            .expect("list consents");
            decode(&out).unwrap()
        }

        const DID: &str = "did:plc:7iza6de2dwap2sbkpav7c6c6";

        async fn active_identity(state: &Arc<AppState>) {
            state
                .db
                .upsert_atproto_identity_intent(&USER, "plc", "did:key:zDnaeUSER")
                .await
                .unwrap();
            state
                .db
                .record_atproto_minted(&USER, DID, None)
                .await
                .unwrap();
        }

        /// The whole ceremony, end to end at the owner boundary: the nest's
        /// authorize endpoint opens a request, the user's own device is pushed
        /// the card, the user approves over their own authed connection, and
        /// the long-poll's reading returns the account, its DID and the scopes
        /// the user was shown.
        #[tokio::test]
        async fn the_ceremony_runs_end_to_end_and_the_card_carries_the_pages_code() {
            let state = fixture_state().await;
            seed_user_and_bridge(&state).await;
            active_identity(&state).await;
            let (_conn, mut rx) = state.ws.subscribe(USER);

            let opened = create(&state, Some("alice")).await;
            assert!(!opened.code.is_empty());

            // The own-device fanout carries everything the card renders, so a
            // client that got the push needs no follow-up read.
            let frame = rx.recv().await.expect("consent push");
            let frame = fauna_protocol::decode_frame(&frame).unwrap();
            let fauna_protocol::Frame::Push(push) = frame else {
                panic!("expected a Push frame")
            };
            assert_eq!(push.kind, "fauna.atproto.consent_requested");
            // Through `from_push`, not a hand-rolled decode: that is the
            // classification every app actually runs, so this pins the
            // registry arm as well as the payload (pin the two
            // sides against each other, never each against its own literal).
            let fauna_protocol::PushEvent::AtprotoConsentRequested(pushed) =
                fauna_protocol::PushEvent::from_push(&push.kind, push.payload)
            else {
                panic!("the consent push must classify as its own variant")
            };
            assert_eq!(pushed.consent.consent_id, opened.consent_id);
            assert_eq!(
                pushed.consent.code, opened.code,
                "the card and the browser page must show ONE code"
            );
            assert_eq!(pushed.consent.client_name.as_deref(), Some("Example App"));
            assert_eq!(
                pushed.consent.client_id, "https://app.example/client-metadata.json",
                "the client_id renders verbatim — nothing derives an origin from it"
            );

            // …and the poll-fallback shows the same card to a client that was
            // closed when the push fired.
            let listed = list(&state, USER).await;
            assert_eq!(listed.consents.len(), 1);
            assert_eq!(listed.consents[0].code, opened.code);

            assert_eq!(fetch(&state, &opened.consent_id).await.status, "pending");

            assert!(
                resolve(&state, USER, &opened.consent_id, true)
                    .await
                    .resolved
            );

            let answered = fetch(&state, &opened.consent_id).await;
            assert_eq!(answered.status, "approved");
            assert_eq!(answered.actor_id, Some(USER));
            assert_eq!(answered.login_did.as_deref(), Some(DID));
            assert_eq!(
                answered.scopes,
                vec!["atproto".to_string(), "repo:app.bsky.feed.post".to_string()],
                "the grant is recorded from the row the USER was shown"
            );
            assert!(list(&state, USER).await.consents.is_empty());
        }

        const PUSH_CLIENT: &str = "http://localhost?redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb";

        async fn push(
            state: &Arc<AppState>,
            hint: &str,
            dpop_jkt: &str,
            authenticated: bool,
        ) -> Option<ConsentRequestRow> {
            open_consent_request(
                state,
                ConsentStart::Push {
                    login_hint: hint,
                    dpop_jkt,
                    authenticated,
                },
                PUSH_CLIENT,
                None,
                &["atproto".into()],
                &[],
                &crate::db::atproto_pds::ConsentBinding::default(),
            )
            .await
            .expect("open quiet push")
        }

        async fn approve_before(state: &Arc<AppState>, dpop_jkt: &str) {
            state
                .db
                .record_atproto_oauth_grant(
                    &USER,
                    &[3u8; 16],
                    PUSH_CLIENT,
                    None,
                    "atproto",
                    &[],
                    dpop_jkt,
                    fauna_core::data::Timestamp::now_millis() as i64 + 60_000,
                    None,
                    crate::db::atproto_pds::OAUTH_GRANT_ISSUER_NEST,
                    &crate::db::third_party_principals::UNATTESTED_DEVICE,
                )
                .await
                .unwrap();
        }

        /// Rule (a), with its proved-identity reading: a quiet push from a
        /// client this account never approved lands in the pending list and
        /// raises NO push; a public client's prior approval counts only for
        /// the key it was bound to; an authenticated client's counts by name.
        #[tokio::test]
        async fn a_quiet_push_notifies_only_an_approval_of_what_the_request_proved() {
            let state = fixture_state().await;
            seed_user_and_bridge(&state).await;
            let (_conn, mut rx) = state.ws.subscribe(USER);

            let quiet = push(&state, "alice", "jkt-a", false).await.expect("a row");
            assert!(rx.try_recv().is_err(), "never approved — no notification");
            assert_eq!(
                list(&state, USER).await.consents.len(),
                1,
                "but it is in the list"
            );
            assert_eq!(quiet.start, ConsentStartKind::Push);

            approve_before(&state, "jkt-a").await;
            push(&state, "alice", "jkt-impostor", false)
                .await
                .expect("a row");
            assert!(
                rx.try_recv().is_err(),
                "a public client's name under a key the user never approved must stay quiet"
            );
            push(&state, "alice", "jkt-a", false).await.expect("a row");
            assert!(rx.try_recv().is_ok(), "the approved installation notifies");
            push(&state, "alice", "jkt-rotated", true)
                .await
                .expect("a row");
            assert!(
                rx.try_recv().is_ok(),
                "an authenticated client proved its client_id, so any prior grant counts"
            );
            assert_eq!(
                list(&state, USER).await.consents.len(),
                1,
                "every repeat replaced the one before (rule (b))"
            );
        }

        /// Rule (c) and the no-oracle rule: an unresolved hint and a blocked
        /// client both open NOTHING — the caller answers both the same way.
        #[tokio::test]
        async fn a_blocked_client_or_an_unknown_hint_opens_nothing() {
            let state = fixture_state().await;
            seed_user_and_bridge(&state).await;
            let (_conn, mut rx) = state.ws.subscribe(USER);

            assert!(push(&state, "nobody-here", "jkt", true).await.is_none());
            state
                .db
                .set_oauth_client_block(&USER, PUSH_CLIENT, true)
                .await
                .unwrap();
            approve_before(&state, "jkt").await;
            assert!(push(&state, "alice", "jkt", true).await.is_none());
            assert!(rx.try_recv().is_err(), "a blocked client notifies nobody");
            assert!(list(&state, USER).await.consents.is_empty());
        }

        /// The typed code opens an unassigned, unlisted row and notifies
        /// nobody — there is nobody to notify until a code is typed.
        #[tokio::test]
        async fn a_typed_code_opens_an_unlisted_row_and_notifies_nobody() {
            let state = fixture_state().await;
            seed_user_and_bridge(&state).await;
            let (_conn, mut rx) = state.ws.subscribe(USER);
            let row = open_consent_request(
                &state,
                ConsentStart::TypedCode,
                PUSH_CLIENT,
                None,
                &["atproto".into()],
                &[],
                &crate::db::atproto_pds::ConsentBinding::default(),
            )
            .await
            .unwrap()
            .expect("the typed code always opens a row");
            assert!(row.actor_id.is_none());
            assert!(rx.try_recv().is_err());
            assert!(list(&state, USER).await.consents.is_empty());
        }

        /// ⚠⚠ **The authorize page must not be a handle-enumeration oracle.**
        /// A `login_hint` naming a real account and one naming nothing produce
        /// rows that differ only in freshly-minted values — same fields, same
        /// status, same shape — because the browser on the other side is
        /// anonymous and attacker-reachable.
        ///
        /// The mutation this exists for: making the outcome carry whether the
        /// hint resolved (or refusing an unresolvable hint) reddens exactly this.
        #[tokio::test]
        async fn an_unknown_login_hint_is_indistinguishable_from_a_known_one() {
            let state = fixture_state().await;
            seed_user_and_bridge(&state).await;

            let known = create(&state, Some("alice")).await;
            let unknown = create(&state, Some("nobody-here")).await;
            let unhinted = create(&state, None).await;

            for other in [&unknown, &unhinted] {
                assert_eq!(
                    known.consent_id.len(),
                    other.consent_id.len(),
                    "an id must not reveal whether the hint resolved"
                );
                assert_eq!(known.code.len(), other.code.len());
                assert_eq!(
                    fetch(&state, &known.consent_id).await.status,
                    fetch(&state, &other.consent_id).await.status,
                    "and neither must the poll's status"
                );
            }

            // The difference is where it belongs — in who can SEE the request:
            // only the resolved one reached an account, and the other two wait
            // in the unassigned pool any account may claim.
            let listed = list(&state, USER).await;
            assert_eq!(listed.consents.len(), 3);
            let other: [u8; 32] = [9u8; 32];
            state.db.create_user(&other, "free", "test").await.unwrap();
            let theirs = list(&state, other).await;
            assert_eq!(
                theirs.consents.len(),
                2,
                "another account sees the unassigned pool but not alice's card"
            );
            assert!(
                !theirs
                    .consents
                    .iter()
                    .any(|c| c.consent_id == known.consent_id)
            );
        }

        /// A decline is an answer, not a timeout: the authorize long-poll learns
        /// it promptly and can fail the browser cleanly. And it is final — see the DB-level
        /// `an_answered_request_cannot_be_answered_again`.
        #[tokio::test]
        async fn a_decline_is_reported_and_grants_nothing() {
            let state = fixture_state().await;
            seed_user_and_bridge(&state).await;
            active_identity(&state).await;

            let opened = create(&state, Some("alice")).await;
            assert!(
                resolve(&state, USER, &opened.consent_id, false)
                    .await
                    .resolved
            );

            let answered = fetch(&state, &opened.consent_id).await;
            assert_eq!(answered.status, "denied");
            assert!(
                answered.actor_id.is_none() && answered.login_did.is_none(),
                "a decline hands the token endpoint nothing to mint a grant from"
            );
            assert!(answered.scopes.is_empty());
        }

        /// A retried approval — a flaky connection, or the user's other device
        /// answering first — must never mint a second grant. The second call
        /// says so honestly instead of succeeding twice.
        #[tokio::test]
        async fn a_replayed_approval_resolves_nothing_the_second_time() {
            let state = fixture_state().await;
            seed_user_and_bridge(&state).await;

            let opened = create(&state, Some("alice")).await;
            assert!(
                resolve(&state, USER, &opened.consent_id, true)
                    .await
                    .resolved
            );
            assert!(
                !resolve(&state, USER, &opened.consent_id, true)
                    .await
                    .resolved
            );
        }

        /// An id this nest never issued reads `expired`, not a distinct
        /// "unknown": both mean *this flow is dead, start again* to the only
        /// caller, and two statuses with one meaning is how they drift.
        #[tokio::test]
        async fn an_unknown_consent_id_reads_as_expired() {
            let state = fixture_state().await;
            seed_user_and_bridge(&state).await;
            assert_eq!(fetch(&state, &[3u8; 32]).await.status, "expired");
        }

        /// The identity gate the account-facing surface already holds: an
        /// approval by an account whose ATProto presence is deactivated yields
        /// no DID, so the layer-2 step-down cannot be undone by
        /// approving a fresh OAuth grant.
        #[tokio::test]
        async fn a_deactivated_identity_approves_but_gets_no_login_did() {
            let state = fixture_state().await;
            seed_user_and_bridge(&state).await;
            active_identity(&state).await;
            state
                .db
                .set_atproto_identity_active(&USER, false)
                .await
                .unwrap();

            let opened = create(&state, Some("alice")).await;
            resolve(&state, USER, &opened.consent_id, true).await;
            let answered = fetch(&state, &opened.consent_id).await;
            assert_eq!(answered.status, "approved");
            assert!(
                answered.login_did.is_none(),
                "a suspended presence must not be re-opened through the OAuth plane"
            );
        }

        /// Class enforcement: the user's half is user-only. A bridge that could
        /// answer a consent request could approve a grant on the user's behalf.
        #[tokio::test]
        async fn the_users_half_is_class_separated() {
            let state = fixture_state().await;
            seed_user_and_bridge(&state).await;

            let opened = create(&state, Some("alice")).await;
            assert!(
                resolve_consent_handler()(
                    state.clone(),
                    BRIDGE,
                    enc(&ResolveConsentRequest {
                        consent_id: opened.consent_id.clone(),
                        approved: true,
                        ..Default::default()
                    })
                )
                .await
                .is_err(),
                "the bridge must not be able to approve on the user's behalf"
            );
            assert!(
                list_pending_consents_handler()(
                    state.clone(),
                    BRIDGE,
                    enc(&ListPendingConsentsRequest {
                        extra: Default::default()
                    })
                )
                .await
                .is_err()
            );
        }

        // ── The card chooses the folder (`authorization-server.md` § Consent) ──

        async fn create_with_scopes(state: &Arc<AppState>, scopes: &[&str]) -> ConsentRequestRow {
            let scopes: Vec<String> = scopes.iter().map(|s| s.to_string()).collect();
            open_consent_request(
                state,
                ConsentStart::Browser {
                    login_hint: Some("alice"),
                },
                "https://app.example/client-metadata.json",
                Some("Example App"),
                &scopes,
                &[],
                &crate::db::atproto_pds::ConsentBinding::default(),
            )
            .await
            .expect("open consent")
            .expect("the browser start always opens a row")
        }

        async fn resolve_choosing(
            state: &Arc<AppState>,
            actor: [u8; 32],
            consent_id: &[u8],
            approved: bool,
            folder: Option<i64>,
        ) -> Result<ResolveConsentReply, RpcError> {
            let req = ResolveConsentRequest {
                consent_id: consent_id.to_vec(),
                approved,
                folder,
                ..Default::default()
            };
            resolve_consent_handler()(state.clone(), actor, enc(&req))
                .await
                .map(|out| decode(&out).unwrap())
        }

        /// A row carrying the bare folder scope is approved with the card's
        /// chosen folder, and the nest qualifies the row with it BEFORE the
        /// approval is recorded — so the read-back the token is minted from
        /// carries the qualified string, nothing re-resolved. One choice
        /// qualifies every bare folder verb; other scopes pass untouched.
        #[tokio::test]
        async fn a_bare_folder_scope_is_qualified_by_the_cards_chosen_folder() {
            let state = fixture_state().await;
            seed_user_and_bridge(&state).await;
            let folder = state.db.create_folder("Inbox", &USER).await.unwrap();
            let opened =
                create_with_scopes(&state, &["fauna:folder:deposit", "fauna:feed:read"]).await;

            assert!(
                resolve_choosing(&state, USER, &opened.consent_id, true, None)
                    .await
                    .is_err(),
                "approving a bare folder scope requires the card's choice"
            );
            assert_eq!(fetch(&state, &opened.consent_id).await.status, "pending");

            assert!(
                resolve_choosing(&state, USER, &opened.consent_id, true, Some(folder))
                    .await
                    .unwrap()
                    .resolved
            );
            let answer = fetch(&state, &opened.consent_id).await;
            assert_eq!(answer.status, "approved");
            assert_eq!(
                answer.scopes,
                vec![
                    format!("fauna:folder:deposit:{folder}"),
                    "fauna:feed:read".into()
                ]
            );
        }

        /// The chosen folder is held to a folder row of the RESOLVING account:
        /// another account's folder, or an id that is no folder at all,
        /// resolves nothing — the uniform answer — and leaves the row live.
        #[tokio::test]
        async fn a_folder_not_the_resolving_accounts_resolves_nothing() {
            let state = fixture_state().await;
            seed_user_and_bridge(&state).await;
            let other: [u8; 32] = [9u8; 32];
            state.db.create_user(&other, "free", "test").await.unwrap();
            let theirs = state.db.create_folder("Theirs", &other).await.unwrap();
            let opened = create_with_scopes(&state, &["fauna:folder:deposit"]).await;

            for folder in [theirs, theirs + 1000] {
                let reply = resolve_choosing(&state, USER, &opened.consent_id, true, Some(folder))
                    .await
                    .unwrap();
                assert!(!reply.resolved, "folder {folder}");
                assert_eq!(fetch(&state, &opened.consent_id).await.status, "pending");
            }
        }

        /// A folder on any other answer is malformed: an approval of a row
        /// carrying no bare folder scope, and every decline. A decline of a
        /// bare row needs no choice.
        #[tokio::test]
        async fn a_folder_is_refused_where_nothing_takes_it() {
            let state = fixture_state().await;
            seed_user_and_bridge(&state).await;
            let folder = state.db.create_folder("Inbox", &USER).await.unwrap();

            let plain = create(&state, Some("alice")).await;
            let qualified =
                create_with_scopes(&state, &[&format!("fauna:folder:deposit:{folder}")]).await;
            for row in [&plain, &qualified] {
                assert!(
                    resolve_choosing(&state, USER, &row.consent_id, true, Some(folder))
                        .await
                        .is_err()
                );
                assert_eq!(fetch(&state, &row.consent_id).await.status, "pending");
            }
            let bare = create_with_scopes(&state, &["fauna:folder:deposit"]).await;
            assert!(
                resolve_choosing(&state, USER, &bare.consent_id, false, Some(folder))
                    .await
                    .is_err()
            );
            assert!(
                resolve_choosing(&state, USER, &bare.consent_id, false, None)
                    .await
                    .unwrap()
                    .resolved
            );
            assert_eq!(fetch(&state, &bare.consent_id).await.status, "denied");
        }
    }
}
