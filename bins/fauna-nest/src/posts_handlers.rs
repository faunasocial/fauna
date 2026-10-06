//! WS-RPC handlers for the user-facing `fauna.posts.*` surface — the
//! create / get / interact plane end-user clients invoke from the feed +
//! post-composer + interaction affordances. Part of the WS-RPC-everywhere
//! migration (tracked internally).
//!
//! Caller-class enforcement lives in `bridge_method_allowlist::is_permitted`
//! (User-only arms for these kinds); the same gate the `fauna.bridges.*`,
//! `fauna.email.*`, and `fauna.conversations.*` user-facing kinds use,
//! partitioned by `CallerClass`.
//!
//! The business logic is **not** duplicated here — each handler decodes its
//! wire request, then calls the shared `pub(crate)` core fns the HTTP twins
//! also call (`routes::ingest_post_core`, `routes::get_post_core`,
//! `interact_routes::interact_with_post_core`), and maps the result onto the
//! `fauna.posts.*` reply / `RpcError` shapes. The create/interact HTTP twins
//! were **DELETED** in the WS-RPC-everywhere rip (T4); only `GET
//! /api/v1/posts/{id}` (`routes::get_post` → `get_post_core`) survives, as
//! permanent public-byte residue — the discovery-feed `fetch_url` a client
//! reads from the author's nest (`api-layers.md` § Remaining HTTP), not a
//! client twin.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use fauna_protocol::{
    RpcError, Value, decode_strict as decode,
    posts::{
        POST_ROOM_LABELS_MAX_IDS, PostCreateReply, PostCreateRequest, PostDeleteReply,
        PostDeleteRequest, PostGetReply, PostGetRequest, PostInteractReply, PostInteractRequest,
        PostRoomLabelsEntry, PostRoomLabelsRemoteRequest, PostRoomLabelsReply,
        PostRoomLabelsRequest, PostsListItem, PostsListReply, PostsListRequest,
    },
};

use crate::api_error::ApiError;
use crate::routes::AppState;
use crate::routes::parse_32_bytes;
use crate::routes::{
    GetPostOutcome, PostCreateError, PostDeleteError, PostDeleteOutcome, RenderSite,
    delete_post_core, get_post_core, ingest_post_core,
};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers ────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

fn permission_denied(reason: &str) -> RpcError {
    crate::rpc_errors::permission_denied_ns("posts", reason)
}

use crate::rpc_errors::internal;

fn invalid_params(reason: &str) -> RpcError {
    crate::rpc_errors::invalid_params_ns("posts", reason)
}

/// The targeted post doesn't exist (or is quarantine-gated for this
/// caller). Mirrors the HTTP twin's `404 NOT_FOUND`.
fn not_found(reason: &str) -> RpcError {
    crate::rpc_errors::not_found_ns("posts", reason)
}

/// The ingest/classify pipeline rejected the post (the HTTP twin's `400
/// ingest rejected` / `403 content_rejected` paths). Mirrors the
/// conversations `fauna.conversations.ingest_failed` convention.
fn ingest_failed(reason: &str) -> RpcError {
    let mut e = RpcError::new("fauna.posts.ingest_failed", "error.posts.ingest_failed");
    e.details = Some(Box::new(Value::String(reason.into())));
    e
}

