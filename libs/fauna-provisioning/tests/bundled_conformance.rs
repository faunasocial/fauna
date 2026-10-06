//! Conformance suite for the open **Fauna Bundled Provider API v1** — THE
//! compliance definition (`docs/goal/architecture/provisioning/
//! bundled-provider-api.md` § Conformance suite; prose never outranks this
//! file).
//!
//! Two halves:
//!
//! 1. **Wire pins (always run, wiremock).** For every endpoint: the exact
//!    request the client adapter sends (path, method, headers, body) and the
//!    response it accepts — including the `409 price_changed` refusal, `404`
//!    as success on both deletes, the RFC 8628 pending / slow-down / denied /
//!    expired arms, the `202 available_after` auth-code arm, and the
//!    zone-relative owner-name contract. An implementer reads these as "what
//!    my server must accept and return".
//! 2. **Live runner (env-gated, self-skipping).** `FAUNA_BUNDLED_BASE_URL` +
//!    `FAUNA_BUNDLED_TOKEN` point the read-only calls at a real
//!    implementation; `FAUNA_BUNDLED_LIVE_MUTATE=1` adds a create-and-delete
//!    round trip for a record and a server. This is how a company proves
//!    conformance before asking for a curated row (spec § Neutrality).

use fauna_provisioning::bundled_api::{
    self, DeviceAuthorization, DevicePoll, device_authorize, device_token,
};
use fauna_provisioning::dns::bundled::BundledDns;
use fauna_provisioning::dns::{DnsProvider, DnsRecord};
use fauna_provisioning::error::ProvisionError;
use fauna_provisioning::registrar::bundled::{AuthCode, BundledRegistrar};
use fauna_provisioning::registrar::{ContactInfo, Registrar, RegistrarAvailability};
use fauna_provisioning::vps::bundled::BundledVps;
use fauna_provisioning::vps::{VpsInstance, VpsProvider};
use wiremock::matchers::{
    body_partial_json, body_string_contains, header, method, path, query_param,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn contact() -> ContactInfo {
    ContactInfo {
        first_name: "Test".into(),
        last_name: "User".into(),
        email: "hello@example.test".into(),
        phone: "+1.5555550100".into(),
        address1: "1 Example Way".into(),
        city: "Exampleton".into(),
        state: "EX".into(),
        postal_code: "00000".into(),
        country: "US".into(),
    }
}

fn me_json() -> serde_json::Value {
    serde_json::json!({
        "api_version": 1,
        "account": { "id": "acct-1" },
        "zones": [{ "id": "z-1", "name": "example.test" }],
        "locations": [{ "id": "eu-1", "name": "Europe 1", "city": "Falkenstein", "country": "DE" }]
    })
}

async fn mock_me(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(me_json()))
        .mount(server)
        .await;
}

// ---------------------------------------------------------------------------
// GET /v1/me — the shared verify
// ---------------------------------------------------------------------------

#[tokio::test]
async fn me_serves_all_three_verifies_with_one_bearer_call() {
    let server = MockServer::start().await;
    mock_me(&server).await;
    let client = reqwest::Client::new();

    let zones = BundledDns::new(server.uri(), "tok".into())
        .verify(&client)
        .await
        .unwrap();
    assert_eq!(zones.len(), 1);
    assert_eq!(
        (zones[0].id.as_str(), zones[0].name.as_str()),
        ("z-1", "example.test")
    );

    let locations = BundledVps::new(server.uri(), "tok".into())
        .verify(&client)
        .await
        .unwrap();
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].id, "eu-1");
    assert_eq!(locations[0].country, "DE");

    BundledRegistrar::new(server.uri(), "tok".into())
        .verify(&client)
        .await
        .unwrap();
}

#[tokio::test]
async fn me_with_a_foreign_api_version_is_refused_by_name() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "api_version": 2 })),
        )
        .mount(&server)
        .await;
    let err = BundledDns::new(server.uri(), "tok".into())
        .verify(&reqwest::Client::new())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("version 2"), "{err}");
}

