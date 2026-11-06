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
//! below cover the purchase lifecycle (a checkout completes PAID, or its
//! delayed payment succeeds → paid; invoice paid → renewal window extends;
//! refund/dispute/cancellation or a delayed payment's failure → void); every
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
    /// The object's own id — on a Checkout Session event the session id the
    /// post-checkout redirect substitutes for `{CHECKOUT_SESSION_ID}` (the
    /// visitor return door's reference); on a subscription event the
    /// subscription id.
    #[serde(default)]
    id: Option<String>,
    /// Checkout's buyer↔actor binding (monetization.md § Pillar 3 Q4).
    #[serde(default)]
    client_reference_id: Option<String>,
    /// Subscription objects carry the current paid-through instant (secs).
    #[serde(default)]
    current_period_end: Option<u64>,
    /// A Checkout Session's settlement state: only `paid` is a payment
    /// (`unpaid` = a delayed method still settling, `no_payment_required` =
    /// a free coupon or a setup-mode session).
    #[serde(default)]
    payment_status: Option<String>,
    /// The subscription a session or invoice belongs to (pre-2025 shape; the
    /// current invoice carries it under `parent.subscription_details`).
    #[serde(default)]
    subscription: Option<IdRef>,
    /// The payment intent a session, invoice, charge or dispute belongs to.
    #[serde(default)]
    payment_intent: Option<IdRef>,
    #[serde(default)]
    parent: Option<InvoiceParent>,
    /// The Payment Link a Checkout Session was sold through — the product
    /// reference a per-product tier mapping matches.
    #[serde(default)]
    payment_link: Option<IdRef>,
}

#[derive(Deserialize, Default)]
struct InvoiceParent {
    #[serde(default)]
    subscription_details: Option<SubscriptionDetails>,
}

#[derive(Deserialize, Default)]
struct SubscriptionDetails {
    #[serde(default)]
    subscription: Option<IdRef>,
}

/// A Stripe object reference: an id string, or — when the endpoint expands
/// it — the object itself, carrying the same id.
#[derive(Deserialize)]
#[serde(untagged)]
enum IdRef {
    Id(String),
    Object { id: String },
}

fn id_of(r: &Option<IdRef>) -> Option<String> {
    match r {
        Some(IdRef::Id(id)) | Some(IdRef::Object { id }) => Some(id.clone()),
        None => None,
    }
}