/// Map an `ApiError` (from `interact_with_post_core`) onto an `RpcError`,
/// preserving the HTTP status semantics: 400 → invalid_params, 403 →
/// permission_denied, 404 → not_found, 501 → unsupported, else internal.
fn rpc_error_from_api(api: ApiError) -> RpcError {
    if api.status == axum::http::StatusCode::NOT_IMPLEMENTED {
        let mut e = RpcError::new("fauna.posts.unsupported", "error.posts.unsupported");
        e.details = Some(Box::new(Value::String(api.message)));
        return e;
    }
    crate::rpc_errors::rpc_error_from_api_ns("posts", api, |ns, reason| {
        crate::rpc_errors::permission_denied_ns(ns, reason)
    })
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

fn parse_post_id(hex_str: &str) -> Result<[u8; 32], RpcError> {
    parse_32_bytes(hex_str).ok_or_else(|| invalid_params("invalid post_id hex"))
}

/// A room id on a `fauna.posts.*` request — the relayed verdict read's only
/// one. A room id *is* its channel id, so the shape is the same 32 bytes; the
/// error stays in this namespace because the kind that carries it does.
fn parse_room_id(hex_str: &str) -> Result<[u8; 32], RpcError> {
    parse_32_bytes(hex_str).ok_or_else(|| invalid_params("invalid room_id hex"))
}

// ── fauna.posts.create ─────────────────────────────────────────

fn posts_create_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.posts.create").await?;
            let req: PostCreateRequest = decode(&payload).map_err(malformed)?;
            let body = Bytes::from(req.body.into_vec());

            // Reuse the full HTTP-twin ingest pipeline — no duplication.
            let post_id = ingest_post_core(&state, actor_id, &body)
                .await
                .map_err(|e| match e {
                    PostCreateError::Ingest(api) => ingest_failed(&api.message),
                    PostCreateError::Internal(msg) => internal(msg),
                })?;

            encode_reply(&PostCreateReply {
                post_id: hex::encode(post_id),
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.posts.delete ─────────────────────────────────────────

fn posts_delete_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.posts.delete").await?;
            let req: PostDeleteRequest = decode(&payload).map_err(malformed)?;

            // Verify-then-decode the signed tombstone (signed-only surface —
            // the envelope signature must verify against `tombstone.author`).
            let tombstone = fauna_core::encoding::decode_tombstone(&req.body).map_err(malformed)?;
            let digest = crate::db::posts::cid_to_digest(&tombstone.post_id);

            match delete_post_core(&state, actor_id, &tombstone, digest, RenderSite::Now).await {
                Ok(outcome) => {
                    // Relay the deletion to the nests the author forwards to,
                    // so a forwarded copy of this post there does not outlive
                    // it (`feed.md` § Post deletion → Propagation — the delete
                    // twin of `maybe_enqueue_outbox`). Gated inside on Private +
                    // the author's own `post_forward` row; a no-op otherwise. The signed
                    // tombstone body (`req.body`) rides verbatim, so the peer
                    // re-verifies the author envelope. Fires on every outcome,
                    // AlreadyGone included — a crash-retry must still chase a
                    // copy a first attempt left behind.
                    crate::routes::maybe_enqueue_delete_outbox(
                        &state,
                        &tombstone.author.0,
                        &req.body,
                    )
                    .await;
                    encode_reply(&PostDeleteReply {
                        post_id: hex::encode(digest),
                        deleted: matches!(outcome, PostDeleteOutcome::Deleted),
                        extra: std::collections::BTreeMap::new(),
                    })
                }
                Err(PostDeleteError::NotAuthor) => {
                    Err(permission_denied("only the author may delete a post"))
                }
                Err(PostDeleteError::Internal(msg)) => Err(internal(msg)),
            }
        })
    })
}

// ── fauna.posts.get ────────────────────────────────────────────

fn posts_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.posts.get").await?;
            let req: PostGetRequest = decode(&payload).map_err(malformed)?;
            let post_id = parse_post_id(&req.post_id)?;

            // The quarantine visibility gate keys on the connection actor
            // (the HTTP twin read it from an optional bearer; here the
            // WS-RPC plane always carries the authed actor).
            match get_post_core(&state, Some(actor_id), post_id).await {
                GetPostOutcome::Found(bytes) => encode_reply(&PostGetReply {
                    body: serde_bytes::ByteBuf::from(bytes),
                    legal_takedown: None,
                    extra: std::collections::BTreeMap::new(),
                }),
                // Taken down under legal obligation: withhold the body (empty)
                // and carry the tombstone reference so the client renders
                // "removed under legal obligation [reference]" in its place
                // (moderation.md § Categories & enforcement item 1).
                GetPostOutcome::LegalTakedown { reference } => encode_reply(&PostGetReply {
                    body: serde_bytes::ByteBuf::new(),
                    legal_takedown: Some(fauna_protocol::posts::LegalTakedownMarker {
                        reference,
                        extra: std::collections::BTreeMap::new(),
                    }),
                    extra: std::collections::BTreeMap::new(),
                }),
                GetPostOutcome::NotFound => Err(not_found("post not found")),
                GetPostOutcome::Error => Err(internal("storage error")),
            }
        })
    })
}

// ── fauna.posts.room_labels ────────────────────────────────────

