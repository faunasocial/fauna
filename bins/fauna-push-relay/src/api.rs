use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing;

use crate::db::RelayDb;

/// Shared application state.
#[derive(Clone)]
pub struct RelayState {
    db: Arc<RelayDb>,
    push_sender: Arc<dyn crate::push_sender::PushSender>,
}

impl RelayState {
    pub fn new(db: RelayDb, sender: Arc<dyn crate::push_sender::PushSender>) -> Self {
        Self {
            db: Arc::new(db),
            push_sender: sender,
        }
    }

    /// Delegate to the database cleanup.
    pub fn cleanup_expired(&self) -> anyhow::Result<usize> {
        self.db.cleanup_expired_wakes()
    }
}

/// Build the axum router with all relay endpoints.
pub fn relay_router(state: RelayState) -> Router {
    Router::new()
        .route("/v1/push-token", put(put_push_token))
        .route("/v1/push-token", delete(delete_push_token))
        .route("/v1/wake", post(post_wake))
        .route(
            "/v1/endpoint/{nonce}",
            get(get_endpoint).post(post_endpoint),
        )
        .with_state(state)
}

// ---------- request / response types ----------

#[derive(Deserialize)]
struct PushTokenRequest {
    actor_id: String,
    platform: String,
    push_token: String,
    timestamp: u64,
    signature: String,
}

impl PushTokenRequest {
    /// The exact bytes `put`/`delete` sign and verify over, under the caller's
    /// operation domain — kept as one method so the two call sites can never
    /// drift apart on field order.
    fn signed_message(&self, domain: &str) -> Vec<u8> {
        let timestamp = self.timestamp.to_string();
        signing_bytes(&[
            domain,
            &self.actor_id,
            &self.platform,
            &self.push_token,
            &timestamp,
        ])
    }
}

#[derive(Deserialize)]
struct WakeRequest {
    target_actor_id: String,
    requester_actor_id: String,
    requester_endpoint: String,
    timestamp: u64,
    signature: String,
}

#[derive(Serialize, Deserialize)]
struct WakeResponse {
    nonce: String,
}

#[derive(Serialize)]
struct EndpointResponse {
    responder_endpoint: String,
}

#[derive(Deserialize)]
struct ReportEndpointRequest {
    responder_endpoint: String,
}

// ---------- signing encoding ----------

// One domain tag per signed operation. A signature is only ever valid for the
// operation whose tag it was minted under, so an observed `put` request can not
// be replayed as a `delete` (both messages otherwise carry identical fields).
const DOMAIN_PUSH_TOKEN_PUT: &str = "fauna-push-relay/v1/push-token/put";
const DOMAIN_PUSH_TOKEN_DELETE: &str = "fauna-push-relay/v1/push-token/delete";
const DOMAIN_WAKE: &str = "fauna-push-relay/v1/wake";

/// Encode `elements` into the canonical bytes an Ed25519 signature is taken
/// over: each element, the leading domain tag included, is emitted as an 8-byte
/// big-endian length followed by its UTF-8 bytes.
///
/// The length prefix is what makes the encoding **injective** — distinct field
/// tuples always produce distinct bytes. Plain concatenation does not: with
/// `platform`/`push_token` adjacent and both variable-length, `("ios", "TOK")`
/// and `("i", "osTOK")` concatenate to the same string, so one observed
/// signature verifies over both, letting anyone who saw a single valid request
/// re-split it and overwrite the victim's stored registration without ever
/// holding their key. Never reintroduce a bare `format!()` concatenation here:
/// any field pair that is adjacent and variable-length reopens that hole.
///
/// Any new implementation of this wire (a mobile app signing its own
/// registration) must reproduce these bytes exactly — the golden vector in
/// `signing_bytes_is_the_documented_golden_vector` pins them.
fn signing_bytes(elements: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for element in elements {
        out.extend_from_slice(&(element.len() as u64).to_be_bytes());
        out.extend_from_slice(element.as_bytes());
    }
    out
}

// ---------- freshness ----------

