//! Spaceship's registrar and DNS adapters, pinned per call against wiremock
//! stand-ins of the shapes its published API reference gives
//! (`docs/goal/architecture/provisioning/registry.md` § Delegation doors, the
//! Spaceship paragraph). No live credential exists: every shape here is the
//! reference's, and the goal doc lists what the first live run must witness.
//! The nameserver door's own tests live with the other doors in
//! `delegation_doors.rs`.

use std::time::Duration;

use fauna_provisioning::dns::spaceship::SpaceshipDns;
use fauna_provisioning::dns::{DnsProvider, DnsRecord};
use fauna_provisioning::error::ProvisionError;
use fauna_provisioning::registrar::spaceship::{PollPolicy, SpaceshipRegistrar};
use fauna_provisioning::registrar::{ContactInfo, Registrar, RegistrarAvailability};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const NO_WAIT: PollPolicy = PollPolicy {
    interval: Duration::ZERO,
    attempts: 3,
};

fn registrar(mock: &MockServer) -> SpaceshipRegistrar {
    SpaceshipRegistrar::with_base_url("k".into(), "s".into(), mock.uri()).with_poll(NO_WAIT)
}

fn dns(mock: &MockServer) -> SpaceshipDns {
    SpaceshipDns::with_base_url("k".into(), "s".into(), mock.uri())
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

fn contact() -> ContactInfo {
    ContactInfo {
        first_name: "Ada".into(),
        last_name: "Lovelace".into(),
        email: "ada@example.com".into(),
        phone: "+1 555 123 4567".into(),
        address1: "1 Example St".into(),
        city: "Springfield".into(),
        state: "IL".into(),
        postal_code: "62701".into(),
        country: "US".into(),
    }
}

fn price(amount: &str) -> serde_json::Value {
    json!({
        "period": {"value": 1, "unit": "y"},
        "priceInDefaultCurrency": {
            "finalPrice": {"amount": amount, "currency": "USD"},
            "regularPrice": {"amount": "99.00", "currency": "USD"}
        }
    })
}

async fn mount_available(mock: &MockServer, domain: &str, result: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/domains/{domain}/available")))
        .and(header("x-api-key", "k"))
        .and(header("x-api-secret", "s"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "domain": domain, "result": result, "premiumPricing": []
        })))
        .mount(mock)
        .await;
}

async fn mount_pricing(mock: &MockServer, domain: &str, register: &str, renew: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/domains/{domain}/pricing")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "domain": domain,
            "priceClass": "standard",
            "prices": {
                "register": [price(register), {
                    "period": {"value": 2, "unit": "y"},
                    "priceInDefaultCurrency": {
                        "finalPrice": {"amount": "1.00", "currency": "USD"},
                        "regularPrice": {"amount": "1.00", "currency": "USD"}
                    }
                }],
                "renew": [price(renew)]
            }
        })))
        .mount(mock)
        .await;
}

// ---------------------------------------------------------------------------
// Registrar
// ---------------------------------------------------------------------------

#[tokio::test]
async fn verify_lists_one_domain_with_the_key_pair() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/domains"))
        .and(query_param("take", "1"))
        .and(query_param("skip", "0"))
        .and(header("x-api-key", "k"))
        .and(header("x-api-secret", "s"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [], "total": 0})))
        .expect(1)
        .mount(&mock)
        .await;
    registrar(&mock).verify(&client()).await.expect("accepted");
}

#[tokio::test]
async fn verify_surfaces_a_refused_key() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/domains"))
        .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"detail":"Unauthorized"}"#))
        .mount(&mock)
        .await;
    match registrar(&mock).verify(&client()).await {
        Err(ProvisionError::Provider { status, .. }) => assert_eq!(status, 401),
        other => panic!("expected a 401 Provider error, got {other:?}"),
    }
}