/// The verdicts a community room's named labelers derived for room-restricted
/// posts, read by a live floor member (`conversation-rooms.md` § The three
/// classes → *What the home nest does with its read*, purpose 3; the
/// mechanism: `room_post_view::index_room_post` writes them, `db::room_labels`
/// keys them `(room, post)` under the `room_post` kind).
///
/// **This is the one door a room post's verdicts leave by.** Every envelope
/// read — `fauna.posts.get`, its HTTP twin, the federation twin, the feed
/// pages — serves the sealed bytes to any follower and carries no verdict;
/// here the caller names posts, the nest finds the room that indexed each
/// from its own reception-pass map, and answers for a post only where the
/// caller is a live member of that room's floor
/// (`conversations_handlers::is_live_floor_member`, the `channel.fetch`
/// gate). A post the caller may not see verdicts for has no entry, exactly
/// like one nobody labelled.
fn posts_room_labels_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.posts.room_labels").await?;
            let req: PostRoomLabelsRequest = decode(&payload).map_err(malformed)?;
            let post_ids = parse_room_label_post_ids(&req.post_ids)?;
            encode_reply(&room_labels_for(&state, &actor_id, &post_ids, None).await?)
        })
    })
}

/// The `post_ids` of a verdict read, parsed and capped — the one ceiling both
/// doors apply, so a relayed batch can be no larger than a same-nest one.
fn parse_room_label_post_ids(ids: &[String]) -> Result<Vec<[u8; 32]>, RpcError> {
    if ids.len() > POST_ROOM_LABELS_MAX_IDS {
        return Err(invalid_params(&format!(
            "at most {POST_ROOM_LABELS_MAX_IDS} post ids per call"
        )));
    }
    ids.iter().map(|id| parse_post_id(id)).collect()
}

/// **The body both verdict doors run** — the same-nest `fauna.posts.room_labels`
/// and the room home's `fauna.federation.conversation.room_labels.fetch`,
/// whose reader is the member the relay names rather than the peer
/// (`federation_handlers::room_labels_fetch_handler`). One body, so a member
/// homed elsewhere reads exactly what a member homed here reads, and the two
/// doors cannot drift on who a verdict reaches — the shape
/// `conversations_handlers::page_verdicts` already holds for the *message*
/// plane's two doors.
///
/// `only_room` is the federation door's, and its whole content is *narrowing*:
/// `Some(room)` drops every post the map assigns to some other room, so one
/// relayed request — gated on exactly one room's foreign-member binding —
/// cannot fish across rooms. It never widens anything, because the floor check
/// below runs either way.
///
/// The gate stays the floor: `is_live_floor_member` per room, of any rank. A
/// post the caller's floor did not index, one no labeler labelled, one the
/// revoke's purge emptied, and every post of a room the caller is not a live
/// member of all simply have no entry — indistinguishable on purpose.
pub(crate) async fn room_labels_for(
    state: &Arc<AppState>,
    reader: &[u8; 32],
    post_ids: &[[u8; 32]],
    only_room: Option<[u8; 32]>,
) -> Result<PostRoomLabelsReply, RpcError> {
    // (room, post) as the map remembers it, grouped by room so the
    // gate runs once per room and the bus read is one range per room.
    let mut by_room: std::collections::BTreeMap<[u8; 32], Vec<[u8; 32]>> =
        std::collections::BTreeMap::new();
    for (room, post) in state
        .db
        .rooms_indexing_posts(post_ids)
        .await
        .map_err(|e| internal(format!("room post map: {e:#}")))?
    {
        if only_room.is_some_and(|only| only != room) {
            continue;
        }
        by_room.entry(room).or_default().push(post);
    }
    let mut found: std::collections::BTreeMap<[u8; 32], PostRoomLabelsEntry> =
        std::collections::BTreeMap::new();
    for (room, posts) in by_room {
        if !crate::conversations_handlers::is_live_floor_member(state, &room, reader).await {
            continue;
        }
        let bus = state
            .db
            .room_post_bus(&room, &posts)
            .await
            .map_err(|e| internal(format!("room post verdicts: {e:#}")))?;
        for (post, bus) in bus {
            let entry = found.entry(post).or_insert_with(|| PostRoomLabelsEntry {
                post_id: hex::encode(post),
                ..Default::default()
            });
            entry.labels.extend(bus.labels);
            entry.scores.extend(bus.scores);
        }
    }
    // In the caller's order, once per id however often it was asked.
    let mut seen = std::collections::BTreeSet::new();
    let posts = post_ids
        .iter()
        .filter(|id| seen.insert(**id))
        .filter_map(|id| found.remove(id))
        .collect();
    Ok(PostRoomLabelsReply {
        posts,
        extra: std::collections::BTreeMap::new(),
    })
}

