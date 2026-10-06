//! Stripe-shaped webhook verification (monetization.md § Pillar 3 first cut:
//! "one fake/test provider + Stripe-shaped webhook verification").
//!
//! Scheme (Stripe's documented v1 signing):
//!
//! - Header `Stripe-Signature: t=<unix_secs>,v1=<hex>[,v1=<hex>…]` (multiple
//!   `v1` entries during endpoint-secret rotation — any match accepts).
//! - Signed payload is `"{t}.{raw_body}"`, MAC is HMAC-SHA256 with the
//!   endpoint's webhook-verification secret.
//! - `t` outside [`TOLERANCE_SECS`] of the caller's clock is rejected
//!   (replay bound).
//!
//! Event normalization is deliberately first-cut narrow: the event types
//! below cover the subscription lifecycle (checkout completes → paid;
//! invoice paid → renewal window extends; refund/dispute → void); every
//! other type verifies but normalizes to `Ignored` (acknowledged, no
//! action). All Stripe JSON knowledge stays inside this module.

use serde::Deserialize;

use crate::{
    PaymentEventKind, PaymentProvider, VerifiedPaymentEvent, VerifyError, WebhookHeaders,
    constant_time_eq,
};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub const KIND: &str = "stripe";

/// Signature header name (lower-case; lookup is case-insensitive).
pub const SIGNATURE_HEADER: &str = "stripe-signature";

/// Replay tolerance for the signed timestamp — Stripe's own recommended
/// default (5 minutes).
pub const TOLERANCE_SECS: u64 = 300;

