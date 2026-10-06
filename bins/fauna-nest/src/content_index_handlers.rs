//! WS-RPC handlers for the `__index` rail — `fauna.index.{record,list}`
//! (`docs/goal/behavior/content-index.md` § Ingest triggers, v1).
//!
//! A capability-position builder (a user's client; the MDA bridge with rollout
//! slice S5) seals its content-index segments and manifests under keys the nest
//! never holds, uploads the sealed bytes over the HTTP blob route, and records
//! each one here. The record is what appends the `sync_changes` row that
//! replicates the blob to the user's other locations — the same rail structure
//! as `__drafts` ([`crate::drafts_handlers`], [`crate::db::drafts`];
//! storage half here is [`crate::db::content_index_rail`]).
//!
//! **Bytes are deliberately not on this plane.** WS-RPC frames cap at 2 MiB
//! (`fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE`) because bulk binary rides
//! the HTTP routes, so [`KIND_RECORD`] carries only `(path, blob hash, size)`.
//! The corollary the builder must honour — **one segment is one blob**, no
//! chunk-manifest form on this rail — is stated at the owner site.
//!
//! **Two properties this module owes, both of which a naive rail would miss:**
//!
//! - **The blob must already be held.** Recording a reference the nest cannot
//!   resolve would produce a journal row pointing at nothing, which every
//!   reader has to treat as corruption — and it is precisely what the builder's
//!   segment-then-manifest publish order exists to prevent. So `record`
//!   verifies the blob store holds `blob_hash` and refuses otherwise, with a
//!   typed retryable code (`bytes_not_held`) an honest writer never sees.
//! - **The path shape is validated, not trusted.** `fauna_index::paths` is the
//!   source of truth for `__index` virtual paths; the nest re-derives rather
//!   than accepting any string, so a malformed or hostile client cannot stuff
//!   unaddressable rows into a rail no user-facing surface lists.
//!
//! ## Two planes
//!
//! The **user plane** ([`KIND_RECORD`] / [`KIND_LIST`]) is `User | Admin` and
//! self-scoped: the owning actor is the authenticated connection actor, so
//! neither kind carries an `actor_id` and a caller reads and writes only their
//! own index.
//!
//! The **bridge plane** ([`KIND_BRIDGE_LIST`]) is the MDA's read-only reach
//! into a target actor's mail/calendar-class entries (rollout slice S5):
//! `BridgeMda` only, naming its target actor explicitly, and bounded to the
//! mail/calendar key class. It could not have been an allowlist flip on the
//! user plane — those kinds have no `actor_id`, so the MDA would have
//! published into its *own* rail. The write half (`fauna.bridges.index_record`)
//! shipped under the same slice and was retired 2026-08-10 when index coverage
//! moved onto the nest-id secondary field (`content-index.md` § Where the
//! index is built → the carrier ruling) — the MDA no longer builds index
//! segments, only reads the ones a client already published. See the
//! bridge-plane section below and `fauna_protocol::content_index`'s module
//! docs.
//!
//! Caller-class enforcement for both lives in
//! `bridge_method_allowlist::is_permitted`.

use std::sync::Arc;
use std::time::Duration;

use fauna_protocol::{
    RpcError, Value,
    content_index::{
        BridgeListIndexBlobsRequest, IndexBlobEntry, KIND_BRIDGE_LIST, KIND_LIST, KIND_RECORD,
        ListIndexBlobsReply, ListIndexBlobsRequest, RecordIndexBlobReply, RecordIndexBlobRequest,
    },
    decode_strict as decode,
};

use crate::routes::AppState;
use crate::rpc_errors::{encode_reply, internal, malformed};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// Blob storage isn't configured yet (no `BackupService`) — the nest hasn't
/// finished claim/onboarding. The client surfaces this as transient and retries.
fn unavailable(reason: &str) -> RpcError {
    crate::rpc_errors::unavailable_ns("index", reason)
}