#[tokio::test]
async fn a_401_surfaces_as_a_provider_error_the_app_renders_as_reenter_credentials() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": { "code": "unauthorized", "message": "token revoked" }
        })))
        .mount(&server)
        .await;
    let err = BundledVps::new(server.uri(), "revoked".into())
        .verify(&reqwest::Client::new())
        .await
        .unwrap_err();
    match err {
        ProvisionError::Provider { status, body } => {
            assert_eq!(status, 401);
            assert_eq!(
                bundled_api::error_code(&body).as_deref(),
                Some("unauthorized")
            );
        }
        other => panic!("expected Provider error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// hosted-auth — RFC 8628 device authorization
// ---------------------------------------------------------------------------

#[tokio::test]
async fn device_authorize_posts_the_public_client_id_form_encoded_without_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/device"))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .and(body_string_contains("client_id=fauna"))
        .and(body_string_contains("scope=provisioning"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_code": "dev-1",
            "user_code": "ABCD-EFGH",
            "verification_uri": "https://bundle.example/activate",
            "verification_uri_complete": "https://bundle.example/activate?user_code=ABCD-EFGH",
            "expires_in": 600,
            "interval": 1
        })))
        .expect(1)
        .mount(&server)
        .await;
    let d = device_authorize(&reqwest::Client::new(), &server.uri())
        .await
        .unwrap();
    assert_eq!(
        d,
        DeviceAuthorization {
            device_code: "dev-1".into(),
            user_code: "ABCD-EFGH".into(),
            verification_uri: "https://bundle.example/activate".into(),
            verification_uri_complete: Some(
                "https://bundle.example/activate?user_code=ABCD-EFGH".into()
            ),
            expires_in: 600,
            interval: 1,
        }
    );
    assert_eq!(
        d.open_url(),
        "https://bundle.example/activate?user_code=ABCD-EFGH"
    );
}

#[tokio::test]
async fn device_token_maps_every_rfc8628_arm() {
    let server = MockServer::start().await;
    let client = reqwest::Client::new();
    for (code, status, body, want) in [
        (
            "d-pending",
            400,
            serde_json::json!({ "error": "authorization_pending" }),
            DevicePoll::Pending,
        ),
        (
            "d-slow",
            400,
            serde_json::json!({ "error": "slow_down" }),
            DevicePoll::SlowDown,
        ),
        (
            "d-expired",
            400,
            serde_json::json!({ "error": "expired_token" }),
            DevicePoll::Expired,
        ),
        (
            "d-denied",
            400,
            serde_json::json!({ "error": "access_denied" }),
            DevicePoll::Denied,
        ),
        (
            "d-ok",
            200,
            serde_json::json!({ "access_token": "tok-final", "token_type": "bearer", "scope": "provisioning" }),
            DevicePoll::Token("tok-final".into()),
        ),
    ] {
        Mock::given(method("POST"))
            .and(path("/v1/auth/token"))
            .and(body_string_contains(
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
            ))
            .and(body_string_contains(format!("device_code={code}")))
            .and(body_string_contains("client_id=fauna"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(&server)
            .await;
        let got = device_token(&client, &server.uri(), code).await.unwrap();
        assert_eq!(got, want, "device_code={code}");
    }
}

#[tokio::test]
async fn device_token_treats_an_unknown_error_as_a_failure_not_a_pending_state() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/token"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;
    let err = device_token(&reqwest::Client::new(), &server.uri(), "d")
        .await
        .unwrap_err();
    assert!(
        matches!(err, ProvisionError::Provider { status: 500, .. }),
        "{err:?}"
    );
}

/// Spec § Endpoints requires `https://`. A plain `http://`
/// base to a NON-loopback host must be refused before either device-flow
/// call reaches the network — never mount a mock, so a regression that
/// dropped the guard would fail on "no request matched" rather than pass by
/// accident.
#[tokio::test]
async fn device_authorize_and_device_token_refuse_a_non_https_base() {
    let client = reqwest::Client::new();
    let err = device_authorize(&client, "http://provider.example")
        .await
        .unwrap_err();
    assert!(matches!(err, ProvisionError::Other(_)), "{err:?}");
    let err = device_token(&client, "http://provider.example", "code")
        .await
        .unwrap_err();
    assert!(matches!(err, ProvisionError::Other(_)), "{err:?}");
}

/// The other half of the same guard: an explicit loopback host on `http://`
/// is the documented development carve-out, not a hole — both device-flow
/// calls must still work exactly as every wiremock-backed test above already
/// exercises via `server.uri()`.
#[tokio::test]
async fn device_authorize_and_device_token_still_work_over_http_loopback() {
    let server = MockServer::start().await;
    assert!(
        server.uri().starts_with("http://127.0.0.1"),
        "this test's premise: wiremock listens on the loopback carve-out, {}",
        server.uri()
    );
    Mock::given(method("POST"))
        .and(path("/v1/auth/device"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_code": "dev-1",
            "user_code": "ABCD-EFGH",
            "verification_uri": "http://provider.example/activate",
            "expires_in": 600,
            "interval": 1
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "access_token": "t" })),
        )
        .mount(&server)
        .await;
    device_authorize(&reqwest::Client::new(), &server.uri())
        .await
        .expect("loopback http:// must still be accepted");
    assert_eq!(
        device_token(&reqwest::Client::new(), &server.uri(), "dev-1")
            .await
            .unwrap(),
        DevicePoll::Token("t".into())
    );
}