/// [`room_labels_for`] as the room home's federation door calls it
/// (`federation_handlers::room_labels_fetch_handler`): the same parse and the
/// same ceiling the same-nest door applies, then the read narrowed to the one
/// room the relay's `require_foreign_member` gate just passed.
///
/// The ids arrive hex because the federation request carries the client's own,
/// forwarded unchanged — a relay that re-derived them could substitute one.
pub(crate) async fn room_labels_for_relay(
    state: &Arc<AppState>,
    reader: &[u8; 32],
    post_ids_hex: &[String],
    room: [u8; 32],
) -> Result<PostRoomLabelsReply, RpcError> {
    let post_ids = parse_room_label_post_ids(post_ids_hex)?;
    room_labels_for(state, reader, &post_ids, Some(room)).await
}

// ── fauna.posts.room_labels_remote ─────────────────────────────

/// A **foreign member's** room-post verdict read, relayed by its own home nest.
///
/// A room post's post → room map is written by the reception pass, which runs
/// only on the nest that stores the post and homes its room
/// (`room_post_view::index_room_post`) — so this nest, which is not that home,
/// resolves no room for the post and its same-nest door answers an empty
/// reply. Empty is how "nobody labelled" and "not yours to read" are
/// deliberately made indistinguishable there, so the member's card simply
/// rendered with no badge and nothing said why. "A member on a foreign nest
/// reaches the room only through their own home nest, which originates the leg
/// to the room's home" (`conversation-rooms.md` § The home nest); this is that
/// leg's client-facing half, the `room.list_roster_remote` /
/// `generations_remote` shape, for the last community-room read that lacked
/// one.
///
/// **This nest keeps nothing.** It forwards the room home's answer and stores
/// no part of it: a verdict is the room's, derived from plaintext this nest
/// never holds and served under the room home's own floor gate, and a relay
/// that cached one would be answering a membership question it has no
/// authority over the next time it was asked.
///
/// The room home resolves the requester's standing itself, from the actor id
/// this nest is authenticated for — it never takes this nest's word for who is
/// a member ([`crate::federation_handlers`]' `require_foreign_member`).
fn posts_room_labels_remote_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.posts.room_labels_remote").await?;
            let req: PostRoomLabelsRemoteRequest = decode(&payload).map_err(malformed)?;
            // Validate the shapes locally; the room's home nest is
            // authoritative for membership and for the verdicts themselves.
            let _ = parse_room_label_post_ids(&req.post_ids)?;
            parse_room_id(&req.room_id)?;
            let peer_url = req.nest_url.trim();
            if peer_url.is_empty() {
                return Err(invalid_params(
                    "nest_url must name the room's home nest (same-nest reads use posts.room_labels)",
                ));
            }

            match crate::federation_pool::originate_room_labels(
                &state.federation_pool,
                &state,
                peer_url,
                &hex::encode(actor_id),
                &req.room_id,
                &req.post_ids,
            )
            .await
            {
                Ok(Ok(reply)) => encode_reply(&reply),
                Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                    peer_err,
                    "the cross-nest room-post verdict read relay",
                )),
                Err(pool_err) => {
                    tracing::error!("federation room labels (room_labels_remote): {pool_err}");
                    Err(internal("federation room-post verdict read failed"))
                }
            }
        })
    })
}

// ── fauna.posts.list ───────────────────────────────────────────

/// Default page size when the request omits `limit`, and the ceiling any
/// larger request is clamped to. Hard-coded rather than configurable: no
/// human ever chooses a page size (`principles.md` — the one-configuration-
/// surface invariant's bucket 1). The ceiling exists so one call cannot ask
/// the nest to materialize an entire post history in a single reply.
const POSTS_LIST_DEFAULT_LIMIT: u32 = 50;
const POSTS_LIST_MAX_LIMIT: u32 = 200;