/// How far a signed request's `timestamp` may sit from the relay's own clock,
/// **in either direction**, before the request is refused.
///
/// `timestamp` is a signed element of all three messages, but until 2026-08-15
/// nothing ever compared it to a clock, so a captured request replayed forever.
/// The sharp edge was the wake route: a replay creates a fresh `pending_wakes`
/// row, and that row is what `has_recent_wake` rate-limits the target's
/// *legitimate* wakes against — so a single captured wake, replayed on a loop,
/// suppressed a target indefinitely at no cost to the attacker.
///
/// **Why 300, and why symmetric.** 300 matches the wake rate limiter's window
/// (`db::WAKE_RATE_LIMIT_SECS`), so the relay carries one time constant rather
/// than two that drift apart — and since row 145 gave the limiter its own
/// retention, the pair really is gapless on the wake route: a replayed wake is
/// rate-limited for exactly as long as it stays fresh. (Before row 145 it was
/// not, and the arithmetic that says it was is worth distrusting on sight: the
/// limiter's rows were being collected at 60s underneath it.) It is also the
/// conventional clock-skew
/// allowance (the same figure AWS SigV4 and most signed-request schemes use),
/// which matters because a device whose clock is minutes off must still be able
/// to register for push — refusing it would break out-of-the-box setup on
/// exactly the devices least able to diagnose it. Symmetric because a
/// *future*-dated timestamp is the same attack wearing a different sign: sign
/// once at `now + 10 years` and the request replays until then.
const FRESHNESS_WINDOW_SECS: u64 = 300;

/// Whether a signed `timestamp` (unix seconds) is fresh against `now`.
///
/// Pure and window-parameterised so the **boundary** is testable without any
/// wall-clock dependency (e2e-conventions.md convention 14: assert
/// latency-independent state). The boundary is **inclusive** — exactly
/// `window` seconds of skew is fresh, one second more is not — which is also
/// what makes the wake analysis above gapless: at exactly 300s a replay is
/// still fresh, and still rate-limited.
fn is_fresh(now_secs: u64, timestamp: u64, window_secs: u64) -> bool {
    now_secs.abs_diff(timestamp) <= window_secs
}

/// The relay's clock, in unix seconds. Saturates at 0 rather than panicking on
/// a pre-epoch system clock; a relay whose clock is that broken refuses
/// everything, which is the safe direction.
fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The refusal a stale request gets.
///
/// Deliberately the same **status** as a bad signature (401), so the routes
/// expose one authentication outcome rather than two, with the reason carried
/// in the body for the one caller who needs it — a legitimate client whose
/// clock has drifted, which is a real out-of-the-box failure and is otherwise
/// indistinguishable from "your key is wrong". Nothing is leaked by saying so:
/// a replayer already knows the timestamp it captured.
fn stale_response() -> axum::response::Response {
    (
        StatusCode::UNAUTHORIZED,
        "stale or future-dated timestamp — check this device's clock",
    )
        .into_response()
}

// ---------- helpers ----------

/// Decode a hex string to a 32-byte array, or return None.
///
/// Deliberately self-contained rather than `fauna_core::hex32::decode`: this relay
/// does not otherwise depend on the foundational `fauna-core` crate, and pulling its
/// full tree (bayespam/fastcdc/zstd/cbor) in for one decode is unjustified coupling.
fn decode_hex_32(s: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(s).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Some(arr)
}

/// Verify an Ed25519 signature over `message` using `actor_id` as the public key.
///
/// Uses `verify_strict`, not `verify`, and refuses weak keys outright. The
/// permissive `verify` is not a binding signature check for a small-order
/// public key: an all-zero `actor_id` with an all-zero signature satisfies the
/// verification equation whenever the challenge scalar happens to clear the
/// key's order, which it does for roughly 30% of messages (measured: 19 of 64).
/// Since `actor_id` is attacker-supplied on every one of these routes, that let
/// anyone mint requests — in particular wake requests — for identities nobody
/// holds a key to. `a_small_order_actor_id_never_verifies` pins the refusal.
fn verify_signature(actor_id_bytes: &[u8; 32], message: &[u8], sig_hex: &str) -> bool {
    let sig_bytes = match hex::decode(sig_hex) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let sig = match Signature::from_slice(&sig_bytes) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let vk = match VerifyingKey::from_bytes(actor_id_bytes) {
        Ok(k) => k,
        Err(_) => return false,
    };
    if vk.is_weak() {
        return false;
    }
    vk.verify_strict(message, &sig).is_ok()
}

// ---------- handlers ----------