/// The dispatch-layer half of the same guard (`bundled_creds`,
/// `dispatch.rs`): a stored `base-url` credential that fails the scheme check
/// must yield no dispatcher at all — the same "missing credential" shape a
/// blank `base-url` already produces — rather than construct one that would
/// attach the bearer token to a cleartext base on every later call.
#[tokio::test]
async fn a_non_https_stored_base_url_yields_no_bundled_dispatcher() {
    use fauna_provisioning::ProviderId;
    use fauna_provisioning::dispatch::{Credentials, dns_provider, registrar, vps_provider};

    let creds = |base: &str| {
        Credentials::from_map(
            [
                ("base-url".to_string(), base.into()),
                ("api-token".to_string(), "tok".to_string().into()),
            ]
            .into_iter()
            .collect(),
        )
    };

    assert!(
        dns_provider(ProviderId::Bundled, creds("http://provider.example"), None).is_none(),
        "a non-https, non-loopback base-url must not yield a DNS dispatcher"
    );
    assert!(
        vps_provider(ProviderId::Bundled, creds("http://provider.example"), None).is_none(),
        "a non-https, non-loopback base-url must not yield a VPS dispatcher"
    );
    assert!(
        registrar(ProviderId::Bundled, creds("http://provider.example")).is_none(),
        "a non-https, non-loopback base-url must not yield a registrar dispatcher"
    );

    // The carve-out still dispatches over loopback — same shape as the
    // device-flow test above, one layer up.
    assert!(dns_provider(ProviderId::Bundled, creds("http://127.0.0.1:9"), None).is_some());
    assert!(dns_provider(ProviderId::Bundled, creds("https://provider.example"), None).is_some());
}

// ---------------------------------------------------------------------------
// Registrar
// ---------------------------------------------------------------------------

#[tokio::test]
async fn check_quotes_first_year_and_renewal_and_folds_status_into_availability() {
    let server = MockServer::start().await;
    let client = reqwest::Client::new();
    for (name, body, want) in [
        (
            "free.example",
            serde_json::json!({ "name": "free.example", "status": "available", "currency": "EUR", "registration_cents": 1099, "renewal_cents": 1499 }),
            RegistrarAvailability::Buyable {
                price_cents: 1099,
                currency: Some("EUR".into()),
                renewal_cents: Some(1499),
            },
        ),
        (
            "taken.example",
            serde_json::json!({ "name": "taken.example", "status": "unavailable" }),
            RegistrarAvailability::Unavailable,
        ),
        (
            "nope.zz",
            serde_json::json!({ "name": "nope.zz", "status": "tld_not_supported" }),
            RegistrarAvailability::TldNotSupported,
        ),
        (
            "unquoted.example",
            serde_json::json!({ "name": "unquoted.example", "status": "available" }),
            RegistrarAvailability::Unavailable,
        ),
    ] {
        Mock::given(method("GET"))
            .and(path("/v1/domains/check"))
            .and(query_param("name", name))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let reg = BundledRegistrar::new(server.uri(), "tok".into());
        assert_eq!(
            reg.availability(&client, name).await.unwrap(),
            want,
            "{name}"
        );
    }
    let raw = BundledRegistrar::new(server.uri(), "tok".into())
        .check(&client, "free.example")
        .await
        .unwrap();
    assert!(raw.available);
    assert_eq!(raw.price_first_year_cents, Some(1099));
    assert_eq!(raw.price_renewal_cents, Some(1499));
    assert_eq!(raw.currency.as_deref(), Some("EUR"));
}

