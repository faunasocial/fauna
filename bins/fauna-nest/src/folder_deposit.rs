//! **Third-party folder deposit ingress** — the nest side
//! (`docs/goal/behavior/file-sync.md` § Third-party deposit ingress).
//!
//! A third-party principal holding `fauna:folder:deposit:<id>` posts one file
//! into one of the account's folders, write-only and blind. The nest seals it
//! **on the user's behalf** to the user's registered recipient key through the
//! D2 seal-key resolver the mail ingest sites share
//! (`CacheDb::get_recipient_seal_key`), and parks it in the folder's **inbox
//! segment** (`db::folder_deposit_inbox`) — a holding area that is not yet a
//! folder entry. The principal learns nothing but "accepted"; a seat's next
//! sync adopts the item (the adoption pass is the sync engine's).
//!
//! Two doors, one gate:
//!
//! - `fauna.folders.deposit` over WS-RPC, on a principal session — device
//!   apps and hosted code ([`principal_deposit_handler`]);
//! - `POST /api/v1/folders/{id}/deposit` for a remote server that cannot
//!   speak WS-RPC ([`deposit_http_handler`], HTTP residue per `api-layers.md`)
//!   — it admits the DPoP-bound token exactly as the principal session's
//!   upgrade does, then runs the same principal dispatch, so the ceiling, the
//!   scope check and this module's handler are the one path both doors take.
//!
//! What the handler checks, in order, after the dispatch chokepoint admitted
//! the kind for the `folder_deposit` arm: the request's shape; that one of the
//! session's scopes names THIS folder (the per-folder half of the arm); that
//! the owner minted a LIVE keyless `deposit` grant over this folder to the
//! key the principal attested (the audit + revocation record,
//! `encryption-at-rest.md` § Capability tiering → *Third-party holders*,
//! re-resolved at every deposit like the oracle's `identity.op` grants); that
//! the folder is the account's own; and that its residency is not
//! metadata-only — the one ruled refusal pair (`ui/folders.md` § Audience and
//! website serving → *Third-party deposit and read*: nothing to park). Every
//! refusal but that last answers alike, so a depositor never learns whether a
//! folder it does not hold exists.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes as BodyBytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde_bytes::ByteBuf;

use fauna_protocol::folders::{
    DEPOSITS_LIST_PAGE_BYTES, DepositEnvelope, FolderDepositReply, FolderDepositRequest,
    FolderDepositsListReply, FolderDepositsListRequest, FolderDepositsRetireReply,
    FolderDepositsRetireRequest, KIND_FOLDERS_DEPOSIT, KIND_FOLDERS_DEPOSITS_LIST,
    KIND_FOLDERS_DEPOSITS_RETIRE, MAX_DEPOSIT_BYTES, ParkedDeposit, is_deposit_name,
};
use fauna_protocol::{RpcError, decode_strict as decode};

use crate::principal_handlers::{PrincipalCaller, PrincipalHandler};
use crate::routes::AppState;
use crate::rpc_errors::{
    encode_reply, internal, invalid_params_ns, malformed, permission_denied_ns,
};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// The error family every refusal here answers in.
const NS: &str = "folders";

/// The refusal for a folder this principal may not deposit into — one
/// answer for a scope that names another folder, a missing or lapsed grant,
/// and a folder that is not the account's or does not exist.
fn not_depositable() -> RpcError {
    permission_denied_ns(
        NS,
        "fauna.folders.deposit: not a folder this principal may deposit into",
    )
}

/// Do the caller's scopes name `folder_id`'s deposit arm?
fn scope_names_folder(scopes: &[String], folder_id: i64) -> bool {
    scopes
        .iter()
        .any(|s| fauna_bridge_atproto::fauna_scope::folder_deposit_qualifier(s) == Some(folder_id))
}

/// Does any of `blobs` carry a `deposit` tuple over `folder_id` whose window
/// is open at `now`? The door's re-resolution of the owner's grant.
fn any_grant_admits_deposit<'a>(
    blobs: impl IntoIterator<Item = &'a [u8]>,
    folder_id: i64,
    now: i64,
) -> bool {
    blobs.into_iter().any(|blob| {
        fauna_mls::wrapped_blob::GrantBlob::from_canonical_bytes(blob).is_ok_and(|grant| {
            // Both bounds, through the one window check every authorizing
            // decode names (`state.rs`'s census).
            fauna_mls::wrapped_blob::grant_window_is_open(&grant, now)
                && grant
                    .scope
                    .iter()
                    .any(|t| t.is_folder_deposit_for(folder_id))
        })
    })
}

