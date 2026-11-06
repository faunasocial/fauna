//! **Third-party folder deposit ingress** — the nest side
//! (`docs/goal/behavior/folder-deposit.md`).
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
//! the principal attested a writer key and the owner minted a LIVE keyless
//! `deposit` grant over this folder to the key the principal attested,
//! confined to that writer (the audit + revocation record,
//! `encryption-at-rest.md` § Capability tiering → *Third-party holders*,
//! re-resolved at every deposit like the oracle's `identity.op` grants); that
//! the folder is the account's own; and that its residency is not
//! metadata-only — the one ruled refusal pair (`ui/folders.md` § Audience and
//! website serving → *Third-party deposit and read*: nothing to park). Every
//! refusal but that last answers alike, so a depositor never learns whether a
//! folder it does not hold exists. Before it seals, the handler verifies the
//! depositor's signature over the [`DepositStatement`] under the caller's own
//! attested writer key — never a key the request names — and writes that key
//! and the signature into the sealed envelope (`folder-deposit.md` § The
//! depositor signs, the owner's device verifies).

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde_bytes::ByteBuf;

use fauna_protocol::folders::{
    DEPOSIT_UPLOAD_TOKEN_BYTES, DEPOSITS_LIST_PAGE_BYTES, DepositEnvelope, DepositPart,
    DepositStatement, FolderDepositBeginReply, FolderDepositBeginRequest,
    FolderDepositCommitRequest, FolderDepositPartRequest, FolderDepositReply, FolderDepositRequest,
    FolderDepositsListReply, FolderDepositsListRequest, FolderDepositsPartReply,
    FolderDepositsPartRequest, FolderDepositsRetireReply, FolderDepositsRetireRequest,
    KIND_FOLDERS_DEPOSIT, KIND_FOLDERS_DEPOSIT_BEGIN, KIND_FOLDERS_DEPOSIT_COMMIT,
    KIND_FOLDERS_DEPOSIT_PART, KIND_FOLDERS_DEPOSITS_LIST, KIND_FOLDERS_DEPOSITS_PART,
    KIND_FOLDERS_DEPOSITS_RETIRE, MAX_DEPOSIT_BYTES, MAX_DEPOSIT_CONTENT_TYPE_BYTES,
    MAX_DEPOSIT_PART_BYTES, MAX_DEPOSIT_PARTS, MAX_DEPOSIT_TOTAL_BYTES, ParkedDeposit,
    is_deposit_name,
};
use fauna_protocol::{RpcError, decode_strict as decode};

use crate::db::folder_deposit_inbox::DepositCommit;
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

/// Does any of `blobs` carry a `deposit` tuple over `folder_id` confined to
/// `writer`, whose window is open at `now`? The door's re-resolution of the
/// owner's grant: a tuple naming another writer admits nothing.
fn any_grant_admits_deposit<'a>(
    blobs: impl IntoIterator<Item = &'a [u8]>,
    folder_id: i64,
    writer: &[u8; 32],
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
                    .any(|t| t.names_deposit_writer(folder_id, writer))
        })
    })
}

/// The most sealed bytes one account's inbox segment holds, across all its
/// folders and its staged uploads, whatever its tier — the backstop that bounds
/// the inbox on a nest that enforces no tier quota (the single-user desktop
/// nest). Where tier quotas are enforced, the tier's `max_inbox_bytes` binds
/// below it (`folder-deposit.md` § The inbox quota).
pub(crate) const DEPOSIT_INBOX_CEILING_BYTES: u64 = 1 << 30;

// The per-deposit bound is the inbox ceiling: no single deposit could rest
// past it, and adoption streams, so nothing smaller binds.
const _: () = assert!(MAX_DEPOSIT_TOTAL_BYTES == DEPOSIT_INBOX_CEILING_BYTES);

/// How long a staged upload may sit idle before it expires with its parts.
pub(crate) const DEPOSIT_UPLOAD_IDLE_SECS: i64 = 60 * 60;