#[tokio::test]
async fn register_posts_the_agreed_price_the_user_contact_and_whois_privacy() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/domains"))
        .and(header("authorization", "Bearer tok"))
        .and(body_partial_json(serde_json::json!({
            "name": "free.example",
            "years": 1,
            "agreed_price_cents": 1099,
            "whois_privacy": true,
            "contact": { "first_name": "Test", "country": "US" }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "name": "free.example",
            "nameservers": ["ns1.bundle.example", "ns2.bundle.example"]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let r = BundledRegistrar::new(server.uri(), "tok".into())
        .register(
            &reqwest::Client::new(),
            "free.example",
            1,
            1099,
            Some(&contact()),
        )
        .await
        .unwrap();
    assert_eq!(r.domain, "free.example");
    assert_eq!(r.nameservers.len(), 2);
}

#[tokio::test]
async fn register_refuses_to_run_without_a_registrant_contact() {
    // Spec § Exit guarantee 1: the domain is registered in the USER's name.
    let server = MockServer::start().await;
    let err = BundledRegistrar::new(server.uri(), "tok".into())
        .register(&reqwest::Client::new(), "free.example", 1, 1099, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("registrant contact"), "{err}");
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "no request may leave"
    );
}

#[tokio::test]
async fn register_surfaces_price_changed_instead_of_paying_a_different_amount() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/domains"))
        .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
            "error": { "code": "price_changed", "message": "now 1299", "details": { "registration_cents": 1299 } }
        })))
        .mount(&server)
        .await;
    let err = BundledRegistrar::new(server.uri(), "tok".into())
        .register(
            &reqwest::Client::new(),
            "free.example",
            1,
            1099,
            Some(&contact()),
        )
        .await
        .unwrap_err();
    match err {
        ProvisionError::Provider { status, body } => {
            assert_eq!(status, 409);
            assert!(body.starts_with("price_changed"), "{body}");
        }
        other => panic!("expected Provider error, got {other:?}"),
    }
}

#[tokio::test]
async fn default_contact_is_prefilled_when_present_and_none_on_404() {
    let server = MockServer::start().await;
    let client = reqwest::Client::new();
    let reg = BundledRegistrar::new(server.uri(), "tok".into());
    Mock::given(method("GET"))
        .and(path("/v1/contact"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "error": { "code": "not_found", "message": "no default contact" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(reg.fetch_default_contact(&client).await.unwrap(), None);
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/v1/contact"))
        .respond_with(ResponseTemplate::new(200).set_body_json(contact()))
        .mount(&server)
        .await;
    assert_eq!(
        reg.fetch_default_contact(&client).await.unwrap(),
        Some(contact())
    );
}

#[tokio::test]
async fn tld_pricing_is_unauthenticated_and_normalizes_tlds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/pricing/tlds"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "currency": "EUR",
            "tlds": [
                { "tld": ".COM", "registration_cents": 1099, "renewal_cents": 1499 },
                { "tld": "net", "registration_cents": 1199, "renewal_cents": 1599 }
            ]
        })))
        .mount(&server)
        .await;
    let quotes = BundledRegistrar::new(server.uri(), "tok".into())
        .list_tld_pricing(&reqwest::Client::new())
        .await
        .unwrap()
        .expect("bundled providers always quote publicly");
    assert_eq!(quotes.len(), 2);
    assert_eq!(quotes[0].tld, "com");
    assert_eq!(quotes[0].currency, "EUR");
    assert_eq!(quotes[1].renewal_cents, 1599);
    let reqs = server.received_requests().await.unwrap();
    assert!(
        reqs.iter()
            .all(|r| !r.headers.contains_key("authorization")),
        "pricing is fetched before the user has signed in — no Bearer"
    );
}