/// `hex(HMAC_SHA256(secret, "{t}.{body}"))` — the v1 signed payload. Exported
/// so tests build valid `Stripe-Signature` headers without duplicating the
/// scheme.
pub fn sign(secret: &str, timestamp_secs: u64, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key len");
    mac.update(timestamp_secs.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// Convenience for tests/harnesses: a complete `t=…,v1=…` header value.
pub fn signature_header(secret: &str, timestamp_secs: u64, body: &[u8]) -> String {
    format!(
        "t={timestamp_secs},v1={}",
        sign(secret, timestamp_secs, body)
    )
}

// Stripe event envelope — only the fields the normalization reads. Unknown
// fields are ignored by serde default behavior; absent fields default.
#[derive(Deserialize)]
struct StripeEvent {
    id: String,
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    data: StripeEventData,
}

#[derive(Deserialize, Default)]
struct StripeEventData {
    #[serde(default)]
    object: StripeObject,
}

#[derive(Deserialize, Default)]
struct StripeObject {
    /// Checkout's buyer↔actor binding (monetization.md § Pillar 3 Q4).
    #[serde(default)]
    client_reference_id: Option<String>,
    /// Subscription objects carry the current paid-through instant (secs).
    #[serde(default)]
    current_period_end: Option<u64>,
    /// Product/plan reference for future per-product tier maps.
    #[serde(default)]
    subscription: Option<String>,
}

pub struct StripeProvider;

impl PaymentProvider for StripeProvider {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn verify(
        &self,
        secret: &str,
        headers: &WebhookHeaders,
        body: &[u8],
        now_secs: u64,
    ) -> Result<VerifiedPaymentEvent, VerifyError> {
        let header = headers
            .get(SIGNATURE_HEADER)
            .ok_or(VerifyError::MissingSignature)?;

        // Parse `t=…,v1=…[,v1=…]`; other schemes (v0) are ignored.
        let mut timestamp: Option<u64> = None;
        let mut candidates: Vec<&str> = Vec::new();
        for part in header.split(',') {
            match part.trim().split_once('=') {
                Some(("t", v)) => timestamp = v.parse().ok(),
                Some(("v1", v)) => candidates.push(v),
                _ => {}
            }
        }
        let timestamp = timestamp.ok_or(VerifyError::MissingSignature)?;
        if candidates.is_empty() {
            return Err(VerifyError::MissingSignature);
        }

        if now_secs.abs_diff(timestamp) > TOLERANCE_SECS {
            return Err(VerifyError::StaleTimestamp);
        }

        let expected = sign(secret, timestamp, body);
        if !candidates
            .iter()
            .any(|c| constant_time_eq(expected.as_bytes(), c.as_bytes()))
        {
            return Err(VerifyError::BadSignature);
        }

        let event: StripeEvent = serde_json::from_slice(body)
            .map_err(|e| VerifyError::MalformedPayload(e.to_string()))?;
        let kind = match event.event_type.as_str() {
            "checkout.session.completed" | "invoice.paid" => PaymentEventKind::Payment,
            "charge.refunded" | "charge.dispute.created" | "customer.subscription.deleted" => {
                PaymentEventKind::Refund
            }
            _ => PaymentEventKind::Ignored,
        };
        Ok(VerifiedPaymentEvent {
            kind,
            buyer_reference: event.data.object.client_reference_id,
            product_reference: event.data.object.subscription,
            valid_until_secs: event.data.object.current_period_end,
            external_ref: event.id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "whsec_stripe_test";
    const NOW: u64 = 1_750_000_000;

    fn checkout_body(reference: Option<&str>) -> Vec<u8> {
        let mut object = serde_json::json!({});
        if let Some(r) = reference {
            object["client_reference_id"] = serde_json::json!(r);
        }
        serde_json::to_vec(&serde_json::json!({
            "id": "evt_1",
            "type": "checkout.session.completed",
            "data": { "object": object },
        }))
        .unwrap()
    }

    fn headers_for(secret: &str, t: u64, body: &[u8]) -> WebhookHeaders {
        WebhookHeaders::from_pairs([(SIGNATURE_HEADER, signature_header(secret, t, body))])
    }

    #[test]
    fn valid_checkout_verifies() {
        let b = checkout_body(Some("deadbeef"));
        let ev = StripeProvider
            .verify(SECRET, &headers_for(SECRET, NOW, &b), &b, NOW)
            .unwrap();
        assert_eq!(ev.kind, PaymentEventKind::Payment);
        assert_eq!(ev.buyer_reference.as_deref(), Some("deadbeef"));
        assert_eq!(ev.external_ref, "evt_1");
    }

    #[test]
    fn skew_within_tolerance_accepted() {
        let b = checkout_body(None);
        let t = NOW - TOLERANCE_SECS;
        assert!(
            StripeProvider
                .verify(SECRET, &headers_for(SECRET, t, &b), &b, NOW)
                .is_ok()
        );
    }

    #[test]
    fn stale_timestamp_rejected() {
        let b = checkout_body(None);
        let t = NOW - TOLERANCE_SECS - 1;
        assert_eq!(
            StripeProvider.verify(SECRET, &headers_for(SECRET, t, &b), &b, NOW),
            Err(VerifyError::StaleTimestamp)
        );
    }

    #[test]
    fn wrong_secret_rejected() {
        let b = checkout_body(None);
        assert_eq!(
            StripeProvider.verify(SECRET, &headers_for("whsec_other", NOW, &b), &b, NOW),
            Err(VerifyError::BadSignature)
        );
    }

    #[test]
    fn rotated_secret_second_v1_accepted() {
        let b = checkout_body(None);
        let stale = sign("whsec_old", NOW, &b);
        let live = sign(SECRET, NOW, &b);
        let h = WebhookHeaders::from_pairs([(
            SIGNATURE_HEADER,
            format!("t={NOW},v1={stale},v1={live}"),
        )]);
        assert!(StripeProvider.verify(SECRET, &h, &b, NOW).is_ok());
    }

    #[test]
    fn header_without_v1_rejected() {
        let b = checkout_body(None);
        let h = WebhookHeaders::from_pairs([(SIGNATURE_HEADER, format!("t={NOW}"))]);
        assert_eq!(
            StripeProvider.verify(SECRET, &h, &b, NOW),
            Err(VerifyError::MissingSignature)
        );
    }

    #[test]
    fn refund_and_dispute_normalize_to_refund() {
        for ty in ["charge.refunded", "charge.dispute.created"] {
            let b = serde_json::to_vec(&serde_json::json!({
                "id": "evt_r", "type": ty, "data": { "object": {} },
            }))
            .unwrap();
            let ev = StripeProvider
                .verify(SECRET, &headers_for(SECRET, NOW, &b), &b, NOW)
                .unwrap();
            assert_eq!(ev.kind, PaymentEventKind::Refund, "type {ty}");
        }
    }

    #[test]
    fn unrelated_event_type_ignored() {
        let b = serde_json::to_vec(&serde_json::json!({
            "id": "evt_x", "type": "payment_intent.created", "data": { "object": {} },
        }))
        .unwrap();
        let ev = StripeProvider
            .verify(SECRET, &headers_for(SECRET, NOW, &b), &b, NOW)
            .unwrap();
        assert_eq!(ev.kind, PaymentEventKind::Ignored);
    }

    #[test]
    fn invoice_paid_carries_period_end() {
        let b = serde_json::to_vec(&serde_json::json!({
            "id": "evt_i", "type": "invoice.paid",
            "data": { "object": { "current_period_end": 1_760_000_000u64 } },
        }))
        .unwrap();
        let ev = StripeProvider
            .verify(SECRET, &headers_for(SECRET, NOW, &b), &b, NOW)
            .unwrap();
        assert_eq!(ev.kind, PaymentEventKind::Payment);
        assert_eq!(ev.valid_until_secs, Some(1_760_000_000));
    }
}