#[tokio::test]
async fn an_available_domain_is_buyable_at_its_one_year_final_price() {
    let mock = MockServer::start().await;
    mount_available(&mock, "example.net", "available").await;
    mount_pricing(&mock, "example.net", "5.98", "10.98").await;
    let got = registrar(&mock)
        .availability(&client(), "example.net")
        .await
        .unwrap();
    assert_eq!(
        got,
        RegistrarAvailability::Buyable {
            price_cents: 598,
            currency: Some("USD".into()),
            renewal_cents: Some(1098),
        }
    );
    let check = registrar(&mock)
        .check(&client(), "example.net")
        .await
        .unwrap();
    assert!(check.available);
    assert_eq!(check.price_first_year_cents, Some(598));
    assert_eq!(check.price_renewal_cents, Some(1098));
}

#[tokio::test]
async fn taken_and_unsupported_tlds_are_told_apart() {
    let mock = MockServer::start().await;
    mount_available(&mock, "taken.example", "taken").await;
    mount_available(&mock, "name.example", "tldNotSupported").await;
    let r = registrar(&mock);
    assert_eq!(
        r.availability(&client(), "taken.example").await.unwrap(),
        RegistrarAvailability::Unavailable
    );
    assert_eq!(
        r.availability(&client(), "name.example").await.unwrap(),
        RegistrarAvailability::TldNotSupported
    );
    // No price is read for a name that cannot be bought.
    let priced = mock
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().ends_with("/pricing"))
        .count();
    assert_eq!(priced, 0);
}

#[tokio::test]
async fn an_invalid_name_or_an_unexpected_error_is_an_error_not_a_verdict() {
    let mock = MockServer::start().await;
    mount_available(&mock, "bad..net", "invalidDomainName").await;
    mount_available(&mock, "flaky.example", "unexpectedError").await;
    let r = registrar(&mock);
    assert!(r.availability(&client(), "bad..net").await.is_err());
    assert!(r.availability(&client(), "flaky.example").await.is_err());
}

async fn mount_register_flow(mock: &MockServer, final_status: &str) {
    Mock::given(method("PUT"))
        .and(path("/contacts"))
        .and(body_json(json!({
            "firstName": "Ada",
            "lastName": "Lovelace",
            "email": "ada@example.com",
            "address1": "1 Example St",
            "city": "Springfield",
            "country": "US",
            "stateProvince": "IL",
            "postalCode": "62701",
            "phone": "+1.5551234567"
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"contactId": "1ZdMXpapqp9sle5dl8BlppTJXAzf3"})),
        )
        .expect(1)
        .mount(mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/domains/example.net"))
        .and(body_json(json!({
            "autoRenew": false,
            "years": 1,
            "privacyProtection": {"level": "high", "userConsent": false},
            "contacts": {
                "registrant": "1ZdMXpapqp9sle5dl8BlppTJXAzf3",
                "admin": "1ZdMXpapqp9sle5dl8BlppTJXAzf3",
                "tech": "1ZdMXpapqp9sle5dl8BlppTJXAzf3",
                "billing": "1ZdMXpapqp9sle5dl8BlppTJXAzf3"
            }
        })))
        .respond_with(
            ResponseTemplate::new(202).insert_header("spaceship-async-operationid", "op42"),
        )
        .expect(1)
        .mount(mock)
        .await;
    // First read pending, then the final status.
    Mock::given(method("GET"))
        .and(path("/async-operations/op42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "pending", "type": "domains_Create", "createdAt": "2100-01-01T00:00:00.000Z"
        })))
        .up_to_n_times(1)
        .mount(mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/async-operations/op42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": final_status,
            "type": "domains_Create",
            "details": {"reason": "registry refused the name"},
            "createdAt": "2100-01-01T00:00:00.000Z"
        })))
        .mount(mock)
        .await;
}

