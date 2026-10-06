//! Namecheap registrar conformance — pins the adapter's behaviour against a
//! wiremock stand-in for Namecheap's single query-driven XML endpoint.
//!
//! Companion to `registrar_conformance.rs` (Gandi) and `dns_conformance.rs`
//! (the Namecheap DNS half). Namecheap gets its own file because its whole API
//! is one path selected by a `Command` query parameter, so every mock here
//! matches on `query_param("Command", …)` rather than on a path.
//!
//! ⚠ The recurring hazard this suite exists to pin: **Namecheap reports
//! API-level failures with HTTP 200** and a `Status="ERROR"` body. An adapter
//! that trusts the transport status reads every failure as a success — which is
//! precisely the bug found in the DNS half on 2026-07-22, where a refused
//! `setHosts` reported a zone write that never happened. For a registrar the
//! same shape means *reporting a domain purchase that never occurred*, so the
//! error-path tests below matter more than the happy path.

use fauna_provisioning::ProviderId;
use fauna_provisioning::dispatch::{self, Credentials};
use fauna_provisioning::registrar::namecheap::NamecheapRegistrar;
use fauna_provisioning::registrar::{ContactInfo, Registrar, RegistrarAvailability};
use std::collections::HashMap;
use wiremock::matchers::{method, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture_contact() -> ContactInfo {
    ContactInfo {
        first_name: "Jane".into(),
        last_name: "Doe".into(),
        email: "jane@example.com".into(),
        phone: "+1 555 555 5555".into(),
        address1: "123 Main St".into(),
        city: "San Francisco".into(),
        state: "CA".into(),
        postal_code: "94110".into(),
        country: "US".into(),
    }
}

fn ok_body(inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<ApiResponse Status="OK" xmlns="http://api.namecheap.com/xml.response">
  <CommandResponse>{inner}</CommandResponse>
</ApiResponse>"#
    )
}

fn error_body(number: &str, message: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<ApiResponse Status="ERROR" xmlns="http://api.namecheap.com/xml.response">
  <Errors><Error Number="{number}">{message}</Error></Errors>
</ApiResponse>"#
    )
}

/// A `<Price>` table for `users.getPricing`, one 1-year REGISTER row.
fn pricing_body(your_price: &str, additional: &str) -> String {
    ok_body(&format!(
        r#"<UserGetPricingResult><ProductType Name="domains"><ProductCategory Name="register">
        <Product Name="com">
          <Price Duration="1" DurationType="YEAR" Price="13.98" RegularPrice="13.98" YourPrice="{your_price}" Currency="USD" AdditionalCost="{additional}" YourAdditonalCost="{additional}"/>
          <Price Duration="2" DurationType="YEAR" Price="27.96" YourPrice="25.96" Currency="USD"/>
        </Product></ProductCategory></ProductType></UserGetPricingResult>"#
    ))
}

async fn mount_pricing(mock: &MockServer, your_price: &str, additional: &str) {
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.users.getPricing"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(pricing_body(your_price, additional)),
        )
        .mount(mock)
        .await;
}

async fn mount_check(mock: &MockServer, attrs: &str) {
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.check"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(ok_body(&format!(r#"<DomainCheckResult {attrs}/>"#))),
        )
        .mount(mock)
        .await;
}

// ---------------------------------------------------------------------------
// verify
// ---------------------------------------------------------------------------

/// `verify` proves the credentials *and* that the account can be billed, via
/// `users.getBalances`. It must also carry Namecheap's five global parameters
/// — a call missing any of them is rejected outright.
#[tokio::test]
async fn verify_calls_get_balances_with_the_global_parameters() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.users.getBalances"))
        .and(query_param("ApiUser", "apiuser"))
        .and(query_param("ApiKey", "apikey"))
        .and(query_param("UserName", "apiuser"))
        // Seeded, never empty: Namecheap rejects a missing ClientIp (1010105).
        .and(query_param("ClientIp", "192.0.2.1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ok_body(
            r#"<UserGetBalancesResult Currency="USD" AvailableBalance="42.00"/>"#,
        )))
        .expect(1)
        .mount(&mock)
        .await;

    let nc = NamecheapRegistrar::with_base_url("apiuser".into(), "apikey".into(), mock.uri());
    nc.verify(&reqwest::Client::new())
        .await
        .expect("verify must succeed on an OK envelope");
}

