//! WS-RPC handlers for the fauna.subscriptions.* namespace.
//!
//! Migrates the 15 authenticated subscription-management routes from
//! bins/fauna-nest/src/subscription_routes.rs to WS-RPC kinds. HTTP
//! twins stay registered until per-app cleanup TODOs retire them;
//! the 5 client-minted-KeyBlob mutation routes among them keep their 503
//! gate on the HTTP side (that management plane is WS-RPC only).
//!
//! **Key custody.** The nest never mints or holds a subscription period key.
//! Every tier is **client-minted**: `tiers.create` stores the author's birth
//! `KeyBlob` at version 1 (a `followers` tier lazily provisioned by a first
//! follow gets its first blob with that follow's approval instead), and every
//! later roster change or rotation arrives as a client-minted `KeyBlob` upload
//! that the nest verifies and stores as-is — it wraps nothing itself.
//!
//! Spec: the broadcast-keyblob upload/accept design (tracked internally).

use std::time::Duration;

use serde_bytes::ByteBuf;

use std::sync::Arc;

use fauna_core::data::{DeviceAuthorization, Timestamp};
use fauna_core::encoding::{
    EmbedAsBytes, canonical_decode, canonical_encode, decode_signed_bytes, verify_envelope,
};
use fauna_core::identity::ActorId;
use fauna_core::subscription::crypto::verify_key_blob_signature;
use fauna_core::subscription::types::KeyBlob;
use fauna_core::subscription::{FOLLOWERS_TIER, FOLLOWERS_TIER_RANK};
use fauna_protocol::subscriptions::{
    ApproveRequestReply, ApproveRequestRequest, DelegateUploadReply, DelegateUploadRequest,
    EncryptedKeyBlobUpload, KeyBlobGetReply, KeyBlobGetRequest, MineListReply, MineListRequest,
    MineSubscription, OffersListRequest, PendingRequest, PostUnlockGetReply, PostUnlockGetRequest,
    PostUnlockOffer, RejectRequestReply, RejectRequestRequest, RemoveSubscriberReply,
    RemoveSubscriberRequest, RequestsListReply, RequestsListRequest, RotateKeyBlobReply,
    RotateKeyBlobRequest, StatusGetReply, StatusGetRequest, SubscribeReply, SubscribeRequest,
    SubscriberEntry, SubscribersListReply, SubscribersListRequest, TierClearFieldReply,
    TierClearFieldRequest, TierClearableField, TierCreateReply, TierCreateRequest, TierDeleteReply,
    TierDeleteRequest, TierItem, TierUpdateReply, TierUpdateRequest, TiersListReply,
    TiersListRequest, UnsubscribeReply, UnsubscribeRequest,
};
use fauna_protocol::{RpcError, Value, decode_strict as decode};

use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Error helpers ──────────────────────────────────────────────

/// FIPS-203 ML-KEM-768 encapsulation-key size, in bytes — the length a
/// subscriber's published post-quantum ek must be (mirrors
/// `fauna_pq_kem::MLKEM768_ENCAPS_KEY_LEN`; not a direct dep of this binary, so
/// the constant is restated, matching the mail leg-A
/// `ML_KEM_768_ENCAPS_KEY_LEN` in `bridge_routing_handlers.rs`).
const MLKEM768_ENCAPS_KEY_LEN: usize = 1184;

pub(crate) fn malformed_upload(reason: impl std::fmt::Display) -> RpcError {
    let mut e = RpcError::new(
        "fauna.subscriptions.malformed_upload",
        "error.subscriptions.malformed_upload",
    );
    e.details = Some(Box::new(Value::String(format!("{reason}"))));
    e
}

pub(crate) fn invalid_delegation(reason: &str) -> RpcError {
    let mut e = RpcError::new(
        "fauna.subscriptions.invalid_delegation",
        "error.subscriptions.invalid_delegation",
    );
    e.details = Some(Box::new(Value::String(reason.into())));
    e
}

pub(crate) fn invalid_signature(reason: &str) -> RpcError {
    let mut e = RpcError::new(
        "fauna.subscriptions.invalid_signature",
        "error.subscriptions.invalid_signature",
    );
    e.details = Some(Box::new(Value::String(reason.into())));
    e
}

pub(crate) fn permission_denied(reason: &str) -> RpcError {
    crate::rpc_errors::permission_denied_ns("subscriptions", reason)
}

pub(crate) use crate::rpc_errors::internal;

fn not_subscribed(reason: &str) -> RpcError {
    let mut e = RpcError::new(
        "fauna.subscriptions.not_subscribed",
        "error.subscriptions.not_subscribed",
    );
    e.details = Some(Box::new(Value::String(reason.into())));
    e
}

fn key_blob_not_found(reason: &str) -> RpcError {
    let mut e = RpcError::new(
        "fauna.subscriptions.key_blob_not_found",
        "error.subscriptions.key_blob_not_found",
    );
    e.details = Some(Box::new(Value::String(reason.into())));
    e
}

fn tier_not_found(reason: &str) -> RpcError {
    crate::rpc_errors::tier_not_found_ns("subscriptions", reason)
}

/// The subscribe door's wire-boundary refusal for `subscriber_id ==
/// author_id`. An author's own access to
/// their post is custody (`FeedManager::unlock_gated_post`'s `is_author`
/// branch), never a purchase or a follow, and the client half of this
/// (`FeedManager::is_local_actor`) already refuses it at both
/// the render trigger and the mutation — this is the same rule at the door
/// every app and any third-party client reaches. Not "unrecoverable state":
/// a landed self-subscribe row is removable (`subscribers.remove`); this is
/// ordinary request validation, so it rides [`forbidden_ns`](crate::rpc_errors::forbidden_ns)
/// rather than a bespoke code family.
fn self_subscribe_refused(reason: &str) -> RpcError {
    crate::rpc_errors::forbidden_ns("subscriptions", reason)
}

fn request_not_found(reason: &str) -> RpcError {
    let mut e = RpcError::new(
        "fauna.subscriptions.request_not_found",
        "error.subscriptions.request_not_found",
    );
    e.details = Some(Box::new(Value::String(reason.into())));
    e
}

fn wrong_request_kind(reason: &str) -> RpcError {
    let mut e = RpcError::new(
        "fauna.subscriptions.wrong_request_kind",
        "error.subscriptions.wrong_request_kind",
    );
    e.details = Some(Box::new(Value::String(reason.into())));
    e
}

fn tier_already_exists(reason: &str) -> RpcError {
    let mut e = RpcError::new(
        "fauna.subscriptions.tier_already_exists",
        "error.subscriptions.tier_already_exists",
    );
    e.details = Some(Box::new(Value::String(reason.into())));
    e
}

fn missing_upload() -> RpcError {
    let mut e = RpcError::new(
        "fauna.subscriptions.missing_upload",
        "error.subscriptions.missing_upload",
    );
    e.details = Some(Box::new(Value::String(
        "encrypted_upload is required: this tier's key is client-minted, so the nest \
         cannot wrap it — the author's client must supply the KeyBlob"
            .into(),
    )));
    e
}

fn tier_mismatch(reason: &str) -> RpcError {
    let mut e = RpcError::new(
        "fauna.subscriptions.tier_mismatch",
        "error.subscriptions.tier_mismatch",
    );
    e.details = Some(Box::new(Value::String(reason.into())));
    e
}

/// The caller tried to change a tier's per-post pay-to-unlock designation.
/// It is **create-time immutable** (`monetization.md` § Per-post pay-to-unlock
/// — re-pointing a sold unlock is a rug-pull on everyone who already bought
/// it), and this covers attaching one to a tier created without it just as
/// much as moving an existing one.
fn designation_immutable(current: Option<&str>, requested: &str) -> RpcError {
    let mut e = RpcError::new(
        "fauna.subscriptions.designation_immutable",
        "error.subscriptions.designation_immutable",
    );
    e.details = Some(Box::new(Value::String(format!(
        "unlocks_post is create-time immutable: tier designates {}, requested {requested}",
        current.unwrap_or("no post")
    ))));
    e
}

fn stale_rotation(stored: u64, uploaded: u64) -> RpcError {
    let mut e = RpcError::new(
        "fauna.subscriptions.stale_rotation",
        "error.subscriptions.stale_rotation",
    );
    e.details = Some(Box::new(Value::String(format!(
        "stored rotated_at: {stored}, uploaded: {uploaded}"
    ))));
    e
}

fn roster_mismatch(expected: usize, got: usize) -> RpcError {
    let mut e = RpcError::new(
        "fauna.subscriptions.roster_mismatch",
        "error.subscriptions.roster_mismatch",
    );
    e.details = Some(Box::new(Value::String(format!(
        "expected {expected} entries, got {got}"
    ))));
    e
}

pub(crate) use crate::rpc_errors::encode_reply;

// ── verify_encrypted_upload ────────────────────────────────────

/// Decode + verify the four-step auth chain for the client-minted KeyBlob
/// upload envelope.
///
/// Returns the decoded `(KeyBlob, DeviceAuthorization)` on success. The
/// state-level checks (tier existence, rotated_at monotonicity, roster
/// shape) live in the per-kind handlers (`requests.approve`,
/// `subscribers.remove`).
///
/// Auth chain (per spec § Auth chain):
/// 1. Body well-formed (BARE decode)            -> `fauna.subscriptions.malformed_upload`
/// 2. `verify_key_blob_signature == Ok(true)`   -> `fauna.subscriptions.invalid_signature`
/// 3. bearer ∈ {key_blob.author, signer.device_key} -> `fauna.subscriptions.permission_denied`
/// 4. key_blob.author == expected_author        -> `fauna.subscriptions.permission_denied`
pub fn verify_encrypted_upload(
    bearer: [u8; 32],
    upload: &EncryptedKeyBlobUpload,
    expected_author: [u8; 32],
) -> Result<(KeyBlob, DeviceAuthorization), RpcError> {
    let (key_blob_bytes, key_blob_env) = upload
        .key_blob
        .clone()
        .into_signed()
        .map_err(|e| malformed_upload(format!("key_blob envelope: {e}")))?;
    let key_blob: KeyBlob = decode_signed_bytes(&key_blob_bytes)
        .map_err(|e| malformed_upload(format!("key_blob: {e}")))?;

    let (signer_auth_bytes, signer_auth_env) = upload
        .signer_auth
        .clone()
        .into_signed()
        .map_err(|e| malformed_upload(format!("signer_auth envelope: {e}")))?;
    let signer_auth: DeviceAuthorization = decode_signed_bytes(&signer_auth_bytes)
        .map_err(|e| malformed_upload(format!("signer_auth: {e}")))?;

    match verify_key_blob_signature(
        &key_blob,
        &key_blob_bytes,
        &key_blob_env,
        &signer_auth,
        &signer_auth_bytes,
        &signer_auth_env,
    ) {
        Ok(true) => {}
        Ok(false) => return Err(invalid_signature("signature chain invalid")),
        Err(_) => return Err(internal("signature verification error")),
    }

    if bearer != key_blob.author.0 && bearer != signer_auth.device_key {
        return Err(permission_denied(
            "bearer is not the author or a delegated device that signed the blob",
        ));
    }

    if key_blob.author.0 != expected_author {
        return Err(permission_denied(
            "key_blob.author does not match the operation's author scope",
        ));
    }

    Ok((key_blob, signer_auth))
}

/// The auth chain plus every **tier-level** state check a client-minted
/// `KeyBlob` upload must clear, in the one order all three upload doors share:
/// `requests.approve`, `subscribers.remove` and `key_blob.rotate`.
///
/// What is deliberately NOT here is the roster check, because it is the only
/// thing the three doors genuinely disagree about — approve expects
/// `roster ∪ {joiner}`, remove expects `roster \ {leaver}`, rotate expects the
/// roster unchanged. Everything above it (chain, tier exists, tier matches,
/// no active MLS group, a stored blob exists — save the one first-blob case
/// [`FirstBlob`] names — `rotated_at` strictly advancing) is one rule with one set of refusal codes, and it was copied
/// out twice before the third door arrived. See [`current_roster`] / [`require_roster_shape`] for the half
/// that varies and [`store_uploaded_blob`] for the half that follows.
async fn verify_and_precheck_upload(
    state: &Arc<AppState>,
    author_id: &[u8; 32],
    tier_name: &str,
    upload: &EncryptedKeyBlobUpload,
    first_blob: FirstBlob,
) -> Result<PrecheckedUpload, RpcError> {
    let (key_blob, _signer_auth) = verify_encrypted_upload(*author_id, upload, *author_id)?;

    // Tier exists.
    let _tier = state
        .db
        .get_subscription_tier(author_id, tier_name)
        .await
        .map_err(internal)?
        .ok_or_else(|| tier_not_found("tier not found"))?;
    // The blob names the tier the operation is scoped to.
    if key_blob.tier != tier_name {
        return Err(tier_mismatch(&format!(
            "key_blob.tier={:?} but request tier={:?}",
            key_blob.tier, tier_name
        )));
    }

    // A tier's stored blob is the prior every upload must advance past. Two
    // writers create tiers: `tiers.create` stores the author's birth blob at
    // version 1, and `ensure_followers_tier` — the first follow of an author
    // who never created their `followers` tier — writes the row alone (the
    // nest cannot mint). That followers tier's first blob therefore rides the
    // first follow approval, minted under the key the author's client
    // generates on demand, and is the ONE upload a blob-less tier accepts.
    // Any other blob-less tier is a state no writer produces: refused.
    let Some((prior_version, _, prior_stored)) = state
        .db
        .get_current_key_blob(author_id, tier_name)
        .await
        .map_err(internal)?
    else {
        if first_blob == FirstBlob::FollowersFirstApproval && tier_name == FOLLOWERS_TIER {
            return Ok(PrecheckedUpload {
                key_blob,
                next_version: 1,
            });
        }
        return Err(key_blob_not_found("tier has no stored key blob"));
    };

    // `rotated_at` strictly monotonic against the stored blob. Stored bytes are
    // the dag-cbor-encoded EmbedAsBytes wire shape; the inner canonical dag-cbor
    // decodes via `decode_signed_bytes` per sign-over-CID.
    let prior_wire: EmbedAsBytes = canonical_decode(&prior_stored)
        .map_err(|e| internal(format!("decode prior blob wire: {e}")))?;
    let prior: KeyBlob = decode_signed_bytes(&prior_wire.bytes)
        .map_err(|e| internal(format!("decode prior blob: {e}")))?;
    if key_blob.rotated_at.0 <= prior.rotated_at.0 {
        return Err(stale_rotation(prior.rotated_at.0, key_blob.rotated_at.0));
    }

    Ok(PrecheckedUpload {
        key_blob,
        next_version: prior_version + 1,
    })
}

/// Whether an upload door may land the FIRST blob of a blob-less tier — only
/// `requests.approve`, and only for the lazily provisioned `followers` tier
/// (see [`verify_and_precheck_upload`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum FirstBlob {
    FollowersFirstApproval,
    Refused,
}

/// What [`verify_and_precheck_upload`] hands the door that called it: the
/// verified blob, and the version it lands at — one past the stored blob the
/// precheck read.
struct PrecheckedUpload {
    key_blob: KeyBlob,
    next_version: i64,
}

/// The tier's confirmed subscriber set — the base every door's roster check
/// adjusts before comparing against the blob's entries.
async fn current_roster(
    state: &Arc<AppState>,
    author_id: &[u8; 32],
    tier_name: &str,
) -> Result<std::collections::BTreeSet<[u8; 32]>, RpcError> {
    let rows = state
        .db
        .list_subscribers(author_id, tier_name)
        .await
        .map_err(internal)?;
    let mut set = std::collections::BTreeSet::new();
    for s in rows {
        let arr: [u8; 32] = s
            .subscriber_id
            .as_slice()
            .try_into()
            .map_err(|_| internal("subscriber_id is not 32 bytes"))?;
        set.insert(arr);
    }
    Ok(set)
}

/// Refuse unless the blob wraps the period key to exactly `expected` — no
/// member left uncovered (they would lose the tier) and none added (they would
/// gain it without ever being approved).
fn require_roster_shape(
    expected: &std::collections::BTreeSet<[u8; 32]>,
    key_blob: &KeyBlob,
) -> Result<(), RpcError> {
    let actual: std::collections::BTreeSet<[u8; 32]> =
        key_blob.entries.iter().map(|e| e.subscriber.0).collect();
    if *expected != actual {
        return Err(roster_mismatch(expected.len(), actual.len()));
    }
    Ok(())
}

/// Store the verified upload as-is (dag-cbor-encoded `EmbedAsBytes` — envelope
/// + canonical bytes) at `version`, the one the precheck computed.
///
/// The content-addressing key is the inner CID's multihash — BLAKE3 over
/// `upload.key_blob.bytes`, the canonical bytes, not the framed wire shape.
async fn store_uploaded_blob(
    state: &Arc<AppState>,
    author_id: &[u8; 32],
    tier_name: &str,
    upload: &EncryptedKeyBlobUpload,
    version: i64,
) -> Result<(), RpcError> {
    let stored_bytes = canonical_encode(&upload.key_blob).map_err(internal)?;
    let blob_hash = blake3::hash(&upload.key_blob.bytes);
    state
        .db
        .upsert_current_key_blob(
            author_id,
            tier_name,
            version,
            blob_hash.as_bytes(),
            &stored_bytes,
        )
        .await
        .map_err(internal)?;
    Ok(())
}

// ── read-only handlers ─────────────────────────────────────────

/// `fauna.subscriptions.status.get` — narrow the multi-tier HTTP shape into
/// the single-tier StatusGetReply by picking the subscriber's highest-rank
/// tier (highest `rank` value: higher ranks subsume lower —
/// `monetization.md:19`; the rank-0 `followers` base is a fallback, below).
/// An earlier revision read the schema the other way ("rank=1 is the top
/// tier") and picked the LOWEST paid rank — invisible while a subscriber
/// held exactly one paid tier, wrong the moment the `monetization.md:126`
/// rank fan-out made every paid subscriber also hold the rank-1 unlock
/// tiers. `unlocks_post`-designated tiers are excluded from the pick
/// entirely: a machine-named single-post tier is a purchase, never the
/// subscription relationship this read reports (`monetization.md:128` — the
/// designation is hidden from every generic tier surface).
///
/// `expires_at` is the chosen tier's paid `valid_until` window when one is
/// stamped (verified-payment grants, monetization.md § Pillar 3) and `None`
/// otherwise (manual/auto grants never expire). `auto_approve` is the chosen
/// tier's `auto_approve` flag (defaults to `false` when the subscriber holds
/// no tier).
fn status_get_handler() -> RpcHandler {
    Box::new(|state, subscriber_id, payload| {
        Box::pin(async move {
            let req: StatusGetRequest = decode(&payload).map_err(malformed_upload)?;
            let author_id = req.author_id.0;

            let subscribed = state
                .db
                .get_subscribed_tiers(&author_id, &subscriber_id)
                .await
                .map_err(internal)?;

            // Pick the tier to report. The free `followers` tier (reserved rank
            // 0) is the *base* relationship (a follow); a paid tier (rank >= 1)
            // always outranks it for display, so `followers` is reported only
            // when it is the sole tier held. Every paid subscriber also holds
            // `followers` (the rank-0 auto-approve cascade), so without this
            // fallback a paid subscriber's status would collapse to "followers"
            // and their paid `subscription-offer-status` badge would read
            // inactive (`fauna_core::format::offer_status`). Among paid tiers
            // the pick is the HIGHEST rank held, skipping `unlocks_post`
            // -designated tiers entirely — both halves are load-bearing and
            // neither substitutes for the other: a
            // rank-1 fan-out tier wins a lowest-rank pick, and a pay-per-view
            // tier (minted at `max + 1`) wins a highest-rank one. One pin per
            // half in `conformance_unlock_rank_fanout.rs`.
            let mut chosen: Option<(i64, String, bool)> = None;
            let mut followers_fallback: Option<(i64, String, bool)> = None;
            for name in subscribed {
                if let Some(tier) = state
                    .db
                    .get_subscription_tier(&author_id, &name)
                    .await
                    .map_err(internal)?
                {
                    if tier.rank == FOLLOWERS_TIER_RANK {
                        followers_fallback = Some((tier.rank, name, tier.auto_approve));
                        continue;
                    }
                    if tier.unlocks_post.is_some() {
                        continue;
                    }
                    let replace = chosen
                        .as_ref()
                        .map(|(r, _, _)| tier.rank > *r)
                        .unwrap_or(true);
                    if replace {
                        chosen = Some((tier.rank, name, tier.auto_approve));
                    }
                }
            }
            let chosen = chosen.or(followers_fallback);

            // A paid entitlement window surfaces as `expires_at` (micros on
            // the wire; storage is epoch seconds). `None` = no expiry —
            // manual/auto grants, or a payment carrying no window
            // (monetization.md § Pillar 3).
            let mut expires_at = None;
            if let Some((_, name, _)) = chosen.as_ref() {
                expires_at = state
                    .db
                    .get_subscriber_valid_until(&author_id, &subscriber_id, name)
                    .await
                    .map_err(internal)?
                    .map(|secs| Timestamp((secs as u64).saturating_mul(1_000_000)));
            }

            let reply = StatusGetReply {
                tier: chosen.as_ref().map(|(_, n, _)| n.clone()),
                expires_at,
                auto_approve: chosen.map(|(_, _, a)| a).unwrap_or(false),
                extra: Default::default(),
            };
            encode_reply(&reply)
        })
    })
}

