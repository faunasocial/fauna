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
use fauna_core::subscription::FOLLOWERS_TIER;
#[cfg(feature = "payments")]
use fauna_core::subscription::types::ClaimRedeemIntent;
use fauna_core::subscription::types::{FollowIntent, FollowIntentKind};
#[cfg(feature = "payments")]
use fauna_protocol::payments::{ClaimRedeemReply, ClaimRedeemRequest};
use fauna_protocol::subscriptions::{
    KeyBlobGetReply, SubscribeReply, SubscribeRequest, TiersListReply, UnsubscribeReply,
    UnsubscribeRequest,
};
use fauna_protocol::{RpcError, decode_strict};

use crate::db::remote_follows::{RemoteFollowState, RemoteLeave};
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
/// `fauna.federation.claims.redeem` — a buyer's nest carries its user's claim
/// redeem intent to the code's home (`federated-follow.md` § Decision 2 →
/// *The redeem twin*). The request is the follow twin's
/// [`FedFollowIntentRequest`] shape; the answer is the local door's
/// [`ClaimRedeemReply`].
pub const CLAIM_REDEEM_KIND: &str = "fauna.federation.claims.redeem";
/// `fauna.federation.follow.offers.list` — a reader's nest reads an author's
/// offered tiers from the author's home (`subscriptions.md` § Pillar 1 → *The
/// offers browse across nests*). The answer is the local door's
/// [`TiersListReply`], through the one shared offered-tiers function.
pub const OFFERS_LIST_KIND: &str = "fauna.federation.follow.offers.list";

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

/// `fauna.federation.follow.offers.list`'s request; the reply is the client
/// wire's [`TiersListReply`], relayed unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedOffersListRequest {
    pub author_id: ActorId,
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
    verify_signed_intent(wire, "follow intent", |i: &FollowIntent| i.created_at)
}

/// [`verify_intent`] for the redeem twin's [`ClaimRedeemIntent`] — the same
/// envelope, capability and future bound (`federated-follow.md` § Decision 2
/// → *The redeem twin*).
#[cfg(feature = "payments")]
pub(crate) fn verify_redeem_intent(wire: &EmbedAsBytes) -> Result<ClaimRedeemIntent, RpcError> {
    verify_signed_intent(wire, "claim redeem intent", |i: &ClaimRedeemIntent| {
        i.created_at
    })
}

