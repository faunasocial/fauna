//! Registrar conformance harness — pins per-registrar adapter behavior
//! using wiremock.
//!
//! Pattern after `tests/dns_conformance.rs`. Each registrar gets a section
//! covering `verify`, `check` (available + unavailable), `register` (happy
//! path), and `list_tld_pricing` for impls that override it.
//!
//! The optional sandbox-backed live test (env-var-gated) lives in the
//! impl's `#[cfg(test)] mod tests` to keep it isolated from this hermetic
//! conformance suite.

use fauna_provisioning::ProviderId;
use fauna_provisioning::dispatch::{self, Credentials};
use fauna_provisioning::error::ProvisionError;
use fauna_provisioning::registrar::ContactInfo;
use fauna_provisioning::registrar::DomainAvailability;
use fauna_provisioning::registrar::Registrar;
use fauna_provisioning::registrar::RegistrarAvailability;
use fauna_provisioning::registrar::RegistrationResult;
use fauna_provisioning::registrar::gandi::GandiRegistrar;
use std::collections::HashMap;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// Stub registrar for testing the default `availability()` impl in isolation
// from any specific provider. Returns a fixed `DomainAvailability` from
// `check()`; other methods panic.
// ---------------------------------------------------------------------------

struct StubRegistrar {
    check_response: DomainAvailability,
}

impl Registrar for StubRegistrar {
    async fn verify(&self, _client: &reqwest::Client) -> Result<(), ProvisionError> {
        unreachable!("StubRegistrar::verify not used in availability tests")
    }

    async fn check(
        &self,
        _client: &reqwest::Client,
        _domain: &str,
    ) -> Result<DomainAvailability, ProvisionError> {
        Ok(self.check_response.clone())
    }

    async fn register(
        &self,
        _client: &reqwest::Client,
        _domain: &str,
        _years: u32,
        _agreed_price_cents: u64,
        _contact: Option<&ContactInfo>,
    ) -> Result<RegistrationResult, ProvisionError> {
        unreachable!("StubRegistrar::register not used in availability tests")
    }
}

fn fixture_contact() -> ContactInfo {
    ContactInfo {
        first_name: "Jane".into(),
        last_name: "Doe".into(),
        email: "jane@example.com".into(),
        phone: "+1.5555555555".into(),
        address1: "123 Main St".into(),
        city: "San Francisco".into(),
        state: "CA".into(),
        postal_code: "94110".into(),
        country: "US".into(),
    }
}

// ---------------------------------------------------------------------------
// Gandi: verify
// ---------------------------------------------------------------------------

/// `verify` succeeds against `GET /v5/organization/user-info` with a Bearer
/// token when the API returns 200 + a JSON user-info body. Failure modes
/// (401, 5xx) are covered by the unavailable-token test below.
#[tokio::test]
async fn gandi_verify_succeeds_on_valid_token() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/organization/user-info"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "user-uuid-123",
            "username": "testuser",
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let gandi = GandiRegistrar::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = gandi.verify(&client).await;

    assert!(result.is_ok(), "verify failed on valid token: {:?}", result);
}

