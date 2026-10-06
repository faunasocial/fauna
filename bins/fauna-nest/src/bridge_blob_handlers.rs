//! WS-RPC handlers for the `fauna.bridges.*` wrapped-blob surface.
//! Per spec § Bridge↔nest WS-RPC API. Nest stores opaque ciphertext
//! only; per-shape size limits live in `db/bridge_blobs.rs`. Caller
//! class enforcement uses `bridge_method_allowlist`.

use std::sync::Arc;
use std::time::Duration;

use serde_bytes::ByteBuf;

use fauna_protocol::{
    RpcError, Value,
    bridge_atproto::{
        AtprotoIdentityView, FetchAtprotoIdentitiesReply, FetchAtprotoIdentitiesRequest,
        FetchAtprotoIdentityKeyBlobReply, FetchAtprotoIdentityKeyBlobRequest,
        FetchAtprotoSessionSecretBlobReply, FetchAtprotoSessionSecretBlobRequest,
        RecordMintedIdentityReply, RecordMintedIdentityRequest,
    },
    decode_strict as decode,
    wrapped_blob::{
        ApprovePendingBridgeReply, ApprovePendingBridgeRequest, DkimSelectorInfo,
        FetchBridgePubkeyReply, FetchBridgePubkeyRequest, FetchMlsSnapshotBlobReply,
        FetchMlsSnapshotBlobRequest, FetchTlsCertBlobReply, FetchTlsCertBlobRequest,
        FetchWebdavKeysBlobReply, FetchWebdavKeysBlobRequest, FetchWrappedMlsBlobReply,
        FetchWrappedMlsBlobRequest, FetchWrappedSubmissionTokenReply,
        FetchWrappedSubmissionTokenRequest, GetCaldavPortReply, GetCaldavPortRequest,
        GetMailServingEnabledReply, GetMailServingEnabledRequest, ListDkimSelectorsReply,
        ListDkimSelectorsRequest, ListPendingBridgesReply, ListPendingBridgesRequest,
        ListServiceUsersReply, ListServiceUsersRequest, MintBulkByteTokenReply,
        MintBulkByteTokenRequest, ProvisionMlsSnapshotBlobRequest, ProvisionReply,
        ProvisionTlsCertBlobRequest, ProvisionWebdavKeysBlobRequest,
        ProvisionWrappedMlsBlobRequest, ProvisionWrappedSubmissionTokenRequest,
        RegisterServiceUserReply, RegisterServiceUserRequest, RejectPendingBridgeReply,
        RejectPendingBridgeRequest, ReportAuthEventReply, ReportAuthEventRequest,
        RequestEnrollmentReply, RequestEnrollmentRequest, RevokeDkimBlobRequest, RevokeReply,
        RevokeServiceUserReply, RevokeServiceUserRequest, RevokeWrappedMlsBlobRequest,
        RevokeWrappedSubmissionTokenRequest, ServiceUserInfo, SetAutoEnableMailForNewUsersReply,
        SetAutoEnableMailForNewUsersRequest, SetCalDavEnabledReply, SetCalDavEnabledRequest,
        SetCaldavPortReply, SetCaldavPortRequest, SetCardDavEnabledReply, SetCardDavEnabledRequest,
        SetMailEnabledReply, SetMailEnabledRequest, SetMailServingEnabledReply,
        SetMailServingEnabledRequest, SetWebDavEnabledReply, SetWebDavEnabledRequest, WebdavFile,
        WebdavListFilesReply, WebdavListFilesRequest, WebdavListFoldersReply,
        WebdavListFoldersRequest, WebdavQuotaReply, WebdavQuotaRequest, WebdavRecordChangeReply,
        WebdavRecordChangeRequest, WebdavServedSet,
    },
};

use fauna_protocol::wrapped_blob::{
    BulkByteMintPurpose, FetchGrantsReply, FetchGrantsRequest, MintGrantReply, MintGrantRequest,
    ReconcileGrantsReply, ReconcileGrantsRequest, RenewGrantReply, RenewGrantRequest, RescoreUnit,
    RescoreWorklistReply, RescoreWorklistRequest, RevokeGrantReply, RevokeGrantRequest,
    SpamBaselineCopy, SpamBaselineWorklistReply, SpamBaselineWorklistRequest, SubmitScoresReply,
    SubmitScoresRequest, SubmitSpamBaselineReply, SubmitSpamBaselineRequest,
};

use crate::bridge_method_allowlist::{CallerClass, actor_prefix_hex};
use crate::db::sync_storage::StorageQuotaError;
use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers ─────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

fn permission_denied(reason: &str) -> RpcError {
    crate::rpc_errors::permission_denied_ns("bridges", reason)
}

use crate::rpc_errors::internal;

fn rate_limited() -> RpcError {
    crate::rpc_errors::rate_limited_ns("bridges")
}

/// The named folder is not exposed to the actor over WebDAV — it does not
/// exist, is not owned by the actor, or is not served
/// ([`crate::db::FolderRow::is_webdav_served`]). The enforceable "cross-set" boundary for the
/// bulk-byte token (`webdav-server.md` § Bulk-byte plane).
fn set_not_served() -> RpcError {
    RpcError::new(
        "fauna.bridges.set_not_served",
        "error.bridges.set_not_served",
    )
}

/// The target actor's own folder row the MDA names, resolved hash-first (S5b):
/// a present `name_hash` alone selects — the address that survives the nest's
/// plaintext name blanking — and the name is the fallback only when the hash is
/// absent. Owner-scoped like `get_folder_for_actor`, so a hit implies ownership;
/// the caller still applies the served filter.
async fn webdav_folder_for_actor(
    state: &AppState,
    folder: &str,
    name_hash: &Option<ByteBuf>,
    target_actor: &[u8; 32],
) -> Result<Option<crate::db::FolderRow>, RpcError> {
    let name_hash = crate::routes::parse_name_hash(name_hash, |m| {
        crate::rpc_errors::invalid_request_ns("bridges", m)
    })?;
    match name_hash {
        Some(h) => {
            state
                .db
                .get_folder_for_actor_by_name_hash(&h, target_actor)
                .await
        }
        None => state.db.get_folder_for_actor(folder, target_actor).await,
    }
    .map_err(internal)
}

/// TTL for a minted bulk-byte token. Short-lived transport authz; the MDA
/// re-mints per transfer as needed (`webdav-server.md` § Bulk-byte plane).
///
/// `pub(crate)` because it owns the bulk-byte plane's token lifetime for the
/// whole nest: `federation_handlers::FOREIGN_WRITE_TOKEN_TTL_SECS` is defined
/// as this value rather than a second hand-written `600`.
pub(crate) const BULK_BYTE_TOKEN_TTL_SECS: u64 = 600;

// The client half of this plane's lifetime contract is
// `fauna_sync_engine::write_token_bearer`'s pre-expiry buffer, which races this
// TTL — not the 3600 s session TTL — so the margin here is the tight one.
// Below the buffer, a minted write token is spent before it is returned.
const _: () = assert!(
    BULK_BYTE_TOKEN_TTL_SECS > fauna_protocol::auth::BEARER_REFRESH_BUFFER_SECS,
    "a bulk-byte write token must outlive the client's pre-expiry refresh \
     buffer, or every upload re-mints instead of using its cached token"
);

fn not_found(reason: &str) -> RpcError {
    crate::rpc_errors::not_found_ns("bridges", reason)
}

/// A WebDAV conditional write lost its race: the path's current manifest-hash
/// ETag did not satisfy the `If-Match` / `If-None-Match` precondition. The Go MDA
/// maps this to a `412 Precondition Failed` (`webdav-server.md` § Protocol
/// surface). Enforced at the nest, the write serialization point.
fn conflict(reason: &str) -> RpcError {
    crate::rpc_errors::conflict_ns("bridges", reason)
}

/// Map a storage-quota outcome from a WebDAV write onto the wire: an `Exceeded`
/// rejection becomes the same typed `fauna.sync.storage_quota_exceeded` error the
/// `fauna.sync.changes.record` twin surfaces (a WebDAV write IS a sync change);
/// a DB error folds into `internal`.
fn webdav_quota_err(e: StorageQuotaError) -> RpcError {
    match e {
        e @ StorageQuotaError::Exceeded { .. } => {
            let mut err = RpcError::new(
                "fauna.sync.storage_quota_exceeded",
                "error.sync.storage_quota_exceeded",
            );
            err.details = Some(Box::new(Value::String(format!("{e}"))));
            err
        }
        e @ StorageQuotaError::MemberCapExceeded { .. } => {
            // Unreachable on the WebDAV owner path (no member cap context) —
            // exhaustiveness only; surface loudly if it ever fires.
            let mut err = RpcError::new(
                "fauna.sync.member_cap_exceeded",
                "error.sync.member_cap_exceeded",
            );
            err.details = Some(Box::new(Value::String(format!("{e}"))));
            err
        }
        // The MDA measures the body it just wrote, so a negative here is a
        // bridge bug rather than a user one — but the refusal is the metering
        // core's, uniform across all five record doors.
        e @ StorageQuotaError::NegativeSize { .. } => {
            let mut err = RpcError::new("fauna.sync.invalid_size", "error.sync.invalid_size");
            err.details = Some(Box::new(Value::String(format!("{e}"))));
            err
        }
        StorageQuotaError::Db(inner) => internal(inner),
    }
}

/// Public-metadata projection of an enrollment row — the only fields the
/// admin's approval cards / Bridges-detail roster need (no x25519 secret, no
/// approver id). Shared by `list_service_users` and `list_pending_bridges`.
/// Project one enrollment row for the wire.
///
/// `holder_view` is the User-visible (non-admin) projection. It withholds the
/// confinement self-probe for the same reason the roster itself is filtered
/// there: UIDs and sandbox status are **deployment topology**, and a user
/// enumerating capability-grant seal targets has no business reading how the
/// box is put together. Admin-class callers get it; nobody else does.
fn to_service_user_info(
    u: crate::db::bridge_service_users::BridgeServiceUser,
    holder_view: bool,
) -> ServiceUserInfo {
    let confinement = if holder_view { None } else { u.confinement };
    ServiceUserInfo {
        bridge_id: u.bridge_id,
        role: u.role.as_str().to_string(),
        status: u.status.as_str().to_string(),
        ed25519_pubkey: u.ed25519_pubkey.to_vec(),
        has_x25519: u.x25519_pubkey.is_some(),
        created_at: u.created_at.max(0) as u64,
        approved_at: u.approved_at.map(|t| t.max(0) as u64),
        confinement_reported_at: confinement.as_ref().map(|c| c.reported_at.max(0) as u64),
        confinement: confinement.map(|c| fauna_protocol::wrapped_blob::BridgeConfinement {
            uid: c.uid.clamp(0, u32::MAX as i64) as u32,
            sealed_store: c.sealed_store,
            landlock: c.landlock,
            seccomp: c.seccomp,
            extra: Default::default(),
        }),
        extra: Default::default(),
    }
}

/// Bound one confinement token before it is stored and later rendered on an
/// admin page.
///
/// The bridge already emits only a closed set, but this value crosses a trust
/// boundary — the reporting process is exactly the one whose compromise we are
/// diagnosing — so nest must not rely on the source's discipline. Same posture
/// as the log plane's admission (`log_plane::admit`): strip control characters,
/// hold a strict charset, cap the length.
///
/// It **bounds** rather than **allowlists** on purpose: a newer bridge may
/// legitimately report a state this build predates, and additive-everywhere
/// (`version-compatibility.md`) says that must survive. An empty or
/// wholly-illegal token collapses to `unknown` rather than failing the call — a
/// bridge must never be unable to enroll because a diagnostic string was odd.
fn bound_confinement_token(s: &str) -> String {
    const MAX: usize = 32;
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
        .take(MAX)
        .collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

async fn require_class(
    state: &Arc<AppState>,
    actor_id: &[u8; 32],
    kind: &str,
) -> Result<CallerClass, RpcError> {
    crate::bridge_method_allowlist::require_permission(&state.db, actor_id, kind, internal).await
}

// ── Provision handlers (user-owned blobs) ──────────────────────

fn provision_wrapped_mls_blob_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.provision_wrapped_mls_blob",
            )
            .await?;
            let req: ProvisionWrappedMlsBlobRequest = decode(&payload).map_err(malformed)?;
            if req.blob.len() > crate::db::bridge_blobs::MAX_WRAPPED_MLS_BYTES {
                return Err(malformed(format!(
                    "wrapped_mls blob too large: {} bytes (max {})",
                    req.blob.len(),
                    crate::db::bridge_blobs::MAX_WRAPPED_MLS_BYTES
                )));
            }
            // Key the row on the credential the client sealed the blob under
            // (mirrors `fetch_wrapped_mls_blob` / `revoke_wrapped_mls_blob`),
            // so an actor's multiple MUA credentials each get their own
            // wrapped-MSEK blob (mail-credentials.md § MUA-username; the MDA
            // fetches by the username's RFC 5233 sub-address suffix). The
            // credential_id is also bound into the blob's AAD, so a tampered
            // mismatch fails the AEAD-unwrap at AUTH.
            let credential_id = req.credential_id.trim();
            if credential_id.is_empty() {
                return Err(malformed(
                    "provision_wrapped_mls_blob: credential_id is empty",
                ));
            }
            state
                .db
                .put_wrapped_mls_blob(&actor_id, credential_id, req.blob.as_ref())
                .await
                .map_err(internal)?;
            encode_reply(&ProvisionReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn provision_mls_snapshot_blob_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.provision_mls_snapshot_blob",
            )
            .await?;
            let req: ProvisionMlsSnapshotBlobRequest = decode(&payload).map_err(malformed)?;
            if req.blob.len() > crate::db::bridge_blobs::MAX_MLS_SNAPSHOT_BYTES {
                return Err(malformed(format!(
                    "mls_snapshot blob too large: {} bytes (max {})",
                    req.blob.len(),
                    crate::db::bridge_blobs::MAX_MLS_SNAPSHOT_BYTES
                )));
            }
            state
                .db
                .put_mls_snapshot_blob(&actor_id, req.blob.as_ref())
                .await
                .map_err(internal)?;
            encode_reply(&ProvisionReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.provision_webdav_keys_blob` — the user's client deposits its
/// MSEK-sealed WebDAV served-set key blob (`webdav-server.md` § Key model). The
/// exact sibling of `provision_mls_snapshot_blob`: self-scoped (the row is keyed
/// on the connection's own `actor_id`, no target param — no escalation surface),
/// stored opaque (nest never decodes the blob), single row per actor
/// (INSERT OR REPLACE, so a re-provision on served-set/generation change wins).
fn provision_webdav_keys_blob_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.provision_webdav_keys_blob",
            )
            .await?;
            let req: ProvisionWebdavKeysBlobRequest = decode(&payload).map_err(malformed)?;
            if req.blob.len() > crate::db::bridge_blobs::MAX_WEBDAV_KEYS_BYTES {
                return Err(malformed(format!(
                    "webdav_keys blob too large: {} bytes (max {})",
                    req.blob.len(),
                    crate::db::bridge_blobs::MAX_WEBDAV_KEYS_BYTES
                )));
            }
            state
                .db
                .put_webdav_keys_blob(&actor_id, req.blob.as_ref())
                .await
                .map_err(internal)?;
            encode_reply(&ProvisionReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn provision_wrapped_submission_token_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.provision_wrapped_submission_token",
            )
            .await?;
            let req: ProvisionWrappedSubmissionTokenRequest =
                decode(&payload).map_err(malformed)?;
            if req.blob.len() > crate::db::bridge_blobs::MAX_WRAPPED_SUBMISSION_TOKEN_BYTES {
                return Err(malformed(format!(
                    "wrapped_submission_token blob too large: {} bytes (max {})",
                    req.blob.len(),
                    crate::db::bridge_blobs::MAX_WRAPPED_SUBMISSION_TOKEN_BYTES
                )));
            }
            // Per-credential, mirroring the wrapped-MLS blob + fetch/revoke
            // sides — each MUA credential gets its own submission token.
            let credential_id = req.credential_id.trim();
            if credential_id.is_empty() {
                return Err(malformed(
                    "provision_wrapped_submission_token: credential_id is empty",
                ));
            }
            state
                .db
                .put_wrapped_submission_token(&actor_id, credential_id, req.blob.as_ref())
                .await
                .map_err(internal)?;
            encode_reply(&ProvisionReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── Provision handlers (admin-owned blobs) ─────────────────────

// List DKIM selectors as unsealed public metadata (the admin DNS page). The
// sealed key is never returned — only the DNS TXT record + created-at the
// admin needs to render the list.
fn list_dkim_selectors_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.list_dkim_selectors").await?;
            let req: ListDkimSelectorsRequest = decode(&payload).map_err(malformed)?;
            let rows = state
                .db
                .list_dkim_selectors(req.domain.as_deref())
                .await
                .map_err(internal)?;
            let selectors = rows
                .into_iter()
                .map(|r| DkimSelectorInfo {
                    domain: r.domain,
                    selector: r.selector,
                    created_at: r.created_at.max(0) as u64,
                    public_dns_value: r.public_dns_value,
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&ListDkimSelectorsReply {
                selectors,
                extra: Default::default(),
            })
        })
    })
}

// Retire a DKIM selector (admin "retire" action — deletes the nest-held key
// and its public record). Idempotent: revoking an absent selector still
// replies `{ ok: true }`.
fn revoke_dkim_blob_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.revoke_dkim_blob").await?;
            let req: RevokeDkimBlobRequest = decode(&payload).map_err(malformed)?;
            state
                .db
                .delete_dkim_selector(&req.domain, &req.selector)
                .await
                .map_err(internal)?;
            encode_reply(&RevokeReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// Enumerate mail service-user bridges. Class-scoped reply (2026-07-17):
// an ADMIN gets the full roster (deliverability page, approval cards, the
// approved-roster rotate page); a plain USER gets only the mint-relevant
// holder view — approved + x25519-attested + content-processor-family role
// (`mda`/`content-processor`) + NOT in-process — which is exactly what the
// Nests-page trust facet needs to enumerate seal targets (nests.md § Where
// logic lives), with no enrollment history / pending topology exposed and
// no second wire kind minted. The in-process exclusion extends the
// spam-baseline holder ruling: a user-facing trust mint must never be
// offered the nest's own disk-resident key as a seal target (sealed to a
// key beside the ciphertext is not sealed). Public metadata only — the
// `x25519_pubkey` is reduced to a `has_x25519` flag, no secrets.
fn list_service_users_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class =
                require_class(&state, &actor_id, "fauna.bridges.list_service_users").await?;
            let req: ListServiceUsersRequest = decode(&payload).map_err(malformed)?;
            use crate::db::bridge_service_users::{BridgeRole, BridgeStatus};
            let role = match req.role.as_deref() {
                Some(s) => Some(
                    BridgeRole::parse(s).ok_or_else(|| malformed(format!("unknown role: {s}")))?,
                ),
                None => None,
            };
            let status = match req.status.as_deref() {
                Some(s) => Some(
                    BridgeStatus::parse(s)
                        .ok_or_else(|| malformed(format!("unknown status: {s}")))?,
                ),
                None => None,
            };
            let rows = state
                .db
                .list_bridge_service_users(role, status)
                .await
                .map_err(internal)?;
            let holder_view = !matches!(class, CallerClass::Admin);
            let service_users = rows
                .into_iter()
                .filter(|u| {
                    if !holder_view {
                        return true;
                    }
                    // The User-visible holder view: requested filters apply
                    // WITHIN this subset (a User asking status="pending" sees
                    // nothing, never the pending roster).
                    u.status == BridgeStatus::Approved
                        && u.x25519_pubkey.is_some()
                        && matches!(u.role, BridgeRole::Mda | BridgeRole::ContentProcessor)
                        && !u.in_process
                })
                .map(|u| to_service_user_info(u, holder_view))
                .collect();
            // Enrollment-strictness self-report (admin-only): whether each mail
            // role currently has an artifact-blessed pubkey, i.e. whether
            // `request_enrollment` is strict (PoP-gated) or the lenient
            // loopback fallback. Read fresh per call — same live-registry
            // semantics as enrollment itself (`blessed_bridge_pubkey`). This is
            // the no-SSH observable a live e2e / admin uses to verify a deployed
            // box is not silently lenient (security.md § Enrollment
            // proof-of-possession contract; testing.md § Gap 3).
            let enrollment_strict = (!holder_view).then(|| {
                use crate::db::bridge_service_users::BridgeRole;
                fauna_protocol::wrapped_blob::MailEnrollmentStrict {
                    mta: blessed_bridge_pubkey(&BridgeRole::Mta).is_some(),
                    mda: blessed_bridge_pubkey(&BridgeRole::Mda).is_some(),
                    extra: Default::default(),
                }
            });
            encode_reply(&ListServiceUsersReply {
                service_users,
                enrollment_strict,
                extra: Default::default(),
            })
        })
    })
}

// ── Bridge approval lifecycle (admin) ──────────────────────────
//
// The admin-pane approval flow (`mail-bridge-lifecycle.md` § Pending approval):
// enumerate pending bridges, then approve (pending → approved) or reject
// (→ revoked). All Admin-gated. These wrap the DB enrollment methods; the role
// is fixed at enrollment, so `approve` *validates* the supplied role rather
// than re-assigning it.

// Enumerate bridges awaiting approval (the approval-card feed). Distinct from
// `list_service_users` (the full roster) so the card view has a no-arg call.
fn list_pending_bridges_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.list_pending_bridges").await?;
            let _req: ListPendingBridgesRequest = decode(&payload).map_err(malformed)?;
            let rows = state
                .db
                .list_pending_bridge_service_users()
                .await
                .map_err(internal)?;
            // `holder_view: false` — this kind is Admin-only at the allowlist
            // (`bridge_method_allowlist.rs`: `matches!(class, Admin)`), so the
            // approval card may show a pending bridge's confinement probe. That
            // is exactly where it is most useful: an admin deciding whether to
            // approve a bridge can see whether the box it runs on is sandboxed.
            let bridges = rows
                .into_iter()
                .map(|u| to_service_user_info(u, false))
                .collect();
            encode_reply(&ListPendingBridgesReply {
                bridges,
                extra: Default::default(),
            })
        })
    })
}

/// The artifact-blessed Ed25519 pubkey nest requires a bridge to prove
/// possession of at `request_enrollment`. Returns `None` — keeping the "auto-approve any
/// fresh loopback pubkey" behaviour of a binary-only nest — unless the artifact
/// provisioned a blessed pubkey for the role through the registry-dir IPC env
/// `FAUNA_BLESSED_KEYS_DIR` (the image path — re-read per enrollment so a
/// service-user re-key's root-mediated re-bless reaches a running nest).
///
/// The registry files stay root-owned and **bridge-UID-unwritable**; nest only
/// reads. Confidentiality is *not* the property here — the pubkey is public —
/// **integrity** is: a bridge UID must not be able to substitute its own
/// pubkey, which the root-owned file + non-bridge-writable dir ensure. The
/// registry is **per-role** so the MTA's blessing cannot be presented to enroll
/// as the MDA. Strictness is gated on this presence so a binary-only / dev nest
/// (no artifact mint) stays lenient (deploy-safe — § Atomicity in
/// `security.md` § Implementation status, slice 2).
fn blessed_bridge_pubkey(role: &crate::db::bridge_service_users::BridgeRole) -> Option<[u8; 32]> {
    use crate::db::bridge_service_users::BridgeRole;
    let role_str = match role {
        BridgeRole::Mta => "mta",
        BridgeRole::Mda => "mda",
        // No artifact-blessed-key registry exists for a content-processor, so
        // there is no blessed pubkey to require — enrollment stays lenient (the
        // deploy-safe default this fn already falls back to when the env is
        // absent). A content-processor is admin-approved explicitly, not via a
        // blessed-pubkey auto-approve.
        BridgeRole::ContentProcessor => return None,
        // The ATProto PDS bridge is admin-approved explicitly (never
        // auto-approved), so S1 keeps enrollment lenient — no blessed-key
        // registry yet. Strict-mode blessing (a `FAUNA_BLESSED_ATPROTO_PDS_*`
        // registry) lands with the image packaging, not the role.
        BridgeRole::AtprotoPds => return None,
    };
    // Live registry read (service-user re-keying, `mail-bridge-lifecycle.md`
    // § Service-user re-keying): when the artifact exports the registry DIR
    // (`FAUNA_BLESSED_KEYS_DIR`, the image path), read the role's blessed
    // pubkey fresh from the root-owned file on EVERY enrollment — a
    // root-mediated re-bless after a bridge key rotation must reach a running
    // nest without a nest restart, which a boot-time env snapshot cannot.
    // Integrity is unchanged: the file + its dir stay root-owned and
    // bridge-UID-unwritable; nest only ever reads.
    let dir = std::env::var("FAUNA_BLESSED_KEYS_DIR").unwrap_or_default();
    if dir.trim().is_empty() {
        return None;
    }
    blessed_pubkey_from_dir(std::path::Path::new(dir.trim()), role_str)
}

/// Read + parse a role's blessed pubkey from the registry dir
/// (`<dir>/<role>.pub`). Pure in (dir, role) — no env — so it is unit-testable
/// against a temp dir. A missing/empty file means "not provisioned" → lenient
/// (`None`), as an unset registry dir is; a malformed value logs loudly
/// and stays lenient (works-out-of-the-box).
fn blessed_pubkey_from_dir(dir: &std::path::Path, role_str: &str) -> Option<[u8; 32]> {
    let path = dir.join(format!("{role_str}.pub"));
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    match parse_blessed_pubkey(&raw) {
        Ok(pk) => pk,
        Err(reason) => {
            tracing::error!(
                target: "bridge_rpc",
                role = role_str,
                path = %path.display(),
                "blessed-registry file is {reason} — enrollment for this role stays \
                 UNAUTHENTICATED"
            );
            None
        }
    }
}

/// Parse a hex-encoded 32-byte Ed25519 pubkey from a registry env value. Pure
/// (no env) so it is unit-testable. `Ok(None)` = absent/empty (not provisioned →
/// lenient); `Ok(Some(pk))` = a valid blessed key; `Err(reason)` =
/// provisioned-but-malformed (the caller logs it and stays lenient).
fn parse_blessed_pubkey(raw: &str) -> Result<Option<[u8; 32]>, &'static str> {
    let hex = raw.trim();
    if hex.is_empty() {
        return Ok(None);
    }
    let bytes = hex::decode(hex).map_err(|_| "not valid hex")?;
    let pk = <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| "not a 32-byte Ed25519 pubkey")?;
    Ok(Some(pk))
}

/// Decide whether a `request_enrollment` may proceed, given the presented identity and the artifact-blessed pubkey for the
/// role. Pure (no env / db) so the policy is unit-testable.
///
/// - `blessed == None` (no registry for the role): **lenient** — the
///   behaviour where the dispatch-layer loopback gate is the only proof. Returns
///   `Ok(None)` (nothing to bind). A router-fronted box without a blessed key is
///   a provisioning gap the caller logs.
/// - `blessed == Some(bpk)`: **strict**. Require (i) the presented Ed25519 pubkey
///   *is* the blessed key for the role, and (ii) a valid Ed25519 signature over
///   [`fauna_protocol::wrapped_blob::enrollment_signed_message`] proving
///   possession of that key — the key file is UID-isolated (slices 1+4), so a
///   co-resident attacker that cannot read it cannot produce the signature. The
///   message also covers the x25519 pubkey, so a valid signature binds it too; returns `Ok(Some(x25519))` for the caller to bind set-once. Reject
///   (`permission_denied`) on any miss.
fn check_enrollment_authorization(
    role_str: &str,
    presented_ed: &[u8; 32],
    x25519: Option<&[u8; 32]>,
    sig: Option<&[u8]>,
    blessed: Option<[u8; 32]>,
) -> Result<Option<[u8; 32]>, RpcError> {
    let Some(blessed) = blessed else {
        // Lenient: the auto-approve path, nothing to bind.
        return Ok(None);
    };
    // Strict: the presented identity must BE the artifact-blessed key —
    // a fresh attacker-generated pubkey (the exploit) is not blessed.
    if presented_ed != &blessed {
        return Err(permission_denied(
            "enrollment Ed25519 pubkey is not the artifact-blessed key for this role",
        ));
    }
    // ...and the enroller must prove possession of it (and bind x25519).
    let x25519 =
        x25519.ok_or_else(|| permission_denied("signed enrollment requires an x25519_pubkey"))?;
    let sig =
        sig.ok_or_else(|| permission_denied("signed enrollment requires an enrollment_sig"))?;
    let msg =
        fauna_protocol::wrapped_blob::enrollment_signed_message(role_str, presented_ed, x25519);
    // The artifact-blessed-key equality check above already pins `presented_ed`,
    // so this is not the attacker-chosen-key case — but it verifies
    // through the one primitive anyway, for a single verification shape.
    if !fauna_core::identity::verify_detached(presented_ed, &msg, sig) {
        return Err(permission_denied(
            "enrollment signature verification failed",
        ));
    }
    Ok(Some(*x25519))
}

/// Bind a PoP-verified x25519 pubkey to the bridge row at enrollment, reusing the
/// set-once freeze (`upsert_bridge_x25519`). Because this happens *before* the
/// later authed `register_service_user`, that handler's identical upsert then
/// only confirms the already-blessed value — so one enrollment signature closes
/// both enrollment requirements' PoP half ("the bridge signs its x25519 pubkey within the
/// same artifact-key attestation", `security.md`).
async fn bind_enrollment_x25519(
    db: &crate::db::CacheDb,
    pk: &[u8; 32],
    xpk: &[u8; 32],
) -> Result<(), RpcError> {
    db.upsert_bridge_x25519(pk, xpk).await.map_err(|e| {
        if e.downcast_ref::<crate::db::bridge_service_users::BridgeX25519Frozen>()
            .is_some()
        {
            permission_denied(
                "x25519 key already attested; rebinding a different key is not permitted",
            )
        } else {
            internal(e)
        }
    })?;
    Ok(())
}

// Bridge self-enrollment (zero-touch). A bridge that has generated a fresh
// keypair announces itself over the **anonymous pre-identity WS** to create a
// `pending` enrollment row, so it surfaces in `list_pending_bridges` for the
// admin to approve from their Fauna app — the one enrollment surface (the
// out-of-band HTTP pre-register route it replaced is removed;
// `mail-bridge-lifecycle.md` § Cold boot / § Pending approval). No
// `require_class`: this runs on the anonymous connection (it is in
// `pre_identity_allowlist`), and the dispatcher has already gated it to a
// **loopback peer** (`pre_identity_allowlist::requires_loopback_peer`).
//
// **Loopback is the FIRST gate, not the only one.** Post-UID-split
// (slices 1+4) a *compromised* co-resident bridge is also a loopback peer, so
// loopback alone would let it self-enroll a rogue/cross-role identity. When the
// artifact has provisioned a **blessed registry** for the role
// (`blessed_bridge_pubkey`), enrollment additionally requires the presenter to
// *be* the blessed Ed25519 key AND prove possession of it with a signature
// (`check_enrollment_authorization`); the key file is UID-isolated, so a
// co-resident attacker cannot read it to forge the proof. Absent a registry
// (binary-only / dev nest) the loopback-only path stands (deploy-safe).
// `_actor` is the zero placeholder of an anonymous connection. Idempotent:
// re-announcing returns the row's current
// status without clobbering an already approved/revoked row (so a bridge can
// poll this until an admin approves). The admin assigns the authoritative role
// at approval (which validates it against this row's role); `role_hint` only
// seeds the row + approval card, and the bridge's operating role still comes
// from `whoami` post-approval.
fn request_enrollment_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            let req: RequestEnrollmentRequest = decode(&payload).map_err(malformed)?;
            let pk: [u8; 32] =
                crate::rpc_errors::require_bytes32("ed25519_pubkey", req.ed25519_pubkey.as_slice())
                    .map_err(malformed)?;
            use crate::db::bridge_service_users::{BridgeRole, BridgeStatus};
            let role = BridgeRole::parse(&req.role_hint).ok_or_else(|| {
                malformed(format!(
                    "role_hint must be \"mta\", \"mda\", \"content-processor\", or \"atproto.pds\", got {:?}",
                    req.role_hint
                ))
            })?;
            // `as_str()` returns `&'static str`, so `role` stays movable below.
            let role_str = role.as_str();

            // Prove possession of an artifact-blessed key (and bind x25519) before this loopback peer may enroll. Parse the optional
            // signed fields, then authorize against the blessed registry. The
            // policy lives in the pure `check_enrollment_authorization`; here we
            // only read the env-provisioned registry and surface a provisioning
            // gap on a router-fronted box that lacks one.
            let x25519_in: Option<[u8; 32]> = match req.x25519_pubkey.as_ref() {
                Some(b) => Some(
                    crate::rpc_errors::require_bytes32("x25519_pubkey", &b[..])
                        .map_err(malformed)?,
                ),
                None => None,
            };
            let sig_in: Option<&[u8]> = req.enrollment_sig.as_ref().map(|b| &b[..]);
            let blessed = blessed_bridge_pubkey(&role);
            if blessed.is_none() && crate::is_fronted_by_router() {
                tracing::warn!(
                    target: "bridge_rpc",
                    role = role_str,
                    "request_enrollment is UNAUTHENTICATED: no blessed-pubkey registry \
                     provisioned for this role on a router-fronted box (provisioning \
                     gap) — any loopback peer can enroll. Check FAUNA_BLESSED_KEYS_DIR \
                     provisioning."
                );
            }
            let verified_x25519 =
                check_enrollment_authorization(role_str, &pk, x25519_in.as_ref(), sig_in, blessed)?;

            // Onboarding auto-approval (`mail-bridge-lifecycle.md` § Onboarding
            // auto-approval). A mail bridge (MTA/MDA) reaches this loopback-gated
            // surface only as a same-host process inside the deployment trust
            // boundary, so once an admin has enabled the deployment subsystem the
            // bridge serves (Admin-class) the enable IS the approval: the box's
            // own bridge is approved with no manual `approve_pending_bridge`
            // click. Trust anchor = loopback origin (enforced at the dispatch
            // layer) + the Admin-class enable toggle.
            //
            // **Per-role, per-axis** (independent enablement, `caldav-server.md` /
            // `webdav-server.md` § Independent enablement): email (SMTP/IMAP),
            // CalDAV, CardDAV, and WebDAV enable separately, all served by the one
            // MDA bridge. The MDA runs — and thus reaches this enrollment — iff
            // `mail_enabled || caldav_enabled || carddav_enabled || webdav_enabled`
            // (mirrors `mail_enable::mda_should_run`), so a DAV-only box (email
            // never enabled) must auto-approve its MDA on the DAV axis alone —
            // otherwise the MDA stays `pending` forever and calendar/contacts/files
            // sync never serves. The MTA serves only email and only runs when email
            // is on (`mta_should_run` / its s6 run-script), so it never reaches this
            // surface on a DAV-only box and auto-approves on the email axis only.
            // All axes off/unset, or a future non-mail bridge role, still lands
            // `pending` for explicit admin approval. Revoked rows are never
            // resurrected (the existing-row branch returns them as-is below).
            let mail_enabled = matches!(
                state.db.get_mail_enabled().await.map_err(internal)?,
                Some(true)
            );
            let caldav_enabled = matches!(
                state.db.get_caldav_enabled().await.map_err(internal)?,
                Some(true)
            );
            let carddav_enabled = matches!(
                state.db.get_carddav_enabled().await.map_err(internal)?,
                Some(true)
            );
            let webdav_enabled = matches!(
                state.db.get_webdav_enabled().await.map_err(internal)?,
                Some(true)
            );
            let auto_approve = match role {
                BridgeRole::Mta => mail_enabled,
                BridgeRole::Mda => {
                    mail_enabled || caldav_enabled || carddav_enabled || webdav_enabled
                }
                // A content-processor is not a mail bridge (no loopback
                // mail/DAV enable axis gates it), so it never auto-approves —
                // it lands `pending` for explicit admin approval, exactly as the
                // "future non-mail bridge role" note above anticipates.
                BridgeRole::ContentProcessor => false,
                // The ATProto PDS bridge is exactly that anticipated non-mail
                // role: no mail/DAV enable axis gates it, and publishing a
                // user's posts to a public network is a deliberate opt-in, so it
                // always lands `pending` for the manual admin approval card
                // (`mail-bridge-lifecycle.md:169`; enable UX is S4).
                BridgeRole::AtprotoPds => false,
            };

            // Idempotent: if a row already exists (pending / approved / revoked),
            // return its current status without re-creating or clobbering it —
            // this is the bridge's poll surface while it waits for approval. A
            // still-`pending` row self-heals to `approved` here once mail is
            // enabled (covers a bridge that enrolled before the admin flipped
            // mail on).
            if let Some(existing) = state
                .db
                .lookup_bridge_service_user(&pk)
                .await
                .map_err(internal)?
            {
                // Bind (or confirm) the PoP-verified x25519 on the poll path too,
                // so a re-enroll that signed a *different* x25519 is rejected by
                // the freeze. No-op on a revoked row (not resurrected below).
                if let Some(xpk) = verified_x25519 {
                    bind_enrollment_x25519(&state.db, &pk, &xpk).await?;
                }
                let status = if auto_approve && existing.status == BridgeStatus::Pending {
                    state
                        .db
                        .approve_bridge_service_user(&pk, None)
                        .await
                        .map_err(internal)?;
                    tracing::info!(
                        target: "bridge_rpc",
                        actor_prefix = actor_prefix_hex(&pk),
                        role = role.as_str(),
                        "pending bridge auto-approved on poll (deployment mail enabled by admin)"
                    );
                    "approved".to_string()
                } else {
                    existing.status.as_str().to_string()
                };
                return encode_reply(&RequestEnrollmentReply {
                    status,
                    extra: Default::default(),
                });
            }

            // First sight of this pubkey. Synthesize a readable bridge_id when
            // the bridge hasn't resolved one yet (first-boot placeholder), so
            // two same-host pending cards are distinguishable.
            let bridge_id =
                if req.bridge_id.trim().is_empty() || req.bridge_id == "unresolved-bridge" {
                    format!("{}-{}", role_str, &hex::encode(pk)[..8])
                } else {
                    req.bridge_id.clone()
                };
            // Create the pending row. On a concurrent-create race (two enrolls
            // for the same fresh pubkey), the loser's insert errors on the
            // UNIQUE pubkey — re-look-up and return the now-present row's status
            // rather than surfacing a spurious internal error.
            match state
                .db
                .create_pending_bridge_service_user(&pk, role, &bridge_id)
                .await
            {
                Ok(()) => {
                    // Bind the PoP-verified x25519 at the earliest point:
                    // the row now exists, so the set-once freeze records it and
                    // the later `register_service_user` only confirms it.
                    if let Some(xpk) = verified_x25519 {
                        bind_enrollment_x25519(&state.db, &pk, &xpk).await?;
                    }
                    if auto_approve {
                        state
                            .db
                            .approve_bridge_service_user(&pk, None)
                            .await
                            .map_err(internal)?;
                        tracing::info!(
                            target: "bridge_rpc",
                            actor_prefix = actor_prefix_hex(&pk),
                            role = role_str,
                            bridge_id = %bridge_id,
                            "bridge self-enrolled + auto-approved (deployment mail enabled by admin)"
                        );
                        encode_reply(&RequestEnrollmentReply {
                            status: "approved".to_string(),
                            extra: Default::default(),
                        })
                    } else {
                        tracing::info!(
                            target: "bridge_rpc",
                            actor_prefix = actor_prefix_hex(&pk),
                            role = role_str,
                            bridge_id = %bridge_id,
                            "bridge self-enrolled (pending approval)"
                        );
                        encode_reply(&RequestEnrollmentReply {
                            status: "pending".to_string(),
                            extra: Default::default(),
                        })
                    }
                }
                Err(e) => {
                    if let Some(row) = state
                        .db
                        .lookup_bridge_service_user(&pk)
                        .await
                        .map_err(internal)?
                    {
                        encode_reply(&RequestEnrollmentReply {
                            status: row.status.as_str().to_string(),
                            extra: Default::default(),
                        })
                    } else {
                        Err(internal(format!("create pending bridge: {e:#}")))
                    }
                }
            }
        })
    })
}

// Approve an enrolled bridge: pending → approved (idempotent on an already-
// approved row). The caller's actor id is recorded as the approver. The role
// is validated against the enrolled row; approving a revoked bridge is refused
// (it must re-enroll with a fresh key — `mail-bridge-lifecycle.md` § Re-keying).
fn approve_pending_bridge_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.approve_pending_bridge").await?;
            let req: ApprovePendingBridgeRequest = decode(&payload).map_err(malformed)?;
            let pk: [u8; 32] =
                crate::rpc_errors::require_bytes32("ed25519_pubkey", req.ed25519_pubkey.as_slice())
                    .map_err(malformed)?;
            use crate::db::bridge_service_users::{BridgeRole, BridgeStatus};
            let requested_role = BridgeRole::parse(&req.role)
                .ok_or_else(|| malformed(format!("unknown role: {}", req.role)))?;

            let row = state
                .db
                .lookup_bridge_service_user(&pk)
                .await
                .map_err(internal)?
                .ok_or_else(|| not_found("no enrollment row for this pubkey"))?;
            if row.role != requested_role {
                return Err(malformed(format!(
                    "role mismatch: bridge enrolled as {}, approve requested {}",
                    row.role.as_str(),
                    requested_role.as_str()
                )));
            }
            match row.status {
                // Idempotent: re-approving an approved bridge is a no-op success.
                BridgeStatus::Approved => {}
                BridgeStatus::Revoked => {
                    return Err(malformed(
                        "cannot approve a revoked bridge; it must re-enroll with a fresh key",
                    ));
                }
                BridgeStatus::Pending => {
                    state
                        .db
                        .approve_bridge_service_user(&pk, Some(&actor_id))
                        .await
                        .map_err(internal)?;
                }
            }
            encode_reply(&ApprovePendingBridgeReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// Reject / revoke an enrolled bridge: → revoked (idempotent). The row is kept
// with `status=revoked` so a re-connect from the same pubkey rejects
// immediately rather than re-pending (`mail-bridge-lifecycle.md` § Pending
// approval → rejection flow).
fn reject_pending_bridge_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.reject_pending_bridge").await?;
            let req: RejectPendingBridgeRequest = decode(&payload).map_err(malformed)?;
            let pk: [u8; 32] =
                crate::rpc_errors::require_bytes32("ed25519_pubkey", req.ed25519_pubkey.as_slice())
                    .map_err(malformed)?;
            // not_found distinguishes a typo'd pubkey from an idempotent
            // re-reject; revoke itself is idempotent on an already-revoked row.
            if state
                .db
                .lookup_bridge_service_user(&pk)
                .await
                .map_err(internal)?
                .is_none()
            {
                return Err(not_found("no enrollment row for this pubkey"));
            }
            state
                .db
                .revoke_bridge_service_user(&pk)
                .await
                .map_err(internal)?;
            // A genuinely pending bridge has no authenticated socket to
            // close (auth needs the `users` row approval inserts), but this
            // kind also reaches *approved* rows (it converges on the same
            // revoke — comment above), so tear down like any other
            // authority strip (transport.md § Revocation teardown).
            state.revoke_actor_authority(&pk).await;
            encode_reply(&RejectPendingBridgeReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// Revoke an already-approved bridge service user — the running-phase
// "Rotate bridge service-user key" affordance (`mail-bridge-lifecycle.md`
// § Service-user re-keying): the bridge's next `whoami` returns `revoked`, it
// shuts down gracefully, the supervisor restarts it, and it regenerates a fresh
// keypair → a new pending approval. Keyed by `bridge_actor_id` (a bridge
// service user's actor_id is its ed25519 pubkey). The pending-vs-approved phase
// is a UI-affordance distinction (this kind on the Bridges-detail page, `reject`
// on the pending card), not a DB gate — both converge on `revoke_bridge_service_user`,
// which is idempotent on an already-revoked row.
fn revoke_service_user_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.revoke_service_user").await?;
            let req: RevokeServiceUserRequest = decode(&payload).map_err(malformed)?;
            let pk: [u8; 32] = crate::rpc_errors::require_bytes32(
                "bridge_actor_id",
                req.bridge_actor_id.as_slice(),
            )
            .map_err(malformed)?;
            // not_found distinguishes a typo'd actor id from an idempotent
            // re-revoke; revoke itself is idempotent on an already-revoked row.
            if state
                .db
                .lookup_bridge_service_user(&pk)
                .await
                .map_err(internal)?
                .is_none()
            {
                return Err(not_found("no enrollment row for this bridge actor id"));
            }
            // Durable state first. No bridge holds a DKIM key — the nest
            // signs — so a revoke touches no key and no published record,
            // whatever the bridge's role (`mail-bridge-lifecycle.md`
            // § Service-user re-keying).
            state
                .db
                .revoke_bridge_service_user(&pk)
                .await
                .map_err(internal)?;
            // Now cut the bridge's live authority — AFTER the durable commit, so
            // a crash mid-write leaves the socket alive against unchanged state
            // (fully recoverable, nothing to reconcile). A compromise-motivated
            // rotation treats the bridge's keys as leaked, so cut authority now,
            // not at the socket's natural death: revoke the bearer, force-close
            // the WS (4401), and purge its mailbox-state push subscriptions,
            // which would otherwise keep streaming per-user mailbox metadata to
            // the revoked socket (a Push is not an RPC — the capability gate
            // never runs for one). The 4401 also triggers the bridge's reconnect
            // → whoami-denied → graceful-shutdown → re-key cycle immediately
            // (`mail-bridge-lifecycle.md` § Service-user re-keying) instead of
            // waiting for the next natural WS drop. Per transport.md § Revocation
            // teardown. This in-memory cut being un-durable is fine: the revoked
            // DB row is re-read on the next boot, so a restart denies the revoked
            // bridge from the row alone.
            state.revoke_actor_authority(&pk).await;
            encode_reply(&RevokeServiceUserReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// Toggle the deployment-wide mail-enable state. Three writes, in
// authoritative order (Phase E): (1) persist the `mail_enabled` DB toggle —
// the source of truth that `fetch_config` reads and the 60 s reconciliation
// tick re-asserts the flag from; (2) materialize the `{data-dir}/imap-enabled`
// flag file (the durable signal Stage 0's supervisor run-script gates on); and
// (3) best-effort signal the supervisor sidekick socket. The DB write and the
// flag-file write are load-bearing (their errors surface); the socket notify is
// best-effort. Per `mail-bridge-lifecycle.md` § Default-off on first claim.
fn set_mail_enabled_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.set_mail_enabled").await?;
            let req: SetMailEnabledRequest = decode(&payload).map_err(malformed)?;
            // (1) Persist the authoritative DB toggle first — `fetch_config`
            // reads it, and the reconciliation tick treats it as ground truth.
            state
                .db
                .set_mail_enabled(req.enabled)
                .await
                .map_err(|e| internal(format!("persist mail_enabled toggle: {e:#}")))?;
            // (1b) Enable-time mail-domain safety net. A freshly-(re)claimed box
            // can have an empty `mail_domains` table (the claim carried no
            // `mail_domain` — e.g. a re-claim of a factory-reset box), which
            // leaves the bridge's `local_domains` projection and the ACME cert
            // SAN set empty, so the MTA/MDA bind nothing and mail never serves.
            // On enable, auto-provision the nest's own (real) domain as the
            // primary mail domain — works-out-of-the-box. Runs BEFORE the
            // supervisor reconcile below so the row exists when the bridge
            // cold-boots and reads `fetch_config`. Composes with claim-time
            // registration (fires only when nothing is registered) and is gated
            // to a real domain. Per `mail-bridge-lifecycle.md` § Default-off on
            // first claim + `mail-multidomain.md` § The primary domain.
            if req.enabled {
                crate::mail_enable::ensure_primary_mail_domain(&state).await;
                // (1c) Admin canonical-recipient-alias safety net. The admin (box
                // claimer) never travels the per-user
                // `provision_recipient_mls_pubkey` path that writes a regular
                // user's canonical `<handle>@<domain>` exact alias, so ensure it
                // here — else `validate_recipient` (the exact-only AUTH login
                // resolver) rejects the admin's own address `no such recipient`
                // and IMAP/CalDAV/submission login fails. Ordered after the domain
                // net so a servable primary domain exists. Per `mail-aliases.md`
                // § Kind 1 — Exact.
                crate::mail_enable::ensure_admin_recipient_aliases(&state).await;
            }
            match crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path) {
                Some(dir) => {
                    crate::mail_enable::set_mail_enable_flag(&dir, req.enabled).map_err(|e| {
                        internal(format!("write mail-enable flag in {}: {e}", dir.display()))
                    })?;
                }
                None => {
                    tracing::debug!(
                        target: "mail_enable",
                        enabled = req.enabled,
                        "no data dir configured (in-memory / test) — flag-file write skipped"
                    );
                }
            }
            // (3) Reconcile the supervisor. The MTA (SMTP) rides email only; the
            // MDA hosts both IMAP and CalDAV, so its up/down also depends on the
            // independent CalDAV toggle — read it (unset ⇒ follow mail, the
            // legacy unified behavior) so a mail-off with CalDAV-on keeps the MDA
            // up serving calendar. Per `caldav-server.md` § Independent enablement.
            let caldav_enabled = state
                .db
                .get_caldav_enabled()
                .await
                .map_err(internal)?
                .unwrap_or(req.enabled);
            // CardDAV gates independently too (contacts-only keeps the MDA up);
            // unset ⇒ follow mail, matching `fetch_config`'s projection.
            let carddav_enabled = state
                .db
                .get_carddav_enabled()
                .await
                .map_err(internal)?
                .unwrap_or(req.enabled);
            // WebDAV gates independently too (files-only keeps the MDA up);
            // unset ⇒ follow mail, matching `fetch_config`'s projection.
            let webdav_enabled = state
                .db
                .get_webdav_enabled()
                .await
                .map_err(internal)?
                .unwrap_or(req.enabled);
            // The client-set NAT axis (not the boot seed) gates the MTA: a
            // private nest never runs the perimeter parser (`mta_should_run`).
            let node_mode = *state.node_mode.read().await;
            crate::mail_enable::reconcile_supervisor(
                node_mode,
                req.enabled,
                caldav_enabled,
                carddav_enabled,
                webdav_enabled,
            )
            .await;
            // Hot-reload: a running bridge re-fetches `fetch_config` (which now
            // reads the persisted toggle) without waiting for a reconnect. Per
            // `mail-bridge-lifecycle.md` § Running.
            crate::bridge_routing_handlers::notify_bridges_config_changed(
                &state,
                fauna_protocol::bridge_routing::config_change_reason::MAIL_ENABLED,
            )
            .await;
            encode_reply(&SetMailEnabledReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// Toggle the deployment-wide CalDAV-enable state — the calendar twin of
// `set_mail_enabled_handler`. CalDAV gates independently of email (it needs only
// the HTTPS surface, not the full MX stack), but both ride the one MDA bridge.
// Three writes, mirroring the mail handler: (1) persist the `caldav_enabled` DB
// toggle (`fetch_config` reads it; the 60 s reconcile tick re-asserts the flag
// from it); (2) materialize the `{data-dir}/caldav-enabled` flag the MDA's s6
// run-script gates on (alongside `imap-enabled`); (3) reconcile the supervisor
// (the MDA is up iff `mail_enabled || caldav_enabled`). A `config_changed` push
// makes a running MDA re-fetch and, if its listener-gating tuple changed, exit
// cleanly so s6 restarts it bound to the new protocol set. Per
// `docs/goal/behavior/caldav-server.md` § Independent enablement.
fn set_caldav_enabled_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.set_caldav_enabled").await?;
            let req: SetCalDavEnabledRequest = decode(&payload).map_err(malformed)?;
            // (1) Persist the authoritative DB toggle first.
            state
                .db
                .set_caldav_enabled(req.enabled)
                .await
                .map_err(|e| internal(format!("persist caldav_enabled toggle: {e:#}")))?;
            // (2) Materialize the flag file (skipped on the in-memory/test path).
            match crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path) {
                Some(dir) => {
                    crate::mail_enable::set_caldav_enable_flag(&dir, req.enabled).map_err(|e| {
                        internal(format!(
                            "write caldav-enable flag in {}: {e}",
                            dir.display()
                        ))
                    })?;
                }
                None => {
                    tracing::debug!(
                        target: "mail_enable",
                        enabled = req.enabled,
                        "no data dir configured (in-memory / test) — caldav flag-file write skipped"
                    );
                }
            }
            // (3) Reconcile the supervisor: the MDA is up iff email or any DAV
            // axis is enabled. The toggles come from the one reader
            // (`effective_service_toggles`), so an unset mail toggle reads OFF
            // here exactly as it does in `fetch_config` — a DAV-only box must
            // not hand the MTA a live `up` for the internet-facing SMTP parser.
            // CalDAV is overridden with the value just persisted rather than
            // re-read.
            let toggles = crate::mail_enable::effective_service_toggles(&state.db)
                .await
                .map_err(internal)?;
            let node_mode = *state.node_mode.read().await;
            crate::mail_enable::reconcile_supervisor(
                node_mode,
                toggles.mail,
                req.enabled,
                toggles.carddav,
                toggles.webdav,
            )
            .await;
            crate::bridge_routing_handlers::notify_bridges_config_changed(
                &state,
                fauna_protocol::bridge_routing::config_change_reason::CALDAV_ENABLED,
            )
            .await;
            encode_reply(&SetCalDavEnabledReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// Toggle the deployment-wide CardDAV-enable state — the contacts twin of
// `set_caldav_enabled_handler`. CardDAV gates independently of email and CalDAV
// (it needs only the HTTPS surface and rides the SAME DAV listener as CalDAV —
// no separate port), but all three ride the one MDA bridge. Three writes,
// mirroring the CalDAV handler: (1) persist the `carddav_enabled` DB toggle
// (`fetch_config` reads it; the 60 s reconcile tick re-asserts the flag from
// it); (2) materialize the `{data-dir}/carddav-enabled` flag the MDA's s6
// run-script gates on (alongside `imap-enabled` + `caldav-enabled`); (3)
// reconcile the supervisor (the MDA is up iff `mail_enabled || caldav_enabled ||
// carddav_enabled`). A `config_changed` push makes a running MDA re-fetch and,
// if its listener-gating tuple changed, exit cleanly so s6 restarts it bound to
// the new protocol set. See `docs/goal/behavior/carddav-server.md`.
fn set_carddav_enabled_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.set_carddav_enabled").await?;
            let req: SetCardDavEnabledRequest = decode(&payload).map_err(malformed)?;
            // (1) Persist the authoritative DB toggle first.
            state
                .db
                .set_carddav_enabled(req.enabled)
                .await
                .map_err(|e| internal(format!("persist carddav_enabled toggle: {e:#}")))?;
            // (2) Materialize the flag file (skipped on the in-memory/test path).
            match crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path) {
                Some(dir) => {
                    crate::mail_enable::set_carddav_enable_flag(&dir, req.enabled).map_err(
                        |e| {
                            internal(format!(
                                "write carddav-enable flag in {}: {e}",
                                dir.display()
                            ))
                        },
                    )?;
                }
                None => {
                    tracing::debug!(
                        target: "mail_enable",
                        enabled = req.enabled,
                        "no data dir configured (in-memory / test) — carddav flag-file write skipped"
                    );
                }
            }
            // (3) Reconcile the supervisor: the MDA is up iff email or any DAV
            // axis is enabled. The toggles come from the one reader
            // (`effective_service_toggles`), so an unset mail toggle reads OFF
            // here exactly as it does in `fetch_config` — a DAV-only box must
            // not hand the MTA a live `up` for the internet-facing SMTP parser.
            // CardDAV is overridden with the value just persisted rather than
            // re-read.
            let toggles = crate::mail_enable::effective_service_toggles(&state.db)
                .await
                .map_err(internal)?;
            let node_mode = *state.node_mode.read().await;
            crate::mail_enable::reconcile_supervisor(
                node_mode,
                toggles.mail,
                toggles.caldav,
                req.enabled,
                toggles.webdav,
            )
            .await;
            crate::bridge_routing_handlers::notify_bridges_config_changed(
                &state,
                fauna_protocol::bridge_routing::config_change_reason::CARDDAV_ENABLED,
            )
            .await;
            encode_reply(&SetCardDavEnabledReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// Toggle the deployment-wide WebDAV-enable state — the files twin of
// `set_carddav_enabled_handler`. WebDAV gates independently of email, CalDAV,
// and CardDAV (it needs only the HTTPS surface and rides the SAME DAV listener
// as CalDAV/CardDAV — no separate port), but all four ride the one MDA bridge.
// Three writes, mirroring the CardDAV handler: (1) persist the `webdav_enabled`
// DB toggle (`fetch_config` reads it; the 60 s reconcile tick re-asserts the
// flag from it); (2) materialize the `{data-dir}/webdav-enabled` flag the MDA's
// s6 run-script gates on (alongside `imap-enabled` + `caldav-enabled` +
// `carddav-enabled`); (3) reconcile the supervisor (the MDA is up iff
// `mail_enabled || caldav_enabled || carddav_enabled || webdav_enabled`). A
// `config_changed` push makes a running MDA re-fetch and, if its listener-gating
// tuple changed, exit cleanly so s6 restarts it bound to the new protocol set.
// Harmless-on: the deployment toggle exposes nothing until a set is individually
// flagged `folders.webdav_enabled`. Per `docs/goal/behavior/webdav-server.md`
// § Independent enablement.
fn set_webdav_enabled_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.set_webdav_enabled").await?;
            let req: SetWebDavEnabledRequest = decode(&payload).map_err(malformed)?;
            // (1) Persist the authoritative DB toggle first.
            state
                .db
                .set_webdav_enabled(req.enabled)
                .await
                .map_err(|e| internal(format!("persist webdav_enabled toggle: {e:#}")))?;
            // (2) Materialize the flag file (skipped on the in-memory/test path).
            match crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path) {
                Some(dir) => {
                    crate::mail_enable::set_webdav_enable_flag(&dir, req.enabled).map_err(|e| {
                        internal(format!(
                            "write webdav-enable flag in {}: {e}",
                            dir.display()
                        ))
                    })?;
                }
                None => {
                    tracing::debug!(
                        target: "mail_enable",
                        enabled = req.enabled,
                        "no data dir configured (in-memory / test) — webdav flag-file write skipped"
                    );
                }
            }
            // (3) Reconcile the supervisor: the MDA is up iff email or any DAV
            // axis is enabled. The toggles come from the one reader
            // (`effective_service_toggles`), so an unset mail toggle reads OFF
            // here exactly as it does in `fetch_config` — a files-only box must
            // not hand the MTA a live `up` for the internet-facing SMTP parser.
            // WebDAV is overridden with the value just persisted rather than
            // re-read.
            let toggles = crate::mail_enable::effective_service_toggles(&state.db)
                .await
                .map_err(internal)?;
            let node_mode = *state.node_mode.read().await;
            crate::mail_enable::reconcile_supervisor(
                node_mode,
                toggles.mail,
                toggles.caldav,
                toggles.carddav,
                req.enabled,
            )
            .await;
            crate::bridge_routing_handlers::notify_bridges_config_changed(
                &state,
                fauna_protocol::bridge_routing::config_change_reason::WEBDAV_ENABLED,
            )
            .await;
            encode_reply(&SetWebDavEnabledReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// Set the deployment-wide CalDAV listener port (`caldav-server.md` § Network
// exposure — admin-settable port). An admin *choice* — a product
// invariant — so it lands in nest state, not a config file. The port never
// gates whether the MDA *starts* (the s6 run-script / desktop supervisor read
// the `caldav-enabled` flag, not the port), so unlike `set_caldav_enabled` this
// drives NO supervisor *reconcile* (`reconcile_supervisor`). It DOES materialize
// a `/data/caldav-port` value flag-file: on a **desktop** box the supervisor
// can't call `fetch_config` (bridge-enrollment-only), so it reads this flag to
// re-pin the MDA's `caldav_listen_https` operator-hatch and rebind (the Docker
// MDA ignores the flag — it reads `fetch_config.caldav_port` directly). The
// `config_changed` (`CALDAV_PORT`) makes a running Docker MDA re-fetch and, if
// its bound CalDAV port changed, exit cleanly so s6 restarts it on the new port
// (the MDA cannot rebind a listener in-process). Admin-only
// (`bridge_method_allowlist.rs`). Per `caldav-server.md` § Network exposure
// (Desktop / IP deployment).
fn set_caldav_port_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.set_caldav_port").await?;
            let req: SetCaldavPortRequest = decode(&payload).map_err(malformed)?;
            // Port 0 is not a bindable listener port — reject it up front for a
            // clean error rather than tripping the DB CHECK constraint. (u16
            // already caps the upper bound at 65535.)
            if req.port == 0 {
                return Err(malformed("caldav_port must be in 1..=65535 (got 0)"));
            }
            // (1) Persist the authoritative DB singleton first.
            state
                .db
                .set_caldav_port(req.port)
                .await
                .map_err(|e| internal(format!("persist caldav_port: {e:#}")))?;
            // (2) Materialize the desktop-supervisor's value flag (skipped on the
            // in-memory/test path, which has no data dir).
            match crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path) {
                Some(dir) => {
                    crate::mail_enable::set_caldav_port_flag(&dir, req.port).map_err(|e| {
                        internal(format!("write caldav-port flag in {}: {e}", dir.display()))
                    })?;
                }
                None => {
                    tracing::debug!(
                        target: "mail_enable",
                        port = req.port,
                        "no data dir configured (in-memory / test) — caldav-port flag-file write skipped"
                    );
                }
            }
            // (3) Push a `config_changed` so a running Docker MDA re-fetches + rebinds.
            crate::bridge_routing_handlers::notify_bridges_config_changed(
                &state,
                fauna_protocol::bridge_routing::config_change_reason::CALDAV_PORT,
            )
            .await;
            encode_reply(&SetCaldavPortReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// Read the effective admin-set CalDAV listener port — the user-readable read
// twin of `set_caldav_port`. `User | Admin` (`bridge_method_allowlist.rs`): a
// regular user's mail-settings page reads the single non-sensitive port number
// to display the CalDAV connection detail for a third-party calendar app
// (`MuaInstructions.caldav_port`; `caldav-server.md` § Implementation status —
// Client endpoint display). NOT caller-scoped — the port is a nest-wide
// singleton, the same value for every actor. Returns the persisted port, or
// `DEFAULT_CALDAV_PORT` (8443) when unset — the same effective value the MDA
// reads via `fetch_config`.
fn get_caldav_port_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.get_caldav_port").await?;
            let _req: GetCaldavPortRequest = decode(&payload).map_err(malformed)?;
            let port = state
                .db
                .get_caldav_port()
                .await
                .map_err(internal)?
                .unwrap_or(fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT);
            encode_reply(&GetCaldavPortReply {
                port,
                extra: Default::default(),
            })
        })
    })
}

// Set the deployment-wide "auto-enable mail for new users" policy. A single DB
// write — unlike `set_mail_enabled` this drives NO bridge state (no flag file,
// no supervisor reconcile, no `config_changed` push): the bridge never reads
// this knob. It is a client-read deployment default surfaced on
// `fauna.setup.status` (`SetupStatusReply.auto_enable_mail_for_new_users`); the
// client gates its first-setup mailbox auto-mint on it together with
// `email_enabled`, because the nest cannot mint the mailbox itself (the MSEK is
// client-held — `mail-credentials.md` § MSEK lifecycle). Admin-only; a
// deployment default, never a per-user control (`admin.md` § Don't do these).
// Per `mail-policy-config.md` § Tier-2 new-user mail defaults.
fn set_auto_enable_mail_for_new_users_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.set_auto_enable_mail_for_new_users",
            )
            .await?;
            let req: SetAutoEnableMailForNewUsersRequest = decode(&payload).map_err(malformed)?;
            state
                .db
                .set_auto_enable_mail_for_new_users(req.enabled)
                .await
                .map_err(|e| {
                    internal(format!(
                        "persist auto_enable_mail_for_new_users toggle: {e:#}"
                    ))
                })?;
            encode_reply(&SetAutoEnableMailForNewUsersReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// Set the per-actor IMAP/CalDAV-serving opt-out (Slice 2 of the
// home-with-public-relay deployment). **User-class + caller-scoped:** the user
// decides whether THIS nest's MDA serves THEIR mail/calendar to external
// MUAs/CalDAV clients. The flag is keyed on the authenticated `actor_id`, so the
// request carries only `enabled` — a caller can set only their own row, never
// another actor's (product invariant: the user controls where they read their
// mail). Orthogonal to the deployment-wide Admin `set_mail_enabled` /
// `set_caldav_enabled` above (whole-nest, listener binding): one user-set row
// here covers both protocols the MDA hosts. Unlike those handlers there is no
// flag file, no supervisor reconcile, and no `config_changed` push — the
// nest-side serving gates (`require_local_mail_serving` for IMAP, the
// `BridgeMda`-path check in the CalDAV handlers) read the row per request, and
// the Go MDA serves every operation via a fresh nest RPC with no local mail/
// calendar cache, so a flip takes effect on the MUA's next operation. Per
// `docs/goal/architecture/nest/deployment-home-with-public-relay.md`
// § MUA reach.
fn set_mail_serving_enabled_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.set_mail_serving_enabled").await?;
            let req: SetMailServingEnabledRequest = decode(&payload).map_err(malformed)?;
            // Caller-scoped: key on the authenticated actor, never a field.
            state
                .db
                .set_actor_mail_serving_enabled(&actor_id, req.enabled)
                .await
                .map_err(|e| internal(format!("persist actor_mail_serving toggle: {e:#}")))?;
            encode_reply(&SetMailServingEnabledReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// Read the per-actor serving flag (default ON / absent ⇒ ON). A `User` caller
// reads its OWN flag — the request's `actor_id` is ignored (caller-scoped). An
// `Admin` caller reads the actor named by `actor_id` (the **read-only** audit
// view per § MUA reach — admin may see another user's flag but not set it),
// falling back to its own when the field is empty. Per
// `docs/goal/architecture/nest/deployment-home-with-public-relay.md`
// § MUA reach.
fn get_mail_serving_enabled_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class =
                require_class(&state, &actor_id, "fauna.bridges.get_mail_serving_enabled").await?;
            let req: GetMailServingEnabledRequest = decode(&payload).map_err(malformed)?;
            // Only an Admin may name another actor; every other class is forced
            // to its own id regardless of the field (caller-scoped read).
            let target: [u8; 32] = if class == CallerClass::Admin && !req.actor_id.is_empty() {
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?
            } else {
                actor_id
            };
            let enabled = state
                .db
                .get_actor_mail_serving_enabled(&target)
                .await
                .map_err(internal)?
                .unwrap_or(true);
            encode_reply(&GetMailServingEnabledReply {
                enabled,
                extra: Default::default(),
            })
        })
    })
}

fn provision_tls_cert_blob_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.provision_tls_cert_blob").await?;
            let req: ProvisionTlsCertBlobRequest = decode(&payload).map_err(malformed)?;
            if req.blob.len() > crate::db::bridge_blobs::MAX_TLS_CERT_BYTES {
                return Err(malformed(format!(
                    "tls_cert blob too large: {} bytes (max {})",
                    req.blob.len(),
                    crate::db::bridge_blobs::MAX_TLS_CERT_BYTES
                )));
            }
            let blob = fauna_mls::wrapped_blob::format::TlsCertBlob::from_canonical_bytes(
                req.blob.as_ref(),
            )
            .map_err(|e| malformed(format!("tls header: {e}")))?;
            let bridge_role = &blob.index.0;
            let bridge_id = &blob.index.1;
            let domain = &blob.index.2;
            state
                .db
                .put_tls_cert_blob(bridge_role, bridge_id, domain, req.blob.as_ref())
                .await
                .map_err(internal)?;
            encode_reply(&ProvisionReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── Fetch handlers (bridge-originated reads) ──────────────────

fn fetch_wrapped_mls_blob_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &bridge_actor,
                "fauna.bridges.fetch_wrapped_mls_blob",
            )
            .await?;
            let req: FetchWrappedMlsBlobRequest = decode(&payload).map_err(malformed)?;
            let target_actor: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            if !state
                .bridge_rate_limit
                .check(&bridge_actor, &target_actor, &req.credential_id)
            {
                tracing::warn!(
                    target: "bridge_rpc",
                    bridge_prefix = actor_prefix_hex(&bridge_actor),
                    target_prefix = actor_prefix_hex(&target_actor),
                    credential_id = %req.credential_id,
                    "rate-limited (wrapped_mls)"
                );
                return Err(rate_limited());
            }
            let blob = state
                .db
                .get_wrapped_mls_blob(&target_actor, &req.credential_id)
                .await
                .map_err(internal)?
                .map(ByteBuf::from);
            encode_reply(&FetchWrappedMlsBlobReply {
                blob,
                extra: Default::default(),
            })
        })
    })
}

fn fetch_mls_snapshot_blob_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &bridge_actor,
                "fauna.bridges.fetch_mls_snapshot_blob",
            )
            .await?;
            let req: FetchMlsSnapshotBlobRequest = decode(&payload).map_err(malformed)?;
            let target_actor: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            // Snapshot fetch isn't credential-keyed; rate-limit on a
            // synthetic "snapshot" credential to keep the bucket
            // structure uniform.
            if !state
                .bridge_rate_limit
                .check(&bridge_actor, &target_actor, "_snapshot")
            {
                tracing::warn!(
                    target: "bridge_rpc",
                    bridge_prefix = actor_prefix_hex(&bridge_actor),
                    target_prefix = actor_prefix_hex(&target_actor),
                    "rate-limited (mls_snapshot)"
                );
                return Err(rate_limited());
            }
            let blob = state
                .db
                .get_mls_snapshot_blob(&target_actor)
                .await
                .map_err(internal)?
                .map(ByteBuf::from);
            encode_reply(&FetchMlsSnapshotBlobReply {
                blob,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.fetch_webdav_keys_blob` — the MDA, on its authed BridgeMda
/// connection, fetches the actor's opaque WebDAV served-set key blob at AUTH to
/// serve WebDAV (`webdav-server.md` § MDA↔nest WS-RPC contract). The exact
/// sibling of `fetch_mls_snapshot_blob`: not credential-keyed, rate-limited on a
/// synthetic credential to keep the bucket structure uniform; nest returns the
/// blob opaque (`None` if the actor has none provisioned).
fn fetch_webdav_keys_blob_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &bridge_actor,
                "fauna.bridges.fetch_webdav_keys_blob",
            )
            .await?;
            let req: FetchWebdavKeysBlobRequest = decode(&payload).map_err(malformed)?;
            let target_actor: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            if !state
                .bridge_rate_limit
                .check(&bridge_actor, &target_actor, "_webdav_keys")
            {
                tracing::warn!(
                    target: "bridge_rpc",
                    bridge_prefix = actor_prefix_hex(&bridge_actor),
                    target_prefix = actor_prefix_hex(&target_actor),
                    "rate-limited (webdav_keys)"
                );
                return Err(rate_limited());
            }
            let blob = state
                .db
                .get_webdav_keys_blob(&target_actor)
                .await
                .map_err(internal)?
                .map(ByteBuf::from);
            encode_reply(&FetchWebdavKeysBlobReply {
                blob,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.mint_bulk_byte_token` — a bridge, on its authed service-user
/// connection, mints a short-TTL scoped token the chunk/manifest HTTP byte routes
/// accept in place of a client bearer it does not hold (`webdav-server.md`
/// § Bulk-byte plane).
///
/// The token conveys transport authz only (chunks are ciphertext; the content key
/// never leaves the bridge) and lives in a store disjoint from the session
/// `TokenStore`, so it can never be replayed as a full session bearer.
///
/// `purpose` selects the gate — and the gate is the *whole* difference between the
/// two, because both yield a bearer of identical power at the byte layer:
///
/// * `Folder` (**BridgeMda only**) — the enforceable "cross-set" boundary: the
///   named set must exist, be owned by `actor_id`, and be served
///   ([`crate::db::FolderRow::is_webdav_served`]), else `set_not_served`.
/// * `MailBody` (**BridgeMta or BridgeMda**) — a sealed body over the inline budget
///   staging on the byte plane (`smtp-server.md` § Message size limits). Mail
///   belongs to no set, so the gate is that the target is a mail recipient here (it
///   has a recipient seal key — the same fact ingest already fails closed on).
/// * `IndexSegment` — **retired 2026-08-10** with the MDA's index build half
///   (`content-index.md` § Where the index is built — the carrier ruling): the
///   MDA is query-only and blob GET needs no token, so this purpose is refused
///   for every caller. The wire variant stays; the refusal is the retirement.
///
/// An MTA asking for a `Folder` token is denied: admitting it to this kind for
/// mail deliberately did not open the WebDAV path to it.
fn mint_bulk_byte_token_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            let class =
                require_class(&state, &bridge_actor, "fauna.bridges.mint_bulk_byte_token").await?;
            let req: MintBulkByteTokenRequest = decode(&payload).map_err(malformed)?;
            let target_actor: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            if !state
                .bridge_rate_limit
                .check(&bridge_actor, &target_actor, "_bulk_byte")
            {
                tracing::warn!(
                    target: "bridge_rpc",
                    bridge_prefix = actor_prefix_hex(&bridge_actor),
                    target_prefix = actor_prefix_hex(&target_actor),
                    "rate-limited (bulk_byte)"
                );
                return Err(rate_limited());
            }

            // The mint gate. Both purposes yield a token of identical power at the
            // byte layer (the chunk store is global + content-addressed — the token
            // is "not a chunk-hash ACL"), so *this* is where the two differ, and it
            // is the only place they differ.
            let mut served_set_id: Option<i64> = None;
            match req.purpose {
                // WebDAV bytes: BridgeMda only, and only for a set actually served
                // to the actor. `get_folder_for_actor` keys on (name, actor_id),
                // so ownership is implied by a hit. Unchanged from before the MTA
                // was admitted to this kind at all — admitting it did NOT open this
                // path (pinned by `an_mta_may_not_mint_a_folder_token`).
                BulkByteMintPurpose::Folder => {
                    if class != CallerClass::BridgeMda {
                        tracing::warn!(
                            target: "bridge_rpc",
                            bridge_prefix = actor_prefix_hex(&bridge_actor),
                            "mint bulk byte token denied: folder tokens are BridgeMda-only"
                        );
                        return Err(permission_denied("folder bulk tokens are BridgeMda-only"));
                    }
                    let served =
                        webdav_folder_for_actor(&state, &req.folder, &req.name_hash, &target_actor)
                            .await?
                            .filter(|row| row.is_webdav_served());
                    let Some(served) = served else {
                        tracing::warn!(
                            target: "bridge_rpc",
                            bridge_prefix = actor_prefix_hex(&bridge_actor),
                            target_prefix = actor_prefix_hex(&target_actor),
                            folder = %fauna_core::log_redact::log_folder_name(&req.folder),
                            "mint bulk byte token denied: set not served"
                        );
                        return Err(set_not_served());
                    };
                    served_set_id = Some(served.id);
                }
                // A sealed mail body over the inline budget (smtp-server.md
                // § Message size limits). Either bridge may stage one: the MTA for
                // an inbound delivery, the MDA for an IMAP APPEND. Mail has no file
                // set, so the gate is the one property that *is* meaningful here —
                // the target is a mail recipient on this nest, i.e. it has a
                // recipient seal key. That is the same fact the ingest handler
                // already fails closed on, so a token can never be minted toward an
                // actor whose mail could not be sealed anyway.
                BulkByteMintPurpose::MailBody => {
                    let is_recipient = state
                        .db
                        .get_actor_mls_pubkey(&target_actor)
                        .await
                        .map_err(internal)?
                        .is_some();
                    if !is_recipient {
                        tracing::warn!(
                            target: "bridge_rpc",
                            bridge_prefix = actor_prefix_hex(&bridge_actor),
                            target_prefix = actor_prefix_hex(&target_actor),
                            "mint bulk byte token denied: actor is not a mail recipient"
                        );
                        return Err(permission_denied("actor is not a mail recipient"));
                    }
                }
                // Retired 2026-08-10 with the MDA's build half — the MDA is
                // query-only and blob GET needs no token, so nothing is left
                // that may write index bytes from a bridge position
                // (`content-index.md` § Where the index is built — the carrier
                // ruling). The wire VARIANT stays (variants are never removed
                // within a major version); the refusal is what retired it.
                BulkByteMintPurpose::IndexSegment => {
                    tracing::warn!(
                        target: "bridge_rpc",
                        bridge_prefix = actor_prefix_hex(&bridge_actor),
                        "mint bulk byte token denied: the index-segment purpose is retired — \
                         the MDA's index build half was removed (query-only since 2026-08-10)"
                    );
                    return Err(permission_denied(
                        "index-segment bulk tokens are retired — the MDA index leg is query-only",
                    ));
                }
                // A bridge is never a cross-nest folder writer — that purpose is
                // minted solely by the `write_token.mint` federation handler after
                // its foreign-member + writer gate (`federation.md` § Cross-nest…).
                // Refuse it here so a caller-set value can never open the byte
                // plane under this bridge's identity (defense in depth; the byte
                // routes never branch on purpose, so the mint gate is the only
                // place it is spent).
                BulkByteMintPurpose::ForeignFolderWrite => {
                    tracing::warn!(
                        target: "bridge_rpc",
                        bridge_prefix = actor_prefix_hex(&bridge_actor),
                        "mint bulk byte token denied: foreign-folder-write tokens are \
                         federation-minted only, never bridge-minted"
                    );
                    return Err(permission_denied(
                        "foreign-folder-write bulk tokens are not bridge-mintable",
                    ));
                }
                // Identically, a bridge is never a source nest writing an owner's
                // backup custody — that purpose is minted solely by the
                // `backup.write_token.mint` federation handler after its
                // nest-writer-grant gate (`federation.md` § Nest-writer backup
                // plane).
                BulkByteMintPurpose::NestBackupWrite => {
                    tracing::warn!(
                        target: "bridge_rpc",
                        bridge_prefix = actor_prefix_hex(&bridge_actor),
                        "mint bulk byte token denied: nest-backup-write tokens are \
                         federation-minted only, never bridge-minted"
                    );
                    return Err(permission_denied(
                        "nest-backup-write bulk tokens are not bridge-mintable",
                    ));
                }
                // And a bridge is never a cross-nest conversation member — that
                // purpose is minted solely by the
                // `conversation.write_token.mint` federation handler after its
                // foreign-member gate (`conversation-rooms.md` § The home nest
                // → *Attachment bytes*).
                BulkByteMintPurpose::ForeignConversationWrite => {
                    tracing::warn!(
                        target: "bridge_rpc",
                        bridge_prefix = actor_prefix_hex(&bridge_actor),
                        "mint bulk byte token denied: foreign-conversation-write tokens are \
                         federation-minted only, never bridge-minted"
                    );
                    return Err(permission_denied(
                        "foreign-conversation-write bulk tokens are not bridge-mintable",
                    ));
                }
                // Nor a cross-nest folder reader — minted solely by the
                // `read_token.mint` federation handler after its foreign-member
                // gate (`federation.md` § Cross-nest… → *Relay serving across
                // nests*). The chunk route's relay arm reads this purpose, so
                // a bridge-minted one would be a door, not just a mislabel.
                BulkByteMintPurpose::ForeignFolderRead => {
                    tracing::warn!(
                        target: "bridge_rpc",
                        bridge_prefix = actor_prefix_hex(&bridge_actor),
                        "mint bulk byte token denied: foreign-folder-read tokens are \
                         federation-minted only, never bridge-minted"
                    );
                    return Err(permission_denied(
                        "foreign-folder-read bulk tokens are not bridge-mintable",
                    ));
                }
            }

            // The served set's row id, never the caller's name string (the name
            // rests sealed); every other purpose names no set — mail belongs to
            // none, and a foreign-folder-write purpose cannot reach here.
            let folder = served_set_id
                .map(crate::bulk_byte_token::folder_attribution)
                .unwrap_or_default();
            let (token, expires_at) = state
                .auth
                .bulk_byte_tokens
                .mint(
                    fauna_core::identity::ActorId(target_actor),
                    folder,
                    req.access,
                    req.purpose,
                    BULK_BYTE_TOKEN_TTL_SECS,
                )
                .await;
            encode_reply(&MintBulkByteTokenReply {
                token,
                expires_at,
                extra: Default::default(),
            })
        })
    })
}

// ── WebDAV data-plane handlers (`webdav-server.md` § MDA↔nest WS-RPC contract) ──
//
// The three `BridgeMda` kinds the Go MDA WebDAV terminator (slice 4) drives over
// the folder substrate. Thin twins of the User-class `fauna.sync.{files,
// changes.record}` handlers, gated on the set being served (owned +
// `FolderRow::is_webdav_served`) — the same mint gate `mint_bulk_byte_token` uses. There is
// NO per-set read-only source on nest (`FolderRow` has no `read_only`); the
// user's read-only-over-WebDAV preference lives in the MSEK-sealed
// `WebdavKeysBlob` and is enforced authoritatively by the MDA at the PUT
// boundary (Guard 2). No mirror store, ever — these read/write the existing
// `sync_changes` feed (§ Architectural rules).

/// `fauna.bridges.webdav_list_folders` — enumerate the actor's WebDAV-served
/// folders ([`crate::db::FolderRow::is_webdav_served`]). Names only; the MDA reads
/// each set's `read_only` + content keys from the `WebdavKeysBlob` it fetched.
fn webdav_list_folders_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(&state, &bridge_actor, "fauna.bridges.webdav_list_folders").await?;
            let req: WebdavListFoldersRequest = decode(&payload).map_err(malformed)?;
            let target_actor: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            if !state
                .bridge_rate_limit
                .check(&bridge_actor, &target_actor, "_webdav_files")
            {
                return Err(rate_limited());
            }
            let folders = state
                .db
                .get_folders_for_actor_full(&target_actor)
                .await
                .map_err(internal)?
                .into_iter()
                .filter(|r| r.is_webdav_served())
                .map(|r| WebdavServedSet {
                    name: r.name,
                    name_hash: r.name_hash.map(serde_bytes::ByteBuf::from),
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&WebdavListFoldersReply {
                folders,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.webdav_list_files` — one served set's latest-per-path files
/// (the `fauna.sync.files` fold, plus the content-key generation for GET
/// decrypt). Gated on the set being served, else `set_not_served`.
fn webdav_list_files_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(&state, &bridge_actor, "fauna.bridges.webdav_list_files").await?;
            let req: WebdavListFilesRequest = decode(&payload).map_err(malformed)?;
            let target_actor: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            if !state
                .bridge_rate_limit
                .check(&bridge_actor, &target_actor, "_webdav_files")
            {
                return Err(rate_limited());
            }
            let fs = webdav_folder_for_actor(&state, &req.folder, &req.name_hash, &target_actor)
                .await?
                .filter(|row| row.is_webdav_served())
                .ok_or_else(set_not_served)?;
            let files = state
                .db
                .get_files_for_folder(fs.id)
                .await
                .map_err(internal)?
                .into_iter()
                .map(|f| WebdavFile {
                    // "" = the scrub sentinel (S9 flip): the MDA renders the
                    // listing sealed-first from the pair below.
                    path: f.path.unwrap_or_default(),
                    manifest_hash: hex::encode(&f.manifest_hash),
                    size_bytes: f.size_bytes,
                    updated_at: f.updated_at,
                    content_key_version: f.content_key_version.map(|v| v as u64),
                    // The sealed label and the salt it opens under, both
                    // already selected by `get_files_for_folder` (path-sealing
                    // S4). The nest cannot open either — it forwards them so the
                    // MDA can render the listing sealed-first under the
                    // `WebdavKeysBlob` keys it already holds
                    // (`webdav-server.md` § Key model).
                    path_sealed: f.path_sealed.map(fauna_protocol::ByteBuf::from),
                    path_hash: Some(fauna_protocol::ByteBuf::from(f.path_hash)),
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&WebdavListFilesReply {
                files,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.webdav_quota` — the actor's file-storage usage and tier
/// ceiling, for the MDA's RFC 4331 quota properties (`webdav-server.md`
/// § Protocol surface (v1) and deliberate deferrals). Reads the very pair
/// `webdav_record_change` meters against, so the space a file manager reports
/// and the point where a save is refused with `507` never disagree. Its own
/// limiter bucket: file managers ask for quota on every folder refresh, and
/// that polling must not starve the file operations' `_webdav_files` budget.
fn webdav_quota_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(&state, &bridge_actor, "fauna.bridges.webdav_quota").await?;
            let req: WebdavQuotaRequest = decode(&payload).map_err(malformed)?;
            let target_actor: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            if !state
                .bridge_rate_limit
                .check(&bridge_actor, &target_actor, "_webdav_quota")
            {
                return Err(rate_limited());
            }
            let (used, limit) = state
                .db
                .get_user_storage_quota(&target_actor)
                .await
                .map_err(internal)?
                .unwrap_or((0, None));
            encode_reply(&WebdavQuotaReply {
                storage_bytes_used: u64::try_from(used).unwrap_or(0),
                storage_bytes_limit: limit.map(|l| u64::try_from(l).unwrap_or(0)),
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.webdav_record_change` — record a WebDAV write as an ordinary
/// folder change attributed to the actor's stable `"WebDAV"` pseudo-device
/// (registered write-capable, idempotent). Gated on the set being served;
/// enforces the `If-Match` / `If-None-Match` ETag conditional at the nest; meters
/// against the actor's tier storage quota (a WebDAV write IS a sync change).
fn webdav_record_change_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(&state, &bridge_actor, "fauna.bridges.webdav_record_change").await?;
            let req: WebdavRecordChangeRequest = decode(&payload).map_err(malformed)?;
            let target_actor: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            if !state
                .bridge_rate_limit
                .check(&bridge_actor, &target_actor, "_webdav_files")
            {
                return Err(rate_limited());
            }
            let fs = webdav_folder_for_actor(&state, &req.folder, &req.name_hash, &target_actor)
                .await?
                .filter(|row| row.is_webdav_served())
                .ok_or_else(set_not_served)?;

            // The path's routing key — one derivation shared by the ETag head
            // read below and the change record further down, so the conditional
            // and the write it guards address exactly the same row.
            let path_hash: [u8; 32] = fauna_core::sync::path_hash(&req.path);

            // ETag (= manifest hash) conditional, enforced at the write
            // serialization point so a lost race is authoritatively a 412
            // (webdav-server.md § Protocol surface). Best-effort head read; true
            // mutual exclusion (Class-2 LOCK) is deferred.
            let head = state
                .db
                .get_file_head_manifest(fs.id, &path_hash)
                .await
                .map_err(internal)?;
            if req.if_none_match.as_deref() == Some("*") && head.is_some() {
                return Err(conflict("resource exists (If-None-Match: *)"));
            }
            if let Some(m) = req.if_match.as_deref() {
                match &head {
                    None => return Err(conflict("resource absent (If-Match)")),
                    Some(h) if m != "*" && hex::encode(h) != m => {
                        return Err(conflict("ETag mismatch (If-Match)"));
                    }
                    Some(_) => {}
                }
            }

            // Attribute the write to the actor's stable "WebDAV" pseudo-device.
            // Deterministic per-actor id — a client could in principle compute
            // the same value, so `fauna.sync.register` refuses it as a
            // reserved id at the door (`sync_handlers.rs::register_handler`);
            // this internal writer bypasses that door and is the only caller
            // meant to ever hold this row. Registered write-capable
            // idempotently so the normal device-sync / forwarding / conflict
            // machinery treats it as a peer device (webdav-server.md §
            // Protocol surface — Device identity).
            // The label never seals: it is nest-authored and identical on every
            // deployment, one of the three machine-authored labels
            // `file-sync.md` § Sealed names & paths declares non-sealing (and
            // `label_custody::is_synthetic_device_label` refuses centrally). This
            // is also the one register writer with no client behind it at all —
            // the nest holds no key to seal with even for a user-chosen label.
            // The derivation is shared with the device-quota count, which
            // excludes exactly this row (`label_custody::webdav_pseudo_device_id`).
            let device_id: [u8; 32] =
                fauna_core::label_custody::webdav_pseudo_device_id(&target_actor);
            state
                .db
                .register_sync_device(
                    &target_actor,
                    &device_id,
                    fauna_core::label_custody::WEBDAV_PSEUDO_DEVICE_LABEL,
                    None,
                    "read,write",
                )
                .await
                .map_err(internal)?;

            let manifest_hash: Option<[u8; 32]> = match req.manifest_hash.as_deref() {
                Some(h) => Some(
                    fauna_core::hex32::decode(h)
                        .map_err(|_| malformed("manifest_hash must be 32-byte hex"))?,
                ),
                None => None,
            };
            let max_storage = state
                .db
                .get_user_tier_max_storage_bytes(&target_actor)
                .await
                .map_err(internal)?
                .unwrap_or(i64::MAX);

            // THE approved compat break (S9 flip — `encryption-at-rest.md`
            // § Carve-outs): this nest rests no plaintext paths, so a record
            // with no seal has no label anyone can ever render. The MDA is
            // co-deployed with this nest and seals every record
            // (`webdav_seal_path`), so this fires only on a wiring bug.
            if req.path_sealed.is_none() {
                return Err(RpcError::new(
                    "fauna.bridges.path_seal_required",
                    "error.bridges.path_seal_required",
                ));
            }
            // Three more guards of the same kind — each fires only on a wiring
            // bug, and together they make the honest DAV population adoptable
            // by construction (`writer-signed-change-records.md` ruling
            // (7)(b)(i)(3)): a served set is never public, so the MDA seals
            // every write and its label under the set's current generation,
            // records that generation, and records a delete with no manifest.
            let is_delete = req.change_type == "delete";
            if !is_delete && req.content_key_version.is_none() {
                return Err(RpcError::new(
                    RpcError::CODE_BRIDGES_CONTENT_KEY_VERSION_REQUIRED,
                    "error.bridges.content_key_version_required",
                ));
            }
            if is_delete && manifest_hash.is_some() {
                return Err(malformed("a delete record carries no manifest_hash"));
            }
            // The header parses without opening the label, as the snapshot
            // re-stamp's licence check does (`filesync_handlers`).
            let label_generation = req
                .path_sealed
                .as_deref()
                .and_then(|b| fauna_core::path_crypto::SealedLabel::from_bytes(b).ok())
                .and_then(|l| l.generation);
            if label_generation.is_none() {
                return Err(malformed(
                    "path_sealed must be a sealed label naming its content-key generation",
                ));
            }

            let seq = state
                .db
                .record_sync_change_metered(
                    &target_actor,
                    // WebDAV records into the target actor's own set: recorder
                    // == metered owner, no member cap.
                    &target_actor,
                    None,
                    &path_hash,
                    manifest_hash.as_ref(),
                    req.size_bytes,
                    &req.change_type,
                    fs.id,
                    &device_id,
                    // S9 flip: a webdav-served set is a sealed user set — the
                    // plaintext rests NULL and the seal below is the label.
                    None,
                    req.content_key_version.map(|v| v as i64),
                    // WebDAV files carry no sidecar thumbnail.
                    None,
                    // The MDA sealed this bridge-side under the served set's
                    // current content-key generation (`webdav_seal_path`) — the
                    // nest holds no key that could, which is why this leg is
                    // sealed by the bridge rather than here
                    // (`webdav-server.md` § Key model). A sealless record was
                    // refused above (S9 flip): the MDA ships in the same
                    // deployment artifact as this nest, so a pre-S4 bridge here
                    // is a wiring bug, not a compat case.
                    req.path_sealed.as_ref().map(|b| &b[..]),
                    // A WebDAV MUA write holds no catch-up anchor — causality
                    // honestly unknown.
                    None,
                    None,
                    max_storage,
                )
                .await
                .map_err(webdav_quota_err)?;

            encode_reply(&WebdavRecordChangeReply {
                seq,
                extra: Default::default(),
            })
        })
    })
}

fn fetch_wrapped_submission_token_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &bridge_actor,
                "fauna.bridges.fetch_wrapped_submission_token",
            )
            .await?;
            let req: FetchWrappedSubmissionTokenRequest = decode(&payload).map_err(malformed)?;
            let target_actor: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            if !state
                .bridge_rate_limit
                .check(&bridge_actor, &target_actor, &req.credential_id)
            {
                tracing::warn!(
                    target: "bridge_rpc",
                    bridge_prefix = actor_prefix_hex(&bridge_actor),
                    target_prefix = actor_prefix_hex(&target_actor),
                    credential_id = %req.credential_id,
                    "rate-limited (wrapped_submission_token)"
                );
                return Err(rate_limited());
            }
            let blob = state
                .db
                .get_wrapped_submission_token(&target_actor, &req.credential_id)
                .await
                .map_err(internal)?
                .map(ByteBuf::from);
            encode_reply(&FetchWrappedSubmissionTokenReply {
                blob,
                extra: Default::default(),
            })
        })
    })
}

// ── ATProto identity surface (S2 — atproto-pds-bridge.md § State & data shape) ──
//
// The one-shot USER-class `enable_identity` kind that used to live here was
// retired in S4-B: the depth selector's `set_integration_level` transition kind
// (`bridge_atproto_handlers.rs`) is the only enable/level mutation path — a
// standalone enable could re-create a hosted identity without the one-backing
// unlink the transition composes (`docs/goal/ui/atproto.md` § Don't do these).

/// The per-user ATProto identity roster, handles derived at READ time from the
/// current Fauna handle + handle-domain (never stored — a rename re-derives).
/// Rows whose handle can't derive (reserved label, malformed claim-era handle,
/// non-public domain) are skipped with a warn — the bridge never sees them.
fn fetch_atproto_identities_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &bridge_actor,
                "fauna.bridges.atproto.fetch_identities",
            )
            .await?;
            let _req: FetchAtprotoIdentitiesRequest = decode(&payload).map_err(malformed)?;

            let domain = state.handle_domain();
            if !fauna_provisioning::probe::resolve_handle_domain(&domain).is_public_dns_name {
                // A localhost/LAN/IP nest derives no ATProto handles at all.
                return encode_reply(&FetchAtprotoIdentitiesReply { identities: vec![] });
            }
            // The PDS serves at the dedicated `pds.<domain>` subdomain: the SNI
            // router is L4-only and cannot split the apex by path, while XRPC must
            // never terminate in nest (atproto-pds-full.md § Wire & process
            // topology, F1 packaging resolution). This derivation flips in lock-step
            // with the router `pds.*` route + the bridge's `pds.<domain>` TLS
            // termination — never an endpoint nothing serves.
            let pds_endpoint = fauna_bridge_atproto::oauth_metadata::oauth_issuer(
                fauna_bridge_atproto::oauth_metadata::pds_host(&domain),
            );

            let rows = state.db.list_atproto_identities().await.map_err(internal)?;
            let mut identities = Vec::with_capacity(rows.len());
            for row in rows {
                let handle = state
                    .db
                    .get_handle(&row.actor_id)
                    .await
                    .map_err(internal)?
                    .unwrap_or_default();
                let atproto_handle =
                    match fauna_protocol::atproto::derive_atproto_handle(&handle, &domain) {
                        Ok(h) => h,
                        Err(e) => {
                            tracing::warn!(
                                target: "bridge_rpc",
                                actor_prefix = actor_prefix_hex(&row.actor_id),
                                error = %e,
                                "atproto identity skipped: handle does not derive"
                            );
                            continue;
                        }
                    };
                identities.push(AtprotoIdentityView {
                    actor_id: ByteBuf::from(row.actor_id.to_vec()),
                    handle: atproto_handle,
                    method: row.method,
                    status: row.status,
                    did: row.did,
                    user_rotation_pub_did_key: row.user_rotation_pub,
                    signing_pub_did_key: row.signing_pub,
                    bridge_rotation_pub_did_key: row.bridge_rotation_pub,
                    pds_endpoint: pds_endpoint.clone(),
                });
            }
            encode_reply(&FetchAtprotoIdentitiesReply { identities })
        })
    })
}

/// Sealed identity-key fetch with automatic provision-on-read: on a miss, if the caller is the approved
/// `atproto.pds` bridge with an attested x25519 AND an identity row exists for
/// the actor, mint the two bridge-custodied K-256 keys, seal to the bridge,
/// persist blob + pubkeys, and return them. Unlike DKIM this errors (rather
/// than degrading to None) on unmet preconditions — the bridge only calls this
/// for actors its roster read just returned, so a miss is a real fault worth
/// surfacing loudly.
fn fetch_atproto_identity_key_blob_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            use crate::db::bridge_service_users::{BridgeRole, BridgeStatus};

            require_class(
                &state,
                &bridge_actor,
                "fauna.bridges.atproto.fetch_identity_key_blob",
            )
            .await?;
            let req: FetchAtprotoIdentityKeyBlobRequest = decode(&payload).map_err(malformed)?;
            let actor_id: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            if !state
                .bridge_rate_limit
                .check(&bridge_actor, &actor_id, "atproto:identity-key-blob")
            {
                return Err(rate_limited());
            }

            let identity = state
                .db
                .get_atproto_identity(&actor_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| malformed("no atproto identity for actor"))?;

            // A blob beside a row that records no published keys is the torn
            // provision (written as two statements before the pair became one
            // transaction). The bridge binds the blob to those keys and must
            // refuse it, so for an identity with no DID yet — nothing published,
            // nothing to orphan — treat it as unprovisioned and mint afresh. A
            // MINTED identity in that state is left alone: its keys are in a
            // public DID document and re-minting would orphan the DID.
            let stored = state
                .db
                .get_atproto_identity_key_blob(&actor_id)
                .await
                .map_err(internal)?;
            let torn_before_mint = identity.signing_pub.is_none() && identity.did.is_none();
            let blob = match stored.filter(|_| !torn_before_mint) {
                Some(bytes) => bytes,
                None => {
                    // Provision-on-read: seal fresh keys to THIS bridge.
                    let row = state
                        .db
                        .lookup_bridge_service_user(&bridge_actor)
                        .await
                        .map_err(internal)?
                        .ok_or_else(|| permission_denied("unknown bridge"))?;
                    if row.role != BridgeRole::AtprotoPds || row.status != BridgeStatus::Approved {
                        return Err(permission_denied("not an approved atproto.pds bridge"));
                    }
                    let x25519 = row.x25519_pubkey.ok_or_else(|| {
                        permission_denied("bridge has not attested an x25519 pubkey")
                    })?;
                    let keys = fauna_provisioning::atproto::seal_atproto_identity_for_provision(
                        &actor_id, &x25519,
                    )
                    .map_err(internal)?;
                    state
                        .db
                        .provision_atproto_identity_keys(
                            &actor_id,
                            &keys.sealed_blob,
                            &keys.signing_pub_did_key,
                            &keys.rotation_pub_did_key,
                        )
                        .await
                        .map_err(internal)?;
                    tracing::info!(
                        target: "bridge_rpc",
                        actor_prefix = actor_prefix_hex(&actor_id),
                        "auto-provisioned atproto identity keys on first fetch \
                         (sealed to approved atproto.pds bridge)"
                    );
                    keys.sealed_blob
                }
            };

            // Pubkeys come from the row (set at provision; stable thereafter).
            let identity = if identity.signing_pub.is_none() {
                state
                    .db
                    .get_atproto_identity(&actor_id)
                    .await
                    .map_err(internal)?
                    .ok_or_else(|| internal("identity row vanished mid-provision"))?
            } else {
                identity
            };
            encode_reply(&FetchAtprotoIdentityKeyBlobReply {
                blob: ByteBuf::from(blob),
                signing_pub_did_key: identity.signing_pub.unwrap_or_default(),
                bridge_rotation_pub_did_key: identity.bridge_rotation_pub.unwrap_or_default(),
            })
        })
    })
}

/// Bridge-wide sealed HS256 session-secret fetch with provision-on-read
/// (`atproto-pds-full.md` § Key material inventory,
/// bridge-wide row): on a miss, mint 32 random bytes nest-side, seal to the
/// caller's attested x25519, persist **first-write-wins**, and return the
/// stored ciphertext. The request carries no key — the caller's own
/// enrollment `(role, bridge_id)` scopes the secret, so a bridge can only
/// ever fetch (or trigger minting of) its own. Like the identity-blob fetch,
/// unmet preconditions error loudly rather than degrade.
fn fetch_atproto_session_secret_blob_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            use crate::db::bridge_service_users::{BridgeRole, BridgeStatus};

            require_class(
                &state,
                &bridge_actor,
                "fauna.bridges.atproto.fetch_session_secret_blob",
            )
            .await?;
            let _req: FetchAtprotoSessionSecretBlobRequest = decode(&payload).map_err(malformed)?;
            if !state.bridge_rate_limit.check(
                &bridge_actor,
                &[0u8; 32],
                "atproto:session-secret-blob",
            ) {
                return Err(rate_limited());
            }

            let row = state
                .db
                .lookup_bridge_service_user(&bridge_actor)
                .await
                .map_err(internal)?
                .ok_or_else(|| permission_denied("unknown bridge"))?;
            if row.role != BridgeRole::AtprotoPds || row.status != BridgeStatus::Approved {
                return Err(permission_denied("not an approved atproto.pds bridge"));
            }
            let role = row.role.as_str();

            let blob = match state
                .db
                .get_atproto_session_secret_blob(role, &row.bridge_id)
                .await
                .map_err(internal)?
            {
                Some(bytes) => bytes,
                None => {
                    let x25519 = row.x25519_pubkey.ok_or_else(|| {
                        permission_denied("bridge has not attested an x25519 pubkey")
                    })?;
                    let sealed =
                        fauna_provisioning::atproto::seal_atproto_session_secret_for_provision(
                            role,
                            &row.bridge_id,
                            &x25519,
                        )
                        .map_err(internal)?;
                    // First-write-wins: if a concurrent fetch raced us, the
                    // stored (already-served) blob is returned, never ours.
                    let stored = state
                        .db
                        .put_atproto_session_secret_blob_if_absent(role, &row.bridge_id, &sealed)
                        .await
                        .map_err(internal)?;
                    tracing::info!(
                        target: "bridge_rpc",
                        bridge_prefix = actor_prefix_hex(&bridge_actor),
                        bridge_id = %row.bridge_id,
                        "auto-provisioned atproto session-token secret on first fetch \
                         (sealed to approved atproto.pds bridge)"
                    );
                    stored
                }
            };

            encode_reply(&FetchAtprotoSessionSecretBlobReply {
                blob: ByteBuf::from(blob),
            })
        })
    })
}

/// Mint report-back: the bridge submitted the PLC genesis op (or constructed
/// the did:web DID) and nest records the DID as data + provenance. Idempotent
/// on the same DID; a different DID against an active identity is refused —
/// identities are never silently replaced.
fn record_minted_identity_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &bridge_actor,
                "fauna.bridges.atproto.record_minted_identity",
            )
            .await?;
            let req: RecordMintedIdentityRequest = decode(&payload).map_err(malformed)?;
            let actor_id: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            if req.did.is_empty() {
                return Err(malformed("did must be non-empty"));
            }
            state
                .db
                .record_atproto_minted(&actor_id, &req.did, req.genesis_cid.as_deref())
                .await
                .map_err(|e| {
                    let msg = e.to_string();
                    if msg.contains("different DID") || msg.contains("no atproto identity") {
                        malformed(msg)
                    } else {
                        internal(msg)
                    }
                })?;
            tracing::info!(
                target: "bridge_rpc",
                actor_prefix = actor_prefix_hex(&actor_id),
                did = %req.did,
                "atproto identity DID recorded"
            );
            encode_reply(&RecordMintedIdentityReply::default())
        })
    })
}

fn fetch_tls_cert_blob_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(&state, &bridge_actor, "fauna.bridges.fetch_tls_cert_blob").await?;
            let req: FetchTlsCertBlobRequest = decode(&payload).map_err(malformed)?;
            if !state.bridge_rate_limit.check(
                &bridge_actor,
                &[0u8; 32],
                &format!("tls:{}:{}:{}", req.bridge_role, req.bridge_id, req.domain),
            ) {
                tracing::warn!(
                    target: "bridge_rpc",
                    bridge_prefix = actor_prefix_hex(&bridge_actor),
                    bridge_role = %req.bridge_role,
                    bridge_id = %req.bridge_id,
                    domain = %req.domain,
                    "rate-limited (tls)"
                );
                return Err(rate_limited());
            }
            // Authoritative seal-on-read: seal the cert currently on disk
            // (whatever ACME / self-signed last wrote) freshly to this bridge,
            // keyed by the domain it fetches with and its attested x25519. This
            // closes the issuance-vs-attestation ordering gap and the
            // apex-vs-PrimaryDomain key mismatch in one place — the eager
            // `store_acme_material` fan-out is only a pre-seed. Fall back to any
            // previously-stored blob only when there is no cert on disk yet.
            //
            // Seal DIRECTLY from `state.db` + `state.acme_dir`, NOT via
            // `state.storage()`. A bridge's TLS cert is deployment infra — the
            // ACME/floor PEM on disk plus the bridge's x25519 in
            // `bridge_service_users` — never user data, so it MUST NOT depend on
            // the user-data storage mode being committed. Routing it through
            // `state.storage()` coupled the two: on a fresh box that enabled mail
            // before the admin committed a storage mode, `state.storage()` is
            // a `Storage` impl taking the default `seal_current_tls_cert_for_bridge`
            // default returns `Ok(None)`, so the MTA/MDA never obtained a cert and
            // 465/587-STARTTLS/993 served no TLS (`TLSV1_ALERT_INTERNAL_ERROR`)
            // indefinitely — a client-causable broken state (`set_mail_enabled`
            // does not gate on a committed mode; the deferred-storage onboarding
            // path is reachable). Root-caused live 2026-07-05: bridges
            // approved+x25519-attested and the apex cert on `/data/acme`, but
            // no storage mode committed → seal `None` → `bridge_tls_cert_blobs`
            // empty. The
            // `SealedStorage`'s override does exactly this same
            // `seal_current_tls_cert_for_bridge_impl(&self.db, &self.acme_dir, …)`
            // call and nothing mode-specific, so a direct call is byte-identical
            // once a mode IS committed and also works before/without one.
            let blob = match crate::storage::seal_current_tls_cert_for_bridge_impl(
                &state.db,
                &state.acme_dir,
                &req.bridge_role,
                &req.bridge_id,
                &req.domain,
            )
            .await
            .map_err(internal)?
            {
                Some(bytes) => Some(ByteBuf::from(bytes)),
                None => state
                    .db
                    .get_tls_cert_blob(&req.bridge_role, &req.bridge_id, &req.domain)
                    .await
                    .map_err(internal)?
                    .map(ByteBuf::from),
            };
            encode_reply(&FetchTlsCertBlobReply {
                blob,
                extra: Default::default(),
            })
        })
    })
}

fn fetch_bridge_pubkey_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.fetch_bridge_pubkey").await?;
            let req: FetchBridgePubkeyRequest = decode(&payload).map_err(malformed)?;
            let role = match req.bridge_role.as_str() {
                "mta" => crate::db::bridge_service_users::BridgeRole::Mta,
                "mda" => crate::db::bridge_service_users::BridgeRole::Mda,
                // The generic content-processor family — e.g. the nest's own
                // web-serve holder (`bridge_id = "web-serve"`, web paywall
                // Pillar 2); `bridge_id` disambiguates within the family.
                "content-processor" => {
                    crate::db::bridge_service_users::BridgeRole::ContentProcessor
                }
                _ => {
                    return Err(malformed(format!(
                        "unknown bridge_role: {}",
                        req.bridge_role
                    )));
                }
            };
            let row = state
                .db
                .find_approved_bridge_by_role_and_id(role, &req.bridge_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| {
                    let mut e = RpcError::new("fauna.bridges.not_found", "error.bridges.not_found");
                    e.details = Some(Box::new(Value::String(format!(
                        "no approved bridge with role={} bridge_id={}",
                        req.bridge_role, req.bridge_id
                    ))));
                    e
                })?;
            // PQ-CAP-3: the holder's published ML-KEM ek (PQ-CAP-2 stores it on
            // `bridge_service_users.mlkem_ek`), so the client mint can wrap grants
            // X-Wing to `from_parts(mlkem_ek, x25519_pubkey)`. `None` for a
            // classical-only holder → the mint degrades to the classical wrap.
            let mlkem_ek = state
                .db
                .bridge_mlkem_ek(&row.ed25519_pubkey)
                .await
                .map_err(internal)?;
            let x25519 = row.x25519_pubkey.ok_or_else(|| {
                let mut e = RpcError::new("fauna.bridges.not_found", "error.bridges.not_found");
                e.details = Some(Box::new(Value::String(
                    "approved bridge has no x25519 pubkey on record (run register_service_user)"
                        .into(),
                )));
                e
            })?;
            encode_reply(&FetchBridgePubkeyReply {
                ed25519_pubkey: row.ed25519_pubkey.to_vec(),
                x25519_pubkey: x25519.to_vec(),
                mlkem_ek: mlkem_ek.map(ByteBuf::from),
                extra: Default::default(),
            })
        })
    })
}

// ── Sidecar log plane (enrolled-bridge leg) ─────────────────────

/// `fauna.bridges.report_log_events` — an enrolled bridge reports a batch of
/// allowlisted, admin-meaningful events for the admin Logs surface
/// (`observability.md` § The sidecar log plane).
///
/// The **source id is derived from the caller's authenticated class**, never
/// from the payload — that is what makes the resulting `<source>:<event>` ring
/// target an attribution rather than a claim. All policy (sanitization, rate
/// limiting, the remote ring) lives in `crate::log_plane`.
fn report_log_events_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class = require_class(
                &state,
                &actor_id,
                fauna_protocol::log_plane::KIND_BRIDGES_REPORT_LOG_EVENTS,
            )
            .await?;
            let source = match class {
                CallerClass::BridgeMta => crate::log_plane::LogSource::Mta,
                CallerClass::BridgeMda => crate::log_plane::LogSource::Mda,
                CallerClass::BridgeAtprotoPds => crate::log_plane::LogSource::Atproto,
                CallerClass::ContentProcessor => crate::log_plane::LogSource::ContentProcessor,
                // Unreachable: the allowlist arm admits only the four bridge
                // classes. Refuse rather than invent an attribution.
                CallerClass::User
                | CallerClass::Admin
                | CallerClass::Custodian
                | CallerClass::ThirdParty => {
                    return Err(permission_denied("log plane is a bridge-only surface"));
                }
            };
            let req: fauna_protocol::log_plane::ReportLogEventsRequest =
                decode(&payload).map_err(malformed)?;
            crate::log_plane::admit(source, &req);
            // Deliberately unconditional success: the plane is best-effort and
            // must never back-pressure or fail the source's real work.
            encode_reply(&fauna_protocol::log_plane::ReportLogEventsReply::default())
        })
    })
}

// ── Revoke handlers (user-owned blobs) ──────────────────────────

fn revoke_wrapped_mls_blob_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class =
                require_class(&state, &actor_id, "fauna.bridges.revoke_wrapped_mls_blob").await?;
            let req: RevokeWrappedMlsBlobRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            // User can only revoke their own blob; admin can revoke any.
            if matches!(class, CallerClass::User) && target != actor_id {
                return Err(permission_denied("user can only revoke own blob"));
            }
            let _ = state
                .db
                .delete_wrapped_mls_blob(&target, &req.credential_id)
                .await
                .map_err(internal)?;
            encode_reply(&RevokeReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn revoke_wrapped_submission_token_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class = require_class(
                &state,
                &actor_id,
                "fauna.bridges.revoke_wrapped_submission_token",
            )
            .await?;
            let req: RevokeWrappedSubmissionTokenRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            if matches!(class, CallerClass::User) && target != actor_id {
                return Err(permission_denied("user can only revoke own token"));
            }
            let _ = state
                .db
                .delete_wrapped_submission_token(&target, &req.credential_id)
                .await
                .map_err(internal)?;
            encode_reply(&RevokeReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── Bridge self-registration + audit ─────────────────────────

fn register_service_user_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            // The caller_class gate confirms the connecting bridge has
            // at least an enrollment row (pending or approved). The
            // handler binds the x25519 key and reports current status;
            // admin approves out of band via HTTP.
            require_class(&state, &actor_id, "fauna.bridges.register_service_user").await?;
            let req: RegisterServiceUserRequest = decode(&payload).map_err(malformed)?;

            // Self-attestation: the actor on the connection must match
            // the request's ed25519_pubkey.
            if req.ed25519_pubkey.as_slice() != &actor_id[..] {
                return Err(permission_denied(
                    "ed25519_pubkey must match connected actor_id",
                ));
            }

            let xpk: [u8; 32] =
                crate::rpc_errors::require_bytes32("x25519_pubkey", req.x25519_pubkey.as_slice())
                    .map_err(malformed)?;

            // The x25519 binding is set-once. A bridge that already
            // attested a key cannot rebind a *different* one — surface that as
            // a permission-denied rather than a 500.
            let status = state
                .db
                .upsert_bridge_x25519(&actor_id, &xpk)
                .await
                .map_err(|e| {
                    if e.downcast_ref::<crate::db::bridge_service_users::BridgeX25519Frozen>()
                        .is_some()
                    {
                        permission_denied(
                            "x25519 key already attested; rebinding a different key is not permitted",
                        )
                    } else {
                        internal(e)
                    }
                })?
                .ok_or_else(|| permission_denied("no enrollment row for this pubkey"))?;

            // PQ-CAP-2: publish the holder's ML-KEM-768 encapsulation key when the
            // bridge sent one (a classical-only bridge — the ATProto bridge — omits it). Runs only
            // after the x25519 upsert above confirmed a non-revoked enrollment row.
            // Length-gated to the FIPS-203 `ek` size; set-once like x25519 (a
            // changed value from a co-resident attacker is frozen out).
            if let Some(ek) = req.mlkem_ek.as_ref() {
                if ek.len() != fauna_mls::wrapped_blob::MLKEM768_ENCAPS_KEY_LEN {
                    return Err(malformed(format!(
                        "mlkem_ek must be {} bytes (ML-KEM-768), got {}",
                        fauna_mls::wrapped_blob::MLKEM768_ENCAPS_KEY_LEN,
                        ek.len()
                    )));
                }
                state
                    .db
                    .upsert_bridge_mlkem_ek(&actor_id, ek.as_ref())
                    .await
                    .map_err(|e| {
                        if e.downcast_ref::<crate::db::bridge_service_users::BridgeMlkemEkFrozen>()
                            .is_some()
                        {
                            permission_denied(
                                "ML-KEM ek already published; rebinding a different key is not permitted",
                            )
                        } else {
                            internal(e)
                        }
                    })?;
            }

            // Confinement self-probe (security.md § Co-resident process trust
            // boundary → Confinement self-probe): record what the bridge
            // observed about its own sandbox. Absent from a bridge that did not probe (or a hostile
            // one), in which case the row keeps whatever it last held (NULL for a
            // bridge that has never probed) — never cleared, so a missing report does not
            // erase a fact, and `confinement_reported_at` shows it is stale.
            //
            // Bounded, then stored VERBATIM: this is a report from the very
            // process whose misconfiguration it describes, so nest holds the
            // charset/length line rather than trusting the source. Nothing here
            // gates a decision — a failure to record must not fail enrollment,
            // so a DB error is logged and swallowed.
            if let Some(c) = req.confinement.as_ref() {
                let row = crate::db::bridge_service_users::BridgeConfinementRow {
                    uid: c.uid as i64,
                    sealed_store: bound_confinement_token(&c.sealed_store),
                    landlock: bound_confinement_token(&c.landlock),
                    seccomp: bound_confinement_token(&c.seccomp),
                    reported_at: crate::db::now_epoch_millis(),
                };
                if row.sealed_store != "denied" {
                    // The one loud case: this bridge says it can reach — or
                    // cannot rule out reaching — the sealed store it is
                    // supposed to be walled off from. nest's own ring, so it
                    // survives independent of the bridge's log plane.
                    tracing::warn!(
                        target: "bridge_service_users",
                        actor_prefix = hex::encode(&actor_id[..4]),
                        uid = row.uid,
                        sealed_store = %row.sealed_store,
                        landlock = %row.landlock,
                        "bridge reports it is NOT confined from the sealed store — \
                         check the deployment artifact (provisioning diagnostic, \
                         not an attestation)"
                    );
                } else {
                    tracing::debug!(
                        target: "bridge_service_users",
                        actor_prefix = hex::encode(&actor_id[..4]),
                        uid = row.uid,
                        landlock = %row.landlock,
                        seccomp = %row.seccomp,
                        "bridge confinement self-probe recorded"
                    );
                }
                if let Err(e) = state.db.record_bridge_confinement(&actor_id, &row).await {
                    tracing::warn!(
                        target: "bridge_service_users",
                        "failed to record bridge confinement diagnostic: {e:#}"
                    );
                }
            }

            let enrollment_request_id =
                format!("enrollment-{}-{}", hex::encode(actor_id), status.as_str());
            encode_reply(&RegisterServiceUserReply {
                enrollment_request_id,
                extra: Default::default(),
            })
        })
    })
}

fn report_auth_event_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(&state, &bridge_actor, "fauna.bridges.report_auth_event").await?;
            let req: ReportAuthEventRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            if !matches!(req.result.as_str(), "ok" | "fail") {
                return Err(malformed(format!("invalid result: {}", req.result)));
            }
            if req.source_ip.trim().is_empty() {
                return Err(malformed("source_ip must not be empty"));
            }
            state
                .db
                .append_bridge_auth_event(
                    &bridge_actor,
                    &target,
                    &req.credential_id,
                    &req.result,
                    &req.source_ip,
                    req.occurred_at as i64,
                    req.reason.as_deref(),
                )
                .await
                .map_err(internal)?;
            encode_reply(&ReportAuthEventReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ──────────────────────────────────

// ── Capability grants (fauna.capabilities.*) ────────────────────
//
// The user-minted, scope-limited, revocable capability plane — the dual of the
// mail-MDA wrapped-blob plane, but OWNER-minted (a content capability is the
// owner's own data access) rather than Admin-minted (design § Phase 2 Step 2
// § 2.3). The nest stores every `GrantBlob` OPAQUE: the wrapped keys are
// HPKE-sealed to the holder's pubkey, so the nest cannot open them
// (`encryption-at-rest.md` § nest holds no content key). The nest reads only the
// blob header (holder / grant_id / epoch_end) to key the storage columns.

/// Re-sync the web paywall after a grant-plane mutation (mint/renew/revoke) by
/// the owner `actor_id`: (1) refresh the in-process web-serve holder's grant
/// registry, so an in-process revoke bites before the reply lands rather than
/// at the next scheduled fetch (revoke-bites-at-use, the same on-demand-refresh
/// posture the MDA drain takes); (2) re-render the owner's published web
/// content, so a fresh grant materializes the sealed full pages (and a revoke
/// clears them) without waiting for the next publish/template change — the
/// grant plane is the third render trigger next to `fauna.web.publish.{set,unset}`.
/// Best-effort throughout: a failure only delays freshness and must not fail
/// the user's mutation (mirrors the publish handlers' logged best-effort render).
async fn refresh_web_serve_holder(state: &Arc<AppState>, actor_id: &[u8; 32]) {
    if let Some(holder) = &state.web_serve_holder
        && let Err(e) = holder.registry.refresh().await
    {
        tracing::warn!(
            target: "web_serve_holder",
            %e,
            "web-serve holder grant refresh failed after grant mutation"
        );
    }
    if let Some(svc) = &state.web_content_service
        && let Err(e) = svc.render_published_posts(actor_id).await
    {
        tracing::warn!(
            target: "web_serve_holder",
            %e,
            "web re-render after grant mutation failed"
        );
    }
}

/// `fauna.capabilities.mint` — the content-owning user deposits a client-built
/// [`GrantBlob`]. Owner-scoped: the handler enforces `ix.owner == caller`
/// server-side, so a user can mint only over their own content. Idempotent
/// (`put_capability_grant` is `INSERT OR REPLACE` on `(owner, grant_id)`).
fn mint_grant_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.capabilities.mint").await?;
            let req: MintGrantRequest = decode(&payload).map_err(malformed)?;
            if req.grant_blob.len() > crate::db::capability_grants::MAX_CAPABILITY_GRANT_BYTES {
                return Err(malformed(format!(
                    "capability grant blob too large: {} bytes (max {})",
                    req.grant_blob.len(),
                    crate::db::capability_grants::MAX_CAPABILITY_GRANT_BYTES
                )));
            }
            // Parse the client-built GrantBlob header ONLY to extract the storage
            // columns (holder / grant_id / epoch_end); the nest never opens the
            // HPKE-sealed wrapped keys — it stores the blob verbatim as opaque
            // ciphertext.
            //
            // window-ok(mint: no grant authorizes here). The caller is the owner
            // (enforced below), and depositing a grant is deliberately
            // window-agnostic — a post-dated grant is a legitimate thing to
            // store. Its window binds at every site that later authorizes ON it.
            let blob = fauna_mls::wrapped_blob::format::GrantBlob::from_canonical_bytes(
                req.grant_blob.as_ref(),
            )
            .map_err(|e| malformed(format!("grant blob: {e}")))?;
            // Owner-scoping (server-side, § 2.3): a user mints only over their
            // OWN content — the blob's owner MUST be the authenticated caller.
            let owner: [u8; 32] =
                crate::rpc_errors::require_bytes32("grant owner_actor_id", blob.index.0.as_slice())
                    .map_err(malformed)?;
            if owner != actor_id {
                return Err(permission_denied(
                    "grant owner_actor_id must be the calling actor",
                ));
            }
            // The holder is the fetch scope key — it must be a 32-byte x25519
            // pubkey or no holder could ever match it on fetch.
            if blob.holder.len() != 32 {
                return Err(malformed("grant holder must be a 32-byte x25519 pubkey"));
            }
            let grant_id = blob.index.1.clone();
            // epoch_end (u64 on the wire) → the i64 storage column / expiry
            // filter; clamp a never-expires sentinel to i64::MAX.
            let epoch_end = i64::try_from(blob.window.1).unwrap_or(i64::MAX);
            state
                .db
                .put_capability_grant(
                    &owner,
                    &grant_id,
                    blob.holder.as_ref(),
                    epoch_end,
                    req.grant_blob.as_ref(),
                )
                .await
                // A quota rejection is client-actionable ("revoke a grant
                // first"), not a server bug to retry — map it like the sibling
                // oversize-blob guard, not to `internal`.
                .map_err(|e| {
                    match e.downcast_ref::<crate::db::capability_grants::GrantQuotaExceeded>() {
                        Some(q) => malformed(q),
                        None => internal(e),
                    }
                })?;
            refresh_web_serve_holder(&state, &actor_id).await;
            // A fresh grant may make the nest the runner of a delegated task
            // kind — wake the lease runner so it claims without waiting out a
            // heartbeat period (delegation_runner module doc).
            state.delegation_runner_wake.notify_one();
            encode_reply(&MintGrantReply {
                grant_id: ByteBuf::from(grant_id),
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.capabilities.fetch` — a holder (an approved MDA / content-processor)
/// pulls the grants sealed to it. The holder identity is the authenticated
/// caller (no field to spoof); the handler resolves the caller's enrolled
/// x25519 and serves only grants whose `holder` matches, omitting
/// expired/revoked ones (that omission is how honest-box revocation bites).
fn fetch_grants_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.capabilities.fetch").await?;
            let _req: FetchGrantsRequest = decode(&payload).map_err(malformed)?;
            // Resolve the caller's enrolled x25519 (the fetch scope key). The
            // gate already restricts the class to BridgeMda | ContentProcessor,
            // so an enrolled approved row exists; the None arms are defensive.
            let row = state
                .db
                .lookup_bridge_service_user(&actor_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| permission_denied("caller is not an enrolled service user"))?;
            let holder = row
                .x25519_pubkey
                .ok_or_else(|| permission_denied("caller has no x25519 pubkey on record"))?;
            let now = crate::db::now_epoch_secs();
            let grants = state
                .db
                .fetch_capability_grants_for_holder(&holder, now)
                .await
                .map_err(internal)?;
            encode_reply(&FetchGrantsReply {
                grants: grants.into_iter().map(ByteBuf::from).collect(),
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.capabilities.fetch` for a third-party **principal** — a session
/// kind (`authorization-server.md` § Scope grammar → *Session kinds need no
/// scope*), self-scoped to the principal row's own `holder_x25519` and to the
/// account the session belongs to. A principal that attested no key reads an
/// empty list, not an error: it holds nothing yet, and asking is not a fault.
pub(crate) fn principal_fetch_grants_handler() -> crate::principal_handlers::PrincipalHandler {
    Box::new(|state, caller, payload| {
        Box::pin(async move {
            let _req: FetchGrantsRequest = decode(&payload).map_err(malformed)?;
            let grants = match caller.holder_x25519 {
                None => Vec::new(),
                Some(holder) => state
                    .db
                    .fetch_capability_grants_for_holder_owned_by(
                        &caller.account,
                        &holder,
                        crate::db::now_epoch_secs(),
                    )
                    .await
                    .map_err(internal)?,
            };
            encode_reply(&FetchGrantsReply {
                grants: grants.into_iter().map(ByteBuf::from).collect(),
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.capabilities.renew` — the owner extends a grant's window and, for an
/// epoch-sealed kind, appends the next window's client-minted wrapped keys
/// (empty for a master-key grant = a window bump only; design § 2.3). Read-
/// modify-write, owner-scoped: the grant is fetched by `(caller, grant_id)`, so
/// a caller can only renew its OWN grant. Idempotent — the append dedups by
/// `(scope, epoch)`, so a retried renew never double-appends a key.
fn renew_grant_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.capabilities.renew").await?;
            let req: RenewGrantRequest = decode(&payload).map_err(malformed)?;
            let existing = state
                .db
                .get_capability_grant(&actor_id, req.grant_id.as_ref())
                .await
                .map_err(internal)?
                .ok_or_else(|| not_found("no such grant for this owner"))?;
            // window-ok(owner-scoped mutation). The row is fetched under the
            // authenticated caller's own owner id, so nothing is authorized BY
            // this grant — the owner is editing their own. Renewing a
            // not-yet-open window is legitimate.
            let mut blob =
                fauna_mls::wrapped_blob::format::GrantBlob::from_canonical_bytes(&existing)
                    .map_err(|e| internal(format!("stored grant blob decode: {e}")))?;
            // Narrowing a window is revocation's job — a renew that shrinks
            // `epoch_end` would let the crypto bound exceed a window the
            // owner tried to tighten (append-only wraps outlive the shrink).
            if req.new_epoch_end < blob.window.1 {
                return Err(malformed(
                    "renew cannot shrink a grant's window — narrowing is revocation's job",
                ));
            }
            let old_epoch_end = blob.window.1;
            blob.window.1 = req.new_epoch_end;
            // The window SLIDES (the retention ruling, `encryption-at-rest.md`
            // § Capability tiering → *Content-sealing epochs*): the client
            // re-centres the start on the renewal instant and the wraps below
            // it are pruned after the merge. The start only ever moves
            // forward — moving it back would widen the grant into epochs
            // this renew carries no wraps for — and never past the end.
            if let Some(new_start) = req.new_epoch_start {
                if new_start < blob.window.0 {
                    return Err(malformed(
                        "renew cannot move a grant's window start backward — a renewal only \
                         slides the window forward",
                    ));
                }
                if new_start > req.new_epoch_end {
                    return Err(malformed(
                        "renew window start is past its end — a window is [start, end]",
                    ));
                }
                blob.window.0 = new_start;
            }
            for wk_bytes in &req.appended_keys {
                let wk = fauna_mls::wrapped_blob::format::WrappedScopeKey::from_canonical_bytes(
                    wk_bytes.as_ref(),
                )
                .map_err(|e| malformed(format!("appended wrapped key: {e}")))?;
                // A grant carries at most one key per (scope, epoch). Same
                // bytes as the stored entry ⇒ a harmless idempotent retry
                // (no-op). Different bytes ⇒ REPLACE it — after an MSEK
                // hard-revoke the client re-derives + re-wraps under the new
                // root for epochs it already held, and a same-root-only dedup
                // would otherwise silently skip the very wraps that need
                // healing.
                match blob
                    .wrapped_keys
                    .iter_mut()
                    .find(|k| k.scope == wk.scope && k.epoch == wk.epoch)
                {
                    Some(existing_wk) => {
                        let existing_bytes = existing_wk.to_canonical_bytes().map_err(|e| {
                            internal(format!("existing wrapped key re-encode: {e}"))
                        })?;
                        if existing_bytes != wk_bytes.as_ref() {
                            *existing_wk = wk;
                        }
                    }
                    None => blob.wrapped_keys.push(wk),
                }
            }
            // INFO-C/INFO-E: refuse a regime-crossing append — a wall-clock-
            // epoch scope (mail/calendar) must never end up mixing a standing
            // (epoch: None) wrap with per-epoch (epoch: Some) wraps. The
            // mint-side XOR guard is per-call and can't see this; the
            // (scope, epoch) dedup above can't either (`None` never matches
            // `Some`).
            if fauna_mls::wrapped_blob::wall_clock_epoch_regime_conflict(&blob.wrapped_keys) {
                return Err(malformed(
                    "renew would mix a standing key with per-epoch keys for a mail/calendar \
                     scope — a grant is bounded (per-epoch only) XOR master-key, never both",
                ));
            }
            // Retention: the merged set keeps only the epochs the (possibly
            // slid) window still covers. Run on every renew — a request that
            // carries no start prunes nothing new, and an appended wrap for
            // an epoch below the start is dropped rather than refused (an
            // idempotent retry of an older renewal must stay harmless).
            let pruned = fauna_mls::wrapped_blob::prune_wraps_below_window_start(
                &mut blob.wrapped_keys,
                blob.window.0,
            );
            if pruned > 0 {
                tracing::debug!(
                    pruned,
                    retained = blob.wrapped_keys.len(),
                    "capabilities.renew: pruned per-epoch wraps below the slid window start"
                );
            }
            // A bounded grant's window is never wider than its wraps: a renew
            // that moves a per-epoch scope's end across a sealing-epoch
            // boundary carries a wrap for every epoch it newly covers, or the
            // holder would fetch a window whose tail no key opens — a keyless
            // bump leaving the extension silently dark (`encryption-at-rest.md`
            // § Capability tiering → *Content-sealing epochs*).
            if let Some((scope, epoch)) = fauna_mls::wrapped_blob::uncovered_bounded_extension(
                &blob.wrapped_keys,
                old_epoch_end,
                req.new_epoch_end,
            ) {
                return Err(malformed(format!(
                    "renew extends a bounded {} grant past its keys — sealing epoch {epoch} has \
                     no wrap; a bounded renewal carries a wrap for every epoch it newly covers",
                    scope.kind.as_deref().unwrap_or("wall-clock")
                )));
            }
            let reblob = blob
                .to_canonical_bytes()
                .map_err(|e| internal(format!("grant blob re-encode: {e}")))?;
            if reblob.len() > crate::db::capability_grants::MAX_CAPABILITY_GRANT_BYTES {
                return Err(malformed(format!(
                    "capability grant blob too large after renew: {} bytes (max {})",
                    reblob.len(),
                    crate::db::capability_grants::MAX_CAPABILITY_GRANT_BYTES
                )));
            }
            let epoch_end = i64::try_from(req.new_epoch_end).unwrap_or(i64::MAX);
            state
                .db
                .put_capability_grant(
                    &actor_id,
                    req.grant_id.as_ref(),
                    blob.holder.as_ref(),
                    epoch_end,
                    &reblob,
                )
                .await
                .map_err(internal)?;
            refresh_web_serve_holder(&state, &actor_id).await;
            state.delegation_runner_wake.notify_one();
            encode_reply(&RenewGrantReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.capabilities.revoke` — the owner deletes a `(caller, grant_id)` row so
/// the holder's next `fetch` returns nothing and it goes dark (honest box).
/// Owner-scoped (the PK is caller-keyed) and idempotent (revoking an absent
/// grant still replies `{ ok: true }`).
fn revoke_grant_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.capabilities.revoke").await?;
            let req: RevokeGrantRequest = decode(&payload).map_err(malformed)?;
            // Snapshot the blob before the delete: a grant that carried the
            // keyless `content.read{spam-model}` scope has a paired
            // sealed-to-holder model copy resting beside `spam_models`, and
            // revocation deletes that artifact too (`mail-spam.md`
            // § Encrypted-mode interaction: "toggle-OFF / grant revoke /
            // model reset deletes it"). Scope tuples are cleartext grant
            // metadata, so this parse opens nothing.
            let revoked_blob = state
                .db
                .get_capability_grant(&actor_id, req.grant_id.as_ref())
                .await
                .map_err(internal)?;
            let _ = state
                .db
                .delete_capability_grant(&actor_id, req.grant_id.as_ref())
                .await
                .map_err(internal)?;
            // window-ok(owner-scoped teardown). The row was just deleted under
            // the caller's own owner id; this decode only asks what cleanup the
            // dead grant owes, and a revoked grant's window is moot.
            if let Some(bytes) = revoked_blob
                && let Ok(blob) =
                    fauna_mls::wrapped_blob::format::GrantBlob::from_canonical_bytes(&bytes)
                && blob.scope.iter().any(|sc| {
                    sc.class == fauna_mls::wrapped_blob::ScopeTuple::CLASS_CONTENT_READ
                        && sc.kind.as_deref()
                            == Some(fauna_mls::wrapped_blob::ScopeTuple::KIND_SPAM_MODEL)
                })
            {
                // Revoking the read the counts were summed under is a
                // departure from the baseline like opt-out, reset and deletion
                // (`mail-spam.md` § Cold start Path 2 → *A contributor's
                // departure withdraws the baseline*). Keyed on the inclusion
                // record alone, so a summed plaintext-row contributor — whom the
                // publish merges without this grant — withdraws too:
                // over-withdrawal is the safe direction.
                state
                    .db
                    .withdraw_spam_baseline_if_contributor(&actor_id)
                    .await
                    .map_err(internal)?;
                state
                    .db
                    .delete_spam_model_holder_copy_for_holder(&actor_id, blob.holder.as_slice())
                    .await
                    .map_err(internal)?;
            }
            refresh_web_serve_holder(&state, &actor_id).await;
            // A revoke may take the nest off a delegated kind — wake the lease
            // runner so the release (and its `lease_changed` push) is prompt.
            state.delegation_runner_wake.notify_one();
            encode_reply(&RevokeGrantReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.capabilities.reconcile` — the one admissible owner-side nest read
/// (`ui/nests.md` § Trust facet — grants → *Reconcile*, ratified 2026-08-15).
/// Returns every grant id the caller (the owner) holds on this nest — ALL
/// rows, expired included — and NOTHING else: no scope, holder, or window,
/// because a `GrantBlob` carries no owner signature and any richer field
/// would be unverifiable nest-authored display data (the same reason the
/// audit view is never a nest read, `ui/nests.md:102`). The client's own
/// revoke-the-unrecognized sweep is the only consumer; this handler does not
/// judge liveness — that stays the log's business.
fn reconcile_grants_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.capabilities.reconcile").await?;
            let _req: ReconcileGrantsRequest = decode(&payload).map_err(malformed)?;
            let grant_ids = state
                .db
                .fetch_capability_grant_ids_for_owner(&actor_id)
                .await
                .map_err(internal)?;
            encode_reply(&ReconcileGrantsReply {
                grant_ids: grant_ids.into_iter().map(ByteBuf::from).collect(),
                extra: Default::default(),
            })
        })
    })
}

// The re-score drain plane (design § 2.5 step 4). Two holder-gated RPCs let a
// capability holder drain the versioned re-score obligation WITHOUT the nest
// reading content: the nest serves a content-free worklist scoped to the
// holder's grants, and accepts a content-free `ScoreEntry` write-back. The
// unseal + re-run happens off-box at the holder (`encryption-at-rest.md:256`,
// `key-material-hierarchy.md` rule #4).

/// Server cap on a single `rescore_worklist` batch (the holder loops until empty
/// — a full batch means "more remain"). Bounds one call's metadata fan-out.
const MAX_RESCORE_WORKLIST: u32 = 512;
/// Server cap on `submit_scores` rows per call (a compromised holder can't
/// flood the write path in one request; the drain batches its write-backs).
const MAX_SUBMIT_SCORE_ROWS: usize = 512;

/// Resolve the authenticated holder's LIVE grants into a flat `(owner_actor_id,
/// ScopeTuple)` list — the auditable scope surface the drain-plane handlers and
/// `label_handlers::authorize_attach` gate on (one resolver, so "what has this
/// holder been granted" cannot come to mean two things).
/// The holder identity is the authenticated caller (no field to spoof);
/// only non-expired grants are returned (`fetch_capability_grants_for_holder`
/// applies the honest-box expiry filter, so a revoked/expired grant grants no
/// work and no write). A malformed stored blob is skipped defensively (the nest
/// stored opaque bytes a client minted; it never opened them). The caller filters
/// by `class`: `content.read` (which owners' content the holder may UNSEAL, for
/// the worklist) vs `content.label-write` (which owners' scores it may WRITE).
pub(crate) async fn holder_granted_scopes(
    state: &Arc<AppState>,
    actor_id: &[u8; 32],
) -> Result<Vec<([u8; 32], fauna_mls::wrapped_blob::format::ScopeTuple)>, RpcError> {
    let row = state
        .db
        .lookup_bridge_service_user(actor_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| permission_denied("caller is not an enrolled service user"))?;
    let holder = row
        .x25519_pubkey
        .ok_or_else(|| permission_denied("caller has no x25519 pubkey on record"))?;
    let now = crate::db::now_epoch_secs();
    let grants = state
        .db
        .fetch_capability_grants_for_holder(&holder, now)
        .await
        .map_err(internal)?;
    let mut out = Vec::new();
    for blob_bytes in &grants {
        let Ok(blob) = fauna_mls::wrapped_blob::format::GrantBlob::from_canonical_bytes(blob_bytes)
        else {
            continue;
        };
        // The whole window, not just expiry — the storage filter cannot express
        // "already started"; a grant reported as held before its
        // window opens is a capability the holder does not yet have.
        if !fauna_mls::wrapped_blob::grant_window_is_open(&blob, now) {
            continue;
        }
        let Ok(owner): Result<[u8; 32], _> = blob.index.0.as_slice().try_into() else {
            continue;
        };
        for scope in blob.scope {
            out.push((owner, scope));
        }
    }
    Ok(out)
}

/// The `ScopeTuple::factor` license a bus factor needs: a community labeler's
/// `labeler:<hex>` factor needs a tuple naming exactly it; every built-in
/// factor (the perimeter scanners, `spam`, …) needs a factor-less tuple. The
/// one mapping the worklist and `submit_scores` gates share.
fn labeler_license_for(factor: &str) -> Option<&str> {
    fauna_core::scoring::is_labeler_factor(factor).then_some(factor)
}

/// `fauna.capabilities.rescore_worklist` — a holder asks "what re-processing do I
/// owe?" The nest intersects the holder's `content.read{kind}` grants with the
/// per-factor obligation gap (`content_scores_behind_for_owner` × the current
/// `model_versions`) and returns only work-units for owners+kinds the holder can
/// actually unseal — never leaking which *other* users have stale content (a
/// metadata-confidentiality boundary). Content-free: only ids/versions cross.
/// A `labeler:<hex>` obligation is owed only to a holder whose read tuple is
/// licensed for that labeler (`ScopeTuple::factor`).
fn rescore_worklist_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.capabilities.rescore_worklist").await?;
            let req: RescoreWorklistRequest = decode(&payload).map_err(malformed)?;
            let limit = if req.limit == 0 || req.limit > MAX_RESCORE_WORKLIST {
                MAX_RESCORE_WORKLIST
            } else {
                req.limit
            } as usize;

            // The (owner, kind, licensed factor) triples this holder may
            // UNSEAL (a `content.read` scope tuple per grant) — the only
            // owners whose stale rows may be revealed. The third element is
            // the tuple's per-factor license (`ScopeTuple::factor`): `None`
            // licenses the built-in perimeter factors, `Some(labeler:<hex>)`
            // exactly that community labeler — so a `labeler:` obligation is
            // surfaced only to a holder the owner granted THAT labeler, never
            // under the composed "read and filter my mail" grant. The holder enforces the same rule on its own key
            // set; this is the honest-box half. Deduped.
            let scopes = holder_granted_scopes(&state, &actor_id).await?;
            let mut readable: Vec<([u8; 32], String, Option<String>)> = Vec::new();
            for (owner, sc) in &scopes {
                if sc.class == "content.read"
                    && let Some(kind) = &sc.kind
                {
                    let triple = (*owner, kind.clone(), sc.factor.clone());
                    if !readable.contains(&triple) {
                        readable.push(triple);
                    }
                }
            }

            // Slice-6 lease gate (delegation_runner module doc): an owner's
            // re-score obligations are admitted only while this nest may claim
            // (or already holds) that owner's `content-rescore` lease — never
            // while a fresh foreign holder runs the kind — and only while the
            // box's grant set is actually sufficient to run it (read +
            // label-write). Claiming inline also closes the mint→drain race:
            // a drain run that lands before the lease runner's next pass
            // acquires the free lease right here. The nest-side analogue of
            // the client `LeaseCoordinator`'s `with_lease_gate`.
            {
                use crate::delegation_runner::{
                    claim, content_rescore_sufficient_owners, may_claim,
                };
                use fauna_core::delegation::KIND_CONTENT_RESCORE;
                let sufficient: std::collections::HashSet<[u8; 32]> =
                    content_rescore_sufficient_owners(&state)
                        .await
                        .map_err(internal)?
                        .into_iter()
                        .collect();
                let mut admitted: std::collections::HashMap<[u8; 32], bool> =
                    std::collections::HashMap::new();
                for (owner, _, _) in &readable {
                    admitted.entry(*owner).or_insert_with(|| {
                        sufficient.contains(owner)
                            && may_claim(&state, *owner, KIND_CONTENT_RESCORE)
                    });
                }
                readable.retain(|(owner, _, _)| admitted.get(owner).copied().unwrap_or(false));
                for (owner, ok) in &admitted {
                    if *ok {
                        claim(&state, *owner, KIND_CONTENT_RESCORE);
                    }
                }
            }

            let versions = state.db.list_model_versions().await.map_err(internal)?;
            let mut units: Vec<RescoreUnit> = Vec::new();
            'outer: for (factor, current) in &versions {
                if *current == 0 {
                    continue;
                }
                let license = labeler_license_for(factor);
                for (owner, kind, licensed) in &readable {
                    if licensed.as_deref() != license {
                        continue;
                    }
                    if units.len() >= limit {
                        break 'outer;
                    }
                    let remaining = limit - units.len();
                    let stale = state
                        .db
                        .content_scores_behind_for_owner(factor, owner, *current, remaining)
                        .await
                        .map_err(internal)?;
                    for sc in stale {
                        // A content.read{mail} grant must not surface a
                        // different-kind row for the same owner (the scan is
                        // factor+owner-scoped, not kind-scoped).
                        if &sc.content_kind != kind {
                            continue;
                        }
                        units.push(RescoreUnit {
                            content_id: ByteBuf::from(sc.content_id),
                            content_kind: sc.content_kind,
                            owner_actor_id: ByteBuf::from(owner.to_vec()),
                            factor: factor.clone(),
                            from_version: sc.scorer_version,
                            to_version: *current,
                            extra: Default::default(),
                        });
                        if units.len() >= limit {
                            break 'outer;
                        }
                    }
                }
            }
            // The decision is now made: `units` is what this holder may work
            // on, given the grants and obligations as of this instant. Counting
            // HERE — after the intersection, before the reply — is what lets an
            // e2e prove a drain saw the post-plant world without waiting out a
            // settle window (`crate::rescore_drain_test_hook`). Never compiled
            // into production.
            #[cfg(feature = "test-hooks")]
            state.rescore_worklist_serves.note_served(units.len());

            encode_reply(&RescoreWorklistReply {
                units,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.capabilities.submit_scores` — a holder writes back re-computed scores
/// after draining a worklist. The nest UPSERTs each row's `ScoreEntry`s into
/// `content_scores` (INSERT OR REPLACE; the re-score's bumped `scorer_version`
/// closes the obligation gap). Content-free (only score metadata crosses; the
/// nest never sees the plaintext it was derived from). Each row is authz'd
/// against the holder's `content.label-write` grant for its `owner_actor_id`; an
/// unauthorized row fails the whole batch (fail-closed, no partial write).
fn submit_scores_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.capabilities.submit_scores").await?;
            let req: SubmitScoresRequest = decode(&payload).map_err(malformed)?;
            if req.rows.len() > MAX_SUBMIT_SCORE_ROWS {
                return Err(malformed(format!(
                    "submit_scores batch too large: {} rows (max {MAX_SUBMIT_SCORE_ROWS})",
                    req.rows.len()
                )));
            }

            // The (owner, kind?, factor?) set this holder may WRITE scores
            // for — a `content.label-write` scope tuple (kind `None` = any
            // kind; the tuple's `factor` licenses the built-in factors when
            // `None`, or exactly one `labeler:<hex>` factor — the nest-side
            // twin of the worklist's per-labeler gate: a
            // holder that was granted only the composed role cannot land a
            // community labeler's row, and a labeler's holder only its own).
            let scopes = holder_granted_scopes(&state, &actor_id).await?;
            let may_write = |owner: &[u8; 32], kind: &str, factor: &str| -> bool {
                let license = labeler_license_for(factor);
                scopes.iter().any(|(o, sc)| {
                    o == owner
                        && sc.class == "content.label-write"
                        && (sc.kind.is_none() || sc.kind.as_deref() == Some(kind))
                        && sc.licenses_factor(license)
                })
            };

            // Validate + authz EVERY row before writing ANY (fail-closed).
            // Authorization binds to the content's TRUE owner — the `actor_id`
            // stored on its existing `content_scores` row — NOT the
            // holder-claimed `row.owner_actor_id`. Otherwise a holder with
            // label-write over owner A could overwrite + re-attribute owner B's
            // row by *claiming* A. `submit_scores` only re-scores already-ingested
            // content, so a `content_id` with no prior owner-attributed row is
            // rejected (the claimed `owner_actor_id` wire field is now ignored).
            // The same holds for the kind: the claimed `content_kind` must equal
            // the stored row's kind, else a `label-write{K1}` grant would reach
            // the owner's K2 rows by claiming K1 and relabel them out of the K2
            // worklist bucket. The legitimate drain echoes
            // the worklist's stored kind, so it always matches.
            let mut prepared: Vec<([u8; 32], [u8; 32])> = Vec::with_capacity(req.rows.len());
            for row in &req.rows {
                let cid: [u8; 32] =
                    crate::rpc_errors::require_bytes32("content_id", row.content_id.as_ref())
                        .map_err(malformed)?;
                let (owner, stored_kind) = state
                    .db
                    .content_score_owner_and_kind(&cid)
                    .await
                    .map_err(internal)?
                    .ok_or_else(|| {
                        permission_denied(
                            "submit_scores for content with no prior owner-attributed score row",
                        )
                    })?;
                if stored_kind != row.content_kind {
                    return Err(permission_denied(
                        "submit_scores content_kind differs from the stored row's kind",
                    ));
                }
                if row
                    .entries
                    .iter()
                    .any(|e| !may_write(&owner, &row.content_kind, &e.factor))
                {
                    return Err(permission_denied(
                        "holder lacks a content.label-write grant for a submitted row",
                    ));
                }
                prepared.push((owner, cid));
            }

            // All authorized → write back. `scorer_version` is holder-asserted
            // (the drain stamps the worklist's `to_version`); the row is the
            // durable watermark, so a bump closes the gap for that item.
            let mut written = 0u32;
            for (row, (owner, cid)) in req.rows.iter().zip(prepared.iter()) {
                let scored_at = i64::try_from(row.scored_at).unwrap_or(i64::MAX);
                state
                    .db
                    .insert_content_scores(
                        cid,
                        &row.content_kind,
                        Some(owner),
                        scored_at,
                        &row.entries,
                    )
                    .await
                    .map_err(internal)?;
                written = written.saturating_add(row.entries.len() as u32);
            }
            encode_reply(&SubmitScoresReply {
                written,
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// The spam-baseline publish drain (`mail-spam.md` § Encrypted-mode interaction,
// ratified 2026-07-13) — the third holder-pull drain instance, beside the
// re-score plane above: `publish_spam_baseline` registers a pending run and
// pokes the holder (`PushEvent::BridgeSpamBaselinePublish`); the holder pulls
// the run's grant-gated sealed-copy worklist here, unseal-merges OFF-BOX with
// its own service-user key (never any key of a user's — the nest core holds no
// in-process decryption authority, `encryption-at-rest.md` § Don't do these),
// and submits its merged half + count back through the run's oneshot.

/// Defensive bound on a submitted merged half: the holder's merge is capped
/// off-box to `MODEL_MAX_BYTES_DEFAULT` like every model persist, so anything
/// past 2× that is malformed, not a bigger merge (a compromised holder can't
/// balloon the publish handler's decode).
const MAX_SUBMIT_SPAM_BASELINE_BYTES: usize = fauna_mail::spam::MODEL_MAX_BYTES_DEFAULT * 2;

/// `fauna.capabilities.spam_baseline_worklist` — the poked holder asks "which
/// sealed contributor copies may I merge for this publish run?" Serves the
/// intersection of (the copies resting for this holder) × (owners whose
/// standing keyless `content.read{spam-model}` grants reach it) × (owners
/// still opted in) × (owners who still have a current model row). The
/// run gate keeps the surface quiet outside a publish window even to an
/// enrolled holder (a compromised holder learns nothing between runs).
pub(crate) fn spam_baseline_worklist_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.capabilities.spam_baseline_worklist",
            )
            .await?;
            let req: SpamBaselineWorklistRequest = decode(&payload).map_err(malformed)?;
            // The worklist exists only inside a publish window — an unknown /
            // expired run id serves nothing (typed, client-actionable).
            if !state
                .spam_baseline_runs
                .lock()
                .await
                .contains_key(req.run_id.as_ref())
            {
                return Err(malformed("no pending spam-baseline publish run"));
            }

            // The holder identity is the authenticated caller's enrolled
            // x25519 (no field to spoof) — the same resolution
            // `holder_granted_scopes` performs.
            let row = state
                .db
                .lookup_bridge_service_user(&actor_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| permission_denied("caller is not an enrolled service user"))?;
            let holder = row
                .x25519_pubkey
                .ok_or_else(|| permission_denied("caller has no x25519 pubkey on record"))?;

            // Owners whose LIVE keyless `content.read{spam-model}` grants
            // reach this holder — the copy alone is NOT authorization; the
            // grant is (`mail-spam.md` § Encrypted-mode interaction, the ✅
            // key-shape callout).
            let scopes = holder_granted_scopes(&state, &actor_id).await?;
            let mut granted_owners: Vec<[u8; 32]> = Vec::new();
            for (owner, sc) in &scopes {
                if sc.class == fauna_mls::wrapped_blob::ScopeTuple::CLASS_CONTENT_READ
                    && sc.kind.as_deref()
                        == Some(fauna_mls::wrapped_blob::ScopeTuple::KIND_SPAM_MODEL)
                    && !granted_owners.contains(owner)
                {
                    granted_owners.push(*owner);
                }
            }

            let mut copies: Vec<SpamBaselineCopy> = Vec::new();
            for (owner, sealed_copy) in state
                .db
                .list_spam_model_holder_copies(&holder)
                .await
                .map_err(internal)?
            {
                if !granted_owners.contains(&owner) {
                    continue;
                }
                // Consent is read LIVE each run — a revoked toggle whose
                // copy-delete hasn't landed (or raced) still serves nothing.
                if !state
                    .db
                    .get_spam_preferences(&owner)
                    .await
                    .map_err(internal)?
                    .contribute_baseline
                {
                    continue;
                }
                // Only an owner who still has a current model row belongs on
                // the worklist: a reset model's stale copy is never served.
                // Every stored model is sealed (`put_spam_model` refuses
                // anything else), so the row's presence is the whole test.
                if state
                    .db
                    .get_spam_model(&owner)
                    .await
                    .map_err(internal)?
                    .is_none()
                {
                    continue;
                }
                copies.push(SpamBaselineCopy {
                    owner_actor_id: ByteBuf::from(owner.to_vec()),
                    sealed_copy: ByteBuf::from(sealed_copy),
                    extra: Default::default(),
                });
            }
            encode_reply(&SpamBaselineWorklistReply {
                copies,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.capabilities.submit_spam_baseline` — the holder writes back its
/// off-box merge for a pending publish run. Removing the run's oneshot and
/// sending the submission through it hands the merged half to the awaiting
/// `publish_spam_baseline_handler`. An unknown / expired run replies
/// `ok: false` — idempotent, never an error (mirroring revoke's idempotency:
/// the publish may have timed out while the holder merged, which is exactly
/// the "holder too slow" outcome the bounded await already priced in).
pub(crate) fn submit_spam_baseline_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.capabilities.submit_spam_baseline").await?;
            let req: SubmitSpamBaselineRequest = decode(&payload).map_err(malformed)?;
            if req.merged_model.len() > MAX_SUBMIT_SPAM_BASELINE_BYTES {
                return Err(malformed(format!(
                    "merged_model too large: {} bytes (max {MAX_SUBMIT_SPAM_BASELINE_BYTES})",
                    req.merged_model.len()
                )));
            }
            // The run belongs to the holder it was poked to. Consuming it is
            // first-submit-wins, so this identity check is what keeps another
            // enrolled holder that passes the coarse role gate — the MDA a
            // standard box always runs, which holds no reaching
            // `content.read{spam-model}` grant — from taking the run with the
            // empty half its always-empty worklist yields. `ok: false` is the
            // existing "arrived at no pending run" ack: the caller had nothing
            // to contribute anyway.
            let mut runs = state.spam_baseline_runs.lock().await;
            let is_bound_holder = runs
                .get(req.run_id.as_ref())
                .is_some_and(|run| run.holder == actor_id);
            if !is_bound_holder {
                return encode_reply(&SubmitSpamBaselineReply {
                    ok: false,
                    extra: Default::default(),
                });
            }
            let run = runs
                .remove(req.run_id.as_ref())
                .expect("run was just observed under this lock");
            drop(runs);
            // A dropped receiver (the publish timed out between our map
            // removal and this send) is fine — same outcome as arriving late.
            // A malformed name (not 32 bytes) is dropped rather than refused:
            // the holder is trusted infra and the run's intersection with its
            // sealed candidates is the real gate; refusing the whole submit
            // would erode every sealed contributor over one bad entry.
            let merged_contributors = req
                .merged_contributors
                .iter()
                .filter_map(|id| <[u8; 32]>::try_from(id.as_ref()).ok())
                .collect();
            let _ = run.tx.send(crate::routes::SpamBaselineSubmission {
                merged_model: req.merged_model,
                contributors: req.contributors,
                unreadable: req.unreadable,
                merged_contributors,
            });
            encode_reply(&SubmitSpamBaselineReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

/// Register the `fauna.capabilities.*` handlers (design § Phase 2 Step 2 § 2.3
/// plus § 2.5 the drain plane). Composed in `build_rpc_router()` beside
/// `register_bridge_blob_handlers`; each kind is gated in
/// `bridge_method_allowlist::is_permitted` (owner-minted mint/renew/revoke;
/// holder-scoped fetch/rescore_worklist/submit_scores), enforced by the central
/// capability gate and the `every_registered_kind_is_gated` coverage tripwire.
pub fn register_capability_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.capabilities.mint",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: mint_grant_handler(),
        },
    );
    b.add(
        "fauna.capabilities.fetch",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_grants_handler(),
        },
    );
    b.add(
        "fauna.capabilities.renew",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: renew_grant_handler(),
        },
    );
    b.add(
        "fauna.capabilities.revoke",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: revoke_grant_handler(),
        },
    );
    b.add(
        "fauna.capabilities.reconcile",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: reconcile_grants_handler(),
        },
    );
    b.add(
        "fauna.capabilities.rescore_worklist",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: rescore_worklist_handler(),
        },
    );
    b.add(
        "fauna.capabilities.submit_scores",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: submit_scores_handler(),
        },
    );
    // The spam-baseline publish drain (`mail-spam.md` § Encrypted-mode
    // interaction, ratified 2026-07-13). Provision-weight deadlines: the
    // worklist can carry copies approaching the model cap; the submit carries
    // the holder's merged half.
    b.add(
        "fauna.capabilities.spam_baseline_worklist",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: spam_baseline_worklist_handler(),
        },
    );
    b.add(
        "fauna.capabilities.submit_spam_baseline",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: submit_spam_baseline_handler(),
        },
    );
}

pub fn register_bridge_blob_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.bridges.provision_wrapped_mls_blob",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: provision_wrapped_mls_blob_handler(),
        },
    );
    b.add(
        "fauna.bridges.provision_mls_snapshot_blob",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: provision_mls_snapshot_blob_handler(),
        },
    );
    b.add(
        "fauna.bridges.provision_webdav_keys_blob",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: provision_webdav_keys_blob_handler(),
        },
    );
    b.add(
        "fauna.bridges.provision_wrapped_submission_token",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: provision_wrapped_submission_token_handler(),
        },
    );
    b.add(
        "fauna.bridges.provision_tls_cert_blob",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: provision_tls_cert_blob_handler(),
        },
    );
    b.add(
        "fauna.bridges.list_dkim_selectors",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_dkim_selectors_handler(),
        },
    );
    b.add(
        "fauna.bridges.revoke_dkim_blob",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: revoke_dkim_blob_handler(),
        },
    );
    b.add(
        "fauna.bridges.list_service_users",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_service_users_handler(),
        },
    );
    b.add(
        "fauna.bridges.request_enrollment",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: request_enrollment_handler(),
        },
    );
    b.add(
        "fauna.bridges.list_pending_bridges",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_pending_bridges_handler(),
        },
    );
    b.add(
        "fauna.bridges.approve_pending_bridge",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: approve_pending_bridge_handler(),
        },
    );
    b.add(
        "fauna.bridges.reject_pending_bridge",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: reject_pending_bridge_handler(),
        },
    );
    b.add(
        "fauna.bridges.set_mail_enabled",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_mail_enabled_handler(),
        },
    );
    b.add(
        "fauna.bridges.set_caldav_enabled",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_caldav_enabled_handler(),
        },
    );
    b.add(
        "fauna.bridges.set_carddav_enabled",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_carddav_enabled_handler(),
        },
    );
    b.add(
        "fauna.bridges.set_webdav_enabled",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_webdav_enabled_handler(),
        },
    );
    b.add(
        "fauna.bridges.set_caldav_port",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_caldav_port_handler(),
        },
    );
    b.add(
        "fauna.bridges.get_caldav_port",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: get_caldav_port_handler(),
        },
    );
    b.add(
        "fauna.bridges.set_auto_enable_mail_for_new_users",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_auto_enable_mail_for_new_users_handler(),
        },
    );
    b.add(
        "fauna.bridges.set_mail_serving_enabled",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_mail_serving_enabled_handler(),
        },
    );
    b.add(
        "fauna.bridges.get_mail_serving_enabled",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: get_mail_serving_enabled_handler(),
        },
    );
    b.add(
        "fauna.bridges.revoke_service_user",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: revoke_service_user_handler(),
        },
    );
    b.add(
        "fauna.bridges.fetch_wrapped_mls_blob",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_wrapped_mls_blob_handler(),
        },
    );
    b.add(
        "fauna.bridges.fetch_mls_snapshot_blob",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_mls_snapshot_blob_handler(),
        },
    );
    b.add(
        "fauna.bridges.fetch_webdav_keys_blob",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_webdav_keys_blob_handler(),
        },
    );
    b.add(
        "fauna.bridges.mint_bulk_byte_token",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: mint_bulk_byte_token_handler(),
        },
    );
    b.add(
        "fauna.bridges.webdav_list_folders",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: webdav_list_folders_handler(),
        },
    );
    b.add(
        "fauna.bridges.webdav_list_files",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: webdav_list_files_handler(),
        },
    );
    b.add(
        "fauna.bridges.webdav_quota",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: webdav_quota_handler(),
        },
    );
    b.add(
        "fauna.bridges.webdav_record_change",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: webdav_record_change_handler(),
        },
    );
    b.add(
        "fauna.bridges.fetch_wrapped_submission_token",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_wrapped_submission_token_handler(),
        },
    );
    b.add(
        "fauna.bridges.fetch_tls_cert_blob",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_tls_cert_blob_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.fetch_identities",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_atproto_identities_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.fetch_identity_key_blob",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_atproto_identity_key_blob_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.fetch_session_secret_blob",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_atproto_session_secret_blob_handler(),
        },
    );
    b.add(
        "fauna.bridges.atproto.record_minted_identity",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: record_minted_identity_handler(),
        },
    );
    b.add(
        "fauna.bridges.fetch_bridge_pubkey",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(2),
            handler: fetch_bridge_pubkey_handler(),
        },
    );
    b.add(
        fauna_protocol::log_plane::KIND_BRIDGES_REPORT_LOG_EVENTS,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(2),
            handler: report_log_events_handler(),
        },
    );
    b.add(
        "fauna.bridges.revoke_wrapped_mls_blob",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: revoke_wrapped_mls_blob_handler(),
        },
    );
    b.add(
        "fauna.bridges.revoke_wrapped_submission_token",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: revoke_wrapped_submission_token_handler(),
        },
    );
    b.add(
        "fauna.bridges.register_service_user",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: register_service_user_handler(),
        },
    );
    b.add(
        "fauna.bridges.report_auth_event",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(2),
            handler: report_auth_event_handler(),
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
    use crate::bridge_approval_test_support::{approve_bridge, approve_bridge_with_x25519};
    use crate::db::CacheDb;
    use crate::test_support::small_spam_model;
    use serde_bytes::ByteBuf;

    async fn fixture_state() -> Arc<AppState> {
        crate::test_support::fixture_state()
    }

    #[tokio::test]
    async fn provision_wrapped_mls_blob_stores_bytes() {
        let state = fixture_state().await;
        let actor = [42u8; 32];
        state.db.create_user(&actor, "free", "test").await.unwrap();
        let req = ProvisionWrappedMlsBlobRequest {
            blob: ByteBuf::from(vec![0xAA; 256]),
            credential_id: "default".to_string(),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());

        let h = provision_wrapped_mls_blob_handler();
        let reply = h(state.clone(), actor, payload).await.expect("ok");
        let _: ProvisionReply = decode(&reply).unwrap();

        let stored = state
            .db
            .get_wrapped_mls_blob(&actor, "default")
            .await
            .unwrap();
        assert_eq!(stored.unwrap().len(), 256);
    }

    #[tokio::test]
    async fn provision_mls_snapshot_blob_stores_bytes() {
        let state = fixture_state().await;
        let actor = [42u8; 32];
        state.db.create_user(&actor, "free", "test").await.unwrap();
        let req = ProvisionMlsSnapshotBlobRequest {
            blob: ByteBuf::from(vec![0xBB; 4096]),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());

        let h = provision_mls_snapshot_blob_handler();
        let _ = h(state.clone(), actor, payload).await.expect("ok");

        let stored = state.db.get_mls_snapshot_blob(&actor).await.unwrap();
        assert_eq!(stored.unwrap().len(), 4096);
    }

    #[tokio::test]
    async fn provision_webdav_keys_blob_stores_bytes() {
        let state = fixture_state().await;
        let actor = [42u8; 32];
        state.db.create_user(&actor, "free", "test").await.unwrap();
        let req = ProvisionWebdavKeysBlobRequest {
            blob: ByteBuf::from(vec![0xDD; 2048]),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());

        let h = provision_webdav_keys_blob_handler();
        let _ = h(state.clone(), actor, payload).await.expect("ok");

        let stored = state.db.get_webdav_keys_blob(&actor).await.unwrap();
        assert_eq!(stored.unwrap().len(), 2048);

        // Re-provision overwrites (single row per actor).
        let req2 = ProvisionWebdavKeysBlobRequest {
            blob: ByteBuf::from(vec![0xEE; 512]),
            extra: Default::default(),
        };
        let payload2 = Bytes::from(encode_canonical(&req2).unwrap().to_vec());
        let _ = provision_webdav_keys_blob_handler()(state.clone(), actor, payload2)
            .await
            .expect("ok");
        let restored = state
            .db
            .get_webdav_keys_blob(&actor)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(restored.len(), 512);
    }

    #[tokio::test]
    async fn provision_wrapped_submission_token_stores_bytes() {
        let state = fixture_state().await;
        let actor = [42u8; 32];
        state.db.create_user(&actor, "free", "test").await.unwrap();
        let req = ProvisionWrappedSubmissionTokenRequest {
            blob: ByteBuf::from(vec![0xCC; 128]),
            credential_id: "default".to_string(),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());

        let h = provision_wrapped_submission_token_handler();
        let _ = h(state.clone(), actor, payload).await.expect("ok");

        let stored = state
            .db
            .get_wrapped_submission_token(&actor, "default")
            .await
            .unwrap();
        assert_eq!(stored.unwrap().len(), 128);
    }

    #[tokio::test]
    async fn list_and_revoke_dkim_require_admin_class() {
        let state = fixture_state().await;
        let non_admin = [99u8; 32];

        let list_req = ListDkimSelectorsRequest {
            domain: None,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&list_req).unwrap().to_vec());
        let err = list_dkim_selectors_handler()(state.clone(), non_admin, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");

        let revoke_req = RevokeDkimBlobRequest {
            domain: "example.com".into(),
            selector: "2026a".into(),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&revoke_req).unwrap().to_vec());
        let err = revoke_dkim_blob_handler()(state, non_admin, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn admin_list_service_users_projects_public_metadata() {
        use crate::db::bridge_service_users::BridgeRole;
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        let mta = [44u8; 32];
        state
            .db
            .create_pending_bridge_service_user(&mta, BridgeRole::Mta, "mta-1")
            .await
            .unwrap();
        state
            .db
            .upsert_bridge_x25519(&mta, &[9u8; 32])
            .await
            .unwrap();
        state
            .db
            .approve_bridge_service_user(&mta, None)
            .await
            .unwrap();

        let req = ListServiceUsersRequest {
            role: Some("mta".into()),
            status: Some("approved".into()),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = list_service_users_handler()(state.clone(), admin, payload)
            .await
            .expect("admin list service users ok");
        let reply: ListServiceUsersReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.service_users.len(), 1);
        let su = &reply.service_users[0];
        assert_eq!(su.bridge_id, "mta-1");
        assert_eq!(su.role, "mta");
        assert_eq!(su.status, "approved");
        assert!(su.has_x25519, "approved MTA attested x25519 → sealable");
        assert_eq!(su.ed25519_pubkey.len(), 32);

        // An UNKNOWN actor (no users row ⇒ no caller class) stays denied —
        // the User relax (2026-07-17) admits registered users only.
        let req = ListServiceUsersRequest {
            role: None,
            status: None,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = list_service_users_handler()(state, [99u8; 32], payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    /// The `enrollment_strict` self-report (security.md § Enrollment
    /// proof-of-possession contract): an ADMIN's reply carries an explicit
    /// per-role strictness diagnostic; with no blessed registry provisioned
    /// (this test env — deliberately no `FAUNA_BLESSED_KEYS_DIR`,
    /// which are process-global and never set by tests, see the module note on
    /// the enrollment_auth tests) it reports the documented lenient fallback
    /// (`false`/`false`) — an explicit "lenient", NOT an absent field. A plain
    /// user's holder view carries `None` (the diagnostic is admin-scoped).
    /// The strict=true half is covered end-to-end by the tier_4
    /// `test_bridge_enrollment_pop.py` (real image, artifact-provisioned
    /// registry) and the live-Hetzner assert.
    #[tokio::test]
    async fn enrollment_strict_reported_to_admin_only() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        let user = [8u8; 32];
        state.db.create_user(&user, "free", "plain").await.unwrap();

        let req = ListServiceUsersRequest {
            role: None,
            status: None,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply: ListServiceUsersReply = decode(
            &list_service_users_handler()(state.clone(), admin, payload.clone())
                .await
                .expect("admin list ok"),
        )
        .unwrap();
        let strict = reply
            .enrollment_strict
            .expect("admin reply must carry the enrollment_strict diagnostic");
        assert!(
            !strict.mta && !strict.mda,
            "no blessed registry in the unit-test env → both roles report lenient"
        );

        let reply: ListServiceUsersReply = decode(
            &list_service_users_handler()(state, user, payload)
                .await
                .expect("user list ok"),
        )
        .unwrap();
        assert_eq!(
            reply.enrollment_strict, None,
            "holder view must not carry the admin provisioning diagnostic"
        );
    }

    /// The class-scoped reply (2026-07-17): a plain registered user sees ONLY
    /// the mint-relevant holder view — approved + x25519-attested +
    /// content-processor-family role + not in-process — never the pending/
    /// revoked roster, the MTA topology, or the nest's own in-process holder
    /// (a user-facing trust mint must not be offered the nest's disk-resident
    /// key as a seal target). Requested filters apply WITHIN that subset.
    #[tokio::test]
    async fn a_plain_user_lists_only_the_approved_offbox_holder_view() {
        use crate::db::bridge_service_users::BridgeRole;
        let state = fixture_state().await;
        let user = [8u8; 32];
        state.db.create_user(&user, "free", "plain").await.unwrap();

        // Visible: an approved MDA and an approved off-box content-processor,
        // both x25519-attested.
        for (pk, role, id) in [
            ([41u8; 32], BridgeRole::Mda, "mda-1"),
            ([42u8; 32], BridgeRole::ContentProcessor, "cp-1"),
        ] {
            state
                .db
                .create_pending_bridge_service_user(&pk, role, id)
                .await
                .unwrap();
            state
                .db
                .upsert_bridge_x25519(&pk, &[9u8; 32])
                .await
                .unwrap();
            state
                .db
                .approve_bridge_service_user(&pk, None)
                .await
                .unwrap();
        }
        // Hidden: an approved MTA (wrong role family)…
        let mta = [43u8; 32];
        state
            .db
            .create_pending_bridge_service_user(&mta, BridgeRole::Mta, "mta-1")
            .await
            .unwrap();
        state
            .db
            .upsert_bridge_x25519(&mta, &[9u8; 32])
            .await
            .unwrap();
        state
            .db
            .approve_bridge_service_user(&mta, None)
            .await
            .unwrap();
        // …a PENDING MDA (status)…
        let pending = [44u8; 32];
        state
            .db
            .create_pending_bridge_service_user(&pending, BridgeRole::Mda, "mda-pending")
            .await
            .unwrap();
        state
            .db
            .upsert_bridge_x25519(&pending, &[9u8; 32])
            .await
            .unwrap();
        // …an approved MDA with NO attested x25519 (unsealable)…
        let bare = [45u8; 32];
        state
            .db
            .create_pending_bridge_service_user(&bare, BridgeRole::Mda, "mda-bare")
            .await
            .unwrap();
        state
            .db
            .approve_bridge_service_user(&bare, None)
            .await
            .unwrap();
        // …and the nest's own IN-PROCESS content-processor (web-serve).
        let inproc = [46u8; 32];
        state
            .db
            .create_pending_bridge_service_user(&inproc, BridgeRole::ContentProcessor, "web-serve")
            .await
            .unwrap();
        state
            .db
            .upsert_bridge_x25519(&inproc, &[9u8; 32])
            .await
            .unwrap();
        state
            .db
            .approve_bridge_service_user(&inproc, None)
            .await
            .unwrap();
        state
            .db
            .mark_bridge_service_user_in_process(&inproc)
            .await
            .unwrap();

        let req = ListServiceUsersRequest {
            role: None,
            status: None,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = list_service_users_handler()(state.clone(), user, payload)
            .await
            .expect("a registered user may list the holder view");
        let reply: ListServiceUsersReply = decode(&reply_bytes).unwrap();
        let mut ids: Vec<_> = reply
            .service_users
            .iter()
            .map(|u| u.bridge_id.clone())
            .collect();
        ids.sort();
        assert_eq!(
            ids,
            vec!["cp-1".to_string(), "mda-1".to_string()],
            "user view = approved, x25519-attested, cp-family, off-box only"
        );

        // A user probing the pending roster sees nothing — the requested
        // filter intersects the holder view instead of widening it.
        let req = ListServiceUsersRequest {
            role: None,
            status: Some("pending".into()),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = list_service_users_handler()(state, user, payload)
            .await
            .expect("filtered probe still answers");
        let reply: ListServiceUsersReply = decode(&reply_bytes).unwrap();
        assert!(
            reply.service_users.is_empty(),
            "status=pending yields nothing for a plain user"
        );
    }

    // ── Bridge approval lifecycle handlers (Stage 1) ────────────────

    use crate::db::bridge_service_users::{BridgeRole, BridgeStatus};

    async fn admin_state_with_pending(
        pk: &[u8; 32],
        role: BridgeRole,
    ) -> (Arc<AppState>, [u8; 32]) {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        state
            .db
            .create_pending_bridge_service_user(pk, role, "b1")
            .await
            .unwrap();
        (state, admin)
    }

    #[tokio::test]
    async fn admin_list_pending_bridges_projects_pending_only() {
        let pending = [0x44u8; 32];
        let (state, admin) = admin_state_with_pending(&pending, BridgeRole::Mta).await;
        // A second, approved bridge must NOT appear in the pending feed.
        let approved = [0x55u8; 32];
        state
            .db
            .create_pending_bridge_service_user(&approved, BridgeRole::Mda, "b2")
            .await
            .unwrap();
        state
            .db
            .approve_bridge_service_user(&approved, Some(&admin))
            .await
            .unwrap();

        let payload = Bytes::from(
            encode_canonical(&ListPendingBridgesRequest {})
                .unwrap()
                .to_vec(),
        );
        let reply_bytes = list_pending_bridges_handler()(state.clone(), admin, payload)
            .await
            .expect("admin list pending ok");
        let reply: ListPendingBridgesReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.bridges.len(), 1);
        assert_eq!(reply.bridges[0].bridge_id, "b1");
        assert_eq!(reply.bridges[0].status, "pending");

        // Non-admin denied.
        let payload = Bytes::from(
            encode_canonical(&ListPendingBridgesRequest {})
                .unwrap()
                .to_vec(),
        );
        let err = list_pending_bridges_handler()(state, [99u8; 32], payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── Bridge self-enrollment (request_enrollment) ────────────────

    #[tokio::test]
    async fn request_enrollment_creates_pending_row_with_synthesized_bridge_id() {
        let state = fixture_state().await;
        let pk = [0x33u8; 32];
        let payload = Bytes::from(
            encode_canonical(&RequestEnrollmentRequest {
                ed25519_pubkey: pk.to_vec(),
                role_hint: "mta".into(),
                bridge_id: String::new(),
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        );
        // Anonymous connection → zero actor placeholder; the handler ignores it
        // (loopback gating happens at the dispatch layer, not here).
        let reply_bytes = request_enrollment_handler()(state.clone(), [0u8; 32], payload)
            .await
            .expect("self-enroll ok");
        let reply: RequestEnrollmentReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.status, "pending");

        let row = state
            .db
            .lookup_bridge_service_user(&pk)
            .await
            .unwrap()
            .expect("pending row created");
        assert_eq!(row.status, BridgeStatus::Pending);
        assert_eq!(row.role, BridgeRole::Mta);
        // Empty bridge_id → synthesized "<role>-<pubkey-prefix>".
        assert_eq!(row.bridge_id, "mta-33333333");
    }

    #[tokio::test]
    async fn request_enrollment_is_idempotent_and_does_not_clobber() {
        let state = fixture_state().await;
        let pk = [0x34u8; 32];
        // Already approved as MDA (with bridge_id "b1") via the test helper.
        approve_bridge(&state.db, &pk, BridgeRole::Mda).await;

        // A late/duplicate self-enroll with a *different* role hint must NOT
        // re-create or downgrade the row — it just reports the current status.
        let payload = Bytes::from(
            encode_canonical(&RequestEnrollmentRequest {
                ed25519_pubkey: pk.to_vec(),
                role_hint: "mta".into(),
                bridge_id: "elsewhere".into(),
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        );
        let reply_bytes = request_enrollment_handler()(state.clone(), [0u8; 32], payload)
            .await
            .expect("idempotent ok");
        let reply: RequestEnrollmentReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.status, "approved");

        let row = state
            .db
            .lookup_bridge_service_user(&pk)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, BridgeStatus::Approved);
        assert_eq!(
            row.role,
            BridgeRole::Mda,
            "role_hint must not override the enrolled role"
        );
        assert_eq!(row.bridge_id, "b1", "bridge_id must not be clobbered");
    }

    #[tokio::test]
    async fn request_enrollment_rejects_unknown_role_hint() {
        let state = fixture_state().await;
        let payload = Bytes::from(
            encode_canonical(&RequestEnrollmentRequest {
                ed25519_pubkey: [0x35u8; 32].to_vec(),
                role_hint: String::new(),
                bridge_id: String::new(),
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        );
        let err = request_enrollment_handler()(state.clone(), [0u8; 32], payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
        // The rejection detail must name every currently-valid role_hint, so it
        // can't silently go stale again when BridgeRole grows a variant.
        let detail = match err.details.as_deref() {
            Some(Value::String(s)) => s.clone(),
            other => panic!("expected a string detail, got {other:?}"),
        };
        for role in ["mta", "mda", "content-processor", "atproto.pds"] {
            assert!(
                detail.contains(role),
                "rejection detail {detail:?} is missing role {role:?}"
            );
        }
        // No row created on a rejected hint.
        assert!(
            state
                .db
                .lookup_bridge_service_user(&[0x35u8; 32])
                .await
                .unwrap()
                .is_none()
        );
    }

    // ── Onboarding auto-approval (mail-bridge-lifecycle.md § Onboarding
    //    auto-approval) ───────────────────────────────────────────────
    //
    // The box's own bridges reach the (loopback-gated) `request_enrollment`
    // surface as same-host processes; once an admin has enabled the
    // deployment mail subsystem (`set_mail_enabled(true)`, Admin-class), the
    // enable IS the approval — a fresh loopback MTA/MDA enrollment lands
    // `approved`, with no manual `approve_pending_bridge` click.

    fn enroll_payload(pk: &[u8; 32], role: &str) -> Bytes {
        Bytes::from(
            encode_canonical(&RequestEnrollmentRequest {
                ed25519_pubkey: pk.to_vec(),
                role_hint: role.into(),
                bridge_id: String::new(),
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        )
    }

    async fn enroll_status(state: &Arc<AppState>, pk: &[u8; 32], role: &str) -> String {
        let reply_bytes =
            request_enrollment_handler()(state.clone(), [0u8; 32], enroll_payload(pk, role))
                .await
                .expect("self-enroll ok");
        let reply: RequestEnrollmentReply = decode(&reply_bytes).unwrap();
        reply.status
    }

    #[tokio::test]
    async fn request_enrollment_auto_approves_loopback_mail_bridge_when_mail_enabled() {
        let state = fixture_state().await;

        // 1. Mail not yet enabled (toggle unset) → enrollment stays pending.
        let pk_off = [0x51u8; 32];
        assert_eq!(
            enroll_status(&state, &pk_off, "mta").await,
            "pending",
            "mail off (unset) → pending"
        );

        // 2. Admin enables mail.
        state.db.set_mail_enabled(true).await.unwrap();

        // 3. A *fresh* enrollment lands approved on first sight — the onboarding
        //    path: the toggle is persisted before the bridge cold-boots + enrolls.
        let pk_new = [0x52u8; 32];
        assert_eq!(
            enroll_status(&state, &pk_new, "mda").await,
            "approved",
            "mail on → fresh loopback enroll auto-approved"
        );
        assert_eq!(
            state
                .db
                .lookup_bridge_service_user(&pk_new)
                .await
                .unwrap()
                .unwrap()
                .status,
            BridgeStatus::Approved,
        );

        // 4. The already-pending pk_off self-heals on its next poll once mail is on.
        assert_eq!(
            enroll_status(&state, &pk_off, "mta").await,
            "approved",
            "pending poll self-heals to approved when mail enabled"
        );
    }

    #[tokio::test]
    async fn request_enrollment_stays_pending_when_mail_explicitly_disabled() {
        let state = fixture_state().await;
        state.db.set_mail_enabled(false).await.unwrap();
        assert_eq!(
            enroll_status(&state, &[0x53u8; 32], "mta").await,
            "pending",
            "mail explicitly disabled → pending"
        );
    }

    #[tokio::test]
    async fn request_enrollment_never_auto_approves_atproto_pds_even_with_all_axes_on() {
        // Claim (mail-bridge-lifecycle.md:169; atproto-pds-bridge.md § Enable UX):
        // a non-mail bridge role is NEVER auto-approved by the mail/DAV enable
        // toggles — publishing a user's posts to a public network is a deliberate
        // opt-in, so the PDS bridge always takes the manual admin approval card.
        // Turn on EVERY loopback enable axis and confirm it STILL lands `pending`.
        let state = fixture_state().await;
        state.db.set_mail_enabled(true).await.unwrap();
        state.db.set_caldav_enabled(true).await.unwrap();
        state.db.set_carddav_enabled(true).await.unwrap();
        state.db.set_webdav_enabled(true).await.unwrap();

        // Sanity: the same box auto-approves a mail bridge (proving the axes are on).
        assert_eq!(
            enroll_status(&state, &[0x70u8; 32], "mta").await,
            "approved",
            "control: an MTA auto-approves when mail is on",
        );
        // The PDS bridge does NOT, on any axis.
        assert_eq!(
            enroll_status(&state, &[0x71u8; 32], "atproto.pds").await,
            "pending",
            "a PDS bridge is not a mail bridge — never auto-approved, always manual",
        );
    }

    #[tokio::test]
    async fn request_enrollment_auto_approves_mda_on_caldav_only_box() {
        // Independent enablement (`caldav-server.md` § Independent enablement): a
        // CalDAV-only box (email never enabled) still brings the MDA up to serve
        // calendar sync, so the MDA's loopback self-enroll must auto-approve on
        // the `caldav_enabled` axis alone — otherwise it stays PENDING forever
        // and CalDAV never serves. The MTA, which only runs (and thus only
        // enrolls) when email is on (`mta_should_run` / its s6 run-script), must
        // NOT auto-approve on the caldav axis.
        let state = fixture_state().await;
        // CalDAV-only: caldav on, mail never enabled (toggle unset).
        state.db.set_caldav_enabled(true).await.unwrap();

        // The MDA auto-approves on the caldav axis alone.
        assert_eq!(
            enroll_status(&state, &[0x61u8; 32], "mda").await,
            "approved",
            "caldav-only → MDA loopback enroll auto-approved"
        );
        // The MTA does NOT auto-approve on the caldav axis (it serves email only).
        assert_eq!(
            enroll_status(&state, &[0x62u8; 32], "mta").await,
            "pending",
            "caldav-only → MTA stays pending (MTA runs only when email is on)"
        );
    }

    #[tokio::test]
    async fn request_enrollment_auto_approves_mda_on_webdav_only_box() {
        // Independent enablement (`webdav-server.md` § Independent enablement): a
        // files-only box (email/CalDAV/CardDAV never enabled) still brings the MDA
        // up to serve WebDAV, so the MDA's loopback self-enroll must auto-approve
        // on the `webdav_enabled` axis alone — otherwise it stays PENDING forever
        // and files never serve (breaking works-out-of-the-box). The MTA, which
        // only runs when email is on, must NOT auto-approve on the webdav axis.
        let state = fixture_state().await;
        // WebDAV-only: webdav on, mail/caldav/carddav never enabled.
        state.db.set_webdav_enabled(true).await.unwrap();

        assert_eq!(
            enroll_status(&state, &[0x63u8; 32], "mda").await,
            "approved",
            "webdav-only → MDA loopback enroll auto-approved"
        );
        assert_eq!(
            enroll_status(&state, &[0x64u8; 32], "mta").await,
            "pending",
            "webdav-only → MTA stays pending (MTA runs only when email is on)"
        );
    }

    #[tokio::test]
    async fn request_enrollment_does_not_resurrect_revoked_bridge_when_mail_enabled() {
        // An admin's reject is durable: a revoked pubkey that re-polls while
        // mail is enabled stays revoked — auto-approval only ever flips
        // `pending` → `approved`, never `revoked` → `approved`.
        let state = fixture_state().await;
        state.db.set_mail_enabled(true).await.unwrap();
        let pk = [0x55u8; 32];
        state
            .db
            .create_pending_bridge_service_user(&pk, BridgeRole::Mta, "mta-rev")
            .await
            .unwrap();
        state.db.revoke_bridge_service_user(&pk).await.unwrap();

        assert_eq!(
            enroll_status(&state, &pk, "mta").await,
            "revoked",
            "revoked stays revoked even with mail enabled"
        );
        assert_eq!(
            state
                .db
                .lookup_bridge_service_user(&pk)
                .await
                .unwrap()
                .unwrap()
                .status,
            BridgeStatus::Revoked,
        );
    }

    #[tokio::test]
    async fn self_enrolled_bridge_appears_in_admin_pending_list() {
        // The observable end result: a bridge self-enrolls → it shows up in the
        // admin's `list_pending_bridges` feed, ready to approve from the client.
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();

        let pk = [0x42u8; 32];
        let enroll = Bytes::from(
            encode_canonical(&RequestEnrollmentRequest {
                ed25519_pubkey: pk.to_vec(),
                role_hint: "mda".into(),
                bridge_id: String::new(),
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        );
        request_enrollment_handler()(state.clone(), [0u8; 32], enroll)
            .await
            .expect("self-enroll ok");

        let list = Bytes::from(
            encode_canonical(&ListPendingBridgesRequest {})
                .unwrap()
                .to_vec(),
        );
        let reply_bytes = list_pending_bridges_handler()(state.clone(), admin, list)
            .await
            .expect("admin list pending ok");
        let reply: ListPendingBridgesReply = decode(&reply_bytes).unwrap();
        let found = reply
            .bridges
            .iter()
            .find(|b| b.ed25519_pubkey == pk.to_vec())
            .expect("self-enrolled bridge in pending feed");
        assert_eq!(found.role, "mda");
        assert_eq!(found.status, "pending");
    }

    // ── Key-possession enrollment (slice 2) ────────────────────
    //
    // The security-critical policy is the pure `check_enrollment_authorization`
    // (env- and db-free), tested exhaustively here; the handler is thin glue
    // (env-provisioned registry → policy → bind). The strict end-to-end against a
    // real artifact-provisioned registry is proven by tier_4 (deploy-verify). We
    // deliberately do NOT set `FAUNA_BLESSED_KEYS_DIR` in-process: it is read
    // process-wide, so a parallel lenient-mode handler test would flip to strict
    // and fail spuriously — the seam is the param, exactly as slice 3's
    // `read_optional_proxy_header` takes its secret as an argument.
    use ed25519_dalek::Signer;
    use fauna_protocol::wrapped_blob::enrollment_signed_message as enroll_msg;

    fn ed_keypair(seed: u8) -> (ed25519_dalek::SigningKey, [u8; 32]) {
        let sk = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let pk = sk.verifying_key().to_bytes();
        (sk, pk)
    }

    #[test]
    fn parse_blessed_pubkey_absent_empty_valid_invalid() {
        assert_eq!(parse_blessed_pubkey(""), Ok(None));
        assert_eq!(parse_blessed_pubkey("   "), Ok(None));
        let pk = [0xABu8; 32];
        assert_eq!(parse_blessed_pubkey(&hex::encode(pk)), Ok(Some(pk)));
        // Surrounding whitespace tolerated (a run-script may add a newline).
        assert_eq!(
            parse_blessed_pubkey(&format!("  {}\n", hex::encode(pk))),
            Ok(Some(pk))
        );
        assert!(parse_blessed_pubkey("nothex!!").is_err());
        assert!(parse_blessed_pubkey(&hex::encode([0u8; 16])).is_err()); // wrong length
    }

    #[test]
    fn blessed_pubkey_from_dir_reads_role_file_live() {
        // The registry-dir path (FAUNA_BLESSED_KEYS_DIR image wiring) must
        // reflect the CURRENT file content on every call — the service-user
        // re-keying re-bless depends on a running nest picking up a rewrite
        // (mail-bridge-lifecycle.md § Service-user re-keying). Exercised via
        // the pure dir helper (no env — see the module-note above on why
        // process-wide env is never set in tests).
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mta.pub");

        // Absent file → not provisioned → lenient.
        assert_eq!(blessed_pubkey_from_dir(dir.path(), "mta"), None);

        // Provisioned → strict against exactly that key (newline tolerated,
        // as the run-script writes one).
        let pk1 = [0x11u8; 32];
        std::fs::write(&path, format!("{}\n", hex::encode(pk1))).unwrap();
        assert_eq!(blessed_pubkey_from_dir(dir.path(), "mta"), Some(pk1));

        // Re-blessed mid-life (rotation): the next read sees the NEW key with
        // no process restart.
        let pk2 = [0x22u8; 32];
        std::fs::write(&path, format!("{}\n", hex::encode(pk2))).unwrap();
        assert_eq!(blessed_pubkey_from_dir(dir.path(), "mta"), Some(pk2));

        // Malformed → logged + lenient (works-out-of-the-box).
        std::fs::write(&path, "nothex!!").unwrap();
        assert_eq!(blessed_pubkey_from_dir(dir.path(), "mta"), None);

        // Per-role: the mda file is separate — absent here → lenient for mda
        // even while mta is strict.
        std::fs::write(&path, format!("{}\n", hex::encode(pk1))).unwrap();
        assert_eq!(blessed_pubkey_from_dir(dir.path(), "mda"), None);
    }

    #[test]
    fn enrollment_auth_lenient_when_no_blessed_registry() {
        // No blessed key for the role → the auto-approve path, nothing to
        // bind, even if the bridge sent (ignored) signed fields.
        let (_sk, pk) = ed_keypair(1);
        let x = [0x22u8; 32];
        let bad = [9u8; 64];
        assert!(matches!(
            check_enrollment_authorization("mta", &pk, Some(&x), Some(&bad[..]), None),
            Ok(None)
        ));
        assert!(matches!(
            check_enrollment_authorization("mta", &pk, None, None, None),
            Ok(None)
        ));
    }

    #[test]
    fn enrollment_auth_strict_rejects_non_blessed_pubkey() {
        // The exploit: an attacker presents a FRESH (non-blessed) key with a
        // perfectly valid self-signature — still rejected, the key isn't blessed.
        let (sk, attacker_pk) = ed_keypair(2);
        let (_bk, blessed_pk) = ed_keypair(3);
        let x = [0x22u8; 32];
        let sig = sk.sign(&enroll_msg("mta", &attacker_pk, &x)).to_bytes();
        let err = check_enrollment_authorization(
            "mta",
            &attacker_pk,
            Some(&x),
            Some(&sig[..]),
            Some(blessed_pk),
        )
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[test]
    fn enrollment_auth_strict_requires_x25519_and_sig() {
        let (sk, blessed_pk) = ed_keypair(4);
        let x = [0x22u8; 32];
        let sig = sk.sign(&enroll_msg("mta", &blessed_pk, &x)).to_bytes();
        // Missing x25519.
        assert!(
            check_enrollment_authorization(
                "mta",
                &blessed_pk,
                None,
                Some(&sig[..]),
                Some(blessed_pk)
            )
            .is_err()
        );
        // Missing signature.
        assert!(
            check_enrollment_authorization("mta", &blessed_pk, Some(&x), None, Some(blessed_pk))
                .is_err()
        );
    }

    #[test]
    fn enrollment_auth_strict_rejects_bad_or_wrong_context_signature() {
        let (sk, blessed_pk) = ed_keypair(5);
        let x = [0x22u8; 32];
        // Garbage signature bytes.
        let zero = [0u8; 64];
        assert_eq!(
            check_enrollment_authorization(
                "mta",
                &blessed_pk,
                Some(&x),
                Some(&zero[..]),
                Some(blessed_pk)
            )
            .unwrap_err()
            .code,
            "fauna.bridges.permission_denied"
        );
        // Valid signature, WRONG role (domain binding) → rejected as "mta".
        let sig_mda = sk.sign(&enroll_msg("mda", &blessed_pk, &x)).to_bytes();
        assert!(
            check_enrollment_authorization(
                "mta",
                &blessed_pk,
                Some(&x),
                Some(&sig_mda[..]),
                Some(blessed_pk)
            )
            .is_err()
        );
        // Valid signature over a DIFFERENT x25519 than presented → rejected
        // (the signature binds x25519, so it can't be swapped).
        let other_x = [0x33u8; 32];
        let sig_other = sk
            .sign(&enroll_msg("mta", &blessed_pk, &other_x))
            .to_bytes();
        assert!(
            check_enrollment_authorization(
                "mta",
                &blessed_pk,
                Some(&x),
                Some(&sig_other[..]),
                Some(blessed_pk)
            )
            .is_err()
        );
    }

    #[test]
    fn enrollment_auth_strict_accepts_blessed_pop_and_returns_x25519() {
        let (sk, blessed_pk) = ed_keypair(6);
        let x = [0x55u8; 32];
        let sig = sk.sign(&enroll_msg("mda", &blessed_pk, &x)).to_bytes();
        let got = check_enrollment_authorization(
            "mda",
            &blessed_pk,
            Some(&x),
            Some(&sig[..]),
            Some(blessed_pk),
        )
        .expect("a blessed key proving possession enrolls");
        assert_eq!(got, Some(x), "the verified x25519 is returned for binding");
    }

    #[tokio::test]
    async fn bind_enrollment_x25519_sets_once_then_freezes() {
        // The glue binds the PoP-verified x25519 at enrollment and maps the
        // set-once freeze to permission_denied.
        let state = fixture_state().await;
        let pk = [0x77u8; 32];
        state
            .db
            .create_pending_bridge_service_user(&pk, BridgeRole::Mda, "mda-x")
            .await
            .unwrap();
        let xa = [0xA1u8; 32];
        bind_enrollment_x25519(&state.db, &pk, &xa)
            .await
            .expect("first bind ok");
        // Idempotent re-bind of the SAME key (the bridge's poll path).
        bind_enrollment_x25519(&state.db, &pk, &xa)
            .await
            .expect("same-key re-bind ok");
        let row = state
            .db
            .lookup_bridge_service_user(&pk)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.x25519_pubkey, Some(xa));
        // A DIFFERENT key is frozen out.
        let err = bind_enrollment_x25519(&state.db, &pk, &[0xB2u8; 32])
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn admin_approve_pending_bridge_flips_to_approved_and_records_approver() {
        let pk = [0x44u8; 32];
        let (state, admin) = admin_state_with_pending(&pk, BridgeRole::Mta).await;

        let payload = Bytes::from(
            encode_canonical(&ApprovePendingBridgeRequest {
                ed25519_pubkey: pk.to_vec(),
                role: "mta".into(),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let reply_bytes = approve_pending_bridge_handler()(state.clone(), admin, payload)
            .await
            .expect("approve ok");
        let reply: ApprovePendingBridgeReply = decode(&reply_bytes).unwrap();
        assert!(reply.ok);

        let row = state
            .db
            .lookup_bridge_service_user(&pk)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, BridgeStatus::Approved);
        assert_eq!(row.approved_by_actor_id, Some(admin));

        // Idempotent: re-approving an already-approved bridge is ok.
        let payload = Bytes::from(
            encode_canonical(&ApprovePendingBridgeRequest {
                ed25519_pubkey: pk.to_vec(),
                role: "mta".into(),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let reply_bytes = approve_pending_bridge_handler()(state, admin, payload)
            .await
            .expect("idempotent approve ok");
        let reply: ApprovePendingBridgeReply = decode(&reply_bytes).unwrap();
        assert!(reply.ok);
    }

    #[tokio::test]
    async fn admin_approve_rejects_role_mismatch_and_unknown_and_revoked() {
        let pk = [0x44u8; 32];
        let (state, admin) = admin_state_with_pending(&pk, BridgeRole::Mta).await;

        // Role mismatch (enrolled mta, approve as mda) → malformed.
        let payload = Bytes::from(
            encode_canonical(&ApprovePendingBridgeRequest {
                ed25519_pubkey: pk.to_vec(),
                role: "mda".into(),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let err = approve_pending_bridge_handler()(state.clone(), admin, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");

        // Unknown pubkey → not_found.
        let payload = Bytes::from(
            encode_canonical(&ApprovePendingBridgeRequest {
                ed25519_pubkey: vec![0xEE; 32],
                role: "mta".into(),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let err = approve_pending_bridge_handler()(state.clone(), admin, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.not_found");

        // Revoked bridge → malformed (must re-enroll).
        state.db.revoke_bridge_service_user(&pk).await.unwrap();
        let payload = Bytes::from(
            encode_canonical(&ApprovePendingBridgeRequest {
                ed25519_pubkey: pk.to_vec(),
                role: "mta".into(),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let err = approve_pending_bridge_handler()(state.clone(), admin, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");

        // Non-admin denied.
        let payload = Bytes::from(
            encode_canonical(&ApprovePendingBridgeRequest {
                ed25519_pubkey: pk.to_vec(),
                role: "mta".into(),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let err = approve_pending_bridge_handler()(state, [99u8; 32], payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn admin_reject_pending_bridge_revokes_and_is_idempotent() {
        let pk = [0x44u8; 32];
        let (state, admin) = admin_state_with_pending(&pk, BridgeRole::Mta).await;

        let payload = Bytes::from(
            encode_canonical(&RejectPendingBridgeRequest {
                ed25519_pubkey: pk.to_vec(),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let reply_bytes = reject_pending_bridge_handler()(state.clone(), admin, payload)
            .await
            .expect("reject ok");
        let reply: RejectPendingBridgeReply = decode(&reply_bytes).unwrap();
        assert!(reply.ok);
        let row = state
            .db
            .lookup_bridge_service_user(&pk)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, BridgeStatus::Revoked);

        // Idempotent: rejecting an already-revoked bridge still replies ok.
        let payload = Bytes::from(
            encode_canonical(&RejectPendingBridgeRequest {
                ed25519_pubkey: pk.to_vec(),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let _ = reject_pending_bridge_handler()(state.clone(), admin, payload)
            .await
            .expect("idempotent reject ok");

        // Unknown pubkey → not_found.
        let payload = Bytes::from(
            encode_canonical(&RejectPendingBridgeRequest {
                ed25519_pubkey: vec![0xEE; 32],
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let err = reject_pending_bridge_handler()(state.clone(), admin, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.not_found");

        // Non-admin denied.
        let payload = Bytes::from(
            encode_canonical(&RejectPendingBridgeRequest {
                ed25519_pubkey: pk.to_vec(),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let err = reject_pending_bridge_handler()(state, [99u8; 32], payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn admin_revoke_service_user_revokes_approved_and_is_idempotent() {
        let pk = [0x55u8; 32];
        let (state, admin) = admin_state_with_pending(&pk, BridgeRole::Mta).await;
        // Running-phase re-keying targets an already-approved bridge.
        state
            .db
            .approve_bridge_service_user(&pk, Some(&admin))
            .await
            .unwrap();

        let revoke = |state: Arc<AppState>, caller: [u8; 32], actor_id: Vec<u8>| async move {
            let payload = Bytes::from(
                encode_canonical(&RevokeServiceUserRequest {
                    bridge_actor_id: actor_id,
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            );
            revoke_service_user_handler()(state, caller, payload).await
        };

        let reply_bytes = revoke(state.clone(), admin, pk.to_vec())
            .await
            .expect("revoke ok");
        let reply: RevokeServiceUserReply = decode(&reply_bytes).unwrap();
        assert!(reply.ok);
        let row = state
            .db
            .lookup_bridge_service_user(&pk)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, BridgeStatus::Revoked);

        // Idempotent on an already-revoked bridge.
        revoke(state.clone(), admin, pk.to_vec())
            .await
            .expect("idempotent revoke ok");

        // Unknown actor id → not_found.
        let err = revoke(state.clone(), admin, vec![0xEE; 32])
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.not_found");

        // Bad length → malformed.
        let err = revoke(state.clone(), admin, vec![0x55; 8])
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");

        // Non-admin denied.
        let err = revoke(state, [99u8; 32], pk.to_vec()).await.unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn revoke_and_reject_tear_down_live_authority_not_just_the_row() {
        // transport.md § Revocation teardown: revoking a bridge must kill its
        // bearer, flag its live connections for the 4401 close, and purge its
        // mailbox-state push subscriptions — not merely flip the DB row.
        // Pre-fix, all three revocation call sites did only the flip, so the
        // revoked MDA kept receiving BridgeMailboxState pushes (a Push is not
        // an RPC — no capability gate runs) until its socket happened to die. This pins the *callers*, which the conformance pins that
        // invoke `disconnect_actor` directly structurally cannot.
        let pk = [0x58u8; 32];
        let (state, admin) = admin_state_with_pending(&pk, BridgeRole::Mda).await;
        state
            .db
            .approve_bridge_service_user(&pk, Some(&admin))
            .await
            .unwrap();

        let token = state
            .auth
            .token_store
            .insert(fauna_core::identity::ActorId(pk), 3600)
            .await;
        let (conn, _rx) = state.ws.subscribe(pk);
        state.bridge_push_registry.register(&[1u8; 32], "INBOX", pk);

        let payload = Bytes::from(
            encode_canonical(&RevokeServiceUserRequest {
                bridge_actor_id: pk.to_vec(),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        revoke_service_user_handler()(state.clone(), admin, payload)
            .await
            .expect("revoke ok");

        assert!(
            conn.is_revoked(),
            "the revoked bridge's live WS must be flagged for the 4401 close"
        );
        assert!(
            state.auth.token_store.validate(&token).await.is_none(),
            "the revoked bridge's bearer must be dead"
        );
        assert_eq!(
            state.bridge_push_registry.subscription_count(),
            0,
            "the revoked MDA's mailbox-state subscriptions must be purged — \
             stale entries re-arm the push stream if the leaked key reconnects"
        );

        // The reject twin (the pending-card affordance, which also reaches
        // approved rows) performs the same teardown.
        let pk2 = [0x59u8; 32];
        state
            .db
            .create_pending_bridge_service_user(&pk2, BridgeRole::Mda, "b2")
            .await
            .unwrap();
        state
            .db
            .approve_bridge_service_user(&pk2, Some(&admin))
            .await
            .unwrap();
        let (conn2, _rx2) = state.ws.subscribe(pk2);
        let payload = Bytes::from(
            encode_canonical(&RejectPendingBridgeRequest {
                ed25519_pubkey: pk2.to_vec(),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        reject_pending_bridge_handler()(state.clone(), admin, payload)
            .await
            .expect("reject ok");
        assert!(
            conn2.is_revoked(),
            "reject converges on the same revoke and must tear down the same way"
        );
    }

    #[tokio::test]
    async fn revoking_a_bridge_touches_no_dkim_key() {
        // § Service-user re-keying: the DKIM key is the nest's, so a revoke
        // of either role leaves every selector and its published record as
        // they were.
        let mta_pk = [0x56u8; 32];
        let (state, admin) = admin_state_with_pending(&mta_pk, BridgeRole::Mta).await;
        state
            .db
            .approve_bridge_service_user(&mta_pk, Some(&admin))
            .await
            .unwrap();
        let mda_pk = [0x57u8; 32];
        state
            .db
            .create_pending_bridge_service_user(&mda_pk, BridgeRole::Mda, "mda-1")
            .await
            .unwrap();
        state
            .db
            .approve_bridge_service_user(&mda_pk, Some(&admin))
            .await
            .unwrap();
        state
            .db
            .add_mail_domain("example.test", true, "testing", "none", None, None)
            .await
            .unwrap();
        let before = state.db.list_dkim_selectors(None).await.unwrap();
        assert_eq!(before.len(), 1, "adding the domain minted its key");

        let revoke = |state: Arc<AppState>, actor_id: Vec<u8>| async move {
            let payload = Bytes::from(
                encode_canonical(&RevokeServiceUserRequest {
                    bridge_actor_id: actor_id,
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            );
            revoke_service_user_handler()(state, admin, payload).await
        };

        for (role, pk) in [("mda", mda_pk), ("mta", mta_pk)] {
            revoke(state.clone(), pk.to_vec())
                .await
                .unwrap_or_else(|_| panic!("{role} revoke ok"));
            assert_eq!(
                state.db.list_dkim_selectors(None).await.unwrap(),
                before,
                "a {role} revoke must not touch a DKIM key or its published record"
            );
        }
        assert!(
            crate::mail_dkim_key::OutboundSigner::load(&*state.db.conn().await)
                .unwrap()
                .sign(b"From: a@example.test\r\n\r\nbody\r\n")
                .is_some(),
            "and the nest still signs for the domain"
        );
    }

    #[tokio::test]
    async fn admin_set_mail_enabled_ok_and_non_admin_denied() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();

        // for_test has an empty db_path → the flag-file write is skipped and
        // the supervisor notify is best-effort; the handler still replies ok.
        // (The flag-file + socket behaviour is unit-tested in `mail_enable`.)
        // The DB toggle (Phase E) is persisted regardless of the data dir.
        for enabled in [true, false] {
            let payload = Bytes::from(
                encode_canonical(&SetMailEnabledRequest {
                    enabled,
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            );
            let reply_bytes = set_mail_enabled_handler()(state.clone(), admin, payload)
                .await
                .expect("set_mail_enabled ok");
            let reply: SetMailEnabledReply = decode(&reply_bytes).unwrap();
            assert!(reply.ok);
            // The handler persists the deployment-wide toggle both ways.
            assert_eq!(state.db.get_mail_enabled().await.unwrap(), Some(enabled));
        }

        // Non-admin denied.
        let payload = Bytes::from(
            encode_canonical(&SetMailEnabledRequest {
                enabled: true,
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let err = set_mail_enabled_handler()(state, [99u8; 32], payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    /// The deployment-wide "auto-enable mail for new users" policy
    /// (`mail-policy-config.md` § Tier-2 new-user mail defaults): unset reads as
    /// ON via `setup_status` (works-out-of-box default), the Admin set persists
    /// both ways and `setup_status` reflects it, and a non-admin is denied.
    #[tokio::test]
    async fn admin_set_auto_enable_mail_for_new_users_ok_and_non_admin_denied() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();

        // Unset ⇒ effective ON (the default surfaced on setup.status).
        assert!(
            crate::discovery_core::setup_status_core(&state, true)
                .await
                .auto_enable_mail_for_new_users,
            "unset policy must surface as ON (default-on for new users)"
        );

        for enabled in [false, true] {
            let payload = Bytes::from(
                encode_canonical(&SetAutoEnableMailForNewUsersRequest {
                    enabled,
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            );
            let reply_bytes =
                set_auto_enable_mail_for_new_users_handler()(state.clone(), admin, payload)
                    .await
                    .expect("set_auto_enable_mail_for_new_users ok");
            let reply: SetAutoEnableMailForNewUsersReply = decode(&reply_bytes).unwrap();
            assert!(reply.ok);
            // Persisted both ways …
            assert_eq!(
                state.db.get_auto_enable_mail_for_new_users().await.unwrap(),
                Some(enabled)
            );
            // … and reflected on the client-read surface.
            assert_eq!(
                crate::discovery_core::setup_status_core(&state, true)
                    .await
                    .auto_enable_mail_for_new_users,
                enabled
            );
        }

        // Non-admin denied — it is a deployment default, admin-only.
        let payload = Bytes::from(
            encode_canonical(&SetAutoEnableMailForNewUsersRequest {
                enabled: false,
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let err = set_auto_enable_mail_for_new_users_handler()(state, [99u8; 32], payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    /// Slice 2 (deployment-home-with-public-relay.md § MUA reach): the per-actor
    /// serving toggle is **User-class + caller-scoped** on set (a user flips only
    /// their OWN row; a bridge is denied), default-ON, and **admin read-only**
    /// (an admin may read any actor's flag for audit but a User read is forced to
    /// its own).
    #[tokio::test]
    async fn mail_serving_enabled_user_scoped_set_admin_read_only() {
        let state = fixture_state().await;
        let user_a = [0xA1u8; 32]; // User class (no bridge/admin row)
        let user_b = [0xB1u8; 32];
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        state.db.create_user(&user_a, "free", "test").await.unwrap();

        // Default: a never-set actor reads back enabled = true (absent ⇒ ON).
        let payload = Bytes::from(
            encode_canonical(&GetMailServingEnabledRequest::default())
                .unwrap()
                .to_vec(),
        );
        let reply: GetMailServingEnabledReply = decode(
            &get_mail_serving_enabled_handler()(state.clone(), user_a, payload)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(reply.enabled, "absent flag defaults to ON");

        // A sets its OWN flag OFF (caller-scoped — request carries only `enabled`).
        let payload = Bytes::from(
            encode_canonical(&SetMailServingEnabledRequest {
                enabled: false,
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let reply: SetMailServingEnabledReply = decode(
            &set_mail_serving_enabled_handler()(state.clone(), user_a, payload)
                .await
                .expect("a user may set its own flag"),
        )
        .unwrap();
        assert!(reply.ok);
        assert_eq!(
            state
                .db
                .get_actor_mail_serving_enabled(&user_a)
                .await
                .unwrap(),
            Some(false)
        );
        // B is untouched — caller-scoped, A cannot reach B's row.
        assert_eq!(
            state
                .db
                .get_actor_mail_serving_enabled(&user_b)
                .await
                .unwrap(),
            None
        );

        // A reads back: even naming B in the request, a User read is forced to
        // self → A's own OFF flag (a user cannot read another actor's flag).
        let payload = Bytes::from(
            encode_canonical(&GetMailServingEnabledRequest {
                actor_id: user_b.to_vec(),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let reply: GetMailServingEnabledReply = decode(
            &get_mail_serving_enabled_handler()(state.clone(), user_a, payload)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(
            !reply.enabled,
            "User read forced to self → A's own OFF flag"
        );

        // Admin reads A's flag for the read-only audit view (via actor_id).
        let payload = Bytes::from(
            encode_canonical(&GetMailServingEnabledRequest {
                actor_id: user_a.to_vec(),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let reply: GetMailServingEnabledReply = decode(
            &get_mail_serving_enabled_handler()(state.clone(), admin, payload)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(!reply.enabled, "admin audit sees A's OFF flag");

        // A bridge may NOT set the per-user toggle (User-only allowlist).
        let mda = [0xEEu8; 32];
        approve_bridge(
            &state.db,
            &mda,
            crate::db::bridge_service_users::BridgeRole::Mda,
        )
        .await;
        let payload = Bytes::from(
            encode_canonical(&SetMailServingEnabledRequest {
                enabled: true,
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        let err = set_mail_serving_enabled_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn set_mail_enabled_emits_config_changed_to_approved_bridge() {
        // Hot-reload: flipping the enable toggle nudges the running bridge to
        // re-fetch fetch_config (which reads the persisted toggle). Per
        // `mail-bridge-lifecycle.md` § Running.
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        let mta = [0x71u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let (_c, mut rx) = state.ws.subscribe(mta);

        let payload = Bytes::from(
            encode_canonical(&SetMailEnabledRequest {
                enabled: true,
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        set_mail_enabled_handler()(state.clone(), admin, payload)
            .await
            .expect("set_mail_enabled ok");

        let mut reasons = Vec::new();
        while let Ok(bytes) = rx.try_recv() {
            if let Ok(fauna_protocol::Frame::Push(p)) = fauna_protocol::decode_frame(&bytes)
                && p.kind == fauna_protocol::bridge_routing::PUSH_KIND_BRIDGE_CONFIG_CHANGED
            {
                let pb = encode_canonical(&p.payload).unwrap();
                let push: fauna_protocol::bridge_routing::BridgeConfigChangedPush =
                    decode(&pb).unwrap();
                reasons.push(push.reason);
            }
        }
        assert_eq!(reasons, vec!["mail_enabled".to_string()]);
    }

    #[tokio::test]
    async fn admin_list_dkim_returns_unsealed_metadata() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        assert!(
            state
                .db
                .mint_mail_dkim_key("example.com", "2026a")
                .await
                .unwrap()
        );
        let stored = state.db.list_dkim_selectors(None).await.unwrap();

        let req = ListDkimSelectorsRequest {
            domain: Some("example.com".into()),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = list_dkim_selectors_handler()(state.clone(), admin, payload)
            .await
            .expect("admin list ok");
        let reply: ListDkimSelectorsReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.selectors.len(), 1);
        assert_eq!(reply.selectors[0].selector, "2026a");
        assert_eq!(
            reply.selectors[0].public_dns_value,
            stored[0].public_dns_value
        );
        assert!(
            reply.selectors[0]
                .public_dns_value
                .starts_with("v=DKIM1; k=ed25519; p=")
        );

        // Revoke removes it.
        let revoke_req = RevokeDkimBlobRequest {
            domain: "example.com".into(),
            selector: "2026a".into(),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&revoke_req).unwrap().to_vec());
        let _ = revoke_dkim_blob_handler()(state.clone(), admin, payload)
            .await
            .expect("admin revoke ok");
        assert!(state.db.list_dkim_selectors(None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn provision_tls_cert_requires_admin_class() {
        let state = fixture_state().await;
        let non_admin = [99u8; 32];
        let req = ProvisionTlsCertBlobRequest {
            blob: ByteBuf::from(vec![0u8; 64]),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let h = provision_tls_cert_blob_handler();
        let err = h(state, non_admin, payload).await.unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn fetch_wrapped_mls_blob_returns_stored_bytes() {
        let state = fixture_state().await;
        let target_actor = [42u8; 32];
        let bridge_actor = [55u8; 32];
        approve_bridge(
            &state.db,
            &bridge_actor,
            crate::db::bridge_service_users::BridgeRole::Mda,
        )
        .await;

        state
            .db
            .put_wrapped_mls_blob(&target_actor, "default", &[0xAB; 64])
            .await
            .unwrap();

        let req = FetchWrappedMlsBlobRequest {
            actor_id: target_actor.to_vec(),
            credential_id: "default".into(),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = fetch_wrapped_mls_blob_handler()(state, bridge_actor, payload)
            .await
            .expect("ok");
        let reply: FetchWrappedMlsBlobReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.blob.as_ref().map(|b| b.len()), Some(64));
    }

    #[tokio::test]
    async fn fetch_wrapped_mls_blob_missing_returns_none() {
        let state = fixture_state().await;
        let bridge_actor = [55u8; 32];
        approve_bridge(
            &state.db,
            &bridge_actor,
            crate::db::bridge_service_users::BridgeRole::Mda,
        )
        .await;

        let req = FetchWrappedMlsBlobRequest {
            actor_id: vec![1u8; 32],
            credential_id: "nope".into(),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = fetch_wrapped_mls_blob_handler()(state, bridge_actor, payload)
            .await
            .expect("ok");
        let reply: FetchWrappedMlsBlobReply = decode(&reply_bytes).unwrap();
        assert!(reply.blob.is_none());
    }

    /// Approve an MTA bridge with a *real* X25519 pubkey (so a blob sealed
    /// to it can be unsealed with the matching secret in-test).
    async fn approve_mta_with_x25519(
        db: &crate::db::CacheDb,
        pk: &[u8; 32],
        x25519_pubkey: &[u8; 32],
    ) {
        use crate::db::bridge_service_users::BridgeRole;
        db.create_pending_bridge_service_user(pk, BridgeRole::Mta, "mta-1")
            .await
            .unwrap();
        db.upsert_bridge_x25519(pk, x25519_pubkey).await.unwrap();
        db.approve_bridge_service_user(pk, None).await.unwrap();
    }

    #[tokio::test]
    async fn fetch_tls_cert_seals_even_with_storage_unconfigured() {
        // Regression guard for the fresh-mail-box 465/993 dead-TLS bug
        // (root-caused live 2026-07-05). A box can enable mail — bridges enroll,
        // auto-approve, attest x25519, and ACME writes the apex cert to
        // `acme_dir`, with a `Storage` impl that declines to seal it (its
        // `seal_current_tls_cert_for_bridge` takes the trait default → `Ok(None)`).
        // `fetch_tls_cert_blob` must STILL deliver the cert: it is deployment infra
        // (on-disk PEM + the bridge's x25519), never user data, so the handler
        // seals directly from `state.db` + `state.acme_dir`, not via
        // `state.storage()`. Before the fix this returned None and the MTA/MDA
        // served no TLS (`TLSV1_ALERT_INTERNAL_ERROR`) indefinitely.
        use fauna_mls::wrapped_blob::{TlsCertBlob, generate_x25519_keypair, unseal_tls_cert};

        // Install a storage impl whose seal-on-read returns `Ok(None)` (the trait
        // default), so a cert that IS delivered proves the handler sealed it
        // itself rather than delegating to `state.storage()`.
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let mut base = AppState::for_test(db);
        base.install_storage_for_test(std::sync::Arc::new(crate::storage::DefaultSealsStorage)
            as crate::storage::SharedStorage);
        let state = Arc::new(base);

        // ACME has written the apex cert to disk (the nest's own 443 works).
        let cert_pem = b"-----BEGIN CERTIFICATE-----\nREAL\n-----END CERTIFICATE-----\n";
        let key_pem = b"-----BEGIN PRIVATE KEY-----\nREAL\n-----END PRIVATE KEY-----\n";
        crate::storage::write_acme_pem_atomic(&state.acme_dir, cert_pem, key_pem).unwrap();

        // The MTA enrolled + auto-approved + attested a real x25519.
        let mta_actor = [44u8; 32];
        let (mta_secret, mta_public) = generate_x25519_keypair();
        approve_mta_with_x25519(&state.db, &mta_actor, &mta_public).await;

        let req = FetchTlsCertBlobRequest {
            bridge_role: "mta".into(),
            bridge_id: "mta-1".into(),
            domain: "fauna.example".into(),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = fetch_tls_cert_blob_handler()(state.clone(), mta_actor, payload)
            .await
            .expect("ok");
        let reply: FetchTlsCertBlobReply = decode(&reply_bytes).unwrap();
        let sealed = reply
            .blob
            .expect("cert must seal even when the storage mode is uncommitted")
            .to_vec();

        // The MTA (holding the x25519 secret) recovers the exact on-disk PEM.
        let blob = TlsCertBlob::from_canonical_bytes(&sealed).unwrap();
        let bundle = unseal_tls_cert(&blob, &mta_secret).unwrap();
        assert_eq!(bundle.cert_chain, cert_pem.to_vec());
        assert_eq!(bundle.priv_key, key_pem.to_vec());

        // And it persisted the freshly-sealed blob for the plain `get` fallback.
        assert!(
            state
                .db
                .get_tls_cert_blob("mta", "mta-1", "fauna.example")
                .await
                .unwrap()
                .is_some()
        );
    }

    // ── ATProto identity surface (S2) ────────────────────────────────────

    async fn atproto_fixture() -> (Arc<AppState>, [u8; 32], [u8; 32], [u8; 32]) {
        use crate::db::bridge_service_users::BridgeRole;
        let state = fixture_state().await;
        state
            .identity_domain
            .store(Some(std::sync::Arc::new("fauna.example".to_string())));

        // Approved atproto.pds bridge with an attested x25519.
        let bridge_actor = [0x71u8; 32];
        let (bridge_sk, bridge_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();
        state
            .db
            .create_pending_bridge_service_user(&bridge_actor, BridgeRole::AtprotoPds, "atproto-1")
            .await
            .unwrap();
        state
            .db
            .approve_bridge_service_user(&bridge_actor, None)
            .await
            .unwrap();
        state
            .db
            .upsert_bridge_x25519(&bridge_actor, &bridge_pk)
            .await
            .unwrap();

        // A user with a pending did:plc intent.
        let alice = [0x72u8; 32];
        state
            .db
            .create_user_with_handle(&alice, "free", "alice", None)
            .await
            .unwrap();
        state
            .db
            .upsert_atproto_identity_intent(&alice, "plc", "did:key:zDnaeUSERSENIOR")
            .await
            .unwrap();
        (state, bridge_actor, bridge_sk, alice)
    }

    /// **The torn provision heals, but only where healing destroys nothing.**
    /// Before the blob and its published keys became one transaction, a crash
    /// between the two writes left a blob beside a row recording no keys. The
    /// bridge binds the blob to those keys, so that pair is unopenable — and
    /// provision-on-read, keyed on the blob's absence, would never revisit it.
    /// With no DID minted nothing is published, so the fetch re-provisions; once
    /// a DID exists its keys are in a public document and the stored blob is
    /// served untouched, because re-minting would orphan the DID.
    #[tokio::test]
    async fn a_torn_key_provision_is_reminted_only_before_the_did_exists() {
        let (state, bridge_actor, bridge_sk, alice) = atproto_fixture().await;
        let fetch = |actor: [u8; 32]| {
            let state = state.clone();
            async move {
                let req = FetchAtprotoIdentityKeyBlobRequest {
                    actor_id: ByteBuf::from(actor.to_vec()),
                };
                let reply = fetch_atproto_identity_key_blob_handler()(
                    state.clone(),
                    bridge_actor,
                    Bytes::from(encode_canonical(&req).unwrap().to_vec()),
                )
                .await
                .expect("fetch ok");
                decode::<FetchAtprotoIdentityKeyBlobReply>(&reply).unwrap()
            }
        };

        const TORN: &[u8] = b"torn: a blob, and no published keys";
        state
            .db
            .put_atproto_identity_key_blob(&alice, TORN)
            .await
            .unwrap();
        let healed = fetch(alice).await;
        let blob = fauna_mls::wrapped_blob::AtprotoIdentityBlob::from_canonical_bytes(&healed.blob)
            .expect("the torn bytes were replaced by a real sealed blob");
        let published = fauna_mls::wrapped_blob::AtprotoIdentityPublishedKeys {
            signing_pub_did_key: &healed.signing_pub_did_key,
            rotation_pub_did_key: &healed.bridge_rotation_pub_did_key,
        };
        fauna_mls::wrapped_blob::unseal_atproto_identity(&blob, &bridge_sk, &published)
            .expect("and it opens against the keys provisioned with it");

        // Torn the same way but MINTED: the blob is the DID's only copy of its
        // keys and must survive, however unopenable the pair is.
        let bob = [0x73u8; 32];
        state
            .db
            .create_user_with_handle(&bob, "free", "bob", None)
            .await
            .unwrap();
        state
            .db
            .upsert_atproto_identity_intent(&bob, "plc", "did:key:zDnaeUSERSENIOR")
            .await
            .unwrap();
        state
            .db
            .put_atproto_identity_key_blob(&bob, TORN)
            .await
            .unwrap();
        state
            .db
            .record_atproto_minted(&bob, "did:plc:minted", Some("bafyexample"))
            .await
            .unwrap();
        let kept = fetch(bob).await;
        assert_eq!(
            kept.blob.as_slice(),
            TORN,
            "a minted DID's keys are never re-minted"
        );
        assert_eq!(kept.signing_pub_did_key, "");
    }

    #[tokio::test]
    async fn atproto_roster_provision_mint_lifecycle() {
        let (state, bridge_actor, bridge_sk, alice) = atproto_fixture().await;

        // Roster: handle derived at read, pending, endpoint from the domain.
        let payload = Bytes::from(
            encode_canonical(&FetchAtprotoIdentitiesRequest::default())
                .unwrap()
                .to_vec(),
        );
        let reply_bytes =
            fetch_atproto_identities_handler()(state.clone(), bridge_actor, payload.clone())
                .await
                .expect("roster ok");
        let reply: FetchAtprotoIdentitiesReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.identities.len(), 1);
        let id = &reply.identities[0];
        assert_eq!(id.handle, "alice.fauna.example");
        assert_eq!(id.method, "plc");
        assert_eq!(id.status, "pending");
        assert_eq!(id.did, None);
        assert_eq!(id.user_rotation_pub_did_key, "did:key:zDnaeUSERSENIOR");
        assert_eq!(id.pds_endpoint, "https://pds.fauna.example");

        // Key-blob fetch provisions on first read and is stable on the second.
        let blob_req = FetchAtprotoIdentityKeyBlobRequest {
            actor_id: ByteBuf::from(alice.to_vec()),
        };
        let blob_payload = Bytes::from(encode_canonical(&blob_req).unwrap().to_vec());
        let first = fetch_atproto_identity_key_blob_handler()(
            state.clone(),
            bridge_actor,
            blob_payload.clone(),
        )
        .await
        .expect("provision-on-read ok");
        let first: FetchAtprotoIdentityKeyBlobReply = decode(&first).unwrap();
        assert!(first.signing_pub_did_key.starts_with("did:key:zQ3s"));
        assert!(
            first
                .bridge_rotation_pub_did_key
                .starts_with("did:key:zQ3s")
        );

        // The blob unseals with the bridge secret and self-describes the same
        // pubkeys the reply carries.
        let blob = fauna_mls::wrapped_blob::AtprotoIdentityBlob::from_canonical_bytes(&first.blob)
            .expect("blob decodes");
        let published = fauna_mls::wrapped_blob::AtprotoIdentityPublishedKeys {
            signing_pub_did_key: &first.signing_pub_did_key,
            rotation_pub_did_key: &first.bridge_rotation_pub_did_key,
        };
        let bundle =
            fauna_mls::wrapped_blob::unseal_atproto_identity(&blob, &bridge_sk, &published)
                .expect("bridge can unseal against the reply's own published keys");
        assert_eq!(bundle.actor_id, alice.to_vec());

        let second =
            fetch_atproto_identity_key_blob_handler()(state.clone(), bridge_actor, blob_payload)
                .await
                .expect("second fetch ok");
        let second: FetchAtprotoIdentityKeyBlobReply = decode(&second).unwrap();
        assert_eq!(first.blob, second.blob, "provisioned blob must be stable");

        // Mint report-back: active + did; same-DID retry ok; different DID refused.
        let mint = |did: &str| {
            let req = RecordMintedIdentityRequest {
                actor_id: ByteBuf::from(alice.to_vec()),
                did: did.into(),
                genesis_cid: Some("bafyexample".into()),
            };
            Bytes::from(encode_canonical(&req).unwrap().to_vec())
        };
        record_minted_identity_handler()(state.clone(), bridge_actor, mint("did:plc:abc"))
            .await
            .expect("mint records");
        record_minted_identity_handler()(state.clone(), bridge_actor, mint("did:plc:abc"))
            .await
            .expect("same-DID retry is idempotent");
        record_minted_identity_handler()(state.clone(), bridge_actor, mint("did:plc:OTHER"))
            .await
            .expect_err("a different DID must be refused");

        let reply_bytes = fetch_atproto_identities_handler()(
            state.clone(),
            bridge_actor,
            Bytes::from(
                encode_canonical(&FetchAtprotoIdentitiesRequest::default())
                    .unwrap()
                    .to_vec(),
            ),
        )
        .await
        .unwrap();
        let reply: FetchAtprotoIdentitiesReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.identities[0].status, "active");
        assert_eq!(reply.identities[0].did.as_deref(), Some("did:plc:abc"));
    }

    /// Flow-trace for the rename hook's NEST half (`atproto-pds-bridge.md`
    /// § Identity — "Handle changes follow the Fauna handle", derived at read):
    /// the user's queued `handle.change` executes → `db.set_handle` writes the
    /// new Fauna handle → the bridge's very next `fetch_identities` reports the
    /// re-derived ATProto handle, with the DID untouched (DID-is-data).
    ///
    /// That re-derivation is the whole input to the bridge-side reconcile, so
    /// this pins the link the Go tests take as given. A handle that stayed
    /// frozen here would leave the rename hook permanently idle — silently.
    #[tokio::test]
    async fn atproto_roster_rederives_the_handle_after_a_handle_change() {
        let (state, bridge_actor, _bridge_sk, alice) = atproto_fixture().await;

        let roster = |st: Arc<AppState>| async move {
            let bytes = fetch_atproto_identities_handler()(
                st,
                bridge_actor,
                Bytes::from(
                    encode_canonical(&FetchAtprotoIdentitiesRequest::default())
                        .unwrap()
                        .to_vec(),
                ),
            )
            .await
            .unwrap();
            decode::<FetchAtprotoIdentitiesReply>(&bytes).unwrap()
        };

        state
            .db
            .record_atproto_minted(&alice, "did:plc:renametest", Some("bafygenesis"))
            .await
            .unwrap();
        let before = roster(state.clone()).await;
        assert_eq!(before.identities[0].handle, "alice.fauna.example");

        // The queued pending action executing IS the rename, as far as every
        // derived-at-read consumer is concerned.
        let action = crate::pending_actions::PendingActionRow {
            id: 1,
            action_type: "handle.change".into(),
            actor_id: alice.to_vec(),
            target: None,
            payload: Some(r#"{"new_handle":"renamed"}"#.into()),
            status: "pending".into(),
            created_at: 0,
            execute_after: 0,
            executed_at: None,
            cancelled_by: None,
            cancelled_at: None,
            requires_quorum: 0,
            approvals: "[]".into(),
            ip_address: None,
            chain_hash: None,
        };
        crate::pending_actions::execute_action(&state, &action)
            .await
            .expect("handle.change applies");

        let after = roster(state.clone()).await;
        assert_eq!(
            after.identities[0].handle, "renamed.fauna.example",
            "the ATProto handle must follow the Fauna handle at read time"
        );
        assert_eq!(
            after.identities[0].did.as_deref(),
            Some("did:plc:renametest"),
            "a rename must never re-DID an identity (DID-is-data)"
        );
    }

    #[tokio::test]
    async fn atproto_session_secret_provision_on_read_is_stable() {
        let (state, bridge_actor, bridge_sk, _alice) = atproto_fixture().await;

        let payload = Bytes::from(
            encode_canonical(&FetchAtprotoSessionSecretBlobRequest::default())
                .unwrap()
                .to_vec(),
        );
        let first = fetch_atproto_session_secret_blob_handler()(
            state.clone(),
            bridge_actor,
            payload.clone(),
        )
        .await
        .expect("provision-on-read ok");
        let first: FetchAtprotoSessionSecretBlobReply = decode(&first).unwrap();

        // The blob unseals with the bridge secret to a 32-byte HS256 key,
        // and its AAD index carries the caller's enrollment.
        let blob =
            fauna_mls::wrapped_blob::AtprotoSessionSecretBlob::from_canonical_bytes(&first.blob)
                .expect("blob decodes");
        assert_eq!(blob.index.0, "atproto.pds");
        assert_eq!(blob.index.1, "atproto-1");
        let bundle = fauna_mls::wrapped_blob::unseal_atproto_session_secret(&blob, &bridge_sk)
            .expect("bridge can unseal");
        assert_eq!(bundle.secret.len(), 32);

        // The durability property the restart-survival e2e rides on: a second
        // fetch returns the identical ciphertext, never a re-mint.
        let second =
            fetch_atproto_session_secret_blob_handler()(state.clone(), bridge_actor, payload)
                .await
                .expect("second fetch ok");
        let second: FetchAtprotoSessionSecretBlobReply = decode(&second).unwrap();
        assert_eq!(first.blob, second.blob, "provisioned secret must be stable");
    }

    #[tokio::test]
    async fn atproto_session_secret_denied_without_atproto_enrollment() {
        use crate::db::bridge_service_users::BridgeRole;
        let (state, _bridge_actor, _sk, _alice) = atproto_fixture().await;

        // An approved MTA (wrong role) must be refused by the allowlist gate —
        // the secret is PDS-bridge-only material.
        let mta_actor = [0x79u8; 32];
        let (_mta_sk, mta_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();
        state
            .db
            .create_pending_bridge_service_user(&mta_actor, BridgeRole::Mta, "mta-1")
            .await
            .unwrap();
        state
            .db
            .approve_bridge_service_user(&mta_actor, None)
            .await
            .unwrap();
        state
            .db
            .upsert_bridge_x25519(&mta_actor, &mta_pk)
            .await
            .unwrap();
        let payload = Bytes::from(
            encode_canonical(&FetchAtprotoSessionSecretBlobRequest::default())
                .unwrap()
                .to_vec(),
        );
        fetch_atproto_session_secret_blob_handler()(state.clone(), mta_actor, payload)
            .await
            .expect_err("MTA must not fetch the PDS session secret");
    }

    // NOTE: the retired `enable_identity` kind's coverage (mint-param
    // validation, caller-class denial, self-scope) lives on in
    // `tests/conformance_atproto_integration_level.rs` against the
    // `set_integration_level` transition kind that replaced it.

    #[tokio::test]
    async fn atproto_key_blob_requires_attested_x25519() {
        use crate::db::bridge_service_users::BridgeRole;
        let state = fixture_state().await;
        state
            .identity_domain
            .store(Some(std::sync::Arc::new("fauna.example".to_string())));
        let bridge_actor = [0x73u8; 32];
        state
            .db
            .create_pending_bridge_service_user(&bridge_actor, BridgeRole::AtprotoPds, "atproto-1")
            .await
            .unwrap();
        state
            .db
            .approve_bridge_service_user(&bridge_actor, None)
            .await
            .unwrap();
        let alice = [0x74u8; 32];
        state
            .db
            .create_user_with_handle(&alice, "free", "alice", None)
            .await
            .unwrap();
        state
            .db
            .upsert_atproto_identity_intent(&alice, "plc", "did:key:zDnaeUSER")
            .await
            .unwrap();
        let req = FetchAtprotoIdentityKeyBlobRequest {
            actor_id: ByteBuf::from(alice.to_vec()),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        fetch_atproto_identity_key_blob_handler()(state.clone(), bridge_actor, payload)
            .await
            .expect_err("no attested x25519 → keys must NOT provision");
    }

    #[tokio::test]
    async fn atproto_roster_empty_on_non_public_domain_and_skips_reserved() {
        let (state, bridge_actor, _sk, _alice) = atproto_fixture().await;

        // A reserved-label handle (the admin-claim carve-out can mint one)
        // never derives an ATProto handle — skipped, not errored.
        let mailuser = [0x75u8; 32];
        state
            .db
            .create_user_with_handle(&mailuser, "free", "mail", None)
            .await
            .unwrap();
        state
            .db
            .upsert_atproto_identity_intent(&mailuser, "web", "")
            .await
            .unwrap();
        let payload = Bytes::from(
            encode_canonical(&FetchAtprotoIdentitiesRequest::default())
                .unwrap()
                .to_vec(),
        );
        let reply_bytes =
            fetch_atproto_identities_handler()(state.clone(), bridge_actor, payload.clone())
                .await
                .unwrap();
        let reply: FetchAtprotoIdentitiesReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.identities.len(), 1, "reserved-handle row skipped");
        assert_eq!(reply.identities[0].handle, "alice.fauna.example");

        // Non-public identity domain (localhost fallback) → empty roster.
        state.identity_domain.store(None);
        let reply_bytes = fetch_atproto_identities_handler()(state.clone(), bridge_actor, payload)
            .await
            .unwrap();
        let reply: FetchAtprotoIdentitiesReply = decode(&reply_bytes).unwrap();
        assert!(reply.identities.is_empty());
    }

    #[tokio::test]
    async fn atproto_kinds_denied_for_mail_roles() {
        use crate::db::bridge_service_users::BridgeRole;
        let (state, _bridge, _sk, _alice) = atproto_fixture().await;
        let mta_actor = [0x76u8; 32];
        state
            .db
            .create_pending_bridge_service_user(&mta_actor, BridgeRole::Mta, "mta-1")
            .await
            .unwrap();
        state
            .db
            .approve_bridge_service_user(&mta_actor, None)
            .await
            .unwrap();
        let payload = Bytes::from(
            encode_canonical(&FetchAtprotoIdentitiesRequest::default())
                .unwrap()
                .to_vec(),
        );
        fetch_atproto_identities_handler()(state.clone(), mta_actor, payload)
            .await
            .expect_err("an MTA must not read the atproto identity roster");
    }

    #[tokio::test]
    async fn revoke_wrapped_mls_blob_deletes_own() {
        let state = fixture_state().await;
        let actor = [42u8; 32];
        state.db.create_user(&actor, "free", "test").await.unwrap();
        state
            .db
            .put_wrapped_mls_blob(&actor, "default", &[0xCD; 32])
            .await
            .unwrap();

        let req = RevokeWrappedMlsBlobRequest {
            actor_id: actor.to_vec(),
            credential_id: "default".into(),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let _reply = revoke_wrapped_mls_blob_handler()(state.clone(), actor, payload)
            .await
            .expect("ok");

        assert!(
            state
                .db
                .get_wrapped_mls_blob(&actor, "default")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn revoke_wrapped_mls_blob_user_cannot_revoke_other() {
        let state = fixture_state().await;
        let actor_a = [1u8; 32];
        let actor_b = [2u8; 32];
        state
            .db
            .put_wrapped_mls_blob(&actor_a, "default", &[0xCD; 32])
            .await
            .unwrap();

        let req = RevokeWrappedMlsBlobRequest {
            actor_id: actor_a.to_vec(),
            credential_id: "default".into(),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = revoke_wrapped_mls_blob_handler()(state, actor_b, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── fauna.capabilities.* handler tests ──────────────────────────

    /// Build a canonical `GrantBlob` for a test (opaque to the nest — only the
    /// header fields it reads are meaningful here).
    fn build_grant_blob(
        owner: &[u8; 32],
        grant_id: &[u8; 16],
        holder_pubkey: &[u8; 32],
        epoch_end: u64,
        wrapped_keys: Vec<fauna_mls::wrapped_blob::WrappedScopeKey>,
    ) -> Vec<u8> {
        use fauna_mls::wrapped_blob::{GrantBlob, GrantIndex, GrantWindow};
        GrantBlob {
            version: 1,
            kind: GrantBlob::KIND.to_string(),
            index: GrantIndex(owner.to_vec(), grant_id.to_vec()),
            holder: ByteBuf::from(holder_pubkey.to_vec()),
            window: GrantWindow(0, epoch_end),
            scope: Vec::new(),
            wrapped_keys,
        }
        .to_canonical_bytes()
        .expect("encode grant blob")
    }

    async fn approve_content_processor_with_x25519(
        db: &crate::db::CacheDb,
        pk: &[u8; 32],
        x25519_pubkey: &[u8; 32],
        bridge_id: &str,
    ) {
        use crate::db::bridge_service_users::BridgeRole;
        approve_bridge_with_x25519(
            db,
            pk,
            BridgeRole::ContentProcessor,
            x25519_pubkey,
            bridge_id,
        )
        .await;
    }

    fn fetch_payload() -> Bytes {
        Bytes::from(
            encode_canonical(&FetchGrantsRequest::default())
                .unwrap()
                .to_vec(),
        )
    }

    #[tokio::test]
    async fn capability_mint_fetch_revoke_round_trip() {
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let owner = [7u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        let grant_id = [0x11u8; 16];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;

        // Mint as the owning user (a non-bridge, non-admin actor ⇒ User class).
        let blob = build_grant_blob(&owner, &grant_id, &cp_public, u64::MAX, Vec::new());
        let mint_req = MintGrantRequest {
            grant_blob: ByteBuf::from(blob.clone()),
            extra: Default::default(),
        };
        let mint_payload = Bytes::from(encode_canonical(&mint_req).unwrap().to_vec());
        let mint_reply: MintGrantReply = decode(
            &mint_grant_handler()(state.clone(), owner, mint_payload)
                .await
                .expect("mint ok"),
        )
        .unwrap();
        assert!(mint_reply.ok);
        assert_eq!(mint_reply.grant_id.as_ref(), &grant_id[..]);

        // Fetch as the content-processor holder returns the opaque blob verbatim.
        let fetch_reply: FetchGrantsReply = decode(
            &fetch_grants_handler()(state.clone(), cp_actor, fetch_payload())
                .await
                .expect("fetch ok"),
        )
        .unwrap();
        assert_eq!(fetch_reply.grants.len(), 1);
        assert_eq!(fetch_reply.grants[0].as_ref(), &blob[..]);

        // Revoke as owner → the holder's next fetch is empty (goes dark).
        let revoke_req = RevokeGrantRequest {
            grant_id: ByteBuf::from(grant_id.to_vec()),
            extra: Default::default(),
        };
        let revoke_payload = Bytes::from(encode_canonical(&revoke_req).unwrap().to_vec());
        let revoke_reply: RevokeGrantReply = decode(
            &revoke_grant_handler()(state.clone(), owner, revoke_payload)
                .await
                .expect("revoke ok"),
        )
        .unwrap();
        assert!(revoke_reply.ok);
        let after: FetchGrantsReply = decode(
            &fetch_grants_handler()(state.clone(), cp_actor, fetch_payload())
                .await
                .expect("fetch ok"),
        )
        .unwrap();
        assert!(after.grants.is_empty());
    }

    #[tokio::test]
    async fn revoke_spam_model_grant_deletes_holder_copy() {
        // The keyless content.read{spam-model} grant's paired sealed-to-holder
        // model copy is deleted on revoke — and ONLY a spam-model-scoped
        // revoke touches it (`mail-spam.md` § Encrypted-mode interaction,
        // ratified 2026-07-13).
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let owner = [7u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;

        let sm_grant = [0x21u8; 16];
        let mail_grant = [0x22u8; 16];
        let sm_blob = build_grant_blob_with_scope(
            &owner,
            &sm_grant,
            &cp_public,
            vec![scope_tuple("content.read", "spam-model")],
        );
        let mail_blob = build_grant_blob_with_scope(
            &owner,
            &mail_grant,
            &cp_public,
            vec![scope_tuple("content.read", "mail")],
        );
        state
            .db
            .put_capability_grant(&owner, &sm_grant, &cp_public, i64::MAX, &sm_blob)
            .await
            .unwrap();
        state
            .db
            .put_capability_grant(&owner, &mail_grant, &cp_public, i64::MAX, &mail_blob)
            .await
            .unwrap();
        // Seed the contributor's sealed-to-holder copy (what an opted-in
        // put_spam_model write stores).
        state
            .db
            .put_spam_model_with_history(
                &owner,
                b"sealed-model",
                None,
                Some((&cp_public, b"sealed-copy")),
            )
            .await
            .unwrap();
        assert_eq!(
            state
                .db
                .list_spam_model_holder_copies(&cp_public)
                .await
                .unwrap()
                .len(),
            1
        );

        let revoke = |grant_id: [u8; 16]| {
            let req = RevokeGrantRequest {
                grant_id: ByteBuf::from(grant_id.to_vec()),
                extra: Default::default(),
            };
            Bytes::from(encode_canonical(&req).unwrap().to_vec())
        };
        // Revoking the MAIL grant leaves the copy (different scope kind).
        revoke_grant_handler()(state.clone(), owner, revoke(mail_grant))
            .await
            .expect("revoke mail ok");
        assert_eq!(
            state
                .db
                .list_spam_model_holder_copies(&cp_public)
                .await
                .unwrap()
                .len(),
            1,
            "a mail-scoped revoke must not touch the spam-model copy"
        );
        // Revoking the SPAM-MODEL grant deletes it.
        revoke_grant_handler()(state.clone(), owner, revoke(sm_grant))
            .await
            .expect("revoke spam-model ok");
        assert!(
            state
                .db
                .list_spam_model_holder_copies(&cp_public)
                .await
                .unwrap()
                .is_empty(),
            "the revoked grant's paired holder copy is gone"
        );
    }

    /// Revoking the `content.read{spam-model}` grant is the fourth way a
    /// summed contributor leaves the deployment baseline, and like the other
    /// three it withdraws the published sum at once (`mail-spam.md` § Cold start
    /// Path 2 → *A contributor's departure withdraws the baseline*). A revoke of
    /// any other scope is not a departure.
    #[tokio::test]
    async fn revoking_a_contributors_spam_model_grant_withdraws_the_published_baseline() {
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let owner = [7u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let copy = crate::test_support::small_spam_model("cheap pills", "lunch agenda", 3);
        seed_sealed_contributor(&state, &owner, &cp_public, &copy).await;
        let sm_grant = [0x21u8; 16];
        let mail_grant = [0x22u8; 16];
        grant_spam_model_read(&state, &owner, &sm_grant, &cp_public).await;
        let mail_blob = build_grant_blob_with_scope(
            &owner,
            &mail_grant,
            &cp_public,
            vec![scope_tuple("content.read", "mail")],
        );
        state
            .db
            .put_capability_grant(&owner, &mail_grant, &cp_public, i64::MAX, &mail_blob)
            .await
            .unwrap();
        // A published sum the owner's counts are part of — landed with its
        // inclusion record, which names the owner as summed, so the revoke is
        // judged on the summed row.
        let departures = state
            .db
            .snapshot_spam_baseline_run()
            .await
            .unwrap()
            .state
            .departures;
        let model = copy.to_bytes();
        let summed = [(owner, 1)];
        let landing = crate::db::spam_baseline::BaselineLanding {
            model_json: &model,
            ham_count: 3,
            spam_count: 3,
            contributors: 3,
            skipped_contributors: 0,
            summed: &summed,
        };
        assert!(
            state
                .db
                .land_spam_baseline_publish(&landing, departures)
                .await
                .unwrap()
        );

        let revoke = |grant_id: [u8; 16]| {
            let req = RevokeGrantRequest {
                grant_id: ByteBuf::from(grant_id.to_vec()),
                extra: Default::default(),
            };
            Bytes::from(encode_canonical(&req).unwrap().to_vec())
        };
        revoke_grant_handler()(state.clone(), owner, revoke(mail_grant))
            .await
            .expect("revoke mail ok");
        assert!(
            state.db.get_spam_baseline().await.unwrap().is_some(),
            "a mail-scoped revoke is not a departure from the baseline"
        );
        revoke_grant_handler()(state.clone(), owner, revoke(sm_grant))
            .await
            .expect("revoke spam-model ok");
        assert_eq!(
            state.db.get_spam_baseline().await.unwrap(),
            None,
            "the contributor revoked the read their counts were summed under"
        );
        let snap = state.db.snapshot_spam_baseline_run().await.unwrap();
        assert!(
            snap.inclusions[&owner].departed,
            "a revoke leaves the account standing: the row is marked, not purged"
        );
    }

    #[tokio::test]
    async fn capability_mint_quota_maps_to_malformed_not_internal() {
        // The 257th distinct grant_id trips MAX_GRANTS_PER_OWNER; the handler
        // must surface it as client-actionable `malformed` ("revoke something
        // first"), never the retry-inviting `internal`.
        let state = fixture_state().await;
        let owner = [7u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        for i in 0..crate::db::capability_grants::MAX_GRANTS_PER_OWNER {
            let mut grant_id = [0u8; 16];
            grant_id[..8].copy_from_slice(&(i as u64).to_be_bytes());
            state
                .db
                .put_capability_grant(&owner, &grant_id, &[9u8; 32], i64::MAX, b"blob")
                .await
                .expect("mint under quota");
        }
        let blob = build_grant_blob(&owner, &[0xFFu8; 16], &[9u8; 32], u64::MAX, Vec::new());
        let req = MintGrantRequest {
            grant_blob: ByteBuf::from(blob),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = mint_grant_handler()(state.clone(), owner, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
        let details = format!("{:?}", err.details);
        assert!(
            details.contains("too many capability grants"),
            "details should name the quota: {details}"
        );
    }

    #[tokio::test]
    async fn capability_mint_rejects_non_owner() {
        let state = fixture_state().await;
        let owner = [7u8; 32];
        let attacker = [8u8; 32];
        // A blob claiming `owner` as its owner, minted by `attacker`.
        let blob = build_grant_blob(&owner, &[1u8; 16], &[9u8; 32], u64::MAX, Vec::new());
        let req = MintGrantRequest {
            grant_blob: ByteBuf::from(blob),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = mint_grant_handler()(state.clone(), attacker, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn capability_fetch_is_scoped_to_callers_own_holder_pubkey() {
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let owner = [7u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        let (_sa, pub_a) = generate_x25519_keypair();
        let (_sb, pub_b) = generate_x25519_keypair();
        let cp_a = [0xA0u8; 32];
        let cp_b = [0xB0u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_a, &pub_a, "cp-a").await;
        approve_content_processor_with_x25519(&state.db, &cp_b, &pub_b, "cp-b").await;

        // Mint a grant sealed to holder A only.
        let blob = build_grant_blob(&owner, &[1u8; 16], &pub_a, u64::MAX, Vec::new());
        let mint_req = MintGrantRequest {
            grant_blob: ByteBuf::from(blob),
            extra: Default::default(),
        };
        let mp = Bytes::from(encode_canonical(&mint_req).unwrap().to_vec());
        mint_grant_handler()(state.clone(), owner, mp)
            .await
            .expect("mint ok");

        // A gets it; B (a different enrolled holder) gets nothing — the handler
        // keys on the CALLER's enrolled x25519, not any spoofable request field.
        let a: FetchGrantsReply = decode(
            &fetch_grants_handler()(state.clone(), cp_a, fetch_payload())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(a.grants.len(), 1);
        let b: FetchGrantsReply = decode(
            &fetch_grants_handler()(state.clone(), cp_b, fetch_payload())
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(b.grants.is_empty());
    }

    #[tokio::test]
    async fn capability_renew_revives_window_and_dedups_appended_keys() {
        use fauna_mls::wrapped_blob::{ScopeTuple, generate_x25519_keypair, seal_capability};
        let state = fixture_state().await;
        let owner = [7u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        let grant_id = [0x22u8; 16];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x5Au8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;

        // Mint an ALREADY-EXPIRED grant (epoch_end = 1 ≪ now), so a plain fetch
        // omits it.
        let blob = build_grant_blob(&owner, &grant_id, &cp_public, 1, Vec::new());
        let mp = Bytes::from(
            encode_canonical(&MintGrantRequest {
                grant_blob: ByteBuf::from(blob),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        );
        mint_grant_handler()(state.clone(), owner, mp)
            .await
            .expect("mint ok");
        let before: FetchGrantsReply = decode(
            &fetch_grants_handler()(state.clone(), cp_actor, fetch_payload())
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(before.grants.is_empty(), "expired grant omitted from fetch");

        // Renew: bump the window far into the future + append one real wrapped key.
        let scope = ScopeTuple {
            class: "content.read".into(),
            kind: Some("mail".into()),
            tier: None,
            set: None,
            factor: None,
        };
        let wk = seal_capability(&[0u8; 32], &owner, &scope, None, &cp_public).unwrap();
        let wk_bytes = wk.to_canonical_bytes().unwrap();
        let renew_of = |keys: Vec<ByteBuf>| {
            Bytes::from(
                encode_canonical(&RenewGrantRequest {
                    grant_id: ByteBuf::from(grant_id.to_vec()),
                    new_epoch_start: None,
                    new_epoch_end: u64::MAX,
                    appended_keys: keys,
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            )
        };
        let renew_reply: RenewGrantReply = decode(
            &renew_grant_handler()(
                state.clone(),
                owner,
                renew_of(vec![ByteBuf::from(wk_bytes.clone())]),
            )
            .await
            .expect("renew ok"),
        )
        .unwrap();
        assert!(renew_reply.ok);

        // Live again, carrying exactly one wrapped key with the bumped window.
        let after: FetchGrantsReply = decode(
            &fetch_grants_handler()(state.clone(), cp_actor, fetch_payload())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(after.grants.len(), 1);
        // window-ok(test: asserts on a blob the handler already served).
        let stored =
            fauna_mls::wrapped_blob::GrantBlob::from_canonical_bytes(after.grants[0].as_ref())
                .unwrap();
        assert_eq!(stored.window.1, u64::MAX);
        assert_eq!(stored.wrapped_keys.len(), 1);

        // Replay the SAME renew — the append dedups by (scope, epoch): still one.
        renew_grant_handler()(
            state.clone(),
            owner,
            renew_of(vec![ByteBuf::from(wk_bytes)]),
        )
        .await
        .expect("renew replay ok");
        let after2: FetchGrantsReply = decode(
            &fetch_grants_handler()(state.clone(), cp_actor, fetch_payload())
                .await
                .unwrap(),
        )
        .unwrap();
        // window-ok(test: asserts on a blob the handler already served).
        let stored2 =
            fauna_mls::wrapped_blob::GrantBlob::from_canonical_bytes(after2.grants[0].as_ref())
                .unwrap();
        assert_eq!(
            stored2.wrapped_keys.len(),
            1,
            "idempotent append (dedup by scope, epoch)"
        );
    }

    #[tokio::test]
    async fn capability_renew_heals_cross_root_by_replacing_differing_bytes_same_scope_epoch() {
        // after an MSEK
        // hard-revoke the client re-derives + re-wraps under the NEW root for
        // an epoch it already held. A same-(scope,epoch) dedup that only
        // skips (never replaces) would leave the holder stuck under the dead
        // root forever — the § 5 "heals via renew" story would be false.
        use fauna_mls::wrapped_blob::{ScopeTuple, generate_x25519_keypair, seal_capability};
        let state = fixture_state().await;
        let owner = [7u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        let grant_id = [0x33u8; 16];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x5Bu8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-2").await;

        let scope = ScopeTuple {
            class: "content.read".into(),
            kind: Some("mail".into()),
            tier: None,
            set: None,
            factor: None,
        };
        // The OLD-root wrap for epoch 5.
        let old_wk = seal_capability(&[0xAAu8; 32], &owner, &scope, Some(5), &cp_public).unwrap();
        let blob = build_grant_blob(&owner, &grant_id, &cp_public, u64::MAX, vec![old_wk]);
        mint_grant_handler()(
            state.clone(),
            owner,
            Bytes::from(
                encode_canonical(&MintGrantRequest {
                    grant_blob: ByteBuf::from(blob),
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("mint ok");

        // Renew appends the SAME (scope, epoch) but under a NEW root (as if
        // MSEK just hard-revoked and the client re-derived) — different bytes.
        let new_wk = seal_capability(&[0xBBu8; 32], &owner, &scope, Some(5), &cp_public).unwrap();
        let new_wk_bytes = new_wk.to_canonical_bytes().unwrap();
        let renew_req = RenewGrantRequest {
            grant_id: ByteBuf::from(grant_id.to_vec()),
            new_epoch_start: None,
            new_epoch_end: u64::MAX,
            appended_keys: vec![ByteBuf::from(new_wk_bytes.clone())],
            extra: Default::default(),
        };
        let reply: RenewGrantReply = decode(
            &renew_grant_handler()(
                state.clone(),
                owner,
                Bytes::from(encode_canonical(&renew_req).unwrap().to_vec()),
            )
            .await
            .expect("renew heals cross-root"),
        )
        .unwrap();
        assert!(reply.ok);

        let after: FetchGrantsReply = decode(
            &fetch_grants_handler()(state.clone(), cp_actor, fetch_payload())
                .await
                .unwrap(),
        )
        .unwrap();
        // window-ok(test: asserts on a blob the handler already served).
        let stored =
            fauna_mls::wrapped_blob::GrantBlob::from_canonical_bytes(after.grants[0].as_ref())
                .unwrap();
        assert_eq!(
            stored.wrapped_keys.len(),
            1,
            "still one wrap for (scope, epoch=5) — replaced in place, not appended alongside"
        );
        assert_eq!(
            stored.wrapped_keys[0].to_canonical_bytes().unwrap(),
            new_wk_bytes,
            "the stored wrap is the NEW-root bytes — the old-root wrap was healed, not left dark"
        );
    }

    #[tokio::test]
    async fn capability_renew_rejects_regime_crossing_append() {
        // INFO-C/INFO-E: the mint-side XOR guard is per-call and can't see
        // this — a renew appending a pure standing (epoch: None) key passes
        // in isolation yet would mix regimes against the grant's EXISTING
        // per-epoch wraps. The (scope, epoch) dedup can't catch it either
        // (`None` never matches `Some`).
        use fauna_mls::wrapped_blob::{ScopeTuple, generate_x25519_keypair, seal_capability};
        let state = fixture_state().await;
        let owner = [7u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        let grant_id = [0x44u8; 16];
        let (_cp_secret, cp_public) = generate_x25519_keypair();

        let scope = ScopeTuple {
            class: "content.read".into(),
            kind: Some("mail".into()),
            tier: None,
            set: None,
            factor: None,
        };
        // A bounded grant: one per-epoch (epoch: Some) wrap, no standing wrap.
        let bounded_wk =
            seal_capability(&[0xCCu8; 32], &owner, &scope, Some(9), &cp_public).unwrap();
        let blob = build_grant_blob(&owner, &grant_id, &cp_public, u64::MAX, vec![bounded_wk]);
        mint_grant_handler()(
            state.clone(),
            owner,
            Bytes::from(
                encode_canonical(&MintGrantRequest {
                    grant_blob: ByteBuf::from(blob),
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("mint ok");

        let standing_wk = seal_capability(&[0xDDu8; 32], &owner, &scope, None, &cp_public).unwrap();
        let renew_req = RenewGrantRequest {
            grant_id: ByteBuf::from(grant_id.to_vec()),
            new_epoch_start: None,
            new_epoch_end: u64::MAX,
            appended_keys: vec![ByteBuf::from(standing_wk.to_canonical_bytes().unwrap())],
            extra: Default::default(),
        };
        let err = renew_grant_handler()(
            state.clone(),
            owner,
            Bytes::from(encode_canonical(&renew_req).unwrap().to_vec()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
        let details = format!("{:?}", err.details);
        assert!(
            details.contains("mix"),
            "details should name the regime-crossing refusal: {details}"
        );
    }

    /// A bounded grant's window is never wider than its wraps: a renew moving
    /// its end across a sealing-epoch boundary without the new epoch's wrap is
    /// refused (a keyless bump would leave that epoch silently dark to the
    /// holder), and the same renew carrying the wrap is accepted. An equal-end
    /// renew — the rotation heal's shape — extends nothing and passes.
    #[tokio::test]
    async fn capability_renew_rejects_an_uncovered_bounded_extension() {
        use fauna_mls::wrapped_blob::{
            MAIL_SEALING_EPOCH_SECS, ScopeTuple, generate_x25519_keypair, seal_capability,
        };
        let state = fixture_state().await;
        let owner = [7u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        let grant_id = [0x45u8; 16];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let scope = ScopeTuple {
            class: "content.read".into(),
            kind: Some("mail".into()),
            tier: None,
            set: None,
            factor: None,
        };
        let wrap = |epoch: u64| {
            ByteBuf::from(
                seal_capability(&[0xCCu8; 32], &owner, &scope, Some(epoch), &cp_public)
                    .unwrap()
                    .to_canonical_bytes()
                    .unwrap(),
            )
        };
        let week = MAIL_SEALING_EPOCH_SECS;
        // Bounded to epoch 9, ending late in it.
        let old_end = 10 * week - 1;
        let bounded_wk =
            seal_capability(&[0xCCu8; 32], &owner, &scope, Some(9), &cp_public).unwrap();
        let blob = build_grant_blob(&owner, &grant_id, &cp_public, old_end, vec![bounded_wk]);
        mint_grant_handler()(
            state.clone(),
            owner,
            Bytes::from(
                encode_canonical(&MintGrantRequest {
                    grant_blob: ByteBuf::from(blob),
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("mint ok");
        let renew = |new_epoch_end: u64, appended_keys: Vec<ByteBuf>| {
            let req = RenewGrantRequest {
                grant_id: ByteBuf::from(grant_id.to_vec()),
                new_epoch_start: None,
                new_epoch_end,
                appended_keys,
                extra: Default::default(),
            };
            renew_grant_handler()(
                state.clone(),
                owner,
                Bytes::from(encode_canonical(&req).unwrap().to_vec()),
            )
        };

        // Into epoch 10 with no wrap for it: refused, naming the epoch.
        let err = renew(11 * week - 1, Vec::new()).await.unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
        let details = format!("{:?}", err.details);
        assert!(
            details.contains("epoch 10"),
            "details should name the uncovered epoch: {details}"
        );
        // Equal-end: a pure key refresh extends nothing.
        renew(old_end, vec![wrap(9)])
            .await
            .expect("equal-end renew ok");
        // The same extension carrying epoch 10's wrap: accepted.
        renew(11 * week - 1, vec![wrap(10)])
            .await
            .expect("covered extension ok");
        // Two epochs further with only one of them: refused at the gap.
        let err = renew(13 * week - 1, vec![wrap(12)]).await.unwrap_err();
        assert!(
            format!("{:?}", err.details).contains("epoch 11"),
            "the first uncovered epoch is named"
        );
    }

    #[tokio::test]
    async fn capability_renew_rejects_window_shrink() {
        // Narrowing a window is revocation's job — append-only wraps make the
        // crypto bound exceed a shrunk window.
        let state = fixture_state().await;
        let owner = [7u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        let grant_id = [0x55u8; 16];
        let blob = build_grant_blob(&owner, &grant_id, &[9u8; 32], 1000, Vec::new());
        mint_grant_handler()(
            state.clone(),
            owner,
            Bytes::from(
                encode_canonical(&MintGrantRequest {
                    grant_blob: ByteBuf::from(blob),
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("mint ok");

        let renew_req = RenewGrantRequest {
            grant_id: ByteBuf::from(grant_id.to_vec()),
            new_epoch_start: None,
            new_epoch_end: 500, // shrink from 1000
            appended_keys: Vec::new(),
            extra: Default::default(),
        };
        let err = renew_grant_handler()(
            state.clone(),
            owner,
            Bytes::from(encode_canonical(&renew_req).unwrap().to_vec()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
        let details = format!("{:?}", err.details);
        assert!(
            details.contains("shrink"),
            "details should name the shrink refusal: {details}"
        );
    }

    #[tokio::test]
    async fn capability_renew_rejects_absent_grant() {
        let state = fixture_state().await;
        let owner = [7u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        let req = RenewGrantRequest {
            grant_id: ByteBuf::from(vec![0xEEu8; 16]),
            new_epoch_start: None,
            new_epoch_end: u64::MAX,
            appended_keys: Vec::new(),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = renew_grant_handler()(state.clone(), owner, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.not_found");
    }

    /// The retention ruling end to end (`encryption-at-rest.md` § Capability
    /// tiering → *Content-sealing epochs*): a bounded mail grant to a
    /// post-quantum holder (the largest wrap) is renewed weekly for 60
    /// epochs the way the client renew driver does it — the end moves to
    /// `now + L`, the start re-centres to `now - L`, the appended wraps cover
    /// exactly the newly covered epochs — and every renewal is accepted, the
    /// held epoch set is exactly the slid window's, and the blob never
    /// approaches the cap. Before the ruling nothing pruned, so the same loop
    /// was refused as "too large after renew" within a few months.
    #[tokio::test]
    async fn capability_renew_slides_a_bounded_grant_through_sixty_weekly_epochs() {
        use fauna_mls::wrapped_blob::{
            GrantBlob, GrantIndex, GrantWindow, MAIL_SEALING_EPOCH_SECS, ScopeTuple,
            bounded_mail_epoch_wraps, bounded_mail_epoch_wraps_for_range, build_renewal_wraps,
            derive_bridge_service_user_mlkem768, generate_x25519_keypair, mail_sealing_epoch_of,
        };
        let state = fixture_state().await;
        let owner = [7u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        let grant_id = [0x60u8; 16];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let (_dk, cp_mlkem_ek) = derive_bridge_service_user_mlkem768(&[0x61u8; 32]);
        let cp_actor = [0x62u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-pq").await;

        const WEEK: u64 = MAIL_SEALING_EPOCH_SECS;
        // The standard grant length (`fauna_client_capabilities::DEFAULT_GRANT_WINDOW_SECS`).
        const L: u64 = 90 * 24 * 60 * 60;
        let msek = [0x9Du8; 32];
        let mail = ScopeTuple {
            class: "content.read".into(),
            kind: Some("mail".into()),
            tier: None,
            set: None,
            factor: None,
        };
        // Mint at a real instant so the fetch's expiry filter keeps it live.
        let mint_at = u64::try_from(crate::db::now_epoch_secs()).expect("unix seconds");
        let window = GrantWindow(mint_at, mint_at + L);
        let wraps = build_renewal_wraps(
            &owner,
            &cp_public,
            Some(&cp_mlkem_ek),
            &[(mail.clone(), bounded_mail_epoch_wraps(&msek, &[], &window))],
        )
        .expect("mint-time wraps");
        let blob = GrantBlob {
            version: 1,
            kind: GrantBlob::KIND.to_string(),
            index: GrantIndex(owner.to_vec(), grant_id.to_vec()),
            holder: ByteBuf::from(cp_public.to_vec()),
            window: window.clone(),
            scope: vec![mail.clone()],
            wrapped_keys: wraps,
        }
        .to_canonical_bytes()
        .expect("encode grant blob");
        let mint_bytes = blob.len();
        mint_grant_handler()(
            state.clone(),
            owner,
            Bytes::from(
                encode_canonical(&MintGrantRequest {
                    grant_blob: ByteBuf::from(blob),
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("mint ok");

        let mut old_start = window.0;
        let mut old_end = window.1;
        let mut largest = mint_bytes;
        let mut most_wraps = 0usize;
        for week in 1..=60u64 {
            // The client's renewal window (`grant_log::renewal_window`): one
            // mint-length ahead, one behind, the start never moving back.
            let now = mint_at + week * WEEK;
            let new_end = now + L;
            let new_start = old_start.max(now.saturating_sub(L));
            let first_new = mail_sealing_epoch_of(old_end) + 1;
            let last = mail_sealing_epoch_of(new_end);
            let appended = build_renewal_wraps(
                &owner,
                &cp_public,
                Some(&cp_mlkem_ek),
                &[(
                    mail.clone(),
                    bounded_mail_epoch_wraps_for_range(&msek, &[], first_new..=last),
                )],
            )
            .expect("renewal wraps")
            .iter()
            .map(|w| ByteBuf::from(w.to_canonical_bytes().expect("encode wrap")))
            .collect();
            let req = RenewGrantRequest {
                grant_id: ByteBuf::from(grant_id.to_vec()),
                new_epoch_start: Some(new_start),
                new_epoch_end: new_end,
                appended_keys: appended,
                extra: Default::default(),
            };
            let reply: RenewGrantReply = decode(
                &renew_grant_handler()(
                    state.clone(),
                    owner,
                    Bytes::from(encode_canonical(&req).unwrap().to_vec()),
                )
                .await
                .unwrap_or_else(|e| panic!("week {week}: renew refused: {:?}", e.details)),
            )
            .unwrap();
            assert!(reply.ok, "week {week}");

            let stored_bytes = state
                .db
                .get_capability_grant(&owner, &grant_id)
                .await
                .unwrap()
                .expect("grant row");
            largest = largest.max(stored_bytes.len());
            // window-ok(test: asserts on the blob the handler stored).
            let stored = GrantBlob::from_canonical_bytes(&stored_bytes).unwrap();
            assert_eq!(
                (stored.window.0, stored.window.1),
                (new_start, new_end),
                "week {week}: the stored window is the slid one"
            );
            let mut held: Vec<u64> = stored.wrapped_keys.iter().filter_map(|k| k.epoch).collect();
            held.sort_unstable();
            let expected: Vec<u64> =
                (mail_sealing_epoch_of(new_start)..=mail_sealing_epoch_of(new_end)).collect();
            assert_eq!(
                held, expected,
                "week {week}: the held epoch set is exactly the window's epochs"
            );
            most_wraps = most_wraps.max(stored.wrapped_keys.len());
            old_start = new_start;
            old_end = new_end;
        }
        // The window is never wider than 2L, so the wrap count is bounded by
        // the calendar (2L/week + the two partial edge epochs), not by how many
        // times the grant was renewed.
        assert!(
            most_wraps <= (2 * L / WEEK + 2) as usize,
            "at most {} wraps for a 2L window, saw {most_wraps}",
            2 * L / WEEK + 2
        );
        assert!(
            largest <= crate::db::capability_grants::MAX_CAPABILITY_GRANT_BYTES / 2,
            "sixty renewals peak at {largest} bytes — more than half the {} cap",
            crate::db::capability_grants::MAX_CAPABILITY_GRANT_BYTES
        );
        // And the premise the ruling fixes, measured: ~27 post-quantum wraps
        // exceed the retired 64 KiB cap, which is why the cap moved with the
        // ruling rather than the window alone.
        assert!(
            largest > 64 * 1024,
            "a post-quantum 2L window ({largest} bytes) would not have fit the retired cap"
        );
    }

    /// The slide is one-way and optional: a start behind the stored one is
    /// refused (widening into the past needs wraps the renew cannot prove), a
    /// start past the end is refused, and a request without a start — a
    /// client that does not slide — keeps the stored start.
    #[tokio::test]
    async fn capability_renew_refuses_a_backward_window_start_and_keeps_it_when_absent() {
        use fauna_mls::wrapped_blob::{
            GrantBlob, GrantIndex, GrantWindow, generate_x25519_keypair,
        };
        let state = fixture_state().await;
        let owner = [7u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        let grant_id = [0x63u8; 16];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x64u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-2").await;
        let blob = GrantBlob {
            version: 1,
            kind: GrantBlob::KIND.to_string(),
            index: GrantIndex(owner.to_vec(), grant_id.to_vec()),
            holder: ByteBuf::from(cp_public.to_vec()),
            window: GrantWindow(1000, 2000),
            scope: Vec::new(),
            wrapped_keys: Vec::new(),
        }
        .to_canonical_bytes()
        .expect("encode grant blob");
        mint_grant_handler()(
            state.clone(),
            owner,
            Bytes::from(
                encode_canonical(&MintGrantRequest {
                    grant_blob: ByteBuf::from(blob),
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("mint ok");
        let renew = |new_epoch_start: Option<u64>, new_epoch_end: u64| {
            Bytes::from(
                encode_canonical(&RenewGrantRequest {
                    grant_id: ByteBuf::from(grant_id.to_vec()),
                    new_epoch_start,
                    new_epoch_end,
                    appended_keys: Vec::new(),
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            )
        };
        let stored_window = |bytes: &[u8]| {
            // window-ok(test: asserts on the blob the handler stored).
            let b = GrantBlob::from_canonical_bytes(bytes).unwrap();
            (b.window.0, b.window.1)
        };

        let reason = |err: &fauna_protocol::RpcError| format!("{:?}", err.details);
        let err = renew_grant_handler()(state.clone(), owner, renew(Some(999), 3000))
            .await
            .unwrap_err();
        assert!(
            reason(&err).contains("backward"),
            "a start behind the stored one is refused: {}",
            reason(&err)
        );
        let err = renew_grant_handler()(state.clone(), owner, renew(Some(3001), 3000))
            .await
            .unwrap_err();
        assert!(
            reason(&err).contains("past its end"),
            "a start past the end is refused: {}",
            reason(&err)
        );
        renew_grant_handler()(state.clone(), owner, renew(None, 3000))
            .await
            .expect("a renew without a start is a plain bump");
        let bytes = state
            .db
            .get_capability_grant(&owner, &grant_id)
            .await
            .unwrap()
            .expect("grant row");
        assert_eq!(
            stored_window(&bytes),
            (1000, 3000),
            "the stored start is kept"
        );
        renew_grant_handler()(state.clone(), owner, renew(Some(1500), 3000))
            .await
            .expect("an equal-end renew may still slide the start");
        let bytes = state
            .db
            .get_capability_grant(&owner, &grant_id)
            .await
            .unwrap()
            .expect("grant row");
        assert_eq!(
            stored_window(&bytes),
            (1500, 3000),
            "the start slid forward"
        );
    }

    // ── The re-score drain plane (design § 2.5 step 4) ──────────────

    /// A `GrantBlob` carrying a declared `scope` (the drain plane gates on it),
    /// stored opaque under `(owner, grant_id)` keyed by `holder_pubkey`.
    fn build_grant_blob_with_scope(
        owner: &[u8; 32],
        grant_id: &[u8; 16],
        holder_pubkey: &[u8; 32],
        scope: Vec<fauna_mls::wrapped_blob::ScopeTuple>,
    ) -> Vec<u8> {
        use fauna_mls::wrapped_blob::{GrantBlob, GrantIndex, GrantWindow};
        GrantBlob {
            version: 1,
            kind: GrantBlob::KIND.to_string(),
            index: GrantIndex(owner.to_vec(), grant_id.to_vec()),
            holder: ByteBuf::from(holder_pubkey.to_vec()),
            window: GrantWindow(0, u64::MAX),
            scope,
            wrapped_keys: Vec::new(),
        }
        .to_canonical_bytes()
        .expect("encode grant blob")
    }

    fn scope_tuple(class: &str, kind: &str) -> fauna_mls::wrapped_blob::ScopeTuple {
        fauna_mls::wrapped_blob::ScopeTuple {
            class: class.into(),
            kind: Some(kind.into()),
            tier: None,
            set: None,
            factor: None,
        }
    }

    fn clamav_entry(v: u32) -> fauna_core::scoring::ScoreEntry {
        fauna_core::scoring::ScoreEntry {
            factor: fauna_core::scoring::factor::CLAMAV.to_string(),
            score: 0,
            tier: fauna_core::scoring::TIER_ADMIN,
            scorer_version: v,
        }
    }

    #[tokio::test]
    async fn rescore_worklist_returns_stale_units_for_held_grant() {
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let owner = [7u8; 32];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;

        // Grant the holder content.read{mail} + content.label-write over the
        // owner's content (the pair the real client mint issues — the slice-6
        // lease gate requires the box to be able to RUN the kind, not just
        // read the worklist).
        let blob = build_grant_blob_with_scope(
            &owner,
            &[0x11u8; 16],
            &cp_public,
            vec![
                scope_tuple("content.read", "mail"),
                scope_tuple("content.label-write", "mail"),
            ],
        );
        state
            .db
            .put_capability_grant(&owner, &[0x11u8; 16], &cp_public, i64::MAX, &blob)
            .await
            .unwrap();

        // A clamav model bump (v1 → v2), and a mail item scored at the old v1.
        state.db.upsert_model_version("clamav", 2).await.unwrap();
        let cid = [0x99u8; 32];
        state
            .db
            .insert_content_scores(
                &cid,
                "mail",
                Some(&owner),
                1_700_000_000,
                &[clamav_entry(1)],
            )
            .await
            .unwrap();

        let req = RescoreWorklistRequest {
            limit: 0,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply: RescoreWorklistReply = decode(
            &rescore_worklist_handler()(state.clone(), cp_actor, payload)
                .await
                .expect("worklist ok"),
        )
        .unwrap();

        assert_eq!(reply.units.len(), 1, "one stale clamav unit owed");
        let unit = &reply.units[0];
        assert_eq!(unit.content_id.as_ref(), &cid[..]);
        assert_eq!(unit.content_kind, "mail");
        assert_eq!(unit.owner_actor_id.as_ref(), &owner[..]);
        assert_eq!(unit.factor, "clamav");
        assert_eq!(unit.from_version, 1);
        assert_eq!(unit.to_version, 2);
    }

    /// A `ScopeTuple` with a per-factor license (`factor`), the per-labeler
    /// grant shape.
    fn scope_tuple_for(
        class: &str,
        kind: &str,
        factor: &str,
    ) -> fauna_mls::wrapped_blob::ScopeTuple {
        fauna_mls::wrapped_blob::ScopeTuple {
            factor: Some(factor.into()),
            ..scope_tuple(class, kind)
        }
    }

    fn labeler_entry(factor: &str, v: u32) -> fauna_core::scoring::ScoreEntry {
        fauna_core::scoring::ScoreEntry {
            factor: factor.to_string(),
            score: 0,
            tier: fauna_core::scoring::TIER_COMMUNITY,
            scorer_version: v,
        }
    }

    /// The per-labeler gate: a `labeler:<hex>` obligation
    /// is owed only to a holder whose read tuple names that labeler. The
    /// composed "read and filter my mail" grant surfaces the built-in
    /// factor's unit and nothing else; labeler A's grant adds A's unit; a
    /// labeler the owner never granted is never surfaced, however stale.
    #[tokio::test]
    async fn rescore_worklist_owes_labeler_units_only_under_that_labelers_grant() {
        use fauna_core::scoring::labeler_factor;
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let owner = [7u8; 32];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;
        let factor_a = labeler_factor(&fauna_core::identity::ActorId([0xA1u8; 32]));
        let factor_b = labeler_factor(&fauna_core::identity::ActorId([0xB2u8; 32]));

        // The composed role: built-in factors only.
        let composed = build_grant_blob_with_scope(
            &owner,
            &[0x11u8; 16],
            &cp_public,
            vec![
                scope_tuple("content.read", "mail"),
                scope_tuple("content.label-write", "mail"),
            ],
        );
        state
            .db
            .put_capability_grant(&owner, &[0x11u8; 16], &cp_public, i64::MAX, &composed)
            .await
            .unwrap();

        for f in ["clamav", factor_a.as_str(), factor_b.as_str()] {
            state.db.upsert_model_version(f, 2).await.unwrap();
        }
        let cid = [0x99u8; 32];
        state
            .db
            .insert_content_scores(
                &cid,
                "mail",
                Some(&owner),
                1_700_000_000,
                &[
                    clamav_entry(1),
                    labeler_entry(&factor_a, 1),
                    labeler_entry(&factor_b, 1),
                ],
            )
            .await
            .unwrap();

        let worklist_factors = |state: Arc<AppState>| async move {
            let req = RescoreWorklistRequest {
                limit: 0,
                extra: Default::default(),
            };
            let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
            let reply: RescoreWorklistReply = decode(
                &rescore_worklist_handler()(state, cp_actor, payload)
                    .await
                    .expect("worklist ok"),
            )
            .unwrap();
            let mut factors: Vec<String> = reply.units.into_iter().map(|u| u.factor).collect();
            factors.sort();
            factors
        };

        assert_eq!(
            worklist_factors(state.clone()).await,
            vec!["clamav".to_string()],
            "the composed grant owes the built-in factor and no community labeler"
        );

        // Labeler A's own grant: read + label-write, both confined to A.
        let labeler_a = build_grant_blob_with_scope(
            &owner,
            &[0x12u8; 16],
            &cp_public,
            vec![
                scope_tuple_for("content.read", "mail", &factor_a),
                scope_tuple_for("content.label-write", "mail", &factor_a),
            ],
        );
        state
            .db
            .put_capability_grant(&owner, &[0x12u8; 16], &cp_public, i64::MAX, &labeler_a)
            .await
            .unwrap();

        let mut want = vec!["clamav".to_string(), factor_a.clone()];
        want.sort();
        assert_eq!(
            worklist_factors(state.clone()).await,
            want,
            "labeler A's grant adds A's unit; B, never granted, stays owed but unsurfaced"
        );
    }

    #[tokio::test]
    async fn rescore_worklist_omits_unheld_owner() {
        // Confidentiality boundary: a holder granted content.read only over
        // owner A must never learn owner B's stale content_ids.
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let owner_a = [0xA1u8; 32];
        let owner_b = [0xB2u8; 32];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;

        // Grant only over owner A (the read + label-write pair the real mint
        // issues, so the lease gate admits A and the boundary tested is the
        // per-owner scope, not the gate).
        let blob = build_grant_blob_with_scope(
            &owner_a,
            &[0x11u8; 16],
            &cp_public,
            vec![
                scope_tuple("content.read", "mail"),
                scope_tuple("content.label-write", "mail"),
            ],
        );
        state
            .db
            .put_capability_grant(&owner_a, &[0x11u8; 16], &cp_public, i64::MAX, &blob)
            .await
            .unwrap();

        state.db.upsert_model_version("clamav", 2).await.unwrap();
        // Both owners have a stale row.
        state
            .db
            .insert_content_scores(&[0xA9u8; 32], "mail", Some(&owner_a), 1, &[clamav_entry(1)])
            .await
            .unwrap();
        state
            .db
            .insert_content_scores(&[0xB9u8; 32], "mail", Some(&owner_b), 1, &[clamav_entry(1)])
            .await
            .unwrap();

        let payload = Bytes::from(
            encode_canonical(&RescoreWorklistRequest::default())
                .unwrap()
                .to_vec(),
        );
        let reply: RescoreWorklistReply = decode(
            &rescore_worklist_handler()(state.clone(), cp_actor, payload)
                .await
                .expect("worklist ok"),
        )
        .unwrap();

        assert_eq!(reply.units.len(), 1, "only the held owner's unit");
        assert_eq!(reply.units[0].owner_actor_id.as_ref(), &owner_a[..]);
    }

    /// The slice-6 lease gate, admit side: serving a worklist inline-claims the
    /// owner's `content-rescore` lease as this nest (closing the mint→drain
    /// race), so clients observing the lease see the nest as the runner.
    #[tokio::test]
    async fn rescore_worklist_claims_the_content_rescore_lease() {
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let owner = [7u8; 32];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;
        let blob = build_grant_blob_with_scope(
            &owner,
            &[0x11u8; 16],
            &cp_public,
            vec![
                scope_tuple("content.read", "mail"),
                scope_tuple("content.label-write", "mail"),
            ],
        );
        state
            .db
            .put_capability_grant(&owner, &[0x11u8; 16], &cp_public, i64::MAX, &blob)
            .await
            .unwrap();
        state.db.upsert_model_version("clamav", 2).await.unwrap();
        state
            .db
            .insert_content_scores(&[0x99u8; 32], "mail", Some(&owner), 1, &[clamav_entry(1)])
            .await
            .unwrap();

        let payload = Bytes::from(
            encode_canonical(&RescoreWorklistRequest::default())
                .unwrap()
                .to_vec(),
        );
        let _: RescoreWorklistReply = decode(
            &rescore_worklist_handler()(state.clone(), cp_actor, payload)
                .await
                .expect("worklist ok"),
        )
        .unwrap();

        let leases = state
            .delegation_leases
            .observe(owner, &["content-rescore".to_string()]);
        assert_eq!(leases.len(), 1, "serving the worklist claimed the lease");
        assert_eq!(
            leases[0].holder,
            crate::delegation_runner::nest_self_ref(&state)
        );
    }

    /// The slice-6 lease gate, deny side: while a fresh foreign participant
    /// holds the owner's `content-rescore` lease (e.g. a future client-side
    /// runner the user pinned), the nest serves that owner NO work — exactly
    /// one participant runs a kind.
    #[tokio::test]
    async fn rescore_worklist_gated_while_foreign_holder_runs_the_kind() {
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let owner = [7u8; 32];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;
        let blob = build_grant_blob_with_scope(
            &owner,
            &[0x11u8; 16],
            &cp_public,
            vec![
                scope_tuple("content.read", "mail"),
                scope_tuple("content.label-write", "mail"),
            ],
        );
        state
            .db
            .put_capability_grant(&owner, &[0x11u8; 16], &cp_public, i64::MAX, &blob)
            .await
            .unwrap();
        state.db.upsert_model_version("clamav", 2).await.unwrap();
        state
            .db
            .insert_content_scores(&[0x99u8; 32], "mail", Some(&owner), 1, &[clamav_entry(1)])
            .await
            .unwrap();

        // A foreign participant holds the lease, fresh.
        state.delegation_leases.heartbeat(
            owner,
            "content-rescore",
            fauna_core::data::ParticipantRef::Device {
                device_id: "dev-a".into(),
            },
            fauna_core::delegation::ParticipantClass::PluggedInDesktop,
        );

        let payload = Bytes::from(
            encode_canonical(&RescoreWorklistRequest::default())
                .unwrap()
                .to_vec(),
        );
        let reply: RescoreWorklistReply = decode(
            &rescore_worklist_handler()(state.clone(), cp_actor, payload)
                .await
                .expect("worklist ok"),
        )
        .unwrap();
        assert!(
            reply.units.is_empty(),
            "no work served while a fresh foreign holder runs the kind"
        );
    }

    /// The slice-6 lease gate, sufficiency side: a read-only grant cannot RUN
    /// the kind (no label-write to submit), so the worklist reveals nothing —
    /// the reveal is pointless work-metadata for a holder that can't act.
    #[tokio::test]
    async fn rescore_worklist_gated_for_read_only_grant() {
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let owner = [7u8; 32];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;
        let blob = build_grant_blob_with_scope(
            &owner,
            &[0x11u8; 16],
            &cp_public,
            vec![scope_tuple("content.read", "mail")],
        );
        state
            .db
            .put_capability_grant(&owner, &[0x11u8; 16], &cp_public, i64::MAX, &blob)
            .await
            .unwrap();
        state.db.upsert_model_version("clamav", 2).await.unwrap();
        state
            .db
            .insert_content_scores(&[0x99u8; 32], "mail", Some(&owner), 1, &[clamav_entry(1)])
            .await
            .unwrap();

        let payload = Bytes::from(
            encode_canonical(&RescoreWorklistRequest::default())
                .unwrap()
                .to_vec(),
        );
        let reply: RescoreWorklistReply = decode(
            &rescore_worklist_handler()(state.clone(), cp_actor, payload)
                .await
                .expect("worklist ok"),
        )
        .unwrap();
        assert!(reply.units.is_empty(), "read-only grant gets no worklist");
        assert!(
            state
                .delegation_leases
                .observe(owner, &["content-rescore".to_string()])
                .is_empty(),
            "and no lease is claimed for an insufficient grant"
        );
    }

    #[tokio::test]
    async fn submit_scores_writes_back_bumped_version() {
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let owner = [7u8; 32];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;

        // The holder holds content.label-write{mail} over the owner.
        let blob = build_grant_blob_with_scope(
            &owner,
            &[0x11u8; 16],
            &cp_public,
            vec![scope_tuple("content.label-write", "mail")],
        );
        state
            .db
            .put_capability_grant(&owner, &[0x11u8; 16], &cp_public, i64::MAX, &blob)
            .await
            .unwrap();

        // Seed the pre-drain row at v1.
        let cid = [0x99u8; 32];
        state
            .db
            .insert_content_scores(&cid, "mail", Some(&owner), 1, &[clamav_entry(1)])
            .await
            .unwrap();

        let req = SubmitScoresRequest {
            rows: vec![fauna_protocol::wrapped_blob::SubmitScoreRow {
                content_id: ByteBuf::from(cid.to_vec()),
                content_kind: "mail".into(),
                owner_actor_id: ByteBuf::from(owner.to_vec()),
                scored_at: 1_700_000_500,
                entries: vec![clamav_entry(2)],
                extra: Default::default(),
            }],
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply: SubmitScoresReply = decode(
            &submit_scores_handler()(state.clone(), cp_actor, payload)
                .await
                .expect("submit ok"),
        )
        .unwrap();
        assert!(reply.ok);
        assert_eq!(reply.written, 1);

        // The row is now at v2 → the obligation gap is closed.
        let rows = state.db.get_content_scores(&cid).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].scorer_version, 2);
    }

    #[tokio::test]
    async fn submit_scores_rejects_row_without_label_write_grant() {
        // Fail-closed: a holder with no content.label-write grant for the row's
        // owner cannot write its scores — and no partial write happens.
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let owner = [7u8; 32];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;

        // Only a READ grant — NOT label-write.
        let blob = build_grant_blob_with_scope(
            &owner,
            &[0x11u8; 16],
            &cp_public,
            vec![scope_tuple("content.read", "mail")],
        );
        state
            .db
            .put_capability_grant(&owner, &[0x11u8; 16], &cp_public, i64::MAX, &blob)
            .await
            .unwrap();

        // Seed a prior owner-attributed row so the rejection exercises the
        // label-write authz path, not the no-prior-row guard.
        let cid = [0x99u8; 32];
        state
            .db
            .insert_content_scores(&cid, "mail", Some(&owner), 1, &[clamav_entry(1)])
            .await
            .unwrap();

        let req = SubmitScoresRequest {
            rows: vec![fauna_protocol::wrapped_blob::SubmitScoreRow {
                content_id: ByteBuf::from(cid.to_vec()),
                content_kind: "mail".into(),
                owner_actor_id: ByteBuf::from(owner.to_vec()),
                scored_at: 1_700_000_500,
                entries: vec![clamav_entry(2)],
                extra: Default::default(),
            }],
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = submit_scores_handler()(state.clone(), cp_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
        // The pre-existing row is untouched (still v1, no partial write).
        let rows = state.db.get_content_scores(&cid).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].scorer_version, 1);
    }

    /// The nest-side twin of the worklist gate: a
    /// `labeler:<hex>` row lands only under a label-write tuple naming that
    /// labeler. The composed grant's holder is refused (and writes nothing);
    /// once labeler A's grant is deposited the same row lands.
    #[tokio::test]
    async fn submit_scores_accepts_a_labeler_row_only_under_that_labelers_grant() {
        use fauna_core::scoring::labeler_factor;
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let owner = [7u8; 32];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;
        let factor_a = labeler_factor(&fauna_core::identity::ActorId([0xA1u8; 32]));

        let composed = build_grant_blob_with_scope(
            &owner,
            &[0x11u8; 16],
            &cp_public,
            vec![
                scope_tuple("content.read", "mail"),
                scope_tuple("content.label-write", "mail"),
            ],
        );
        state
            .db
            .put_capability_grant(&owner, &[0x11u8; 16], &cp_public, i64::MAX, &composed)
            .await
            .unwrap();

        let cid = [0x99u8; 32];
        state
            .db
            .insert_content_scores(
                &cid,
                "mail",
                Some(&owner),
                1,
                &[labeler_entry(&factor_a, 1)],
            )
            .await
            .unwrap();

        let submit = |state: Arc<AppState>| {
            let entry = labeler_entry(&factor_a, 2);
            async move {
                let req = SubmitScoresRequest {
                    rows: vec![fauna_protocol::wrapped_blob::SubmitScoreRow {
                        content_id: ByteBuf::from(cid.to_vec()),
                        content_kind: "mail".into(),
                        owner_actor_id: ByteBuf::from(owner.to_vec()),
                        scored_at: 1_700_000_500,
                        entries: vec![entry],
                        extra: Default::default(),
                    }],
                    extra: Default::default(),
                };
                let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
                submit_scores_handler()(state, cp_actor, payload).await
            }
        };

        let err = submit(state.clone()).await.unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
        let rows = state.db.get_content_scores(&cid).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].scorer_version, 1,
            "the composed grant lands no labeler row"
        );

        let labeler_a = build_grant_blob_with_scope(
            &owner,
            &[0x12u8; 16],
            &cp_public,
            vec![
                scope_tuple_for("content.read", "mail", &factor_a),
                scope_tuple_for("content.label-write", "mail", &factor_a),
            ],
        );
        state
            .db
            .put_capability_grant(&owner, &[0x12u8; 16], &cp_public, i64::MAX, &labeler_a)
            .await
            .unwrap();
        submit(state.clone())
            .await
            .expect("labeler A's grant lands A's row");
        let rows = state.db.get_content_scores(&cid).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].scorer_version, 2);
    }

    #[tokio::test]
    async fn submit_scores_rejects_cross_owner_reattribution() {
        // a holder that holds content.label-write over owner A but only
        // content.read over owner B must NOT be able to overwrite +
        // re-attribute B's score row by *claiming* owner A on the submit. The
        // write is authorized against the content's STORED owner (B), so the
        // holder's label-write{A} grant does not authorize touching B's row.
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let owner_a = [7u8; 32];
        let owner_b = [8u8; 32];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;

        // The holder holds label-write over A (only read over B).
        let blob = build_grant_blob_with_scope(
            &owner_a,
            &[0x11u8; 16],
            &cp_public,
            vec![
                scope_tuple("content.label-write", "mail"),
                scope_tuple("content.read", "mail"),
            ],
        );
        state
            .db
            .put_capability_grant(&owner_a, &[0x11u8; 16], &cp_public, i64::MAX, &blob)
            .await
            .unwrap();

        // Content X belongs to owner B (stored under B at v1).
        let cid = [0x99u8; 32];
        state
            .db
            .insert_content_scores(&cid, "mail", Some(&owner_b), 1, &[clamav_entry(1)])
            .await
            .unwrap();

        // The holder submits X while claiming owner A.
        let req = SubmitScoresRequest {
            rows: vec![fauna_protocol::wrapped_blob::SubmitScoreRow {
                content_id: ByteBuf::from(cid.to_vec()),
                content_kind: "mail".into(),
                owner_actor_id: ByteBuf::from(owner_a.to_vec()),
                scored_at: 1_700_000_500,
                entries: vec![clamav_entry(2)],
                extra: Default::default(),
            }],
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = submit_scores_handler()(state.clone(), cp_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");

        // B's row is unchanged: still v1, still attributed to B.
        let rows = state.db.get_content_scores(&cid).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].scorer_version, 1);
        assert_eq!(
            state.db.content_score_owner(&cid).await.unwrap(),
            Some(owner_b),
        );
    }

    #[tokio::test]
    async fn submit_scores_rejects_a_claimed_kind_that_differs_from_the_stored_kind() {
        // a holder granted content.label-write{mail}
        // over an owner must NOT reach that owner's `post` row by *claiming*
        // kind `mail` on the submit — the grant's kind is judged against the
        // STORED row's kind, and the row is neither overwritten nor relabelled.
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let owner = [7u8; 32];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;

        // label-write over mail only; read over post (how it learns the id).
        let blob = build_grant_blob_with_scope(
            &owner,
            &[0x11u8; 16],
            &cp_public,
            vec![
                scope_tuple("content.label-write", "mail"),
                scope_tuple("content.read", "post"),
            ],
        );
        state
            .db
            .put_capability_grant(&owner, &[0x11u8; 16], &cp_public, i64::MAX, &blob)
            .await
            .unwrap();

        // An OWNER-ATTRIBUTED `post` row at v1 — owner-less rows are already
        // refused by the no-prior-row guard, which would mask this check.
        let cid = [0x99u8; 32];
        state
            .db
            .insert_content_scores(&cid, "post", Some(&owner), 1, &[clamav_entry(1)])
            .await
            .unwrap();

        let req = SubmitScoresRequest {
            rows: vec![fauna_protocol::wrapped_blob::SubmitScoreRow {
                content_id: ByteBuf::from(cid.to_vec()),
                content_kind: "mail".into(),
                owner_actor_id: ByteBuf::from(owner.to_vec()),
                scored_at: 1_700_000_500,
                entries: vec![clamav_entry(2)],
                extra: Default::default(),
            }],
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = submit_scores_handler()(state.clone(), cp_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");

        // The post row is untouched: still v1, still kind `post`.
        let rows = state.db.get_content_scores(&cid).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].scorer_version, 1);
        assert_eq!(
            state.db.content_score_owner_and_kind(&cid).await.unwrap(),
            Some((owner, "post".to_string())),
        );
    }

    #[tokio::test]
    async fn submit_scores_rejects_content_with_no_prior_row() {
        // The content-bound authz guard: submit_scores only ever re-scores
        // content that was already ingested + scored, so a content_id with no
        // owner-attributed row is rejected (a holder cannot mint a fresh score
        // row for arbitrary content it never legitimately observed).
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let owner = [7u8; 32];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;

        let blob = build_grant_blob_with_scope(
            &owner,
            &[0x11u8; 16],
            &cp_public,
            vec![scope_tuple("content.label-write", "mail")],
        );
        state
            .db
            .put_capability_grant(&owner, &[0x11u8; 16], &cp_public, i64::MAX, &blob)
            .await
            .unwrap();

        // No prior content_scores row for cid.
        let cid = [0x99u8; 32];
        let req = SubmitScoresRequest {
            rows: vec![fauna_protocol::wrapped_blob::SubmitScoreRow {
                content_id: ByteBuf::from(cid.to_vec()),
                content_kind: "mail".into(),
                owner_actor_id: ByteBuf::from(owner.to_vec()),
                scored_at: 1_700_000_500,
                entries: vec![clamav_entry(2)],
                extra: Default::default(),
            }],
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = submit_scores_handler()(state.clone(), cp_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
        assert!(state.db.get_content_scores(&cid).await.unwrap().is_empty());
    }

    // ── The spam-baseline publish drain (`mail-spam.md` § Encrypted-mode
    //    interaction, ratified 2026-07-13) ──────────────────────────────

    /// **The definition-of-success negative — the whole point of the keyless
    /// shape:** a holder whose ONLY grant from an owner is
    /// `content.read{spam-model}` gets that owner's spam-baseline worklist
    /// during a publish run, but gets **nothing** from the mail-read-gated
    /// re-score worklist for the same owner — the contributor's grant
    /// demonstrably does not convey mail read (`mail-spam.md` § Encrypted-mode
    /// interaction; `key-material-hierarchy.md` § Don't do these). The
    /// crypto-layer half (the grant carries zero key material) is pinned in
    /// `fauna-client-capabilities` / `fauna-mls`; this is the authorization-
    /// plane half.
    #[tokio::test]
    async fn spam_model_grant_conveys_no_mail_worklist() {
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let owner = [7u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;

        // The ONLY grant: keyless content.read{spam-model} over the owner.
        let blob = build_grant_blob_with_scope(
            &owner,
            &[0x31u8; 16],
            &cp_public,
            vec![scope_tuple("content.read", "spam-model")],
        );
        state
            .db
            .put_capability_grant(&owner, &[0x31u8; 16], &cp_public, i64::MAX, &blob)
            .await
            .unwrap();

        // A stale mail-score obligation for the owner — exactly the setup the
        // mail-granted worklist test serves a unit for.
        state.db.upsert_model_version("clamav", 2).await.unwrap();
        let cid = [0x99u8; 32];
        state
            .db
            .insert_content_scores(
                &cid,
                "mail",
                Some(&owner),
                1_700_000_000,
                &[clamav_entry(1)],
            )
            .await
            .unwrap();

        // The mail-read-gated re-score worklist serves NOTHING to this holder.
        let req = RescoreWorklistRequest {
            limit: 0,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply: RescoreWorklistReply = decode(
            &rescore_worklist_handler()(state.clone(), cp_actor, payload)
                .await
                .expect("worklist call itself is permitted"),
        )
        .unwrap();
        assert!(
            reply.units.is_empty(),
            "a spam-model grant must not reveal (or authorize) any mail work"
        );
    }

    /// The FORWARD-hardening twin of `spam_model_grant_conveys_no_mail_worklist`: a
    /// holder granted `content.read{mail}` over an owner gets that owner's MAIL
    /// re-score work but is never handed a NON-mail row for the same owner —
    /// the `content_kind != kind` filter (line ~3117) is per-kind, not just
    /// per-owner, so a mail grant can never leak (or authorize re-scoring of) a
    /// calendar/post/etc. content id. The scan is factor+owner-scoped, so a
    /// stale non-mail row for the owner IS returned by
    /// `content_scores_behind_for_owner` and must be dropped by the kind gate —
    /// this test seeds both a stale mail row and a stale calendar row and
    /// asserts only the mail unit surfaces (so the gate is exercised actively,
    /// not passing vacuously on an empty scan).
    #[tokio::test]
    async fn mail_grant_conveys_no_non_mail_worklist() {
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let owner = [7u8; 32];
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;

        // The ONLY grant: content.read{mail} + content.label-write{mail} (the
        // pair the lease gate requires to admit any mail work at all). There is
        // deliberately NO calendar-kind grant.
        let blob = build_grant_blob_with_scope(
            &owner,
            &[0x41u8; 16],
            &cp_public,
            vec![
                scope_tuple("content.read", "mail"),
                scope_tuple("content.label-write", "mail"),
            ],
        );
        state
            .db
            .put_capability_grant(&owner, &[0x41u8; 16], &cp_public, i64::MAX, &blob)
            .await
            .unwrap();

        // A clamav bump, and TWO stale rows for the owner: one mail, one
        // calendar. The scan is factor+owner-scoped and returns both.
        state.db.upsert_model_version("clamav", 2).await.unwrap();
        let mail_cid = [0x99u8; 32];
        state
            .db
            .insert_content_scores(
                &mail_cid,
                "mail",
                Some(&owner),
                1_700_000_000,
                &[clamav_entry(1)],
            )
            .await
            .unwrap();
        let cal_cid = [0xCAu8; 32];
        state
            .db
            .insert_content_scores(
                &cal_cid,
                "calendar",
                Some(&owner),
                1_700_000_000,
                &[clamav_entry(1)],
            )
            .await
            .unwrap();

        let req = RescoreWorklistRequest {
            limit: 0,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply: RescoreWorklistReply = decode(
            &rescore_worklist_handler()(state.clone(), cp_actor, payload)
                .await
                .expect("worklist call permitted"),
        )
        .unwrap();

        // Exactly the mail unit — the calendar row is dropped by the kind gate.
        assert_eq!(reply.units.len(), 1, "only the mail row is authorized");
        assert_eq!(reply.units[0].content_kind, "mail");
        assert_eq!(reply.units[0].content_id.as_ref(), &mail_cid[..]);
        assert!(
            reply.units.iter().all(|u| u.content_kind != "calendar"),
            "a mail grant must never surface a calendar row"
        );
    }

    /// Opt an actor into the deployment baseline (the db half of what
    /// `set_baseline_contribution` persists — the handler lives in
    /// `bridge_imap_handlers`, whose tests drive it through the RPC).
    async fn opt_in_baseline(state: &Arc<AppState>, actor: &[u8; 32]) {
        let mut prefs = state.db.get_spam_preferences(actor).await.unwrap();
        prefs.contribute_baseline = true;
        state
            .db
            .upsert_spam_preferences(actor, &prefs)
            .await
            .unwrap();
    }

    /// Seed a contributor: an opaque sealed `spam_models` row plus a
    /// sealed-to-holder copy — what an opted-in `put_spam_model` write stores.
    /// The copy bytes are a `SpamModel` serialization standing in for what the
    /// holder would decrypt (the nest never opens it; the tests' simulated
    /// holder "unseals" by decoding).
    async fn seed_sealed_contributor(
        state: &Arc<AppState>,
        actor: &[u8; 32],
        holder_pubkey: &[u8; 32],
        copy_model: &fauna_mail::spam::SpamModel,
    ) {
        state
            .db
            .put_spam_model_with_history(
                actor,
                b"\xEE\xEE not a plaintext SpamModel",
                None,
                Some((holder_pubkey, &copy_model.to_bytes())),
            )
            .await
            .unwrap();
        opt_in_baseline(state, actor).await;
    }

    /// Mint the keyless `content.read{spam-model}` grant from `owner` to the
    /// holder (the toggle→grant client wiring's artifact, piece (b)).
    async fn grant_spam_model_read(
        state: &Arc<AppState>,
        owner: &[u8; 32],
        grant_id: &[u8; 16],
        holder_pubkey: &[u8; 32],
    ) {
        let blob = build_grant_blob_with_scope(
            owner,
            grant_id,
            holder_pubkey,
            vec![scope_tuple("content.read", "spam-model")],
        );
        state
            .db
            .put_capability_grant(owner, grant_id, holder_pubkey, i64::MAX, &blob)
            .await
            .unwrap();
    }

    fn spam_baseline_worklist_payload(run_id: &[u8]) -> Bytes {
        Bytes::from(
            encode_canonical(&SpamBaselineWorklistRequest {
                run_id: ByteBuf::from(run_id.to_vec()),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        )
    }

    /// End-to-end sealed merge: four opted-in contributors, every model sealed
    /// at rest; the publish handler merges nothing itself and drives them all
    /// through the holder drain (worklist pull → off-box merge → submit), so the
    /// holder's half is the baseline and the k-anon floor counts the
    /// contributors it named (`mail-spam.md` § Encrypted-mode interaction).
    #[tokio::test]
    async fn publish_spam_baseline_drains_sealed_contributors_through_holder() {
        use fauna_mail::spam::SpamModel;
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let admin = [1u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;

        // Four opted-in contributors with standing spam-model grants.
        let p1 = [0x10u8; 32];
        let p2 = [0x11u8; 32];
        let p1_model = small_spam_model("buy cheap pills now", "lunch agenda", 3);
        let p2_model = small_spam_model("cheap watches sale", "status update", 2);
        seed_sealed_contributor(&state, &p1, &cp_public, &p1_model).await;
        seed_sealed_contributor(&state, &p2, &cp_public, &p2_model).await;
        grant_spam_model_read(&state, &p1, &[0x33u8; 16], &cp_public).await;
        grant_spam_model_read(&state, &p2, &[0x34u8; 16], &cp_public).await;
        let s1 = [0x20u8; 32];
        let s2 = [0x21u8; 32];
        let s1_model = small_spam_model("limited time offer", "team sync notes", 4);
        let s2_model = small_spam_model("free crypto airdrop", "meeting agenda", 1);
        seed_sealed_contributor(&state, &s1, &cp_public, &s1_model).await;
        seed_sealed_contributor(&state, &s2, &cp_public, &s2_model).await;
        grant_spam_model_read(&state, &s1, &[0x31u8; 16], &cp_public).await;
        grant_spam_model_read(&state, &s2, &[0x32u8; 16], &cp_public).await;

        // Publish runs concurrently with the simulated holder below (the
        // handler awaits the holder's submit against the pending run).
        let pub_state = state.clone();
        // spawn-ok(test)
        let publish_task = tokio::spawn(async move {
            let payload = Bytes::from(
                encode_canonical(
                    &fauna_protocol::bridge_routing::PublishSpamBaselineRequest::default(),
                )
                .unwrap()
                .to_vec(),
            );
            crate::bridge_imap_handlers::publish_spam_baseline_handler()(pub_state, admin, payload)
                .await
        });

        // The simulated holder learns the run_id the way the real one gets it
        // from the `spam_baseline_publish` push payload.
        let run_id = {
            let mut found = None;
            for _ in 0..1000 {
                {
                    let runs = state.spam_baseline_runs.lock().await;
                    if let Some(k) = runs.keys().next() {
                        found = Some(k.clone());
                    }
                }
                if found.is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            found.expect("publish registered a pending run")
        };

        // Holder leg 1: pull the grant-gated worklist for the run.
        let wl: SpamBaselineWorklistReply = decode(
            &spam_baseline_worklist_handler()(
                state.clone(),
                cp_actor,
                spam_baseline_worklist_payload(&run_id),
            )
            .await
            .expect("worklist ok"),
        )
        .unwrap();
        assert_eq!(wl.copies.len(), 4, "every contributor served");

        // Holder leg 2: "unseal" (decode the test stand-in) + merge off-box.
        let mut merged = SpamModel::new();
        let mut merged_count = 0u32;
        for c in &wl.copies {
            let m = SpamModel::from_bytes(c.sealed_copy.as_ref())
                .expect("test copy decodes (stands in for the unsealed model)");
            merged.merge(&m);
            merged_count += 1;
        }
        let submit_req = SubmitSpamBaselineRequest {
            run_id: ByteBuf::from(run_id.clone()),
            merged_model: merged.to_bytes(),
            contributors: merged_count,
            unreadable: 0,
            // The holder names every copy it merged.
            merged_contributors: wl.copies.iter().map(|c| c.owner_actor_id.clone()).collect(),
            extra: Default::default(),
        };
        let submit_reply: SubmitSpamBaselineReply = decode(
            &submit_spam_baseline_handler()(
                state.clone(),
                cp_actor,
                Bytes::from(encode_canonical(&submit_req).unwrap().to_vec()),
            )
            .await
            .expect("submit ok"),
        )
        .unwrap();
        assert!(submit_reply.ok, "submission reached the pending run");

        // The publish lands the holder's half: 4 named (≥ floor 3).
        let reply: fauna_protocol::bridge_routing::PublishSpamBaselineReply = decode(
            &publish_task
                .await
                .unwrap()
                .expect("publish handler succeeds"),
        )
        .unwrap();
        assert_eq!(reply.contributors, 4, "all four holder-merged");
        assert!(reply.published, "the named count reaches the k-anon floor");
        assert_eq!(reply.skipped_contributors, 0, "nobody eroded this run");

        // The stored baseline carries ALL FOUR models' samples.
        let baseline_bytes = state.db.get_spam_baseline().await.unwrap().unwrap();
        let baseline = SpamModel::from_bytes(&baseline_bytes).expect("valid baseline");
        assert_eq!(
            baseline.sample_count(),
            p1_model.sample_count()
                + p2_model.sample_count()
                + s1_model.sample_count()
                + s2_model.sample_count(),
            "every contributor's samples rest in the published baseline"
        );
    }

    /// The holder NAMES the contributors it merged, and the run records exactly
    /// those as summed (`mail-spam.md` § Cold start Path 2 → *A contributor's
    /// departure withdraws the baseline*: "a sealed contributor that publish
    /// skipped … withdraws nothing"). Four opted-in contributors; the
    /// simulated holder merges a STRICT SUBSET of the candidates — all but s2 —
    /// and names them. The publish counts three (the floor), reports one
    /// skipped, and the inclusion record holds s1 but not s2. Then the skipped candidate's opt-out leaves the baseline served on
    /// both serving paths (s2 was never in the sum), while the merged one's
    /// opt-out withdraws it.
    #[tokio::test]
    async fn publish_spam_baseline_records_only_the_holder_named_sealed_contributors() {
        use crate::db::bridge_service_users::BridgeRole;
        use fauna_mail::spam::SpamModel;
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        use fauna_protocol::bridge_routing::SetBaselineContributionRequest;
        let state = fixture_state().await;
        let admin = [1u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        // The MDA the cold-start serving path is fetched through.
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;

        let p1 = [0x10u8; 32];
        let p2 = [0x11u8; 32];
        let p1_model = small_spam_model("buy cheap pills now", "lunch agenda", 3);
        let p2_model = small_spam_model("cheap watches sale", "status update", 2);
        seed_sealed_contributor(&state, &p1, &cp_public, &p1_model).await;
        seed_sealed_contributor(&state, &p2, &cp_public, &p2_model).await;
        grant_spam_model_read(&state, &p1, &[0x33u8; 16], &cp_public).await;
        grant_spam_model_read(&state, &p2, &[0x34u8; 16], &cp_public).await;
        let s1 = [0x20u8; 32];
        let s2 = [0x21u8; 32];
        for actor in [&s1, &s2] {
            // Users, so the opt-out RPC below admits them as callers.
            state.db.create_user(actor, "free", "test").await.unwrap();
        }
        let s1_model = small_spam_model("limited time offer", "team sync notes", 4);
        let s2_model = small_spam_model("free crypto airdrop", "meeting agenda", 1);
        seed_sealed_contributor(&state, &s1, &cp_public, &s1_model).await;
        seed_sealed_contributor(&state, &s2, &cp_public, &s2_model).await;
        grant_spam_model_read(&state, &s1, &[0x31u8; 16], &cp_public).await;
        grant_spam_model_read(&state, &s2, &[0x32u8; 16], &cp_public).await;

        let pub_state = state.clone();
        // spawn-ok(test)
        let publish_task = tokio::spawn(async move {
            let payload = Bytes::from(
                encode_canonical(
                    &fauna_protocol::bridge_routing::PublishSpamBaselineRequest::default(),
                )
                .unwrap()
                .to_vec(),
            );
            crate::bridge_imap_handlers::publish_spam_baseline_handler()(pub_state, admin, payload)
                .await
        });
        let run_id = {
            let mut found = None;
            for _ in 0..1000 {
                {
                    let runs = state.spam_baseline_runs.lock().await;
                    if let Some(k) = runs.keys().next() {
                        found = Some(k.clone());
                    }
                }
                if found.is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            found.expect("publish registered a pending run")
        };
        let wl: SpamBaselineWorklistReply = decode(
            &spam_baseline_worklist_handler()(
                state.clone(),
                cp_actor,
                spam_baseline_worklist_payload(&run_id),
            )
            .await
            .expect("worklist ok"),
        )
        .unwrap();
        assert_eq!(wl.copies.len(), 4, "every contributor served");

        // The holder merges every copy but s2's (s2's counts unreadable to it)
        // and names the three — the real holder's `aggregate_spam_model_copies`
        // shape.
        let mut half = SpamModel::new();
        let mut named = Vec::new();
        for c in wl.copies.iter().filter(|c| c.owner_actor_id.as_ref() != s2) {
            half.merge(&SpamModel::from_bytes(c.sealed_copy.as_ref()).expect("test copy decodes"));
            named.push(c.owner_actor_id.clone());
        }
        let submit_req = SubmitSpamBaselineRequest {
            run_id: ByteBuf::from(run_id.clone()),
            merged_model: half.to_bytes(),
            contributors: 3,
            unreadable: 1,
            merged_contributors: named,
            extra: Default::default(),
        };
        let submit_reply: SubmitSpamBaselineReply = decode(
            &submit_spam_baseline_handler()(
                state.clone(),
                cp_actor,
                Bytes::from(encode_canonical(&submit_req).unwrap().to_vec()),
            )
            .await
            .expect("submit ok"),
        )
        .unwrap();
        assert!(submit_reply.ok);

        let reply: fauna_protocol::bridge_routing::PublishSpamBaselineReply = decode(
            &publish_task
                .await
                .unwrap()
                .expect("publish handler succeeds"),
        )
        .unwrap();
        assert_eq!(reply.contributors, 3, "the three the holder named");
        assert!(reply.published, "three reach the floor");
        assert_eq!(reply.skipped_contributors, 1, "s2 eroded, honestly");

        // The inclusion record names exactly who the sum holds.
        let snapshot = state.db.snapshot_spam_baseline_run().await.unwrap();
        let mut recorded: Vec<[u8; 32]> = snapshot.inclusions.keys().copied().collect();
        recorded.sort();
        assert_eq!(
            recorded,
            vec![p1, p2, s1],
            "s2 was skipped, so it holds no inclusion row"
        );
        crate::test_support::assert_baseline_served(&state, &mda, 0x60).await;

        // The skipped candidate departs: nothing of theirs is served, so
        // nothing is withdrawn.
        let opt_out = |actor: [u8; 32]| {
            let state = state.clone();
            async move {
                crate::bridge_imap_handlers::set_baseline_contribution_handler()(
                    state,
                    actor,
                    Bytes::from(
                        encode_canonical(&SetBaselineContributionRequest {
                            contribute: false,
                            extra: Default::default(),
                        })
                        .unwrap()
                        .to_vec(),
                    ),
                )
                .await
                .expect("opt-out ok");
            }
        };
        opt_out(s2).await;
        crate::test_support::assert_baseline_served(&state, &mda, 0x70).await;

        // The merged one departs: their counts ARE in the sum, so it goes.
        opt_out(s1).await;
        crate::test_support::assert_baseline_withdrawn(&state, &mda, 0x80, "s1 was summed").await;
    }

    /// Nothing named, nothing counted (`mail-spam.md` § Cold start Path 2 →
    /// *A contributor's departure withdraws the baseline*): a holder half that
    /// merges every copy but names none is not folded. Four opted-in
    /// contributors; the run counts nobody — below the floor, so it withholds —
    /// and reports every candidate skipped.
    #[tokio::test]
    async fn publish_spam_baseline_counts_nothing_from_a_half_that_names_no_contributor() {
        use fauna_mail::spam::SpamModel;
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let admin = [1u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;

        let p1 = [0x10u8; 32];
        let p2 = [0x11u8; 32];
        let p1_model = small_spam_model("buy cheap pills now", "lunch agenda", 3);
        let p2_model = small_spam_model("cheap watches sale", "status update", 2);
        seed_sealed_contributor(&state, &p1, &cp_public, &p1_model).await;
        seed_sealed_contributor(&state, &p2, &cp_public, &p2_model).await;
        grant_spam_model_read(&state, &p1, &[0x33u8; 16], &cp_public).await;
        grant_spam_model_read(&state, &p2, &[0x34u8; 16], &cp_public).await;
        let s1 = [0x20u8; 32];
        let s2 = [0x21u8; 32];
        seed_sealed_contributor(
            &state,
            &s1,
            &cp_public,
            &small_spam_model("limited time offer", "team sync notes", 4),
        )
        .await;
        seed_sealed_contributor(
            &state,
            &s2,
            &cp_public,
            &small_spam_model("free crypto airdrop", "meeting agenda", 1),
        )
        .await;
        grant_spam_model_read(&state, &s1, &[0x31u8; 16], &cp_public).await;
        grant_spam_model_read(&state, &s2, &[0x32u8; 16], &cp_public).await;

        let pub_state = state.clone();
        // spawn-ok(test)
        let publish_task = tokio::spawn(async move {
            let payload = Bytes::from(
                encode_canonical(
                    &fauna_protocol::bridge_routing::PublishSpamBaselineRequest::default(),
                )
                .unwrap()
                .to_vec(),
            );
            crate::bridge_imap_handlers::publish_spam_baseline_handler()(pub_state, admin, payload)
                .await
        });
        let run_id = {
            let mut found = None;
            for _ in 0..1000 {
                {
                    let runs = state.spam_baseline_runs.lock().await;
                    if let Some(k) = runs.keys().next() {
                        found = Some(k.clone());
                    }
                }
                if found.is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            found.expect("publish registered a pending run")
        };
        let wl: SpamBaselineWorklistReply = decode(
            &spam_baseline_worklist_handler()(
                state.clone(),
                cp_actor,
                spam_baseline_worklist_payload(&run_id),
            )
            .await
            .expect("worklist ok"),
        )
        .unwrap();
        assert_eq!(wl.copies.len(), 4, "every contributor served");

        // Every copy merged, none named.
        let mut merged = SpamModel::new();
        for c in &wl.copies {
            merged
                .merge(&SpamModel::from_bytes(c.sealed_copy.as_ref()).expect("test copy decodes"));
        }
        let submit_req = SubmitSpamBaselineRequest {
            run_id: ByteBuf::from(run_id.clone()),
            merged_model: merged.to_bytes(),
            contributors: 4,
            unreadable: 0,
            merged_contributors: Vec::new(),
            extra: Default::default(),
        };
        let submit_reply: SubmitSpamBaselineReply = decode(
            &submit_spam_baseline_handler()(
                state.clone(),
                cp_actor,
                Bytes::from(encode_canonical(&submit_req).unwrap().to_vec()),
            )
            .await
            .expect("submit ok"),
        )
        .unwrap();
        assert!(submit_reply.ok, "the submission reached the pending run");

        let reply: fauna_protocol::bridge_routing::PublishSpamBaselineReply = decode(
            &publish_task
                .await
                .unwrap()
                .expect("publish handler succeeds"),
        )
        .unwrap();
        assert_eq!(reply.contributors, 0, "the half named nobody");
        assert!(!reply.published, "zero is below the floor");
        assert_eq!(reply.skipped_contributors, 4, "every candidate eroded");
    }

    /// A co-deployed MDA must not starve the seal-target holder's merge.
    /// A standard box ALWAYS runs an MDA (`mail-bridge-lifecycle.md` § the
    /// per-role auto-approval condition), and that MDA is itself a capability
    /// holder whose drain answers the publish poke — but contributor copies are
    /// sealed only to the content-processor seal target
    /// (`resolve_content_processor_holder_seal_target`), so the MDA's worklist is
    /// always empty and it submits an empty half having done no merge work at
    /// all. The run therefore belongs to the seal-target holder: a submit from
    /// any other enrolled service user must not consume it, or the real merged
    /// half is dropped and every sealed contributor is reported eroded
    /// (`mail-spam.md` § Encrypted-mode interaction — aggregation runs at *the*
    /// granted holder, singular).
    #[tokio::test]
    async fn publish_spam_baseline_survives_a_co_deployed_mdas_empty_submit() {
        use crate::db::bridge_service_users::BridgeRole;
        use fauna_mail::spam::SpamModel;
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let admin = [1u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();

        // The seal target: the box's content-processor holder.
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;
        // The always-present MDA — enrolled, x25519-attested (it holds content
        // grants for the re-score drain), and poked by today's broadcast.
        let (_mda_secret, mda_public) = generate_x25519_keypair();
        let mda_actor = [0x66u8; 32];
        approve_bridge_with_x25519(&state.db, &mda_actor, BridgeRole::Mda, &mda_public, "mda-1")
            .await;

        // Four contributors whose copies rest for the content processor alone.
        let p1 = [0x10u8; 32];
        let p2 = [0x11u8; 32];
        let p1_model = small_spam_model("buy cheap pills now", "lunch agenda", 3);
        let p2_model = small_spam_model("cheap watches sale", "status update", 2);
        seed_sealed_contributor(&state, &p1, &cp_public, &p1_model).await;
        seed_sealed_contributor(&state, &p2, &cp_public, &p2_model).await;
        grant_spam_model_read(&state, &p1, &[0x33u8; 16], &cp_public).await;
        grant_spam_model_read(&state, &p2, &[0x34u8; 16], &cp_public).await;
        let s1 = [0x20u8; 32];
        let s2 = [0x21u8; 32];
        let s1_model = small_spam_model("limited time offer", "team sync notes", 4);
        let s2_model = small_spam_model("free crypto airdrop", "meeting agenda", 1);
        seed_sealed_contributor(&state, &s1, &cp_public, &s1_model).await;
        seed_sealed_contributor(&state, &s2, &cp_public, &s2_model).await;
        grant_spam_model_read(&state, &s1, &[0x31u8; 16], &cp_public).await;
        grant_spam_model_read(&state, &s2, &[0x32u8; 16], &cp_public).await;

        let pub_state = state.clone();
        // spawn-ok(test)
        let publish_task = tokio::spawn(async move {
            let payload = Bytes::from(
                encode_canonical(
                    &fauna_protocol::bridge_routing::PublishSpamBaselineRequest::default(),
                )
                .unwrap()
                .to_vec(),
            );
            crate::bridge_imap_handlers::publish_spam_baseline_handler()(pub_state, admin, payload)
                .await
        });
        let run_id = {
            let mut found = None;
            for _ in 0..1000 {
                {
                    let runs = state.spam_baseline_runs.lock().await;
                    if let Some(k) = runs.keys().next() {
                        found = Some(k.clone());
                    }
                }
                if found.is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            found.expect("publish registered a pending run")
        };

        // The MDA wins the race in production: its worklist is empty, so it
        // submits an empty half with zero merge CPU while the content processor
        // is still HPKE-Opening its copies. That submit must NOT take the run.
        let mda_submit = SubmitSpamBaselineRequest {
            run_id: ByteBuf::from(run_id.clone()),
            merged_model: Vec::new(),
            contributors: 0,
            unreadable: 0,
            merged_contributors: Vec::new(),
            extra: Default::default(),
        };
        let mda_reply: SubmitSpamBaselineReply = decode(
            &submit_spam_baseline_handler()(
                state.clone(),
                mda_actor,
                Bytes::from(encode_canonical(&mda_submit).unwrap().to_vec()),
            )
            .await
            .expect("the MDA's submit is a well-formed call"),
        )
        .unwrap();
        assert!(
            !mda_reply.ok,
            "a non-seal-target holder's submit must not consume the seal target's run"
        );

        // The real holder's merge still lands.
        let wl: SpamBaselineWorklistReply = decode(
            &spam_baseline_worklist_handler()(
                state.clone(),
                cp_actor,
                spam_baseline_worklist_payload(&run_id),
            )
            .await
            .expect("worklist ok"),
        )
        .unwrap();
        assert_eq!(wl.copies.len(), 4, "every contributor served");
        let mut merged = SpamModel::new();
        let mut merged_count = 0u32;
        for c in &wl.copies {
            let m = SpamModel::from_bytes(c.sealed_copy.as_ref())
                .expect("test copy decodes (stands in for the unsealed model)");
            merged.merge(&m);
            merged_count += 1;
        }
        let submit_reply: SubmitSpamBaselineReply = decode(
            &submit_spam_baseline_handler()(
                state.clone(),
                cp_actor,
                Bytes::from(
                    encode_canonical(&SubmitSpamBaselineRequest {
                        run_id: ByteBuf::from(run_id.clone()),
                        merged_model: merged.to_bytes(),
                        contributors: merged_count,
                        unreadable: 0,
                        merged_contributors: wl
                            .copies
                            .iter()
                            .map(|c| c.owner_actor_id.clone())
                            .collect(),
                        extra: Default::default(),
                    })
                    .unwrap()
                    .to_vec(),
                ),
            )
            .await
            .expect("submit ok"),
        )
        .unwrap();
        assert!(
            submit_reply.ok,
            "the seal-target holder's submit reaches its pending run"
        );

        let reply: fauna_protocol::bridge_routing::PublishSpamBaselineReply = decode(
            &publish_task
                .await
                .unwrap()
                .expect("publish handler succeeds"),
        )
        .unwrap();
        assert_eq!(reply.contributors, 4, "all four holder-merged");
        assert_eq!(
            reply.skipped_contributors, 0,
            "the holder's half is not eroded by the MDA's empty submit"
        );
        assert!(reply.published, "the named count reaches the k-anon floor");
        let baseline_bytes = state.db.get_spam_baseline().await.unwrap().unwrap();
        let baseline = SpamModel::from_bytes(&baseline_bytes).expect("valid baseline");
        assert_eq!(
            baseline.sample_count(),
            p1_model.sample_count()
                + p2_model.sample_count()
                + s1_model.sample_count()
                + s2_model.sample_count(),
            "every contributor's samples rest in the published baseline"
        );
    }

    /// Worklist gating: a copy is served only when (grant stands) × (owner
    /// opted in) × (owner still has a current model row) × (a publish run is
    /// pending) × (the caller is an enrolled holder).
    #[tokio::test]
    async fn spam_baseline_worklist_gates_on_grant_optin_sealed_and_run() {
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;
        let copy_model = small_spam_model("spam token", "ham token", 1);

        // A: copy + opted in + sealed row, but NO spam-model grant.
        let a = [0xA1u8; 32];
        seed_sealed_contributor(&state, &a, &cp_public, &copy_model).await;
        // B: copy + grant + sealed row, but NOT opted in (default off).
        let b = [0xB1u8; 32];
        state
            .db
            .put_spam_model_with_history(
                &b,
                b"\xEE\xEE not a plaintext SpamModel",
                None,
                Some((&cp_public, &copy_model.to_bytes())),
            )
            .await
            .unwrap();
        grant_spam_model_read(&state, &b, &[0x41u8; 16], &cp_public).await;
        // C: copy + grant + opted in, but the model row is GONE (a reset that
        // left its copy behind) — a stale copy is never served.
        let c = [0xC1u8; 32];
        seed_sealed_contributor(&state, &c, &cp_public, &copy_model).await;
        grant_spam_model_read(&state, &c, &[0x42u8; 16], &cp_public).await;
        state.db.delete_spam_model(&c).await.unwrap();
        // D: all four conditions hold — the only served copy.
        let d = [0xD1u8; 32];
        seed_sealed_contributor(&state, &d, &cp_public, &copy_model).await;
        grant_spam_model_read(&state, &d, &[0x43u8; 16], &cp_public).await;

        // No pending run yet ⇒ even the enrolled holder gets malformed (the
        // worklist exists only inside a publish window).
        let no_run = spam_baseline_worklist_handler()(
            state.clone(),
            cp_actor,
            spam_baseline_worklist_payload(&[0x99u8; 16]),
        )
        .await
        .unwrap_err();
        assert_eq!(no_run.code, "fauna.protocol.malformed");

        // Open a pending run (what the publish handler does).
        let run_id = vec![0xAAu8; 16];
        let (tx, _rx) = tokio::sync::oneshot::channel();
        state.spam_baseline_runs.lock().await.insert(
            run_id.clone(),
            crate::routes::SpamBaselineRun {
                holder: cp_actor,
                tx,
            },
        );

        let wl: SpamBaselineWorklistReply = decode(
            &spam_baseline_worklist_handler()(
                state.clone(),
                cp_actor,
                spam_baseline_worklist_payload(&run_id),
            )
            .await
            .expect("worklist ok"),
        )
        .unwrap();
        assert_eq!(wl.copies.len(), 1, "only the fully-eligible owner serves");
        assert_eq!(wl.copies[0].owner_actor_id.as_ref(), &d[..]);

        // A non-enrolled caller (a plain User) is denied by the class gate —
        // with the kind's family code, since the one-seam derivation
        // reserves the central `fauna.bridges.permission_denied` for
        // unknown/revoked actors and unlisted kinds.
        let user = [0x77u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let err = spam_baseline_worklist_handler()(
            state.clone(),
            user,
            spam_baseline_worklist_payload(&run_id),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.capabilities.permission_denied");
    }

    /// THE definition-of-success negative for the keyless spam-model scope:
    /// a holder holding ONLY `content.read{spam-model}` for an owner gets
    /// that owner's copies from the spam-baseline worklist — and gets
    /// NOTHING from the mail-read-gated `rescore_worklist` for the same
    /// owner. The spam-model grant demonstrably does not convey mail read
    /// (`mail-spam.md` § Encrypted-mode interaction, the ✅ key-shape
    /// callout).
    #[tokio::test]
    async fn spam_model_grant_conveys_no_mail_read_worklist() {
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;

        // The owner mints ONLY the keyless spam-model grant.
        let owner = [0x66u8; 32];
        let copy_model = small_spam_model("spam token", "ham token", 2);
        seed_sealed_contributor(&state, &owner, &cp_public, &copy_model).await;
        grant_spam_model_read(&state, &owner, &[0x51u8; 16], &cp_public).await;

        // The owner ALSO has a stale mail re-score obligation (the setup
        // `rescore_worklist_returns_stale_units_for_held_grant` reveals with
        // a content.read{mail} grant).
        state.db.upsert_model_version("clamav", 2).await.unwrap();
        state
            .db
            .insert_content_scores(
                &[0x99u8; 32],
                "mail",
                Some(&owner),
                1_700_000_000,
                &[clamav_entry(1)],
            )
            .await
            .unwrap();

        // The spam-baseline worklist (inside a run) serves the owner's copy…
        let run_id = vec![0xABu8; 16];
        let (tx, _rx) = tokio::sync::oneshot::channel();
        state.spam_baseline_runs.lock().await.insert(
            run_id.clone(),
            crate::routes::SpamBaselineRun {
                holder: cp_actor,
                tx,
            },
        );
        let wl: SpamBaselineWorklistReply = decode(
            &spam_baseline_worklist_handler()(
                state.clone(),
                cp_actor,
                spam_baseline_worklist_payload(&run_id),
            )
            .await
            .expect("worklist ok"),
        )
        .unwrap();
        assert_eq!(wl.copies.len(), 1, "the spam-model grant reaches the copy");

        // …while the mail-read-gated re-score worklist reveals NOTHING for
        // the same owner.
        let payload = Bytes::from(
            encode_canonical(&RescoreWorklistRequest::default())
                .unwrap()
                .to_vec(),
        );
        let rescore: RescoreWorklistReply = decode(
            &rescore_worklist_handler()(state.clone(), cp_actor, payload)
                .await
                .expect("rescore worklist ok"),
        )
        .unwrap();
        assert!(
            rescore.units.is_empty(),
            "a content.read{{spam-model}} grant must not surface mail re-score work"
        );
    }

    /// Submitting for an unknown / expired run replies `ok: false` and
    /// changes nothing — idempotent, mirroring revoke's idempotency. An
    /// over-bound merged half is malformed regardless of run state.
    #[tokio::test]
    async fn submit_spam_baseline_unknown_run_is_ok_false() {
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let state = fixture_state().await;
        let (_cp_secret, cp_public) = generate_x25519_keypair();
        let cp_actor = [0x55u8; 32];
        approve_content_processor_with_x25519(&state.db, &cp_actor, &cp_public, "cp-1").await;

        let req = SubmitSpamBaselineRequest {
            run_id: ByteBuf::from(vec![0xBBu8; 16]),
            merged_model: small_spam_model("spam", "ham", 1).to_bytes(),
            contributors: 1,
            unreadable: 0,
            merged_contributors: Vec::new(),
            extra: Default::default(),
        };
        let reply: SubmitSpamBaselineReply = decode(
            &submit_spam_baseline_handler()(
                state.clone(),
                cp_actor,
                Bytes::from(encode_canonical(&req).unwrap().to_vec()),
            )
            .await
            .expect("idempotent, not an error"),
        )
        .unwrap();
        assert!(!reply.ok, "no pending run ⇒ ok: false");
        assert!(
            state.db.get_spam_baseline().await.unwrap().is_none(),
            "a runless submit writes nothing"
        );

        // Defensive size bound: a merged half past 2× the model cap is
        // malformed before any run lookup.
        let oversized = SubmitSpamBaselineRequest {
            run_id: ByteBuf::from(vec![0xBBu8; 16]),
            merged_model: vec![0x00; MAX_SUBMIT_SPAM_BASELINE_BYTES + 1],
            contributors: 1,
            unreadable: 0,
            merged_contributors: Vec::new(),
            extra: Default::default(),
        };
        let err = submit_spam_baseline_handler()(
            state.clone(),
            cp_actor,
            Bytes::from(encode_canonical(&oversized).unwrap().to_vec()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn register_service_user_self_attestation() {
        let state = fixture_state().await;
        let bridge_actor = [11u8; 32];
        // Approve first to verify the self-attestation path; pending-state
        // register is exercised by bridge_method_allowlist's
        // caller_class_returns_none_for_pending_bridge.
        approve_bridge(
            &state.db,
            &bridge_actor,
            crate::db::bridge_service_users::BridgeRole::Mta,
        )
        .await;
        // approve_bridge() bound xpk = [9u8; 32] as the first attestation;
        // re-attesting the *same* key over register_service_user is the
        // idempotent self-attestation path (x25519 is set-once).
        let req = RegisterServiceUserRequest {
            ed25519_pubkey: bridge_actor.to_vec(),
            x25519_pubkey: vec![9u8; 32],
            mlkem_ek: None,
            role: "mta".into(),
            bridge_id: "mta-1".into(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = register_service_user_handler()(state.clone(), bridge_actor, payload)
            .await
            .expect("ok");
        let reply: RegisterServiceUserReply = decode(&reply_bytes).unwrap();
        assert!(reply.enrollment_request_id.contains("approved"));

        // The attested key stays bound (idempotent re-attestation).
        let row = state
            .db
            .lookup_bridge_service_user(&bridge_actor)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.x25519_pubkey, Some([9u8; 32]));
    }

    /// The confinement self-probe's whole point: a *deployed* box's isolation
    /// facts become readable over the wire. Drives the real handler, then reads
    /// the real admin projection — the end-to-end nest half of the no-SSH
    /// observable (`security.md` § Co-resident process trust boundary).
    #[tokio::test]
    async fn register_service_user_records_confinement_and_admin_can_read_it() {
        let state = fixture_state().await;
        let bridge_actor = [11u8; 32];
        approve_bridge(
            &state.db,
            &bridge_actor,
            crate::db::bridge_service_users::BridgeRole::Mta,
        )
        .await;

        let req = RegisterServiceUserRequest {
            ed25519_pubkey: bridge_actor.to_vec(),
            x25519_pubkey: vec![9u8; 32],
            role: "mta".into(),
            bridge_id: "mta-1".into(),
            confinement: Some(fauna_protocol::wrapped_blob::BridgeConfinement {
                uid: 1001,
                sealed_store: "denied".into(),
                landlock: "partial".into(),
                seccomp: "filter".into(),
                extra: Default::default(),
            }),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        register_service_user_handler()(state.clone(), bridge_actor, payload)
            .await
            .expect("ok");

        let row = state
            .db
            .lookup_bridge_service_user(&bridge_actor)
            .await
            .unwrap()
            .unwrap();
        let c = row.confinement.clone().expect("confinement recorded");
        assert_eq!(c.uid, 1001);
        assert_eq!(c.sealed_store, "denied");
        assert_eq!(c.landlock, "partial");
        assert_eq!(c.seccomp, "filter");
        assert!(c.reported_at > 0, "reported_at must be stamped by nest");

        // The admin projection carries it; the stamp comes from nest's clock,
        // not the bridge's, so a bridge cannot backdate or future-date itself.
        let info = to_service_user_info(row, false);
        let projected = info.confinement.expect("admin projection carries it");
        assert_eq!(projected.uid, 1001);
        assert_eq!(projected.landlock, "partial");
        assert_eq!(info.confinement_reported_at, Some(c.reported_at as u64));
    }

    /// Last-write-wins, NOT set-once — the deliberate difference from the
    /// x25519 / mlkem_ek bindings beside it. A redeploy that loses the sandbox
    /// must be able to overwrite a previously healthy report, or the diagnostic
    /// would keep reassuring an admin about an image that is no longer running.
    #[tokio::test]
    async fn confinement_report_is_overwritten_each_boot() {
        let state = fixture_state().await;
        let bridge_actor = [11u8; 32];
        approve_bridge(
            &state.db,
            &bridge_actor,
            crate::db::bridge_service_users::BridgeRole::Mta,
        )
        .await;

        let register = |sealed: &'static str, landlock: &'static str| {
            let state = state.clone();
            async move {
                let req = RegisterServiceUserRequest {
                    ed25519_pubkey: bridge_actor.to_vec(),
                    x25519_pubkey: vec![9u8; 32],
                    role: "mta".into(),
                    bridge_id: "mta-1".into(),
                    confinement: Some(fauna_protocol::wrapped_blob::BridgeConfinement {
                        uid: 1001,
                        sealed_store: sealed.into(),
                        landlock: landlock.into(),
                        seccomp: "filter".into(),
                        extra: Default::default(),
                    }),
                    ..Default::default()
                };
                let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
                register_service_user_handler()(state, bridge_actor, payload)
                    .await
                    .expect("ok");
            }
        };

        register("denied", "fully").await;
        register("readable", "off").await;

        let c = state
            .db
            .lookup_bridge_service_user(&bridge_actor)
            .await
            .unwrap()
            .unwrap()
            .confinement
            .expect("confinement recorded");
        assert_eq!(
            (c.sealed_store.as_str(), c.landlock.as_str()),
            ("readable", "off"),
            "a later boot's report must replace the earlier one — a frozen \
             binding would hide a regression behind a stale success"
        );
    }

    /// The report crosses a trust boundary from the very process whose
    /// misconfiguration it describes, so nest bounds it rather than trusting the
    /// source's discipline. An injection attempt must not reach the admin page,
    /// and must not fail the enrollment either.
    #[tokio::test]
    async fn confinement_tokens_are_bounded_on_admission() {
        assert_eq!(bound_confinement_token("denied"), "denied");
        // Control characters and markup are stripped, not escaped.
        assert_eq!(
            bound_confinement_token("den\nied<script>alert(1)</script>"),
            "deniedscriptalert1script"
        );
        // Over-length is truncated to the cap.
        assert_eq!(bound_confinement_token(&"a".repeat(200)).len(), 32);
        // A wholly-illegal token degrades to `unknown` rather than erroring:
        // a bridge must never fail to enroll over a diagnostic string.
        assert_eq!(bound_confinement_token("\u{1}\u{2}\u{3}"), "unknown");
        assert_eq!(bound_confinement_token(""), "unknown");
        // Uppercase is not in the charset; a mixed token keeps its legal half
        // rather than being discarded outright.
        assert_eq!(bound_confinement_token("DENied"), "ied");
    }

    /// A hostile token from a compromised bridge reaches the DB bounded, driven
    /// through the real handler rather than the helper alone.
    #[tokio::test]
    async fn hostile_confinement_tokens_do_not_reach_the_admin_projection() {
        let state = fixture_state().await;
        let bridge_actor = [11u8; 32];
        approve_bridge(
            &state.db,
            &bridge_actor,
            crate::db::bridge_service_users::BridgeRole::Mta,
        )
        .await;
        let req = RegisterServiceUserRequest {
            ed25519_pubkey: bridge_actor.to_vec(),
            x25519_pubkey: vec![9u8; 32],
            role: "mta".into(),
            bridge_id: "mta-1".into(),
            confinement: Some(fauna_protocol::wrapped_blob::BridgeConfinement {
                uid: 0,
                sealed_store: "denied\n[nest] all clear".into(),
                landlock: "\u{0}".repeat(64),
                seccomp: "filter".into(),
                extra: Default::default(),
            }),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        register_service_user_handler()(state.clone(), bridge_actor, payload)
            .await
            .expect("a hostile diagnostic must not fail enrollment");

        let c = state
            .db
            .lookup_bridge_service_user(&bridge_actor)
            .await
            .unwrap()
            .unwrap()
            .confinement
            .unwrap();
        assert!(
            !c.sealed_store.contains('\n') && !c.sealed_store.contains(' '),
            "control chars/spaces must be stripped before storage: {:?}",
            c.sealed_store
        );
        assert_eq!(c.landlock, "unknown");
    }

    /// The confinement is deployment topology, so the User-visible holder view
    /// must not carry it — the same boundary that filters the roster itself.
    /// Asserted on the projection directly, since that is the single place the
    /// distinction is made.
    #[tokio::test]
    async fn holder_view_withholds_the_confinement_diagnostic() {
        let state = fixture_state().await;
        let bridge_actor = [11u8; 32];
        approve_bridge(
            &state.db,
            &bridge_actor,
            crate::db::bridge_service_users::BridgeRole::Mda,
        )
        .await;
        state
            .db
            .record_bridge_confinement(
                &bridge_actor,
                &crate::db::bridge_service_users::BridgeConfinementRow {
                    uid: 1002,
                    sealed_store: "denied".into(),
                    landlock: "partial".into(),
                    seccomp: "filter".into(),
                    reported_at: 1_700_000_000_000,
                },
            )
            .await
            .unwrap();
        let row = state
            .db
            .lookup_bridge_service_user(&bridge_actor)
            .await
            .unwrap()
            .unwrap();

        let holder = to_service_user_info(row.clone(), true);
        assert_eq!(
            holder.confinement, None,
            "a plain user enumerating seal targets must not learn bridge UIDs \
             or sandbox status"
        );
        assert_eq!(holder.confinement_reported_at, None);
        // Non-vacuous: the admin view of the SAME row does carry it, so this
        // test would go red if the projection stopped populating it at all.
        assert!(to_service_user_info(row, false).confinement.is_some());
    }

    #[tokio::test]
    async fn register_service_user_rejects_x25519_rebind() {
        // The set-once freeze: an enrolled bridge whose x25519 is already attested
        // cannot rebind a *different* key via register_service_user — a
        // co-resident attacker holding the bridge's ed25519 still cannot
        // swap the sealing target. The rejection is permission_denied
        // and the attested key is unchanged.
        let state = fixture_state().await;
        let bridge_actor = [11u8; 32];
        approve_bridge(
            &state.db,
            &bridge_actor,
            crate::db::bridge_service_users::BridgeRole::Mta,
        )
        .await; // binds x25519 = [9u8; 32]

        let req = RegisterServiceUserRequest {
            ed25519_pubkey: bridge_actor.to_vec(),
            x25519_pubkey: vec![22u8; 32], // different from the attested [9; 32]
            mlkem_ek: None,
            role: "mta".into(),
            bridge_id: "mta-1".into(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = register_service_user_handler()(state.clone(), bridge_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");

        // The attested key is unchanged.
        let row = state
            .db
            .lookup_bridge_service_user(&bridge_actor)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.x25519_pubkey, Some([9u8; 32]));
    }

    #[tokio::test]
    async fn register_service_user_rejects_pubkey_mismatch() {
        let state = fixture_state().await;
        let bridge_actor = [11u8; 32];
        approve_bridge(
            &state.db,
            &bridge_actor,
            crate::db::bridge_service_users::BridgeRole::Mta,
        )
        .await;

        let req = RegisterServiceUserRequest {
            ed25519_pubkey: vec![99u8; 32], // mismatched
            x25519_pubkey: vec![22u8; 32],
            mlkem_ek: None,
            role: "mta".into(),
            bridge_id: "mta-1".into(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = register_service_user_handler()(state, bridge_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn register_service_user_publishes_mlkem_ek() {
        // PQ-CAP-2: a bridge that sends a 1184-byte ML-KEM ek has it stored on
        // its bridge_service_users row alongside the idempotent x25519
        // re-attestation, so the client mint can later seal grants X-Wing to it.
        let state = fixture_state().await;
        let bridge_actor = [11u8; 32];
        approve_bridge(
            &state.db,
            &bridge_actor,
            crate::db::bridge_service_users::BridgeRole::Mda,
        )
        .await; // binds x25519 = [9u8; 32]

        let ek = vec![0x5Au8; 1184];
        let req = RegisterServiceUserRequest {
            ed25519_pubkey: bridge_actor.to_vec(),
            x25519_pubkey: vec![9u8; 32],
            mlkem_ek: Some(ByteBuf::from(ek.clone())),
            role: "mda".into(),
            bridge_id: "b1".into(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        register_service_user_handler()(state.clone(), bridge_actor, payload)
            .await
            .expect("ok");

        let stored = state.db.bridge_mlkem_ek(&bridge_actor).await.unwrap();
        assert_eq!(stored, Some(ek));
    }

    #[tokio::test]
    async fn register_service_user_rejects_wrong_length_mlkem_ek() {
        // A non-1184-byte ek is a malformed request (FIPS-203 fixes the size).
        let state = fixture_state().await;
        let bridge_actor = [11u8; 32];
        approve_bridge(
            &state.db,
            &bridge_actor,
            crate::db::bridge_service_users::BridgeRole::Mda,
        )
        .await;
        let req = RegisterServiceUserRequest {
            ed25519_pubkey: bridge_actor.to_vec(),
            x25519_pubkey: vec![9u8; 32],
            mlkem_ek: Some(ByteBuf::from(vec![0u8; 1000])),
            role: "mda".into(),
            bridge_id: "b1".into(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = register_service_user_handler()(state.clone(), bridge_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
        // Nothing was stored.
        let stored = state.db.bridge_mlkem_ek(&bridge_actor).await.unwrap();
        assert_eq!(stored, None);
    }

    #[tokio::test]
    async fn register_service_user_rejects_mlkem_ek_rebind() {
        // Set-once freeze at the handler boundary: once an ek is published, a
        // *different* one is rejected as permission_denied (the co-resident
        // attacker can't redirect the ML-KEM half of future grant seals).
        let state = fixture_state().await;
        let bridge_actor = [11u8; 32];
        approve_bridge(
            &state.db,
            &bridge_actor,
            crate::db::bridge_service_users::BridgeRole::Mda,
        )
        .await;
        // First publish binds ek_a.
        state
            .db
            .upsert_bridge_mlkem_ek(&bridge_actor, &vec![0xAAu8; 1184])
            .await
            .unwrap();
        // A register carrying a *different* ek is frozen out.
        let req = RegisterServiceUserRequest {
            ed25519_pubkey: bridge_actor.to_vec(),
            x25519_pubkey: vec![9u8; 32],
            mlkem_ek: Some(ByteBuf::from(vec![0xBBu8; 1184])),
            role: "mda".into(),
            bridge_id: "b1".into(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = register_service_user_handler()(state.clone(), bridge_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
        // The originally-bound ek is unchanged.
        let stored = state.db.bridge_mlkem_ek(&bridge_actor).await.unwrap();
        assert_eq!(stored, Some(vec![0xAAu8; 1184]));
    }

    #[tokio::test]
    async fn fetch_bridge_pubkey_surfaces_published_mlkem_ek() {
        // PQ-CAP-3: the reply exposes the holder's published ML-KEM ek so the
        // client mint selects the X-Wing wrap — and `None` before any publish,
        // so the mint degrades to the classical wrap for a holder that has not
        // yet published.
        let state = fixture_state().await;
        let bridge_actor = [11u8; 32];
        approve_bridge(
            &state.db,
            &bridge_actor,
            crate::db::bridge_service_users::BridgeRole::Mda,
        )
        .await; // bridge_id "b1", x25519 = [9u8; 32]

        let req = FetchBridgePubkeyRequest {
            bridge_role: "mda".into(),
            bridge_id: "b1".into(),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());

        // Before publish: x25519 present, no ML-KEM ek.
        let reply_bytes =
            fetch_bridge_pubkey_handler()(state.clone(), bridge_actor, payload.clone())
                .await
                .expect("fetch ok");
        let reply: FetchBridgePubkeyReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.x25519_pubkey, vec![9u8; 32]);
        assert_eq!(reply.mlkem_ek, None);

        // After publish: the ek surfaces for the mint's X-Wing selection.
        let ek = vec![0x5Au8; 1184];
        state
            .db
            .upsert_bridge_mlkem_ek(&bridge_actor, &ek)
            .await
            .unwrap();
        let reply_bytes = fetch_bridge_pubkey_handler()(state.clone(), bridge_actor, payload)
            .await
            .expect("fetch ok");
        let reply: FetchBridgePubkeyReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.mlkem_ek, Some(ByteBuf::from(ek)));
    }

    #[tokio::test]
    async fn report_auth_event_persists_row() {
        let state = fixture_state().await;
        let bridge_actor = [33u8; 32];
        approve_bridge(
            &state.db,
            &bridge_actor,
            crate::db::bridge_service_users::BridgeRole::Mda,
        )
        .await;

        let req = ReportAuthEventRequest {
            actor_id: vec![44u8; 32],
            credential_id: "cred-1".into(),
            result: "fail".into(),
            source_ip: "203.0.113.5".into(),
            occurred_at: 1_700_000_000,
            reason: Some("AEAD verify failed".into()),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let _ = report_auth_event_handler()(state.clone(), bridge_actor, payload)
            .await
            .expect("ok");

        let target = [44u8; 32];
        let rows = state
            .db
            .list_bridge_auth_events_for_actor(&target, 5)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].result, "fail");
    }

    #[tokio::test]
    async fn report_auth_event_rejects_unknown_result() {
        let state = fixture_state().await;
        let bridge_actor = [33u8; 32];
        approve_bridge(
            &state.db,
            &bridge_actor,
            crate::db::bridge_service_users::BridgeRole::Mda,
        )
        .await;

        let req = ReportAuthEventRequest {
            actor_id: vec![44u8; 32],
            credential_id: "cred-1".into(),
            result: "weird".into(),
            source_ip: "203.0.113.5".into(),
            occurred_at: 1_700_000_000,
            reason: None,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = report_auth_event_handler()(state, bridge_actor, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn report_auth_event_rejects_empty_source_ip() {
        let state = fixture_state().await;
        let bridge_actor = [33u8; 32];
        approve_bridge(
            &state.db,
            &bridge_actor,
            crate::db::bridge_service_users::BridgeRole::Mda,
        )
        .await;

        for empty in ["", "   ", "\t"] {
            let req = ReportAuthEventRequest {
                actor_id: vec![44u8; 32],
                credential_id: "cred-1".into(),
                result: "ok".into(),
                source_ip: empty.into(),
                occurred_at: 1_700_000_000,
                reason: None,
                extra: Default::default(),
            };
            let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
            let err = report_auth_event_handler()(state.clone(), bridge_actor, payload)
                .await
                .unwrap_err();
            assert_eq!(
                err.code, "fauna.protocol.malformed",
                "expected malformed for source_ip={empty:?}"
            );
        }
    }

    /// `get_caldav_port` is the **user-readable** read twin of the Admin-only
    /// `set_caldav_port`: a regular User reads the effective port for the
    /// mail-settings CalDAV connection detail. Unset ⇒ `DEFAULT_CALDAV_PORT`;
    /// after an admin sets it, every actor reads the new value (nest-wide
    /// singleton, not caller-scoped).
    #[tokio::test]
    async fn get_caldav_port_user_readable_defaults_then_reflects_set() {
        let state = fixture_state().await;
        let user = [0xA1u8; 32]; // User class (no bridge/admin row)
        state.db.create_user(&user, "free", "test").await.unwrap();

        // Unset ⇒ the shared default a User reads back.
        let payload = Bytes::from(
            encode_canonical(&GetCaldavPortRequest::default())
                .unwrap()
                .to_vec(),
        );
        let reply: GetCaldavPortReply = decode(
            &get_caldav_port_handler()(state.clone(), user, payload)
                .await
                .expect("a User may read the CalDAV port"),
        )
        .unwrap();
        assert_eq!(
            reply.port,
            fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT,
            "unset ⇒ DEFAULT_CALDAV_PORT"
        );

        // Admin sets a non-default port; the User read reflects it (singleton).
        state.db.set_caldav_port(9000).await.unwrap();
        let payload = Bytes::from(
            encode_canonical(&GetCaldavPortRequest::default())
                .unwrap()
                .to_vec(),
        );
        let reply: GetCaldavPortReply = decode(
            &get_caldav_port_handler()(state.clone(), user, payload)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(reply.port, 9000, "User read reflects the admin-set port");
    }
}