/// **The load-bearing one.** Bad credentials come back as HTTP 200 with a
/// `Status="ERROR"` body; `verify` must fail, not silently succeed.
#[tokio::test]
async fn verify_fails_on_an_http_200_api_error() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.users.getBalances"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(error_body("1011102", "API Key is invalid")),
        )
        .mount(&mock)
        .await;

    let nc = NamecheapRegistrar::with_base_url("apiuser".into(), "bad".into(), mock.uri());
    let err = nc
        .verify(&reqwest::Client::new())
        .await
        .expect_err("an HTTP-200 Status=ERROR body must not read as success");
    assert!(
        err.to_string().contains("1011102"),
        "the error must surface Namecheap's own code: {err}"
    );
}

/// The IP-allowlist self-heal reaches the registrar too: Namecheap echoes the
/// source address it observed, the transport retries once carrying it, and the
/// call succeeds without any external reflector.
#[tokio::test]
async fn an_allowlist_rejection_self_heals_from_the_echoed_address() {
    let mock = MockServer::start().await;
    // First attempt: the seeded placeholder is refused, and the rejection names
    // the address Namecheap actually saw.
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.users.getBalances"))
        .and(query_param("ClientIp", "192.0.2.1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(error_body("1011150", "Invalid request IP: 203.0.113.7")),
        )
        .expect(1)
        .mount(&mock)
        .await;
    // Retry: same command, now claiming the echoed address.
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.users.getBalances"))
        .and(query_param("ClientIp", "203.0.113.7"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ok_body(
            r#"<UserGetBalancesResult Currency="USD" AvailableBalance="42.00"/>"#,
        )))
        .expect(1)
        .mount(&mock)
        .await;

    let nc = NamecheapRegistrar::with_base_url("apiuser".into(), "apikey".into(), mock.uri());
    nc.verify(&reqwest::Client::new())
        .await
        .expect("the retry with the echoed address must succeed");
}

/// When the retry is refused too, the user gets the exact address to allowlist
/// rather than a generic failure.
#[tokio::test]
async fn a_persistent_allowlist_rejection_names_the_address_to_allowlist() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.users.getBalances"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(error_body("1011150", "Invalid request IP: 203.0.113.7")),
        )
        .mount(&mock)
        .await;

    let nc = NamecheapRegistrar::with_base_url("apiuser".into(), "apikey".into(), mock.uri());
    let err = nc
        .verify(&reqwest::Client::new())
        .await
        .expect_err("a still-refused address must be an error")
        .to_string();
    assert!(err.contains("203.0.113.7"), "must name the address: {err}");
    assert!(
        err.contains("API Access"),
        "must say where to add it: {err}"
    );
}

// ---------------------------------------------------------------------------
// check / availability
// ---------------------------------------------------------------------------

/// An available name quotes the account's `YourPrice` plus the ICANN fee — the
/// number Namecheap actually charges, which is what the wizard shows and what
/// the user confirms.
#[tokio::test]
async fn check_quotes_your_price_plus_the_icann_fee() {
    let mock = MockServer::start().await;
    mount_check(
        &mock,
        r#"Domain="example.com" Available="true" ErrorNo="0" IsPremiumName="false" PremiumRegistrationPrice="0" IcannFee="0""#,
    )
    .await;
    mount_pricing(&mock, "9.98", "0.18").await;

    let nc = NamecheapRegistrar::with_base_url("u".into(), "k".into(), mock.uri());
    let avail = nc
        .check(&reqwest::Client::new(), "example.com")
        .await
        .expect("check must succeed");

    assert!(avail.available);
    assert_eq!(avail.price_first_year_cents, Some(998 + 18));
    assert_eq!(avail.currency.as_deref(), Some("USD"));
    assert_eq!(avail.domain, "example.com");
}

