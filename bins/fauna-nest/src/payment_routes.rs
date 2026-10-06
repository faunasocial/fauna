//! Public HTTP webhook ingress for payment providers — Pillar 3 of
//! `docs/goal/behavior/monetization.md`.
//!
//! `POST /api/v1/payments/webhook/{author_id}/{provider}` — the URL a
//! creator registers at their payment provider. Providers cannot speak
//! WS-RPC; this is the same deliberate external-caller carve-out class as
//! the surviving public HTTP reads (api-layers.md). Registered in the
//! `build_router` route table, which sits ABOVE the web-content fallback —
//! structurally un-shadowable by user web content.
//!
//! ⚠ Every rejection MUST be non-2xx: the web-serving catch-all answers
//! unmatched paths with a `200` info page, which a provider's delivery
//! system would record as success — so unknown provider/author is `404`,
//! a failed signature is `401`, a signature-valid-but-unparseable body is
//! `400`, and a dangling tier mapping is `409`. A signature-valid event of
//! an unhandled type is acknowledged `200` (it WAS delivered) and ignored.
//!
//! Verification is delegated to the shared adapter seam
//! ([`fauna_payments::PaymentProvider`]) over the EXACT raw body bytes; the
//! outcome is applied by the transport-agnostic engine ([`crate::payment_core`]).

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use fauna_payments::{PaymentEventKind, VerifyError, WebhookHeaders};

use crate::api_error::ApiError;
use crate::payment_core::{self, GrantError, PaymentApplied};
use crate::routes::AppState;

/// Compose and spend the `payments.unlock.purchase` gate for a verified
/// webhook payment, reporting the refusal's own wire code so the log and the
/// 4xx body say which bound bit.
///
/// A provider that redelivers a webhook re-spends here, because the gate is a
/// check-and-spend that runs *before* the operation and the waist's idempotency
/// lives after it (§ Usage accounting — "spend-on-commit … over-counting against
/// the actor, never under-counting"). That is the direction a bound tolerates,
/// and tier 1's payments ceiling is sized an order of magnitude above any
/// redelivery pattern precisely because the anti-runaway case is what it exists
/// for.
async fn gate_purchase(
    state: &Arc<AppState>,
    entitlement: &fauna_payments::PaymentEntitlement,
) -> Result<(), String> {
    // A provider event names a tier, never a price this nest can compare, so
    // the operation's magnitude is an honest `0` (monetization.md § The asking
    // price — "Fauna never parses provider-side prices").
    let op = payment_core::purchase_gate_op(state, entitlement, 0)
        .await
        .map_err(|e| e.to_string())?;
    crate::feature_gate::gate(state, &entitlement.payee.0, &op)
        .await
        .map_err(|e| e.code)
}