fn posts_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.posts.list").await?;
            let req: PostsListRequest = decode(&payload).map_err(malformed)?;

            // Both cursor halves or neither. A key half alone (the pre-keyset
            // client's shape, which left the wire with the compat-remnant
            // sweep) or a tiebreak half alone is refused rather than paged by
            // a made-up predicate.
            let cursor = match (req.cursor_created_at, req.cursor_post_id.as_deref()) {
                (Some(ts), Some(id_hex)) => Some((ts, parse_post_id(id_hex)?)),
                (None, None) => None,
                (Some(_), None) => {
                    return Err(invalid_params("cursor_created_at without cursor_post_id"));
                }
                (None, Some(_)) => {
                    return Err(invalid_params("cursor_post_id without cursor_created_at"));
                }
            };

            let limit = req
                .limit
                .unwrap_or(POSTS_LIST_DEFAULT_LIMIT)
                .clamp(1, POSTS_LIST_MAX_LIMIT);

            // Self-scoping is structural: the corpus is keyed on the
            // authenticated connection actor and the request has no field with
            // which to name another one.
            let rows = state
                .db
                .list_authored_posts_page(&actor_id, cursor, limit)
                .await
                .map_err(|e| internal(format!("list posts: {e:#}")))?;

            // A short page is the last page — the walk stops without paying
            // for an extra empty round-trip.
            let next = (rows.len() as u32 == limit)
                .then(|| rows.last())
                .flatten()
                .map(|last| (last.created_at, hex::encode(last.post_id)));

            encode_reply(&PostsListReply {
                posts: rows
                    .into_iter()
                    .map(|p| PostsListItem {
                        post_id: hex::encode(p.post_id),
                        created_at: p.created_at,
                        body: p.body,
                        extra: std::collections::BTreeMap::new(),
                    })
                    .collect(),
                cursor_created_at: next.as_ref().map(|(ts, _)| *ts),
                cursor_post_id: next.map(|(_, id)| id),
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.posts.interact ───────────────────────────────────────

fn posts_interact_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.posts.interact").await?;
            let req: PostInteractRequest = decode(&payload).map_err(malformed)?;
            let post_id = parse_post_id(&req.post_id)?;

            let outcome = crate::interact_routes::interact_with_post_core(
                &state,
                actor_id,
                post_id,
                &req.action,
            )
            .await
            .map_err(rpc_error_from_api)?;

            // Serialize the protocol-specific result JSON to a string —
            // round-trips the heterogeneous HTTP twin reply bodies
            // (`{"ok":true}`, `{action,target_post_id,source}`, bridged
            // protocol responses) byte-for-byte on the wire.
            let result = serde_json::to_string(&outcome.result)
                .map_err(|e| internal(format!("serialize interact result: {e}")))?;

            encode_reply(&PostInteractReply {
                action: req.action,
                source: outcome.source,
                result,
                // The target's counters after the act, when this nest can speak
                // for them — what lets a client move the tapped count without a
                // feed re-query (`ui/feed.md` § Interaction bar). `None` on a
                // bridged source and on `unrepost`; see `InteractOutcome`.
                counts: outcome.counts,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── Registration entry point ───────────────────────────────────

pub fn register_posts_handlers(b: &mut RpcRouterBuilder) {
    // Per-kind replay semantics + rationale: see
    // `KindRegistry::register_posts_kinds`.
    //
    // - get: pure read (replay-safe @5s).
    // - create: content-addressed post_id ⇒ put_post + replicate +
    //   bluesky write-through are idempotent on a byte-identical
    //   re-submit; replay-safe @30s (ingest verify + classify + insert +
    //   spawn fan-out).
    // - interact: forbids replay @5s — the `like` action increments a
    //   non-content-addressed score and inserts a notification, so a
    //   replay would double-count + re-notify.
    b.add(
        "fauna.posts.get",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: posts_get_handler(),
        },
    );
    // - room_labels: a pure read of a derived view (a community room's
    //   verdicts for room-restricted posts), floor-gated inside — the same
    //   class as `get` (replay-safe @5s).
    b.add(
        "fauna.posts.room_labels",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: posts_room_labels_handler(),
        },
    );
    // - room_labels_remote: the same read relayed to a room's home nest, for
    //   a member homed elsewhere. Pure read like the door it forwards to, so
    //   replay-safe; the longer deadline is the peer round trip (the
    //   `room.list_roster_remote` posture).
    b.add(
        "fauna.posts.room_labels_remote",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: posts_room_labels_remote_handler(),
        },
    );
    // - list: pure paged read, self-scoped by construction (replay-safe @5s).
    b.add(
        "fauna.posts.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: posts_list_handler(),
        },
    );
    b.add(
        "fauna.posts.create",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: posts_create_handler(),
        },
    );
    b.add(
        "fauna.posts.interact",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(5),
            handler: posts_interact_handler(),
        },
    );
    // delete: idempotent end state (already-gone is `deleted:false`, not an
    // error) ⇒ replay-safe @10s. Rationale: `KindRegistry::register_posts_kinds`.
    b.add(
        "fauna.posts.delete",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: posts_delete_handler(),
        },
    );
}
