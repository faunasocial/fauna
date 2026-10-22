//! The federated follow's subscribe twin, its state leg and its key-blob relay
//! — `fauna.federation.follow.{subscribe,unsubscribe,state,key_blob.fetch}`
//! (`docs/goal/behavior/federated-follow.md` § Decision 2 and § Decision 3's
//! `KeyBlob` paragraph).
//!
//! A reader on this nest follows an author homed on another: the client's
//! `fauna.subscriptions.subscribe` names the author's home and carries the
//! reader's own signed [`FollowIntent`]; this nest checks the intent is its
//! own user's and names itself, records a `remote_follows` row, and relays the
//! intent to the home **verbatim** — it never signs for its user. The home
//! verifies the intent's authoring envelope, binds its `home_nest_id` to the
//! channel's verified origin, applies the monotonic intent rule, and runs the
//! same subscribe body a local follower gets with the subscriber's home on the
//! request row. The request's fate comes back as a `follow.state` push through
//! the home's outbox, and the reader's `key_blob.get` for that author is
//! relayed to the home, which serves the roster-covering blob only to a
//! subscriber homed at the asking nest.
//!
//! Both halves live here: the follower-side arms the `subscription_handlers`
//! doors call, and the home-side channel handlers. Delivery of the author's
//! posts (§ Decision 1) is [`crate::follow_delivery`]'s, whose kind this
//! module's registrar enrols beside the twin's.

use std::sync::Arc;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

use fauna_core::data::{Capability, Timestamp};
use fauna_core::encoding::EmbedAsBytes;
use fauna_core::identity::ActorId;
use fauna_core::subscription::types::{FollowIntent, FollowIntentKind};
use fauna_protocol::subscriptions::{
    KeyBlobGetReply, SubscribeReply, SubscribeRequest, UnsubscribeReply, UnsubscribeRequest,
};
use fauna_protocol::{RpcError, decode_strict};

use crate::db::remote_follows::RemoteFollowState;
use crate::federation_router::{FederationRouterBuilder, RpcHandler, RpcKindMeta};
use crate::routes::AppState;
use crate::rpc_errors::{encode_reply, internal};
use crate::subscription_handlers::{SubscriberHome, malformed_upload};

/// `fauna.federation.follow.subscribe` — a follower's nest carries its user's
/// subscribe intent to the author's home.
pub const SUBSCRIBE_KIND: &str = "fauna.federation.follow.subscribe";
/// `fauna.federation.follow.unsubscribe` — the same for a leave.
pub const UNSUBSCRIBE_KIND: &str = "fauna.federation.follow.unsubscribe";
/// `fauna.federation.follow.state` — the home tells the follower's nest what
/// became of the request.
pub const STATE_KIND: &str = "fauna.federation.follow.state";
/// `fauna.federation.follow.key_blob.fetch` — the follower's nest reads the
/// author's current `KeyBlob` for its own subscriber.
pub const KEY_BLOB_FETCH_KIND: &str = "fauna.federation.follow.key_blob.fetch";

/// The most pending requests from remote subscribers one hosted author holds:
/// past it, a subscriber this nest has never seen is refused `rate_limited`
/// until the author drains the queue (`federated-follow.md` § Decision 2). The
/// per-author window (`bridge_rate_limit::FOLLOW_INTENT_TARGET_MAX`) bounds the
/// rate; this bounds what an author who never approves is left holding. A hard
/// constant, not a knob.
pub const MAX_REMOTE_PENDING_SUBSCRIBE_REQUESTS: usize = 500;

/// `outbox.entry_type` of a queued [`STATE_KIND`] push; the row's
/// `peer_nest_id` is the subscriber's home.
pub const ENTRY_TYPE_FOLLOW_STATE: &str = "follow_state";

// ── Wire ───────────────────────────────────────────────────────────────

/// The twin's request: the subscriber's intent split from its embed-as-bytes
/// wire exactly as `fauna.federation.post.forward` splits a post, so the home
/// verifies the very bytes the subscriber signed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedFollowIntentRequest {
    #[serde(with = "serde_bytes")]
    pub intent_bytes: Vec<u8>,
    /// 100-byte signed envelope (36-byte CID || 64-byte sig) of the intent.
    #[serde(with = "serde_bytes")]
    pub intent_envelope: Vec<u8>,
    /// The canonical embed-as-bytes `DeviceAuthorization` granting `Follow`,
    /// when a device rather than the identity signed the intent.
    #[serde(default, with = "serde_bytes", skip_serializing_if = "Option::is_none")]
    pub signer_auth: Option<Vec<u8>>,
    /// The subscriber's 1184-byte ML-KEM-768 encapsulation key, as the
    /// same-nest `subscribe` carries it. Ignored on an unsubscribe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mlkem_encaps_key: Option<ByteBuf>,
    /// The carrying nest's own URL as it declares it — a sighting only: the
    /// home binds the URL it stores to the channel's verified origin
    /// (`federation_handlers::resolve_origin_home_url`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_nest_url: Option<String>,
}