/// The most staged uploads one principal may hold open at once, across every
/// folder it deposits into. A begun upload parks no byte the inbox quota
/// counts until its first part, so without this count a principal looping
/// `fauna.folders.deposit.begin` piles up rows that every sweep then scans
/// under the database lock (`folder-deposit.md` § Deposits larger than one
/// message).
pub(crate) const MAX_STAGED_DEPOSIT_UPLOADS_PER_PRINCIPAL: u32 = 16;

/// The most staged uploads one folder may hold open at once, across every
/// principal depositing into it — the bound on the owner's side, however many
/// principals the owner consented to.
pub(crate) const MAX_STAGED_DEPOSIT_UPLOADS_PER_FOLDER: u32 = 64;

/// The typed refusal for a begin past
/// [`MAX_STAGED_DEPOSIT_UPLOADS_PER_PRINCIPAL`] or
/// [`MAX_STAGED_DEPOSIT_UPLOADS_PER_FOLDER`] (HTTP `429`).
fn too_many_uploads() -> RpcError {
    crate::rpc_errors::coded_ns(
        NS,
        "too_many_uploads",
        "too many staged uploads are open; commit or abandon one (an idle upload expires \
         after an hour) before beginning another",
    )
}

/// The typed refusal for a deposit or part that would take the account's
/// inbox segment past its quota.
fn inbox_full() -> RpcError {
    crate::rpc_errors::coded_ns(
        NS,
        "inbox_full",
        "the folder owner's deposit inbox is full until their apps adopt what is parked",
    )
}

/// The account's inbox quota in sealed bytes: the ceiling, and below it the
/// tier's `max_inbox_bytes` where tier quotas are enforced (`0` — the storage-
/// only `backup` tier — admits nothing).
async fn inbox_limit(state: &AppState, account: &[u8; 32]) -> Result<u64, RpcError> {
    let mut limit = DEPOSIT_INBOX_CEILING_BYTES;
    if *state.enforce_tier_quotas.read().await
        && let Some(max) = state
            .db
            .get_user_tier_max_inbox_bytes(account)
            .await
            .map_err(internal)?
    {
        limit = limit.min(u64::try_from(max).unwrap_or(0));
    }
    Ok(limit)
}

/// What every deposit call is admitted with: the folder, the key its items
/// seal to, the inbox quota, and the caller's attested writer key — the one
/// its signature is verified under and the envelope names.
struct Admitted {
    folder: crate::db::FolderRow,
    key: crate::db::bridge_routing::RecipientSealKey,
    limit: u64,
    writer: [u8; 32],
}

/// The checks the module doc lists, run at EVERY deposit call — a
/// single-message deposit, and each begin, part and commit of a larger one —
/// so revoking the grant or the principal ends an upload mid-way.
async fn admit(
    state: &AppState,
    caller: &PrincipalCaller,
    folder_id: i64,
) -> Result<Admitted, RpcError> {
    if !scope_names_folder(&caller.scopes, folder_id) {
        return Err(not_depositable());
    }
    let Some(holder) = caller.holder_x25519 else {
        return Err(not_depositable());
    };
    // A principal that attested no writer key cannot sign, so cannot deposit.
    let Some(writer) = caller.writer_ed25519 else {
        return Err(not_depositable());
    };
    let now = crate::db::now_epoch_secs();
    let grants = state
        .db
        .fetch_capability_grants_for_holder_owned_by(&caller.account, &holder, now)
        .await
        .map_err(internal)?;
    if !any_grant_admits_deposit(grants.iter().map(Vec::as_slice), folder_id, &writer, now) {
        return Err(not_depositable());
    }
    let folder = state
        .db
        .get_folder_by_id(folder_id)
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
    let limit = inbox_limit(state, &caller.account).await?;
    Ok(Admitted {
        folder,
        key,
        limit,
        writer,
    })
}