/// `verify` returns `ProvisionError::Provider` when the API rejects the
/// token with 401.
#[tokio::test]
async fn gandi_verify_rejects_invalid_token() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/organization/user-info"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "object": "HTTPUnauthorized",
            "cause": "Unauthorized",
            "code": 401,
            "message": "The server could not verify that you are authorized",
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let gandi = GandiRegistrar::with_base_url("bad-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = gandi.verify(&client).await;

    let err = result.expect_err("expected verify to fail with 401");
    match err {
        fauna_provisioning::error::ProvisionError::Provider { status, .. } => {
            assert_eq!(status, 401);
        }
        other => panic!("expected Provider error, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Gandi: check (availability + price)
// ---------------------------------------------------------------------------

/// `check` for an available domain returns `available: true` with the 1-year
/// `price_after_taxes` converted to integer cents and the response currency
/// echoed back.
#[tokio::test]
async fn gandi_check_available_domain_returns_price() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/domain/check"))
        .and(query_param("name", "example.com"))
        .and(query_param("processes", "create"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "products": [{
                "status": "available",
                "name": "example.com",
                "process": "create",
                "currency": "USD",
                "prices": [
                    {
                        "duration_unit": "y",
                        "duration": 1,
                        "type": "create",
                        "price_after_taxes": 12.0,
                        "price_before_taxes": 10.0,
                    },
                    {
                        "duration_unit": "y",
                        "duration": 2,
                        "type": "create",
                        "price_after_taxes": 24.0,
                        "price_before_taxes": 20.0,
                    },
                ],
            }],
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let gandi = GandiRegistrar::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = gandi.check(&client, "example.com").await;

    let avail = result.expect("check failed");
    assert_eq!(avail.domain, "example.com");
    assert!(avail.available, "expected available=true");
    assert_eq!(avail.price_first_year_cents, Some(1200));
    assert_eq!(avail.currency.as_deref(), Some("USD"));
}

/// `check` for an unavailable domain returns `available: false` and no price.
#[tokio::test]
async fn gandi_check_unavailable_domain_has_no_price() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/domain/check"))
        .and(query_param("name", "taken.com"))
        .and(query_param("processes", "create"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "products": [{
                "status": "unavailable",
                "name": "taken.com",
                "process": "create",
                "currency": "USD",
                "prices": [],
            }],
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let gandi = GandiRegistrar::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = gandi.check(&client, "taken.com").await;

    let avail = result.expect("check failed");
    assert_eq!(avail.domain, "taken.com");
    assert!(!avail.available, "expected available=false");
    assert_eq!(avail.price_first_year_cents, None);
}

// ---------------------------------------------------------------------------
// Gandi: register
// ---------------------------------------------------------------------------

/// `register` POSTs to `/v5/domain/domains` with `fqdn`, `duration`, and an
/// `owner` contact mapped from `ContactInfo`. Gandi's create endpoint
/// returns 202 Accepted with a minimal envelope — actual nameservers are
/// assigned by LiveDNS at activation time, so `RegistrationResult.nameservers`
/// is left empty and the caller fetches them via the DNS verify step.
#[tokio::test]
async fn gandi_register_posts_owner_contact_and_duration() {
    let mock = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/domain/domains"))
        .and(header("authorization", "Bearer test-token"))
        .and(body_partial_json(serde_json::json!({
            "fqdn": "example.com",
            "duration": 2,
            "owner": {
                "given": "Jane",
                "family": "Doe",
                "email": "jane@example.com",
                "phone": "+1.5555555555",
                "streetaddr": "123 Main St",
                "city": "San Francisco",
                "state": "CA",
                "zip": "94110",
                "country": "US",
                "type": 0,
            },
        })))
        .respond_with(ResponseTemplate::new(202).set_body_json(serde_json::json!({
            "message": "Created",
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let gandi = GandiRegistrar::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let contact = fixture_contact();
    let result = gandi
        .register(&client, "example.com", 2, 2400, Some(&contact))
        .await;

    let res = result.expect("register failed");
    assert_eq!(res.domain, "example.com");
}

// ---------------------------------------------------------------------------
// Gandi: availability (overridden — distinguishes TldNotSupported)
// ---------------------------------------------------------------------------

async fn gandi_availability_for_status(
    domain: &str,
    status: &str,
    extra_product_fields: serde_json::Value,
) -> RegistrarAvailability {
    let mock = MockServer::start().await;
    let mut product = serde_json::json!({
        "status": status,
        "name": domain,
        "process": "create",
    });
    if let serde_json::Value::Object(extras) = extra_product_fields
        && let serde_json::Value::Object(p) = &mut product
    {
        p.extend(extras);
    }
    Mock::given(method("GET"))
        .and(path("/domain/check"))
        .and(query_param("name", domain))
        .and(query_param("processes", "create"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "products": [product],
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let gandi = GandiRegistrar::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    gandi
        .availability(&client, domain)
        .await
        .expect("availability failed")
}

#[tokio::test]
async fn gandi_availability_buyable_when_available_premium() {
    // Gandi marks premium TLDs (e.g. shorter .com names) as
    // `available_premium` — still buyable, just at a higher price tier.
    let result = gandi_availability_for_status(
        "premium.com",
        "available_premium",
        serde_json::json!({
            "currency": "USD",
            "prices": [{
                "duration_unit": "y",
                "duration": 1,
                "type": "create",
                "price_after_taxes": 250.0,
                "price_before_taxes": 200.0,
            }],
        }),
    )
    .await;
    match result {
        RegistrarAvailability::Buyable {
            price_cents,
            currency,
            ..
        } => {
            assert_eq!(price_cents, 25000);
            assert_eq!(currency.as_deref(), Some("USD"));
        }
        other => panic!("expected Buyable, got: {other:?}"),
    }
}

#[tokio::test]
async fn gandi_availability_unavailable_when_reserved() {
    // `available_reserved` means the TLD's policy reserves the name (e.g.
    // anti-cybersquatting); not generally buyable.
    let result =
        gandi_availability_for_status("reserved.com", "available_reserved", serde_json::json!({}))
            .await;
    assert!(matches!(result, RegistrarAvailability::Unavailable));
}

#[tokio::test]
async fn gandi_availability_tld_not_supported_when_unknown() {
    // Gandi returns `unknown` when the TLD isn't in its tables — distinct
    // from "registered." Wizard surfaces "this registrar can't sell .foo."
    let result = gandi_availability_for_status("site.foo", "unknown", serde_json::json!({})).await;
    assert!(matches!(result, RegistrarAvailability::TldNotSupported));
}

#[tokio::test]
async fn gandi_availability_tld_not_supported_when_error_invalid() {
    // Any `error_*` status from Gandi indicates the registrar can't quote
    // this domain — treat as TldNotSupported so the wizard can route the
    // user to a different registrar rather than showing "already taken."
    let result =
        gandi_availability_for_status("weird.invalid", "error_invalid", serde_json::json!({}))
            .await;
    assert!(matches!(result, RegistrarAvailability::TldNotSupported));
}

// ---------------------------------------------------------------------------
// Dispatch wiring
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Default `availability()` impl: maps `check()` output → RegistrarAvailability
// ---------------------------------------------------------------------------

#[tokio::test]
async fn default_availability_buyable_when_check_returns_available_with_price() {
    let stub = StubRegistrar {
        check_response: DomainAvailability {
            domain: "example.com".into(),
            available: true,
            price_first_year_cents: Some(1200),
            price_renewal_cents: None,
            currency: Some("USD".into()),
        },
    };
    let client = reqwest::Client::new();
    let result = stub.availability(&client, "example.com").await.unwrap();
    match result {
        RegistrarAvailability::Buyable {
            price_cents,
            currency,
            ..
        } => {
            assert_eq!(price_cents, 1200);
            assert_eq!(currency.as_deref(), Some("USD"));
        }
        other => panic!("expected Buyable, got: {other:?}"),
    }
}

#[tokio::test]
async fn default_availability_unavailable_when_check_returns_unavailable() {
    let stub = StubRegistrar {
        check_response: DomainAvailability {
            domain: "taken.com".into(),
            available: false,
            price_first_year_cents: None,
            price_renewal_cents: None,
            currency: None,
        },
    };
    let client = reqwest::Client::new();
    let result = stub.availability(&client, "taken.com").await.unwrap();
    assert!(matches!(result, RegistrarAvailability::Unavailable));
}

#[tokio::test]
async fn default_availability_unavailable_when_available_but_no_price() {
    // Edge case: registrar says available but didn't quote a price. Without
    // a price the wizard can't show a confirmable Buyable option, so it
    // surfaces as Unavailable.
    let stub = StubRegistrar {
        check_response: DomainAvailability {
            domain: "weird.com".into(),
            available: true,
            price_first_year_cents: None,
            price_renewal_cents: None,
            currency: None,
        },
    };
    let client = reqwest::Client::new();
    let result = stub.availability(&client, "weird.com").await.unwrap();
    assert!(matches!(result, RegistrarAvailability::Unavailable));
}

#[test]
fn dispatch_registrar_gandi_with_token_returns_some() {
    let mut map = HashMap::new();
    map.insert("personal-access-token".into(), "abc".into());
    let creds = Credentials::from_map(map);
    assert!(dispatch::registrar(ProviderId::Gandi, creds).is_some());
}

#[test]
fn dispatch_registrar_gandi_without_token_returns_none() {
    let creds = Credentials::default();
    assert!(dispatch::registrar(ProviderId::Gandi, creds).is_none());
}

// ---------------------------------------------------------------------------
// Default `fetch_default_contact()` impl: returns Ok(None) without HTTP
// ---------------------------------------------------------------------------

/// C5: A registrar that doesn't override `fetch_default_contact` gets the
/// trait's default impl, which returns `Ok(None)` without making any HTTP
/// call. Future overrides (Gandi) must not change this behavior for stubs.
#[tokio::test]
async fn default_fetch_default_contact_returns_none() {
    let stub = StubRegistrar {
        check_response: DomainAvailability {
            domain: "unused.com".into(),
            available: false,
            price_first_year_cents: None,
            price_renewal_cents: None,
            currency: None,
        },
    };
    let client = reqwest::Client::new();
    let got = stub
        .fetch_default_contact(&client)
        .await
        .expect("default impl must succeed");
    assert!(
        got.is_none(),
        "default fetch_default_contact must return None"
    );
}

/// C6: `RegistrarDispatch` forwards `fetch_default_contact` to per-variant
/// impls. Porkbun uses the trait default (no override), which returns
/// `Ok(None)` without contacting any server. Gandi's override is exercised
/// in tests C1-C4 once Task 2 lands.
#[tokio::test]
async fn dispatch_fetch_default_contact_routes_correctly() {
    let mut map = HashMap::new();
    map.insert("api-key".into(), "stub".into());
    map.insert("secret-api-key".into(), "stub".into());
    let creds = Credentials::from_map(map);
    let r = dispatch::registrar(ProviderId::Porkbun, creds).expect("porkbun registrar dispatch");
    let client = reqwest::Client::new();
    let got = r
        .fetch_default_contact(&client)
        .await
        .expect("dispatch forwarding must not error");
    assert!(
        got.is_none(),
        "porkbun must use trait default returning None"
    );
}

// ---------------------------------------------------------------------------
// Gandi: fetch_default_contact (overridden — two-call chain to user-info +
// organizations endpoints; pre-fills the wizard's WHOIS contact form)
// ---------------------------------------------------------------------------

/// C1: Both endpoints return 200 with a fully-populated organization payload.
/// All 9 `ContactInfo` fields must be populated from the response. Field
/// names mirror Gandi's API (`given`/`family`/`streetaddr`/`zip`); the
/// adapter maps them to `ContactInfo`'s `first_name`/`last_name`/`address1`/
/// `postal_code` respectively.
#[tokio::test]
async fn gandi_fetch_default_contact_happy_path() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/organization/user-info"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "11111111-1111-1111-1111-111111111111",
        })))
        .expect(1)
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path(
            "/organization/organizations/11111111-1111-1111-1111-111111111111",
        ))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "given": "Alice",
            "family": "Liddell",
            "email": "alice@example.com",
            "phone": "+1234567890",
            "streetaddr": "1 Wonderland Way",
            "city": "Oxford",
            "state": "Oxfordshire",
            "zip": "OX1 1AA",
            "country": "GB",
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let gandi = GandiRegistrar::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let got = gandi
        .fetch_default_contact(&client)
        .await
        .expect("fetch_default_contact must succeed");
    let c = got.expect("happy path returns Some");
    assert_eq!(c.first_name, "Alice");
    assert_eq!(c.last_name, "Liddell");
    assert_eq!(c.email, "alice@example.com");
    assert_eq!(c.phone, "+1234567890");
    assert_eq!(c.address1, "1 Wonderland Way");
    assert_eq!(c.city, "Oxford");
    assert_eq!(c.state, "Oxfordshire");
    assert_eq!(c.postal_code, "OX1 1AA");
    assert_eq!(c.country, "GB");
}

/// C2: Organizations response omits some fields (`family`, `state`, `zip`).
/// `#[serde(default)]` on every field of `GandiOrganization` makes those
/// become empty strings, so the wizard's form materializes with the rest
/// pre-filled and lets the user type the missing pieces.
#[tokio::test]
async fn gandi_fetch_default_contact_missing_fields_are_empty() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/organization/user-info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "22222222-2222-2222-2222-222222222222",
        })))
        .expect(1)
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path(
            "/organization/organizations/22222222-2222-2222-2222-222222222222",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "given": "Bob",
            "email": "bob@example.com",
            "country": "FR",
            // family, phone, streetaddr, city, state, zip omitted
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let gandi = GandiRegistrar::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let c = gandi
        .fetch_default_contact(&client)
        .await
        .expect("fetch_default_contact must succeed even with sparse payload")
        .expect("missing-fields path still returns Some");
    assert_eq!(c.first_name, "Bob");
    assert_eq!(c.last_name, ""); // missing → empty
    assert_eq!(c.email, "bob@example.com");
    assert_eq!(c.phone, ""); // missing → empty
    assert_eq!(c.address1, ""); // missing → empty
    assert_eq!(c.city, ""); // missing → empty
    assert_eq!(c.state, ""); // missing → empty
    assert_eq!(c.postal_code, ""); // missing → empty
    assert_eq!(c.country, "FR");
}

/// C3: user-info returns 500 — `fetch_default_contact` must yield `Ok(None)`
/// rather than propagating the error. Prefill failure must never block
/// `verify_dns`; the wizard treats `None` as "no prefill" and the user just
/// types the contact themselves.
#[tokio::test]
async fn gandi_fetch_default_contact_user_info_500() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/organization/user-info"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&mock)
        .await;

    let gandi = GandiRegistrar::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let got = gandi
        .fetch_default_contact(&client)
        .await
        .expect("user-info 500 must not propagate as Err");
    assert!(got.is_none(), "user-info 500 must yield Ok(None)");
}

/// C4: user-info 200 but the organizations endpoint returns 500. Same
/// rationale as C3 — non-success on either call falls back to `Ok(None)`.
#[tokio::test]
async fn gandi_fetch_default_contact_organizations_500() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/organization/user-info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "33333333-3333-3333-3333-333333333333",
        })))
        .expect(1)
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path(
            "/organization/organizations/33333333-3333-3333-3333-333333333333",
        ))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&mock)
        .await;

    let gandi = GandiRegistrar::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let got = gandi
        .fetch_default_contact(&client)
        .await
        .expect("organizations 500 must not propagate as Err");
    assert!(got.is_none(), "organizations 500 must yield Ok(None)");
}