/// `fauna.federation.follow.state`'s request. `state` is one of
/// `approved` / `rejected` / `removed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FedFollowStateRequest {
    pub author: ActorId,
    pub subscriber: ActorId,
    pub tier: String,
    pub state: String,
    pub at: Timestamp,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FedFollowStateReply {}

/// `fauna.federation.follow.key_blob.fetch`'s request; the reply is the
/// client wire's [`KeyBlobGetReply`], relayed unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedFollowKeyBlobFetchRequest {
    pub author: ActorId,
    pub tier: String,
    pub subscriber: ActorId,
}

// ── Errors ─────────────────────────────────────────────────────────────

fn sub_forbidden(reason: &str) -> RpcError {
    crate::rpc_errors::forbidden_ns("subscriptions", reason)
}

fn fed_forbidden(reason: &str) -> RpcError {
    crate::rpc_errors::forbidden_ns("federation", reason)
}

use crate::subscription_handlers::{
    invalid_signature, key_blob_not_found, not_subscribed, tier_not_found,
};

/// A relay failure the reader's own nest owns: the home could not be reached.
fn relay_failed(what: &str, err: impl std::fmt::Display) -> RpcError {
    tracing::error!("federated follow: {what}: {err}");
    internal(format!("{what} failed"))
}

// ── The intent ─────────────────────────────────────────────────────────

/// Verify an intent's authoring envelope under `Capability::Follow` — the
/// subscriber's identity, or a device whose cert grants `Follow` — and the
/// future bound every signed `created_at` gets (a far-future stamp would
/// otherwise lock the subscriber out of every later act under the monotonic
/// rule). Both nests run it.
pub(crate) fn verify_intent(wire: &EmbedAsBytes) -> Result<FollowIntent, RpcError> {
    let (bytes, env) = wire
        .clone()
        .into_signed()
        .map_err(|_| malformed_upload("follow intent: invalid envelope"))?;
    let intent: FollowIntent = fauna_core::encoding::decode_signed_bytes(&bytes)
        .map_err(|_| malformed_upload("follow intent: does not decode"))?;
    fauna_core::encoding::verify_authoring_envelope(
        &intent,
        &bytes,
        &env,
        wire.signer_auth.as_deref(),
        &Capability::Follow,
        intent.created_at,
    )
    .map_err(|_| invalid_signature("follow intent signature does not verify"))?;
    if let Err(reason) = crate::storage::reject_future_created_at(intent.created_at) {
        return Err(malformed_upload(format!(
            "follow intent: {}",
            reason.as_snake_case()
        )));
    }
    Ok(intent)
}

fn intent_wire(req: &FedFollowIntentRequest) -> Result<EmbedAsBytes, RpcError> {
    if req.intent_envelope.len() != 100 {
        return Err(malformed_upload(
            "follow intent: invalid envelope (need 100 bytes)",
        ));
    }
    let signer_auth = match &req.signer_auth {
        Some(cert) => Some(Box::new(
            fauna_core::encoding::canonical_decode::<EmbedAsBytes>(cert)
                .map_err(|_| malformed_upload("follow intent: invalid signer_auth cert"))?,
        )),
        None => None,
    };
    Ok(EmbedAsBytes {
        envelope: req.intent_envelope.clone(),
        bytes: req.intent_bytes.clone(),
        signer_auth,
    })
}

fn split_intent(
    state: &AppState,
    wire: &EmbedAsBytes,
    mlkem_encaps_key: Option<ByteBuf>,
) -> Result<FedFollowIntentRequest, RpcError> {
    let signer_auth = wire
        .signer_auth
        .as_deref()
        .map(fauna_core::encoding::canonical_encode)
        .transpose()
        .map_err(internal)?;
    Ok(FedFollowIntentRequest {
        intent_bytes: wire.bytes.clone(),
        intent_envelope: wire.envelope.clone(),
        signer_auth,
        mlkem_encaps_key,
        origin_nest_url: state.handle_domain_if_set().map(|d| format!("https://{d}")),
    })
}