#[tokio::test]
async fn auth_code_is_handed_out_on_demand_or_dated_under_a_transfer_lock() {
    let server = MockServer::start().await;
    let client = reqwest::Client::new();
    let reg = BundledRegistrar::new(server.uri(), "tok".into());
    Mock::given(method("GET"))
        .and(path("/v1/domains/free.example/auth-code"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "auth_code": "EPP-SECRET" })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/domains/locked.example/auth-code"))
        .respond_with(
            ResponseTemplate::new(202)
                .set_body_json(serde_json::json!({ "available_after": "2026-10-25T00:00:00Z" })),
        )
        .mount(&server)
        .await;
    assert_eq!(
        reg.auth_code(&client, "free.example").await.unwrap(),
        AuthCode::Ready("EPP-SECRET".into())
    );
    assert_eq!(
        reg.auth_code(&client, "locked.example").await.unwrap(),
        AuthCode::AvailableAfter("2026-10-25T00:00:00Z".into())
    );
}

// ---------------------------------------------------------------------------
// DNS
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_record_posts_the_zone_relative_owner_verbatim_with_priority_only_when_set() {
    let server = MockServer::start().await;
    let client = reqwest::Client::new();
    let dns = BundledDns::new(server.uri(), "tok".into());
    Mock::given(method("POST"))
        .and(path("/v1/zones/z-1/records"))
        .and(header("authorization", "Bearer tok"))
        .and(body_partial_json(serde_json::json!({ "type": "MX", "name": "@", "value": "mail.example.test", "ttl": 300, "priority": 10 })))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "r-1", "type": "MX", "name": "@", "value": "mail.example.test", "ttl": 300, "priority": 10 })))
        .expect(1)
        .mount(&server)
        .await;
    dns.create_record(
        &client,
        "z-1",
        &DnsRecord {
            record_type: "MX".into(),
            name: "@".into(),
            value: "mail.example.test".into(),
            ttl: 300,
            priority: Some(10),
        },
    )
    .await
    .unwrap();
    Mock::given(method("POST"))
        .and(path("/v1/zones/z-1/records"))
        .and(body_partial_json(serde_json::json!({ "type": "A", "name": "mail", "value": "203.0.113.5", "ttl": 300 })))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "r-2", "type": "A", "name": "mail", "value": "203.0.113.5", "ttl": 300 })))
        .expect(1)
        .mount(&server)
        .await;
    dns.create_record(
        &client,
        "z-1",
        &DnsRecord {
            record_type: "A".into(),
            name: "mail".into(),
            value: "203.0.113.5".into(),
            ttl: 300,
            priority: None,
        },
    )
    .await
    .unwrap();
    let bodies: Vec<serde_json::Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert!(
        bodies[1].get("priority").is_none(),
        "no priority key on an A record: {}",
        bodies[1]
    );
}

#[tokio::test]
async fn find_records_filters_by_name_and_type_and_a_relative_owner_is_never_re_relativized() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/zones/z-1/records"))
        .and(query_param("name", "mail.example.test"))
        .and(query_param("type", "A"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "records": [
                { "id": 7, "type": "A", "name": "mail.example.test", "value": "203.0.113.5", "ttl": 300 },
                { "id": "r-9", "type": "A", "name": "other", "value": "203.0.113.9", "ttl": 300 }
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    // A relative owner that happens to end in the zone name goes to the wire
    // untouched (registry.md § The owner-name contract) and only the exact
    // match comes back, even from a lenient server that over-returns.
    let found = BundledDns::new(server.uri(), "tok".into())
        .find_records(&reqwest::Client::new(), "z-1", "mail.example.test", "A")
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].value, "203.0.113.5");
}