/// The deposit itself, behind the dispatch chokepoint: the checks the module
/// doc lists, then the seal and the park.
async fn deposit(
    state: &AppState,
    caller: &PrincipalCaller,
    req: FolderDepositRequest,
) -> Result<FolderDepositReply, RpcError> {
    if !is_deposit_name(&req.name) {
        return Err(invalid_params_ns(
            NS,
            "name must be one plain file-name component",
        ));
    }
    if req.body.len() > MAX_DEPOSIT_BYTES {
        return Err(invalid_params_ns(
            NS,
            format!("a deposit carries at most {MAX_DEPOSIT_BYTES} bytes"),
        ));
    }
    if !scope_names_folder(&caller.scopes, req.folder_id) {
        return Err(not_depositable());
    }
    let Some(holder) = caller.holder_x25519 else {
        return Err(not_depositable());
    };
    let now = crate::db::now_epoch_secs();
    let grants = state
        .db
        .fetch_capability_grants_for_holder_owned_by(&caller.account, &holder, now)
        .await
        .map_err(internal)?;
    if !any_grant_admits_deposit(grants.iter().map(Vec::as_slice), req.folder_id, now) {
        return Err(not_depositable());
    }
    let folder = state
        .db
        .get_folder_by_id(req.folder_id)
        .await
        .map_err(internal)?
        .filter(|f| f.actor_id.as_slice() == caller.account.as_slice())
        .ok_or_else(not_depositable)?;
    if crate::folder_handlers::residency_of(&folder) == "metadata_only" {
        return Err(permission_denied_ns(
            NS,
            "fauna.folders.deposit: the folder keeps no content on this server (metadata-only \
             residency), so there is nowhere to park a deposit",
        ));
    }
    let Some(key) = state
        .db
        .get_recipient_seal_key(&caller.account)
        .await
        .map_err(internal)?
    else {
        return Err(crate::rpc_errors::unavailable_ns(
            NS,
            "the account has published no recipient key to seal a deposit to",
        ));
    };
    let envelope = fauna_protocol::encode_canonical(&DepositEnvelope {
        name: req.name,
        content_type: req.content_type,
        body: req.body,
        extra: Default::default(),
    })
    .map_err(internal)?;
    let sealed = crate::bridge_routing_handlers::seal_recipient_blob(
        &envelope,
        &key.mls_pubkey,
        Some(key.mlkem_ek.as_slice()),
        "folder deposit",
    )?;
    state
        .db
        .insert_folder_deposit(folder.id, &caller.principal_id, &sealed, now)
        .await
        .map_err(internal)?;
    // The owner's connected seats adopt on the nudge rather than at their
    // next rescan tick.
    crate::sync_handlers::notify_owner_sync_changed(state, &folder);
    Ok(FolderDepositReply {
        accepted: true,
        extra: Default::default(),
    })
}

/// `fauna.folders.deposit` for a third-party **principal** — the WS-RPC door.
pub(crate) fn principal_deposit_handler() -> PrincipalHandler {
    Box::new(|state, caller, payload| {
        Box::pin(async move {
            let req: FolderDepositRequest = decode(&payload).map_err(malformed)?;
            encode_reply(&deposit(&state, &caller, req).await?)
        })
    })
}

/// The actor-side half of `fauna.folders.deposit`: the kind's wire contract
/// lives in the actor router (one kind, one wire contract), but NO actor
/// class holds it — the matrix arm is `ThirdParty` only — so this handler
/// runs the central gate and is refused there, always (the
/// `fauna.nostr.bunker.bind` precedent).
fn deposit_actor_handler() -> RpcHandler {
    Box::new(|state, actor_id, _payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission_default(
                &state,
                &actor_id,
                KIND_FOLDERS_DEPOSIT,
            )
            .await?;
            // Unreachable while the matrix arm admits no actor class.
            Err(crate::rpc_errors::central_permission_denied())
        })
    })
}

/// The folder `folder_id` when `actor_id` owns it — the owner's half of the
/// inbox reaches the account's own folders only, and a folder it does not
/// own answers as one that does not exist.
async fn owned_folder(
    state: &AppState,
    actor_id: &[u8; 32],
    folder_id: i64,
) -> Result<i64, RpcError> {
    state
        .db
        .get_folder_by_id(folder_id)
        .await
        .map_err(internal)?
        .filter(|f| f.actor_id.as_slice() == actor_id.as_slice())
        .map(|f| f.id)
        .ok_or_else(|| crate::rpc_errors::not_found_ns(NS, "folder not found"))
}