/// The follower's-nest check of an intent its own user hands it: genuine,
/// the bearer's, about this author, of this kind, and naming THIS nest as the
/// subscriber's home — a nest relays only an intent that names itself.
fn check_own_intent(
    state: &AppState,
    wire: &EmbedAsBytes,
    bearer: &[u8; 32],
    author: &[u8; 32],
    kind: FollowIntentKind,
) -> Result<FollowIntent, RpcError> {
    let intent = verify_intent(wire)?;
    if intent.subscriber.0 != *bearer {
        return Err(sub_forbidden("the follow intent is not the caller's"));
    }
    if intent.author.0 != *author || intent.kind != kind {
        return Err(malformed_upload(
            "follow intent: names another author or act than the request",
        ));
    }
    if intent.home_nest_id != state.nest_identity.public_key_bytes() {
        return Err(sub_forbidden(
            "the follow intent names another nest as the subscriber's home",
        ));
    }
    Ok(intent)
}

// ── Follower side: the doors' remote arms ──────────────────────────────

/// `fauna.subscriptions.subscribe`'s remote arm: the author is not hosted here
/// and the caller named the author's home. Relays the caller's intent and
/// returns the home's answer verbatim, recording what it implies.
pub(crate) async fn remote_subscribe(
    state: &Arc<AppState>,
    bearer: [u8; 32],
    req: &SubscribeRequest,
    home_url: &str,
) -> Result<SubscribeReply, RpcError> {
    let wire = req.intent.as_ref().ok_or_else(|| {
        malformed_upload(
            "a follow of an author on another nest carries the subscriber's signed intent",
        )
    })?;
    let intent = check_own_intent(
        state,
        wire,
        &bearer,
        &req.author_id.0,
        FollowIntentKind::Subscribe,
    )?;
    if intent.tier != req.tier {
        return Err(malformed_upload(
            "follow intent: names another tier than the request",
        ));
    }
    let home_url = home_url.trim().trim_end_matches('/');
    let home_nest_id = state
        .federation_pool
        .resolve_peer_nest_id(home_url)
        .await
        .map_err(|e| relay_failed("resolve the author's home nest", e))?;
    let fed_req = split_intent(state, wire, req.mlkem_encaps_key.clone())?;
    let reply = match crate::federation_pool::originate_follow_subscribe(
        &state.federation_pool,
        state,
        home_url,
        &home_nest_id,
        fed_req,
    )
    .await
    {
        Ok(Ok(reply)) => reply,
        Ok(Err(peer_err)) => {
            return Err(crate::rpc_errors::map_peer_relay_error(
                peer_err,
                "the federated follow",
            ));
        }
        Err(e) => return Err(relay_failed("relay the follow to its home nest", e)),
    };
    let (follow_state, request_id) = match &reply {
        SubscribeReply::Approved { .. } => (RemoteFollowState::Approved, None),
        SubscribeReply::Queued { request_id } => (RemoteFollowState::Pending, Some(*request_id)),
        SubscribeReply::Unknown => (RemoteFollowState::Pending, None),
    };
    state
        .db
        .upsert_remote_follow(
            &bearer,
            &req.author_id.0,
            &req.tier,
            &home_nest_id,
            home_url,
            follow_state,
            request_id,
            intent.created_at.as_i64(),
        )
        .await
        .map_err(internal)?;
    Ok(reply)
}

/// `fauna.subscriptions.unsubscribe`'s remote arm. The follow's state moves
/// only when the home's `follow.state` push says the leave committed.
pub(crate) async fn remote_unsubscribe(
    state: &Arc<AppState>,
    bearer: [u8; 32],
    req: &UnsubscribeRequest,
    home_url: &str,
) -> Result<UnsubscribeReply, RpcError> {
    let wire = req.intent.as_ref().ok_or_else(|| {
        malformed_upload(
            "an unsubscribe from an author on another nest carries the subscriber's signed intent",
        )
    })?;
    check_own_intent(
        state,
        wire,
        &bearer,
        &req.author_id.0,
        FollowIntentKind::Unsubscribe,
    )?;
    let home_url = home_url.trim().trim_end_matches('/');
    let home_nest_id = state
        .federation_pool
        .resolve_peer_nest_id(home_url)
        .await
        .map_err(|e| relay_failed("resolve the author's home nest", e))?;
    let fed_req = split_intent(state, wire, None)?;
    match crate::federation_pool::originate_follow_unsubscribe(
        &state.federation_pool,
        state,
        home_url,
        &home_nest_id,
        fed_req,
    )
    .await
    {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
            peer_err,
            "the federated unsubscribe",
        )),
        Err(e) => Err(relay_failed("relay the unsubscribe to its home nest", e)),
    }
}