#[tokio::test]
async fn delete_record_resolves_the_id_by_value_and_treats_absent_as_success() {
    let server = MockServer::start().await;
    let client = reqwest::Client::new();
    let dns = BundledDns::new(server.uri(), "tok".into());
    Mock::given(method("GET"))
        .and(path("/v1/zones/z-1/records"))
        .and(query_param("name", "_acme-challenge"))
        .and(query_param("type", "TXT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "records": [
                { "id": "r-keep", "type": "TXT", "name": "_acme-challenge", "value": "keep", "ttl": 60 },
                { "id": "r-gone", "type": "TXT", "name": "_acme-challenge", "value": "gone", "ttl": 60 }
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/zones/z-1/records/r-gone"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    dns.delete_record(&client, "z-1", "_acme-challenge", "TXT", "gone")
        .await
        .unwrap();
    // Already absent (no value match) → no DELETE, still Ok.
    dns.delete_record(&client, "z-1", "_acme-challenge", "TXT", "never-existed")
        .await
        .unwrap();
    // A 404 on the DELETE itself is also success (raced away).
    Mock::given(method("DELETE"))
        .and(path("/v1/zones/z-1/records/r-keep"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    dns.delete_record(&client, "z-1", "_acme-challenge", "TXT", "keep")
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// VPS
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_server_passes_cloud_init_verbatim_and_labels_as_a_map() {
    let server = MockServer::start().await;
    let user_data = "#cloud-config\nruncmd:\n  - echo \"hi\"\n";
    Mock::given(method("POST"))
        .and(path("/v1/servers"))
        .and(header("authorization", "Bearer tok"))
        .and(body_partial_json(serde_json::json!({
            "name": "nest-example-test",
            "location": "eu-1",
            "server_type": "small",
            "user_data": user_data,
            "labels": { "managed-by": "fauna", "fauna-e2e": "1" }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": 4242, "name": "nest-example-test", "ipv4": "203.0.113.5", "status": "provisioning", "labels": {}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let inst = BundledVps::new(server.uri(), "tok".into())
        .create_server(
            &reqwest::Client::new(),
            "nest-example-test",
            "eu-1",
            "small",
            user_data,
            &[
                ("managed-by".into(), "fauna".into()),
                ("fauna-e2e".into(), "1".into()),
            ],
        )
        .await
        .unwrap();
    assert_eq!(inst.server_id, "4242");
    assert_eq!(inst.ipv4, "203.0.113.5");
}

#[tokio::test]
async fn server_types_are_the_providers_own_catalog_in_its_order_capped_at_five() {
    let server = MockServer::start().await;
    let types: Vec<serde_json::Value> = (1..=7)
        .map(|i| {
            serde_json::json!({ "id": format!("t{i}"), "vcpu": i, "mem_gb": (i * 2) as f32, "disk_gb": i * 40, "price_monthly_cents": i * 100, "currency": "EUR" })
        })
        .collect();
    Mock::given(method("GET"))
        .and(path("/v1/server_types"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "server_types": types })),
        )
        .mount(&server)
        .await;
    // The registry's `["*"]` wildcard is ignored — the server curates.
    let got = BundledVps::new(server.uri(), "tok".into())
        .list_server_types(&reqwest::Client::new(), &["*"])
        .await
        .unwrap();
    assert_eq!(
        got.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        ["t1", "t2", "t3", "t4", "t5"]
    );
    assert_eq!(got[1].mem_gb, 4.0);
    assert_eq!(got[4].price_monthly_cents, 500);
}

#[tokio::test]
async fn ptr_get_and_put_are_idempotent_overwrites() {
    let server = MockServer::start().await;
    let client = reqwest::Client::new();
    let vps = BundledVps::new(server.uri(), "tok".into());
    let inst = VpsInstance {
        server_id: "4242".into(),
        ipv4: "203.0.113.5".into(),
    };
    Mock::given(method("GET"))
        .and(path("/v1/servers/4242/ptr"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ptr": null })))
        .mount(&server)
        .await;
    assert_eq!(vps.get_ptr(&client, &inst).await.unwrap(), None);
    Mock::given(method("PUT"))
        .and(path("/v1/servers/4242/ptr"))
        .and(header("authorization", "Bearer tok"))
        .and(body_partial_json(
            serde_json::json!({ "ptr": "mail.example.test" }),
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(2)
        .mount(&server)
        .await;
    vps.set_ptr(&client, &inst, "mail.example.test")
        .await
        .unwrap();
    vps.set_ptr(&client, &inst, "mail.example.test")
        .await
        .unwrap();
}

/// Spec § Exit guarantee 2 — *the server is enumerable and deletable by the
/// user's own token*: `GET /v1/servers?label=managed-by=fauna`. This is the
/// listing primitive the retire view runs on (`../installers/vps.md`
/// § Uninstall → *Listing primitive*), and the filter is what keeps a person
/// from ever being shown a non-fauna server in the same account.
#[tokio::test]
async fn list_managed_servers_sends_the_label_filter() {
    let server = MockServer::start().await;
    let client = reqwest::Client::new();
    let vps = BundledVps::new(server.uri(), "tok".into());

    Mock::given(method("GET"))
        .and(path("/v1/servers"))
        .and(query_param("label", "managed-by=fauna"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "servers": [
                {
                    "id": "s-1",
                    "name": "nest-example-test",
                    "ipv4": "203.0.113.5",
                    // Optional in the spec; carried through when present so
                    // the retire view can scope an `AAAA` removal to it.
                    "ipv6": "2001:db8::5",
                    "status": "running",
                    "labels": { "managed-by": "fauna" }
                },
                {
                    // An implementation that ignores the filter must still not
                    // be able to surface a non-fauna box: the adapter re-checks
                    // the marker client-side.
                    "id": "s-2",
                    "name": "someone-elses-production-db",
                    "ipv4": "203.0.113.99",
                    "status": "running",
                    "labels": { "team": "data" }
                }
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let servers = vps.list_managed_servers(&client).await.unwrap();
    assert_eq!(servers.len(), 1, "unmarked rows must never surface");
    assert_eq!(servers[0].server_id, "s-1");
    assert_eq!(servers[0].name, "nest-example-test");
    assert_eq!(servers[0].ipv4.as_deref(), Some("203.0.113.5"));
    assert_eq!(servers[0].ipv6.as_deref(), Some("2001:db8::5"));
    assert!(servers[0].marked);

    // A listed row is directly the `delete_server` take — no second lookup.
    let instance: VpsInstance = (&servers[0]).into();
    assert_eq!(instance.server_id, "s-1");
}

#[tokio::test]
async fn find_server_by_name_and_delete_with_404_as_success() {
    let server = MockServer::start().await;
    let client = reqwest::Client::new();
    let vps = BundledVps::new(server.uri(), "tok".into());
    Mock::given(method("GET"))
        .and(path("/v1/servers"))
        .and(query_param("name", "nest-example-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "servers": [{ "id": "s-1", "name": "nest-example-test", "ipv4": "203.0.113.5", "status": "running", "labels": { "managed-by": "fauna" } }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/servers"))
        .and(query_param("name", "nope"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "servers": [] })),
        )
        .mount(&server)
        .await;
    let found = vps
        .find_server_by_name(&client, "nest-example-test")
        .await
        .unwrap()
        .expect("listed");
    assert_eq!(found.server_id, "s-1");
    assert!(
        vps.find_server_by_name(&client, "nope")
            .await
            .unwrap()
            .is_none()
    );

    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-1"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-gone"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    vps.delete_server(&client, &found).await.unwrap();
    vps.delete_server(
        &client,
        &VpsInstance {
            server_id: "s-gone".into(),
            ipv4: String::new(),
        },
    )
    .await
    .unwrap();
}

// ---------------------------------------------------------------------------
// Live runner — env-gated, self-skipping (spec § Conformance suite)
// ---------------------------------------------------------------------------

fn live_target() -> Option<(String, String)> {
    let base = std::env::var("FAUNA_BUNDLED_BASE_URL").ok()?;
    let token = std::env::var("FAUNA_BUNDLED_TOKEN").ok()?;
    if base.trim().is_empty() || token.trim().is_empty() {
        return None;
    }
    Some((base, token))
}

/// Read-only half: every endpoint a wizard touches before it commits money,
/// plus the CORS preflight the web build depends on (spec § CORS).
#[tokio::test]
async fn live_read_only_conformance() {
    let Some((base, token)) = live_target() else {
        eprintln!(
            "live_read_only_conformance: FAUNA_BUNDLED_BASE_URL/FAUNA_BUNDLED_TOKEN unset — skipped"
        );
        return;
    };
    let client = reqwest::Client::new();

    // CORS preflight — mandatory on every endpoint; checked on the one
    // every flow starts with.
    let preflight = client
        .request(
            reqwest::Method::OPTIONS,
            format!("{}/v1/me", bundled_api::normalize_base_url(&base)),
        )
        .header("origin", "https://app.example")
        .header("access-control-request-method", "GET")
        .header(
            "access-control-request-headers",
            "authorization, content-type",
        )
        .send()
        .await
        .unwrap();
    assert!(
        preflight.status().is_success(),
        "OPTIONS /v1/me → {}",
        preflight.status()
    );
    assert_eq!(
        preflight
            .headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("*"),
        "spec § CORS: Access-Control-Allow-Origin must be *"
    );

    let dns = BundledDns::new(base.clone(), token.clone());
    let vps = BundledVps::new(base.clone(), token.clone());
    let reg = BundledRegistrar::new(base.clone(), token.clone());
    let zones = dns.verify(&client).await.unwrap();
    let locations = vps.verify(&client).await.unwrap();
    assert!(
        !locations.is_empty(),
        "a bundled provider must offer at least one location"
    );
    reg.verify(&client).await.unwrap();
    let quotes = reg.list_tld_pricing(&client).await.unwrap();
    assert!(
        quotes.is_some_and(|q| !q.is_empty()),
        "public TLD pricing must be non-empty"
    );
    let types = vps.list_server_types(&client, &["*"]).await.unwrap();
    assert!(
        !types.is_empty() && types.len() <= 5,
        "1..=5 curated server types, got {}",
        types.len()
    );
    let probe = format!("fauna-conformance-{}.example", std::process::id());
    match reg.availability(&client, &probe).await.unwrap() {
        RegistrarAvailability::TldNotSupported | RegistrarAvailability::Unavailable => {}
        RegistrarAvailability::Buyable { .. } => panic!(".example must never be sold"),
    }
    if let Some(z) = zones.first() {
        let _ = dns.find_records(&client, &z.id, "@", "A").await.unwrap();
    }
}

/// Mutating half — opt-in: creates and deletes one TXT record in the first
/// zone and one server, asserting the idempotent-delete contract on both.
#[tokio::test]
async fn live_mutating_conformance() {
    let Some((base, token)) = live_target() else {
        eprintln!(
            "live_mutating_conformance: FAUNA_BUNDLED_BASE_URL/FAUNA_BUNDLED_TOKEN unset — skipped"
        );
        return;
    };
    if std::env::var("FAUNA_BUNDLED_LIVE_MUTATE").ok().as_deref() != Some("1") {
        eprintln!("live_mutating_conformance: FAUNA_BUNDLED_LIVE_MUTATE!=1 — skipped");
        return;
    }
    let client = reqwest::Client::new();
    let dns = BundledDns::new(base.clone(), token.clone());
    let vps = BundledVps::new(base.clone(), token.clone());

    let zone = dns
        .verify(&client)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("a zone to test in");
    let rec = DnsRecord {
        record_type: "TXT".into(),
        name: "_fauna-conformance".into(),
        value: format!("probe-{}", std::process::id()),
        ttl: 60,
        priority: None,
    };
    dns.create_record(&client, &zone.id, &rec).await.unwrap();
    let found = dns
        .find_records(&client, &zone.id, &rec.name, "TXT")
        .await
        .unwrap();
    assert!(
        found.iter().any(|r| r.value == rec.value),
        "created record must be listed"
    );
    dns.delete_record(&client, &zone.id, &rec.name, "TXT", &rec.value)
        .await
        .unwrap();
    dns.delete_record(&client, &zone.id, &rec.name, "TXT", &rec.value)
        .await
        .unwrap();

    let location = vps
        .verify(&client)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .id;
    let server_type = vps
        .list_server_types(&client, &["*"])
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .id;
    let name = format!("fauna-conformance-{}", std::process::id());
    let inst = vps
        .create_server(
            &client,
            &name,
            &location,
            &server_type,
            "#cloud-config\n",
            &[
                ("managed-by".into(), "fauna".into()),
                ("fauna-e2e".into(), "1".into()),
            ],
        )
        .await
        .unwrap();
    assert!(
        vps.find_server_by_name(&client, &name)
            .await
            .unwrap()
            .is_some()
    );
    vps.delete_server(&client, &inst).await.unwrap();
    vps.delete_server(&client, &inst).await.unwrap();
}