#[tokio::test]
async fn register_saves_the_contact_creates_and_polls_the_operation_to_success() {
    let mock = MockServer::start().await;
    mount_pricing(&mock, "example.net", "5.98", "10.98").await;
    mount_register_flow(&mock, "success").await;
    let result = registrar(&mock)
        .register(&client(), "example.net", 1, 598, Some(&contact()))
        .await
        .expect("registered");
    assert_eq!(result.domain, "example.net");
    let polls = mock
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/async-operations/op42")
        .count();
    assert_eq!(polls, 2, "pending, then success");
}

#[tokio::test]
async fn a_failed_operation_is_an_error_naming_the_reason() {
    let mock = MockServer::start().await;
    mount_pricing(&mock, "example.net", "5.98", "10.98").await;
    mount_register_flow(&mock, "failed").await;
    let err = registrar(&mock)
        .register(&client(), "example.net", 1, 598, Some(&contact()))
        .await
        .expect_err("failed operation");
    assert!(
        err.to_string().contains("registry refused the name"),
        "{err}"
    );
}

#[tokio::test]
async fn an_operation_still_pending_at_the_bound_says_it_may_still_complete() {
    let mock = MockServer::start().await;
    mount_pricing(&mock, "example.net", "5.98", "10.98").await;
    mount_register_flow(&mock, "pending").await;
    let err = registrar(&mock)
        .register(&client(), "example.net", 1, 598, Some(&contact()))
        .await
        .expect_err("timed out");
    let text = err.to_string();
    assert!(
        text.contains("op42") && text.contains("may still complete"),
        "{text}"
    );
}

/// The create carries no price, so a price that rose above the confirmed one
/// is refused before the contact is saved or anything is charged.
#[tokio::test]
async fn a_price_above_the_confirmed_one_is_refused_before_any_charge() {
    let mock = MockServer::start().await;
    mount_pricing(&mock, "example.net", "7.00", "10.98").await;
    let err = registrar(&mock)
        .register(&client(), "example.net", 1, 598, Some(&contact()))
        .await
        .expect_err("price rose");
    let text = err.to_string();
    assert!(text.contains("7.00") && text.contains("5.98"), "{text}");
    let writes = mock
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() != "GET")
        .count();
    assert_eq!(writes, 0, "nothing saved or created");
}