/// A taken name carries no price — and costs no pricing round-trip.
#[tokio::test]
async fn check_reports_an_unavailable_domain_without_a_price() {
    let mock = MockServer::start().await;
    mount_check(
        &mock,
        r#"Domain="taken.com" Available="false" ErrorNo="0" IsPremiumName="false""#,
    )
    .await;
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.users.getPricing"))
        .respond_with(ResponseTemplate::new(200).set_body_string(pricing_body("9.98", "0.18")))
        .expect(0)
        .mount(&mock)
        .await;

    let nc = NamecheapRegistrar::with_base_url("u".into(), "k".into(), mock.uri());
    let avail = nc
        .check(&reqwest::Client::new(), "taken.com")
        .await
        .expect("check must succeed");
    assert!(!avail.available);
    assert_eq!(avail.price_first_year_cents, None);
}

/// A premium name's price is quoted inline and the standard TLD table does not
/// apply to it. Quoting the table price here would show the user a number far
/// below what Namecheap bills — a money bug, not a display bug.
#[tokio::test]
async fn check_uses_the_inline_premium_price_not_the_tld_table() {
    let mock = MockServer::start().await;
    mount_check(
        &mock,
        r#"Domain="lux.com" Available="true" ErrorNo="0" IsPremiumName="true" PremiumRegistrationPrice="2500.00" IcannFee="0.18""#,
    )
    .await;
    // Present but must not be consulted for a premium name.
    mount_pricing(&mock, "9.98", "0.18").await;

    let nc = NamecheapRegistrar::with_base_url("u".into(), "k".into(), mock.uri());
    let avail = nc
        .check(&reqwest::Client::new(), "lux.com")
        .await
        .expect("check must succeed");
    assert_eq!(
        avail.price_first_year_cents,
        Some(250_000 + 18),
        "a premium name must quote its inline price, not the standard TLD price"
    );
}

/// `availability` distinguishes "Namecheap doesn't sell this TLD" from
/// "already taken". The signal is structural — Namecheap's own pricing table
/// quotes no 1-year REGISTER row for a TLD it doesn't carry.
#[tokio::test]
async fn availability_reports_tld_not_supported_when_namecheap_quotes_no_price() {
    let mock = MockServer::start().await;
    mount_check(
        &mock,
        r#"Domain="example.zzz" Available="true" ErrorNo="0" IsPremiumName="false""#,
    )
    .await;
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.users.getPricing"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ok_body(
            r#"<UserGetPricingResult><ProductType Name="domains"><ProductCategory Name="register">
            </ProductCategory></ProductType></UserGetPricingResult>"#,
        )))
        .mount(&mock)
        .await;

    let nc = NamecheapRegistrar::with_base_url("u".into(), "k".into(), mock.uri());
    let outcome = nc
        .availability(&reqwest::Client::new(), "example.zzz")
        .await
        .expect("availability must succeed");
    assert_eq!(outcome, RegistrarAvailability::TldNotSupported);
}

#[tokio::test]
async fn availability_reports_buyable_with_the_confirmable_price() {
    let mock = MockServer::start().await;
    mount_check(
        &mock,
        r#"Domain="example.com" Available="true" ErrorNo="0" IsPremiumName="false""#,
    )
    .await;
    mount_pricing(&mock, "9.98", "0.18").await;

    let nc = NamecheapRegistrar::with_base_url("u".into(), "k".into(), mock.uri());
    let outcome = nc
        .availability(&reqwest::Client::new(), "example.com")
        .await
        .expect("availability must succeed");
    assert_eq!(
        outcome,
        RegistrarAvailability::Buyable {
            price_cents: 998 + 18,
            currency: Some("USD".into()),
            renewal_cents: None,
        }
    );
}