/// Verify the depositor's signature over `statement` under `writer` — a
/// missing or failing one is `invalid_params`.
fn verify_statement(
    statement: &DepositStatement,
    writer: &[u8; 32],
    sig: &[u8],
) -> Result<(), RpcError> {
    statement.verify(writer, sig).map_err(|e| {
        invalid_params_ns(
            NS,
            format!(
                "fauna.folders.deposit: {e} under the writer key this principal attested \
                 (folder-deposit.md § The depositor signs)"
            ),
        )
    })
}

/// Seal `value`'s canonical CBOR to the owner's recipient key — the
/// recipient-blob seal the nest's other seal sites use.
fn seal_to_owner<T: serde::Serialize>(
    key: &crate::db::bridge_routing::RecipientSealKey,
    value: &T,
) -> Result<Vec<u8>, RpcError> {
    let plaintext = fauna_protocol::encode_canonical(value).map_err(internal)?;
    crate::bridge_routing_handlers::seal_recipient_blob(
        fauna_mls::wrapped_blob::RecordKind::Mail,
        &plaintext,
        &key.mls_pubkey,
        Some(key.mlkem_ek.as_slice()),
        "folder deposit",
    )
}

fn accepted() -> FolderDepositReply {
    FolderDepositReply {
        accepted: true,
        extra: Default::default(),
    }
}

/// A media type longer than [`MAX_DEPOSIT_CONTENT_TYPE_BYTES`] is refused on
/// both kinds: it is sealed into the item, and its bytes must stay small.
fn require_deposit_content_type(content_type: &str) -> Result<(), RpcError> {
    if content_type.len() <= MAX_DEPOSIT_CONTENT_TYPE_BYTES {
        Ok(())
    } else {
        Err(invalid_params_ns(
            NS,
            format!("content_type is at most {MAX_DEPOSIT_CONTENT_TYPE_BYTES} bytes"),
        ))
    }
}

fn require_deposit_name(name: &str) -> Result<(), RpcError> {
    if is_deposit_name(name) {
        Ok(())
    } else {
        Err(invalid_params_ns(
            NS,
            "name must be one plain file-name component",
        ))
    }
}

/// A single-message deposit, behind the dispatch chokepoint: the checks, then
/// the seal and the park.
async fn deposit(
    state: &AppState,
    caller: &PrincipalCaller,
    req: FolderDepositRequest,
) -> Result<FolderDepositReply, RpcError> {
    require_deposit_name(&req.name)?;
    require_deposit_content_type(&req.content_type)?;
    if req.body.len() > MAX_DEPOSIT_BYTES {
        return Err(invalid_params_ns(
            NS,
            format!(
                "a deposit carries at most {MAX_DEPOSIT_BYTES} bytes in one message; a larger \
                 one goes through fauna.folders.deposit.begin"
            ),
        ));
    }
    let Admitted {
        folder,
        key,
        limit,
        writer,
    } = admit(state, caller, req.folder_id).await?;
    // The nest holds the whole plaintext here, so the statement binds the
    // body too.
    let statement =
        DepositStatement::for_body(req.folder_id, &req.name, &req.content_type, &req.body);
    verify_statement(&statement, &writer, &req.sig)?;
    let sealed = seal_to_owner(
        &key,
        &DepositEnvelope {
            name: req.name,
            content_type: req.content_type,
            body: req.body,
            parts: 0,
            writer: Some(writer),
            sig: req.sig,
            ..Default::default()
        },
    )?;
    let now = crate::db::now_epoch_secs();
    state
        .db
        .insert_folder_deposit(folder.id, &caller.principal_id, &sealed, now, limit)
        .await
        .map_err(internal)?
        .ok_or_else(inbox_full)?;
    // The owner's connected seats adopt on the nudge rather than at their
    // next rescan tick.
    crate::sync_handlers::notify_owner_sync_changed(state, &folder);
    Ok(accepted())
}