/// `key_blob.get`'s remote arm: the caller holds an `approved` remote follow
/// of `author` at `tier` → the home's current blob, relayed; `Ok(None)` when
/// it holds none (the door answers `not_subscribed`). The blob is a roster, so
/// this nest relays it and keeps no copy.
pub(crate) async fn remote_key_blob(
    state: &Arc<AppState>,
    subscriber: &[u8; 32],
    author: &[u8; 32],
    tier: &str,
) -> Result<Option<KeyBlobGetReply>, RpcError> {
    let follows = state
        .db
        .remote_follows_of(subscriber, author)
        .await
        .map_err(internal)?;
    let Some(follow) = follows
        .into_iter()
        .find(|f| f.tier == tier && f.state == RemoteFollowState::Approved)
    else {
        return Ok(None);
    };
    let req = FedFollowKeyBlobFetchRequest {
        author: ActorId(*author),
        tier: tier.to_string(),
        subscriber: ActorId(*subscriber),
    };
    match crate::federation_pool::originate_follow_key_blob_fetch(
        &state.federation_pool,
        state,
        &follow.home_nest_url,
        &follow.home_nest_id,
        req,
    )
    .await
    {
        Ok(Ok(reply)) => Ok(Some(reply)),
        Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
            peer_err,
            "the federated key-blob read",
        )),
        Err(e) => Err(relay_failed("relay the key-blob read to its home nest", e)),
    }
}

// ── Home side: the producer of the state leg ───────────────────────────

/// Queue a `follow.state` push to a remote subscriber's home — the request's
/// fate (`approved` / `rejected`) or the end of the follow (`removed`).
/// Best-effort like every post-commit enqueue: the act itself has landed, and
/// the follower's view is eventually consistent, never ahead of the roster.
/// An approval also backfills the author's newest posts to that home, and a
/// removal drops the deliveries pending there once nobody homed there remains
/// (`federated-follow.md` § Decision 1).
pub(crate) async fn enqueue_follow_state(
    state: &AppState,
    author: &[u8; 32],
    subscriber: &[u8; 32],
    tier: &str,
    home_nest_id: &[u8; 32],
    follow_state: RemoteFollowState,
) {
    let push = FedFollowStateRequest {
        author: ActorId(*author),
        subscriber: ActorId(*subscriber),
        tier: tier.to_string(),
        state: follow_state.as_str().to_string(),
        at: Timestamp::now(),
    };
    let payload = match fauna_core::encoding::canonical_encode(&push) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("follow.state: encode push: {e}");
            return;
        }
    };
    if let Err(e) = state
        .db
        .outbox_enqueue_to_peer(author, &payload, ENTRY_TYPE_FOLLOW_STATE, home_nest_id)
        .await
    {
        tracing::error!(
            "follow.state: enqueue push ({}): {e}",
            follow_state.as_str()
        );
    }
    // Queued behind the push, so the follower's nest has recorded the
    // approval before the first delivery reaches its follow gate.
    match follow_state {
        RemoteFollowState::Approved => {
            crate::follow_delivery::backfill_new_home(state, author, home_nest_id).await
        }
        RemoteFollowState::Removed => {
            crate::follow_delivery::discard_if_home_left(state, author, home_nest_id).await
        }
        RemoteFollowState::Pending | RemoteFollowState::Rejected => {}
    }
}

// ── Home side: the channel handlers ────────────────────────────────────

fn decode<T: serde::de::DeserializeOwned>(payload: &Bytes) -> Result<T, RpcError> {
    decode_strict::<T>(payload).map_err(crate::rpc_errors::malformed)
}

/// The home's checks shared by the twin's two kinds, in § Decision 2's order:
/// the intent's envelope, its home bound to the verified origin, its author
/// hosted here. Returns the intent and the subscriber's home as the request
/// rows store it.
async fn admit_relayed_intent(
    state: &AppState,
    origin_nest_id: [u8; 32],
    req: &FedFollowIntentRequest,
    kind: FollowIntentKind,
) -> Result<(FollowIntent, Option<String>), RpcError> {
    let intent = verify_intent(&intent_wire(req)?)?;
    if intent.kind != kind {
        return Err(malformed_upload(
            "follow intent: names another act than the kind",
        ));
    }
    if intent.home_nest_id != origin_nest_id {
        return Err(fed_forbidden(
            "the follow intent names another nest as the subscriber's home",
        ));
    }
    if state
        .db
        .get_user(&intent.author.0)
        .await
        .map_err(internal)?
        .is_none()
    {
        return Err(tier_not_found("tier not found"));
    }
    let home_url = crate::federation_handlers::resolve_origin_home_url(
        state,
        &origin_nest_id,
        req.origin_nest_url.as_deref(),
    )
    .await?;
    Ok((intent, home_url))
}