#[tokio::test]
async fn register_without_a_contact_is_refused_before_the_network() {
    let mock = MockServer::start().await;
    let r = registrar(&mock);
    assert!(r.requires_contact());
    assert!(
        r.register(&client(), "example.net", 1, 598, None)
            .await
            .is_err()
    );
    assert!(mock.received_requests().await.unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// DNS
// ---------------------------------------------------------------------------

#[tokio::test]
async fn verify_pages_the_accounts_domains_into_zones() {
    let mock = MockServer::start().await;
    let page = |names: &[&str]| {
        json!({
            "items": names.iter().map(|n| json!({"name": n, "unicodeName": n})).collect::<Vec<_>>(),
            "total": 101
        })
    };
    let first: Vec<String> = (0..100).map(|i| format!("d{i}.example")).collect();
    let first_refs: Vec<&str> = first.iter().map(String::as_str).collect();
    Mock::given(method("GET"))
        .and(path("/domains"))
        .and(query_param("skip", "0"))
        .and(query_param("take", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(&first_refs)))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/domains"))
        .and(query_param("skip", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(&["last.example"])))
        .mount(&mock)
        .await;
    let zones = dns(&mock).verify(&client()).await.unwrap();
    assert_eq!(zones.len(), 101);
    assert_eq!(zones[100].id, "last.example");
    assert_eq!(zones[100].name, "last.example");
    assert!(zones[100].nameservers.is_empty(), "no zone door");
}

#[tokio::test]
async fn create_record_puts_one_typed_item() {
    let mock = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/dns/records/example.net"))
        .and(header("x-api-key", "k"))
        .and(body_json(json!({
            "force": true,
            "items": [{
                "type": "MX", "name": "@", "exchange": "mail.example.net",
                "preference": 10, "ttl": 3600
            }]
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&mock)
        .await;
    dns(&mock)
        .create_record(
            &client(),
            "example.net",
            &DnsRecord {
                record_type: "MX".into(),
                name: "@".into(),
                value: "mail.example.net.".into(),
                ttl: 3600,
                priority: Some(10),
            },
        )
        .await
        .expect("created");
}

#[tokio::test]
async fn create_record_surfaces_a_validation_refusal() {
    let mock = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/dns/records/example.net"))
        .respond_with(ResponseTemplate::new(422).set_body_string(
            r#"{"detail":"Validation failed","data":[{"field":"items[0].value","details":"too long"}]}"#,
        ))
        .mount(&mock)
        .await;
    let err = dns(&mock)
        .create_record(
            &client(),
            "example.net",
            &DnsRecord {
                record_type: "TXT".into(),
                name: "@".into(),
                value: "v=spf1 mx -all".into(),
                ttl: 300,
                priority: None,
            },
        )
        .await
        .expect_err("refused");
    match err {
        ProvisionError::Provider { status, body } => {
            assert_eq!(status, 422);
            assert!(body.contains("too long"), "{body}");
        }
        other => panic!("expected Provider error, got {other:?}"),
    }
}

async fn mount_records(mock: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/dns/records/example.net"))
        .and(query_param("take", "100"))
        .and(query_param("skip", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                {"type": "TXT", "name": "@", "ttl": 300, "value": "v=spf1 mx -all", "group": {"type": "custom"}},
                {"type": "TXT", "name": "@", "ttl": 300, "value": "google-site-verification=x", "group": {"type": "custom"}},
                {"type": "MX", "name": "@", "ttl": 300, "exchange": "mail.example.net", "preference": 10, "group": {"type": "custom"}},
                {"type": "A", "name": "mail", "ttl": 300, "address": "192.0.2.1", "group": {"type": "custom"}},
                {"type": "SRV", "name": "@", "ttl": 300, "service": "_caldavs", "protocol": "_tcp",
                 "priority": 0, "weight": 1, "port": 443, "target": "mail.example.net", "group": {"type": "custom"}}
            ],
            "total": 5
        })))
        .mount(mock)
        .await;
}

#[tokio::test]
async fn find_records_filters_by_name_and_type() {
    let mock = MockServer::start().await;
    mount_records(&mock).await;
    let d = dns(&mock);
    let txt = d
        .find_records(&client(), "example.net", "@", "TXT")
        .await
        .unwrap();
    assert_eq!(txt.len(), 2);
    let mx = d
        .find_records(&client(), "example.net", "@", "MX")
        .await
        .unwrap();
    assert_eq!(mx.len(), 1);
    assert_eq!(mx[0].value, "mail.example.net");
    assert_eq!(mx[0].priority, Some(10));
    let srv = d
        .find_records(&client(), "example.net", "_caldavs._tcp", "SRV")
        .await
        .unwrap();
    assert_eq!(srv.len(), 1);
    assert_eq!(srv[0].value, "0 1 443 mail.example.net");
    assert!(
        d.find_records(&client(), "example.net", "www", "A")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn delete_record_sends_the_matching_item_only() {
    let mock = MockServer::start().await;
    mount_records(&mock).await;
    Mock::given(method("DELETE"))
        .and(path("/dns/records/example.net"))
        .and(body_json(json!([
            {"type": "TXT", "name": "@", "value": "v=spf1 mx -all"}
        ])))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&mock)
        .await;
    dns(&mock)
        .delete_record(&client(), "example.net", "@", "TXT", "\"v=spf1 mx -all\"")
        .await
        .expect("deleted");
}

#[tokio::test]
async fn deleting_an_absent_record_is_a_no_op() {
    let mock = MockServer::start().await;
    mount_records(&mock).await;
    dns(&mock)
        .delete_record(&client(), "example.net", "@", "TXT", "v=DMARC1; p=none")
        .await
        .expect("idempotent");
    let deletes = mock
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "DELETE")
        .count();
    assert_eq!(deletes, 0);
}