/// PUT /v1/push-token
async fn put_push_token(
    State(state): State<RelayState>,
    axum::Json(req): axum::Json<PushTokenRequest>,
) -> impl IntoResponse {
    let actor_id = match decode_hex_32(&req.actor_id) {
        Some(id) => id,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };
    let message = req.signed_message(DOMAIN_PUSH_TOKEN_PUT);

    if !verify_signature(&actor_id, &message, &req.signature) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    // Freshness is checked AFTER the signature, not before: an unverified
    // timestamp is not a statement about anything, and refusing on it first
    // would mean acting on (and, if this ever meters refusals, counting)
    // attacker-supplied input that nobody has authenticated.
    if !is_fresh(now_unix_secs(), req.timestamp, FRESHNESS_WINDOW_SECS) {
        return stale_response();
    }

    match state
        .db
        .upsert_token(&actor_id, &req.platform, &req.push_token)
    {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => {
            tracing::error!("upsert_token failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// DELETE /v1/push-token
async fn delete_push_token(
    State(state): State<RelayState>,
    axum::Json(req): axum::Json<PushTokenRequest>,
) -> impl IntoResponse {
    let actor_id = match decode_hex_32(&req.actor_id) {
        Some(id) => id,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };

    let message = req.signed_message(DOMAIN_PUSH_TOKEN_DELETE);

    if !verify_signature(&actor_id, &message, &req.signature) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !is_fresh(now_unix_secs(), req.timestamp, FRESHNESS_WINDOW_SECS) {
        return stale_response();
    }

    match state.db.delete_token(&actor_id) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => {
            tracing::error!("delete_token failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// POST /v1/wake
async fn post_wake(
    State(state): State<RelayState>,
    axum::Json(req): axum::Json<WakeRequest>,
) -> impl IntoResponse {
    let requester_id = match decode_hex_32(&req.requester_actor_id) {
        Some(id) => id,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };
    let target_id = match decode_hex_32(&req.target_actor_id) {
        Some(id) => id,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };

    // Verify requester signature
    let timestamp = req.timestamp.to_string();
    let message = signing_bytes(&[
        DOMAIN_WAKE,
        &req.target_actor_id,
        &req.requester_actor_id,
        &req.requester_endpoint,
        &timestamp,
    ]);

    if !verify_signature(&requester_id, &message, &req.signature) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    // The route this check exists for: without it a captured wake replays
    // forever, and each replay re-arms the rate limiter below against the
    // target's own legitimate wakes.
    if !is_fresh(now_unix_secs(), req.timestamp, FRESHNESS_WINDOW_SECS) {
        return stale_response();
    }

    // Look up target's push token
    let push_token = match state.db.get_token(&target_id) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, "target actor not registered").into_response();
        }
        Err(e) => {
            tracing::error!("get_token failed: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Rate-limit: reject if a wake was already sent for this target in the last 5 minutes
    match state.db.has_recent_wake(&target_id) {
        Ok(true) => return StatusCode::TOO_MANY_REQUESTS.into_response(),
        Ok(false) => {}
        Err(e) => {
            tracing::error!("has_recent_wake failed: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    // Create pending wake
    let nonce = match state.db.create_wake(&target_id, &req.requester_endpoint) {
        Ok(n) => n,
        Err(e) => {
            tracing::error!("create_wake failed: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Send push notification to wake the target peer
    let wake_payload = crate::push_sender::WakePayload {
        requester_actor_id: req.requester_actor_id.clone(),
        nonce: nonce.clone(),
    };
    if let Err(e) = state
        .push_sender
        .send(&push_token.platform, &push_token.push_token, &wake_payload)
        .await
    {
        tracing::warn!("push send failed: {e}");
        // Don't fail the request — nonce is still created, requester can poll
    }

    (StatusCode::OK, axum::Json(WakeResponse { nonce })).into_response()
}

/// GET /v1/endpoint/{nonce}
async fn get_endpoint(
    State(state): State<RelayState>,
    Path(nonce): Path<String>,
) -> impl IntoResponse {
    match state.db.get_responder_endpoint(&nonce) {
        Ok(Some(endpoint)) => (
            StatusCode::OK,
            axum::Json(EndpointResponse {
                responder_endpoint: endpoint,
            }),
        )
            .into_response(),
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!("get_responder_endpoint failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// POST /v1/endpoint/{nonce}
async fn post_endpoint(
    State(state): State<RelayState>,
    Path(nonce): Path<String>,
    axum::Json(req): axum::Json<ReportEndpointRequest>,
) -> impl IntoResponse {
    match state
        .db
        .set_responder_endpoint(&nonce, &req.responder_endpoint)
    {
        Ok(true) => StatusCode::OK.into_response(),
        // No such wake — an unknown nonce, or one whose wake has expired. Saying
        // so is both honest (a 200 over a no-op told the reporter its endpoint
        // had landed when it had not) and what makes the nonce's scope
        // observable: holding one nonce demonstrably buys nothing about
        // another wake.
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("set_responder_endpoint failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use ed25519_dalek::SigningKey;
    use tower::util::ServiceExt;

    fn test_state() -> RelayState {
        let db = RelayDb::open(std::path::PathBuf::from(":memory:")).unwrap();
        RelayState::new(db, Arc::new(crate::push_sender::LogPushSender))
    }

    fn sample_push_token_request() -> PushTokenRequest {
        PushTokenRequest {
            actor_id: "aa".to_string(),
            platform: "apns".to_string(),
            push_token: "tok".to_string(),
            timestamp: 42,
            signature: "unused".to_string(),
        }
    }

    #[test]
    fn signed_message_length_prefixes_every_field_in_wire_order() {
        let req = sample_push_token_request();
        let msg = req.signed_message(DOMAIN_PUSH_TOKEN_PUT);
        assert_eq!(
            msg,
            signing_bytes(&[DOMAIN_PUSH_TOKEN_PUT, "aa", "apns", "tok", "42"])
        );
        // The pre-2026-08-15 encoding — a bare concatenation, no domain tag.
        assert_ne!(msg, b"aaapnstok42".to_vec());
    }

    /// The golden vector every other implementation of this wire must
    /// reproduce byte-for-byte. Mirrored verbatim by
    /// `signing_bytes_matches_the_relays_golden_vector` in
    /// `libs/fauna-peer/src/relay_client.rs`; if you change the encoding, both
    /// tests must change together or the two ends have silently forked.
    #[test]
    fn signing_bytes_is_the_documented_golden_vector() {
        const GOLDEN: &str = "00000000000000186661756e612d707573682d72656c61792f76312f77616b650000000000000002616100000000000000026262000000000000000d312e322e332e343a353138323000000000000000023432";
        let bytes = signing_bytes(&[DOMAIN_WAKE, "aa", "bb", "1.2.3.4:51820", "42"]);
        assert_eq!(hex::encode(&bytes), GOLDEN);
    }

    /// Distinct field tuples must never share signing bytes. Under the old
    /// bare concatenation `("apns", "tok")` and `("a", "pnstok")` both produced
    /// `aaapnstokbb42`, so one observed signature verified over both.
    #[test]
    fn a_resplit_of_platform_and_push_token_has_distinct_signing_bytes() {
        let original = sample_push_token_request();
        let resplit = PushTokenRequest {
            platform: "a".to_string(),
            push_token: "pnstok".to_string(),
            ..sample_push_token_request()
        };
        // The hole this pins: the two are identical under plain concatenation.
        assert_eq!(
            format!(
                "{}{}{}{}",
                original.actor_id, original.platform, original.push_token, original.timestamp
            ),
            format!(
                "{}{}{}{}",
                resplit.actor_id, resplit.platform, resplit.push_token, resplit.timestamp
            ),
        );
        assert_ne!(
            original.signed_message(DOMAIN_PUSH_TOKEN_PUT),
            resplit.signed_message(DOMAIN_PUSH_TOKEN_PUT),
        );
    }

    /// `put` and `delete` carry identical fields, so without a domain tag one
    /// observed registration signature also authorized deletion.
    #[test]
    fn put_and_delete_signing_bytes_differ_for_identical_fields() {
        let req = sample_push_token_request();
        assert_ne!(
            req.signed_message(DOMAIN_PUSH_TOKEN_PUT),
            req.signed_message(DOMAIN_PUSH_TOKEN_DELETE),
        );
    }

    /// A timestamp the freshness check accepts.
    ///
    /// Route tests used to hard-code 1711929600 (2024-04-01), which every one
    /// of them now has to stop doing: the routes compare `timestamp` to the
    /// clock, so a fixed literal ages into a refusal. Stamping `now` is
    /// latency-independent in the sense convention 14 asks for — it sits 300s
    /// from the boundary, and no test in this file runs for five minutes.
    fn fresh_timestamp() -> u64 {
        now_unix_secs()
    }

    /// A timestamp far enough outside the window that no scheduling delay can
    /// make it fresh — deliberately not `now - FRESHNESS_WINDOW_SECS - 1`,
    /// which would be a one-second race. The exact boundary is asserted by the
    /// pure `is_fresh` tests instead, where there is no clock at all.
    ///
    /// Saturating, on purpose: the mutation this helper exists to catch is
    /// the window widened towards infinity, and with plain `-` that mutation
    /// used to red the three route pins by an arithmetic-overflow panic in
    /// HERE rather than by the `401 != 200` they assert — a red for the wrong
    /// reason, which reads as a test bug and not as "the relay now accepts a
    /// stale request". Saturating to 0 keeps the timestamp maximally stale and
    /// lets the route's own assertion be the thing that fails.
    fn stale_timestamp() -> u64 {
        now_unix_secs()
            .saturating_sub(FRESHNESS_WINDOW_SECS)
            .saturating_sub(3600)
    }

    /// The boundary, asserted without a clock: inclusive on both sides, and
    /// symmetric, so a future-dated request is refused exactly as a past-dated
    /// one is. A signer who could post-date freely would have a signature that
    /// replays until the date it named.
    #[test]
    fn the_freshness_window_is_inclusive_and_symmetric() {
        let now = 1_000_000u64;
        let w = 300u64;

        assert!(is_fresh(now, now, w), "a request stamped now is fresh");
        assert!(is_fresh(now, now - w, w), "exactly window-old is fresh");
        assert!(is_fresh(now, now + w, w), "exactly window-ahead is fresh");
        assert!(
            !is_fresh(now, now - w - 1, w),
            "one second past the window is stale"
        );
        assert!(
            !is_fresh(now, now + w + 1, w),
            "one second beyond the window in the FUTURE is stale too — a \
             post-dated signature must not replay until the date it names"
        );
    }

    /// The window the routes run with is the rate limiter's nominal window, so
    /// the relay carries one time constant rather than two that drift apart.
    /// If someone changes one, this fails and points at the other.
    #[test]
    fn the_freshness_window_matches_the_wake_rate_limit_window() {
        assert_eq!(
            FRESHNESS_WINDOW_SECS,
            crate::db::WAKE_RATE_LIMIT_SECS,
            "these are deliberately equal, not accidentally: the relay carries \
             one answer to 'how long is a wake interesting'. They are kept as \
             two named constants rather than one alias because they mean \
             different things — skew allowance vs. how long a target stays \
             protected — so splitting them is allowed, but only deliberately, \
             which is what reddening this test forces someone to be."
        );
    }

    fn make_keypair() -> (SigningKey, [u8; 32]) {
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let verifying_key = signing_key.verifying_key();
        let actor_id: [u8; 32] = verifying_key.to_bytes();
        (signing_key, actor_id)
    }

    /// A wrong signature under a *real* key must be refused.
    ///
    /// This deliberately no longer uses an all-zero `actor_id`: that is a
    /// small-order point, and under the old permissive `verify` an all-zero
    /// signature over it verified for ~30% of messages — so this assertion was
    /// a coin flip that re-rolled whenever the signed bytes changed (mutating
    /// the encoding on 2026-08-15 flipped it red). `a_small_order_actor_id_never_verifies`
    /// now covers the weak-key case on purpose rather than by luck.
    #[tokio::test]
    async fn register_token_requires_valid_signature() {
        let state = test_state();
        let app = relay_router(state);

        let (_, actor_id) = make_keypair();
        let body = serde_json::json!({
            "actor_id": hex::encode(actor_id),
            "platform": "apns",
            "push_token": "device_token_abc",
            "timestamp": fresh_timestamp(),
            "signature": hex::encode([0u8; 64])  // invalid signature
        });

        let req = Request::builder()
            .method("PUT")
            .uri("/v1/push-token")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn register_token_with_valid_signature() {
        let state = test_state();
        let app = relay_router(state);

        let (signing_key, actor_id) = make_keypair();
        let actor_id_hex = hex::encode(actor_id);
        let platform = "apns";
        let push_token = "device_token_abc";
        let timestamp = fresh_timestamp();

        let message = signing_bytes(&[
            DOMAIN_PUSH_TOKEN_PUT,
            &actor_id_hex,
            platform,
            push_token,
            &timestamp.to_string(),
        ]);

        use ed25519_dalek::Signer;
        let sig = signing_key.sign(&message);
        let sig_hex = hex::encode(sig.to_bytes());

        let body = serde_json::json!({
            "actor_id": actor_id_hex,
            "platform": platform,
            "push_token": push_token,
            "timestamp": timestamp,
            "signature": sig_hex,
        });

        let req = Request::builder()
            .method("PUT")
            .uri("/v1/push-token")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// A small-order `actor_id` must never verify, for **any** message.
    ///
    /// `actor_id` is attacker-supplied on every signed route, so a key whose
    /// order divides the challenge scalar is a free forgery: under the old
    /// permissive `verify`, an all-zero key with an all-zero signature was
    /// accepted for 19 of these 64 messages. The wake route is the one that
    /// mattered — the requester signature is its only authentication, so this
    /// let anyone push-spam and rate-limit any target they could name.
    #[test]
    fn a_small_order_actor_id_never_verifies() {
        let zero_sig = hex::encode([0u8; 64]);
        for i in 0..64u32 {
            let msg = format!("probe message {i}");
            assert!(
                !verify_signature(&[0u8; 32], msg.as_bytes(), &zero_sig),
                "all-zero (small-order) actor_id accepted an all-zero signature over {msg:?}"
            );
        }
    }

    /// End-to-end form of `a_resplit_of_platform_and_push_token_has_distinct_signing_bytes`:
    /// an observer who captured one valid registration must not be able to move
    /// the platform/push_token boundary and have the relay accept it. Before
    /// 2026-08-15 this returned 200 and overwrote the victim's stored row.
    #[tokio::test]
    async fn a_resplit_registration_is_rejected_by_the_router() {
        let state = test_state();
        let app = relay_router(state.clone());

        let (signing_key, actor_id) = make_keypair();
        let actor_id_hex = hex::encode(actor_id);
        let timestamp = fresh_timestamp();

        // The victim's own, correctly signed registration.
        let message = signing_bytes(&[
            DOMAIN_PUSH_TOKEN_PUT,
            &actor_id_hex,
            "apns",
            "device_token_abc",
            &timestamp.to_string(),
        ]);
        use ed25519_dalek::Signer;
        let sig_hex = hex::encode(signing_key.sign(&message).to_bytes());

        // The attacker re-splits `platform`/`push_token` at a different point.
        // The concatenation is byte-identical, so the old scheme verified it.
        let body = serde_json::json!({
            "actor_id": actor_id_hex,
            "platform": "a",
            "push_token": "pnsdevice_token_abc",
            "timestamp": timestamp,
            "signature": sig_hex,
        });
        let req = Request::builder()
            .method("PUT")
            .uri("/v1/push-token")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        // And nothing was written for the victim.
        assert!(state.db.get_token(&actor_id).unwrap().is_none());
    }

    /// A captured `PUT` body must not delete the registration when replayed
    /// against `DELETE`: the two operations sign under different domain tags.
    #[tokio::test]
    async fn a_put_body_replayed_as_delete_is_rejected() {
        let state = test_state();

        let (signing_key, actor_id) = make_keypair();
        let actor_id_hex = hex::encode(actor_id);
        let timestamp = fresh_timestamp();

        let message = signing_bytes(&[
            DOMAIN_PUSH_TOKEN_PUT,
            &actor_id_hex,
            "apns",
            "device_token_abc",
            &timestamp.to_string(),
        ]);
        use ed25519_dalek::Signer;
        let sig_hex = hex::encode(signing_key.sign(&message).to_bytes());

        let body = serde_json::json!({
            "actor_id": actor_id_hex,
            "platform": "apns",
            "push_token": "device_token_abc",
            "timestamp": timestamp,
            "signature": sig_hex,
        });
        let body_str = serde_json::to_string(&body).unwrap();

        // The registration itself succeeds.
        let app = relay_router(state.clone());
        let req = Request::builder()
            .method("PUT")
            .uri("/v1/push-token")
            .header("content-type", "application/json")
            .body(Body::from(body_str.clone()))
            .unwrap();
        assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::OK);

        // Replaying the very same signed body as a DELETE must not.
        let app2 = relay_router(state.clone());
        let req2 = Request::builder()
            .method("DELETE")
            .uri("/v1/push-token")
            .header("content-type", "application/json")
            .body(Body::from(body_str))
            .unwrap();
        assert_eq!(
            app2.oneshot(req2).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        assert!(state.db.get_token(&actor_id).unwrap().is_some());
    }

    #[tokio::test]
    async fn wake_returns_nonce() {
        let state = test_state();

        // First, register a push token for the target actor
        let target_signing = SigningKey::from_bytes(&[99u8; 32]);
        let target_actor_id = target_signing.verifying_key().to_bytes();
        state
            .db
            .upsert_token(&target_actor_id, "apns", "target_device_token")
            .unwrap();

        let app = relay_router(state);

        // Now send a wake request from the requester
        let (requester_signing, requester_actor_id) = make_keypair();
        let target_hex = hex::encode(target_actor_id);
        let requester_hex = hex::encode(requester_actor_id);
        let endpoint = "1.2.3.4:51820";
        let timestamp = fresh_timestamp();

        let message = signing_bytes(&[
            DOMAIN_WAKE,
            &target_hex,
            &requester_hex,
            endpoint,
            &timestamp.to_string(),
        ]);
        use ed25519_dalek::Signer;
        let sig = requester_signing.sign(&message);
        let sig_hex = hex::encode(sig.to_bytes());

        let body = serde_json::json!({
            "target_actor_id": target_hex,
            "requester_actor_id": requester_hex,
            "requester_endpoint": endpoint,
            "timestamp": timestamp,
            "signature": sig_hex,
        });

        let req = Request::builder()
            .method("POST")
            .uri("/v1/wake")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let wake_resp: WakeResponse = serde_json::from_slice(&body_bytes).unwrap();
        assert!(!wake_resp.nonce.is_empty());
    }

    /// A **correctly signed** registration whose timestamp is outside the
    /// window is refused. The signature is genuine — that is the whole point:
    /// this is what a captured request looks like when it is replayed later,
    /// and until 2026-08-15 the relay accepted it forever.
    #[tokio::test]
    async fn a_stale_registration_is_refused_despite_a_valid_signature() {
        let state = test_state();
        let app = relay_router(state);

        let (signing_key, actor_id) = make_keypair();
        let actor_id_hex = hex::encode(actor_id);
        let platform = "apns";
        let push_token = "device_token_abc";
        let timestamp = stale_timestamp();

        let message = signing_bytes(&[
            DOMAIN_PUSH_TOKEN_PUT,
            &actor_id_hex,
            platform,
            push_token,
            &timestamp.to_string(),
        ]);
        use ed25519_dalek::Signer;
        let sig_hex = hex::encode(signing_key.sign(&message).to_bytes());

        let body = serde_json::json!({
            "actor_id": actor_id_hex,
            "platform": platform,
            "push_token": push_token,
            "timestamp": timestamp,
            "signature": sig_hex,
        });

        let req = Request::builder()
            .method("PUT")
            .uri("/v1/push-token")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// Same for the delete route — a captured `DELETE` is the one that
    /// unregisters a victim's device, so it must age out too.
    #[tokio::test]
    async fn a_stale_deletion_is_refused_despite_a_valid_signature() {
        let state = test_state();
        let (signing_key, actor_id) = make_keypair();
        state
            .db
            .upsert_token(&actor_id, "apns", "device_token_abc")
            .unwrap();
        let app = relay_router(state.clone());

        let actor_id_hex = hex::encode(actor_id);
        let platform = "apns";
        let push_token = "device_token_abc";
        let timestamp = stale_timestamp();

        let message = signing_bytes(&[
            DOMAIN_PUSH_TOKEN_DELETE,
            &actor_id_hex,
            platform,
            push_token,
            &timestamp.to_string(),
        ]);
        use ed25519_dalek::Signer;
        let sig_hex = hex::encode(signing_key.sign(&message).to_bytes());

        let body = serde_json::json!({
            "actor_id": actor_id_hex,
            "platform": platform,
            "push_token": push_token,
            "timestamp": timestamp,
            "signature": sig_hex,
        });

        let req = Request::builder()
            .method("DELETE")
            .uri("/v1/push-token")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // And the registration it tried to delete is still there — a refused
        // replay must not half-apply.
        assert!(state.db.get_token(&actor_id).unwrap().is_some());
    }

    /// The route the row was filed for. A captured wake, replayed once its
    /// window has passed, is refused — so it can no longer create the
    /// `pending_wakes` row that `has_recent_wake` would then hold against the
    /// target's own legitimate wakes.
    #[tokio::test]
    async fn a_stale_wake_is_refused_and_cannot_suppress_the_target() {
        let state = test_state();

        let target_signing = SigningKey::from_bytes(&[123u8; 32]);
        let target_actor_id = target_signing.verifying_key().to_bytes();
        state
            .db
            .upsert_token(&target_actor_id, "apns", "target_device_token")
            .unwrap();

        let (requester_signing, requester_actor_id) = make_keypair();
        let target_hex = hex::encode(target_actor_id);
        let requester_hex = hex::encode(requester_actor_id);
        let endpoint = "1.2.3.4:51820";
        let timestamp = stale_timestamp();

        let message = signing_bytes(&[
            DOMAIN_WAKE,
            &target_hex,
            &requester_hex,
            endpoint,
            &timestamp.to_string(),
        ]);
        use ed25519_dalek::Signer;
        let sig_hex = hex::encode(requester_signing.sign(&message).to_bytes());

        let body = serde_json::json!({
            "target_actor_id": target_hex,
            "requester_actor_id": requester_hex,
            "requester_endpoint": endpoint,
            "timestamp": timestamp,
            "signature": sig_hex,
        });

        let req = Request::builder()
            .method("POST")
            .uri("/v1/wake")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = relay_router(state.clone()).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // The suppression itself, asserted rather than argued: the refused
        // replay left no wake row, so the target's rate limiter is untouched.
        assert!(
            !state.db.has_recent_wake(&target_actor_id).unwrap(),
            "a refused replay must not arm the rate limiter against its target"
        );
    }

    #[tokio::test]
    async fn wake_rate_limited() {
        let state = test_state();

        // Register a push token for the target actor
        let target_signing = SigningKey::from_bytes(&[77u8; 32]);
        let target_actor_id = target_signing.verifying_key().to_bytes();
        state
            .db
            .upsert_token(&target_actor_id, "apns", "target_device_token")
            .unwrap();

        let (requester_signing, requester_actor_id) = make_keypair();
        let target_hex = hex::encode(target_actor_id);
        let requester_hex = hex::encode(requester_actor_id);
        let endpoint = "9.9.9.9:51820";
        let timestamp = fresh_timestamp();

        let message = signing_bytes(&[
            DOMAIN_WAKE,
            &target_hex,
            &requester_hex,
            endpoint,
            &timestamp.to_string(),
        ]);
        use ed25519_dalek::Signer;
        let sig = requester_signing.sign(&message);
        let sig_hex = hex::encode(sig.to_bytes());

        let body = serde_json::json!({
            "target_actor_id": target_hex,
            "requester_actor_id": requester_hex,
            "requester_endpoint": endpoint,
            "timestamp": timestamp,
            "signature": sig_hex,
        });
        let body_str = serde_json::to_string(&body).unwrap();

        // First wake — should succeed
        let app = relay_router(state.clone());
        let req = Request::builder()
            .method("POST")
            .uri("/v1/wake")
            .header("content-type", "application/json")
            .body(Body::from(body_str.clone()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Second wake for the same target — should be rate-limited
        let app2 = relay_router(state);
        let req2 = Request::builder()
            .method("POST")
            .uri("/v1/wake")
            .header("content-type", "application/json")
            .body(Body::from(body_str))
            .unwrap();
        let resp2 = app2.oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn report_and_poll_endpoint() {
        let state = test_state();
        let target_id = [50u8; 32];
        state.db.upsert_token(&target_id, "apns", "tok").unwrap();
        let nonce = state.db.create_wake(&target_id, "1.2.3.4:51820").unwrap();

        let app = relay_router(state);

        // POST responder endpoint
        let req = Request::builder()
            .method("POST")
            .uri(format!("/v1/endpoint/{nonce}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"responder_endpoint":"5.6.7.8:51820"}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // GET endpoint — should return the reported endpoint
        let req = Request::get(format!("/v1/endpoint/{nonce}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["responder_endpoint"], "5.6.7.8:51820");
    }

    /// **The pin: the nonce IS the capability, and it is scoped to its
    /// own wake.** This plane is deliberately unauthenticated in both
    /// directions (`api-layers.md` § Push relay states the ruling and why), so
    /// what has to hold is that holding *a* nonce buys nothing about *another*
    /// wake — neither a read nor a write.
    ///
    /// The write half is the one that had a real defect: `post_endpoint`
    /// answered `200 OK` to a nonce matching no wake, so a caller could not
    /// tell a successful report from one that went nowhere, and this assertion
    /// could not be written at all.
    #[tokio::test]
    async fn a_nonce_buys_nothing_about_another_wake() {
        let state = test_state();
        let target_id = [60u8; 32];
        state.db.upsert_token(&target_id, "apns", "tok").unwrap();
        let mine = state.db.create_wake(&target_id, "1.2.3.4:51820").unwrap();
        state
            .db
            .set_responder_endpoint(&mine, "5.6.7.8:51820")
            .unwrap();

        let app = relay_router(state.clone());
        let stranger = uuid::Uuid::new_v4().to_string();
        assert_ne!(stranger, mine);

        // Read: an unrelated nonce learns nothing.
        let req = Request::get(format!("/v1/endpoint/{stranger}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NO_CONTENT,
            "an unrelated nonce must not resolve to another wake's endpoint"
        );

        // Write: an unrelated nonce is refused, and — the part that matters —
        // the real wake's endpoint is untouched.
        let req = Request::builder()
            .method("POST")
            .uri(format!("/v1/endpoint/{stranger}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"responder_endpoint":"9.9.9.9:1"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "a report against no wake must say so, not answer 200 to a no-op"
        );
        assert_eq!(
            state.db.get_responder_endpoint(&mine).unwrap().as_deref(),
            Some("5.6.7.8:51820"),
            "another wake's endpoint must be unreachable from a stranger's nonce"
        );
    }
}
