//! The fake/test payment provider — the first-cut adapter every tier_3 test
//! drives (monetization.md § Pillar 3: "one fake/test provider + a
//! Stripe-shaped webhook verification").
//!
//! Deliberately the simplest sound scheme:
//!
//! - Signature: `X-Fauna-Signature: hex(HMAC_SHA256(secret, raw_body))`.
//! - Body (JSON): `{ "id": "...", "event": "payment"|"refund",
//!   "reference": "<actor hex>"?, "product": "..."?, "valid_until": secs? }`.
//!
//! No timestamp/replay tolerance — the real-provider replay concern is
//! covered by the Stripe adapter; this one exists to exercise the engine.

use serde::Deserialize;

use crate::{
    PaymentEventKind, PaymentProvider, VerifiedPaymentEvent, VerifyError, WebhookHeaders,
    constant_time_eq,
};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub const KIND: &str = "fake";

/// Signature header name (lower-case; header lookup is case-insensitive).
pub const SIGNATURE_HEADER: &str = "x-fauna-signature";

/// `hex(HMAC_SHA256(secret, body))` — exported so tests and the e2e harness
/// sign exactly the way the adapter verifies.
pub fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key len");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

#[derive(Deserialize)]
struct FakeEventBody {
    id: String,
    event: String,
    #[serde(default)]
    reference: Option<String>,
    #[serde(default)]
    product: Option<String>,
    #[serde(default)]
    valid_until: Option<u64>,
}

pub struct FakeProvider;

impl PaymentProvider for FakeProvider {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn verify(
        &self,
        secret: &str,
        headers: &WebhookHeaders,
        body: &[u8],
        _now_secs: u64,
    ) -> Result<VerifiedPaymentEvent, VerifyError> {
        let claimed = headers
            .get(SIGNATURE_HEADER)
            .ok_or(VerifyError::MissingSignature)?;
        let expected = sign(secret, body);
        if !constant_time_eq(expected.as_bytes(), claimed.trim().as_bytes()) {
            return Err(VerifyError::BadSignature);
        }

        let parsed: FakeEventBody = serde_json::from_slice(body)
            .map_err(|e| VerifyError::MalformedPayload(e.to_string()))?;
        let kind = match parsed.event.as_str() {
            "payment" => PaymentEventKind::Payment,
            "refund" => PaymentEventKind::Refund,
            _ => PaymentEventKind::Ignored,
        };
        Ok(VerifiedPaymentEvent {
            kind,
            buyer_reference: parsed.reference,
            product_reference: parsed.product,
            valid_until_secs: parsed.valid_until,
            external_ref: parsed.id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "whsec_test_secret";

    fn body(event: &str, reference: Option<&str>, valid_until: Option<u64>) -> Vec<u8> {
        let mut v = serde_json::json!({ "id": "pay_1", "event": event });
        if let Some(r) = reference {
            v["reference"] = serde_json::json!(r);
        }
        if let Some(t) = valid_until {
            v["valid_until"] = serde_json::json!(t);
        }
        serde_json::to_vec(&v).unwrap()
    }

    fn signed_headers(secret: &str, body: &[u8]) -> WebhookHeaders {
        WebhookHeaders::from_pairs([(SIGNATURE_HEADER, sign(secret, body))])
    }

    #[test]
    fn valid_payment_verifies_and_normalizes() {
        let b = body(
            "payment",
            Some("aa".repeat(32).as_str()),
            Some(1_800_000_000),
        );
        let ev = FakeProvider
            .verify(SECRET, &signed_headers(SECRET, &b), &b, 0)
            .unwrap();
        assert_eq!(ev.kind, PaymentEventKind::Payment);
        assert_eq!(
            ev.buyer_reference.as_deref(),
            Some("aa".repeat(32).as_str())
        );
        assert_eq!(ev.valid_until_secs, Some(1_800_000_000));
        assert_eq!(ev.external_ref, "pay_1");
    }

    #[test]
    fn refund_event_normalizes_to_refund() {
        let b = body("refund", Some("bb"), None);
        let ev = FakeProvider
            .verify(SECRET, &signed_headers(SECRET, &b), &b, 0)
            .unwrap();
        assert_eq!(ev.kind, PaymentEventKind::Refund);
    }

    #[test]
    fn unknown_event_type_is_ignored_not_error() {
        let b = body("chargeback.preview", None, None);
        let ev = FakeProvider
            .verify(SECRET, &signed_headers(SECRET, &b), &b, 0)
            .unwrap();
        assert_eq!(ev.kind, PaymentEventKind::Ignored);
    }

    #[test]
    fn missing_signature_rejected() {
        let b = body("payment", None, None);
        assert_eq!(
            FakeProvider.verify(SECRET, &WebhookHeaders::new(), &b, 0),
            Err(VerifyError::MissingSignature)
        );
    }

    #[test]
    fn wrong_secret_rejected() {
        let b = body("payment", None, None);
        assert_eq!(
            FakeProvider.verify(SECRET, &signed_headers("other_secret", &b), &b, 0),
            Err(VerifyError::BadSignature)
        );
    }

    #[test]
    fn tampered_body_rejected() {
        let b = body("payment", Some("cc"), None);
        let headers = signed_headers(SECRET, &b);
        let mut tampered = b.clone();
        let last = tampered.len() - 2;
        tampered[last] ^= 1;
        assert_eq!(
            FakeProvider.verify(SECRET, &headers, &tampered, 0),
            Err(VerifyError::BadSignature)
        );
    }

    #[test]
    fn garbage_json_with_valid_signature_is_malformed() {
        let b = b"not json at all".to_vec();
        let err = FakeProvider
            .verify(SECRET, &signed_headers(SECRET, &b), &b, 0)
            .unwrap_err();
        assert!(matches!(err, VerifyError::MalformedPayload(_)));
    }
}