/// The writer recorded a blob this nest does not hold. Typed and distinct from
/// `invalid_request` so a client can self-heal by re-uploading and retrying,
/// rather than treating its own segment as malformed.
fn bytes_not_held(reason: &str) -> RpcError {
    let mut e = RpcError::new("fauna.index.bytes_not_held", "error.index.bytes_not_held");
    e.details = Some(Box::new(Value::String(reason.into())));
    e
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// A recorded path must be one `fauna_index::paths` itself can produce: a
/// `__index/<kind>/seg-<8 digits>.idx` segment, or one of the two manifests.
/// Anything else is refused — the nest re-derives the shape rather than
/// trusting the writer's string.
fn validate_index_path(path: &str) -> Result<(), RpcError> {
    let known_manifest =
        path == fauna_index::manifest_path() || path == fauna_index::mailcal_manifest_path();
    if known_manifest || fauna_index::parse_segment_path(path).is_some() {
        return Ok(());
    }
    Err(malformed(
        "path is not an `__index` virtual path — expected `__index/<kind>/seg-<8 digits>.idx`, \
         `__index/manifest.idx`, or `__index/manifest-mailcal.idx`",
    ))
}

// ── fauna.index.record ──────────────────────────────────────────

fn index_record_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_RECORD).await?;
            let req: RecordIndexBlobRequest = decode(&payload).map_err(malformed)?;
            validate_index_path(&req.path)?;
            if req.size_bytes <= 0 {
                return Err(malformed("size_bytes must be positive"));
            }
            let hash_bytes: [u8; 32] = fauna_core::hex32::decode(&req.blob_hash)
                .map_err(|_| malformed("blob_hash must be 32 hex-encoded bytes"))?;

            let backup_svc = state
                .backup_service
                .as_ref()
                .ok_or_else(|| unavailable("blob storage not configured"))?;
            let hash = fauna_core::data::ContentHash::from_digest_raw(hash_bytes);

            // Upload-before-record: a row the nest cannot resolve is worse than
            // a refused write, because the builder's publish order guarantees a
            // manifest only ever references segments that already landed.
            //
            // `len` rather than `exists` because the same probe answers the
            // next question: the nest HOLDS the bytes, so it can measure them
            // instead of believing the caller's figure.
            let held = backup_svc
                .local_blob_store()
                .len(&hash)
                .await
                .map_err(internal)?;
            let Some(held) = held else {
                return Err(bytes_not_held(
                    "this nest holds no blob under that hash — upload the sealed bytes, \
                     then re-record",
                ));
            };

            // The declared size is VERIFIED, not believed.
            // It is not decorative: it lands in `sync_changes.size_bytes`, which
            // is what `fauna.index.list` reports back and what the shared fold
            // planner budgets its merges on — so a wrong figure misplans every
            // later compaction of that slice. The blob PUT now records
            // `blob_metadata` from the body itself, and `put_blob_metadata` is
            // `INSERT OR IGNORE`, so a lie can no longer reach that table; the
            // rail's own row is the surface still open, and this closes it.
            //
            // An equality, not a ceiling: the nest knows the exact answer, and
            // the honest builder declares the sealed length it uploaded.
            if req.size_bytes as u64 != held {
                return Err(malformed(format!(
                    "size_bytes ({}) is not the held blob's length ({held})",
                    req.size_bytes
                )));
            }

            let seq = state
                .db
                .record_index_blob_change(&actor_id, &req.path, &hash_bytes, req.size_bytes)
                .await
                .map_err(internal)?;
            encode_reply(&RecordIndexBlobReply {
                seq,
                extra: Default::default(),
            })
        })
    })
}

// ── Paging, shared by both list planes ──────────────────