#[tokio::test]
async fn availability_reports_unavailable_for_a_taken_name_in_a_carried_tld() {
    let mock = MockServer::start().await;
    mount_check(
        &mock,
        r#"Domain="taken.com" Available="false" ErrorNo="0" IsPremiumName="false""#,
    )
    .await;
    mount_pricing(&mock, "9.98", "0.18").await;

    let nc = NamecheapRegistrar::with_base_url("u".into(), "k".into(), mock.uri());
    let outcome = nc
        .availability(&reqwest::Client::new(), "taken.com")
        .await
        .expect("availability must succeed");
    assert_eq!(outcome, RegistrarAvailability::Unavailable);
}

// ---------------------------------------------------------------------------
// register
// ---------------------------------------------------------------------------

/// The happy path, pinning the three things Namecheap rejects a create over:
/// all four WHOIS roles present, the phone in `+CC.NNNN` form, and WHOIS
/// privacy requested.
#[tokio::test]
async fn register_submits_all_four_contact_roles_and_enables_whois_privacy() {
    let mock = MockServer::start().await;
    let mut m = Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.create"))
        .and(query_param("DomainName", "example.com"))
        .and(query_param("Years", "2"))
        .and(query_param("AddFreeWhoisguard", "yes"))
        .and(query_param("WGEnabled", "yes"));
    for role in ["Registrant", "Tech", "Admin", "AuxBilling"] {
        m = m
            .and(query_param(format!("{role}FirstName"), "Jane"))
            .and(query_param(
                format!("{role}EmailAddress"),
                "jane@example.com",
            ))
            .and(query_param(format!("{role}Country"), "US"))
            // Normalised from the fixture's "+1 555 555 5555" — Namecheap
            // rejects any other shape with an opaque parameter error.
            .and(query_param(format!("{role}Phone"), "+1.5555555555"));
    }
    m.respond_with(ResponseTemplate::new(200).set_body_string(ok_body(
        r#"<DomainCreateResult Domain="example.com" Registered="true" ChargedAmount="10.16" DomainID="1" OrderID="2" TransactionID="3" WhoisguardEnable="true"/>"#,
    )))
    .expect(1)
    .mount(&mock)
    .await;

    let nc = NamecheapRegistrar::with_base_url("u".into(), "k".into(), mock.uri());
    let result = nc
        .register(
            &reqwest::Client::new(),
            "example.com",
            2,
            1016,
            Some(&fixture_contact()),
        )
        .await
        .expect("register must succeed");
    assert_eq!(result.domain, "example.com");
    // Namecheap's create reports no nameservers; the wizard's follow-up
    // `dns_verify(<registrar>, <same creds>)` discovers the zone.
    assert!(result.nameservers.is_empty());
}

/// **The expensive one to get wrong.** A refused registration arrives as HTTP
/// 200 with `Status="ERROR"` — reporting it as success would tell the user they
/// own a domain they do not, and the wizard would march on to DNS setup for a
/// zone that doesn't exist.
#[tokio::test]
async fn register_fails_on_an_http_200_api_error() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.create"))
        .respond_with(ResponseTemplate::new(200).set_body_string(error_body(
            "2528166",
            "Order creation failed due to insufficient funds",
        )))
        .mount(&mock)
        .await;

    let nc = NamecheapRegistrar::with_base_url("u".into(), "k".into(), mock.uri());
    let err = nc
        .register(
            &reqwest::Client::new(),
            "example.com",
            1,
            1016,
            Some(&fixture_contact()),
        )
        .await
        .expect_err("a refused registration must not read as a purchase")
        .to_string();
    assert!(
        err.contains("insufficient funds"),
        "the user must see why it failed: {err}"
    );
}

/// Belt and braces on the same class: an OK envelope whose result element says
/// `Registered="false"` is still a failed purchase.
#[tokio::test]
async fn register_fails_when_the_result_says_it_did_not_register() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.create"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ok_body(
            r#"<DomainCreateResult Domain="example.com" Registered="false"/>"#,
        )))
        .mount(&mock)
        .await;

    let nc = NamecheapRegistrar::with_base_url("u".into(), "k".into(), mock.uri());
    let err = nc
        .register(
            &reqwest::Client::new(),
            "example.com",
            1,
            1016,
            Some(&fixture_contact()),
        )
        .await
        .expect_err("Registered=\"false\" is not a purchase")
        .to_string();
    assert!(err.contains("did not register"), "{err}");
}