/// `POST /api/v1/payments/webhook/{author_id}/{provider}`.
pub async fn payment_webhook(
    State(state): State<Arc<AppState>>,
    Path((author_hex, provider_kind)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Ok(author_id) = fauna_core::hex32::decode(&author_hex) else {
        return ApiError::not_found("unknown webhook target").into_response();
    };

    // Unknown provider kind or no config row for (author, kind) → 404, never
    // the catch-all 200.
    let Some(adapter) = fauna_payments::provider_for_kind(&provider_kind) else {
        return ApiError::not_found("unknown payment provider").into_response();
    };
    let config = match state
        .db
        .get_payment_provider(&author_id, &provider_kind)
        .await
    {
        Ok(Some(c)) => c,
        Ok(None) => {
            return ApiError::not_found("payment provider not configured").into_response();
        }
        Err(e) => {
            tracing::error!("payment webhook: get_payment_provider: {e}");
            return ApiError::internal("storage error").into_response();
        }
    };

    let wh_headers = WebhookHeaders::from_pairs(
        headers
            .iter()
            .filter_map(|(name, value)| value.to_str().ok().map(|v| (name.as_str(), v))),
    );
    let now_secs = fauna_core::data::Timestamp::now_secs() as u64;

    // Provider-status evidence stamps (monetization.md § Pillar 3 → "Provider
    // status — evidence-based, no ping"): stamped against the existing row we
    // already fetched above, never by an active probe. `MalformedPayload`
    // stamps verified too — the signature check (which proves the secret is
    // correct) runs BEFORE the body parse in every adapter, so a malformed
    // body still proves a good secret; only the three signature/timestamp
    // variants below are genuine rejections.
    let event = match adapter.verify(&config.webhook_secret, &wh_headers, &body, now_secs) {
        Ok(ev) => {
            if let Err(e) = state
                .db
                .stamp_payment_provider_verified(&author_id, &provider_kind)
                .await
            {
                tracing::error!("payment webhook: stamp_payment_provider_verified: {e}");
            }
            ev
        }
        Err(
            e @ (VerifyError::MissingSignature
            | VerifyError::BadSignature
            | VerifyError::StaleTimestamp),
        ) => {
            tracing::warn!(provider = %provider_kind, "payment webhook rejected: {e}");
            if let Err(e) = state
                .db
                .stamp_payment_provider_rejected(&author_id, &provider_kind)
                .await
            {
                tracing::error!("payment webhook: stamp_payment_provider_rejected: {e}");
            }
            return ApiError::unauthorized("webhook signature verification failed").into_response();
        }
        Err(VerifyError::MalformedPayload(why)) => {
            tracing::warn!(provider = %provider_kind, "payment webhook malformed: {why}");
            if let Err(e) = state
                .db
                .stamp_payment_provider_verified(&author_id, &provider_kind)
                .await
            {
                tracing::error!("payment webhook: stamp_payment_provider_verified: {e}");
            }
            return ApiError::bad_request("malformed webhook payload").into_response();
        }
    };

    // Reduce the verified event to the narrow waist — from here down this
    // handler carries no payment logic at all, only HTTP framing.
    let mapping = fauna_payments::TierMapping {
        tier: config.tier_name.clone(),
    };
    let Some(entitlement) = fauna_payments::PaymentEntitlement::from_verified_event(
        &provider_kind,
        fauna_core::identity::ActorId(author_id),
        &event,
        &mapping,
    ) else {
        return ApiError::conflict("no tier mapping covers this payment").into_response();
    };

    match event.kind {
        PaymentEventKind::Ignored => json_ok(serde_json::json!({ "status": "ignored" })),
        PaymentEventKind::Payment => {
            // **Gate surface `payments.unlock.purchase`** — the buy side, bound
            // to the **payee** (`dynamic-features.md` § Charter members).
            //
            // Whose quota: the account whose nest-arbitrated operation this is.
            // A webhook is a *sale landing on this creator's own provider
            // config, on their own nest*, and tier 1's payments sizing is
            // written for exactly that subject ("a viral post can genuinely
            // sell to a four-figure audience in a day"). The buyer, when they
            // are a local account, spends at their own surfaces —
            // `claims.redeem` — so nothing is counted twice against one person.
            //
            // Deliberately NOT gated in `payment_core::apply_payment`, the
            // waist all three mechanisms funnel through: claim redemption
            // reaches it too, and it has already spent at
            // `payments.claim.redeem`. One user-visible purchase act must cost
            // one unit, so the gate sits on the mechanisms that turn external
            // money into an entitlement with no already-gated operation behind
            // them — this one and the zap purchase.
            if let Err(e) = gate_purchase(&state, &entitlement).await {
                // Non-2xx, per the module rule — and specifically the 4xx class
                // so a provider records a refusal rather than retrying into a
                // quota that is not going to move. `403` says what happened:
                // the payment verified, and this nest will not apply it.
                tracing::warn!(
                    provider = %provider_kind,
                    code = %e,
                    "payment webhook refused by the feature gate"
                );
                return ApiError::forbidden(format!("payment feature gate: {e}")).into_response();
            }
            match payment_core::apply_payment(&state, &entitlement).await {
                Ok(PaymentApplied::Granted { queued }) => json_ok(serde_json::json!({
                    "status": "granted",
                    "queued": queued,
                })),
                // The code rides back in the webhook response body — visible to the
                // creator in the provider's delivery dashboard, and to provider-side
                // receipt tooling.
                Ok(PaymentApplied::ClaimMinted { code }) => json_ok(serde_json::json!({
                    "status": "claim_minted",
                    "claim_code": code,
                })),
                Ok(PaymentApplied::ClaimExists { code }) => json_ok(serde_json::json!({
                    "status": "claim_exists",
                    "claim_code": code,
                })),
                Err(e) => grant_error_response(e),
            }
        }
        PaymentEventKind::Refund => match payment_core::apply_refund(&state, &entitlement).await {
            Ok(acted) => {
                // A refund voids the paid window, so a membership buyer may now be
                // lapsed — reconcile them immediately rather than wait for the
                // hourly sweep (monetization.md § Pillar 4 Rail C step 3: "at
                // webhook ingress for that buyer"). Bound buyers only; an unbound
                // refund names no member. A renewal *payment* re-stamps the window
                // in `grant_membership`, so only the refund path can strand a lapse.
                if let fauna_payments::Buyer::Actor(buyer) = &entitlement.buyer
                    && let Err(e) = state.db.reconcile_lapsed_memberships(Some(&buyer.0)).await
                {
                    tracing::error!("post-refund lapse reconcile: {e}");
                }
                json_ok(serde_json::json!({
                    "status": if acted { "refund_applied" } else { "no_action" },
                }))
            }
            Err(e) => grant_error_response(e),
        },
    }
}

fn grant_error_response(e: GrantError) -> Response {
    match e {
        GrantError::TierNotFound => {
            ApiError::conflict("tier mapping no longer exists").into_response()
        }
        GrantError::Internal(why) => {
            tracing::error!("payment webhook: {why}");
            ApiError::internal("grant failed").into_response()
        }
    }
}

fn json_ok(value: serde_json::Value) -> Response {
    axum::Json(value).into_response()
}