/// Drop every staged upload idle past [`DEPOSIT_UPLOAD_IDLE_SECS`] — run where
/// uploads begin and where the owner's seats list, so a vanished principal's
/// bytes come back without a timer of their own.
async fn expire_idle_uploads(state: &AppState) -> Result<(), RpcError> {
    let cutoff = crate::db::now_epoch_secs() - DEPOSIT_UPLOAD_IDLE_SECS;
    state
        .db
        .expire_staged_folder_deposits(cutoff)
        .await
        .map_err(internal)?;
    Ok(())
}

/// The staged upload `upload` names, as `caller` began it in `folder_id`:
/// its row id and whether it is committed.
async fn find_upload(
    state: &AppState,
    caller: &PrincipalCaller,
    folder_id: i64,
    upload: &[u8],
) -> Result<(i64, bool), RpcError> {
    if upload.len() != DEPOSIT_UPLOAD_TOKEN_BYTES {
        return Err(invalid_params_ns(NS, "not an upload token"));
    }
    state
        .db
        .find_folder_deposit_upload(folder_id, &caller.principal_id, upload)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            crate::rpc_errors::not_found_ns(
                NS,
                "no staged upload by this token (never begun, or idle past its expiry)",
            )
        })
}

/// `fauna.folders.deposit.begin` — open a staged upload.
async fn deposit_begin(
    state: &AppState,
    caller: &PrincipalCaller,
    req: FolderDepositBeginRequest,
) -> Result<FolderDepositBeginReply, RpcError> {
    let Admitted { folder, .. } = admit(state, caller, req.folder_id).await?;
    expire_idle_uploads(state).await?;
    let mut token = [0u8; DEPOSIT_UPLOAD_TOKEN_BYTES];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut token);
    state
        .db
        .begin_folder_deposit_upload(
            folder.id,
            &caller.principal_id,
            &token,
            crate::db::now_epoch_secs(),
            MAX_STAGED_DEPOSIT_UPLOADS_PER_PRINCIPAL,
            MAX_STAGED_DEPOSIT_UPLOADS_PER_FOLDER,
        )
        .await
        .map_err(internal)?
        .ok_or_else(too_many_uploads)?;
    Ok(FolderDepositBeginReply {
        upload: ByteBuf::from(token.to_vec()),
        extra: Default::default(),
    })
}

/// `fauna.folders.deposit.part` — seal one part on arrival and stage it.
async fn deposit_part(
    state: &AppState,
    caller: &PrincipalCaller,
    req: FolderDepositPartRequest,
) -> Result<FolderDepositReply, RpcError> {
    if req.body.len() > MAX_DEPOSIT_PART_BYTES {
        return Err(invalid_params_ns(
            NS,
            format!("a part carries at most {MAX_DEPOSIT_PART_BYTES} bytes"),
        ));
    }
    if req.index >= MAX_DEPOSIT_PARTS {
        return Err(invalid_params_ns(
            NS,
            format!("a deposit has at most {MAX_DEPOSIT_PARTS} parts"),
        ));
    }
    let Admitted {
        folder, key, limit, ..
    } = admit(state, caller, req.folder_id).await?;
    let (id, committed) = find_upload(state, caller, folder.id, &req.upload).await?;
    if committed {
        return Err(crate::rpc_errors::invalid_request_ns(
            NS,
            "this upload is committed; it takes no more parts",
        ));
    }
    let plain_len = req.body.len() as u64;
    let sealed = seal_to_owner(
        &key,
        &DepositPart {
            index: req.index,
            body: req.body,
            extra: Default::default(),
        },
    )?;
    if !state
        .db
        .put_folder_deposit_part(
            id,
            req.index,
            &sealed,
            plain_len,
            crate::db::now_epoch_secs(),
            limit,
        )
        .await
        .map_err(internal)?
    {
        return Err(inbox_full());
    }
    Ok(accepted())
}