impl StripeObject {
    /// The purchase this event's object belongs to, by every stable id it
    /// names, in a fixed order: subscription, payment intent, then (for a
    /// Checkout Session) the session itself. A charge or dispute names its
    /// payment intent, a subscription object its own id — the ids the
    /// checkout and invoice recorded the purchase under.
    fn purchase_refs(&self, event_type: &str) -> Vec<String> {
        let mut refs = Vec::new();
        match event_type {
            "customer.subscription.deleted" => refs.extend(self.id.clone()),
            "charge.refunded" | "charge.dispute.created" => {
                refs.extend(id_of(&self.payment_intent));
            }
            t if t.starts_with("checkout.session.") => {
                refs.extend(id_of(&self.subscription));
                refs.extend(id_of(&self.payment_intent));
                refs.extend(self.id.clone());
            }
            "invoice.paid" => {
                let nested = self
                    .parent
                    .as_ref()
                    .and_then(|p| p.subscription_details.as_ref())
                    .and_then(|d| id_of(&d.subscription));
                refs.extend(id_of(&self.subscription).or(nested));
                refs.extend(id_of(&self.payment_intent));
            }
            _ => {}
        }
        refs.dedup();
        refs
    }
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
        let object = &event.data.object;
        let kind = match event.event_type.as_str() {
            // A completed session is a payment only once its money
            // has cleared; a delayed method settles later, in its own event.
            "checkout.session.completed" if object.payment_status.as_deref() == Some("paid") => {
                PaymentEventKind::Payment
            }
            "checkout.session.async_payment_succeeded" | "invoice.paid" => {
                PaymentEventKind::Payment
            }
            "checkout.session.async_payment_failed"
            | "charge.refunded"
            | "charge.dispute.created"
            | "customer.subscription.deleted" => PaymentEventKind::Refund,
            _ => PaymentEventKind::Ignored,
        };
        let return_ref = match (kind, event.event_type.as_str()) {
            (
                PaymentEventKind::Payment,
                "checkout.session.completed" | "checkout.session.async_payment_succeeded",
            ) => object.id.clone(),
            _ => None,
        };
        let purchase_refs = object.purchase_refs(&event.event_type);
        // Only a Checkout Session names the product it sold, by its Payment
        // Link; a renewal invoice is matched by its recorded purchase instead.
        let product_reference = if event.event_type.starts_with("checkout.session.") {
            id_of(&object.payment_link)
        } else {
            None
        };
        let object = event.data.object;
        Ok(VerifiedPaymentEvent {
            kind,
            return_ref,
            buyer_reference: object.client_reference_id,
            product_reference,
            valid_until_secs: object.current_period_end,
            external_ref: event.id,
            purchase_refs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "whsec_stripe_test";
    const NOW: u64 = 1_750_000_000;

    fn checkout_body(reference: Option<&str>) -> Vec<u8> {
        let mut object = serde_json::json!({ "id": "cs_test_1", "payment_status": "paid" });
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
        // The Checkout Session id is the return door's reference.
        assert_eq!(ev.return_ref.as_deref(), Some("cs_test_1"));
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

    fn verify_json(v: serde_json::Value) -> VerifiedPaymentEvent {
        let b = serde_json::to_vec(&v).unwrap();
        StripeProvider
            .verify(SECRET, &headers_for(SECRET, NOW, &b), &b, NOW)
            .unwrap()
    }

    fn session_event(ty: &str, object: serde_json::Value) -> VerifiedPaymentEvent {
        verify_json(serde_json::json!({
            "id": "evt_s", "type": ty, "data": { "object": object },
        }))
    }

    /// A completed Checkout Session whose payment has not cleared
    /// (`unpaid`: a delayed method still settling) or never will
    /// (`no_payment_required`: a free coupon, a setup-mode session) is no
    /// payment — it mints nothing.
    #[test]
    fn a_completed_session_that_is_not_paid_is_no_payment() {
        for status in ["unpaid", "no_payment_required"] {
            let ev = session_event(
                "checkout.session.completed",
                serde_json::json!({ "id": "cs_1", "payment_status": status,
                                    "payment_intent": "pi_1" }),
            );
            assert_eq!(
                ev.kind,
                PaymentEventKind::Ignored,
                "payment_status {status}"
            );
        }
        // A session with no payment_status at all is not taken as paid either.
        let ev = session_event(
            "checkout.session.completed",
            serde_json::json!({ "id": "cs_1" }),
        );
        assert_eq!(ev.kind, PaymentEventKind::Ignored);
    }

    /// The delayed payment's settlement is the payment; its failure
    /// voids whatever the purchase holds.
    #[test]
    fn a_delayed_payment_pays_on_success_and_voids_on_failure() {
        let object = serde_json::json!({ "id": "cs_1", "payment_status": "paid",
                                         "payment_intent": "pi_1" });
        let ok = session_event("checkout.session.async_payment_succeeded", object.clone());
        assert_eq!(ok.kind, PaymentEventKind::Payment);
        assert_eq!(ok.return_ref.as_deref(), Some("cs_1"));
        assert_eq!(
            ok.purchase_refs,
            vec!["pi_1".to_string(), "cs_1".to_string()]
        );

        let failed = session_event("checkout.session.async_payment_failed", object);
        assert_eq!(failed.kind, PaymentEventKind::Refund);
        assert_eq!(
            failed.purchase_refs,
            vec!["pi_1".to_string(), "cs_1".to_string()]
        );
    }

    /// A checkout names its purchase by the ids a later refund,
    /// dispute or cancellation names it by — never only by its own `evt_`.
    #[test]
    fn a_checkout_names_its_purchase_by_subscription_intent_and_session() {
        let one_off = session_event(
            "checkout.session.completed",
            serde_json::json!({ "id": "cs_1", "payment_status": "paid",
                                "payment_intent": "pi_1" }),
        );
        assert_eq!(one_off.kind, PaymentEventKind::Payment);
        assert_eq!(
            one_off.purchase_refs,
            vec!["pi_1".to_string(), "cs_1".to_string()]
        );

        let recurring = session_event(
            "checkout.session.completed",
            serde_json::json!({ "id": "cs_2", "payment_status": "paid",
                                "subscription": "sub_2", "payment_intent": null }),
        );
        assert_eq!(
            recurring.purchase_refs,
            vec!["sub_2".to_string(), "cs_2".to_string()]
        );
        // An expanded reference (an object carrying its id) names the same id.
        let expanded = session_event(
            "checkout.session.completed",
            serde_json::json!({ "id": "cs_3", "payment_status": "paid",
                                "subscription": { "id": "sub_3", "object": "subscription" } }),
        );
        assert_eq!(
            expanded.purchase_refs,
            vec!["sub_3".to_string(), "cs_3".to_string()]
        );
    }

    #[test]
    fn a_refund_dispute_or_cancellation_names_the_purchase_it_voids() {
        for (ty, object, want) in [
            (
                "charge.refunded",
                serde_json::json!({ "id": "ch_1", "payment_intent": "pi_1" }),
                "pi_1",
            ),
            (
                "charge.dispute.created",
                serde_json::json!({ "id": "dp_1", "charge": "ch_1", "payment_intent": "pi_1" }),
                "pi_1",
            ),
            (
                "customer.subscription.deleted",
                serde_json::json!({ "id": "sub_2", "object": "subscription" }),
                "sub_2",
            ),
        ] {
            let ev = verify_json(serde_json::json!({
                "id": "evt_r", "type": ty, "data": { "object": object },
            }));
            assert_eq!(ev.kind, PaymentEventKind::Refund, "type {ty}");
            assert_eq!(ev.purchase_refs, vec![want.to_string()], "type {ty}");
            assert_ne!(ev.external_ref, want, "the delivery id is not the purchase");
        }
    }

    #[test]
    fn a_paid_invoice_names_its_subscription_and_intent() {
        // The pre-2025 invoice shape carries `subscription` at the top level;
        // the current one under `parent.subscription_details`.
        let old = verify_json(serde_json::json!({
            "id": "evt_i", "type": "invoice.paid",
            "data": { "object": { "id": "in_1", "subscription": "sub_2", "payment_intent": "pi_9" } },
        }));
        assert_eq!(
            old.purchase_refs,
            vec!["sub_2".to_string(), "pi_9".to_string()]
        );
        let new = verify_json(serde_json::json!({
            "id": "evt_i", "type": "invoice.paid",
            "data": { "object": { "id": "in_1",
                "parent": { "subscription_details": { "subscription": "sub_2" } } } },
        }));
        assert_eq!(new.purchase_refs, vec!["sub_2".to_string()]);
    }

    #[test]
    fn only_checkout_completion_carries_a_return_ref() {
        // An invoice's object id is no checkout reference: a renewal's claim
        // reaches the buyer some other way (ruling (2)).
        let b = serde_json::to_vec(&serde_json::json!({
            "id": "evt_i", "type": "invoice.paid",
            "data": { "object": { "id": "in_1" } },
        }))
        .unwrap();
        let ev = StripeProvider
            .verify(SECRET, &headers_for(SECRET, NOW, &b), &b, NOW)
            .unwrap();
        assert_eq!(ev.return_ref, None);
    }

    /// A Checkout Session names the Payment Link it was sold through — the
    /// product reference a per-product tier mapping matches (monetization.md
    /// § Pillar 3 → *Configuration*). No other event carries one: a renewal
    /// is matched by the purchase it belongs to, never by a product.
    #[test]
    fn a_checkout_names_its_payment_link_as_the_product() {
        for ty in [
            "checkout.session.completed",
            "checkout.session.async_payment_succeeded",
        ] {
            let ev = session_event(
                ty,
                serde_json::json!({ "id": "cs_1", "payment_status": "paid",
                                    "payment_link": "plink_1" }),
            );
            assert_eq!(ev.product_reference.as_deref(), Some("plink_1"), "{ty}");
        }
        let expanded = session_event(
            "checkout.session.completed",
            serde_json::json!({ "id": "cs_2", "payment_status": "paid",
                                "payment_link": { "id": "plink_2", "object": "payment_link" } }),
        );
        assert_eq!(expanded.product_reference.as_deref(), Some("plink_2"));
        let hand_built = session_event(
            "checkout.session.completed",
            serde_json::json!({ "id": "cs_3", "payment_status": "paid",
                                "payment_link": null }),
        );
        assert_eq!(hand_built.product_reference, None);
        let invoice = verify_json(serde_json::json!({
            "id": "evt_i", "type": "invoice.paid",
            "data": { "object": { "id": "in_1", "payment_link": "plink_1" } },
        }));
        assert_eq!(invoice.product_reference, None);
    }
}