/// The author's per-target bound on intents from subscribers this nest has
/// never seen (§ Decision 2): any fresh keypair passes the hello and any fresh
/// subscriber key signs, so a bound on the origin bounds nothing. A stranger's
/// intent — a subscribe or a leave alike, each writes a seen-stamp row — spends
/// the author's window budget, and a stranger's subscribe is refused while the
/// author already holds [`MAX_REMOTE_PENDING_SUBSCRIBE_REQUESTS`] remote
/// requests. A subscriber already holding a pending or approved row for the
/// author is free of the cap, since it holds at most one pending request per
/// tier; "seen" is not that (a leave that ended nothing stamps too), so a
/// seen subscriber with no row meets the cap, and only the budget is skipped
/// for it (it spent that on first sight).
async fn admit_stranger_intent(
    state: &AppState,
    author: &[u8; 32],
    subscriber: &[u8; 32],
    kind: FollowIntentKind,
) -> Result<(), RpcError> {
    if kind == FollowIntentKind::Subscribe
        && !state
            .db
            .holds_follow_row(author, subscriber)
            .await
            .map_err(internal)?
        && state
            .db
            .count_remote_pending_subscribe_requests(author)
            .await
            .map_err(internal)?
            >= MAX_REMOTE_PENDING_SUBSCRIBE_REQUESTS
    {
        return Err(crate::rpc_errors::rate_limited());
    }
    if state
        .db
        .follow_intent_seen(author, subscriber)
        .await
        .map_err(internal)?
    {
        return Ok(());
    }
    if !state.federation_target_budget.check_with_max(
        author,
        &[0u8; 32],
        "follow_intent",
        crate::bridge_rate_limit::FOLLOW_INTENT_TARGET_MAX,
    ) {
        return Err(crate::rpc_errors::rate_limited());
    }
    Ok(())
}

fn subscribe_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedFollowIntentRequest = decode(&payload)?;
            let (intent, home_url) =
                admit_relayed_intent(&state, origin_nest_id, &req, FollowIntentKind::Subscribe)
                    .await?;
            let (author, subscriber) = (intent.author.0, intent.subscriber.0);
            admit_stranger_intent(&state, &author, &subscriber, FollowIntentKind::Subscribe)
                .await?;
            let home = SubscriberHome {
                nest_id: &origin_nest_id,
                url: home_url.as_deref(),
            };
            let local_req = SubscribeRequest {
                author_id: intent.author,
                tier: intent.tier.clone(),
                mlkem_encaps_key: req.mlkem_encaps_key.clone(),
                home_nest_url: None,
                intent: None,
                extra: Default::default(),
            };
            if !state
                .db
                .admit_follow_intent(
                    &author,
                    &subscriber,
                    &intent.tier,
                    intent.created_at.as_i64(),
                )
                .await
                .map_err(internal)?
            {
                // An older intent changes nothing: answer from the state as it
                // stands, never by acting on the stale word.
                let reply = crate::subscription_handlers::subscribe_state(
                    &state,
                    &author,
                    &subscriber,
                    &intent.tier,
                )
                .await?
                .ok_or_else(|| sub_forbidden("a newer follow intent supersedes this one"))?;
                return encode_reply(&reply);
            }
            let reply = crate::subscription_handlers::subscribe_core_from(
                &state,
                subscriber,
                &local_req,
                Some(home),
            )
            .await?;
            encode_reply(&reply)
        })
    })
}

fn unsubscribe_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedFollowIntentRequest = decode(&payload)?;
            let (intent, home_url) =
                admit_relayed_intent(&state, origin_nest_id, &req, FollowIntentKind::Unsubscribe)
                    .await?;
            let (author, subscriber) = (intent.author.0, intent.subscriber.0);
            admit_stranger_intent(&state, &author, &subscriber, FollowIntentKind::Unsubscribe)
                .await?;
            let at = intent.created_at.as_i64();
            // A leave covers several tiers, so it is admitted only when it is
            // newer than every intent seen from this subscriber for this
            // author — and stamps each tier it leaves, so no earlier subscribe
            // replayed afterwards can undo it.
            if !state
                .db
                .admit_follow_intent_over_author(&author, &subscriber, &intent.tier, at)
                .await
                .map_err(internal)?
            {
                let reply =
                    crate::subscription_handlers::unsubscribe_state(&state, &author, &subscriber)
                        .await?
                        .ok_or_else(|| {
                            not_subscribed("a newer follow intent supersedes this one")
                        })?;
                return encode_reply(&reply);
            }
            let home = SubscriberHome {
                nest_id: &origin_nest_id,
                url: home_url.as_deref(),
            };
            let (reply, tiers) = crate::subscription_handlers::unsubscribe_core(
                &state,
                &author,
                &subscriber,
                Some(home),
            )
            .await?;
            for tier in tiers {
                state
                    .db
                    .admit_follow_intent(&author, &subscriber, &tier, at)
                    .await
                    .map_err(internal)?;
            }
            encode_reply(&reply)
        })
    })
}