fn verify_signed_intent<T: fauna_core::encoding::Signed>(
    wire: &EmbedAsBytes,
    what: &str,
    created_at: impl Fn(&T) -> Timestamp,
) -> Result<T, RpcError> {
    let (bytes, env) = wire
        .clone()
        .into_signed()
        .map_err(|_| malformed_upload(format!("{what}: invalid envelope")))?;
    let intent: T = fauna_core::encoding::decode_signed_bytes(&bytes)
        .map_err(|_| malformed_upload(format!("{what}: does not decode")))?;
    let at = created_at(&intent);
    fauna_core::encoding::verify_authoring_envelope(
        &intent,
        &bytes,
        &env,
        wire.signer_auth.as_deref(),
        &Capability::Follow,
        at,
    )
    .map_err(|_| invalid_signature(&format!("{what} signature does not verify")))?;
    if let Err(reason) = crate::storage::reject_future_created_at(at) {
        return Err(malformed_upload(format!(
            "{what}: {}",
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

/// The buyer's-nest check of a redeem intent its own user hands it: genuine,
/// the bearer's, for this code, and naming THIS nest as the redeemer's home.
#[cfg(feature = "payments")]
pub(crate) fn check_own_redeem_intent(
    state: &AppState,
    wire: &EmbedAsBytes,
    bearer: &[u8; 32],
    code: &str,
) -> Result<ClaimRedeemIntent, RpcError> {
    let intent = verify_redeem_intent(wire)?;
    if intent.redeemer.0 != *bearer {
        return Err(sub_forbidden("the claim redeem intent is not the caller's"));
    }
    if intent.code != code {
        return Err(malformed_upload(
            "claim redeem intent: names another code than the request",
        ));
    }
    if intent.home_nest_id != state.nest_identity.public_key_bytes() {
        return Err(sub_forbidden(
            "the claim redeem intent names another nest as the redeemer's home",
        ));
    }
    Ok(intent)
}

/// `fauna.payments.claims.redeem`'s remote arm (`monetization.md` § Pillar 3 →
/// *Which nest an in-app redeem binds on*): the code's author is not hosted
/// here and the caller named the author's home. Relays the caller's own
/// checked intent and returns the home's answer — or its typed refusal —
/// verbatim, recording the subscription it implies as a remote follow, so the
/// state leg, the key-blob relay, delivery and the leave apply unchanged.
#[cfg(feature = "payments")]
pub(crate) async fn remote_redeem(
    state: &Arc<AppState>,
    bearer: [u8; 32],
    req: &ClaimRedeemRequest,
    intent: &ClaimRedeemIntent,
    home_url: &str,
) -> Result<ClaimRedeemReply, RpcError> {
    let wire = req.intent.as_ref().ok_or_else(|| {
        malformed_upload("a redeem on another nest carries the redeemer's signed intent")
    })?;
    let home_url = home_url.trim().trim_end_matches('/');
    let home_nest_id = state
        .federation_pool
        .resolve_peer_nest_id(home_url)
        .await
        .map_err(|e| relay_failed("resolve the author's home nest", e))?;
    let fed_req = split_intent(state, wire, None)?;
    let reply = match crate::federation_pool::originate_claim_redeem(
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
                "the cross-nest claim redeem",
            ));
        }
        Err(e) => return Err(relay_failed("relay the redeem to its home nest", e)),
    };
    let follow_state = if reply.queued {
        RemoteFollowState::Pending
    } else {
        RemoteFollowState::Approved
    };
    state
        .db
        .upsert_remote_follow(
            &bearer,
            &intent.author.0,
            &reply.tier,
            &home_nest_id,
            home_url,
            follow_state,
            None,
            intent.created_at.as_i64(),
        )
        .await
        .map_err(internal)?;
    Ok(reply)
}

/// `fauna.subscriptions.unsubscribe`'s remote arm. The leave is relayed to the
/// home stored on the follow, never to a URL the caller names, and the reader's
/// own rows the leave names move to `removed` once the home has answered it or
/// could not be reached — the leave is the reader's act over the reader's own
/// data, so an unreachable home holds nothing back — but never when the home
/// refuses it, so a refused leave leaves no half-state here
/// (`federated-follow.md` § Decision 2, the state leg). A row still `pending`
/// moves too: the home withdraws the waiting request (`subscriptions.md`
/// § The free followers tier — follow and unfollow, rule 4).
pub(crate) async fn remote_unsubscribe(
    state: &Arc<AppState>,
    bearer: [u8; 32],
    req: &UnsubscribeRequest,
) -> Result<UnsubscribeReply, RpcError> {
    let wire = req.intent.as_ref().ok_or_else(|| {
        malformed_upload(
            "an unsubscribe from an author on another nest carries the subscriber's signed intent",
        )
    })?;
    let intent = check_own_intent(
        state,
        wire,
        &bearer,
        &req.author_id.0,
        FollowIntentKind::Unsubscribe,
    )?;
    // The home the follow is stored at: the newest paid tier's row (what a
    // leave ends), else any row of the follow.
    let rows = state
        .db
        .remote_follows_of(&bearer, &req.author_id.0)
        .await
        .map_err(internal)?;
    let (home_nest_id, home_url) = rows
        .iter()
        .filter(|r| r.tier != FOLLOWERS_TIER)
        .max_by_key(|r| r.updated_at)
        .or_else(|| rows.iter().max_by_key(|r| r.updated_at))
        .map(|r| (r.home_nest_id, r.home_nest_url.clone()))
        .ok_or_else(|| {
            crate::subscription_handlers::not_subscribed(
                "no follow of this author is recorded on this nest",
            )
        })?;
    // Marked by name, from the signed intent's tier (`subscriptions.md` § The
    // free followers tier — follow and unfollow, rule 2): the unfollow marks
    // every row, a paid tier its own; the home's `removed` pushes mark the
    // rest of the at-or-above set as the author's client commits each tier.
    let leave = if intent.tier == FOLLOWERS_TIER {
        RemoteLeave::All
    } else {
        RemoteLeave::Only(&intent.tier)
    };
    let fed_req = split_intent(state, wire, None)?;
    let relayed = crate::federation_pool::originate_follow_unsubscribe(
        &state.federation_pool,
        state,
        &home_url,
        &home_nest_id,
        fed_req,
    )
    .await;
    let mark = || async {
        state
            .db
            .leave_remote_follow(&bearer, &req.author_id.0, leave)
            .await
            .map_err(internal)
    };
    match relayed {
        Ok(Ok(reply)) => {
            mark().await?;
            Ok(reply)
        }
        // The home refused: nothing moved there, so nothing moves here.
        Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
            peer_err,
            "the federated unsubscribe",
        )),
        Err(e) => {
            mark().await?;
            Err(relay_failed("relay the unsubscribe to its home nest", e))
        }
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

/// `offers.list_remote`'s relay arm: an author this nest does not host is
/// read from `home_url` — the peer resolved and pinned as the subscribe twin
/// resolves it — and the home's [`TiersListReply`] forwarded unstored (no
/// cache: tier definitions change at the author's will). A home predating the
/// kind refuses it at its router, which [`map_peer_relay_error`] re-speaks as
/// `fauna.federation.peer_nest_outdated`.
///
/// [`map_peer_relay_error`]: crate::rpc_errors::map_peer_relay_error
pub(crate) async fn remote_offers_list(
    state: &Arc<AppState>,
    author: &[u8; 32],
    home_url: &str,
) -> Result<TiersListReply, RpcError> {
    let home_url = home_url.trim().trim_end_matches('/');
    let home_nest_id = state
        .federation_pool
        .resolve_peer_nest_id(home_url)
        .await
        .map_err(|e| relay_failed("resolve the author's home nest", e))?;
    let req = FedOffersListRequest {
        author_id: ActorId(*author),
    };
    match crate::federation_pool::originate_follow_offers_list(
        &state.federation_pool,
        state,
        home_url,
        &home_nest_id,
        req,
    )
    .await
    {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
            peer_err,
            "the cross-nest offers browse",
        )),
        Err(e) => Err(relay_failed("relay the offers browse to its home nest", e)),
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
            crate::follow_delivery::backfill_new_home(state, author, home_nest_id, tier).await
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
            // The intent's tier is the leave's floor (`subscriptions.md`
            // § The free followers tier — follow and unfollow, rule 2).
            let (reply, tiers) = crate::subscription_handlers::unsubscribe_core(
                &state,
                &author,
                &subscriber,
                Some(home),
                Some(&intent.tier),
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

/// The redeem twin on the code's home (`federated-follow.md` § Decision 2 →
/// *The redeem twin*): the follow twin's admission — the envelope, the home
/// bound to the verified origin, the author hosted here (else
/// `claim_not_found`, so the kind tells a guesser nothing a wrong code would
/// not) — then the author's stranger budget, spent by EVERY relayed redeem
/// (a miss answers whether a code exists), and the pending cap for a redeemer
/// holding no row; then the redeem for the remote redeemer with their home.
#[cfg(feature = "payments")]
fn claim_redeem_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedFollowIntentRequest = decode(&payload)?;
            let intent = verify_redeem_intent(&intent_wire(&req)?)?;
            if intent.home_nest_id != origin_nest_id {
                return Err(fed_forbidden(
                    "the claim redeem intent names another nest as the redeemer's home",
                ));
            }
            let (author, redeemer) = (intent.author.0, intent.redeemer.0);
            if state
                .db
                .get_user(&author)
                .await
                .map_err(internal)?
                .is_none()
            {
                return Err(crate::payment_handlers::claim_not_found());
            }
            if !state
                .db
                .holds_follow_row(&author, &redeemer)
                .await
                .map_err(internal)?
                && state
                    .db
                    .count_remote_pending_subscribe_requests(&author)
                    .await
                    .map_err(internal)?
                    >= MAX_REMOTE_PENDING_SUBSCRIBE_REQUESTS
            {
                return Err(crate::rpc_errors::rate_limited());
            }
            if !state.federation_target_budget.check_with_max(
                &author,
                &[0u8; 32],
                "follow_intent",
                crate::bridge_rate_limit::FOLLOW_INTENT_TARGET_MAX,
            ) {
                return Err(crate::rpc_errors::rate_limited());
            }
            let home_url = crate::federation_handlers::resolve_origin_home_url(
                &state,
                &origin_nest_id,
                req.origin_nest_url.as_deref(),
            )
            .await?;
            // The code names the tier; a code of another author is unknown here.
            let code = intent.code.trim();
            let claim = state
                .db
                .get_payment_claim(code)
                .await
                .map_err(internal)?
                .filter(|c| c.author_id.as_slice() == author.as_slice())
                .ok_or_else(crate::payment_handlers::claim_not_found)?;
            // Stamped once the code has named the tier, so a later leave orders
            // after it — and an older redeem replayed after a leave changes
            // nothing (the follow twin's monotonic rule).
            if !state
                .db
                .admit_follow_intent(
                    &author,
                    &redeemer,
                    &claim.tier_name,
                    intent.created_at.as_i64(),
                )
                .await
                .map_err(internal)?
            {
                return Err(fed_forbidden("a newer intent supersedes this redeem"));
            }
            let home = SubscriberHome {
                nest_id: &origin_nest_id,
                url: home_url.as_deref(),
            };
            let redeemed =
                crate::payment_core::redeem_claim_from(&state, &redeemer, code, Some(home))
                    .await
                    .map_err(crate::payment_handlers::redeem_error)?;
            encode_reply(&crate::payment_handlers::redeemed_reply(redeemed))
        })
    })
}