/// `fauna.folders.deposits.list` — one page of the owner's parked items,
/// sealed as they rest: the nest holds no key to open them, and adoption opens
/// them on the owner's seat.
fn deposits_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission_default(
                &state,
                &actor_id,
                KIND_FOLDERS_DEPOSITS_LIST,
            )
            .await?;
            let req: FolderDepositsListRequest = decode(&payload).map_err(malformed)?;
            let folder_id = owned_folder(&state, &actor_id, req.folder_id).await?;
            let (rows, more) = state
                .db
                .list_folder_deposits_page(folder_id, req.after, DEPOSITS_LIST_PAGE_BYTES)
                .await
                .map_err(internal)?;
            encode_reply(&FolderDepositsListReply {
                items: rows
                    .into_iter()
                    .map(|r| ParkedDeposit {
                        id: r.id,
                        sealed: ByteBuf::from(r.sealed),
                        received_at: r.received_at,
                        extra: Default::default(),
                    })
                    .collect(),
                more,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.folders.deposits.retire` — drop one parked item once the owner's
/// seat has recorded its adopted change row. Idempotent: an item already
/// retired answers `retired: false`.
fn deposits_retire_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission_default(
                &state,
                &actor_id,
                KIND_FOLDERS_DEPOSITS_RETIRE,
            )
            .await?;
            let req: FolderDepositsRetireRequest = decode(&payload).map_err(malformed)?;
            let folder_id = owned_folder(&state, &actor_id, req.folder_id).await?;
            let retired = state
                .db
                .retire_folder_deposit(folder_id, req.deposit_id)
                .await
                .map_err(internal)?;
            encode_reply(&FolderDepositsRetireReply {
                retired,
                extra: Default::default(),
            })
        })
    })
}

/// Register the deposit kind on the actor router. NOT replay-safe: every
/// accepted call parks one more item. 30 s covers a full-size body's seal.
/// Beside it, the owner's list and retire: a read and an idempotent drop.
pub fn register_folder_deposit_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        KIND_FOLDERS_DEPOSIT,
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: deposit_actor_handler(),
        },
    );
    b.add(
        KIND_FOLDERS_DEPOSITS_LIST,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: deposits_list_handler(),
        },
    );
    b.add(
        KIND_FOLDERS_DEPOSITS_RETIRE,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: deposits_retire_handler(),
        },
    );
}

/// The HTTP door's query: the file's name, and its media type comes from the
/// request's own `Content-Type`.
#[derive(serde::Deserialize)]
pub struct DepositQuery {
    name: String,
}

/// `POST /api/v1/folders/{id}/deposit` — a remote principal's deposit. The
/// token rides `Authorization: DPoP <token>` with its `DPoP` proof (RFC 9449;
/// `htm` POST, `htu` this folder's door); the body is the file's bytes and
/// `?name=` its name. Answers `202 {"accepted": true}` and nothing else.
pub async fn deposit_http_handler(
    State(state): State<Arc<AppState>>,
    Path(folder_id): Path<i64>,
    Query(query): Query<DepositQuery>,
    headers: HeaderMap,
    crate::registration::OptionalConnectInfo(peer_addr): crate::registration::OptionalConnectInfo,
    body: BodyBytes,
) -> Response {
    let htu = fauna_bridge_atproto::oauth_metadata::folder_deposit_htu(
        &state.web_serving_domain(),
        folder_id,
    );
    let presented = crate::principal_session::parse_http_presentation(&headers);
    let admission = match crate::principal_session::admit_presentation(
        &state,
        presented.as_ref(),
        "POST",
        &htu,
        peer_addr.map(|a| a.ip()),
    )
    .await
    {
        Ok(admission) => admission,
        Err(response) => return response,
    };
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let payload = match fauna_protocol::encode_canonical(&FolderDepositRequest {
        folder_id,
        name: query.name,
        content_type,
        body: ByteBuf::from(body.to_vec()),
        extra: Default::default(),
    }) {
        Ok(p) => Bytes::from(p.to_vec()),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    match crate::principal_handlers::dispatch_principal(
        state.clone(),
        &admission.binding,
        KIND_FOLDERS_DEPOSIT,
        payload,
    )
    .await
    {
        Ok(_) => (
            StatusCode::ACCEPTED,
            axum::Json(serde_json::json!({ "accepted": true })),
        )
            .into_response(),
        Err(err) => crate::principal_session::rpc_error_response(&err),
    }
}

#[cfg(test)]
mod tests;