/// Receive side, on the follower's nest: the home says what became of a
/// follow. Accepted only for a row whose home is the verified origin — a nest
/// that is nobody's home for this follow moves nothing.
fn state_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedFollowStateRequest = decode(&payload)?;
            let follow_state = match RemoteFollowState::parse(&req.state) {
                Some(
                    s @ (RemoteFollowState::Approved
                    | RemoteFollowState::Rejected
                    | RemoteFollowState::Removed),
                ) => s,
                _ => {
                    return Err(crate::rpc_errors::malformed(format!(
                        "unknown follow state {:?}",
                        req.state
                    )));
                }
            };
            let moved = state
                .db
                .set_remote_follow_state_from(
                    &req.subscriber.0,
                    &req.author.0,
                    &req.tier,
                    &origin_nest_id,
                    follow_state,
                )
                .await
                .map_err(internal)?;
            if !moved {
                return Err(fed_forbidden(
                    "no follow of this author is homed at this nest",
                ));
            }
            encode_reply(&FedFollowStateReply {})
        })
    })
}

/// The home serves its author's current blob to the follower's nest for the
/// one subscriber it names — iff that subscriber is on the roster AND the
/// roster row's home is the asking nest (the twin's structural binding).
fn key_blob_fetch_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedFollowKeyBlobFetchRequest = decode(&payload)?;
            let (author, subscriber) = (req.author.0, req.subscriber.0);
            let entitled = state
                .db
                .is_subscriber(&author, &subscriber, &req.tier)
                .await
                .map_err(internal)?
                && state
                    .db
                    .subscriber_home(&author, &subscriber, &req.tier)
                    .await
                    .map_err(internal)?
                    == Some(origin_nest_id);
            if !entitled {
                return Err(not_subscribed("not subscribed to this tier from this nest"));
            }
            let (version, hash, blob_data) = state
                .db
                .get_current_key_blob(&author, &req.tier)
                .await
                .map_err(internal)?
                .ok_or_else(|| key_blob_not_found("no key blob for this tier"))?;
            encode_reply(&KeyBlobGetReply {
                version: version as u64,
                blob_hash: ByteBuf::from(hash),
                blob_data: ByteBuf::from(blob_data),
                extra: Default::default(),
            })
        })
    })
}