/// Receive side, on the follower's nest: the home says what became of a
/// follow. Accepted only for a row whose home is the verified origin — a nest
/// that is nobody's home for this follow moves nothing. An `approved` push for
/// a tier the reader's paid follow includes (the home's rank cascade) opens
/// that tier's row beside it (`CacheDb::set_remote_follow_state_from`).
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

/// The home answers a reader's nest with what its own `offers.list` answers —
/// the one shared [`offered_tiers`] body — and carries no reach gate beyond
/// the nest signature: the same fields are public on its HTTP route. An
/// author it does not host answers the empty list, never `not_found`, so the
/// read is no hosting oracle.
///
/// [`offered_tiers`]: crate::subscription_handlers::offered_tiers
fn offers_list_handler() -> RpcHandler {
    Box::new(|state, _origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedOffersListRequest = decode(&payload)?;
            encode_reply(
                &crate::subscription_handlers::offered_tiers(&state, &req.author_id.0).await?,
            )
        })
    })
}

pub fn register_follow_federation_handlers(b: &mut FederationRouterBuilder) {
    for (kind, handler) in [
        (SUBSCRIBE_KIND, subscribe_handler()),
        (OFFERS_LIST_KIND, offers_list_handler()),
        (UNSUBSCRIBE_KIND, unsubscribe_handler()),
        (STATE_KIND, state_handler()),
        (KEY_BLOB_FETCH_KIND, key_blob_fetch_handler()),
        #[cfg(feature = "payments")]
        (CLAIM_REDEEM_KIND, claim_redeem_handler()),
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

    /// The rank cascade (`monetization.md` § Per-post pay-to-unlock): an
    /// `approved` push for a tier the reader has no row of opens one, from the
    /// home of the reader's live paid follow of that author and only from it;
    /// the free follow alone, a stranger nest, a non-approval push, or a paid
    /// follow the reader left opens nothing.
    #[tokio::test]
    async fn an_approved_push_opens_an_included_tier_beside_a_live_paid_follow() {
        let state = fixture_state();
        let reader = ActorKeypair::generate().actor_id().0;
        let author = [0x35; 32];
        let push = |origin: [u8; 32], tier: &'static str, to: &'static str| {
            let req = FedFollowStateRequest {
                author: ActorId(author),
                subscriber: ActorId(reader),
                tier: tier.into(),
                state: to.into(),
                at: Timestamp::now(),
            };
            state_handler()(
                state.clone(),
                origin,
                Bytes::from(encode_canonical(&req).unwrap().to_vec()),
            )
        };
        let tiers = || async {
            state
                .db
                .remote_follows_of(&reader, &author)
                .await
                .unwrap()
                .into_iter()
                .map(|r| (r.tier, r.state, r.home_nest_url))
                .collect::<Vec<_>>()
        };
        let follow = |tier: &'static str, at: RemoteFollowState| {
            let db = state.db.clone();
            async move {
                db.upsert_remote_follow(
                    &reader,
                    &author,
                    tier,
                    &HOME_A,
                    "https://a.example",
                    at,
                    None,
                    ago(5) as i64,
                )
                .await
                .unwrap()
            }
        };
        // The free follow alone includes nothing.
        follow(FOLLOWERS_TIER, RemoteFollowState::Approved).await;
        let err = push(HOME_A, "post-1", "approved")
            .await
            .expect_err("a free follow includes no per-post tier");
        assert_eq!(err.code, "fauna.federation.forbidden");
        // A pending paid follow: its home's approval opens the included tier.
        follow("gold", RemoteFollowState::Pending).await;
        for (origin, to) in [(STRANGER_NEST, "approved"), (HOME_A, "rejected")] {
            push(origin, "post-1", to)
                .await
                .expect_err("only the home's approval opens a row");
        }
        push(HOME_A, "post-1", "approved")
            .await
            .expect("the home's cascade approval opens the included tier");
        assert!(tiers().await.contains(&(
            "post-1".into(),
            RemoteFollowState::Approved,
            "https://a.example".into()
        )));
        // A paid follow the reader left opens nothing more.
        state
            .db
            .leave_remote_follow(&reader, &author, RemoteLeave::AllBut(FOLLOWERS_TIER))
            .await
            .unwrap();
        push(HOME_A, "post-2", "approved")
            .await
            .expect_err("a left follow includes nothing");
        assert!(!tiers().await.iter().any(|(t, ..)| t == "post-2"));
    }

    /// `removed` is terminal for a home's `follow.state` push (§ Decision 2):
    /// a home that answers a leave with `rejected` and then `approved` cannot
    /// walk the row back open, and a `removed` push onto a `removed` row is
    /// still accepted so the home's retry stays idempotent.
    #[tokio::test]
    async fn a_state_push_never_moves_a_removed_follow() {
        let state = fixture_state();
        let reader = ActorKeypair::generate().actor_id().0;
        let author = [0x34; 32];
        state
            .db
            .upsert_remote_follow(
                &reader,
                &author,
                "gold",
                &HOME_A,
                "https://a.example",
                RemoteFollowState::Approved,
                None,
                ago(5) as i64,
            )
            .await
            .unwrap();
        state
            .db
            .leave_remote_follow(&reader, &author, RemoteLeave::AllBut(FOLLOWERS_TIER))
            .await
            .unwrap();
        let push = |to: &str| {
            let req = FedFollowStateRequest {
                author: ActorId(author),
                subscriber: ActorId(reader),
                tier: "gold".into(),
                state: to.into(),
                at: Timestamp::now(),
            };
            state_handler()(
                state.clone(),
                HOME_A,
                Bytes::from(encode_canonical(&req).unwrap().to_vec()),
            )
        };
        let left =
            || async { state.db.remote_follows_of(&reader, &author).await.unwrap()[0].state };
        assert_eq!(left().await, RemoteFollowState::Removed);
        push("rejected")
            .await
            .expect("a rejected push answers success");
        push("approved")
            .await
            .expect("an approved push answers success");
        assert_eq!(
            left().await,
            RemoteFollowState::Removed,
            "rejected then approved must not re-open a left follow"
        );
        push("removed")
            .await
            .expect("a removed push onto a removed row is accepted");
        assert_eq!(left().await, RemoteFollowState::Removed);
    }

    /// The reader's leave of a remote paid follow moves the reader's
    /// own row at once, so a home that never acts on it — here, one that is
    /// never even reached — stops delivering into the reader's feed, and the
    /// home's stale `approved` push cannot re-open it. The free follow, which
    /// a leave never ends, stays.
    #[tokio::test]
    async fn a_readers_leave_ends_the_follow_on_its_own_nest_whatever_the_home_answers() {
        use crate::db::feeds::{FeedAudience, FeedLocality};
        use fauna_core::data::{Post, PostBody};
        use fauna_core::scoring::FilterCombination;

        let state = fixture_state();
        let reader = ActorKeypair::generate();
        let reader_id = reader.actor_id().0;
        let author = ActorKeypair::generate();
        let author_id = author.actor_id().0;
        // A home this test never reaches: the leave's relay fails, as a home
        // that ignores the leave would leave it.
        let home_url = "https://0.0.0.0:1";
        state
            .db
            .upsert_remote_follow(
                &reader_id,
                &author_id,
                "gold",
                &HOME_A,
                home_url,
                RemoteFollowState::Approved,
                None,
                ago(60) as i64,
            )
            .await
            .unwrap();
        let post = Post {
            author: author.actor_id(),
            created_at: Timestamp::now(),
            body: PostBody::Text {
                content: "a delivered post".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let (bytes, env) = fauna_core::encoding::sign_envelope(&author, &post).unwrap();
        let wire =
            fauna_core::encoding::canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).unwrap();
        let post_id = *blake3::hash(&wire).as_bytes();
        state
            .db
            .put_delivered_post_index_only(&post_id, &wire, home_url, &HOME_A)
            .await
            .unwrap();
        let listed = || async {
            state
                .db
                .query_feed_for(
                    &[],
                    FilterCombination::All,
                    &[],
                    None,
                    50,
                    FeedAudience::OnBox,
                    FeedLocality::ForReader(reader_id),
                )
                .await
                .unwrap()
                .into_iter()
                .any(|r| r.post_id == post_id)
        };
        assert!(listed().await, "the follow delivers before the leave");

        let req = UnsubscribeRequest {
            author_id: ActorId(author_id),
            // The caller's named home is not where the leave goes.
            home_nest_url: Some("https://elsewhere.example".into()),
            intent: Some(intent(
                &reader,
                author_id,
                "gold",
                FollowIntentKind::Unsubscribe,
                state.nest_identity.public_key_bytes(),
                ago(5),
            )),
            tier: None,
            extra: Default::default(),
        };
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            remote_unsubscribe(&state, reader_id, &req),
        )
        .await;

        let row_state = || async {
            state
                .db
                .remote_follows_of(&reader_id, &author_id)
                .await
                .unwrap()[0]
                .state
        };
        assert_eq!(row_state().await, RemoteFollowState::Removed);
        assert!(!listed().await, "the left follow delivers nothing more");
        assert_eq!(
            state
                .db
                .has_approved_remote_follow_from(&author_id, &HOME_A)
                .await
                .unwrap(),
            None,
            "the delivery gate no longer admits that home for this author"
        );

        // The home's stale `approved` lands, and re-opens nothing.
        let push = FedFollowStateRequest {
            author: ActorId(author_id),
            subscriber: ActorId(reader_id),
            tier: "gold".into(),
            state: "approved".into(),
            at: Timestamp::now(),
        };
        state_handler()(
            state.clone(),
            HOME_A,
            Bytes::from(encode_canonical(&push).unwrap().to_vec()),
        )
        .await
        .expect("the home's push is answered");
        assert_eq!(row_state().await, RemoteFollowState::Removed);
        assert!(
            !listed().await,
            "a stale approval does not restore the follow"
        );
    }

    /// A leave never ends the free follow (unsubscribe is not unfollow), and a
    /// leave of an author with no follow recorded here is `not_subscribed`
    /// before anything is dialled.
    #[tokio::test]
    async fn a_leave_keeps_the_free_follow_and_needs_a_recorded_follow() {
        let state = fixture_state();
        let reader = ActorKeypair::generate();
        let reader_id = reader.actor_id().0;
        let author_id = ActorKeypair::generate().actor_id().0;
        let req = UnsubscribeRequest {
            author_id: ActorId(author_id),
            home_nest_url: Some("https://0.0.0.0:1".into()),
            intent: Some(intent(
                &reader,
                author_id,
                "gold",
                FollowIntentKind::Unsubscribe,
                state.nest_identity.public_key_bytes(),
                ago(5),
            )),
            tier: None,
            extra: Default::default(),
        };
        let err = remote_unsubscribe(&state, reader_id, &req)
            .await
            .expect_err("no follow recorded");
        assert_eq!(err.code, "fauna.subscriptions.not_subscribed");

        state
            .db
            .upsert_remote_follow(
                &reader_id,
                &author_id,
                FOLLOWERS_TIER,
                &HOME_A,
                "https://0.0.0.0:1",
                RemoteFollowState::Approved,
                None,
                ago(60) as i64,
            )
            .await
            .unwrap();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            remote_unsubscribe(&state, reader_id, &req),
        )
        .await;
        assert_eq!(
            state
                .db
                .remote_follows_of(&reader_id, &author_id)
                .await
                .unwrap()[0]
                .state,
            RemoteFollowState::Approved,
            "the free follow survives a leave"
        );

        // The unfollow — the intent names `followers` — marks every row,
        // the free follow included (`subscriptions.md` § The free followers
        // tier — follow and unfollow, rule 2).
        let unfollow = UnsubscribeRequest {
            intent: Some(intent(
                &reader,
                author_id,
                FOLLOWERS_TIER,
                FollowIntentKind::Unsubscribe,
                state.nest_identity.public_key_bytes(),
                ago(4),
            )),
            tier: Some(FOLLOWERS_TIER.into()),
            ..req.clone()
        };
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            remote_unsubscribe(&state, reader_id, &unfollow),
        )
        .await;
        assert_eq!(
            state
                .db
                .remote_follows_of(&reader_id, &author_id)
                .await
                .unwrap()[0]
                .state,
            RemoteFollowState::Removed,
            "the unfollow ends the free follow on the reader's own nest"
        );
    }

    /// The by-name marking's three shapes on the reader's own nest: `All`
    /// marks every row, `Only` the named one, `AllBut` every row but it.
    #[tokio::test]
    async fn a_leave_marks_the_readers_rows_by_name() {
        use crate::db::remote_follows::RemoteLeave;
        let author = [0x35; 32];
        for (leave, want_removed) in [
            (RemoteLeave::All, vec!["followers", "gold", "silver"]),
            (RemoteLeave::Only("gold"), vec!["gold"]),
            (RemoteLeave::AllBut(FOLLOWERS_TIER), vec!["gold", "silver"]),
        ] {
            let state = fixture_state();
            let reader = ActorKeypair::generate().actor_id().0;
            for tier in [FOLLOWERS_TIER, "silver", "gold"] {
                state
                    .db
                    .upsert_remote_follow(
                        &reader,
                        &author,
                        tier,
                        &HOME_A,
                        "https://a.example",
                        RemoteFollowState::Approved,
                        None,
                        ago(5) as i64,
                    )
                    .await
                    .unwrap();
            }
            let moved = state
                .db
                .leave_remote_follow(&reader, &author, leave)
                .await
                .unwrap();
            let mut removed: Vec<String> = state
                .db
                .remote_follows_of(&reader, &author)
                .await
                .unwrap()
                .into_iter()
                .filter(|r| r.state == RemoteFollowState::Removed)
                .map(|r| r.tier)
                .collect();
            removed.sort();
            assert_eq!(removed, want_removed, "{leave:?}");
            assert_eq!(moved, want_removed.len(), "{leave:?}");
        }
    }

    /// An unfollow in the pending window: the reader's `pending` followers row
    /// moves to `removed` with the leave, so `status.get` no longer reports a
    /// follow asked (`subscriptions.md` § The free followers tier — follow and
    /// unfollow, rule 4) — while a `rejected` row stays as the home left it.
    #[tokio::test]
    async fn an_unfollow_withdraws_the_readers_pending_row() {
        use crate::db::remote_follows::RemoteLeave;
        let state = fixture_state();
        let reader = ActorKeypair::generate().actor_id().0;
        let author = [0x36; 32];
        for (tier, row_state) in [
            (FOLLOWERS_TIER, RemoteFollowState::Pending),
            ("gold", RemoteFollowState::Rejected),
        ] {
            state
                .db
                .upsert_remote_follow(
                    &reader,
                    &author,
                    tier,
                    &HOME_A,
                    "https://a.example",
                    row_state,
                    None,
                    ago(5) as i64,
                )
                .await
                .unwrap();
        }
        let rows = state.db.remote_follows_of(&reader, &author).await.unwrap();
        assert!(crate::subscription_handlers::remote_status(&rows).follow_pending);
        let moved = state
            .db
            .leave_remote_follow(&reader, &author, RemoteLeave::All)
            .await
            .unwrap();
        assert_eq!(moved, 1);
        let rows = state.db.remote_follows_of(&reader, &author).await.unwrap();
        let status = crate::subscription_handlers::remote_status(&rows);
        assert!(!status.follow_pending, "the withdrawn follow is not asked");
        assert_eq!(status.tier, None);
        assert!(
            rows.iter()
                .any(|r| r.tier == "gold" && r.state == RemoteFollowState::Rejected)
        );
    }

    // ── The redeem twin (`federated-follow.md` § Decision 2 → *The redeem
    //    twin*) — the author's-nest arm, driven by its handler directly ──────

    #[cfg(feature = "payments")]
    mod claim_redeem_twin {
        use super::*;

        fn redeem_intent(
            redeemer: &ActorKeypair,
            author: [u8; 32],
            code: &str,
            home: [u8; 32],
            at: u64,
        ) -> EmbedAsBytes {
            let intent = ClaimRedeemIntent {
                redeemer: redeemer.actor_id(),
                author: ActorId(author),
                code: code.to_string(),
                home_nest_id: home,
                created_at: Timestamp(at),
            };
            let (bytes, env) = fauna_core::encoding::sign_envelope(redeemer, &intent).unwrap();
            EmbedAsBytes::from_signed(bytes, env)
        }

        /// A hosted author with a paid `gold` tier and one unredeemed code.
        async fn author_with_code(state: &Arc<AppState>, code: &str) -> [u8; 32] {
            let author = hosted_author(state).await;
            state
                .db
                .create_subscription_tier(
                    &author, "gold", 1, None, None, None, false, None, None, false,
                )
                .await
                .unwrap();
            mint(state, &author, "gold", code).await;
            author
        }

        async fn mint(state: &Arc<AppState>, author: &[u8; 32], tier: &str, code: &str) {
            state
                .db
                .insert_payment_claim(code, author, tier, "manual", code, None)
                .await
                .unwrap();
        }

        async fn redeem(
            state: &Arc<AppState>,
            origin: [u8; 32],
            wire: &EmbedAsBytes,
        ) -> Result<ClaimRedeemReply, RpcError> {
            claim_redeem_handler()(state.clone(), origin, twin_payload(wire))
                .await
                .map(|b| decode_strict(&b).unwrap())
        }

        /// The redeem lands for the remote buyer exactly as a local one does —
        /// the claim stamped to them — and the payment-entitled request carries
        /// their home, so the approval copies it onto the roster.
        #[tokio::test]
        async fn a_relayed_redeem_binds_the_remote_buyer_and_carries_their_home() {
            let state = fixture_state();
            let author = author_with_code(&state, "CODE-0001").await;
            let buyer = ActorKeypair::generate();
            let reply = redeem(
                &state,
                HOME_A,
                &redeem_intent(&buyer, author, "CODE-0001", HOME_A, ago(5)),
            )
            .await
            .expect("the code's home admits its own buyer's nest");
            assert_eq!((reply.tier.as_str(), reply.queued), ("gold", true));
            assert_eq!(reply.author, ActorId(author));
            let claim = state
                .db
                .get_payment_claim("CODE-0001")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(claim.redeemed_by, Some(buyer.actor_id().0.to_vec()));
            let rows = state.db.list_subscribe_requests(&author).await.unwrap();
            assert_eq!(rows.len(), 1);
            assert!(rows[0].payment_entitled);
            assert_eq!(
                state.db.subscribe_request_home(rows[0].id).await.unwrap(),
                Some(HOME_A),
                "the request row names the buyer's home"
            );
        }

        /// § Decision 2's structural half holds for the redeem: an intent
        /// naming another home is refused and stamps nothing.
        #[tokio::test]
        async fn a_redeem_intent_naming_another_home_is_forbidden() {
            let state = fixture_state();
            let author = author_with_code(&state, "CODE-0002").await;
            let buyer = ActorKeypair::generate();
            let err = redeem(
                &state,
                STRANGER_NEST,
                &redeem_intent(&buyer, author, "CODE-0002", HOME_A, ago(5)),
            )
            .await
            .expect_err("a nest carrying another home's intent is refused");
            assert_eq!(err.code, "fauna.federation.forbidden");
            let claim = state
                .db
                .get_payment_claim("CODE-0002")
                .await
                .unwrap()
                .unwrap();
            assert!(claim.redeemed_at.is_none(), "the claim is not consumed");
        }

        /// An author not hosted here, and a code of another author hosted here,
        /// both answer as a wrong code does: the kind tells a guesser nothing.
        #[tokio::test]
        async fn an_unhosted_author_or_another_authors_code_is_claim_not_found() {
            let state = fixture_state();
            let _author = author_with_code(&state, "CODE-0003").await;
            let other = hosted_author(&state).await;
            let buyer = ActorKeypair::generate();
            for author in [ActorKeypair::generate().actor_id().0, other] {
                let err = redeem(
                    &state,
                    HOME_A,
                    &redeem_intent(&buyer, author, "CODE-0003", HOME_A, ago(5)),
                )
                .await
                .expect_err("refused");
                assert_eq!(err.code, "fauna.payments.claim_not_found");
            }
            let claim = state
                .db
                .get_payment_claim("CODE-0003")
                .await
                .unwrap()
                .unwrap();
            assert!(claim.redeemed_at.is_none());
        }

        /// A voided code is refused across nests as at the local door.
        #[tokio::test]
        async fn a_voided_code_is_refused_across_nests() {
            let state = fixture_state();
            let author = author_with_code(&state, "CODE-0004").await;
            assert!(state.db.void_payment_claim("CODE-0004").await.unwrap());
            let err = redeem(
                &state,
                HOME_A,
                &redeem_intent(
                    &ActorKeypair::generate(),
                    author,
                    "CODE-0004",
                    HOME_A,
                    ago(5),
                ),
            )
            .await
            .expect_err("refused");
            assert_eq!(err.code, "fauna.payments.claim_voided");
        }

        /// A membership tier's code admits an account to this nest, which a
        /// redeem across nests never grants: refused before the claim is
        /// stamped, so the buyer can still register here with it.
        #[tokio::test]
        async fn a_membership_code_is_refused_with_claim_admits_elsewhere_and_stays_unredeemed() {
            let state = fixture_state();
            let author = author_with_code(&state, "CODE-0005").await;
            state
                .db
                .upsert_membership_tier(&author, "gold", "personal", "free")
                .await
                .unwrap();
            let err = redeem(
                &state,
                HOME_A,
                &redeem_intent(
                    &ActorKeypair::generate(),
                    author,
                    "CODE-0005",
                    HOME_A,
                    ago(5),
                ),
            )
            .await
            .expect_err("refused");
            assert_eq!(err.code, "fauna.payments.claim_admits_elsewhere");
            let claim = state
                .db
                .get_payment_claim("CODE-0005")
                .await
                .unwrap()
                .unwrap();
            assert!(claim.redeemed_at.is_none(), "the code stays redeemable");
            assert!(
                state
                    .db
                    .list_subscribe_requests(&author)
                    .await
                    .unwrap()
                    .is_empty(),
                "nothing is enqueued"
            );
        }

        /// The budget is spent by EVERY relayed redeem, a seen subscriber's
        /// included — a miss answers whether a code exists. A buyer holding a
        /// row keeps guessing; the window refuses past its bound.
        #[tokio::test]
        async fn every_relayed_redeem_spends_the_authors_budget_a_seen_buyers_too() {
            let state = fixture_state();
            let author = author_with_code(&state, "CODE-0006").await;
            let buyer = ActorKeypair::generate();
            redeem(
                &state,
                HOME_A,
                &redeem_intent(&buyer, author, "CODE-0006", HOME_A, ago(5)),
            )
            .await
            .expect("the first redeem is admitted");
            let budget = crate::bridge_rate_limit::FOLLOW_INTENT_TARGET_MAX as usize;
            let mut misses = 0usize;
            loop {
                let guess = format!("GUESS-{misses:04}");
                match redeem(
                    &state,
                    HOME_A,
                    &redeem_intent(&buyer, author, &guess, HOME_A, ago(5)),
                )
                .await
                {
                    Err(e) if e.code.ends_with(".rate_limited") => break,
                    Err(e) => assert_eq!(e.code, "fauna.payments.claim_not_found"),
                    Ok(r) => panic!("a guessed code redeemed: {r:?}"),
                }
                misses += 1;
                assert!(misses <= budget, "the window never refused");
            }
            assert_eq!(
                misses + 1,
                budget,
                "the redeem and the misses share one budget"
            );
        }

        /// An older redeem replayed after a newer intent for the tier changes
        /// nothing: the monotonic rule a leave relies on.
        #[tokio::test]
        async fn an_older_redeem_after_a_newer_intent_for_the_tier_changes_nothing() {
            let state = fixture_state();
            let author = author_with_code(&state, "CODE-0007").await;
            let buyer = ActorKeypair::generate();
            assert!(
                state
                    .db
                    .admit_follow_intent(&author, &buyer.actor_id().0, "gold", ago(5) as i64)
                    .await
                    .unwrap()
            );
            let err = redeem(
                &state,
                HOME_A,
                &redeem_intent(&buyer, author, "CODE-0007", HOME_A, ago(60)),
            )
            .await
            .expect_err("a stale intent is refused");
            assert_eq!(err.code, "fauna.federation.forbidden");
            let claim = state
                .db
                .get_payment_claim("CODE-0007")
                .await
                .unwrap()
                .unwrap();
            assert!(claim.redeemed_at.is_none());
        }
    }

    /// Seed the four tier shapes the browse must tell apart: an offered paid
    /// tier carrying an asking price, a hidden tier, and a per-post tier.
    async fn seed_offer_shapes(state: &Arc<AppState>, author: &[u8; 32]) {
        let ask = fauna_protocol::subscriptions::TierAskingPrice {
            value: 1000,
            unit: fauna_protocol::subscriptions::UNIT_MSAT.into(),
            extra: Default::default(),
        };
        let db = &state.db;
        db.create_subscription_tier(
            author,
            "gold",
            20,
            Some("Top tier"),
            Some("$10/mo"),
            Some("https://pay.example/gold"),
            false,
            None,
            Some(&ask),
            false,
        )
        .await
        .unwrap();
        db.create_subscription_tier(
            author,
            "backstage",
            30,
            None,
            None,
            None,
            false,
            None,
            None,
            true,
        )
        .await
        .unwrap();
        db.create_subscription_tier(
            author,
            "post-unlock-1",
            1,
            None,
            None,
            None,
            true,
            Some("1".repeat(64).as_str()),
            None,
            false,
        )
        .await
        .unwrap();
    }

    fn offers_payload(author: [u8; 32]) -> Bytes {
        let req = FedOffersListRequest {
            author_id: ActorId(author),
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    }

    /// `subscriptions.md` § Pillar 1 → *The offers browse across nests*: the
    /// home answers what its own `offers.list` answers — the offered tier, no
    /// hidden tier, no per-post tier, and no `asking_price` — to any
    /// nest-signed peer, with no reach gate.
    #[tokio::test]
    async fn the_home_answers_a_hosted_authors_offered_tiers_with_the_three_withholdings() {
        let state = fixture_state();
        let author = hosted_author(&state).await;
        seed_offer_shapes(&state, &author).await;

        let reply = offers_list_handler()(state.clone(), STRANGER_NEST, offers_payload(author))
            .await
            .expect("the home serves the offers browse");
        let reply: TiersListReply = fauna_protocol::decode_strict(&reply).unwrap();
        let names: Vec<&str> = reply.tiers.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["gold"], "only the offered tier: {names:?}");
        assert!(reply.tiers[0].asking_price.is_none());
        assert_eq!(reply.tiers[0].price_hint.as_deref(), Some("$10/mo"));
    }

    /// An author the home does not host answers the empty list, never a
    /// `not_found` — the read is no hosting oracle.
    #[tokio::test]
    async fn the_home_answers_an_unhosted_author_with_the_empty_list() {
        let state = fixture_state();
        let stranger = ActorKeypair::generate().actor_id().0;

        let reply = offers_list_handler()(state.clone(), STRANGER_NEST, offers_payload(stranger))
            .await
            .expect("an unhosted author is the empty list, not an error");
        let reply: TiersListReply = fauna_protocol::decode_strict(&reply).unwrap();
        assert!(reply.tiers.is_empty());
    }
}
