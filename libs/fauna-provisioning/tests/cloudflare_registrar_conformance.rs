//! Cloudflare Registrar conformance harness — pins the adapter's behavior
//! using wiremock, especially the structural `TldNotSupported` signal (the
//! `reason: "extension_not_supported_via_api"` field on an unregistrable
//! `domain-check` result) that is this registrar's answer to the beta API's
//! "only a subset of TLDs" gap. Pattern after `namecheap_registrar_conformance.rs`.

use fauna_provisioning::ProviderId;
use fauna_provisioning::dispatch::{self, Credentials};
use fauna_provisioning::registrar::Registrar;
use fauna_provisioning::registrar::RegistrarAvailability;
use fauna_provisioning::registrar::cloudflare::CloudflareRegistrar;
use std::collections::HashMap;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn registrar_with_base(mock_uri: &str) -> CloudflareRegistrar {
    CloudflareRegistrar::with_base_url("test-token".into(), "acct-123".into(), mock_uri.into())
}

// ---------------------------------------------------------------------------
// verify
// ---------------------------------------------------------------------------

/// `verify` probes `domain-check` on the inert `cloudflare.com` domain and
/// succeeds on any well-formed `success: true` response, regardless of that
/// domain's own registrability (Cloudflare exposes no dedicated
/// account/credential-check endpoint for the Registrar scope).
#[tokio::test]
async fn verify_succeeds_on_valid_token_and_account() {
    let mock = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/accounts/acct-123/registrar/domain-check"))
        .and(header("authorization", "Bearer test-token"))
        .and(body_json(
            serde_json::json!({ "domains": ["cloudflare.com"] }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true, "errors": [], "messages": [],
            "result": { "domains": [
                { "name": "cloudflare.com", "registrable": false, "reason": "domain_unavailable" }
            ] },
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let cf = registrar_with_base(&mock.uri());
    let client = reqwest::Client::new();
    let result = cf.verify(&client).await;
    assert!(result.is_ok(), "verify failed on valid token: {:?}", result);
}

/// `verify` returns `ProvisionError::Provider` when the API rejects the
/// token/account_id with a non-2xx status.
#[tokio::test]
async fn verify_rejects_invalid_token() {
    let mock = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/accounts/acct-123/registrar/domain-check"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "success": false,
            "errors": [{"code": 9109, "message": "Invalid access token"}],
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let cf = registrar_with_base(&mock.uri());
    let client = reqwest::Client::new();
    let err = cf.verify(&client).await.expect_err("expected 403 to fail");
    match err {
        fauna_provisioning::error::ProvisionError::Provider { status, .. } => {
            assert_eq!(status, 403);
        }
        other => panic!("expected Provider error, got: {other:?}"),
    }
}

/// A 200 envelope with `success: false` (Cloudflare's in-band failure shape)
/// must also fail — checking only the HTTP status would misread it as ok.
#[tokio::test]
async fn verify_rejects_success_false_envelope() {
    let mock = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/accounts/acct-123/registrar/domain-check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": false,
            "errors": [{"code": 1003, "message": "Invalid account identifier"}],
            "messages": [],
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let cf = registrar_with_base(&mock.uri());
    let client = reqwest::Client::new();
    let err = cf
        .verify(&client)
        .await
        .expect_err("success:false must not read as ok");
    assert!(err.to_string().contains("Invalid account identifier"));
}

// ---------------------------------------------------------------------------
// check (availability + price)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn check_available_domain_returns_price() {
    let mock = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/accounts/acct-123/registrar/domain-check"))
        .and(body_json(serde_json::json!({ "domains": ["acmecorp.dev"] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true, "errors": [], "messages": [],
            "result": { "domains": [{
                "name": "acmecorp.dev", "registrable": true, "tier": "standard",
                "pricing": {"currency": "USD", "registration_cost": "10.11", "renewal_cost": "10.11"},
            }] },
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let cf = registrar_with_base(&mock.uri());
    let client = reqwest::Client::new();
    let avail = cf
        .check(&client, "acmecorp.dev")
        .await
        .expect("check failed");
    assert_eq!(avail.domain, "acmecorp.dev");
    assert!(avail.available);
    assert_eq!(avail.price_first_year_cents, Some(1011));
    assert_eq!(avail.price_renewal_cents, Some(1011));
    assert_eq!(avail.currency.as_deref(), Some("USD"));
}

#[tokio::test]
async fn check_unavailable_domain_has_no_price() {
    let mock = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/accounts/acct-123/registrar/domain-check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true, "errors": [], "messages": [],
            "result": { "domains": [
                { "name": "taken.com", "registrable": false, "reason": "domain_unavailable" }
            ] },
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let cf = registrar_with_base(&mock.uri());
    let client = reqwest::Client::new();
    let avail = cf.check(&client, "taken.com").await.expect("check failed");
    assert!(!avail.available);
    assert_eq!(avail.price_first_year_cents, None);
    assert_eq!(avail.currency, None);
}

// ---------------------------------------------------------------------------
// availability (overridden — structural TldNotSupported via `reason`)
// ---------------------------------------------------------------------------

async fn availability_for(domain_check_body: serde_json::Value) -> RegistrarAvailability {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/accounts/acct-123/registrar/domain-check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true, "errors": [], "messages": [],
            "result": { "domains": [domain_check_body] },
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let cf = registrar_with_base(&mock.uri());
    let client = reqwest::Client::new();
    cf.availability(&client, "example.test")
        .await
        .expect("availability failed")
}

#[tokio::test]
async fn availability_buyable_when_registrable_with_pricing() {
    let result = availability_for(serde_json::json!({
        "name": "acmecorp.dev", "registrable": true, "tier": "standard",
        "pricing": {"currency": "USD", "registration_cost": "10.11", "renewal_cost": "10.11"},
    }))
    .await;
    match result {
        RegistrarAvailability::Buyable {
            price_cents,
            currency,
            ..
        } => {
            assert_eq!(price_cents, 1011);
            assert_eq!(currency.as_deref(), Some("USD"));
        }
        other => panic!("expected Buyable, got: {other:?}"),
    }
}

/// The structural TldNotSupported signal this adapter relies on: Cloudflare's
/// beta-API "only a subset of TLDs" gap surfaces as `registrable: false` +
/// `reason: "extension_not_supported_via_api"` — no static TLD list needed.
#[tokio::test]
async fn availability_tld_not_supported_when_extension_not_supported_via_api() {
    let result = availability_for(serde_json::json!({
        "name": "mybrand.uk", "registrable": false, "reason": "extension_not_supported_via_api",
    }))
    .await;
    assert!(matches!(result, RegistrarAvailability::TldNotSupported));
}

/// A domain that's simply taken (not a TLD-coverage problem) reads as
/// `Unavailable`, distinguished from the TLD-not-supported case above by the
/// `reason` value alone.
#[tokio::test]
async fn availability_unavailable_when_domain_unavailable() {
    let result = availability_for(serde_json::json!({
        "name": "taken.com", "registrable": false, "reason": "domain_unavailable",
    }))
    .await;
    assert!(matches!(result, RegistrarAvailability::Unavailable));
}

/// Defensive edge case: `registrable: true` but no `pricing` object (not
/// documented as possible, but the response is attacker/upstream-controlled
/// JSON) must not be read as a confirmable `Buyable` with no price.
#[tokio::test]
async fn availability_unavailable_when_registrable_but_unpriced() {
    let result = availability_for(serde_json::json!({
        "name": "weird.dev", "registrable": true,
    }))
    .await;
    assert!(matches!(result, RegistrarAvailability::Unavailable));
}

// ---------------------------------------------------------------------------
// register
// ---------------------------------------------------------------------------

/// `register` POSTs `{"domain_name": domain}` — no duration or price-ack
/// field exists on this endpoint, and no contact (account-level default).
#[tokio::test]
async fn register_posts_domain_name_only() {
    let mock = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/accounts/acct-123/registrar/registrations"))
        .and(header("authorization", "Bearer test-token"))
        .and(body_json(
            serde_json::json!({ "domain_name": "acmecorp.dev" }),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "success": true, "errors": [], "messages": [],
            "result": {
                "domain_name": "acmecorp.dev", "state": "succeeded", "completed": true,
                "created_at": "2026-07-23T10:00:00Z", "updated_at": "2026-07-23T10:00:03Z",
            },
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let cf = registrar_with_base(&mock.uri());
    let client = reqwest::Client::new();
    let res = cf
        .register(&client, "acmecorp.dev", 1, 1011, None)
        .await
        .expect("register failed");
    assert_eq!(res.domain, "acmecorp.dev");
    assert!(
        res.nameservers.is_empty(),
        "no nameservers field in the response — discovered via the post-register dns_verify step"
    );
}

/// A 202 Accepted ("state": "in_progress") is still a successful acceptance
/// of the registration request — Cloudflare and Gandi both use 202 this way,
/// and neither the trait nor the orchestrator polls a status endpoint today.
#[tokio::test]
async fn register_accepts_202_in_progress() {
    let mock = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/accounts/acct-123/registrar/registrations"))
        .respond_with(ResponseTemplate::new(202).set_body_json(serde_json::json!({
            "success": true, "errors": [], "messages": [],
            "result": {
                "domain_name": "acmecorp.dev", "state": "in_progress", "completed": false,
                "created_at": "2026-07-23T10:00:00Z", "updated_at": "2026-07-23T10:00:10Z",
            },
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let cf = registrar_with_base(&mock.uri());
    let client = reqwest::Client::new();
    let res = cf
        .register(&client, "acmecorp.dev", 1, 1011, None)
        .await
        .expect("register failed");
    assert_eq!(res.domain, "acmecorp.dev");
}

/// A `success: false` envelope under a 2xx status must fail loudly — the
/// same lying-success class of bug the Namecheap adapter had to fix for.
#[tokio::test]
async fn register_rejects_success_false_envelope() {
    let mock = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/accounts/acct-123/registrar/registrations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": false,
            "errors": [{"code": 1100, "message": "No default registrant contact configured"}],
            "messages": [],
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let cf = registrar_with_base(&mock.uri());
    let client = reqwest::Client::new();
    let err = cf
        .register(&client, "acmecorp.dev", 1, 1011, None)
        .await
        .expect_err("success:false must not read as registered");
    assert!(err.to_string().contains("No default registrant contact"));
}

// ---------------------------------------------------------------------------
// registry integration
// ---------------------------------------------------------------------------

#[test]
fn requires_contact_is_false() {
    let cf = CloudflareRegistrar::new("t".into(), "a".into());
    assert!(
        !cf.requires_contact(),
        "Cloudflare uses the account's default registrant contact"
    );
}

/// The registry declares Cloudflare a registrar, so `dispatch::registrar`
/// must hand back an adapter for it — the flow-trace link between
/// `i18n/providers.yaml` and the impl.
#[test]
fn dispatch_returns_a_cloudflare_registrar() {
    let mut map = HashMap::new();
    map.insert("api-token".into(), "t".into());
    map.insert("account-id".into(), "acct-123".into());
    let creds = Credentials::from_map(map);
    assert!(
        dispatch::registrar(ProviderId::Cloudflare, creds).is_some(),
        "cloudflare declares the registrar capability but dispatch returns None"
    );
}

/// Missing `account-id` (registrar-only field, not needed by DNS) yields
/// `None` rather than a half-built adapter that would 404 on every call.
#[test]
fn dispatch_returns_none_without_account_id() {
    let mut map = HashMap::new();
    map.insert("api-token".into(), "t".into());
    let creds = Credentials::from_map(map);
    assert!(dispatch::registrar(ProviderId::Cloudflare, creds).is_none());
}

/// `requires_contact` reaches the wizard through the dispatcher — this is
/// what skips the contact form, and `registrar_requires_contact: false` in
/// the registry must agree with it.
#[test]
fn dispatch_reports_that_cloudflare_does_not_require_a_contact() {
    use fauna_provisioning::PROVIDERS;
    let mut map = HashMap::new();
    map.insert("api-token".into(), "t".into());
    map.insert("account-id".into(), "acct-123".into());
    let creds = Credentials::from_map(map);
    let d = dispatch::registrar(ProviderId::Cloudflare, creds).expect("adapter");
    assert!(!d.requires_contact());

    let meta = PROVIDERS
        .iter()
        .find(|p| p.id == ProviderId::Cloudflare)
        .expect("cloudflare in the registry");
    assert_eq!(
        meta.registrar_requires_contact,
        Some(false),
        "the registry's registrar_requires_contact must match the adapter"
    );
}

/// Every Cloudflare Registrar endpoint requires a bearer token, so the
/// pre-credential TLD price table is genuinely unavailable — `Ok(None)`,
/// the trait default (no override), never a fabricated table.
#[tokio::test]
async fn list_tld_pricing_is_none_because_cloudflare_requires_auth() {
    let cf =
        CloudflareRegistrar::with_base_url("t".into(), "a".into(), "http://127.0.0.1:1".into());
    let client = reqwest::Client::new();
    let result = cf
        .list_tld_pricing(&client)
        .await
        .expect("default impl must not make a network call");
    assert!(result.is_none());
}