/// Row-fetch bound per list page -- a handler **memory** bound, not the page
/// size. Same figure and same derivation as the backup rail's
/// `BACKUP_LIST_FETCH_CAP`: an encoded entry plus its
/// `segments::RECORD_WIRE_OVERHEAD` runs ~250 bytes, so the frame byte budget
/// (~2 MiB) binds at roughly eight thousand rows and therefore binds FIRST.
/// That ordering is what makes the paging additive: a caller that
/// sends no `limit` (the shared rail publisher and the mail bridge send none) is cut only where the frame could not have carried the
/// reply anyway. If every fetched row nonetheless fits the budget, the minted
/// cursor simply yields one extra (possibly empty) page -- never a lost row.
const INDEX_LIST_FETCH_CAP: i64 = 8192;

/// Read one page of `actor`'s live `__index` rows, apply `keep`, and cut the
/// result at the ratified frame budget (`transport.md` § Max frame corollary --
/// close early, never skip-and-continue), minting the resume cursor.
///
/// **The cursor advances past everything READ, not everything SERVED**, which
/// is why this is not the backup rail's `cut_list_page`: `keep` drops
/// master-class rows on the bridge plane *after* the page comes back, so a page
/// of nothing but master-class rows must serve nothing and still move the walk
/// on. Minting from the last served row there would re-read the same rows
/// forever. The genuinely shared primitive is
/// `segments::take_page_within_budget`, which both planes use.
async fn list_index_page(
    state: &std::sync::Arc<AppState>,
    actor: &[u8; 32],
    cursor: Option<&str>,
    limit: i64,
    keep: impl Fn(&crate::db::content_index_rail::IndexBlobRow) -> bool,
) -> Result<ListIndexBlobsReply, RpcError> {
    let fetch = if limit > 0 {
        limit.min(INDEX_LIST_FETCH_CAP)
    } else {
        INDEX_LIST_FETCH_CAP
    };
    let rows = state
        .db
        .list_index_blobs(actor, cursor, fetch)
        .await
        .map_err(internal)?;

    // Captured before the filter: the walk resumes past the last row READ.
    let fetched = rows.len() as i64;
    let last_read = rows.last().map(|r| r.path.clone());

    let kept: Vec<IndexBlobEntry> = rows
        .into_iter()
        .filter(|r| keep(r))
        .map(|r| IndexBlobEntry {
            path: r.path,
            blob_hash: hex::encode(r.blob_hash),
            size_bytes: r.size_bytes,
            extra: Default::default(),
        })
        .collect();

    let (page, rest) = crate::segments::take_page_within_budget(kept, |e| {
        fauna_protocol::encode_canonical(e)
            .map(|b| b.len())
            .unwrap_or(crate::segments::SERVE_PAGE_BUDGET_BYTES + 1)
    });
    // A head row alone over the budget freezes the page LOUDLY. An empty page
    // with no cursor would read as "drained" and hide every row behind it, and
    // skipping it is the one thing the corollary forbids -- on a user's own
    // at-rest index that would be silent, unrecoverable-looking loss.
    if page.is_empty() && !rest.is_empty() {
        return Err(internal(
            "a single `__index` list row exceeds the serve page budget -- page frozen",
        ));
    }

    let next_cursor = if !rest.is_empty() {
        // Stopped on the byte budget mid-page: resume after the last SERVED row.
        page.last().map(|e| e.path.clone())
    } else if fetched == fetch {
        // Everything read was served or deliberately filtered, and the read
        // filled its bound, so more may remain: resume after the last READ row.
        last_read
    } else {
        // The read came back short of its bound: the rail is drained.
        None
    };

    Ok(ListIndexBlobsReply {
        entries: page,
        next_cursor,
        extra: Default::default(),
    })
}

// ── fauna.index.list ────────────────────────────────────────────

fn index_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_LIST).await?;
            let req: ListIndexBlobsRequest = decode(&payload).map_err(malformed)?;
            // Every class, unfiltered: this plane is the owner's own client,
            // which holds every index key.
            let reply =
                list_index_page(&state, &actor_id, req.cursor.as_deref(), req.limit, |_| {
                    true
                })
                .await?;
            encode_reply(&reply)
        })
    })
}