/// `fauna.subscriptions.requests.list` — list pending subscribe/unsubscribe
/// requests addressed to the bearer (the author).
fn requests_list_handler() -> RpcHandler {
    Box::new(|state, author_id, payload| {
        Box::pin(async move {
            let _req: RequestsListRequest = decode(&payload).map_err(malformed_upload)?;

            let rows = state
                .db
                .list_subscribe_requests(&author_id)
                .await
                .map_err(internal)?;

            let mut requests = Vec::with_capacity(rows.len());
            for r in rows {
                let sub: [u8; 32] = r
                    .subscriber_id
                    .as_slice()
                    .try_into()
                    .map_err(|_| internal("subscriber_id is not 32 bytes"))?;
                requests.push(PendingRequest {
                    request_id: r.id,
                    subscriber_id: ActorId(sub),
                    tier_name: r.tier_name,
                    kind: r.kind,
                    // SubscribeRequestRow.created_at is epoch seconds; Timestamp is micros.
                    created_at: Timestamp((r.created_at as u64).saturating_mul(1_000_000)),
                    // Surface the subscriber's published ML-KEM ek (S4b) so an
                    // the author wraps hybrid to the brand-new
                    // subscriber before they reach the roster (S4c-2 persisted it
                    // on the `subscribe_requests` row at enqueue time).
                    mlkem_encaps_key: r.mlkem_encaps_key.map(ByteBuf::from),
                    payment_entitled: r.payment_entitled,
                    extra: Default::default(),
                });
            }

            encode_reply(&RequestsListReply {
                requests,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.subscriptions.tiers.list` — the calling author's own subscription
/// tier definitions, ascending by rank. Replay-safe pure read; an actor can
/// always read their own tiers (keyed on the bearer, no request id). The
/// authenticated complement of the public `GET /api/v1/subscriptions/tiers/
/// {author_id}` HTTP read — see `monetization.md` § Pillar 1.
fn tiers_list_handler() -> RpcHandler {
    Box::new(|state, author_id, payload| {
        Box::pin(async move {
            let _req: TiersListRequest = decode(&payload).map_err(malformed_upload)?;

            let rows = state
                .db
                .list_subscription_tiers(&author_id)
                .await
                .map_err(internal)?;

            // The author's OWN read carries every tier, designated ones
            // included: they need their unlock tiers to gate the post and to
            // audit sales. The §1 My-tiers list and the compose gate picker
            // exclude designated tiers client-side off `unlocks_post`
            // (`monetization.md:128`) — the generic *offer* surfaces are the
            // ones filtered nest-side, below.
            let tiers = rows
                .into_iter()
                .map(|t| TierItem {
                    // The asking price rides the AUTHOR's read so their edit
                    // form round-trips it. It is absent from the buyer-facing
                    // offer surfaces below, which keep rendering `price_hint`:
                    // that string is the human price, this number is the
                    // machine one, and the two are independent by design.
                    asking_price: t.asking_price(),
                    name: t.name,
                    rank: t.rank as u32,
                    description: t.description,
                    price_hint: t.price_hint,
                    payment_url: t.payment_url,
                    auto_approve: t.auto_approve,
                    // SubscriptionTierRow.created_at is epoch seconds; Timestamp is micros.
                    created_at: Timestamp((t.created_at as u64).saturating_mul(1_000_000)),
                    unlocks_post: t.unlocks_post,
                    hidden: t.hidden,
                    extra: Default::default(),
                })
                .collect();

            encode_reply(&TiersListReply {
                tiers,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.subscriptions.offers.list` — **another** actor's offered tier
/// definitions, ascending by rank. The subscriber-browse read for the profile
/// Tiers tab when viewing someone else's profile (`profile.md` § Layout & flow;
/// `monetization.md` § Pillar 1 — `subscription-offers-section`). Unlike the
/// bearer-keyed own-read `tiers.list`, this takes the target `author_id` from
/// the request: tier definitions are public (the unauthenticated HTTP read
/// `GET /api/v1/subscriptions/tiers/{author_id}` already serves them to anyone),
/// so any authenticated caller may read any author's — this is the WS-RPC
/// successor for the authenticated in-client browse (priority #2: no per-app
/// HTTP glue). Replay-safe pure read. Reuses the `TiersListReply` shape.
fn offers_list_handler() -> RpcHandler {
    Box::new(|state, _caller_id, payload| {
        Box::pin(async move {
            let req: OffersListRequest = decode(&payload).map_err(malformed_upload)?;

            let rows = state
                .db
                .list_subscription_tiers(&req.author_id.0)
                .await
                .map_err(internal)?;

            // A per-post pay-to-unlock tier is NEVER offered in a generic
            // tier browse (`monetization.md:128`): it is a degenerate tier
            // auto-minted for one post, and its affordance renders on that
            // post (teaser card / detail / the Pillar-2 web teaser page).
            // Listing it here would show a stranger a nameless "tier" they
            // cannot situate — and would let them enumerate an author's
            // for-sale posts from the tier list. The author's own
            // `tiers.list` above is the surface that carries them.
            let tiers = rows
                .into_iter()
                // A hidden tier is never offered (monetization.md § The
                // unifying model — A tier may be hidden): the reserved
                // owner-only tier exists to seal imported content to its
                // author alone, and listing it would offer strangers a tier
                // nobody can join.
                .filter(|t| t.unlocks_post.is_none() && !t.hidden)
                .map(|t| TierItem {
                    // Deliberately NOT carried on the public offer surface:
                    // the asking price is the author's own machine threshold,
                    // and a browsing stranger reads `price_hint`. Withholding
                    // it also keeps a prospective buyer from discovering the
                    // exact number that would auto-grant, which is the
                    // author's business and not part of the offer.
                    asking_price: None,
                    unlocks_post: t.unlocks_post,
                    name: t.name,
                    rank: t.rank as u32,
                    description: t.description,
                    price_hint: t.price_hint,
                    payment_url: t.payment_url,
                    auto_approve: t.auto_approve,
                    // SubscriptionTierRow.created_at is epoch seconds; Timestamp is micros.
                    created_at: Timestamp((t.created_at as u64).saturating_mul(1_000_000)),
                    hidden: t.hidden,
                    extra: Default::default(),
                })
                .collect();

            encode_reply(&TiersListReply {
                tiers,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.subscriptions.post_unlock.get` — the buyer's price read for one
/// sold post (`monetization.md` § Per-post pay-to-unlock → *the buyer's price
/// read is post-addressed*, ruled 2026-07-29). Keyed `(author_id, post_id)`
/// and POST-addressed: `post_id = blake3(body)` is unguessable without having
/// seen the post, so possession of the id is evidence of legitimate teaser
/// access — which is why this read may answer the designated tier's public
/// purchase fields while both generic offer surfaces filter designated tiers
/// out (`offers_list_handler` above). Answers iff one of the author's tiers
/// genuinely **sells** the post — the two-direction binding
/// [`CacheDb::get_tier_selling_post`] owns, so a price is never quoted for a
/// post that is not gated to the tier quoting it; an
/// unsold post, a foreign id, and an unknown author are all the SAME empty
/// reply, so the read confirms nothing a designated post id doesn't already
/// prove (the anti-enumeration property). USER-class, ordinary dispatch limits
/// — an author-keyed indexed point read needs no bespoke throttle. Replay-safe
/// pure read.
fn post_unlock_get_handler() -> RpcHandler {
    Box::new(|state, _caller_id, payload| {
        Box::pin(async move {
            let req: PostUnlockGetRequest = decode(&payload).map_err(malformed_upload)?;
            if !is_post_id(&req.post_id) {
                return Err(malformed_upload("post_id must be a 32-byte post id in hex"));
            }

            let offer = state
                .db
                .get_tier_selling_post(&req.author_id.0, &req.post_id)
                .await
                .map_err(internal)?
                .map(|t| PostUnlockOffer {
                    tier_name: t.name,
                    price_hint: t.price_hint,
                    payment_url: t.payment_url,
                    extra: Default::default(),
                });

            encode_reply(&PostUnlockGetReply {
                offer,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.subscriptions.mine.list` — the *calling* actor's own subscriptions
/// across every creator (active + pending), for the `subscription-settings`
/// consumer page. Caller-scoped (keyed on the bearer, never a request id).
/// Resolves each distinct creator's handle nest-side (the creator is a local
/// account) so the client renders a handle without an N+1 lookup; `None` when
/// the creator has no local handle, and the client falls back to the hex id.
fn mine_list_handler() -> RpcHandler {
    Box::new(|state, subscriber_id, payload| {
        Box::pin(async move {
            let _req: MineListRequest = decode(&payload).map_err(malformed_upload)?;

            let rows = state
                .db
                .list_my_subscriptions(&subscriber_id)
                .await
                .map_err(internal)?;

            // Resolve creator handles, caching per author within this call (a
            // subscriber commonly holds several tiers from one creator).
            let mut handle_cache: std::collections::HashMap<[u8; 32], Option<String>> =
                std::collections::HashMap::new();
            let mut subscriptions = Vec::with_capacity(rows.len());
            for r in rows {
                let author: [u8; 32] = r
                    .author_id
                    .as_slice()
                    .try_into()
                    .map_err(|_| internal("author_id is not 32 bytes"))?;
                let handle = match handle_cache.get(&author) {
                    Some(h) => h.clone(),
                    None => {
                        // Normalize an empty handle (e.g. an admin-provisioned
                        // account that never set one) to `None` so the client
                        // falls back to the hex actor id rather than a blank.
                        let h = state
                            .db
                            .get_handle(&author)
                            .await
                            .map_err(internal)?
                            .filter(|s| !s.is_empty());
                        handle_cache.insert(author, h.clone());
                        h
                    }
                };
                subscriptions.push(MineSubscription {
                    author_id: ActorId(author),
                    tier: r.tier_name,
                    status: r.status,
                    handle,
                    // MySubscriptionRow.since is epoch seconds; Timestamp is micros.
                    since: Timestamp((r.since as u64).saturating_mul(1_000_000)),
                    extra: Default::default(),
                });
            }

            encode_reply(&MineListReply {
                subscriptions,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.subscriptions.key_blob.get` — return the full dag-cbor-encoded KeyBlob
/// for a tier the bearer subscribes to, or that the bearer **authors**: the
/// author minted the blob (its entries are wrapped to subscriber pubkeys, so
/// it holds nothing the author doesn't already have) and reads its
/// content-address as the gated-compose `GatedInfo.key_blob_ref`
/// (`ui/feed.md` § Encryption at rest).
fn key_blob_get_handler() -> RpcHandler {
    Box::new(|state, subscriber_id, payload| {
        Box::pin(async move {
            let req: KeyBlobGetRequest = decode(&payload).map_err(malformed_upload)?;
            let author_id = req.author_id.0;

            if subscriber_id != author_id {
                let subscribed = state
                    .db
                    .is_subscriber(&author_id, &subscriber_id, &req.tier_name)
                    .await
                    .map_err(internal)?;
                if !subscribed {
                    return Err(not_subscribed("not subscribed to this tier"));
                }
            }

            let (version, hash, blob_data) = state
                .db
                .get_current_key_blob(&author_id, &req.tier_name)
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

/// `fauna.subscriptions.subscribers.list` — list active subscribers of a
/// tier owned by the bearer (the author).
fn subscribers_list_handler() -> RpcHandler {
    Box::new(|state, author_id, payload| {
        Box::pin(async move {
            let req: SubscribersListRequest = decode(&payload).map_err(malformed_upload)?;

            let rows = state
                .db
                .list_subscribers(&author_id, &req.tier_name)
                .await
                .map_err(internal)?;

            let mut subscribers = Vec::with_capacity(rows.len());
            for s in rows {
                let sub: [u8; 32] = s
                    .subscriber_id
                    .as_slice()
                    .try_into()
                    .map_err(|_| internal("subscriber_id is not 32 bytes"))?;
                subscribers.push(SubscriberEntry {
                    subscriber_id: ActorId(sub),
                    // SubscriberRow.approved_at is epoch seconds; Timestamp is micros.
                    joined_at: Timestamp((s.approved_at as u64).saturating_mul(1_000_000)),
                    // Surface the subscriber's published ML-KEM ek so an
                    // authors wrap a hybrid X-Wing KeyBlob to it (post-quantum
                    // surface B). `None` for classical-only subscribers.
                    mlkem_encaps_key: s.mlkem_encaps_key.map(ByteBuf::from),
                    extra: Default::default(),
                });
            }

            encode_reply(&SubscribersListReply {
                subscribers,
                extra: Default::default(),
            })
        })
    })
}

// ── write handlers ─────────────────────────────────────────────

/// A post id as it travels the wire: `blake3(body)` — 32 bytes — rendered as
/// 64 lowercase hex chars (`fauna_protocol::posts::PostCreateReply::post_id`).
fn is_post_id(s: &str) -> bool {
    fauna_core::hex32::is_hex64(s)
}

/// Longest denomination tag this nest will store. Generous next to `"msat"`
/// and any plausible successor (an ISO currency code is 3 chars); it exists
/// only so an unknown unit cannot be used to write unbounded text into the
/// tier row.
const MAX_ASKING_PRICE_UNIT_LEN: usize = 32;

/// Turn the wire asking price into the shared comparison type, refusing only
/// what is *structurally* junk.
///
/// **An unknown unit is deliberately NOT refused.** `monetization.md` § The
/// asking price ratifies that a unit this build does not know compares as *not
/// met*, fail-closed — which presupposes the nest stored it. Refusing here
/// would instead make an older nest reject a newer client's tier outright,
/// breaking the bidirectional within-a-major compatibility
/// `version-compatibility.md` requires; the field must round-trip through a
/// nest that cannot interpret it. So the vocabulary check belongs at
/// comparison time (`AskingPrice::is_met_by`) and nowhere else, and this
/// function's whole job is bounding the string and rejecting an empty tag.
///
/// **It validates onto the WIRE type, not the comparison type.** A nest built
/// without the `payments` member has no `fauna_payments::AskingPrice` at all,
/// and must still accept, store and re-serve a price a full client authored
/// (`dynamic-features.md` § Wire-compat posture) — so the storage path speaks
/// the ungated wire type end to end, and only the zap purchase path (inside the
/// `zaps` gate) converts to the comparison type.
///
/// A `value` of `0` is likewise accepted: "any amount in this unit buys it" is
/// a coherent author choice, and it is distinct from the absent price that
/// makes a tier unbuyable by inference.
fn validated_asking_price(
    wire: Option<&fauna_protocol::subscriptions::TierAskingPrice>,
) -> Result<Option<fauna_protocol::subscriptions::TierAskingPrice>, RpcError> {
    let Some(wire) = wire else {
        return Ok(None);
    };
    let unit = wire.unit.trim();
    if unit.is_empty() || unit.len() > MAX_ASKING_PRICE_UNIT_LEN {
        return Err(malformed_upload(
            "asking_price.unit must be a 1-32 character denomination tag",
        ));
    }
    // Stored as sent, not as trimmed: the comparison is exact string equality
    // (` msat` is not `msat`), so silently repairing a padded tag here would
    // make the nest accept a price it then compares differently from the one
    // the author's client believes it set.
    if unit != wire.unit {
        return Err(malformed_upload(
            "asking_price.unit must not carry leading or trailing whitespace",
        ));
    }
    Ok(Some(wire.clone()))
}

/// `fauna.subscriptions.tiers.create` — create a subscription tier owned by
/// the bearer.
///
/// Inserts the tier row and **never mints a period key**: the nest holds no
/// mint authority (`storage-modes.md` § Architectural rules, "Don't have the
/// nest mint … its own grants"; the subscription plane's analogue). The first
/// `KeyBlob` lands via `requests.approve` from the author's client.
fn tiers_create_handler() -> RpcHandler {
    Box::new(|state, author_id, payload| {
        Box::pin(async move {
            let req: TierCreateRequest = decode(&payload).map_err(malformed_upload)?;

            // Input validation — mirrors the HTTP twin's bad_request paths.
            if req.name.is_empty() || req.name.len() > 64 {
                return Err(malformed_upload("tier name must be 1-64 characters"));
            }
            // The reserved followers slot (`monetization.md` § The unifying
            // model): `rank < 1` is refused for every name EXCEPT the reserved
            // `followers` at exactly rank 0 — the idempotent client-side twin of
            // `ensure_followers_tier`, birth blob included, so an author nobody
            // follows yet can gate a post to followers (archive-import slice 3,
            // ruled 2026-09-06). The reserved row's SHAPE is the nest's, not the
            // client's: auto-approve, free, undesignated, offered.
            let reserved_followers =
                req.name == FOLLOWERS_TIER && i64::from(req.rank) == FOLLOWERS_TIER_RANK;
            if req.rank < 1 && !reserved_followers {
                return Err(malformed_upload("rank must be >= 1"));
            }
            // The tier every room-restricted post carries is a constant, never
            // an author's own tier (`ui/feed.md` § Encryption at rest →
            // *Room-restricted — the ruling*, ruling 3): a subscribable tier
            // of that name would put a tier an app could offer on a post whose
            // readers are a room's floor, never a subscription.
            if req.name == fauna_core::subscription::ROOM_POST_TIER {
                return Err(malformed_upload(
                    "\"room\" is reserved for room-restricted posts and is never an author's tier",
                ));
            }
            // The ceiling `OWNER_ONLY_TIER_RANK`'s own doc claims
            // (`libs/fauna-core/src/subscription/mod.rs` — "a paid tier is
            // created at `1 <= rank < u32::MAX`") was, until this check,
            // enforced by nothing: `tiers.create` bounded rank only from
            // below (above). Reserve the top rank the same way rank 0 is
            // reserved to `followers`, so the reserved owner-only tier
            // (`monetization.md` § The unifying model — *A tier may be
            // hidden*) stays the only tier any cascade could ever reach it
            // through, and only by name.
            if req.rank == fauna_core::subscription::OWNER_ONLY_TIER_RANK
                && req.name != fauna_core::subscription::OWNER_ONLY_TIER
            {
                return Err(malformed_upload(
                    "rank must be below the reserved owner-only tier's rank",
                ));
            }
            if reserved_followers {
                // `created` is true only when THIS call provisioned the row:
                // read existence before the idempotent ensure.
                let existed = state
                    .db
                    .get_subscription_tier(&author_id, FOLLOWERS_TIER)
                    .await
                    .map_err(internal)?
                    .is_some();
                ensure_followers_tier(&state, &author_id).await?;
                let created = !existed;
                let had_blob = state
                    .db
                    .get_current_key_blob(&author_id, FOLLOWERS_TIER)
                    .await
                    .map_err(internal)?
                    .is_some();
                if !had_blob {
                    let upload = &req.encrypted_upload;
                    let (key_blob, _signer_auth) =
                        verify_encrypted_upload(author_id, upload, author_id)?;
                    if key_blob.tier != FOLLOWERS_TIER {
                        return Err(tier_mismatch(&format!(
                            "key_blob.tier={:?} but created tier={:?}",
                            key_blob.tier, FOLLOWERS_TIER
                        )));
                    }
                    if !key_blob.entries.is_empty() {
                        return Err(roster_mismatch(0, key_blob.entries.len()));
                    }
                    let stored_bytes = canonical_encode(&upload.key_blob).map_err(internal)?;
                    let blob_hash = blake3::hash(&upload.key_blob.bytes);
                    state
                        .db
                        .upsert_current_key_blob(
                            &author_id,
                            FOLLOWERS_TIER,
                            1,
                            blob_hash.as_bytes(),
                            &stored_bytes,
                        )
                        .await
                        .map_err(internal)?;
                }
                return encode_reply(&TierCreateReply {
                    created,
                    extra: Default::default(),
                });
            }
            // Per-post pay-to-unlock designation: validate the FORMAT only.
            // The post it names does not exist yet — the author's client
            // builds the gated body (which carries this tier's name), takes
            // `post_id = blake3(body)`, creates the tier, and only then posts
            // — so an existence check here could never pass. The tier also
            // deliberately outlives the post (`monetization.md:131`).
            if let Some(post_id) = req.unlocks_post.as_deref()
                && !is_post_id(post_id)
            {
                return Err(malformed_upload(
                    "unlocks_post must be a 32-byte post id in hex",
                ));
            }
            // `hidden` + `unlocks_post` together is refused, not merely
            // unused: ruling 4 makes a hidden tier not subscribable, so a
            // hidden sold-post tier could never be bought by anyone. This
            // refusal is one layer of defense, not the only one — the
            // creation-time fan-out (`enqueue_unlock_fanout_for_new_tier`)
            // additionally checks rule (f) directly, as its own in-consumer
            // guard, since a caller-side refusal alone is a guard
            // the caller must remember, not one the consumer enforces itself.
            if req.hidden && req.unlocks_post.is_some() {
                return Err(malformed_upload(
                    "a tier cannot be both hidden and unlocks_post — a hidden tier is never subscribable, so nothing could ever buy it",
                ));
            }
            let asking_price = match validated_asking_price(req.asking_price.as_ref()) {
                Ok(p) => p,
                Err(e) => return Err(e),
            };

            // **Gate surfaces `payments.tier.price` and
            // `payments.paywall.designate`** (`dynamic-features.md` § Charter
            // members — the sell side's middle two doors). Both are compiled
            // out of a store-safe nest **on purpose**: an excised build still
            // accepts, stores and re-serves a priced tier a full client
            // authored (§ Implementation status today — "what excises is the
            // *comparison*, never the price at rest"), so gating the authoring
            // path there would break the wire-compat invariant instead of
            // enforcing anything — there is no comparison left to gate.
            //
            // A free tier is not a payments operation at all: the plane binds
            // *priced* tier creation, so an ordinary followers/patron tier
            // passes through untouched.
            //
            // A create that both prices a tier and designates it as a post
            // unlock spends on both surfaces. They are separate registry rows
            // because a rule-setter may bind them separately, and one call
            // doing two gated things is two gated things.
            #[cfg(feature = "payments")]
            if asking_price.is_some() || req.payment_url.is_some() {
                crate::feature_gate::gate(
                    &state,
                    &author_id,
                    &fauna_core::feature_gate::GateOp {
                        feature: fauna_core::feature_gate::GatedFeature::Payments,
                        surface: fauna_core::feature_gate::SURFACE_PAYMENTS_TIER_PRICE,
                        // Authoring a price introduces nobody and moves nothing;
                        // the value dimension counts money that actually
                        // changed hands, at the buy-side surfaces.
                        new_counterparties: 0,
                        magnitude: 0,
                    },
                )
                .await?;
            }
            #[cfg(feature = "payments")]
            if req.unlocks_post.is_some() {
                crate::feature_gate::gate(
                    &state,
                    &author_id,
                    &fauna_core::feature_gate::GateOp {
                        feature: fauna_core::feature_gate::GatedFeature::Payments,
                        surface: fauna_core::feature_gate::SURFACE_PAYMENTS_PAYWALL_DESIGNATE,
                        new_counterparties: 0,
                        magnitude: 0,
                    },
                )
                .await?;
            }

            // Birth KeyBlob (`ui/feed.md` § Encryption at rest — broadcast
            // tiers): REQUIRED on the wire, and verified BEFORE the tier row
            // exists, so a refused envelope leaves no keyless tier behind.
            // The same 4-step auth chain as `requests.approve`, then the
            // create-specific state checks — blob tier matches, and the roster
            // is EMPTY (a tier is born with no subscribers; a non-empty birth
            // roster is a client bug or forgery attempt). The nest never mints
            // a period key or an MLS solo group: the author's client minted the
            // tier key and this blob under it.
            let upload = &req.encrypted_upload;
            let (key_blob, _signer_auth) = verify_encrypted_upload(author_id, upload, author_id)?;
            if key_blob.tier != req.name {
                return Err(tier_mismatch(&format!(
                    "key_blob.tier={:?} but created tier={:?}",
                    key_blob.tier, req.name
                )));
            }
            if !key_blob.entries.is_empty() {
                return Err(roster_mismatch(0, key_blob.entries.len()));
            }

            // Tier insert. UNIQUE-constraint violation maps to
            // tier_already_exists, mirroring the HTTP twin's 409.
            if let Err(e) = state
                .db
                .create_subscription_tier(
                    &author_id,
                    &req.name,
                    req.rank as i64,
                    req.description.as_deref(),
                    req.price_hint.as_deref(),
                    req.payment_url.as_deref(),
                    req.auto_approve,
                    req.unlocks_post.as_deref(),
                    asking_price.as_ref(),
                    req.hidden,
                )
                .await
            {
                if format!("{e:?}").contains("UNIQUE constraint") {
                    return Err(tier_already_exists("tier already exists"));
                }
                tracing::error!("tiers.create: create_subscription_tier: {e}");
                return Err(internal(format!("storage error: {e}")));
            }

            // Store the verified birth blob exactly like the approve arm, so
            // `key_blob.get` and the gated-compose `key_blob_ref` see one
            // canonical shape. A brand-new tier has no prior blob (the
            // UNIQUE-constraint check above rejects re-creates), so the birth
            // blob is always version 1.
            let stored_bytes = canonical_encode(&upload.key_blob).map_err(internal)?;
            let blob_hash = blake3::hash(&upload.key_blob.bytes);
            state
                .db
                .upsert_current_key_blob(
                    &author_id,
                    &req.name,
                    1,
                    blob_hash.as_bytes(),
                    &stored_bytes,
                )
                .await
                .map_err(internal)?;

            // Creation-time half of the rank fan-out (`monetization.md:126`):
            // a designated tier is "included in every paid subscription" —
            // existing ones too — so enqueue the author-client mint for every
            // current unexpired subscriber at or above this rank. The
            // author's own drain pump (online right now: it is what called
            // this) picks them up. Errors propagate like the birth-blob arm:
            // the tier row exists either way, and the client surfaces the
            // failed create.
            if req.unlocks_post.is_some() {
                state
                    .db
                    .enqueue_unlock_fanout_for_new_tier(&author_id, &req.name)
                    .await
                    .map_err(internal)?;
            }

            encode_reply(&TierCreateReply {
                created: true,
                extra: Default::default(),
            })
        })
    })
}

// ── write handlers (non-mode-affected) ─────────────────────────

/// `fauna.subscriptions.tiers.update` — update mutable fields of an existing
/// tier owned by the bearer (the author). Mirrors the HTTP twin's
/// `or_else(current)` merge: any `None` field on the request keeps the
/// tier's current value.
///
/// Note: `req.rank` is accepted on the wire but not applied — the DB layer's
/// `update_subscription_tier` does not expose `rank` today. Flag for
/// follow-up if rank-update becomes a product requirement.
fn tiers_update_handler() -> RpcHandler {
    Box::new(|state, author_id, payload| {
        Box::pin(async move {
            let req: TierUpdateRequest = decode(&payload).map_err(malformed_upload)?;

            // Mirror HTTP twin: load current row and use it as the fallback
            // for any field the client left as None.
            let current = state
                .db
                .get_subscription_tier(&author_id, &req.name)
                .await
                .map_err(internal)?
                .ok_or_else(|| tier_not_found("tier not found"))?;

            let desc = req
                .description
                .as_deref()
                .or(current.description.as_deref());
            let price = req.price_hint.as_deref().or(current.price_hint.as_deref());
            let url = req
                .payment_url
                .as_deref()
                .or(current.payment_url.as_deref());
            let auto = req.auto_approve.unwrap_or(current.auto_approve);

            // The per-post pay-to-unlock designation is CREATE-time
            // immutable (`monetization.md:131`): re-pointing a tier people
            // already bought silently changes what they bought, and
            // attaching one after the fact is the same class. `None` keeps
            // the current value like every other field on this handler, and
            // re-sending the value the tier already carries is a no-op accept
            // so an auto-retrying or replaying client is never turned into an
            // error — only an actual *change* is refused.
            if let Some(requested) = req.unlocks_post.as_deref()
                && current.unlocks_post.as_deref() != Some(requested)
            {
                return Err(designation_immutable(
                    current.unlocks_post.as_deref(),
                    requested,
                ));
            }

            // The asking price IS mutable here, and that asymmetry with the
            // designation above is ratified, not an oversight
            // (`monetization.md` § The asking price — Editability): a new
            // price binds *future* events only, so no entitlement already
            // granted changes meaning, whereas re-pointing a sold unlock
            // changes what buyers already bought. `None` keeps the current
            // price, exactly like `desc`/`price`/`url` above — this handler
            // has no clear verb for any of its optional fields.
            let asking = match validated_asking_price(req.asking_price.as_ref()) {
                Ok(Some(p)) => Some(p),
                Ok(None) => current.asking_price(),
                Err(e) => return Err(e),
            };

            // **Gate surface `payments.tier.price`** — the editable half. Only
            // a request that actually *carries* a price is a pricing
            // operation: this handler has no clear verb, so a `None` field
            // means "keep what the tier already has", and re-affirming an
            // existing price by omission is not an act to bind. The
            // `unlocks_post` designation has no update arm to gate — it is
            // create-time immutable above, and a re-send of the current value
            // is a no-op accept.
            #[cfg(feature = "payments")]
            if req.asking_price.is_some() || req.payment_url.is_some() {
                crate::feature_gate::gate(
                    &state,
                    &author_id,
                    &fauna_core::feature_gate::GateOp {
                        feature: fauna_core::feature_gate::GatedFeature::Payments,
                        surface: fauna_core::feature_gate::SURFACE_PAYMENTS_TIER_PRICE,
                        new_counterparties: 0,
                        magnitude: 0,
                    },
                )
                .await?;
            }

            // rank updates not yet wired in the DB layer (the HTTP twin that
            // also skipped them has been deleted).
            let updated = state
                .db
                .update_subscription_tier(
                    &author_id,
                    &req.name,
                    desc,
                    price,
                    url,
                    auto,
                    asking.as_ref(),
                )
                .await
                .map_err(internal)?;
            if !updated {
                return Err(tier_not_found("tier not found"));
            }
            encode_reply(&TierUpdateReply {
                updated: true,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.subscriptions.tiers.clear_field` — clear one optional field of a
/// tier owned by the bearer back to unset (`monetization.md` § The asking
/// price — *Editability* / `TierClearableField`'s doc
/// comment in `fauna-protocol`). `tiers.update`'s `None` means "keep the
/// current value" for `description`/`price_hint`/`payment_url`/`asking_price`
/// alike, so it has no way to wipe one back to unset — this dedicated kind is
/// that missing verb.
fn tiers_clear_field_handler() -> RpcHandler {
    Box::new(|state, author_id, payload| {
        Box::pin(async move {
            let req: TierClearFieldRequest = decode(&payload).map_err(malformed_upload)?;

            let current = state
                .db
                .get_subscription_tier(&author_id, &req.name)
                .await
                .map_err(internal)?
                .ok_or_else(|| tier_not_found("tier not found"))?;

            // `update_subscription_tier` writes every argument unconditionally
            // (its own contract — see its doc comment), so "keep" is spelled
            // by re-passing the current value and "clear" by passing `None`
            // for exactly the targeted field.
            let desc = (req.field != TierClearableField::Description)
                .then_some(current.description.as_deref())
                .flatten();
            let price = (req.field != TierClearableField::PriceHint)
                .then_some(current.price_hint.as_deref())
                .flatten();
            let url = (req.field != TierClearableField::PaymentUrl)
                .then_some(current.payment_url.as_deref())
                .flatten();
            let asking = (req.field != TierClearableField::AskingPrice)
                .then(|| current.asking_price())
                .flatten();

            let updated = state
                .db
                .update_subscription_tier(
                    &author_id,
                    &req.name,
                    desc,
                    price,
                    url,
                    current.auto_approve,
                    asking.as_ref(),
                )
                .await
                .map_err(internal)?;
            if !updated {
                return Err(tier_not_found("tier not found"));
            }
            encode_reply(&TierClearFieldReply {
                cleared: true,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.subscriptions.tiers.delete` — delete a tier owned by the bearer.
/// The DB layer rejects deletion when active subscribers exist (surfaces as
/// `fauna.protocol.internal` with the storage-layer message; HTTP twin
/// reshapes this as 409 conflict, but the WS-RPC error namespace doesn't
/// have a `tier_has_subscribers` code today — using `internal` mirrors the
/// strict behavior).
fn tiers_delete_handler() -> RpcHandler {
    Box::new(|state, author_id, payload| {
        Box::pin(async move {
            let req: TierDeleteRequest = decode(&payload).map_err(malformed_upload)?;

            let deleted = state
                .db
                .delete_subscription_tier(&author_id, &req.name)
                .await
                .map_err(internal)?;
            if !deleted {
                return Err(tier_not_found("tier not found"));
            }
            encode_reply(&TierDeleteReply {
                deleted: true,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.subscriptions.requests.reject` — reject a pending subscribe or
/// unsubscribe request. The bearer must be the author the request is
/// addressed to.
fn requests_reject_handler() -> RpcHandler {
    Box::new(|state, author_id, payload| {
        Box::pin(async move {
            let req: RejectRequestRequest = decode(&payload).map_err(malformed_upload)?;

            let (req_author, _req_subscriber, _tier_name, _kind, _ek) = state
                .db
                .get_subscribe_request(req.request_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| request_not_found("request not found"))?;

            let req_author_arr: [u8; 32] = req_author
                .as_slice()
                .try_into()
                .map_err(|_| internal("subscribe_request.author_id is not 32 bytes"))?;
            if req_author_arr != author_id {
                return Err(permission_denied("request belongs to another author"));
            }

            // HTTP twin ignores the delete result (best-effort cleanup); we
            // do the same — the auth check is the load-bearing step.
            let _ = state.db.delete_subscribe_request(req.request_id).await;

            encode_reply(&RejectRequestReply {
                rejected: true,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.subscriptions.delegate.upload` — store a dag-cbor-encoded
/// `DeviceAuthorization` minted by the bearer. 1:1 port of the HTTP twin
/// (`upload_delegation`); all four checks fire:
///
/// 1. Bearer is the `actor_id` of the authorization (only the actor can
///    upload their own delegation).
/// 2. `auth.device_key == nest_pubkey` — the delegation authorizes *this
///    nest's* signing key, not some arbitrary device.
/// 3. `auth.capabilities` contains `ManageSubscribers` (or `All`) — without
///    it the delegation can't be used for subscription management.
/// 4. The embedded signature verifies under the actor's public key.
fn delegate_upload_handler() -> RpcHandler {
    Box::new(|state, bearer_actor_id, payload| {
        Box::pin(async move {
            let req: DelegateUploadRequest = decode(&payload).map_err(malformed_upload)?;

            let (auth_bytes, auth_env) = req
                .authorization
                .clone()
                .into_signed()
                .map_err(|e| malformed_upload(format!("authorization envelope: {e}")))?;
            let auth: DeviceAuthorization = decode_signed_bytes(&auth_bytes)
                .map_err(|e| malformed_upload(format!("authorization: {e}")))?;

            if auth.actor_id.0 != bearer_actor_id {
                return Err(permission_denied("can only upload your own delegation"));
            }

            // Nest-key check: the delegation must authorize this nest's own
            // signing key. Pull the nest's verifying key from state and
            // compare bytewise to `auth.device_key`.
            let nest_key = state
                .nest_signing_key
                .as_ref()
                .ok_or_else(|| internal("nest keypair not initialized"))?;
            let nest_pubkey = nest_key.verifying_key().to_bytes();
            if auth.device_key != nest_pubkey {
                return Err(invalid_delegation(
                    "device_key does not match nest public key",
                ));
            }

            // Capability check: `ManageSubscribers` (or `All`) must be present.
            let has_manage = auth.capabilities.iter().any(|c| {
                matches!(
                    c,
                    fauna_core::data::Capability::ManageSubscribers
                        | fauna_core::data::Capability::All
                )
            });
            if !has_manage {
                return Err(invalid_delegation("ManageSubscribers capability required"));
            }

            if verify_envelope(&auth, &auth_bytes, &auth_env).is_err() {
                return Err(invalid_signature("delegation signature invalid"));
            }

            // Store the verified canonical bytes — receivers re-verify by
            // recomputing the CID from these bytes against the envelope.
            state
                .db
                .upsert_device_authorization(&bearer_actor_id, &auth.device_key, &auth_bytes)
                .await
                .map_err(internal)?;

            encode_reply(&DelegateUploadReply {
                uploaded: true,
                extra: Default::default(),
            })
        })
    })
}

// ── subscribe / unsubscribe ────────────────────────────────────

/// Lazily provision the reserved free [`FOLLOWERS_TIER`] for `author_id` on the
/// first follow, so `subscribe(author, "followers")` succeeds for any author
/// with no pre-created tier (follow = subscribe to the free tier;
/// `monetization.md` § Pillar 1). Only the reserved name reaches this, by two
/// doors: the first follow, and `tiers.create`'s reserved-name arm — the
/// slice-3 relaxation (`monetization.md` § The unifying model, the rank-0
/// paragraph, ruled 2026-09-06), which lets the author's own client provision
/// the tier *and its birth `KeyBlob`* before any follower exists. Both doors
/// land the same row, so a create after a follow (or the reverse) answers
/// `created: false` rather than conflicting. `tiers.create` still rejects
/// `rank < 1` under every OTHER name, which is what keeps
/// [`FOLLOWERS_TIER_RANK`] reserved.
///
/// The row is a normal tier ([`FOLLOWERS_TIER_RANK`] = 0, `auto_approve`, free),
/// so every downstream path (cascade, roster, `KeyBlob`, offers filter, status)
/// treats it uniformly — no per-path special-casing. Sitting at rank 0 (below
/// every paid tier), it is pulled into every paid subscription's at-or-below
/// rank fan-out (`enqueue_unlock_fanout`), so a paid subscriber is a follower
/// too.
///
/// Idempotent + concurrency-safe: a racing follow that loses the `INSERT` sees
/// the `UNIQUE` violation and treats it as success. **The row is all that is
/// written** — the nest cannot mint the period key, so this tier has no birth
/// blob: the follow enqueues and the author's client uploads the tier's FIRST
/// `KeyBlob` on approval (the one blob-less upload the precheck accepts,
/// [`FirstBlob::FollowersFirstApproval`]).
async fn ensure_followers_tier(
    state: &Arc<AppState>,
    author_id: &[u8; 32],
) -> Result<(), RpcError> {
    // Fast path: already provisioned (the common case after the first follow).
    if state
        .db
        .get_subscription_tier(author_id, FOLLOWERS_TIER)
        .await
        .map_err(internal)?
        .is_some()
    {
        return Ok(());
    }

    // Insert the reserved rank-0 free tier. A concurrent follow may win the
    // race; a UNIQUE violation means the row now exists — treat as success.
    match state
        .db
        .create_subscription_tier(
            author_id,
            FOLLOWERS_TIER,
            FOLLOWERS_TIER_RANK,
            None,  // description
            None,  // price_hint — free
            None,  // payment_url — free
            true,  // auto_approve — following needs no author approval
            None,  // unlocks_post — the followers tier sells no post
            None,  // asking_price — following is free, so nothing may buy it
            false, // hidden — the followers tier is an ordinary offer
        )
        .await
    {
        Ok(()) => {}
        Err(e) if format!("{e:?}").contains("UNIQUE constraint") => return Ok(()),
        Err(e) => {
            tracing::error!("ensure_followers_tier: create_subscription_tier: {e}");
            return Err(internal(format!("provision followers tier: {e}")));
        }
    }

    Ok(())
}

/// `fauna.subscriptions.subscribe` — subscriber-initiated subscribe to a
/// tier.
///
/// Idempotent on already-subscribed (→ `Approved`) and on pending-request
/// (→ `Queued { request_id }` of the existing row).
///
/// A new subscription always inserts a `kind='subscribe'` row and returns
/// `Queued`, `auto_approve` or not: the nest holds no tier key, so the author's
/// client picks the row up via `requests.list` and commits it via
/// `requests.approve` with the `encrypted_upload` envelope — only the client
/// can wrap the tier's key. `auto_approve` tells that client to approve with
/// no author judgment.
fn subscribe_handler() -> RpcHandler {
    Box::new(|state, subscriber_id, payload| {
        Box::pin(async move {
            let req: SubscribeRequest = decode(&payload).map_err(malformed_upload)?;
            let author_id = req.author_id.0;

            // The client half already refuses an author subscribing to
            // their own tier (`FeedManager::is_local_actor`), but
            // the nest's own subscribe door never checked — so any non-Fauna or
            // non-conforming client could still put the author in
            // their own roster (a `post-unlock-*` tier included). Refused BEFORE
            // the followers-tier auto-provision below, so a self-follow does not
            // even lazily mint that tier for a tier-less author.
            if subscriber_id == author_id {
                return Err(self_subscribe_refused("cannot subscribe to your own tier"));
            }

            // Subscriber's optional published ML-KEM ek (post-quantum surface B).
            // If present it must be exactly 1184 bytes (ML-KEM-768), mirroring the
            // mail leg-A `provision_recipient_mls_pubkey` length gate. It rides to
            // the pending `subscribe_requests` row, so the author client's
            // KeyBlob wrap selects the hybrid X-Wing suite for this subscriber.
            let mlkem_ek: Option<&[u8]> = req.mlkem_encaps_key.as_ref().map(|b| b.as_ref());
            if let Some(ek) = mlkem_ek
                && ek.len() != MLKEM768_ENCAPS_KEY_LEN
            {
                return Err(malformed_upload(
                    "mlkem_encaps_key must be 1184 bytes (ML-KEM-768)",
                ));
            }

            // Follow = subscribe to the free "followers" tier. The nest
            // auto-provisions it lazily on the first follow so it exists for any
            // author with no pre-created tier (monetization.md § Pillar 1); the
            // tier lookup below then finds it. All other tier names must be
            // author-created.
            //
            // Only for an author this nest hosts: the caller
            // names any 32-byte `author_id`, and `subscription_tiers` has no
            // foreign key to `users`, so an unchecked provision would let a
            // loop over fresh ids grow it — and `subscribe_requests`, which no
            // author ever drains — without bound. An unhosted author gets the
            // same `tier_not_found` a missing tier gets, and nothing is written.
            if req.tier == FOLLOWERS_TIER {
                let hosted = state
                    .db
                    .get_user(&author_id)
                    .await
                    .map_err(internal)?
                    .is_some();
                if !hosted {
                    return Err(tier_not_found("tier not found"));
                }
                ensure_followers_tier(&state, &author_id).await?;
            }

            // Tier existence is always checked — the enqueue path also needs a
            // real tier_name to write into subscribe_requests (foreign key on
            // subscription_tiers).
            let tier = state
                .db
                .get_subscription_tier(&author_id, &req.tier)
                .await
                .map_err(internal)?
                .ok_or_else(|| tier_not_found("tier not found"))?;

            // Ruling 4 (`monetization.md` § The unifying model → *A tier may
            // be hidden*): hidden means not offered AND not subscribable. The
            // refusal is the same code a nonexistent tier gets, so a stranger
            // who guesses the reserved name learns nothing, and no pending
            // request can ever land for the author to approve by accident.
            if tier.hidden {
                return Err(tier_not_found("tier not found"));
            }

            // Idempotency: if already subscribed, return Approved rather than
            // enqueue duplicate accept work for the author's client.
            if state
                .db
                .is_subscriber(&author_id, &subscriber_id, &req.tier)
                .await
                .map_err(internal)?
            {
                // Post-quantum surface-B upgrade (SUB-1): an already-subscribed
                // subscriber re-subscribing with a freshly published ek upgrades
                // classical → hybrid across all their tiers under this author (the
                // ek is identity-derived — one per subscriber). Without this the
                // idempotent return drops the published ek and the author keeps
                // wrapping classical (HNDL-exposed) entries forever; the author's
                // next rotation now re-wraps an X-Wing entry off the stored ek.
                if let Some(ek) = mlkem_ek {
                    state
                        .db
                        .update_subscriber_mlkem_ek(&author_id, &subscriber_id, ek)
                        .await
                        .map_err(internal)?;
                }
                // Self-heal: an already-held grant re-checks its rank fan-out
                // (`monetization.md:126`) — usually a no-op, but it lets a
                // subscriber whose fan-out enqueue was ever missed recover it
                // by re-subscribing. Best-effort like the enqueue proper.
                if let Err(e) = state
                    .db
                    .enqueue_unlock_fanout(&author_id, &subscriber_id, &req.tier)
                    .await
                {
                    tracing::error!("subscribe: unlock fan-out re-check({}): {e}", req.tier);
                }
                return encode_reply(&SubscribeReply::Approved {
                    tier: req.tier.clone(),
                    expires_at: None,
                });
            }

            // Idempotency: if a pending request exists for this
            // (author, subscriber, tier), return Queued with the existing
            // row's id rather than inserting a duplicate.
            if state
                .db
                .has_pending_subscribe_request(&author_id, &subscriber_id, &req.tier)
                .await
                .map_err(internal)?
            {
                // Post-quantum surface-B upgrade (SUB-1, enqueue twin): an enqueued
                // but not-yet-approved subscriber who re-subscribes carrying a
                // freshly published ek upgrades their pending request row(s), so the
                // ek survives the enqueue → approve gap and the eventual approval
                // wraps an X-Wing (not classical) entry. Without this the idempotent
                // Queued return drops the published ek.
                if let Some(ek) = mlkem_ek {
                    state
                        .db
                        .update_subscribe_request_mlkem_ek(&author_id, &subscriber_id, ek)
                        .await
                        .map_err(internal)?;
                }
                let rows = state
                    .db
                    .list_subscribe_requests(&author_id)
                    .await
                    .map_err(internal)?;
                let existing_id = rows
                    .into_iter()
                    .find(|r| {
                        r.kind == "subscribe"
                            && r.tier_name == req.tier
                            && r.subscriber_id.as_slice() == subscriber_id.as_slice()
                    })
                    .map(|r| r.id)
                    .ok_or_else(|| internal("has_pending says yes but list_requests is empty"))?;
                return encode_reply(&SubscribeReply::Queued {
                    request_id: existing_id,
                });
            }

            // Enqueue for the author's client (carry the ek to approval).
            let request_id = state
                .db
                .insert_subscribe_request(
                    &author_id,
                    &subscriber_id,
                    &req.tier,
                    "subscribe",
                    mlkem_ek,
                )
                .await
                .map_err(internal)?;
            encode_reply(&SubscribeReply::Queued { request_id })
        })
    })
}

/// `fauna.subscriptions.unsubscribe` — subscriber-initiated unsubscribe from an
/// author's **paid** tiers.
///
/// **Scope: rank ≥ 1 only — the free rank-0 `followers` tier is never touched.**
/// Cancelling a paid subscription must not silently unfollow; following is a
/// separate act with its own affordance (`monetization.md` § Pillar 1 — follow =
/// subscribe to the free tier). This preserves the retired *plaintext* arm's
/// scope (`remove_subscriber_from_tiers_at_or_above(.., min_rank = 1)`). The
/// retired *encrypted* arm enqueued a row for **every** held tier, the free one
/// included, so an unsubscribe there also unfollowed — a latent bug the mode
/// collapse surfaced, resolved onto the richer arm rather than replicated
/// (priority #4).
///
/// Each in-scope tier gets a `kind='unsubscribe'` row: the removal rotation
/// needs the author's client, so the subscriber stays in the roster until it
/// commits the removal via `subscribers.remove`.
///
/// Reply is `Queued` with the FIRST enqueued row's id (the rest are
/// discoverable via `requests.list`). No paid tiers held →
/// `fauna.subscriptions.not_subscribed`. `UnsubscribeReply::Removed` stays on
/// the wire as a variant this nest no longer answers.
fn unsubscribe_handler() -> RpcHandler {
    Box::new(|state, subscriber_id, payload| {
        Box::pin(async move {
            let req: UnsubscribeRequest = decode(&payload).map_err(malformed_upload)?;
            let author_id = req.author_id.0;

            // Held tiers, narrowed to rank >= 1 (the scope note above).
            let held = state
                .db
                .get_subscribed_tiers(&author_id, &subscriber_id)
                .await
                .map_err(internal)?;
            let mut tiers: Vec<String> = Vec::new();
            for name in held {
                let rank = state
                    .db
                    .get_subscription_tier(&author_id, &name)
                    .await
                    .map_err(internal)?
                    .map(|t| t.rank)
                    .unwrap_or(FOLLOWERS_TIER_RANK);
                if rank >= 1 {
                    tiers.push(name);
                }
            }
            if tiers.is_empty() {
                return Err(not_subscribed(
                    "no active subscription under this author to unsubscribe from",
                ));
            }

            let mut first_queued: Option<i64> = None;
            for tier_name in &tiers {
                // The author's client owns the removal rotation.
                let id = state
                    .db
                    .insert_subscribe_request(
                        &author_id,
                        &subscriber_id,
                        tier_name,
                        "unsubscribe",
                        None, // unsubscribe carries no published ek
                    )
                    .await
                    .map_err(internal)?;
                first_queued.get_or_insert(id);
            }

            // `tiers` is non-empty (checked above), so a row was enqueued.
            let request_id =
                first_queued.ok_or_else(|| internal("unsubscribe enqueued no request"))?;
            encode_reply(&UnsubscribeReply::Queued { request_id })
        })
    })
}

// ── requests.approve / subscribers.remove ──────────────────────

/// Approval: verify the client-minted KeyBlob (auth chain via
/// `verify_encrypted_upload`, then state checks tier-exists/matches, a stored
/// blob exists, rotated_at monotonicity, roster shape), mutate `subscribers`,
/// store the blob bytes at the bumped version, delete the request row.
async fn encrypted_approve(
    state: &Arc<AppState>,
    author_id: &[u8; 32],
    subscriber_id: &[u8; 32],
    tier_name: &str,
    request_id: i64,
    upload: &EncryptedKeyBlobUpload,
    mlkem_encaps_key: Option<&[u8]>,
) -> Result<bytes::Bytes, RpcError> {
    let checked = verify_and_precheck_upload(
        state,
        author_id,
        tier_name,
        upload,
        FirstBlob::FollowersFirstApproval,
    )
    .await?;

    // Roster shape: current_subscribers ∪ {subscriber}.
    let mut expected = current_roster(state, author_id, tier_name).await?;
    expected.insert(*subscriber_id);
    require_roster_shape(&expected, &checked.key_blob)?;

    // Mutation: insert subscriber (carrying their published ML-KEM ek so future
    // author-side rotations re-wrap a hybrid entry — post-quantum surface B),
    // store the verified upload, delete request row.
    state
        .db
        .add_subscriber(author_id, subscriber_id, tier_name, mlkem_encaps_key)
        .await
        .map_err(internal)?;
    store_uploaded_blob(state, author_id, tier_name, upload, checked.next_version).await?;
    let _ = state.db.delete_subscribe_request(request_id).await;

    encode_reply(&ApproveRequestReply {
        subscriber: ActorId(*subscriber_id),
        tier: tier_name.to_string(),
        key_version: checked.next_version as u64,
        extra: Default::default(),
    })
}

/// `fauna.subscriptions.requests.approve` — author approves a pending
/// subscribe request:
///
/// - The upload is required (`fauna.subscriptions.missing_upload` without
///   one): verify the client-minted KeyBlob, then accept it as-is.
/// - The request's tier is `hidden` → `fauna.subscriptions.tier_not_found`
///   (`monetization.md` § The unifying model → *A tier may be hidden*,
///   ruling 4).
fn requests_approve_handler() -> RpcHandler {
    Box::new(|state, author_id, payload| {
        Box::pin(async move {
            let req: ApproveRequestRequest = decode(&payload).map_err(malformed_upload)?;

            let (req_author, req_subscriber, tier_name, kind, req_mlkem_ek) = state
                .db
                .get_subscribe_request(req.request_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| request_not_found("request not found"))?;

            // An `unsubscribe` row cannot be approve-minted: the upload's
            // KeyBlob covers the roster INCLUDING the leaver, so accepting it
            // here would silently cancel the leave. The commit path for a
            // queued leave is the author client's removal rotation
            // (`subscribers.remove`) — the author pump drives it with no
            // judgment (monetization.md § Pillar 1, the unsubscribe-commit
            // rule; the shared orchestration refuses the same misroute
            // client-side with `AuthorError::WrongRequestKind`).
            if kind != "subscribe" {
                return Err(wrong_request_kind(
                    "only a subscribe request can be approved; a queued \
                     unsubscribe is committed by the author client's removal \
                     rotation (subscribers.remove), never approve-minted",
                ));
            }

            let req_author_arr: [u8; 32] = req_author
                .as_slice()
                .try_into()
                .map_err(|_| internal("subscribe_request.author_id is not 32 bytes"))?;
            if req_author_arr != author_id {
                return Err(permission_denied("request belongs to another author"));
            }
            let subscriber_id: [u8; 32] = req_subscriber
                .as_slice()
                .try_into()
                .map_err(|_| internal("subscribe_request.subscriber_id is not 32 bytes"))?;

            // Read the request's verified-payment window BEFORE the approve
            // deletes the row; a payment-entitled approval stamps it on the
            // subscribers row afterwards (monetization.md § Pillar 3 —
            // expiry self-heals via valid_until).
            let payment_window = state
                .db
                .get_request_payment_window(req.request_id)
                .await
                .map_err(internal)?
                .and_then(|(entitled, valid_until)| entitled.then_some(valid_until));

            // Ruling 4 (`monetization.md` § The unifying model → *A tier may
            // be hidden*): hidden means not offered AND not subscribable, so
            // no request against one may ever be approved either — the same
            // refusal the `subscribe` door gives, checked here ahead of the
            // upload's verification. This is the enqueue-side guards' backstop: a request
            // can only reach this row if the `subscribe`/
            // `tiers.create` doors let a hidden-tier request through (a future
            // regression at those doors, or a non-conforming path).
            let tier = state
                .db
                .get_subscription_tier(&author_id, &tier_name)
                .await
                .map_err(internal)?
                .ok_or_else(|| tier_not_found("tier not found"))?;
            if tier.hidden {
                return Err(tier_not_found("tier not found"));
            }

            let upload = req.encrypted_upload.ok_or_else(missing_upload)?;
            let reply = encrypted_approve(
                &state,
                &author_id,
                &subscriber_id,
                &tier_name,
                req.request_id,
                &upload,
                req_mlkem_ek.as_deref(),
            )
            .await?;

            // Approval succeeded (add_subscriber resets valid_until to NULL);
            // a paid request re-stamps its window on the requested tier only.
            if let Some(valid_until) = payment_window {
                state
                    .db
                    .set_subscriber_valid_until(&author_id, &subscriber_id, &tier_name, valid_until)
                    .await
                    .map_err(internal)?;
            }

            // Grant-time rank fan-out (`monetization.md:126`, delivery ruled
            // 2026-07-29): the entitlement just landed, so enqueue the
            // author-client mint for every `unlocks_post`-designated tier the
            // cascade includes at the granted rank (no-op when the granted
            // tier is itself designated). Best-effort — the approve has
            // landed and must not be un-reported; a missed enqueue heals at
            // the next boot reconcile (`reconcile_unlock_fanout_requests`) or
            // the subscriber's idempotent re-subscribe.
            if let Err(e) = state
                .db
                .enqueue_unlock_fanout(&author_id, &subscriber_id, &tier_name)
                .await
            {
                tracing::error!("requests.approve: unlock fan-out enqueue({tier_name}): {e}");
            }
            Ok(reply)
        })
    })
}

/// Subscriber removal: verify the client-minted KeyBlob (auth chain + state
/// checks; roster check subtracts the removed subscriber instead of adding
/// one), mutate just this tier's `subscribers` row, store the blob at the
/// bumped version.
async fn encrypted_remove(
    state: &Arc<AppState>,
    author_id: &[u8; 32],
    subscriber_id: &[u8; 32],
    tier_name: &str,
    upload: &EncryptedKeyBlobUpload,
) -> Result<bytes::Bytes, RpcError> {
    let checked =
        verify_and_precheck_upload(state, author_id, tier_name, upload, FirstBlob::Refused).await?;

    // Roster shape: current_subscribers \ {subscriber}.
    let mut expected = current_roster(state, author_id, tier_name).await?;
    expected.remove(subscriber_id);
    require_roster_shape(&expected, &checked.key_blob)?;

    // Mutation: remove from just this tier; the remove is tier-scoped (the
    // author's client uploads one blob per tier to fan a cascade out).
    let removed = state
        .db
        .remove_subscriber(author_id, subscriber_id, tier_name)
        .await
        .map_err(internal)?;
    if !removed {
        return Err(not_subscribed("subscriber not in this tier"));
    }
    store_uploaded_blob(state, author_id, tier_name, upload, checked.next_version).await?;

    encode_reply(&RemoveSubscriberReply {
        subscriber: ActorId(*subscriber_id),
        tier: tier_name.to_string(),
        key_version: checked.next_version as u64,
        extra: Default::default(),
    })
}

/// `fauna.subscriptions.subscribers.remove` — author-initiated removal of a
/// subscriber from a tier. The upload is required, as for `requests.approve`.
fn subscribers_remove_handler() -> RpcHandler {
    Box::new(|state, author_id, payload| {
        Box::pin(async move {
            let req: RemoveSubscriberRequest = decode(&payload).map_err(malformed_upload)?;
            let subscriber_id = req.subscriber_id.0;
            let tier_name = req.tier_name;

            let upload = req.encrypted_upload.ok_or_else(missing_upload)?;
            encrypted_remove(&state, &author_id, &subscriber_id, &tier_name, &upload).await
        })
    })
}

/// `fauna.subscriptions.key_blob.rotate` — republish a tier's broadcast
/// `KeyBlob` under a fresh period key, roster untouched.
///
/// **Why the third upload door exists.** The other two land a blob as the
/// *by-product* of a roster change, so an author whose membership is stable had
/// no way to re-key at all — and that is exactly the post-succession case:
/// a seed thief read the author's period keys, so the live period key is
/// compromised while every subscriber is still legitimately entitled
/// (`succession-aftermath.md` § Re-key scope, the tier row).
///
/// Every refusal is its siblings' (`verify_and_precheck_upload`), plus
/// the roster check that makes this door what it is: entries must equal the
/// **unchanged** roster, so a rotation can neither quietly drop a paying
/// subscriber nor admit someone who was never approved.
fn key_blob_rotate_handler() -> RpcHandler {
    Box::new(|state, author_id, payload| {
        Box::pin(async move {
            let req: RotateKeyBlobRequest = decode(&payload).map_err(malformed_upload)?;
            let tier_name = req.tier_name;

            let upload = req.encrypted_upload;
            let checked = verify_and_precheck_upload(
                &state,
                &author_id,
                &tier_name,
                &upload,
                FirstBlob::Refused,
            )
            .await?;

            // Roster shape: exactly current_subscribers.
            let expected = current_roster(&state, &author_id, &tier_name).await?;
            require_roster_shape(&expected, &checked.key_blob)?;

            store_uploaded_blob(
                &state,
                &author_id,
                &tier_name,
                &upload,
                checked.next_version,
            )
            .await?;

            encode_reply(&RotateKeyBlobReply {
                tier: tier_name,
                key_version: checked.next_version as u64,
                extra: Default::default(),
            })
        })
    })
}

// ── handler registration ───────────────────────────────────────

/// Register all fauna.subscriptions.* WS-RPC handlers on the builder.
///
/// Read-only handlers (Task 7a) are wired up; write handlers
/// (subscribe/unsubscribe, tiers.create/update/delete, requests.approve/
/// reject, subscribers.remove, delegate.upload) land in Tasks 7b–10.
///
/// # `forbid_replay` — the money family, audited 2026-08-01
///
/// The criterion, stated once here and referenced by the per-kind notes below
/// and by `payment_handlers::register_payment_handlers`: under `transport.md`
/// § Idempotency and reconnect-with-resume, `forbid_replay: false` is an
/// **assertion that the handler is naturally idempotent** under a repeated
/// same-key call — never a claim that the nest's idempotency cache will catch
/// a double-apply. That cache is per-`RpcConnection`, and `request_auto_retry`
/// waits for the *reconnect* before re-issuing, so the retry always lands on a
/// fresh connection with an empty cache and the handler runs again for real.
///
/// Every mutating kind in this family was read to its writes for this pass.
/// The audit turned up one recurring shape worth naming, because it decides
/// several of the notes below: **the consume-shaped kinds converge in state but
/// diverge in reply.** `tiers.delete`, `requests.reject`, `requests.approve`
/// and the encrypted `subscribers.remove` each consume the thing they are
/// keyed on (a tier row, a request row, a rotation generation), so a replay
/// finds it gone and answers `tier_not_found` / `request_not_found` /
/// `stale_rotation` where the first call answered success. That is a
/// misleading answer, never a double-apply — and it is precisely the case the
/// idempotency cache used to be credited with covering. Per the precedent set
/// at `fauna.backup.nest_key.revoke`, the fix when a divergent reply starts to
/// matter is **a lookup, not `forbid_replay: true`**: flipping the flag would
/// only push the ambiguity onto the application, while leaving the state that
/// already converged unexplained. `unsubscribe` is where that lookup was owed
/// and is now paid (see its note).
pub fn register_subscription_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.subscriptions.status.get",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: status_get_handler(),
        },
    );
    b.add(
        "fauna.subscriptions.requests.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: requests_list_handler(),
        },
    );
    b.add(
        "fauna.subscriptions.tiers.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: tiers_list_handler(),
        },
    );
    b.add(
        "fauna.subscriptions.offers.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: offers_list_handler(),
        },
    );
    b.add(
        "fauna.subscriptions.post_unlock.get",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: post_unlock_get_handler(),
        },
    );
    b.add(
        "fauna.subscriptions.mine.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: mine_list_handler(),
        },
    );
    b.add(
        "fauna.subscriptions.key_blob.get",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: key_blob_get_handler(),
        },
    );
    b.add(
        "fauna.subscriptions.subscribers.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: subscribers_list_handler(),
        },
    );
    // `forbid_replay: false` audited 2026-08-01. The tier row is keyed
    // `UNIQUE(author_id, name)`, so a replay never inserts a second one — it is
    // refused with `tier_already_exists`, and that refusal is what makes the
    // two side effects below unreachable on a repeat. Both are keyed anyway:
    // the birth `KeyBlob` is an `upsert_current_key_blob` at version 1, and the
    // designated-tier fan-out is `enqueue_unlock_fanout_for_new_tier`, an
    // `ON CONFLICT … DO UPDATE` per candidate subscriber.
    b.add(
        "fauna.subscriptions.tiers.create",
        RpcKindMeta {
            forbid_replay: false,
            // Headroom matched to the upload doors; the client-side registry
            // mirrors it (kind-registry parity).
            default_deadline: Duration::from_secs(15),
            handler: tiers_create_handler(),
        },
    );
    // `forbid_replay: false` audited 2026-08-01. A merge-with-current `UPDATE`
    // keyed on `(author_id, name)`: the handler reads the row and uses it as
    // the fallback for every `None` field, so a repeat writes the values it
    // just wrote. Absolute, not relative — no accumulator, no toggle.
    b.add(
        "fauna.subscriptions.tiers.update",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: tiers_update_handler(),
        },
    );
    // `forbid_replay: false`: a keyed `UPDATE` writing the same clear on
    // every call for a given (tier, field) pair — a repeat is a no-op write
    // of the value the row already holds.
    b.add(
        "fauna.subscriptions.tiers.clear_field",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: tiers_clear_field_handler(),
        },
    );
    // `forbid_replay: false` audited 2026-08-01. Keyed delete, and the DB layer
    // refuses to drop a tier with active subscribers, so the destructive reach
    // is bounded by that guard rather than by call count. Consume-shaped (see
    // the fn doc): a replay answers `tier_not_found` for a tier it deleted
    // itself. State converged; reply diverged.
    b.add(
        "fauna.subscriptions.tiers.delete",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: tiers_delete_handler(),
        },
    );
    // `forbid_replay: false` audited 2026-08-01. Consume-shaped: the auth check
    // (does this request belong to the bearer?) is the load-bearing step, and
    // the delete that follows is best-effort by design. A replay finds the row
    // gone and answers `request_not_found` — it cannot reject a *different*
    // request, because the id it consumed is never reissued
    // (`INTEGER PRIMARY KEY AUTOINCREMENT`).
    b.add(
        "fauna.subscriptions.requests.reject",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: requests_reject_handler(),
        },
    );
    // `forbid_replay: false` audited 2026-08-01. Four verifications then one
    // `upsert_device_authorization` keyed on `(actor_id, device_key)`; the
    // stored value is the canonical signed bytes the request carried, so a
    // repeat re-stores the identical delegation. Fully idempotent in reply too.
    b.add(
        "fauna.subscriptions.delegate.upload",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: delegate_upload_handler(),
        },
    );
    // `forbid_replay: false` audited 2026-08-01 — and this handler is the
    // family's model, because it was written idempotent on purpose rather than
    // by luck. Three ordered early-returns absorb a repeat: already a
    // subscriber → `Approved`; a pending request already enqueued → `Queued`
    // carrying *that* row's id; only then the insert. Every side effect
    // reachable on the repeat path is keyed (`update_subscriber_mlkem_ek`,
    // `update_subscribe_request_mlkem_ek`, `enqueue_unlock_fanout`).
    // Reply-idempotent as well as state-idempotent — the bar the rest of the
    // family is measured against.
    b.add(
        "fauna.subscriptions.subscribe",
        RpcKindMeta {
            forbid_replay: false,
            // The same headroom as tiers.create (kind-registry parity).
            default_deadline: Duration::from_secs(15),
            handler: subscribe_handler(),
        },
    );
    // `forbid_replay: false` audited 2026-08-01 — this is the kind the pass
    // actually fixed, so the flag now states something true. The enqueue was
    // NOT convergent: it went through a bare `INSERT`, and
    // because the subscriber deliberately stays in the roster until the
    // author's client commits, a replay saw the identical input and hit the
    // `UNIQUE(author_id, subscriber_id, tier_name, kind)` constraint — so an
    // unsubscribe that had already succeeded came back as
    // `fauna.protocol.internal`. `insert_subscribe_request` is now an
    // upsert-then-read (the shape `upsert_payment_entitled_request` already
    // used next door), so the replay returns the same `Queued` row id the first
    // call minted. Pinned by
    // `unsubscribe_replay_returns_the_same_queued_row`.
    b.add(
        "fauna.subscriptions.unsubscribe",
        RpcKindMeta {
            forbid_replay: false,
            // The same headroom as its sibling doors (kind-registry parity).
            default_deadline: Duration::from_secs(15),
            handler: unsubscribe_handler(),
        },
    );
    // `forbid_replay: false` audited 2026-08-01. Consume-shaped on
    // `request_id`: the approve deletes the request row, so a replay answers
    // `request_not_found` rather than approving twice. The two writes that
    // outlive the row are keyed — `set_subscriber_valid_until` re-stamps the
    // same window read from the request *before* the delete, and
    // `enqueue_unlock_fanout` is an `ON CONFLICT … DO UPDATE`. Note the reach
    // is bounded by the row, not by the caller: the id is consumed, so a
    // repeat cannot approve a request that arrived after it.
    b.add(
        "fauna.subscriptions.requests.approve",
        RpcKindMeta {
            forbid_replay: false,
            // One verify + one store; the headroom matches its sibling upload
            // doors (kind-registry parity).
            default_deadline: Duration::from_secs(15),
            handler: requests_approve_handler(),
        },
    );
    // `forbid_replay: false` audited 2026-08-01. Keyed on
    // `(author_id, subscriber_id, tier_name)`, and removal is convergent —
    // a repeat removes a row that is already gone. It is consume-shaped on the *rotation generation* rather than on a row: the
    // `stale_rotation` guard refuses any upload whose `rotated_at` does not
    // advance past the stored blob, so replaying one upload cannot re-bump the
    // roster or install a second blob. Its roster check (`current \ {subscriber}`)
    // then fails closed on a repeat, which is why the reply diverges.
    // `forbid_replay: false` on the same reasoning as its two sibling upload
    // doors: the `stale_rotation` guard refuses any upload whose `rotated_at`
    // does not advance past the stored blob, so replaying one is refused rather
    // than applied twice — the consume-shape is the rotation generation, not a
    // row. There is no roster mutation here for a replay to double at all.
    b.add(
        "fauna.subscriptions.key_blob.rotate",
        RpcKindMeta {
            forbid_replay: false,
            // One verify + one store, like its siblings; matched to them so the kind-registry parity test stays honest.
            default_deadline: Duration::from_secs(15),
            handler: key_blob_rotate_handler(),
        },
    );
    b.add(
        "fauna.subscriptions.subscribers.remove",
        RpcKindMeta {
            forbid_replay: false,
            // One verify + one store; the headroom matches its sibling upload
            // doors (kind-registry parity).
            default_deadline: Duration::from_secs(15),
            handler: subscribers_remove_handler(),
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

    use std::sync::Arc;

    use fauna_core::encoding::canonical_encode;
    use fauna_core::subscription::types::{KeyBlob, KeyBlobEntry};
    use fauna_protocol::subscriptions::TierAskingPrice;

    use crate::db::CacheDb;
    use crate::routes::AppState;

    async fn fixture_state() -> Arc<AppState> {
        crate::test_support::fixture_state()
    }

    /// Like `fixture_state` but with a populated `nest_signing_key` so the
    /// delegate.upload nest-key check can pass.
    async fn fixture_state_with_nest_key() -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[0x42u8; 32]);
        Arc::new(AppState {
            nest_signing_key: Some(signing_key),
            ..AppState::for_test(db)
        })
    }

    /// Build a valid 32-byte Ed25519 verifying key from a seed. Test actor
    /// IDs must be on-curve because `ActorId::to_x25519_public()` panics on
    /// invalid points.
    fn actor_from_seed(seed_byte: u8) -> [u8; 32] {
        ed25519_dalek::SigningKey::from_bytes(&[seed_byte; 32])
            .verifying_key()
            .to_bytes()
    }

    /// A `tiers.create` request for `name` at `rank` carrying the REQUIRED
    /// birth envelope — an empty-roster `KeyBlob` self-signed by the author
    /// `actor_from_seed(seed_byte)` names. Tests override fields with struct
    /// update (`TierCreateRequest { hidden: true, ..tier_create(..) }`).
    fn tier_create(seed_byte: u8, name: &str, rank: u32) -> TierCreateRequest {
        let kp = ActorKeypair::from_secret([seed_byte; 32]);
        let (auth, auth_bytes, auth_env) = self_signed_auth(&kp);
        TierCreateRequest {
            name: name.into(),
            rank,
            description: None,
            price_hint: None,
            payment_url: None,
            auto_approve: false,
            encrypted_upload: mint_upload(
                &kp,
                &auth,
                &auth_bytes,
                &auth_env,
                name,
                // The earliest instant, so a test's later approve / rotation
                // (any `rotated_at` above it) passes the monotonic check.
                Timestamp(1),
                &[],
                &[0x42u8; 32],
            ),
            unlocks_post: None,
            asking_price: None,
            hidden: false,
            extra: Default::default(),
        }
    }

    /// Create `name` at `rank` for the author `actor_from_seed(seed_byte)`
    /// names, through the real `tiers.create` door — so the tier carries the
    /// birth `KeyBlob` at version 1, exactly as every production tier does
    /// (`rotated_at` 1, so any later upload advances past it). Every test that
    /// drives an upload door seeds its tier here: a tier with no stored blob
    /// is refused at those doors.
    async fn create_tier(state: &Arc<AppState>, seed_byte: u8, name: &str, rank: u32) {
        let reply = tiers_create_handler()(
            state.clone(),
            actor_from_seed(seed_byte),
            encode_req(&tier_create(seed_byte, name, rank)),
        )
        .await
        .expect("tiers.create with the birth envelope");
        assert!(decode_reply::<TierCreateReply>(&reply).created);
    }

    fn encode_req<T: serde::Serialize>(req: &T) -> Bytes {
        Bytes::from(encode_canonical(req).expect("encode req").to_vec())
    }

    fn decode_reply<T: serde::de::DeserializeOwned>(b: &Bytes) -> T {
        decode(b).expect("decode reply")
    }

    // ── status.get ─────────────────────────────────────────────

    #[tokio::test]
    async fn status_get_returns_highest_rank_tier_for_subscriber() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x01);
        let subscriber = actor_from_seed(0x02);

        // Two paid tiers. Under the post-fix convention the HIGHER rank is
        // the higher tier, so `gold` is rank 2 and `silver` rank 1 — the fixture
        // read the other way round while the pick was still lowest-rank-wins,
        // which is why this test outlived its own contract (see the assertion).
        state
            .db
            .create_subscription_tier(
                &author, "gold", 2, None, None, None, true, None, None, false,
            )
            .await
            .unwrap();
        state
            .db
            .create_subscription_tier(
                &author, "silver", 1, None, None, None, false, None, None, false,
            )
            .await
            .unwrap();

        // Subscribe to both.
        state
            .db
            .add_subscriber(&author, &subscriber, "gold", None)
            .await
            .unwrap();
        state
            .db
            .add_subscriber(&author, &subscriber, "silver", None)
            .await
            .unwrap();

        let req = StatusGetRequest {
            author_id: ActorId(author),
            extra: Default::default(),
        };
        let bytes = status_get_handler()(state, subscriber, encode_req(&req))
            .await
            .expect("status.get ok");
        let reply: StatusGetReply = decode_reply(&bytes);

        // HIGHEST rank among paid tiers wins → "gold" (rank=2, auto_approve=true).
        // This assertion is what the fix inverted: it read
        // "lowest rank value wins → gold (rank=1)" and kept passing only because
        // the fixture happened to spell the premium tier as rank 1. A fix that
        // inverts a selection rule must sweep every test that encoded the old
        // direction — the fix's own new pins are green by construction.
        assert_eq!(reply.tier.as_deref(), Some("gold"));
        assert!(reply.auto_approve);
        assert!(reply.expires_at.is_none());
    }

    #[tokio::test]
    async fn status_get_returns_none_when_not_subscribed() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x03);
        let stranger = actor_from_seed(0x04);

        state
            .db
            .create_subscription_tier(
                &author, "gold", 1, None, None, None, true, None, None, false,
            )
            .await
            .unwrap();

        let req = StatusGetRequest {
            author_id: ActorId(author),
            extra: Default::default(),
        };
        let bytes = status_get_handler()(state, stranger, encode_req(&req))
            .await
            .expect("status.get ok");
        let reply: StatusGetReply = decode_reply(&bytes);

        assert!(reply.tier.is_none());
        assert!(!reply.auto_approve);
    }

    // ── requests.list ──────────────────────────────────────────

    #[tokio::test]
    async fn requests_list_returns_both_subscribe_and_unsubscribe_rows() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x05);
        let alice = actor_from_seed(0x06);
        let bob = actor_from_seed(0x07);

        // FK: subscribe_requests references subscription_tiers (author_id, name).
        state
            .db
            .create_subscription_tier(
                &author, "gold", 1, None, None, None, true, None, None, false,
            )
            .await
            .unwrap();
        state
            .db
            .create_subscription_tier(
                &author, "silver", 2, None, None, None, true, None, None, false,
            )
            .await
            .unwrap();

        state
            .db
            .insert_subscribe_request(&author, &alice, "gold", "subscribe", None)
            .await
            .unwrap();
        state
            .db
            .insert_subscribe_request(&author, &bob, "silver", "unsubscribe", None)
            .await
            .unwrap();

        let req = RequestsListRequest {};
        let bytes = requests_list_handler()(state, author, encode_req(&req))
            .await
            .expect("requests.list ok");
        let reply: RequestsListReply = decode_reply(&bytes);

        assert_eq!(reply.requests.len(), 2);
        let kinds: std::collections::BTreeSet<&str> =
            reply.requests.iter().map(|r| r.kind.as_str()).collect();
        assert!(kinds.contains("subscribe"));
        assert!(kinds.contains("unsubscribe"));

        // Verify subscriber_id round-trip + tier_name attached to the right row.
        let subscribe_row = reply
            .requests
            .iter()
            .find(|r| r.kind == "subscribe")
            .expect("subscribe row present");
        assert_eq!(subscribe_row.subscriber_id.0, alice);
        assert_eq!(subscribe_row.tier_name, "gold");
    }

    // ── key_blob.get ───────────────────────────────────────────

    #[tokio::test]
    async fn key_blob_get_returns_dag_cbor_encoded_bytes_verbatim() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x08);
        let subscriber = actor_from_seed(0x09);

        state
            .db
            .create_subscription_tier(
                &author, "gold", 1, None, None, None, true, None, None, false,
            )
            .await
            .unwrap();
        state
            .db
            .add_subscriber(&author, &subscriber, "gold", None)
            .await
            .unwrap();

        // Seed a dag-cbor-encoded blob.
        let signing = ed25519_dalek::SigningKey::from_bytes(&[0x42u8; 32]);
        let signer_pk = signing.verifying_key().to_bytes();
        let blob = KeyBlob {
            author: ActorId(author),
            tier: "gold".into(),
            rotated_at: Timestamp(1_700_000_000_000_000),
            entries: vec![KeyBlobEntry {
                subscriber: ActorId(subscriber),
                encrypted_key: vec![1, 2, 3, 4, 5, 6, 7, 8],
                suite: fauna_core::subscription::types::KemSuiteId::Classical,
            }],
            signer: signer_pk,
            key_commitment: [0x6b; 32],
        };
        let signer_kp = fauna_core::identity::ActorKeypair::from_secret(signing.to_bytes());
        let (canon_bytes, env) = sign_envelope(&signer_kp, &blob).expect("sign blob");
        let blob_hash: [u8; 32] = *blake3::hash(&canon_bytes).as_bytes();
        let wire = EmbedAsBytes::from_signed(canon_bytes, env);
        let stored = canonical_encode(&wire).expect("dag-cbor-encode wire");
        state
            .db
            .upsert_current_key_blob(&author, "gold", 3, &blob_hash, &stored)
            .await
            .unwrap();

        let req = KeyBlobGetRequest {
            author_id: ActorId(author),
            tier_name: "gold".into(),
            extra: Default::default(),
        };
        let bytes = key_blob_get_handler()(state, subscriber, encode_req(&req))
            .await
            .expect("key_blob.get ok");
        let reply: KeyBlobGetReply = decode_reply(&bytes);

        assert_eq!(reply.version, 3);
        assert_eq!(reply.blob_hash.as_ref(), blob_hash.as_slice());
        assert_eq!(reply.blob_data.as_ref(), stored.as_slice());

        // And the bytes still decode through the embed-as-bytes wire shape.
        let wire: EmbedAsBytes = canonical_decode(reply.blob_data.as_ref()).expect("decode wire");
        let round: KeyBlob = decode_signed_bytes(&wire.bytes).expect("decode keyblob");
        assert_eq!(round.tier, "gold");
        assert_eq!(round.entries.len(), 1);
    }

    #[tokio::test]
    async fn key_blob_get_rejects_non_subscriber() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x0A);
        let stranger = actor_from_seed(0x0B);

        state
            .db
            .create_subscription_tier(
                &author, "gold", 1, None, None, None, true, None, None, false,
            )
            .await
            .unwrap();

        let req = KeyBlobGetRequest {
            author_id: ActorId(author),
            tier_name: "gold".into(),
            extra: Default::default(),
        };
        let err = key_blob_get_handler()(state, stranger, encode_req(&req))
            .await
            .expect_err("must reject non-subscriber");
        assert_eq!(err.code, "fauna.subscriptions.not_subscribed");
    }

    // ── subscribers.list ───────────────────────────────────────

    #[tokio::test]
    async fn subscribers_list_returns_actors_with_joined_at_in_micros() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x0C);
        let alice = actor_from_seed(0x0D);
        let bob = actor_from_seed(0x0E);

        state
            .db
            .create_subscription_tier(
                &author, "gold", 1, None, None, None, true, None, None, false,
            )
            .await
            .unwrap();
        state
            .db
            .add_subscriber(&author, &alice, "gold", None)
            .await
            .unwrap();
        state
            .db
            .add_subscriber(&author, &bob, "gold", None)
            .await
            .unwrap();

        let req = SubscribersListRequest {
            tier_name: "gold".into(),
            extra: Default::default(),
        };
        let bytes = subscribers_list_handler()(state, author, encode_req(&req))
            .await
            .expect("subscribers.list ok");
        let reply: SubscribersListReply = decode_reply(&bytes);

        assert_eq!(reply.subscribers.len(), 2);
        let ids: std::collections::BTreeSet<[u8; 32]> = reply
            .subscribers
            .iter()
            .map(|s| s.subscriber_id.0)
            .collect();
        assert!(ids.contains(&alice));
        assert!(ids.contains(&bob));

        // approved_at is seconds in the DB; reply joined_at is micros.
        for s in &reply.subscribers {
            assert!(s.joined_at.0 > 0, "joined_at should be non-zero");
        }
    }

    // ── tiers.create ───────────────────────────────────────────

    /// The envelope is REQUIRED: a create without the birth `KeyBlob` — the
    /// shape an older client once sent, whose accept arm the compat-remnant
    /// sweep removed (`version-compatibility.md` § Dimension 2, the fourth
    /// ratified exception) — is refused at decode, and leaves no tier row.
    #[tokio::test]
    async fn tiers_create_refuses_a_create_without_the_birth_envelope() {
        #[derive(serde::Serialize)]
        struct EnvelopeLessCreate {
            name: String,
            rank: u32,
            auto_approve: bool,
        }
        let state = fixture_state().await;
        let author = actor_from_seed(0x32);

        let err = tiers_create_handler()(
            state.clone(),
            author,
            encode_req(&EnvelopeLessCreate {
                name: "tier1".into(),
                rank: 1,
                auto_approve: true,
            }),
        )
        .await
        .expect_err("an envelope-less create must be refused");
        assert_eq!(err.code, "fauna.subscriptions.malformed_upload");
        assert!(
            state
                .db
                .get_subscription_tier(&author, "tier1")
                .await
                .unwrap()
                .is_none(),
            "a refused create must leave no keyless tier behind"
        );
    }

    /// A forged birth envelope is refused BEFORE the tier row exists, so it
    /// cannot leave a keyless tier behind either.
    #[tokio::test]
    async fn tiers_create_refuses_a_foreign_envelope_before_inserting_the_tier() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x34);

        // Signed by a DIFFERENT actor than the caller.
        let req = TierCreateRequest {
            auto_approve: true,
            ..tier_create(0x35, "tier1", 1)
        };
        tiers_create_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect_err("a foreign-signed birth envelope must be refused");
        assert!(
            state
                .db
                .get_subscription_tier(&author, "tier1")
                .await
                .unwrap()
                .is_none(),
            "verification runs before the insert"
        );
    }

    #[tokio::test]
    async fn tiers_create_stores_the_client_birth_blob_and_no_mls_group() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x31);

        let req = TierCreateRequest {
            name: "tier1".into(),
            rank: 1,
            description: None,
            price_hint: None,
            payment_url: None,
            auto_approve: true,
            unlocks_post: None,
            ..tier_create(0x31, "tier1", 1)
        };
        let bytes = tiers_create_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect("tiers.create ok");
        let reply: TierCreateReply = decode_reply(&bytes);
        assert!(reply.created);

        // Tier row exists.
        assert!(
            state
                .db
                .get_subscription_tier(&author, "tier1")
                .await
                .unwrap()
                .is_some(),
            "tiers.create must insert the tier row"
        );

        // The nest holds no mint authority: the author's client owns the
        // broadcast key and uploaded the birth KeyBlob on this create, which
        // is stored at version 1 — the prior every later upload door requires.
        let (version, _, _) = state
            .db
            .get_current_key_blob(&author, "tier1")
            .await
            .unwrap()
            .expect("the birth blob is stored");
        assert_eq!(version, 1);
    }

    /// `tiers.create` persists `hidden` — the archive-import machine's mint of
    /// the reserved owner-only tier goes through this kind like any tier.
    #[tokio::test]
    async fn tiers_create_persists_the_hidden_flag() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x33);

        let req = TierCreateRequest {
            name: fauna_core::subscription::OWNER_ONLY_TIER.into(),
            rank: fauna_core::subscription::OWNER_ONLY_TIER_RANK,
            auto_approve: false,
            hidden: true,
            ..tier_create(
                0x33,
                fauna_core::subscription::OWNER_ONLY_TIER,
                fauna_core::subscription::OWNER_ONLY_TIER_RANK,
            )
        };
        let bytes = tiers_create_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect("tiers.create ok");
        let reply: TierCreateReply = decode_reply(&bytes);
        assert!(reply.created);

        let row = state
            .db
            .get_subscription_tier(&author, fauna_core::subscription::OWNER_ONLY_TIER)
            .await
            .unwrap()
            .expect("row exists");
        assert!(row.hidden, "the flag is stored");
        assert_eq!(
            row.rank,
            i64::from(u32::MAX),
            "the top rank survives the i64 column"
        );
    }

    /// The tier every room-restricted post carries is a reserved constant,
    /// never an author's (`ui/feed.md` § Encryption at rest → *Room-restricted
    /// — the ruling*, ruling 3), so no author may create a tier of that name —
    /// at any rank — and nothing is stored when one tries.
    #[tokio::test]
    async fn tiers_create_refuses_the_room_post_tier_name() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x36);
        let req = TierCreateRequest {
            name: fauna_core::subscription::ROOM_POST_TIER.into(),
            rank: 3,
            ..tier_create(0x36, fauna_core::subscription::ROOM_POST_TIER, 3)
        };
        let err = tiers_create_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect_err("the room post tier is never an author's");
        assert!(format!("{err:?}").contains("reserved"), "{err:?}");
        assert!(
            state
                .db
                .get_subscription_tier(&author, fauna_core::subscription::ROOM_POST_TIER)
                .await
                .unwrap()
                .is_none(),
            "no row is stored"
        );
    }

    /// Arm B: `OWNER_ONLY_TIER_RANK`'s
    /// own doc comment (`libs/fauna-core/src/subscription/mod.rs`) claims "a
    /// paid tier is created at `1 <= rank < u32::MAX`" — this is the check
    /// that makes the claim true. An ordinary name may not sit at or above
    /// the reserved owner-only tier's rank; only the reserved name itself may
    /// (`tiers_create_persists_the_hidden_flag`, above, is that positive
    /// control).
    #[tokio::test]
    async fn tiers_create_rejects_rank_at_the_owner_only_ceiling_for_an_ordinary_name() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x37);
        let req = TierCreateRequest {
            name: "gold".into(),
            rank: fauna_core::subscription::OWNER_ONLY_TIER_RANK,
            ..tier_create(0x37, "gold", fauna_core::subscription::OWNER_ONLY_TIER_RANK)
        };
        let err = tiers_create_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect_err("an ordinary name may not sit at the reserved ceiling");
        assert!(format!("{err:?}").contains("reserved"), "{err:?}");
        assert!(
            state
                .db
                .get_subscription_tier(&author, "gold")
                .await
                .unwrap()
                .is_none(),
            "no row is stored"
        );
    }

    /// A hidden tier is never subscribable (ruling 4), so a hidden `unlocks_post` tier could never be
    /// bought. This refusal is one layer of defense, not the only one —
    /// `enqueue_unlock_fanout_for_new_tier` checks rule (f) directly as its
    /// own in-consumer guard, so even without this refusal the
    /// creation-time fan-out could not reach a hidden target.
    #[tokio::test]
    async fn tiers_create_refuses_hidden_combined_with_unlocks_post() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x38);
        let req = TierCreateRequest {
            name: "secret-sale".into(),
            rank: 1,
            hidden: true,
            unlocks_post: Some(
                "3333333333333333333333333333333333333333333333333333333333333333".into(),
            ),
            ..tier_create(0x38, "secret-sale", 1)
        };
        let err = tiers_create_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect_err("hidden + unlocks_post together is refused");
        assert!(format!("{err:?}").contains("hidden"), "{err:?}");
        assert!(
            state
                .db
                .get_subscription_tier(&author, "secret-sale")
                .await
                .unwrap()
                .is_none(),
            "no row is stored"
        );
    }

    /// The creation-time twin's own in-consumer check: unlike `enqueue_unlock_fanout`,
    /// this door's doc comment used to argue
    /// its target could never be hidden "by construction" — true only because
    /// `tiers.create` refuses `hidden` combined with `unlocks_post`, which is
    /// the caller's guard, not this function's own. Seeded directly at the DB
    /// layer, since no production caller
    /// can build a hidden designated tier through `tiers.create`.
    #[tokio::test]
    async fn enqueue_unlock_fanout_for_new_tier_excludes_a_hidden_target() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x3a);
        let sub = actor_from_seed(0x3b);

        // The subscriber's existing, unexpired, undesignated tier at a rank
        // above both designated tiers below — eligible for either fan-out.
        state
            .db
            .create_subscription_tier(
                &author, "gold", 5, None, None, None, true, None, None, false,
            )
            .await
            .unwrap();
        state
            .db
            .add_subscriber(&author, &sub, "gold", None)
            .await
            .unwrap();

        // The positive control: an ordinary designated (`unlocks_post`) tier.
        state
            .db
            .create_subscription_tier(
                &author,
                "post-unlock-control",
                1,
                None,
                None,
                None,
                true,
                Some("1111111111111111111111111111111111111111111111111111111111111111"),
                None,
                false,
            )
            .await
            .unwrap();
        // The hidden designated tier under test — never reachable through
        // `tiers.create`, seeded directly to isolate this function's own guard.
        state
            .db
            .create_subscription_tier(
                &author,
                "backstage-sale",
                1,
                None,
                None,
                None,
                true,
                Some("2222222222222222222222222222222222222222222222222222222222222222"),
                None,
                true,
            )
            .await
            .unwrap();

        let control_changed = state
            .db
            .enqueue_unlock_fanout_for_new_tier(&author, "post-unlock-control")
            .await
            .unwrap();
        assert_eq!(
            control_changed, 1,
            "the positive control must fan in the existing subscriber"
        );

        let hidden_changed = state
            .db
            .enqueue_unlock_fanout_for_new_tier(&author, "backstage-sale")
            .await
            .unwrap();
        assert_eq!(
            hidden_changed, 0,
            "a hidden tier must never be a creation-time cascade target"
        );

        let rows = state.db.list_subscribe_requests(&author).await.unwrap();
        assert!(
            rows.iter().any(|r| r.tier_name == "post-unlock-control"),
            "the positive control's request must be enqueued: {rows:?}"
        );
        assert!(
            !rows.iter().any(|r| r.tier_name == "backstage-sale"),
            "the hidden tier's request must never be enqueued: {rows:?}"
        );
    }

    /// Ruling 4 (`monetization.md` § The unifying model → *A tier may be
    /// hidden*): hidden means not offered AND not subscribable. A `subscribe`
    /// naming the reserved owner-only tier answers `tier_not_found` —
    /// indistinguishable from a tier that does not exist — and leaves no
    /// pending request behind for the author to approve by accident.
    #[tokio::test]
    async fn subscribe_to_a_hidden_tier_is_tier_not_found_and_enqueues_nothing() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x34);
        let stranger = actor_from_seed(0x35);

        let create = TierCreateRequest {
            name: fauna_core::subscription::OWNER_ONLY_TIER.into(),
            rank: fauna_core::subscription::OWNER_ONLY_TIER_RANK,
            auto_approve: false,
            hidden: true,
            ..tier_create(
                0x34,
                fauna_core::subscription::OWNER_ONLY_TIER,
                fauna_core::subscription::OWNER_ONLY_TIER_RANK,
            )
        };
        tiers_create_handler()(state.clone(), author, encode_req(&create))
            .await
            .expect("tiers.create ok");

        let req = SubscribeRequest {
            author_id: ActorId(author),
            tier: fauna_core::subscription::OWNER_ONLY_TIER.into(),
            mlkem_encaps_key: None,
            extra: Default::default(),
        };
        let err = subscribe_handler()(state.clone(), stranger, encode_req(&req))
            .await
            .expect_err("a hidden tier is not subscribable");
        assert_eq!(err.code, "fauna.subscriptions.tier_not_found");
        assert!(
            state
                .db
                .list_subscribe_requests(&author)
                .await
                .unwrap()
                .is_empty(),
            "no pending request lands for a hidden tier"
        );
    }

    /// The client half already refuses an author subscribing to their
    /// own tier (`FeedManager::is_local_actor`); the nest's own
    /// subscribe door must refuse it too, since any non-Fauna or
    /// non-conforming client reaches this handler directly. Asserted on the
    /// DB, not the reply alone: no `subscribers` row, no `subscribe_requests`
    /// row, and — since the author here has no pre-created tier — no
    /// `FOLLOWERS_TIER` lazily minted either, which is the whole point of
    /// placing the guard ahead of `ensure_followers_tier`.
    #[tokio::test]
    async fn subscribe_to_your_own_tier_is_refused_and_writes_nothing() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x37);

        let req = SubscribeRequest {
            author_id: ActorId(author),
            tier: FOLLOWERS_TIER.into(),
            mlkem_encaps_key: None,
            extra: Default::default(),
        };
        let err = subscribe_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect_err("an author may not subscribe to their own tier");
        assert_eq!(err.code, "fauna.subscriptions.forbidden");
        assert!(
            !state
                .db
                .is_subscriber(&author, &author, FOLLOWERS_TIER)
                .await
                .unwrap(),
            "no subscribers row for the self-follow"
        );
        assert!(
            state
                .db
                .list_subscribe_requests(&author)
                .await
                .unwrap()
                .is_empty(),
            "no pending subscribe_requests row for the self-follow"
        );
        assert!(
            state
                .db
                .get_subscription_tier(&author, FOLLOWERS_TIER)
                .await
                .unwrap()
                .is_none(),
            "the self-follow must not lazily mint FOLLOWERS_TIER for a tier-less author"
        );
    }

    /// Following an author this nest does not host must not lazily
    /// mint a `FOLLOWERS_TIER` for them — the caller names any 32-byte
    /// `author_id`, and a loop over fresh ids would otherwise grow
    /// `subscription_tiers` and `subscribe_requests` without bound. The
    /// answer is the same `tier_not_found` a missing tier gets, and the DB
    /// holds neither row afterwards.
    #[tokio::test]
    async fn following_an_author_this_nest_does_not_host_writes_nothing() {
        let state = fixture_state().await;
        let stranger = actor_from_seed(0x3A);
        let follower = actor_from_seed(0x3B);
        state
            .db
            .create_user(&follower, "free", "follower")
            .await
            .unwrap();

        let req = SubscribeRequest {
            author_id: ActorId(stranger),
            tier: FOLLOWERS_TIER.into(),
            mlkem_encaps_key: None,
            extra: Default::default(),
        };
        let err = subscribe_handler()(state.clone(), follower, encode_req(&req))
            .await
            .expect_err("following an unhosted author is refused");
        assert_eq!(err.code, tier_not_found("").code);
        assert!(
            state
                .db
                .get_subscription_tier(&stranger, FOLLOWERS_TIER)
                .await
                .unwrap()
                .is_none(),
            "no followers tier is minted for an author this nest does not host"
        );
        assert!(
            state
                .db
                .list_subscribe_requests(&stranger)
                .await
                .unwrap()
                .is_empty(),
            "no pending subscribe_requests row for the phantom follow"
        );
    }

    /// The same refusal for a `post-unlock-*` tier: gated posts are never sold
    /// to their own author (`FeedManager::unlock_gated_post`'s `is_author`
    /// branch is custody, not a purchase) — the guard applies to every tier
    /// name, not only `FOLLOWERS_TIER`.
    #[tokio::test]
    async fn subscribe_to_your_own_post_unlock_tier_is_also_refused() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x38);

        let create = TierCreateRequest {
            name: "post-unlock-abc123".into(),
            rank: 5,
            auto_approve: true,
            ..tier_create(0x38, "post-unlock-abc123", 5)
        };
        tiers_create_handler()(state.clone(), author, encode_req(&create))
            .await
            .expect("tiers.create ok");

        let req = SubscribeRequest {
            author_id: ActorId(author),
            tier: "post-unlock-abc123".into(),
            mlkem_encaps_key: None,
            extra: Default::default(),
        };
        let err = subscribe_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect_err("an author may not buy their own gated post");
        assert_eq!(err.code, "fauna.subscriptions.forbidden");
        assert!(
            !state
                .db
                .is_subscriber(&author, &author, "post-unlock-abc123")
                .await
                .unwrap(),
            "no subscribers row for the self-purchase"
        );
    }

    #[tokio::test]
    async fn tiers_create_rejects_unique_violation() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x32);

        let req = TierCreateRequest {
            name: "tier1".into(),
            rank: 1,
            description: None,
            price_hint: None,
            payment_url: None,
            auto_approve: true,
            unlocks_post: None,
            ..tier_create(0x32, "tier1", 1)
        };
        tiers_create_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect("first create ok");

        let err = tiers_create_handler()(state, author, encode_req(&req))
            .await
            .expect_err("second create must conflict");
        assert_eq!(err.code, "fauna.subscriptions.tier_already_exists");
    }

    #[tokio::test]
    async fn tiers_create_rejects_invalid_rank() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x33);

        let req = TierCreateRequest {
            name: "tier1".into(),
            rank: 0,
            description: None,
            price_hint: None,
            payment_url: None,
            auto_approve: true,
            unlocks_post: None,
            ..tier_create(0x33, "tier1", 0)
        };
        let err = tiers_create_handler()(state, author, encode_req(&req))
            .await
            .expect_err("rank=0 must be rejected");
        assert_eq!(err.code, "fauna.subscriptions.malformed_upload");
    }

    /// The slice-3 relaxation (`monetization.md` § The unifying model, the
    /// rank-0 paragraph): the reserved `followers` name at exactly rank 0 is
    /// the idempotent client-side twin of `ensure_followers_tier`, birth blob
    /// included — so an account nobody follows yet can gate a post to
    /// followers. Any other name at rank 0 stays refused.
    #[tokio::test]
    async fn tiers_create_accepts_the_reserved_followers_name_at_rank_zero_idempotently() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x36);

        let req = TierCreateRequest {
            name: FOLLOWERS_TIER.into(),
            rank: 0,
            auto_approve: true,
            hidden: false,
            ..tier_create(0x36, FOLLOWERS_TIER, 0)
        };
        let first: TierCreateReply = decode_reply(
            &tiers_create_handler()(state.clone(), author, encode_req(&req))
                .await
                .expect("first create ok"),
        );
        assert!(first.created);
        let row = state
            .db
            .get_subscription_tier(&author, FOLLOWERS_TIER)
            .await
            .unwrap()
            .expect("row exists");
        assert_eq!(row.rank, FOLLOWERS_TIER_RANK);
        assert!(row.auto_approve, "forced true: following needs no approval");
        assert!(!row.hidden);

        let second: TierCreateReply = decode_reply(
            &tiers_create_handler()(state.clone(), author, encode_req(&req))
                .await
                .expect("second create is idempotent, never tier_already_exists"),
        );
        assert!(!second.created);

        // The row the nest itself provisions on a first follow is the same row:
        // creating after `ensure_followers_tier` ran is `created: false` too.
        let other = actor_from_seed(0x37);
        ensure_followers_tier(&state, &other).await.unwrap();
        let other_req = TierCreateRequest {
            auto_approve: true,
            ..tier_create(0x37, FOLLOWERS_TIER, 0)
        };
        let after_nest: TierCreateReply = decode_reply(
            &tiers_create_handler()(state.clone(), other, encode_req(&other_req))
                .await
                .expect("create after nest provisioning ok"),
        );
        assert!(!after_nest.created);

        // Every other name at rank 0 is still the reserved-slot refusal.
        let other_name = TierCreateRequest {
            name: "tier1".into(),
            rank: 0,
            auto_approve: true,
            ..tier_create(0x36, "tier1", 0)
        };
        let err = tiers_create_handler()(state, author, encode_req(&other_name))
            .await
            .expect_err("rank=0 under any other name must be rejected");
        assert_eq!(err.code, "fauna.subscriptions.malformed_upload");
    }

    /// The relaxation's `auto_approve` is forced, not trusted: a client that
    /// sends `false` (or `hidden: true`) for the followers tier gets the
    /// reserved row's shape, because the followers tier's meaning
    /// (follow = subscribe, no judgment) is the nest's to keep. The same goes
    /// for every monetization field — the followers tier is free and
    /// undesignated, so a description, a payment URL, an asking price or a
    /// per-post designation sent with the reserved create is dropped, not
    /// stored.
    #[tokio::test]
    async fn the_reserved_followers_create_forces_the_reserved_shape() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x38);
        let req = TierCreateRequest {
            name: FOLLOWERS_TIER.into(),
            rank: 0,
            auto_approve: false,
            hidden: true,
            description: Some("not stored either".into()),
            price_hint: Some("5 EUR".into()),
            payment_url: Some("https://pay.example/followers".into()),
            unlocks_post: Some("ab".repeat(32)),
            asking_price: Some(fauna_protocol::subscriptions::TierAskingPrice {
                value: 1_000,
                unit: "msat".into(),
                extra: Default::default(),
            }),
            ..tier_create(0x38, FOLLOWERS_TIER, 0)
        };
        tiers_create_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect("create ok");
        let row = state
            .db
            .get_subscription_tier(&author, FOLLOWERS_TIER)
            .await
            .unwrap()
            .unwrap();
        assert!(row.auto_approve);
        assert!(!row.hidden);
        assert_eq!(row.description, None);
        assert_eq!(row.price_hint, None);
        assert_eq!(row.payment_url, None);
        assert_eq!(row.unlocks_post, None, "the followers tier sells no post");
        assert_eq!(
            (row.asking_price_value, row.asking_price_unit),
            (None, None),
            "the followers tier is free"
        );
    }

    /// The relaxation stores the birth blob exactly like an ordinary create
    /// (`upsert_current_key_blob` version 1) — and only when the tier has none,
    /// so a second create can never roll a live roster back to empty.
    #[tokio::test]
    async fn the_reserved_followers_create_stores_a_birth_blob_once() {
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x39u8; 32]);
        let author = author_kp.actor_id().0;
        let (auth, auth_bytes, auth_env) = self_signed_auth(&author_kp);

        let upload = mint_upload(
            &author_kp,
            &auth,
            &auth_bytes,
            &auth_env,
            FOLLOWERS_TIER,
            Timestamp(1_700_000_100_000_000),
            &[], // a tier is born with an empty roster
            &[0x91u8; 32],
        );
        let first: TierCreateReply = decode_reply(
            &tiers_create_handler()(
                state.clone(),
                author,
                encode_req(&TierCreateRequest {
                    name: FOLLOWERS_TIER.into(),
                    rank: 0,
                    auto_approve: true,
                    encrypted_upload: upload,
                    ..tier_create(0x39, FOLLOWERS_TIER, 0)
                }),
            )
            .await
            .expect("create with birth blob ok"),
        );
        assert!(first.created);

        let got: KeyBlobGetReply = decode_reply(
            &key_blob_get_handler()(
                state.clone(),
                author,
                encode_req(&KeyBlobGetRequest {
                    author_id: ActorId(author),
                    tier_name: FOLLOWERS_TIER.into(),
                    extra: Default::default(),
                }),
            )
            .await
            .expect("the author reads the birth blob"),
        );
        assert_eq!(got.version, 1, "birth blob is version 1");

        // A second create — with a DIFFERENT blob — creates nothing and must
        // never overwrite the stored one (a live roster would be rolled back).
        let second_upload = mint_upload(
            &author_kp,
            &auth,
            &auth_bytes,
            &auth_env,
            FOLLOWERS_TIER,
            Timestamp(1_700_000_200_000_000),
            &[],
            &[0x92u8; 32],
        );
        let second: TierCreateReply = decode_reply(
            &tiers_create_handler()(
                state.clone(),
                author,
                encode_req(&TierCreateRequest {
                    name: FOLLOWERS_TIER.into(),
                    rank: 0,
                    auto_approve: true,
                    encrypted_upload: second_upload,
                    ..tier_create(0x39, FOLLOWERS_TIER, 0)
                }),
            )
            .await
            .expect("second create is idempotent"),
        );
        assert!(!second.created);

        let after: KeyBlobGetReply = decode_reply(
            &key_blob_get_handler()(
                state,
                author,
                encode_req(&KeyBlobGetRequest {
                    author_id: ActorId(author),
                    tier_name: FOLLOWERS_TIER.into(),
                    extra: Default::default(),
                }),
            )
            .await
            .expect("the blob is still served"),
        );
        assert_eq!(after.version, 1);
        assert_eq!(
            after.blob_data, got.blob_data,
            "the stored birth blob is untouched by a second create"
        );
    }

    // ── tiers.update ───────────────────────────────────────────

    #[tokio::test]
    async fn tiers_update_applies_new_fields_keeps_unset_ones() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x16);

        state
            .db
            .create_subscription_tier(
                &author,
                "gold",
                1,
                Some("old desc"),
                Some("$5"),
                None,
                true,
                None,
                None,
                false,
            )
            .await
            .unwrap();

        // Update description + auto_approve; leave price_hint/payment_url
        // unset on the wire. HTTP-twin parity says current values stick.
        let req = TierUpdateRequest {
            name: "gold".into(),
            rank: None,
            description: Some("new desc".into()),
            price_hint: None,
            payment_url: None,
            auto_approve: Some(false),
            unlocks_post: None,
            ..Default::default()
        };
        let bytes = tiers_update_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect("tiers.update ok");
        let reply: TierUpdateReply = decode_reply(&bytes);
        assert!(reply.updated);

        let after = state
            .db
            .get_subscription_tier(&author, "gold")
            .await
            .unwrap()
            .expect("tier present");
        assert_eq!(after.description.as_deref(), Some("new desc"));
        // price_hint preserved from the original create.
        assert_eq!(after.price_hint.as_deref(), Some("$5"));
        assert!(!after.auto_approve);
    }

    #[tokio::test]
    async fn tiers_update_returns_tier_not_found_for_unknown_name() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x17);

        let req = TierUpdateRequest {
            name: "ghost".into(),
            rank: None,
            description: Some("x".into()),
            price_hint: None,
            payment_url: None,
            auto_approve: None,
            unlocks_post: None,
            ..Default::default()
        };
        let err = tiers_update_handler()(state, author, encode_req(&req))
            .await
            .expect_err("unknown tier must fail");
        assert_eq!(err.code, "fauna.subscriptions.tier_not_found");
    }

    // ── tiers.clear_field ────────────────────────────────────────

    #[tokio::test]
    async fn tiers_clear_field_wipes_only_the_named_field() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x19);

        state
            .db
            .create_subscription_tier(
                &author,
                "gold",
                1,
                Some("old desc"),
                Some("$5"),
                Some("https://pay.example/gold"),
                true,
                None,
                Some(&TierAskingPrice {
                    value: 1000,
                    unit: "msat".into(),
                    extra: Default::default(),
                }),
                false,
            )
            .await
            .unwrap();

        // Clearing description must not disturb price_hint, payment_url,
        // asking_price or auto_approve.
        let req = TierClearFieldRequest {
            name: "gold".into(),
            field: TierClearableField::Description,
            ..Default::default()
        };
        let bytes = tiers_clear_field_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect("clear ok");
        let reply: TierClearFieldReply = decode_reply(&bytes);
        assert!(reply.cleared);

        let after = state
            .db
            .get_subscription_tier(&author, "gold")
            .await
            .unwrap()
            .expect("tier present");
        assert_eq!(after.description, None, "description must be cleared");
        assert_eq!(after.price_hint.as_deref(), Some("$5"));
        assert_eq!(
            after.payment_url.as_deref(),
            Some("https://pay.example/gold")
        );
        assert!(after.auto_approve);
        assert_eq!(
            after.asking_price(),
            Some(TierAskingPrice {
                value: 1000,
                unit: "msat".into(),
                extra: Default::default(),
            }),
            "asking_price must be untouched by a description clear"
        );

        // Clearing asking_price back to unset — the field the gap was
        // filed over.
        let req = TierClearFieldRequest {
            name: "gold".into(),
            field: TierClearableField::AskingPrice,
            ..Default::default()
        };
        tiers_clear_field_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect("clear ok");
        let after = state
            .db
            .get_subscription_tier(&author, "gold")
            .await
            .unwrap()
            .expect("tier present");
        assert_eq!(after.asking_price(), None, "asking_price must be cleared");
        // price_hint still untouched.
        assert_eq!(after.price_hint.as_deref(), Some("$5"));
    }

    #[tokio::test]
    async fn tiers_clear_field_returns_tier_not_found_for_unknown_name() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x1a);

        let req = TierClearFieldRequest {
            name: "ghost".into(),
            field: TierClearableField::PriceHint,
            ..Default::default()
        };
        let err = tiers_clear_field_handler()(state, author, encode_req(&req))
            .await
            .expect_err("unknown tier must fail");
        assert_eq!(err.code, "fauna.subscriptions.tier_not_found");
    }

    // ── tiers.delete ───────────────────────────────────────────

    #[tokio::test]
    async fn tiers_delete_removes_tier_when_no_subscribers() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x18);

        state
            .db
            .create_subscription_tier(
                &author, "gold", 1, None, None, None, true, None, None, false,
            )
            .await
            .unwrap();

        let req = TierDeleteRequest {
            name: "gold".into(),
            extra: Default::default(),
        };
        let bytes = tiers_delete_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect("tiers.delete ok");
        let reply: TierDeleteReply = decode_reply(&bytes);
        assert!(reply.deleted);

        let after = state
            .db
            .get_subscription_tier(&author, "gold")
            .await
            .unwrap();
        assert!(after.is_none());
    }

    #[tokio::test]
    async fn tiers_delete_returns_tier_not_found_for_unknown_name() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x19);

        let req = TierDeleteRequest {
            name: "ghost".into(),
            extra: Default::default(),
        };
        let err = tiers_delete_handler()(state, author, encode_req(&req))
            .await
            .expect_err("unknown tier must fail");
        assert_eq!(err.code, "fauna.subscriptions.tier_not_found");
    }

    // ── requests.reject ────────────────────────────────────────

    #[tokio::test]
    async fn requests_reject_removes_request_for_addressed_author() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x1A);
        let alice = actor_from_seed(0x1B);

        state
            .db
            .create_subscription_tier(
                &author, "gold", 1, None, None, None, true, None, None, false,
            )
            .await
            .unwrap();
        let req_id = state
            .db
            .insert_subscribe_request(&author, &alice, "gold", "subscribe", None)
            .await
            .unwrap();

        let req = RejectRequestRequest {
            request_id: req_id,
            extra: Default::default(),
        };
        let bytes = requests_reject_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect("requests.reject ok");
        let reply: RejectRequestReply = decode_reply(&bytes);
        assert!(reply.rejected);

        let still_there = state.db.get_subscribe_request(req_id).await.unwrap();
        assert!(still_there.is_none(), "request should be deleted");
    }

    #[tokio::test]
    async fn requests_reject_returns_request_not_found_for_unknown_id() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x1C);

        let req = RejectRequestRequest {
            request_id: 9_999_999,
            extra: Default::default(),
        };
        let err = requests_reject_handler()(state, author, encode_req(&req))
            .await
            .expect_err("unknown request must fail");
        assert_eq!(err.code, "fauna.subscriptions.request_not_found");
    }

    // ── delegate.upload ────────────────────────────────────────

    /// Build a `DeviceAuthorization` whose `actor_id` matches `signing`'s
    /// verifying key, signed by `signing`. Returns the embed-as-bytes wire
    /// shape ready to drop into `DelegateUploadRequest.authorization`.
    fn make_signed_delegation(
        signing: &ed25519_dalek::SigningKey,
        device_key: [u8; 32],
        capabilities: Vec<fauna_core::data::Capability>,
    ) -> EmbedAsBytes {
        let actor_pk = signing.verifying_key().to_bytes();
        let auth = DeviceAuthorization {
            actor_id: ActorId(actor_pk),
            device_key,
            capabilities,
            created_at: Timestamp(1_700_000_000_000_000),
            expires_at: None,
        };
        let kp = fauna_core::identity::ActorKeypair::from_secret(signing.to_bytes());
        let (bytes, env) =
            fauna_core::encoding::sign_envelope(&kp, &auth).expect("sign delegation");
        EmbedAsBytes::from_signed(bytes, env)
    }

    /// Read the test fixture's nest signing key out of state, returning the
    /// verifying-key bytes the delegate.upload nest-key check expects.
    fn nest_pubkey(state: &AppState) -> [u8; 32] {
        state
            .nest_signing_key
            .as_ref()
            .expect("test fixture must set nest_signing_key")
            .verifying_key()
            .to_bytes()
    }

    #[tokio::test]
    async fn delegate_upload_stores_verified_authorization() {
        let state = fixture_state_with_nest_key().await;
        let signing = ed25519_dalek::SigningKey::from_bytes(&[0x1Du8; 32]);
        let actor_pk = signing.verifying_key().to_bytes();
        // device_key must match the nest's signing key for the upload to be
        // accepted (post-fix nest-key check).
        let device_key = nest_pubkey(&state);

        let auth_wire = make_signed_delegation(
            &signing,
            device_key,
            vec![fauna_core::data::Capability::ManageSubscribers],
        );
        // Stored bytes are the canonical dag-cbor (`auth_wire.bytes`); the
        // handler verifies the envelope first, then persists `bytes` only.
        let expected_stored = auth_wire.bytes.clone();

        let req = DelegateUploadRequest {
            authorization: auth_wire,
            extra: Default::default(),
        };
        let bytes = delegate_upload_handler()(state.clone(), actor_pk, encode_req(&req))
            .await
            .expect("delegate.upload ok");
        let reply: DelegateUploadReply = decode_reply(&bytes);
        assert!(reply.uploaded);

        let stored = state
            .db
            .get_device_authorization(&actor_pk)
            .await
            .unwrap()
            .expect("delegation stored");
        assert_eq!(stored.as_slice(), expected_stored.as_slice());
    }

    #[tokio::test]
    async fn delegate_upload_rejects_tampered_signature() {
        let state = fixture_state_with_nest_key().await;
        let signing = ed25519_dalek::SigningKey::from_bytes(&[0x1Eu8; 32]);
        let actor_pk = signing.verifying_key().to_bytes();
        // Use the nest pubkey so we reach the signature check (the nest-key
        // and capability gates fire before signature verification).
        let device_key = nest_pubkey(&state);

        let mut auth_wire = make_signed_delegation(
            &signing,
            device_key,
            vec![fauna_core::data::Capability::ManageSubscribers],
        );
        // Flip the last byte of the envelope (in the signature portion).
        // The envelope is [36-byte CID || 64-byte sig]; the last byte sits
        // in the signature tail.
        let last = auth_wire.envelope.len() - 1;
        auth_wire.envelope[last] ^= 0x01;

        let req = DelegateUploadRequest {
            authorization: auth_wire,
            extra: Default::default(),
        };
        let err = delegate_upload_handler()(state, actor_pk, encode_req(&req))
            .await
            .expect_err("tampered signature must fail");
        assert_eq!(err.code, "fauna.subscriptions.invalid_signature");
    }

    #[tokio::test]
    async fn delegate_upload_rejects_when_bearer_is_not_actor() {
        let state = fixture_state_with_nest_key().await;
        let signing = ed25519_dalek::SigningKey::from_bytes(&[0x1Fu8; 32]);
        let device_key = nest_pubkey(&state);
        let intruder = actor_from_seed(0x20);

        let auth_bytes = make_signed_delegation(
            &signing,
            device_key,
            vec![fauna_core::data::Capability::ManageSubscribers],
        );

        let req = DelegateUploadRequest {
            authorization: auth_bytes,
            extra: Default::default(),
        };
        let err = delegate_upload_handler()(state, intruder, encode_req(&req))
            .await
            .expect_err("foreign bearer must fail");
        assert_eq!(err.code, "fauna.subscriptions.permission_denied");
    }

    #[tokio::test]
    async fn delegate_upload_rejects_when_device_key_is_not_nest_pubkey() {
        let state = fixture_state_with_nest_key().await;
        let signing = ed25519_dalek::SigningKey::from_bytes(&[0x21u8; 32]);
        let actor_pk = signing.verifying_key().to_bytes();
        // Pick an Ed25519 verifying key that is *not* the nest's pubkey.
        let other_device = ed25519_dalek::SigningKey::from_bytes(&[0x99u8; 32])
            .verifying_key()
            .to_bytes();
        assert_ne!(other_device, nest_pubkey(&state));

        let auth_bytes = make_signed_delegation(
            &signing,
            other_device,
            vec![fauna_core::data::Capability::ManageSubscribers],
        );

        let req = DelegateUploadRequest {
            authorization: auth_bytes,
            extra: Default::default(),
        };
        let err = delegate_upload_handler()(state, actor_pk, encode_req(&req))
            .await
            .expect_err("non-nest device_key must fail");
        assert_eq!(err.code, "fauna.subscriptions.invalid_delegation");
    }

    #[tokio::test]
    async fn delegate_upload_rejects_when_manage_subscribers_capability_missing() {
        let state = fixture_state_with_nest_key().await;
        let signing = ed25519_dalek::SigningKey::from_bytes(&[0x22u8; 32]);
        let actor_pk = signing.verifying_key().to_bytes();
        let device_key = nest_pubkey(&state);

        // Capabilities present but ManageSubscribers / All not among them.
        let auth_bytes = make_signed_delegation(
            &signing,
            device_key,
            vec![fauna_core::data::Capability::Post],
        );

        let req = DelegateUploadRequest {
            authorization: auth_bytes,
            extra: Default::default(),
        };
        let err = delegate_upload_handler()(state, actor_pk, encode_req(&req))
            .await
            .expect_err("missing ManageSubscribers must fail");
        assert_eq!(err.code, "fauna.subscriptions.invalid_delegation");
    }

    // ── subscribe ──────────────────────────────────────────────

    #[tokio::test]
    async fn subscribe_without_auto_approve_returns_queued() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x42);
        let subscriber = actor_from_seed(0x43);

        state
            .db
            .create_subscription_tier(
                &author, "gold", 1, None, None, None, false, None, None, false,
            )
            .await
            .unwrap();

        let req = SubscribeRequest {
            author_id: ActorId(author),
            tier: "gold".into(),
            mlkem_encaps_key: None,
            extra: Default::default(),
        };
        let bytes = subscribe_handler()(state.clone(), subscriber, encode_req(&req))
            .await
            .expect("subscribe queues");
        let reply: SubscribeReply = decode_reply(&bytes);
        let request_id = match reply {
            SubscribeReply::Queued { request_id } => request_id,
            other => panic!("expected Queued, got {other:?}"),
        };

        let rows = state.db.list_subscribe_requests(&author).await.unwrap();
        let row = rows
            .iter()
            .find(|r| r.id == request_id)
            .expect("the inserted row should be listable");
        assert_eq!(row.kind, "subscribe");
        assert_eq!(row.tier_name, "gold");
        assert_eq!(row.subscriber_id.as_slice(), subscriber.as_slice());

        // No subscriber row yet — author must approve.
        assert!(
            !state
                .db
                .is_subscriber(&author, &subscriber, "gold")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    /// A tier still enqueues with `auto_approve = true` — the nest holds no key
    /// it could fan out, so only the author's client can grant the
    /// subscription.
    async fn subscribe_auto_approve_tier_still_queues() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x44);
        let subscriber = actor_from_seed(0x45);

        // auto_approve=true → still enqueues.
        state
            .db
            .create_subscription_tier(
                &author, "gold", 1, None, None, None, true, None, None, false,
            )
            .await
            .unwrap();

        let req = SubscribeRequest {
            author_id: ActorId(author),
            tier: "gold".into(),
            mlkem_encaps_key: None,
            extra: Default::default(),
        };
        let bytes = subscribe_handler()(state.clone(), subscriber, encode_req(&req))
            .await
            .expect("subscribe.encrypted.queued ok");
        let reply: SubscribeReply = decode_reply(&bytes);
        match reply {
            SubscribeReply::Queued { request_id } => {
                assert!(
                    request_id > 0,
                    "queued request_id should be a real row id, got {request_id}"
                );
            }
            other => {
                panic!("a client-minted tier must enqueue even with auto_approve, got {other:?}")
            }
        }

        // No server-side mint — subscriber stays out of the roster.
        assert!(
            !state
                .db
                .is_subscriber(&author, &subscriber, "gold")
                .await
                .unwrap(),
            "a client-minted tier must not auto-add the subscriber server-side"
        );
    }

    #[tokio::test]
    async fn subscribe_already_subscribed_returns_approved_idempotent() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x46);
        let subscriber = actor_from_seed(0x47);

        state
            .db
            .create_subscription_tier(
                &author, "gold", 1, None, None, None, false, None, None, false,
            )
            .await
            .unwrap();
        state
            .db
            .add_subscriber(&author, &subscriber, "gold", None)
            .await
            .unwrap();

        let req = SubscribeRequest {
            author_id: ActorId(author),
            tier: "gold".into(),
            mlkem_encaps_key: None,
            extra: Default::default(),
        };
        let bytes = subscribe_handler()(state.clone(), subscriber, encode_req(&req))
            .await
            .expect("subscribe.plaintext.idempotent ok");
        let reply: SubscribeReply = decode_reply(&bytes);
        match reply {
            SubscribeReply::Approved { tier, .. } => assert_eq!(tier, "gold"),
            other => panic!("expected Approved on already-subscribed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn subscribe_returns_tier_not_found() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x48);
        let subscriber = actor_from_seed(0x49);

        let req = SubscribeRequest {
            author_id: ActorId(author),
            tier: "ghost".into(),
            mlkem_encaps_key: None,
            extra: Default::default(),
        };
        let err = subscribe_handler()(state, subscriber, encode_req(&req))
            .await
            .expect_err("unknown tier must fail");
        assert_eq!(err.code, "fauna.subscriptions.tier_not_found");
    }

    // ── unsubscribe ────────────────────────────────────────────

    /// **Unsubscribe never unfollows.** It is scoped to paid tiers (rank >= 1);
    /// the free rank-0 `followers` tier survives, so cancelling a subscription
    /// leaves the follow intact (`monetization.md` § Pillar 1): no leave is
    /// enqueued for it, so the author client's removal never reaches it.
    #[tokio::test]
    async fn unsubscribe_leaves_the_free_followers_tier_intact() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x4E);
        let subscriber = actor_from_seed(0x4F);

        // A follow (free rank-0 tier) plus a paid subscription.
        state
            .db
            .create_subscription_tier(
                &author,
                FOLLOWERS_TIER,
                FOLLOWERS_TIER_RANK,
                None,
                None,
                None,
                true,
                None,
                None,
                false,
            )
            .await
            .unwrap();
        state
            .db
            .create_subscription_tier(
                &author, "gold", 1, None, None, None, false, None, None, false,
            )
            .await
            .unwrap();
        for tier in [FOLLOWERS_TIER, "gold"] {
            state
                .db
                .add_subscriber(&author, &subscriber, tier, None)
                .await
                .unwrap();
        }

        let req = UnsubscribeRequest {
            author_id: ActorId(author),
            extra: Default::default(),
        };
        let bytes = unsubscribe_handler()(state.clone(), subscriber, encode_req(&req))
            .await
            .expect("unsubscribe ok");
        assert!(matches!(
            decode_reply::<UnsubscribeReply>(&bytes),
            UnsubscribeReply::Queued { .. }
        ));

        let rows = state.db.list_subscribe_requests(&author).await.unwrap();
        let leaves: Vec<&str> = rows
            .iter()
            .filter(|r| r.kind == "unsubscribe")
            .map(|r| r.tier_name.as_str())
            .collect();
        assert_eq!(
            leaves,
            vec!["gold"],
            "only the paid tier is queued to leave — the free followers tier \
             must SURVIVE, unsubscribe is not unfollow"
        );
    }

    #[tokio::test]
    /// A **client-minted** tier can only be rotated by the author's client, so
    /// leaving it enqueues an intent rather than removing on the spot.
    async fn unsubscribe_client_minted_tier_enqueues_intent() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x4C);
        let subscriber = actor_from_seed(0x4D);

        state
            .db
            .create_subscription_tier(
                &author, "gold", 1, None, None, None, false, None, None, false,
            )
            .await
            .unwrap();
        // Seed the subscriber row directly (subscribers ordinarily land via
        // requests.approve; here we just need an active roster entry to enqueue
        // an unsubscribe against). No period key → client-minted tier.
        state
            .db
            .add_subscriber(&author, &subscriber, "gold", None)
            .await
            .unwrap();

        let req = UnsubscribeRequest {
            author_id: ActorId(author),
            extra: Default::default(),
        };
        let bytes = unsubscribe_handler()(state.clone(), subscriber, encode_req(&req))
            .await
            .expect("unsubscribe enqueues");
        let reply: UnsubscribeReply = decode_reply(&bytes);
        let request_id = match reply {
            UnsubscribeReply::Queued { request_id } => request_id,
            other => panic!("expected Queued on a client-minted tier, got {other:?}"),
        };

        // Row is in subscribe_requests with kind='unsubscribe'.
        let rows = state.db.list_subscribe_requests(&author).await.unwrap();
        let row = rows
            .iter()
            .find(|r| r.id == request_id)
            .expect("returned request_id must be listable");
        assert_eq!(row.kind, "unsubscribe");
        assert_eq!(row.tier_name, "gold");
        assert_eq!(row.subscriber_id.as_slice(), subscriber.as_slice());

        // Subscriber stays in the roster until the author's client commits
        // the removal (Task 10).
        assert!(
            state
                .db
                .is_subscriber(&author, &subscriber, "gold")
                .await
                .unwrap(),
            "leaving a client-minted tier must not mutate the roster server-side"
        );
    }

    #[tokio::test]
    /// `fauna.subscriptions.unsubscribe` is `forbid_replay: false`, which
    /// `transport.md:241-244` makes an assertion that the handler is naturally
    /// idempotent under a repeated same-key call — and `request_auto_retry`
    /// re-issues on a *fresh* connection whose idempotency cache is empty, so
    /// the handler really does run twice. The client-minted arm enqueues an
    /// intent; the subscriber deliberately stays in the roster until the
    /// author's client commits, so the retry sees the identical input and must
    /// return the identical `Queued` reply off the one existing row — never a
    /// second row, and never an opaque storage error for an unsubscribe that
    /// already succeeded.
    async fn unsubscribe_replay_returns_the_same_queued_row() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x5A);
        let subscriber = actor_from_seed(0x5B);

        state
            .db
            .create_subscription_tier(
                &author, "gold", 1, None, None, None, false, None, None, false,
            )
            .await
            .unwrap();
        state
            .db
            .add_subscriber(&author, &subscriber, "gold", None)
            .await
            .unwrap();

        let req = UnsubscribeRequest {
            author_id: ActorId(author),
            extra: Default::default(),
        };

        let first: UnsubscribeReply = decode_reply(
            &unsubscribe_handler()(state.clone(), subscriber, encode_req(&req))
                .await
                .expect("first unsubscribe enqueues"),
        );
        let first_id = match first {
            UnsubscribeReply::Queued { request_id } => request_id,
            other => panic!("expected Queued on a client-minted tier, got {other:?}"),
        };

        // The auto-retry: same actor, same request, fresh connection.
        let replay: UnsubscribeReply = decode_reply(
            &unsubscribe_handler()(state.clone(), subscriber, encode_req(&req))
                .await
                .expect("a replayed unsubscribe must not fail — the first one succeeded"),
        );
        let replay_id = match replay {
            UnsubscribeReply::Queued { request_id } => request_id,
            other => panic!("expected the same Queued reply on replay, got {other:?}"),
        };

        assert_eq!(
            first_id, replay_id,
            "the replay must name the row the first call created, not a new one"
        );
        let unsub_rows: Vec<_> = state
            .db
            .list_subscribe_requests(&author)
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.kind == "unsubscribe" && r.tier_name == "gold")
            .collect();
        assert_eq!(
            unsub_rows.len(),
            1,
            "a replayed unsubscribe must leave exactly one enqueued intent"
        );
    }

    #[tokio::test]
    async fn unsubscribe_not_subscribed_errors() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x4E);
        let stranger = actor_from_seed(0x4F);

        // No tiers, no subscriptions.
        let req = UnsubscribeRequest {
            author_id: ActorId(author),
            extra: Default::default(),
        };
        let err = unsubscribe_handler()(state, stranger, encode_req(&req))
            .await
            .expect_err("unsubscribe with no paid subscriptions must error");
        assert_eq!(err.code, "fauna.subscriptions.not_subscribed");
    }

    // ── requests.approve / subscribers.remove ──────────────────

    use fauna_core::data::Capability;
    use fauna_core::encoding::{EmbedAsBytes, sign_envelope};
    use fauna_core::identity::ActorKeypair;
    use fauna_core::subscription::crypto::mint_key_blob;
    use fauna_protocol::subscriptions::EncryptedKeyBlobUpload;

    /// Build a self-signed `DeviceAuthorization` granting `ManageSubscribers`
    /// to the same keypair (single-device author case). Returns
    /// `(auth, bytes, env)` for the embed-as-bytes wire shape callers.
    fn self_signed_auth(
        kp: &ActorKeypair,
    ) -> (DeviceAuthorization, Vec<u8>, fauna_cbor::SignedEnvelope) {
        let auth = DeviceAuthorization {
            actor_id: kp.actor_id(),
            device_key: kp.actor_id().0,
            capabilities: vec![Capability::ManageSubscribers],
            created_at: Timestamp(1_700_000_000_000_000),
            expires_at: None,
        };
        let (bytes, env) = sign_envelope(kp, &auth).expect("sign auth");
        (auth, bytes, env)
    }

    /// Mint a KeyBlob via the shared-Rust primitive and package it as a
    /// ready-to-attach `EncryptedKeyBlobUpload`.
    fn mint_upload(
        signer: &ActorKeypair,
        signer_auth: &DeviceAuthorization,
        signer_auth_bytes: &[u8],
        signer_auth_env: &fauna_cbor::SignedEnvelope,
        tier: &str,
        rotated_at: Timestamp,
        subscribers: &[ActorId],
        wrapped_key: &[u8; 32],
    ) -> EncryptedKeyBlobUpload {
        let minted = mint_key_blob(
            signer,
            signer_auth,
            signer_auth_bytes,
            signer_auth_env,
            tier.to_string(),
            rotated_at,
            subscribers,
            &[],
            wrapped_key,
        )
        .expect("mint_key_blob");
        EncryptedKeyBlobUpload {
            key_blob: EmbedAsBytes::from_signed(minted.bytes, minted.envelope),
            signer_auth: EmbedAsBytes {
                envelope: signer_auth_env_bytes(signer_auth_env),
                bytes: signer_auth_bytes.to_vec(),
                signer_auth: None,
            },
            extra: Default::default(),
        }
    }

    /// Flat 100-byte serialization of a `SignedEnvelope` (36-byte CID || 64-byte sig)
    /// for embedding alongside the canonical bytes.
    fn signer_auth_env_bytes(env: &fauna_cbor::SignedEnvelope) -> Vec<u8> {
        let mut out = Vec::with_capacity(100);
        out.extend_from_slice(env.cid().as_bytes());
        out.extend_from_slice(env.sig());
        out
    }

    #[tokio::test]
    async fn approve_request_accepts_client_minted_keyblob() {
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x50u8; 32]);
        let subscriber_kp = ActorKeypair::from_secret([0x51u8; 32]);
        let author = author_kp.actor_id().0;
        let subscriber = subscriber_kp.actor_id().0;

        create_tier(&state, 0x50, "tier1", 1).await;
        let request_id = state
            .db
            .insert_subscribe_request(&author, &subscriber, "tier1", "subscribe", None)
            .await
            .unwrap();

        let (auth, auth_bytes, auth_env) = self_signed_auth(&author_kp);
        let upload = mint_upload(
            &author_kp,
            &auth,
            &auth_bytes,
            &auth_env,
            "tier1",
            Timestamp(1_700_000_100_000_000),
            &[subscriber_kp.actor_id()],
            &[0x77u8; 32],
        );

        let req = ApproveRequestRequest {
            request_id,
            encrypted_upload: Some(upload),
            extra: Default::default(),
        };
        let bytes = requests_approve_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect("encrypted approve ok");
        let reply: ApproveRequestReply = decode_reply(&bytes);
        assert_eq!(reply.key_version, 2);
        assert_eq!(reply.tier, "tier1");
        assert_eq!(reply.subscriber.0, subscriber);

        // Roster mutation + request deletion + blob round-trips.
        assert!(
            state
                .db
                .is_subscriber(&author, &subscriber, "tier1")
                .await
                .unwrap()
        );
        assert!(
            state
                .db
                .get_subscribe_request(request_id)
                .await
                .unwrap()
                .is_none()
        );
        let (version, _hash, blob_data) = state
            .db
            .get_current_key_blob(&author, "tier1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(version, 2);
        let wire: EmbedAsBytes = canonical_decode(&blob_data).unwrap();
        let stored: KeyBlob = decode_signed_bytes(&wire.bytes).unwrap();
        assert_eq!(stored.entries.len(), 1);
        assert_eq!(stored.entries[0].subscriber.0, subscriber);
        assert_eq!(stored.tier, "tier1");
    }

    /// A key blob carrying an entry wrapped with a KEM suite this build does
    /// not name is still a well-formed upload: the nest verifies it over the
    /// carried bytes and stores it (`post-quantum.md` § 7.1, the
    /// `KeyBlobEntry` bullet). Before the unknown arm, the decode failed and
    /// the upload was refused as malformed.
    #[tokio::test]
    async fn approve_request_stores_a_keyblob_with_an_unknown_suite_entry() {
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x52u8; 32]);
        let subscriber_kp = ActorKeypair::from_secret([0x53u8; 32]);
        let author = author_kp.actor_id().0;
        let subscriber = subscriber_kp.actor_id().0;

        create_tier(&state, 0x52, "tier1", 1).await;
        let request_id = state
            .db
            .insert_subscribe_request(&author, &subscriber, "tier1", "subscribe", None)
            .await
            .unwrap();

        let (auth, auth_bytes, auth_env) = self_signed_auth(&author_kp);
        let mut upload = mint_upload(
            &author_kp,
            &auth,
            &auth_bytes,
            &auth_env,
            "tier1",
            Timestamp(1_700_000_100_000_000),
            &[subscriber_kp.actor_id()],
            &[0x77u8; 32],
        );
        // The newer author wrapped this entry with a suite this build lacks.
        let (bytes, _env) = upload.key_blob.clone().into_signed().unwrap();
        let mut newer: fauna_cbor::Value = canonical_decode(&bytes).unwrap();
        let fauna_cbor::Value::Map(blob) = &mut newer else {
            panic!("a key blob is a map");
        };
        let Some(fauna_cbor::Value::List(entries)) = blob.get_mut("entries") else {
            panic!("entries is a list");
        };
        let fauna_cbor::Value::Map(entry) = &mut entries[0] else {
            panic!("an entry is a map");
        };
        entry.insert(
            "suite".into(),
            fauna_cbor::Value::String("MlKem1024".into()),
        );
        let (bytes, env) =
            fauna_cbor::SignedEnvelope::sign(&newer, author_kp.signing_key()).unwrap();
        upload.key_blob = EmbedAsBytes::from_signed(bytes, env);

        let req = ApproveRequestRequest {
            request_id,
            encrypted_upload: Some(upload),
            extra: Default::default(),
        };
        requests_approve_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect("an unknown-suite entry is a well-formed upload");

        let (_version, _hash, blob_data) = state
            .db
            .get_current_key_blob(&author, "tier1")
            .await
            .unwrap()
            .unwrap();
        let wire: EmbedAsBytes = canonical_decode(&blob_data).unwrap();
        let stored: KeyBlob = decode_signed_bytes(&wire.bytes).unwrap();
        assert_eq!(
            stored.entries[0].suite,
            fauna_core::subscription::types::KemSuiteId::Unknown
        );
    }

    #[tokio::test]
    /// A tier cannot be approved without an upload — the nest has no key to
    /// wrap.
    async fn approve_request_requires_an_upload() {
        let state = fixture_state().await;
        let author = actor_from_seed(0x54);
        let subscriber = actor_from_seed(0x55);

        create_tier(&state, 0x54, "tier1", 1).await;
        let request_id = state
            .db
            .insert_subscribe_request(&author, &subscriber, "tier1", "subscribe", None)
            .await
            .unwrap();

        let req = ApproveRequestRequest {
            request_id,
            encrypted_upload: None,
            extra: Default::default(),
        };
        let err = requests_approve_handler()(state, author, encode_req(&req))
            .await
            .expect_err("encrypted approve without upload must fail");
        assert_eq!(err.code, "fauna.subscriptions.missing_upload");
    }

    /// **A tier with no stored blob is refused, never accepted as a first
    /// upload** — save the lazily provisioned `followers` tier, whose first
    /// blob rides the first follow approval. Every other tier holds its birth
    /// blob from `tiers.create`, so a blob-less one is a state no writer
    /// produces; the precheck that used to skip the monotonic check for one now
    /// refuses it, at every upload door, before anything is written.
    #[tokio::test]
    async fn the_upload_doors_refuse_a_tier_with_no_stored_blob() {
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x5Fu8; 32]);
        let subscriber_kp = ActorKeypair::from_secret([0x75u8; 32]);
        let author = author_kp.actor_id().0;
        let subscriber = subscriber_kp.actor_id().0;
        // The follow below lazily provisions `followers`, which the nest does
        // only for an author it hosts.
        state
            .db
            .create_user(&author, "free", "author")
            .await
            .unwrap();

        // Straight into the DB: the one way to reach a blob-less tier.
        state
            .db
            .create_subscription_tier(
                &author, "tier1", 1, None, None, None, false, None, None, false,
            )
            .await
            .unwrap();
        let request_id = state
            .db
            .insert_subscribe_request(&author, &subscriber, "tier1", "subscribe", None)
            .await
            .unwrap();

        let (auth, auth_bytes, auth_env) = self_signed_auth(&author_kp);
        let approve = ApproveRequestRequest {
            request_id,
            encrypted_upload: Some(mint_upload(
                &author_kp,
                &auth,
                &auth_bytes,
                &auth_env,
                "tier1",
                Timestamp(1_700_000_100_000_000),
                &[subscriber_kp.actor_id()],
                &[0x77u8; 32],
            )),
            extra: Default::default(),
        };
        let err = requests_approve_handler()(state.clone(), author, encode_req(&approve))
            .await
            .expect_err("approve on a blob-less tier must fail");
        assert_eq!(err.code, "fauna.subscriptions.key_blob_not_found");

        let rotate = RotateKeyBlobRequest {
            tier_name: "tier1".into(),
            encrypted_upload: mint_upload(
                &author_kp,
                &auth,
                &auth_bytes,
                &auth_env,
                "tier1",
                Timestamp(1_700_000_100_000_000),
                &[],
                &[0x78u8; 32],
            ),
            extra: Default::default(),
        };
        let err = key_blob_rotate_handler()(state.clone(), author, encode_req(&rotate))
            .await
            .expect_err("rotate on a blob-less tier must fail");
        assert_eq!(err.code, "fauna.subscriptions.key_blob_not_found");

        // Nothing landed: no roster row, no blob, the request still pending.
        assert!(
            !state
                .db
                .is_subscriber(&author, &subscriber, "tier1")
                .await
                .unwrap()
        );
        assert!(
            state
                .db
                .get_current_key_blob(&author, "tier1")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            state
                .db
                .get_subscribe_request(request_id)
                .await
                .unwrap()
                .is_some()
        );

        // The positive control: a first follow provisions `followers` with no
        // blob, and its approval lands the tier's first blob at version 1.
        let follow = SubscribeRequest {
            author_id: ActorId(author),
            tier: FOLLOWERS_TIER.into(),
            mlkem_encaps_key: None,
            extra: Default::default(),
        };
        let bytes = subscribe_handler()(state.clone(), subscriber, encode_req(&follow))
            .await
            .expect("follow ok");
        let SubscribeReply::Queued { request_id } = decode_reply(&bytes) else {
            panic!("a follow queues for the author's client");
        };
        assert!(
            state
                .db
                .get_current_key_blob(&author, FOLLOWERS_TIER)
                .await
                .unwrap()
                .is_none(),
            "the lazy provision writes the row alone"
        );
        let approve = ApproveRequestRequest {
            request_id,
            encrypted_upload: Some(mint_upload(
                &author_kp,
                &auth,
                &auth_bytes,
                &auth_env,
                FOLLOWERS_TIER,
                Timestamp(1_700_000_100_000_000),
                &[subscriber_kp.actor_id()],
                &[0x79u8; 32],
            )),
            extra: Default::default(),
        };
        let bytes = requests_approve_handler()(state.clone(), author, encode_req(&approve))
            .await
            .expect("the lazily provisioned followers tier takes its first blob");
        assert_eq!(decode_reply::<ApproveRequestReply>(&bytes).key_version, 1);
    }

    #[tokio::test]
    async fn approve_request_rejects_stale_rotated_at() {
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x58u8; 32]);
        let subscriber_kp = ActorKeypair::from_secret([0x59u8; 32]);
        let author = author_kp.actor_id().0;
        let subscriber = subscriber_kp.actor_id().0;

        create_tier(&state, 0x58, "tier1", 1).await;
        let request_id = state
            .db
            .insert_subscribe_request(&author, &subscriber, "tier1", "subscribe", None)
            .await
            .unwrap();

        // Seed a prior key_blob at T (no subscribers yet). The stored payload
        // is the dag-cbor-encoded `EmbedAsBytes` wire shape that the handler
        // expects (matches `encrypted_approve`'s mutation step).
        let (auth, auth_bytes, auth_env) = self_signed_auth(&author_kp);
        let prior_rotated = Timestamp(1_700_000_100_000_000);
        let prior_minted = mint_key_blob(
            &author_kp,
            &auth,
            &auth_bytes,
            &auth_env,
            "tier1".to_string(),
            prior_rotated,
            &[],
            &[],
            &[0u8; 32],
        )
        .unwrap();
        let prior_wire =
            EmbedAsBytes::from_signed(prior_minted.bytes.clone(), prior_minted.envelope);
        let prior_stored = fauna_core::encoding::canonical_encode(&prior_wire).unwrap();
        let prior_hash: [u8; 32] = *blake3::hash(&prior_minted.bytes).as_bytes();
        state
            .db
            .upsert_current_key_blob(&author, "tier1", 1, &prior_hash, &prior_stored)
            .await
            .unwrap();

        // Upload with rotated_at == T (equal, not strictly greater).
        let upload = mint_upload(
            &author_kp,
            &auth,
            &auth_bytes,
            &auth_env,
            "tier1",
            prior_rotated,
            &[subscriber_kp.actor_id()],
            &[0x77u8; 32],
        );
        let req = ApproveRequestRequest {
            request_id,
            encrypted_upload: Some(upload),
            extra: Default::default(),
        };
        let err = requests_approve_handler()(state, author, encode_req(&req))
            .await
            .expect_err("equal rotated_at must fail");
        assert_eq!(err.code, "fauna.subscriptions.stale_rotation");
        // Details mention the stored timestamp.
        if let Some(Value::String(s)) = err.details.as_deref() {
            assert!(s.contains("stored rotated_at"), "details: {s}");
        } else {
            panic!("stale_rotation must carry details text");
        }
    }

    #[tokio::test]
    async fn approve_request_rejects_roster_mismatch() {
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x5Au8; 32]);
        let subscriber_kp = ActorKeypair::from_secret([0x5Bu8; 32]);
        let other_kp = ActorKeypair::from_secret([0x5Cu8; 32]);
        let author = author_kp.actor_id().0;
        let subscriber = subscriber_kp.actor_id().0;

        create_tier(&state, 0x5A, "tier1", 1).await;
        let request_id = state
            .db
            .insert_subscribe_request(&author, &subscriber, "tier1", "subscribe", None)
            .await
            .unwrap();

        // Mint a blob covering an unrelated actor (not the subscriber the
        // request is for) — roster check expects {subscriber} but the blob
        // covers {other}.
        let (auth, auth_bytes, auth_env) = self_signed_auth(&author_kp);
        let upload = mint_upload(
            &author_kp,
            &auth,
            &auth_bytes,
            &auth_env,
            "tier1",
            Timestamp(1_700_000_100_000_000),
            &[other_kp.actor_id()],
            &[0x77u8; 32],
        );
        let req = ApproveRequestRequest {
            request_id,
            encrypted_upload: Some(upload),
            extra: Default::default(),
        };
        let err = requests_approve_handler()(state, author, encode_req(&req))
            .await
            .expect_err("roster mismatch must fail");
        assert_eq!(err.code, "fauna.subscriptions.roster_mismatch");
        if let Some(Value::String(s)) = err.details.as_deref() {
            assert!(s.contains("expected"), "details: {s}");
        } else {
            panic!("roster_mismatch must carry details text");
        }
    }

    #[tokio::test]
    async fn approve_request_rejects_tier_mismatch() {
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x5Du8; 32]);
        let subscriber_kp = ActorKeypair::from_secret([0x5Eu8; 32]);
        let author = author_kp.actor_id().0;
        let subscriber = subscriber_kp.actor_id().0;

        // The actual tier and request are for "tier1".
        create_tier(&state, 0x5D, "tier1", 1).await;
        // Also seed "othertier" so mint_key_blob's auth-check passes
        // (mint_key_blob doesn't validate the tier name against any state;
        // we just need the blob to carry a different tier than the request).
        let request_id = state
            .db
            .insert_subscribe_request(&author, &subscriber, "tier1", "subscribe", None)
            .await
            .unwrap();

        let (auth, auth_bytes, auth_env) = self_signed_auth(&author_kp);
        let upload = mint_upload(
            &author_kp,
            &auth,
            &auth_bytes,
            &auth_env,
            "othertier", // wrong tier name in the blob
            Timestamp(1_700_000_100_000_000),
            &[subscriber_kp.actor_id()],
            &[0x77u8; 32],
        );
        let req = ApproveRequestRequest {
            request_id,
            encrypted_upload: Some(upload),
            extra: Default::default(),
        };
        let err = requests_approve_handler()(state, author, encode_req(&req))
            .await
            .expect_err("tier_name mismatch must fail");
        assert_eq!(err.code, "fauna.subscriptions.tier_mismatch");
    }

    #[tokio::test]
    async fn approve_request_rejects_tier_not_found() {
        // Spec § Author-initiated upload matrix: tier doesn't exist →
        // `fauna.subscriptions.tier_not_found`. Distinct from `tier_mismatch`
        // (which requires the tier to exist but disagrees with the blob's
        // tier name).
        //
        // `delete_subscription_tier` cascades through `subscribe_requests`
        // (no `ON DELETE CASCADE` — the helper does it explicitly to satisfy
        // the FK), so we can't just "delete the tier after inserting the
        // request". Instead we drop the subscribe_requests FK constraint
        // for this fixture and write the request row directly — a
        // structurally identical row to one the subscribe
        // path would produce, just no FK-checked tier behind it.
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x68u8; 32]);
        let subscriber_kp = ActorKeypair::from_secret([0x69u8; 32]);
        let author = author_kp.actor_id().0;
        let subscriber = subscriber_kp.actor_id().0;

        // Bypass FK + insert directly. PRAGMA foreign_keys is ON for the
        // pool but defer-checked per-statement; toggling it OFF locally
        // lets the test simulate the "tier vanished after request" race.
        let request_id: i64 = {
            let conn = state.db.conn().await;
            conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
            conn.execute(
                "INSERT INTO subscribe_requests \
                 (author_id, subscriber_id, tier_name, created_at, kind) \
                 VALUES (?1, ?2, ?3, ?4, 'subscribe')",
                rusqlite::params![author.as_slice(), subscriber.as_slice(), "tier1", 0i64],
            )
            .unwrap();
            let id = conn.last_insert_rowid();
            conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
            id
        };

        let (auth, auth_bytes, auth_env) = self_signed_auth(&author_kp);
        let upload = mint_upload(
            &author_kp,
            &auth,
            &auth_bytes,
            &auth_env,
            "tier1",
            Timestamp(1_700_000_100_000_000),
            &[subscriber_kp.actor_id()],
            &[0x77u8; 32],
        );
        let req = ApproveRequestRequest {
            request_id,
            encrypted_upload: Some(upload),
            extra: Default::default(),
        };
        let err = requests_approve_handler()(state, author, encode_req(&req))
            .await
            .expect_err("tier_not_found must fail");
        assert_eq!(err.code, "fauna.subscriptions.tier_not_found");
    }

    /// The `requests.approve` guard is the enqueue-side hidden-tier refusal's
    /// backstop (`monetization.md` § The unifying model → *A tier may be
    /// hidden*, ruling 4): `subscribe` already refuses a hidden tier and
    /// enqueues nothing (`subscribe_to_a_hidden_tier_is_tier_not_found_and_enqueues_nothing`),
    /// so this pin bypasses that door — inserting the pending row directly —
    /// to simulate the one way a hidden-tier request can still reach this
    /// handler: a future regression at the `subscribe`/`tiers.create` doors, or a
    /// non-conforming path. Removing the guard added in
    /// `requests_approve_handler` must red this.
    #[tokio::test]
    async fn approve_request_rejects_a_hidden_tier() {
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x7Au8; 32]);
        let subscriber_kp = ActorKeypair::from_secret([0x7Bu8; 32]);
        let author = author_kp.actor_id().0;
        let subscriber = subscriber_kp.actor_id().0;

        let create = TierCreateRequest {
            name: "backstage".into(),
            rank: 5,
            auto_approve: false,
            hidden: true,
            ..tier_create(0x7A, "backstage", 5)
        };
        tiers_create_handler()(state.clone(), author, encode_req(&create))
            .await
            .expect("tiers.create ok");

        let request_id = state
            .db
            .insert_subscribe_request(&author, &subscriber, "backstage", "subscribe", None)
            .await
            .unwrap();

        let (auth, auth_bytes, auth_env) = self_signed_auth(&author_kp);
        let upload = mint_upload(
            &author_kp,
            &auth,
            &auth_bytes,
            &auth_env,
            "backstage",
            Timestamp(1_700_000_100_000_000),
            &[subscriber_kp.actor_id()],
            &[0x77u8; 32],
        );
        let req = ApproveRequestRequest {
            request_id,
            encrypted_upload: Some(upload),
            extra: Default::default(),
        };
        let err = requests_approve_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect_err("a hidden tier must never be approved");
        assert_eq!(err.code, "fauna.subscriptions.tier_not_found");
        assert!(
            state
                .db
                .list_subscribers(&author, "backstage")
                .await
                .unwrap()
                .is_empty(),
            "no subscriber may be added to a hidden tier via approve"
        );
    }

    /// Build a `DeviceAuthorization` where `author_kp` delegates to a
    /// *separate* `device_kp`. Used to exercise the delegated-device branch
    /// of the auth chain (the blob is minted by the device key, not the
    /// author key directly). Returns `(auth, bytes, env)` for the
    /// embed-as-bytes wire shape callers.
    fn delegated_auth(
        author_kp: &ActorKeypair,
        device_kp: &ActorKeypair,
        caps: Vec<Capability>,
    ) -> (DeviceAuthorization, Vec<u8>, fauna_cbor::SignedEnvelope) {
        let auth = DeviceAuthorization {
            actor_id: author_kp.actor_id(),
            device_key: device_kp.actor_id().0,
            capabilities: caps,
            created_at: Timestamp(1_700_000_000_000_000),
            expires_at: None,
        };
        let (bytes, env) = sign_envelope(author_kp, &auth).expect("sign delegated auth");
        (auth, bytes, env)
    }

    #[tokio::test]
    async fn approve_request_accepts_delegated_device_keyblob() {
        // Spec § Auth chain: a delegated device with `ManageSubscribers` can
        // mint a KeyBlob on the author's behalf. The handler resolves
        // `bearer ∈ {key_blob.author, signer_auth.device_key}` — here the
        // bearer is the author (WS-RPC connection identity), so the first
        // branch fires.
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x6Au8; 32]);
        let device_kp = ActorKeypair::from_secret([0x6Bu8; 32]);
        let subscriber_kp = ActorKeypair::from_secret([0x6Cu8; 32]);
        let author = author_kp.actor_id().0;
        let subscriber = subscriber_kp.actor_id().0;

        create_tier(&state, 0x6A, "tier1", 1).await;
        let request_id = state
            .db
            .insert_subscribe_request(&author, &subscriber, "tier1", "subscribe", None)
            .await
            .unwrap();

        let (auth, auth_bytes, auth_env) =
            delegated_auth(&author_kp, &device_kp, vec![Capability::ManageSubscribers]);
        // Signer is the *device* keypair; the auth proves the author
        // delegated to it.
        let upload = mint_upload(
            &device_kp,
            &auth,
            &auth_bytes,
            &auth_env,
            "tier1",
            Timestamp(1_700_000_100_000_000),
            &[subscriber_kp.actor_id()],
            &[0x77u8; 32],
        );
        let req = ApproveRequestRequest {
            request_id,
            encrypted_upload: Some(upload),
            extra: Default::default(),
        };
        let bytes = requests_approve_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect("delegated-device approve ok");
        let reply: ApproveRequestReply = decode_reply(&bytes);
        assert_eq!(reply.key_version, 2);
        assert_eq!(reply.tier, "tier1");
        assert_eq!(reply.subscriber.0, subscriber);

        assert!(
            state
                .db
                .is_subscriber(&author, &subscriber, "tier1")
                .await
                .unwrap()
        );
        let (_, _, blob_data) = state
            .db
            .get_current_key_blob(&author, "tier1")
            .await
            .unwrap()
            .unwrap();
        let wire: EmbedAsBytes = canonical_decode(&blob_data).unwrap();
        let stored: KeyBlob = decode_signed_bytes(&wire.bytes).unwrap();
        // The blob's `author` field matches the WS-RPC bearer; its `signer`
        // is the delegated device key.
        assert_eq!(stored.author.0, author);
        assert_eq!(stored.signer, device_kp.actor_id().0);
    }

    #[tokio::test]
    async fn approve_request_rejects_malformed_keyblob_bare() {
        // Spec § Auth chain step 1: BARE-decoding the key_blob field fails →
        // `fauna.subscriptions.malformed_upload`. We keep `signer_auth`
        // valid so we cleanly isolate the key_blob decode error.
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x6Du8; 32]);
        let subscriber_kp = ActorKeypair::from_secret([0x6Eu8; 32]);
        let author = author_kp.actor_id().0;
        let subscriber = subscriber_kp.actor_id().0;

        create_tier(&state, 0x6D, "tier1", 1).await;
        let request_id = state
            .db
            .insert_subscribe_request(&author, &subscriber, "tier1", "subscribe", None)
            .await
            .unwrap();

        let (_auth, auth_bytes, auth_env) = self_signed_auth(&author_kp);
        let upload = EncryptedKeyBlobUpload {
            // Garbage bytes — envelope shape valid but key_blob bytes
            // won't decode as a KeyBlob.
            key_blob: EmbedAsBytes {
                envelope: vec![0u8; 100],
                bytes: vec![0xFFu8; 4],
                signer_auth: None,
            },
            signer_auth: EmbedAsBytes {
                envelope: signer_auth_env_bytes(&auth_env),
                bytes: auth_bytes,
                signer_auth: None,
            },
            extra: Default::default(),
        };
        let req = ApproveRequestRequest {
            request_id,
            encrypted_upload: Some(upload),
            extra: Default::default(),
        };
        let err = requests_approve_handler()(state, author, encode_req(&req))
            .await
            .expect_err("malformed key_blob must fail");
        assert_eq!(err.code, "fauna.subscriptions.malformed_upload");
    }

    #[tokio::test]
    async fn approve_request_rejects_malformed_signer_auth_bare() {
        // Spec § Auth chain step 1: BARE-decoding signer_auth fails →
        // `fauna.subscriptions.malformed_upload`. key_blob decode succeeds
        // first (handler order); the failure surfaces from signer_auth.
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x6Fu8; 32]);
        let subscriber_kp = ActorKeypair::from_secret([0x70u8; 32]);
        let author = author_kp.actor_id().0;
        let subscriber = subscriber_kp.actor_id().0;

        create_tier(&state, 0x6F, "tier1", 1).await;
        let request_id = state
            .db
            .insert_subscribe_request(&author, &subscriber, "tier1", "subscribe", None)
            .await
            .unwrap();

        // Mint a valid blob so the key_blob decode succeeds; corrupt only
        // signer_auth.
        let (auth, auth_bytes, auth_env) = self_signed_auth(&author_kp);
        let minted = fauna_core::subscription::crypto::mint_key_blob(
            &author_kp,
            &auth,
            &auth_bytes,
            &auth_env,
            "tier1".to_string(),
            Timestamp(1_700_000_100_000_000),
            &[subscriber_kp.actor_id()],
            &[],
            &[0x77u8; 32],
        )
        .expect("mint blob");
        let upload = EncryptedKeyBlobUpload {
            key_blob: EmbedAsBytes::from_signed(minted.bytes, minted.envelope),
            // Envelope structurally valid (100 bytes of zeros, fails CID
            // prefix decode) → malformed_upload.
            signer_auth: EmbedAsBytes {
                envelope: vec![0u8; 100],
                bytes: vec![0xFFu8; 4],
                signer_auth: None,
            },
            extra: Default::default(),
        };
        let req = ApproveRequestRequest {
            request_id,
            encrypted_upload: Some(upload),
            extra: Default::default(),
        };
        let err = requests_approve_handler()(state, author, encode_req(&req))
            .await
            .expect_err("malformed signer_auth must fail");
        assert_eq!(err.code, "fauna.subscriptions.malformed_upload");
    }

    #[tokio::test]
    async fn approve_request_rejects_missing_capability() {
        // Spec § Auth chain step 2: signer_auth lacks ManageSubscribers (or
        // All), so `verify_key_blob_signature` returns Ok(false) →
        // `fauna.subscriptions.invalid_signature`. We bypass `mint_key_blob`
        // (which enforces the capability check itself) and hand-build a
        // blob signed under an authorization that has no capabilities.
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x71u8; 32]);
        let subscriber_kp = ActorKeypair::from_secret([0x72u8; 32]);
        let author = author_kp.actor_id().0;
        let subscriber = subscriber_kp.actor_id().0;

        create_tier(&state, 0x71, "tier1", 1).await;
        let request_id = state
            .db
            .insert_subscribe_request(&author, &subscriber, "tier1", "subscribe", None)
            .await
            .unwrap();

        // Author self-delegation with empty capabilities — signs cleanly
        // (signature is valid) but `verify_key_blob_signature` rejects on
        // the capability check.
        let auth = DeviceAuthorization {
            actor_id: author_kp.actor_id(),
            device_key: author_kp.actor_id().0,
            capabilities: vec![],
            created_at: Timestamp(1_700_000_000_000_000),
            expires_at: None,
        };
        let (auth_bytes, auth_env) =
            sign_envelope(&author_kp, &auth).expect("sign empty-caps auth");
        // Build + sign the blob directly (bypassing `mint_key_blob`'s
        // capability check).
        let blob = KeyBlob {
            author: author_kp.actor_id(),
            tier: "tier1".into(),
            rotated_at: Timestamp(1_700_000_100_000_000),
            entries: vec![KeyBlobEntry {
                subscriber: subscriber_kp.actor_id(),
                encrypted_key: vec![0x77u8; 32],
                suite: fauna_core::subscription::types::KemSuiteId::Classical,
            }],
            signer: author_kp.actor_id().0,
            key_commitment: [0x6b; 32],
        };
        let (blob_bytes, blob_env) = sign_envelope(&author_kp, &blob).expect("sign blob");

        let upload = EncryptedKeyBlobUpload {
            key_blob: EmbedAsBytes::from_signed(blob_bytes, blob_env),
            signer_auth: EmbedAsBytes::from_signed(auth_bytes, auth_env),
            extra: Default::default(),
        };
        let req = ApproveRequestRequest {
            request_id,
            encrypted_upload: Some(upload),
            extra: Default::default(),
        };
        let err = requests_approve_handler()(state, author, encode_req(&req))
            .await
            .expect_err("missing capability must fail");
        assert_eq!(err.code, "fauna.subscriptions.invalid_signature");
    }

    #[tokio::test]
    async fn approve_request_rejects_tampered_blob_bytes() {
        // Spec § Author-initiated upload matrix row: bytes inside the
        // dag-cbor-encoded blob are mutated after signing. Depending on
        // where the mutation lands the strict canonical decode may reject
        // it outright (malformed_upload), or it may still decode to a valid
        // structure whose signature no longer matches (invalid_signature).
        // Either branch is correct per spec — assert the code is one of
        // `invalid_signature` or `malformed_upload`.
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x73u8; 32]);
        let subscriber_kp = ActorKeypair::from_secret([0x74u8; 32]);
        let author = author_kp.actor_id().0;
        let subscriber = subscriber_kp.actor_id().0;

        create_tier(&state, 0x73, "tier1", 1).await;
        let request_id = state
            .db
            .insert_subscribe_request(&author, &subscriber, "tier1", "subscribe", None)
            .await
            .unwrap();

        let (auth, auth_bytes, auth_env) = self_signed_auth(&author_kp);
        let upload = mint_upload(
            &author_kp,
            &auth,
            &auth_bytes,
            &auth_env,
            "tier1",
            Timestamp(1_700_000_100_000_000),
            &[subscriber_kp.actor_id()],
            &[0x77u8; 32],
        );
        // Mutate one byte in the middle of the canonical key_blob bytes.
        let mut tampered_blob = upload.key_blob.clone();
        let mid = tampered_blob.bytes.len() / 2;
        tampered_blob.bytes[mid] ^= 0x55;
        let tampered = EncryptedKeyBlobUpload {
            key_blob: tampered_blob,
            signer_auth: upload.signer_auth,
            extra: Default::default(),
        };

        let req = ApproveRequestRequest {
            request_id,
            encrypted_upload: Some(tampered),
            extra: Default::default(),
        };
        let err = requests_approve_handler()(state, author, encode_req(&req))
            .await
            .expect_err("tampered blob bytes must fail");
        assert!(
            err.code == "fauna.subscriptions.invalid_signature"
                || err.code == "fauna.subscriptions.malformed_upload",
            "expected invalid_signature or malformed_upload, got {}",
            err.code
        );
    }

    // ── Documented unreachable matrix rows ─────────────────────
    //
    // Two rows of the spec § Author-initiated upload matrix are
    // structurally unreachable through the public dispatch path:
    //
    // 1. **bearer-not-in-chain** (spec § Auth chain step 3:
    //    `bearer ∈ {key_blob.author, signer_auth.device_key}`).
    //    `requests_approve_handler` enforces the request-author match
    //    before delegating to `verify_encrypted_upload`, passing the
    //    bearer as both `bearer` *and* `expected_author`. With
    //    `bearer == expected_author == author_id`, step 3 collapses
    //    into step 4 — the bearer is the request author by
    //    construction. Covered indirectly by the author-scope-mismatch
    //    test above.
    //
    // 2. **author-scope mismatch** (spec § Auth chain step 4:
    //    `key_blob.author == expected_author`). With
    //    `bearer == expected_author`, step 3 already requires
    //    `bearer ∈ {key_blob.author, signer.device_key}`. The only way
    //    to satisfy step 3 without satisfying step 4 is for
    //    `bearer == signer.device_key != key_blob.author`. But
    //    `verify_key_blob_signature` (step 2) enforces
    //    `key_blob.author == signer_auth.actor_id` AND
    //    `key_blob.signer == signer_auth.device_key`, and the dispatch
    //    handler passes the same actor_id for both bearer and
    //    expected_author — so reaching step 4 with a mismatch requires
    //    cooking a blob whose `author != signer_auth.actor_id`, which
    //    step 2 rejects first.
    //
    // The defensive checks are kept in `verify_encrypted_upload` so the
    // helper remains correct if called from a future code path that
    // separates bearer from expected_author (e.g. a server-side
    // authorizing key acting on the author's behalf). Until such a
    // path exists, these rows have no driveable test.

    #[tokio::test]
    async fn remove_subscriber_accepts_client_minted_keyblob() {
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x60u8; 32]);
        let alice_kp = ActorKeypair::from_secret([0x61u8; 32]);
        let bob_kp = ActorKeypair::from_secret([0x62u8; 32]);
        let author = author_kp.actor_id().0;
        let alice = alice_kp.actor_id().0;
        let bob = bob_kp.actor_id().0;

        create_tier(&state, 0x60, "tier1", 1).await;
        state
            .db
            .add_subscriber(&author, &alice, "tier1", None)
            .await
            .unwrap();
        state
            .db
            .add_subscriber(&author, &bob, "tier1", None)
            .await
            .unwrap();

        // Mint a blob with bob removed: entries == {alice}.
        let (auth, auth_bytes, auth_env) = self_signed_auth(&author_kp);
        let upload = mint_upload(
            &author_kp,
            &auth,
            &auth_bytes,
            &auth_env,
            "tier1",
            Timestamp(1_700_000_200_000_000),
            &[alice_kp.actor_id()],
            &[0x88u8; 32],
        );

        let req = RemoveSubscriberRequest {
            tier_name: "tier1".into(),
            subscriber_id: ActorId(bob),
            encrypted_upload: Some(upload),
            extra: Default::default(),
        };
        let bytes = subscribers_remove_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect("encrypted remove ok");
        let reply: RemoveSubscriberReply = decode_reply(&bytes);
        assert_eq!(reply.subscriber.0, bob);
        assert_eq!(reply.tier, "tier1");
        assert_eq!(reply.key_version, 2);

        assert!(
            state
                .db
                .is_subscriber(&author, &alice, "tier1")
                .await
                .unwrap()
        );
        assert!(
            !state
                .db
                .is_subscriber(&author, &bob, "tier1")
                .await
                .unwrap()
        );

        let (_, _, blob_data) = state
            .db
            .get_current_key_blob(&author, "tier1")
            .await
            .unwrap()
            .unwrap();
        let wire: EmbedAsBytes = canonical_decode(&blob_data).unwrap();
        let stored: KeyBlob = decode_signed_bytes(&wire.bytes).unwrap();
        assert_eq!(stored.entries.len(), 1);
        assert_eq!(stored.entries[0].subscriber.0, alice);
    }

    #[tokio::test]
    async fn remove_subscriber_rejects_roster_mismatch() {
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x63u8; 32]);
        let alice_kp = ActorKeypair::from_secret([0x64u8; 32]);
        let bob_kp = ActorKeypair::from_secret([0x65u8; 32]);
        let author = author_kp.actor_id().0;
        let alice = alice_kp.actor_id().0;
        let bob = bob_kp.actor_id().0;

        create_tier(&state, 0x63, "tier1", 1).await;
        state
            .db
            .add_subscriber(&author, &alice, "tier1", None)
            .await
            .unwrap();
        state
            .db
            .add_subscriber(&author, &bob, "tier1", None)
            .await
            .unwrap();

        // Mint a blob that DOESN'T match {alice} (the post-removal roster):
        // include both alice + bob, but we're removing bob → roster should
        // shrink to {alice}.
        let (auth, auth_bytes, auth_env) = self_signed_auth(&author_kp);
        let upload = mint_upload(
            &author_kp,
            &auth,
            &auth_bytes,
            &auth_env,
            "tier1",
            Timestamp(1_700_000_200_000_000),
            &[alice_kp.actor_id(), bob_kp.actor_id()],
            &[0x88u8; 32],
        );

        let req = RemoveSubscriberRequest {
            tier_name: "tier1".into(),
            subscriber_id: ActorId(bob),
            encrypted_upload: Some(upload),
            extra: Default::default(),
        };
        let err = subscribers_remove_handler()(state, author, encode_req(&req))
            .await
            .expect_err("roster mismatch on remove must fail");
        assert_eq!(err.code, "fauna.subscriptions.roster_mismatch");
    }

    // ── key_blob.rotate ────────────────────────────────────────
    //
    // The roster-preserving re-key: the door the post-succession rotation
    // publishes through, and the only one that lands a blob without a
    // membership change.

    #[tokio::test]
    /// The property the whole leg exists for: the roster is untouched, every
    /// member is re-wrapped, and the stored blob now carries the NEW key —
    /// so the next broadcast is sealed under something the predecessor's
    /// custody never held.
    async fn key_blob_rotate_republishes_over_the_unchanged_roster() {
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x90u8; 32]);
        let alice_kp = ActorKeypair::from_secret([0x91u8; 32]);
        let bob_kp = ActorKeypair::from_secret([0x92u8; 32]);
        let author = author_kp.actor_id().0;

        create_tier(&state, 0x90, "tier1", 1).await;
        for s in [alice_kp.actor_id().0, bob_kp.actor_id().0] {
            state
                .db
                .add_subscriber(&author, &s, "tier1", None)
                .await
                .unwrap();
        }

        let (auth, auth_bytes, auth_env) = self_signed_auth(&author_kp);
        let upload = mint_upload(
            &author_kp,
            &auth,
            &auth_bytes,
            &auth_env,
            "tier1",
            Timestamp(1_700_000_300_000_000),
            &[alice_kp.actor_id(), bob_kp.actor_id()],
            &[0xABu8; 32],
        );
        let req = RotateKeyBlobRequest {
            tier_name: "tier1".into(),
            encrypted_upload: upload,
            extra: Default::default(),
        };
        let bytes = key_blob_rotate_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect("rotate ok");
        let reply: RotateKeyBlobReply = decode_reply(&bytes);
        assert_eq!(reply.tier, "tier1");
        assert_eq!(reply.key_version, 2);

        // The roster did not move.
        assert!(
            state
                .db
                .is_subscriber(&author, &alice_kp.actor_id().0, "tier1")
                .await
                .unwrap()
        );
        assert!(
            state
                .db
                .is_subscriber(&author, &bob_kp.actor_id().0, "tier1")
                .await
                .unwrap()
        );
        // And both members can still reach the new key.
        let (_v, _h, blob_data) = state
            .db
            .get_current_key_blob(&author, "tier1")
            .await
            .unwrap()
            .unwrap();
        let wire: EmbedAsBytes = canonical_decode(&blob_data).unwrap();
        let stored: KeyBlob = decode_signed_bytes(&wire.bytes).unwrap();
        assert_eq!(stored.entries.len(), 2);
        assert_eq!(stored.rotated_at.0, 1_700_000_300_000_000);
    }

    #[tokio::test]
    /// A rotation may not quietly shed a paying subscriber (nor admit one who
    /// was never approved) — the roster check is what keeps the re-key from
    /// doubling as an unlogged removal.
    async fn key_blob_rotate_rejects_a_blob_that_drops_a_member() {
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x93u8; 32]);
        let alice_kp = ActorKeypair::from_secret([0x94u8; 32]);
        let bob_kp = ActorKeypair::from_secret([0x95u8; 32]);
        let author = author_kp.actor_id().0;

        create_tier(&state, 0x93, "tier1", 1).await;
        for s in [alice_kp.actor_id().0, bob_kp.actor_id().0] {
            state
                .db
                .add_subscriber(&author, &s, "tier1", None)
                .await
                .unwrap();
        }

        let (auth, auth_bytes, auth_env) = self_signed_auth(&author_kp);
        let upload = mint_upload(
            &author_kp,
            &auth,
            &auth_bytes,
            &auth_env,
            "tier1",
            Timestamp(1_700_000_300_000_000),
            &[alice_kp.actor_id()],
            &[0xACu8; 32],
        );
        let req = RotateKeyBlobRequest {
            tier_name: "tier1".into(),
            encrypted_upload: upload,
            extra: Default::default(),
        };
        let err = key_blob_rotate_handler()(state, author, encode_req(&req))
            .await
            .expect_err("a rotation that drops bob must fail");
        assert_eq!(err.code, "fauna.subscriptions.roster_mismatch");
    }

    #[tokio::test]
    /// Replaying a rotation is refused rather than applied twice — the
    /// consume-shape that earns `forbid_replay: false` on this kind.
    async fn key_blob_rotate_refuses_a_non_advancing_rotated_at() {
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x96u8; 32]);
        let alice_kp = ActorKeypair::from_secret([0x97u8; 32]);
        let author = author_kp.actor_id().0;

        create_tier(&state, 0x96, "tier1", 1).await;
        state
            .db
            .add_subscriber(&author, &alice_kp.actor_id().0, "tier1", None)
            .await
            .unwrap();

        let (auth, auth_bytes, auth_env) = self_signed_auth(&author_kp);
        let mint_at = |ts: u64, key: u8| {
            let upload = mint_upload(
                &author_kp,
                &auth,
                &auth_bytes,
                &auth_env,
                "tier1",
                Timestamp(ts),
                &[alice_kp.actor_id()],
                &[key; 32],
            );
            RotateKeyBlobRequest {
                tier_name: "tier1".into(),
                encrypted_upload: upload,
                extra: Default::default(),
            }
        };

        key_blob_rotate_handler()(
            state.clone(),
            author,
            encode_req(&mint_at(1_700_000_400_000_000, 0xAD)),
        )
        .await
        .expect("first rotation ok");
        let err = key_blob_rotate_handler()(
            state,
            author,
            encode_req(&mint_at(1_700_000_400_000_000, 0xAE)),
        )
        .await
        .expect_err("a replay at the same rotated_at must fail");
        assert_eq!(err.code, "fauna.subscriptions.stale_rotation");
    }

    #[tokio::test]
    /// A tier that never had a subscriber has no stored blob to read an era
    /// stamp off, so the client rotates it once and publishes over the empty
    /// roster — which is what plants the stamp that stops the next pass
    /// repeating. The door has to accept that empty blob for the leg to
    /// terminate.
    async fn key_blob_rotate_accepts_an_empty_roster() {
        let state = fixture_state().await;
        let author_kp = ActorKeypair::from_secret([0x9Au8; 32]);
        let author = author_kp.actor_id().0;

        create_tier(&state, 0x9A, "tier1", 1).await;

        let (auth, auth_bytes, auth_env) = self_signed_auth(&author_kp);
        let upload = mint_upload(
            &author_kp,
            &auth,
            &auth_bytes,
            &auth_env,
            "tier1",
            Timestamp(1_700_000_600_000_000),
            &[],
            &[0xB0u8; 32],
        );
        let req = RotateKeyBlobRequest {
            tier_name: "tier1".into(),
            encrypted_upload: upload,
            extra: Default::default(),
        };
        let bytes = key_blob_rotate_handler()(state.clone(), author, encode_req(&req))
            .await
            .expect("empty-roster rotation ok");
        let reply: RotateKeyBlobReply = decode_reply(&bytes);
        assert_eq!(reply.key_version, 2);

        let (_v, _h, blob_data) = state
            .db
            .get_current_key_blob(&author, "tier1")
            .await
            .unwrap()
            .unwrap();
        let wire: EmbedAsBytes = canonical_decode(&blob_data).unwrap();
        let stored: KeyBlob = decode_signed_bytes(&wire.bytes).unwrap();
        assert!(stored.entries.is_empty());
        // The stamp the client's owed-derivation reads back.
        assert_eq!(stored.author.0, author);
    }
}