pub fn register_follow_federation_handlers(b: &mut FederationRouterBuilder) {
    for (kind, handler) in [
        (SUBSCRIBE_KIND, subscribe_handler()),
        (UNSUBSCRIBE_KIND, unsubscribe_handler()),
        (STATE_KIND, state_handler()),
        (KEY_BLOB_FETCH_KIND, key_blob_fetch_handler()),
        (
            crate::follow_delivery::DELIVER_KIND,
            crate::follow_delivery::deliver_handler(),
        ),
    ] {
        b.add(
            kind,
            RpcKindMeta {
                forbid_replay: false,
                default_deadline: std::time::Duration::from_secs(30),
                handler,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use fauna_core::identity::ActorKeypair;
    use fauna_core::subscription::FOLLOWERS_TIER;
    use fauna_protocol::encode_canonical;

    use crate::test_support::fixture_state;

    const HOME_A: [u8; 32] = [0xA1; 32];
    const STRANGER_NEST: [u8; 32] = [0xB2; 32];

    fn intent(
        subscriber: &ActorKeypair,
        author: [u8; 32],
        tier: &str,
        kind: FollowIntentKind,
        home: [u8; 32],
        at: u64,
    ) -> EmbedAsBytes {
        let intent = FollowIntent {
            subscriber: subscriber.actor_id(),
            author: ActorId(author),
            tier: tier.to_string(),
            kind,
            home_nest_id: home,
            created_at: Timestamp(at),
        };
        let (bytes, env) = fauna_core::encoding::sign_envelope(subscriber, &intent).unwrap();
        EmbedAsBytes::from_signed(bytes, env)
    }

    fn twin_payload(wire: &EmbedAsBytes) -> Bytes {
        let req = FedFollowIntentRequest {
            intent_bytes: wire.bytes.clone(),
            intent_envelope: wire.envelope.clone(),
            signer_auth: None,
            mlkem_encaps_key: None,
            origin_nest_url: Some("https://a.example".into()),
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    }

    /// A moment `n` seconds into the past, so no stamp trips the future bound.
    fn ago(n: u64) -> u64 {
        Timestamp::now().0 - n * 1_000_000
    }

    async fn hosted_author(state: &Arc<AppState>) -> [u8; 32] {
        let author = ActorKeypair::generate().actor_id().0;
        state
            .db
            .create_user(&author, "free", "author")
            .await
            .unwrap();
        author
    }

    /// § Decision 2's structural half: the home binds the intent's
    /// `home_nest_id` to the channel's verified origin, so a nest cannot put a
    /// subscriber homed elsewhere on an author's roster.
    #[tokio::test]
    async fn an_intent_naming_another_nest_as_home_is_forbidden_at_the_twin() {
        let state = fixture_state();
        let author = hosted_author(&state).await;
        let reader = ActorKeypair::generate();
        let wire = intent(
            &reader,
            author,
            FOLLOWERS_TIER,
            FollowIntentKind::Subscribe,
            HOME_A,
            ago(5),
        );

        let err = subscribe_handler()(state.clone(), STRANGER_NEST, twin_payload(&wire))
            .await
            .expect_err("a nest carrying an intent that names another home is refused");
        assert_eq!(err.code, "fauna.federation.forbidden");
        assert!(
            state
                .db
                .list_subscribe_requests(&author)
                .await
                .unwrap()
                .is_empty(),
            "nothing reaches the roster's queue"
        );

        // The nest the intent names is admitted, and the request carries it.
        let ok = subscribe_handler()(state.clone(), HOME_A, twin_payload(&wire))
            .await
            .expect("the intent's own home carries it");
        let reply: SubscribeReply = decode_strict(&ok).unwrap();
        let SubscribeReply::Queued { request_id } = reply else {
            panic!("a first follow is queued for the author's client: {reply:?}");
        };
        assert_eq!(
            state.db.subscribe_request_home(request_id).await.unwrap(),
            Some(HOME_A)
        );
    }

    /// A tampered intent — another subscriber's name over this signer's
    /// signature — never verifies.
    #[tokio::test]
    async fn an_intent_signed_by_someone_else_is_refused() {
        let state = fixture_state();
        let author = hosted_author(&state).await;
        let reader = ActorKeypair::generate();
        let forger = ActorKeypair::generate();
        let intent_value = FollowIntent {
            subscriber: reader.actor_id(),
            author: ActorId(author),
            tier: FOLLOWERS_TIER.into(),
            kind: FollowIntentKind::Subscribe,
            home_nest_id: HOME_A,
            created_at: Timestamp(ago(5)),
        };
        let (bytes, _) = fauna_core::encoding::sign_envelope(&reader, &intent_value).unwrap();
        let (_, forged_env) = fauna_core::encoding::sign_envelope(&forger, &intent_value).unwrap();
        let wire = EmbedAsBytes::from_signed(bytes, forged_env);
        let err = subscribe_handler()(state.clone(), HOME_A, twin_payload(&wire))
            .await
            .expect_err("a signature that is not the subscriber's is refused");
        assert_eq!(err.code, "fauna.subscriptions.invalid_signature");
    }

    /// The monotonic rule: once a leave newer than a subscribe intent has been
    /// acted on, replaying that older subscribe changes nothing — while a
    /// genuinely newer subscribe is still admitted.
    #[tokio::test]
    async fn an_older_intent_after_a_newer_unsubscribe_changes_nothing() {
        let state = fixture_state();
        let author = hosted_author(&state).await;
        let reader = ActorKeypair::generate();
        let subscriber = reader.actor_id().0;
        state
            .db
            .create_subscription_tier(
                &author, "gold", 1, None, None, None, false, None, None, false,
            )
            .await
            .unwrap();

        let old_subscribe = intent(
            &reader,
            author,
            "gold",
            FollowIntentKind::Subscribe,
            HOME_A,
            ago(60),
        );
        subscribe_handler()(state.clone(), HOME_A, twin_payload(&old_subscribe))
            .await
            .expect("the first subscribe is queued");
        // The author approved it; the subscriber then left, newer.
        let queued = state.db.list_subscribe_requests(&author).await.unwrap();
        state
            .db
            .add_subscriber(&author, &subscriber, "gold", None)
            .await
            .unwrap();
        state
            .db
            .delete_subscribe_request(queued[0].id)
            .await
            .unwrap();
        let leave = intent(
            &reader,
            author,
            "gold",
            FollowIntentKind::Unsubscribe,
            HOME_A,
            ago(30),
        );
        unsubscribe_handler()(state.clone(), HOME_A, twin_payload(&leave))
            .await
            .expect("the leave is queued");
        // The author's client commits the leave.
        let leave_row = state.db.list_subscribe_requests(&author).await.unwrap();
        assert_eq!(leave_row.len(), 1);
        assert_eq!(
            leave_row[0].home_nest_url.as_deref(),
            Some("https://a.example")
        );
        state
            .db
            .remove_subscriber(&author, &subscriber, "gold")
            .await
            .unwrap();
        state
            .db
            .delete_subscribe_request(leave_row[0].id)
            .await
            .unwrap();

        // The stale subscribe, replayed, writes nothing.
        let err = subscribe_handler()(state.clone(), HOME_A, twin_payload(&old_subscribe))
            .await
            .expect_err("an intent older than the leave is not acted on");
        assert_eq!(err.code, "fauna.subscriptions.forbidden");
        assert!(
            state
                .db
                .list_subscribe_requests(&author)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            !state
                .db
                .is_subscriber(&author, &subscriber, "gold")
                .await
                .unwrap()
        );

        // A newer subscribe is the subscriber's real word, and is queued.
        let fresh = intent(
            &reader,
            author,
            "gold",
            FollowIntentKind::Subscribe,
            HOME_A,
            ago(1),
        );
        let ok = subscribe_handler()(state.clone(), HOME_A, twin_payload(&fresh))
            .await
            .expect("a newer subscribe is admitted");
        assert!(matches!(
            decode_strict::<SubscribeReply>(&ok).unwrap(),
            SubscribeReply::Queued { .. }
        ));
    }

    /// The key-blob relay's home half: the blob goes to the asking nest only
    /// for a subscriber on the roster whose roster row names that nest.
    #[tokio::test]
    async fn key_blob_fetch_serves_only_the_subscribers_own_home() {
        let state = fixture_state();
        let author = hosted_author(&state).await;
        let subscriber = ActorKeypair::generate().actor_id().0;
        state
            .db
            .create_subscription_tier(
                &author, "gold", 1, None, None, None, false, None, None, false,
            )
            .await
            .unwrap();
        let request_id = state
            .db
            .insert_subscribe_request(&author, &subscriber, "gold", "subscribe", None)
            .await
            .unwrap();
        state
            .db
            .set_subscribe_request_home(request_id, &HOME_A, Some("https://a.example"))
            .await
            .unwrap();
        state
            .db
            .add_subscriber(&author, &subscriber, "gold", None)
            .await
            .unwrap();
        state
            .db
            .copy_subscribe_request_home(request_id, &author, &subscriber, "gold")
            .await
            .unwrap();
        state
            .db
            .upsert_current_key_blob(&author, "gold", 1, &[7u8; 32], b"blob")
            .await
            .unwrap();

        let fetch = |origin: [u8; 32]| {
            let req = FedFollowKeyBlobFetchRequest {
                author: ActorId(author),
                tier: "gold".into(),
                subscriber: ActorId(subscriber),
            };
            key_blob_fetch_handler()(
                state.clone(),
                origin,
                Bytes::from(encode_canonical(&req).unwrap().to_vec()),
            )
        };
        let err = fetch(STRANGER_NEST)
            .await
            .expect_err("a nest the subscriber is not homed at reads nothing");
        assert_eq!(err.code, "fauna.subscriptions.not_subscribed");
        let ok = fetch(HOME_A)
            .await
            .expect("the subscriber's own home reads it");
        let reply: KeyBlobGetReply = decode_strict(&ok).unwrap();
        assert_eq!(reply.blob_data.as_ref(), b"blob");
    }

    /// The state leg's receive side moves a follow only on its home's word.
    #[tokio::test]
    async fn a_state_push_moves_a_follow_only_from_its_home() {
        let state = fixture_state();
        let reader = ActorKeypair::generate().actor_id().0;
        let author = [0x33; 32];
        state
            .db
            .upsert_remote_follow(
                &reader,
                &author,
                FOLLOWERS_TIER,
                &HOME_A,
                "https://a.example",
                RemoteFollowState::Pending,
                Some(9),
                ago(5) as i64,
            )
            .await
            .unwrap();
        let push = |origin: [u8; 32]| {
            let req = FedFollowStateRequest {
                author: ActorId(author),
                subscriber: ActorId(reader),
                tier: FOLLOWERS_TIER.into(),
                state: "approved".into(),
                at: Timestamp::now(),
            };
            state_handler()(
                state.clone(),
                origin,
                Bytes::from(encode_canonical(&req).unwrap().to_vec()),
            )
        };
        let err = push(STRANGER_NEST)
            .await
            .expect_err("a stranger moves nothing");
        assert_eq!(err.code, "fauna.federation.forbidden");
        assert_eq!(
            state.db.remote_follows_of(&reader, &author).await.unwrap()[0].state,
            RemoteFollowState::Pending
        );
        push(HOME_A).await.expect("the home's push lands");
        assert_eq!(
            state.db.remote_follows_of(&reader, &author).await.unwrap()[0].state,
            RemoteFollowState::Approved
        );
    }
}