// ── The bridge plane — the MDA's index reach (rollout slice S5) ──
//
// `content-index.md` § Where the index is built ratifies the MDA bridge, during
// an active MUA-AUTH session, as a *reader* of the mail/calendar-class slices a
// client already built (the write half retired 2026-08-10 — see the module
// doc's "Two planes" section above): it holds the MSEK-derived mail/calendar
// index-segment key, so it can reach the mail and calendar slices and nothing
// else (§ Architectural rules #3; `key-material-hierarchy.md` § Path
// B-sibling-4, rule #7's blast radius).
//
// This is a separate kind rather than an allowlist flip on the user plane
// because the user-plane kinds are *self-scoped* — they carry no `actor_id`, so
// an MDA admitted there would publish into the bridge's own rail, never the
// user's. Naming the target explicitly is the shape every other
// MDA-on-behalf-of-user kind already uses (`fetch_mls_snapshot_blob`).

/// Resolve the target actor a bridge names, and rate-limit the (bridge, target)
/// pair the way every other MDA-on-behalf kind does. The synthetic `_index`
/// credential keeps the bucket structure uniform — this rail is not
/// credential-keyed, exactly like the snapshot fetch's `_snapshot`.
fn bridge_target(
    state: &Arc<AppState>,
    bridge_actor: &[u8; 32],
    actor_id: &[u8],
) -> Result<[u8; 32], RpcError> {
    let target: [u8; 32] =
        crate::rpc_errors::require_bytes32("actor_id", actor_id).map_err(malformed)?;
    if !state
        .bridge_rate_limit
        .check(bridge_actor, &target, "_index")
    {
        tracing::warn!(
            target: "bridge_rpc",
            bridge_prefix = crate::bridge_method_allowlist::actor_prefix_hex(bridge_actor),
            target_prefix = crate::bridge_method_allowlist::actor_prefix_hex(&target),
            "rate-limited (index rail)"
        );
        return Err(RpcError::new(
            "fauna.bridges.rate_limited",
            "error.bridges.rate_limited",
        ));
    }
    Ok(target)
}

fn bridge_index_list_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_permission(&state, &bridge_actor, KIND_BRIDGE_LIST).await?;
            let req: BridgeListIndexBlobsRequest = decode(&payload).map_err(malformed)?;
            let target = bridge_target(&state, &bridge_actor, req.actor_id.as_slice())?;
            let reply = list_index_page(
                &state,
                &target,
                req.cursor.as_deref(),
                req.limit,
                // Master-class entries are OMITTED rather than refused: the MDA
                // holds no key for them, so they are noise it could not act on
                // — and withholding them keeps the user's per-kind segment
                // counts away from a MUA-credential-reachable position. Same
                // class boundary as `record`, read side.
                //
                // Filtering AFTER the read is what makes an empty-page-with-a-
                // cursor normal here (see `list_index_page`); the alternative —
                // filtering in SQL — would put a key-class rule the shared
                // `fauna_index` crate owns into a nest query string.
                |r| fauna_index::path_key_class(&r.path) == Some(fauna_index::KindClass::MailCal),
            )
            .await?;
            encode_reply(&reply)
        })
    })
}

// ── Registration entry point ────────────────────────────────────

pub fn register_content_index_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        KIND_RECORD,
        RpcKindMeta {
            // Idempotent: the same (path, blob hash) twice converges to the
            // same live row, so a replay is harmless.
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: index_record_handler(),
        },
    );
    b.add(
        KIND_LIST,
        RpcKindMeta {
            // Pure read of the calling actor's own rail.
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: index_list_handler(),
        },
    );
    // `fauna.bridges.index_record` retired 2026-08-10 with the MDA's build
    // half (`content-index.md` § Where the index is built — the carrier
    // ruling); the MDA is query-only, so only the list kind remains.
    b.add(
        KIND_BRIDGE_LIST,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: bridge_index_list_handler(),
        },
    );
}