/// Namecheap uses per-registration WHOIS contacts, so a missing contact is a
/// caller error caught before any network call.
#[tokio::test]
async fn register_without_a_contact_fails_before_calling_namecheap() {
    // Unroutable base — reaching the network at all would hang/fail loudly.
    let nc = NamecheapRegistrar::with_base_url(
        "u".into(),
        "k".into(),
        "http://127.0.0.1:1/xml.response".into(),
    );
    let err = nc
        .register(&reqwest::Client::new(), "example.com", 1, 1016, None)
        .await
        .expect_err("Namecheap requires a contact");
    assert!(err.to_string().contains("requires_contact"), "{err}");
}

/// A phone number whose country code can't be determined is refused before the
/// call, with the expected format in the message — Namecheap would otherwise
/// reject it opaquely, or worse accept a guessed country code and attach an
/// unreachable WHOIS contact to the registration.
#[tokio::test]
async fn register_refuses_an_unnormalizable_phone_before_calling_namecheap() {
    let nc = NamecheapRegistrar::with_base_url(
        "u".into(),
        "k".into(),
        "http://127.0.0.1:1/xml.response".into(),
    );
    let mut contact = fixture_contact();
    contact.phone = "5555555555".into();
    let err = nc
        .register(
            &reqwest::Client::new(),
            "example.com",
            1,
            1016,
            Some(&contact),
        )
        .await
        .expect_err("an ambiguous phone number must not be guessed at")
        .to_string();
    assert!(err.contains("+1.5551234567"), "must show the format: {err}");
}

// ---------------------------------------------------------------------------
// registry integration
// ---------------------------------------------------------------------------

/// The registry declares Namecheap a registrar, so `dispatch::registrar` must
/// hand back an adapter for it — the flow-trace link between
/// `i18n/providers.yaml` and the impl.
#[test]
fn dispatch_returns_a_namecheap_registrar() {
    let mut map = HashMap::new();
    map.insert("api-user".into(), "u".into());
    map.insert("api-key".into(), "k".into());
    let creds = Credentials::from_map(map);
    assert!(
        dispatch::registrar(ProviderId::Namecheap, creds).is_some(),
        "namecheap declares the registrar capability but dispatch returns None"
    );
}

/// Missing credentials yield `None` rather than a half-built adapter.
#[test]
fn dispatch_returns_none_without_credentials() {
    let mut map = HashMap::new();
    map.insert("api-user".into(), "u".into());
    let creds = Credentials::from_map(map);
    assert!(dispatch::registrar(ProviderId::Namecheap, creds).is_none());
}

/// `requires_contact` reaches the wizard through the dispatcher — this is what
/// makes the contact form render, and `registrar_requires_contact: true` in the
/// registry must agree with it.
#[test]
fn dispatch_reports_that_namecheap_requires_a_contact() {
    use fauna_provisioning::PROVIDERS;
    let mut map = HashMap::new();
    map.insert("api-user".into(), "u".into());
    map.insert("api-key".into(), "k".into());
    let creds = Credentials::from_map(map);
    let d = dispatch::registrar(ProviderId::Namecheap, creds).expect("adapter");
    assert!(d.requires_contact());

    let meta = PROVIDERS
        .iter()
        .find(|p| p.id == ProviderId::Namecheap)
        .expect("namecheap in the registry");
    assert_eq!(
        meta.registrar_requires_contact,
        Some(true),
        "the registry's registrar_requires_contact must match the adapter"
    );
}

/// `getPricing` needs authentication, so the pre-credential TLD price table is
/// genuinely unavailable — `Ok(None)`, never a fabricated table.
#[tokio::test]
async fn list_tld_pricing_is_none_because_namecheap_requires_auth() {
    let nc = NamecheapRegistrar::with_base_url(
        "u".into(),
        "k".into(),
        "http://127.0.0.1:1/xml.response".into(),
    );
    assert!(
        nc.list_tld_pricing(&reqwest::Client::new())
            .await
            .expect("must not error")
            .is_none()
    );
}