/// `fauna.folders.deposit.commit` — verify the depositor's signature, seal the
/// header (the name, the media type, the part count, the writer and the
/// signature) and make the item adoptable in one write.
///
/// The parts may arrive out of order or be re-sent and rest sealed, so the
/// nest cannot hash them: the statement is built from the request's own
/// `hash` and the staged parts' summed plaintext length, and verifies
/// authorship alone — the body binding is the adopter's (a principal lying
/// about its own hash loses only its own deposit).
async fn deposit_commit(
    state: &AppState,
    caller: &PrincipalCaller,
    req: FolderDepositCommitRequest,
) -> Result<FolderDepositReply, RpcError> {
    if req.parts == 0 || req.parts > MAX_DEPOSIT_PARTS {
        return Err(invalid_params_ns(
            NS,
            format!("a staged deposit has 1 to {MAX_DEPOSIT_PARTS} parts"),
        ));
    }
    require_deposit_content_type(&req.content_type)?;
    let Admitted {
        folder,
        key,
        limit,
        writer,
    } = admit(state, caller, req.folder_id).await?;
    let (id, committed) = find_upload(state, caller, folder.id, &req.upload).await?;
    if committed {
        return Ok(accepted());
    }
    if let Err(e) = require_deposit_name(&req.name) {
        // Nothing the principal can do makes this upload committable: give
        // its bytes back now rather than at its expiry.
        state
            .db
            .discard_folder_deposit_upload(id)
            .await
            .map_err(internal)?;
        return Err(e);
    }
    let hash: [u8; 32] = req.hash.as_slice().try_into().map_err(|_| {
        invalid_params_ns(
            NS,
            "hash is the whole body's 32-byte BLAKE3 (folder-deposit.md § The depositor signs)",
        )
    })?;
    let statement = DepositStatement {
        folder_id: req.folder_id,
        name: req.name.clone(),
        content_type: req.content_type.clone(),
        len: state
            .db
            .staged_folder_deposit_plain_length(id)
            .await
            .map_err(internal)?,
        hash,
    };
    verify_statement(&statement, &writer, &req.sig)?;
    let header = seal_to_owner(
        &key,
        &DepositEnvelope {
            name: req.name,
            content_type: req.content_type,
            body: ByteBuf::new(),
            parts: req.parts,
            writer: Some(writer),
            sig: req.sig,
            ..Default::default()
        },
    )?;
    match state
        .db
        .commit_folder_deposit_upload(id, req.parts, &header, crate::db::now_epoch_secs(), limit)
        .await
        .map_err(internal)?
    {
        DepositCommit::Committed => {
            crate::sync_handlers::notify_owner_sync_changed(state, &folder);
            Ok(accepted())
        }
        DepositCommit::AlreadyCommitted => Ok(accepted()),
        DepositCommit::InboxFull => Err(inbox_full()),
        DepositCommit::PartsMismatch => Err(invalid_params_ns(
            NS,
            "the staged parts are not exactly the indices 0..parts",
        )),
        DepositCommit::Missing => Err(crate::rpc_errors::not_found_ns(
            NS,
            "no staged upload by this token (never begun, or idle past its expiry)",
        )),
    }
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

/// The principal handlers of a deposit larger than one message —
/// `fauna.folders.deposit.{begin,part,commit}`, the same gate at each call.
pub(crate) fn principal_staged_deposit_handlers() -> Vec<(&'static str, PrincipalHandler)> {
    vec![
        (
            KIND_FOLDERS_DEPOSIT_BEGIN,
            Box::new(|state, caller, payload| {
                Box::pin(async move {
                    let req: FolderDepositBeginRequest = decode(&payload).map_err(malformed)?;
                    encode_reply(&deposit_begin(&state, &caller, req).await?)
                })
            }),
        ),
        (
            KIND_FOLDERS_DEPOSIT_PART,
            Box::new(|state, caller, payload| {
                Box::pin(async move {
                    let req: FolderDepositPartRequest = decode(&payload).map_err(malformed)?;
                    encode_reply(&deposit_part(&state, &caller, req).await?)
                })
            }),
        ),
        (
            KIND_FOLDERS_DEPOSIT_COMMIT,
            Box::new(|state, caller, payload| {
                Box::pin(async move {
                    let req: FolderDepositCommitRequest = decode(&payload).map_err(malformed)?;
                    encode_reply(&deposit_commit(&state, &caller, req).await?)
                })
            }),
        ),
    ]
}

/// The actor-side half of `fauna.folders.deposit`: the kind's wire contract
/// lives in the actor router (one kind, one wire contract), but NO actor
/// class holds it — the matrix arm is `ThirdParty` only — so this handler
/// runs the central gate and is refused there, always (the
/// `fauna.nostr.bunker.bind` precedent).
fn deposit_actor_handler(kind: &'static str) -> RpcHandler {
    Box::new(move |state, actor_id, _payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission_default(&state, &actor_id, kind)
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
            // Every rescan tick of an owner's seat lists, so this is where an
            // abandoned upload's bytes come back when no deposit follows it.
            expire_idle_uploads(&state).await?;
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

/// `fauna.folders.deposits.part` — one sealed part of a parked item larger
/// than one message, which adoption opens in index order.
fn deposits_part_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission_default(
                &state,
                &actor_id,
                KIND_FOLDERS_DEPOSITS_PART,
            )
            .await?;
            let req: FolderDepositsPartRequest = decode(&payload).map_err(malformed)?;
            let folder_id = owned_folder(&state, &actor_id, req.folder_id).await?;
            let sealed = state
                .db
                .get_folder_deposit_part(folder_id, req.deposit_id, req.index)
                .await
                .map_err(internal)?
                .ok_or_else(|| crate::rpc_errors::not_found_ns(NS, "no such deposit part"))?;
            encode_reply(&FolderDepositsPartReply {
                sealed: ByteBuf::from(sealed),
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
            handler: deposit_actor_handler(KIND_FOLDERS_DEPOSIT),
        },
    );
    // A larger deposit: `begin` opens a fresh upload per call, never
    // replayed; a re-sent part replaces itself and a repeated commit answers
    // accepted, so both are replay-safe.
    for (kind, forbid_replay, secs) in [
        (KIND_FOLDERS_DEPOSIT_BEGIN, true, 5),
        (KIND_FOLDERS_DEPOSIT_PART, false, 30),
        (KIND_FOLDERS_DEPOSIT_COMMIT, false, 10),
    ] {
        b.add(
            kind,
            RpcKindMeta {
                forbid_replay,
                default_deadline: Duration::from_secs(secs),
                handler: deposit_actor_handler(kind),
            },
        );
    }
    b.add(
        KIND_FOLDERS_DEPOSITS_PART,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: deposits_part_handler(),
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

/// The header the HTTP door's depositor signature rides: the 64-byte Ed25519
/// signature over the [`DepositStatement`], unpadded base64url
/// (`folder-deposit.md` § The depositor signs, the owner's device verifies).
pub const DEPOSIT_SIGNATURE_HEADER: &str = "fauna-deposit-signature";

/// The signature [`DEPOSIT_SIGNATURE_HEADER`] carries; `None` when it is
/// absent or not 64 bytes of unpadded base64url.
fn deposit_signature(headers: &HeaderMap) -> Option<[u8; 64]> {
    use base64::Engine as _;
    let value = headers.get(DEPOSIT_SIGNATURE_HEADER)?.to_str().ok()?;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value.trim())
        .ok()?
        .try_into()
        .ok()
}

/// `POST /api/v1/folders/{id}/deposit` — a remote principal's deposit. The
/// token rides `Authorization: DPoP <token>` with its `DPoP` proof (RFC 9449;
/// `htm` POST, `htu` this folder's door); the body is the file's bytes,
/// `?name=` its name and [`DEPOSIT_SIGNATURE_HEADER`] the depositor's
/// signature over the statement of the folder, the name, the request's
/// `Content-Type`, the body's length and its BLAKE3 (`400` when absent or
/// malformed). Answers `202 {"accepted": true}` and nothing else.
///
/// The body streams: a file that fits one message goes through
/// `fauna.folders.deposit` as it always has; a larger one — up to
/// [`MAX_DEPOSIT_TOTAL_BYTES`] — is cut into parts as it arrives and goes
/// through `fauna.folders.deposit.{begin,part,commit}`, each part sealed before
/// the next is read, so the door never holds more than two parts of plaintext
/// and the item becomes adoptable only once the whole body has arrived. The
/// door hashes the body as it streams and verifies the signature over the
/// streamed length and hash before it dispatches the kind that makes the item
/// adoptable, so the body binding holds on this door whatever its size. The
/// route is mounted outside the router's blanket body limit for this reason
/// (`lib.rs`); this handler is the one that bounds it.
pub async fn deposit_http_handler(
    State(state): State<Arc<AppState>>,
    Path(folder_id): Path<i64>,
    Query(query): Query<DepositQuery>,
    headers: HeaderMap,
    crate::registration::OptionalConnectInfo(peer_addr): crate::registration::OptionalConnectInfo,
    body: axum::body::Body,
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
    let Some(sig) = deposit_signature(&headers) else {
        return crate::principal_session::rpc_error_response(&invalid_params_ns(
            NS,
            "the deposit carries no Fauna-Deposit-Signature header of 64 bytes, unpadded \
             base64url (folder-deposit.md § The depositor signs)",
        ));
    };
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let door = HttpDoor {
        state,
        binding: admission.binding,
        folder_id,
    };
    match door.deposit(query.name, content_type, sig, body).await {
        Ok(()) => (
            StatusCode::ACCEPTED,
            axum::Json(serde_json::json!({ "accepted": true })),
        )
            .into_response(),
        Err(DoorError::Rpc(err)) => crate::principal_session::rpc_error_response(&err),
        Err(DoorError::Status(status)) => status.into_response(),
    }
}

/// Why the HTTP door refused: a refusal of the kind it dispatched, or the
/// body itself (too large, or cut off mid-way).
enum DoorError {
    Rpc(RpcError),
    Status(StatusCode),
}

impl From<RpcError> for DoorError {
    fn from(err: RpcError) -> Self {
        Self::Rpc(err)
    }
}

/// One admitted HTTP deposit: every kind it sends goes through the same
/// principal dispatch the WS-RPC door's does.
struct HttpDoor {
    state: Arc<AppState>,
    binding: crate::principal_handlers::PrincipalBinding,
    folder_id: i64,
}

impl HttpDoor {
    async fn call<Req: serde::Serialize, Reply: serde::de::DeserializeOwned>(
        &self,
        kind: &str,
        req: &Req,
    ) -> Result<Reply, DoorError> {
        let payload = fauna_protocol::encode_canonical(req)
            .map_err(|_| DoorError::Status(StatusCode::INTERNAL_SERVER_ERROR))?;
        let reply = crate::principal_handlers::dispatch_principal(
            self.state.clone(),
            &self.binding,
            kind,
            Bytes::from(payload.to_vec()),
        )
        .await?;
        decode(&reply).map_err(|_| DoorError::Status(StatusCode::INTERNAL_SERVER_ERROR))
    }

    /// Verify `sig` over the statement of the whole streamed body, under the
    /// caller's attested writer key, before the kind that makes the item
    /// adoptable is dispatched. A caller with no writer key (or no longer a
    /// principal) is left to that kind, which refuses it with the gate's
    /// own answer.
    async fn verify_streamed(
        &self,
        statement: &DepositStatement,
        sig: &[u8],
    ) -> Result<(), DoorError> {
        let caller = crate::principal_handlers::resolve_principal(&self.state, &self.binding)
            .await
            .map_err(|_| DoorError::Status(StatusCode::INTERNAL_SERVER_ERROR))?;
        if let Some(writer) = caller.and_then(|c| c.writer_ed25519) {
            verify_statement(statement, &writer, sig)?;
        }
        Ok(())
    }

    async fn deposit(
        &self,
        name: String,
        content_type: String,
        sig: [u8; 64],
        body: axum::body::Body,
    ) -> Result<(), DoorError> {
        use futures_util::StreamExt;
        // Refuse a name or type the commit would refuse before reading any body.
        require_deposit_name(&name)?;
        require_deposit_content_type(&content_type)?;
        let mut hasher = blake3::Hasher::new();
        let mut stream = body.into_data_stream();
        let mut buf: Vec<u8> = Vec::with_capacity(MAX_DEPOSIT_PART_BYTES);
        let mut upload: Option<ByteBuf> = None;
        let mut parts: u32 = 0;
        let mut total: u64 = 0;
        while let Some(frame) = stream.next().await {
            let frame = frame.map_err(|_| DoorError::Status(StatusCode::BAD_REQUEST))?;
            total = total.saturating_add(frame.len() as u64);
            if total > MAX_DEPOSIT_TOTAL_BYTES {
                // The staged parts expire on their own; nothing was committed.
                return Err(DoorError::Status(StatusCode::PAYLOAD_TOO_LARGE));
            }
            hasher.update(&frame);
            buf.extend_from_slice(&frame);
            // A part leaves only once more bytes follow it, so a body that
            // fits one message never opens an upload.
            while buf.len() > MAX_DEPOSIT_PART_BYTES {
                let rest = buf.split_off(MAX_DEPOSIT_PART_BYTES);
                let part = std::mem::replace(&mut buf, rest);
                self.send_part(&mut upload, parts, part).await?;
                parts += 1;
            }
        }
        let statement = DepositStatement {
            folder_id: self.folder_id,
            name: name.clone(),
            content_type: content_type.clone(),
            len: total,
            hash: *hasher.finalize().as_bytes(),
        };
        let Some(_) = upload else {
            self.verify_streamed(&statement, &sig).await?;
            let _: FolderDepositReply = self
                .call(
                    KIND_FOLDERS_DEPOSIT,
                    &FolderDepositRequest {
                        folder_id: self.folder_id,
                        name,
                        content_type,
                        body: ByteBuf::from(buf),
                        sig: ByteBuf::from(sig.to_vec()),
                        ..Default::default()
                    },
                )
                .await?;
            return Ok(());
        };
        self.send_part(&mut upload, parts, buf).await?;
        parts += 1;
        // The staged commit verifies authorship alone; the body binding on
        // this door is this check, over what actually streamed.
        self.verify_streamed(&statement, &sig).await?;
        let _: FolderDepositReply = self
            .call(
                KIND_FOLDERS_DEPOSIT_COMMIT,
                &FolderDepositCommitRequest {
                    folder_id: self.folder_id,
                    upload: upload.expect("opened above"),
                    name,
                    content_type,
                    parts,
                    hash: ByteBuf::from(statement.hash.to_vec()),
                    sig: ByteBuf::from(sig.to_vec()),
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    /// Send part `index`, opening the upload first when this is the first.
    async fn send_part(
        &self,
        upload: &mut Option<ByteBuf>,
        index: u32,
        body: Vec<u8>,
    ) -> Result<(), DoorError> {
        if upload.is_none() {
            let begun: FolderDepositBeginReply = self
                .call(
                    KIND_FOLDERS_DEPOSIT_BEGIN,
                    &FolderDepositBeginRequest {
                        folder_id: self.folder_id,
                        extra: Default::default(),
                    },
                )
                .await?;
            *upload = Some(begun.upload);
        }
        let _: FolderDepositReply = self
            .call(
                KIND_FOLDERS_DEPOSIT_PART,
                &FolderDepositPartRequest {
                    folder_id: self.folder_id,
                    upload: upload.clone().expect("opened above"),
                    index,
                    body: ByteBuf::from(body),
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
