//! DNS config state-machine test suite (design tracked internally).
//! Test plan section #1-#14, all landed — tests 13-14 (AC5) drive the real
//! orchestrator end-to-end via a wiremock Gandi + Hetzner backend.
use fauna_onboarding_machine::nest_api::{ClaimAdminResponse, SetupStatus};
use fauna_onboarding_machine::{
    FakeNestApi, HandleCheckOutcome, OnboardingMachine, OnboardingObserver,
    observer::CountingObserver, state::ProviderStatus,
};
use fauna_provisioning::dns::DnsZone;
use fauna_provisioning::registrar::{ContactInfo, RegistrarAvailability};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Test-fixture stand-in for a resolved "domain is unregistered" probe
/// outcome — `domain_status()`/`provider_status()` derive from the outer
/// variant only, so the inner fields are irrelevant to these tests.
fn unregistered_outcome() -> HandleCheckOutcome {
    HandleCheckOutcome::DomainAvailable {
        buyable_via_provider: false,
        price: None,
    }
}
use std::sync::Arc;

fn machine() -> Arc<OnboardingMachine> {
    let observer: Arc<dyn OnboardingObserver> = CountingObserver::new();
    OnboardingMachine::new(observer)
}

// ── AC5 orchestrator harness (tests 13-14) ──────────────────────────────
//
// Drives `OnboardingMachine::start_provisioning()` through the real
// `run_provisioning_inner` -> `provision_with_registration_snapshot` against
// a single wiremock server standing in for both the DNS/registrar provider
// and the VPS provider (their path namespaces don't collide) plus the final
// nest health poll.

/// A `DnsProvider::verify()` GET is issued twice by the orchestrator's
/// domain step: once pre-flight (must NOT find the zone, so registration
/// proceeds) and once post-registration (must find it, so the zone gets
/// discovered). This responder switches response on the second+ call
/// without depending on wiremock's mock-priority ordering for sequencing.
struct SequencedResponder {
    calls: std::sync::atomic::AtomicUsize,
    first: ResponseTemplate,
    rest: ResponseTemplate,
}

impl SequencedResponder {
    fn new(first: ResponseTemplate, rest: ResponseTemplate) -> Self {
        Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
            first,
            rest,
        }
    }
}

impl wiremock::Respond for SequencedResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n == 0 {
            self.first.clone()
        } else {
            self.rest.clone()
        }
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

/// Mounts a Hetzner VPS backend (find_server_by_name -> none so
/// create_server actually fires, create_server -> instance 42/5.6.7.8,
/// get_ptr -> unset so set_ptr fires) plus the nest health poll. Shared by
/// both AC5 tests — the VPS leg + provider is identical in both; only the
/// DNS/registrar provider differs (see each test's own doc comment).
async fn mount_hetzner_vps_and_health(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"servers": []})))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/servers"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_string(r#"{"server":{"id":42,"public_net":{"ipv4":{"ip":"5.6.7.8"}}}}"#),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/servers/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "server": {"public_net": {"ipv4": {"dns_ptr": null}}}
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/servers/42/actions/change_dns_ptr"))
        .respond_with(ResponseTemplate::new(201))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/health"))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
}

/// A machine whose vps/dns/nest provider base URLs are all overridden to
/// `server`'s URI (the provider APIs' path namespaces don't collide, so one
/// wiremock server backs all three).
/// Fake cloud providers **and** a fake nest that answers the claim.
///
/// A standard-path run does not reach `Succeeded` on the providers alone any
/// more: `Succeeded` means built *and claimed* (`onboarding.md` § 6 *Provisioning
/// = build + claim*), so the run ends with a claim against the box. These tests
/// assert provider-leg behaviour, so the nest half is arranged to succeed and
/// then stays out of the way.
fn machine_with_provider_urls(server: &MockServer) -> Arc<OnboardingMachine> {
    let mut urls = std::collections::HashMap::new();
    urls.insert("vps".to_string(), server.uri());
    urls.insert("dns".to_string(), server.uri());
    urls.insert("nest".to_string(), server.uri());
    let fake = Arc::new(FakeNestApi::new());
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: false,
        ..Default::default()
    }));
    fake.set_claim_admin_response(Ok(ClaimAdminResponse {
        ok: true,
        token: Some("tok".into()),
        expires_at: Some(0),
        domain: Some("example.com".into()),
        deployment_seed: None,
    }));
    let observer: Arc<dyn OnboardingObserver> = CountingObserver::new();
    let m = OnboardingMachine::with_nest_api_and_provider_base_urls(observer, fake, Some(urls));
    // The claim signs with the admin's own identity, which the real flow has by
    // this page (it is chosen on `identity_choice`, long before provisioning).
    m.seed_identity("01".repeat(32));
    m
}

/// Selects Hetzner as the VPS provider and seeds the creds/server-type/
/// location state `start_provisioning`'s VPS leg requires. Must run AFTER
/// `select_vps_provider` (which clears creds/server_types/locations).
fn seed_hetzner_vps(m: &OnboardingMachine) {
    m.select_vps_provider("hetzner".into());
    m.set_vps_cred("api-token".into(), "test-token".into());
    m.set_vps_state_for_test(|v| {
        v.server_types = vec![fauna_provisioning::vps::ServerTypeInfo {
            id: "cx22".into(),
            vcpu: 2,
            mem_gb: 4.0,
            disk_gb: 40,
            price_monthly_cents: 500,
            currency: "EUR".into(),
        }];
        v.selected_server_type_id = Some("cx22".into());
        v.locations = vec![fauna_provisioning::vps::VpsLocation {
            id: "fsn1".into(),
            name: "Falkenstein".into(),
            city: "Falkenstein".into(),
            country: "DE".into(),
        }];
        v.selected_location_id = Some("fsn1".into());
        v.enable_mail = Some(false);
    });
}

/// Polls `provisioning_snapshot()` until the run leaves Idle/Running
/// (Succeeded, Failed, or Cancelled), or gives up after 5s.
async fn wait_for_provisioning_to_finish(m: &Arc<OnboardingMachine>) {
    use fauna_provisioning::progress::OverallStatus;
    for _ in 0..250 {
        let snap = m.provisioning_snapshot();
        if !matches!(snap.overall, OverallStatus::Idle | OverallStatus::Running) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Test 1: a fresh machine with provider selected but verify not yet run
/// returns NotReady.
#[test]
fn provider_status_not_ready_before_verify() {
    let m = machine();
    m.set_current_handle("alice@example.com".into());
    m.select_dns_provider("gandi".into());
    assert_eq!(m.provider_status(), ProviderStatus::NotReady);
    assert!(!m.can_continue_dns(), "NotReady must block continue");
}

/// Test 2: zones contain the handle's domain → ProviderHasDomain.
#[test]
fn provider_status_provider_has_domain() {
    let m = machine();
    m.set_current_handle("alice@example.com".into());
    m.select_dns_provider("gandi".into());
    m.set_dns_state_for_test(|d| {
        d.verified = true;
        d.current_zones = vec![DnsZone {
            id: "zone-1".into(),
            name: "example.com".into(),
        }];
    });
    assert_eq!(m.provider_status(), ProviderStatus::ProviderHasDomain);
    assert!(m.can_continue_dns());
}

/// Test 3: probe says registered, zone not present → RegisteredElsewhere.
#[test]
fn provider_status_registered_elsewhere() {
    let m = machine();
    m.set_current_handle("alice@example.com".into());
    m.select_dns_provider("gandi".into());
    m.set_dns_state_for_test(|d| {
        d.verified = true;
        d.current_zones = vec![DnsZone {
            id: "zone-other".into(),
            name: "other.com".into(),
        }];
    });
    m.set_handle_check_outcome_for_test(HandleCheckOutcome::RegisteredNoNest);
    assert_eq!(m.provider_status(), ProviderStatus::RegisteredElsewhere);
    assert!(!m.can_continue_dns());
}

/// Test 7: gandi availability said TldNotSupported → UnregisteredNotBuyable.
#[test]
fn provider_status_unregistered_not_buyable_tld_not_supported() {
    let m = machine();
    m.set_current_handle("alice@example.foo".into());
    m.select_dns_provider("gandi".into());
    m.set_dns_state_for_test(|d| {
        d.verified = true;
        d.current_zones = vec![];
        d.current_availability = Some(RegistrarAvailability::TldNotSupported);
    });
    m.set_handle_check_outcome_for_test(unregistered_outcome());
    assert_eq!(m.provider_status(), ProviderStatus::UnregisteredNotBuyable);
    assert!(!m.can_continue_dns());
}

/// Test 8: provider with no Registrar cap (Cloudflare) →
/// UnregisteredNotBuyable. current_availability is None because verify
/// skipped the registrar call entirely.
#[test]
fn provider_status_unregistered_not_buyable_no_registrar_cap() {
    let m = machine();
    m.set_current_handle("alice@example.com".into());
    m.select_dns_provider("cloudflare".into());
    m.set_dns_state_for_test(|d| {
        d.verified = true;
        d.current_zones = vec![];
        d.current_availability = None;
    });
    m.set_handle_check_outcome_for_test(unregistered_outcome());
    assert_eq!(m.provider_status(), ProviderStatus::UnregisteredNotBuyable);
    assert!(!m.can_continue_dns());
}

/// Test 4: gandi availability is Buyable + price_agreed + contact set →
/// can_continue_dns true.
#[test]
fn provider_status_unregistered_buyable_gandi() {
    let m = machine();
    m.set_current_handle("alice@example.com".into());
    m.select_dns_provider("gandi".into());
    // Gandi's required cred field is `personal-access-token`; without it,
    // `dispatch::registrar` yields None and the buy-path
    // `requires_contact()` check defaults to true (blocking continue
    // unless a contact is set). Populating the cred lets the dispatch
    // resolve to a real registrar so `requires_contact() == true`
    // is sourced from Gandi's impl, not the unwrap_or default.
    m.set_dns_creds(
        [("personal-access-token".into(), "stub".into())]
            .into_iter()
            .collect(),
    );
    m.set_dns_state_for_test(|d| {
        d.verified = true;
        d.buy_domain = true;
        d.current_zones = vec![];
        d.current_availability = Some(RegistrarAvailability::Buyable {
            price_cents: 1500,
            currency: Some("EUR".into()),
            renewal_cents: None,
        });
        d.contact = Some(ContactInfo {
            first_name: "Alice".into(),
            last_name: "Liddell".into(),
            email: "a@l.com".into(),
            phone: "+1".into(),
            address1: "1 wonderland".into(),
            city: "Oxford".into(),
            state: "Oxfordshire".into(),
            postal_code: "OX1".into(),
            country: "GB".into(),
        });
    });
    m.set_handle_check_outcome_for_test(unregistered_outcome());
    m.confirm_price();

    assert_eq!(
        m.provider_status(),
        ProviderStatus::UnregisteredBuyable {
            price_cents: 1500,
            currency: Some("EUR".into()),
        }
    );
    assert!(m.can_continue_dns());
}

/// Test 5: same as #4 but no confirm_price → can_continue blocked.
#[test]
fn provider_status_unregistered_buyable_blocks_without_price_agreed() {
    let m = machine();
    m.set_current_handle("alice@example.com".into());
    m.select_dns_provider("gandi".into());
    m.set_dns_creds(
        [("personal-access-token".into(), "stub".into())]
            .into_iter()
            .collect(),
    );
    m.set_dns_state_for_test(|d| {
        d.verified = true;
        d.buy_domain = true;
        d.current_availability = Some(RegistrarAvailability::Buyable {
            price_cents: 1500,
            currency: None,
            renewal_cents: None,
        });
        d.contact = Some(ContactInfo::default());
    });
    m.set_handle_check_outcome_for_test(unregistered_outcome());
    // confirm_price NOT called.
    assert!(!m.can_continue_dns(), "must block without price_agreed");
}

/// Test 6: same as #4 but no contact set → can_continue blocked
/// (Gandi.requires_contact() == true).
#[test]
fn provider_status_unregistered_buyable_blocks_without_contact() {
    let m = machine();
    m.set_current_handle("alice@example.com".into());
    m.select_dns_provider("gandi".into());
    m.set_dns_creds(
        [("personal-access-token".into(), "stub".into())]
            .into_iter()
            .collect(),
    );
    m.set_dns_state_for_test(|d| {
        d.verified = true;
        d.buy_domain = true;
        d.current_availability = Some(RegistrarAvailability::Buyable {
            price_cents: 1500,
            currency: None,
            renewal_cents: None,
        });
        d.contact = None;
    });
    m.set_handle_check_outcome_for_test(unregistered_outcome());
    m.confirm_price();
    assert!(!m.can_continue_dns(), "must block when contact missing");
}

/// Test 9: set_contact unblocks Continue.
#[test]
fn set_contact_persists_and_unblocks_continue() {
    let m = machine();
    m.set_current_handle("alice@example.com".into());
    m.select_dns_provider("gandi".into());
    m.set_dns_creds(
        [("personal-access-token".into(), "stub".into())]
            .into_iter()
            .collect(),
    );
    m.set_dns_state_for_test(|d| {
        d.verified = true;
        d.buy_domain = true;
        d.current_availability = Some(RegistrarAvailability::Buyable {
            price_cents: 1500,
            currency: None,
            renewal_cents: None,
        });
    });
    m.set_handle_check_outcome_for_test(unregistered_outcome());
    m.confirm_price();
    assert!(!m.can_continue_dns(), "blocked without contact");

    m.set_contact(ContactInfo::default());
    assert!(m.can_continue_dns(), "unblocked after set_contact");
}

/// Test 10: select_dns_provider clears caches.
#[test]
fn select_dns_provider_clears_caches() {
    let m = machine();
    m.set_current_handle("alice@example.com".into());
    m.select_dns_provider("gandi".into());
    m.set_dns_state_for_test(|d| {
        d.verified = true;
        d.current_zones = vec![DnsZone {
            id: "z".into(),
            name: "example.com".into(),
        }];
        d.current_availability = Some(RegistrarAvailability::Unavailable);
        d.contact = Some(ContactInfo::default());
    });

    m.select_dns_provider("porkbun".into());

    let dns = m.dns_config();
    assert!(dns.current_zones.is_empty());
    assert!(dns.current_availability.is_none());
    assert!(dns.contact.is_none());
}

/// Test 11: verify_dns calls fetch_default_contact and populates
/// dns.contact with the response.
#[tokio::test]
async fn verify_dns_prefills_contact_from_gandi() {
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    // Gandi DNS verify endpoint (used by `Gandi::verify` in `dns/gandi.rs`).
    Mock::given(method("GET"))
        .and(path("/domain/domains"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    // Gandi registrar `availability` calls /domain/check; return Buyable.
    Mock::given(method("GET"))
        .and(path("/domain/check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "products": [{
                "status": "available",
                "currency": "EUR",
                "prices": [{
                    "duration": 1,
                    "duration_unit": "y",
                    "type": "create",
                    "price_after_taxes": 15.0,
                }]
            }]
        })))
        .mount(&server)
        .await;
    // Gandi user-info + organizations for fetch_default_contact.
    Mock::given(method("GET"))
        .and(path("/organization/user-info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "44444444-4444-4444-4444-444444444444"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/organization/organizations/.*$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "given": "Gerry",
            "family": "Mander",
            "email": "g@m.com",
            "country": "US"
        })))
        .mount(&server)
        .await;

    let m = machine();
    m.set_current_handle("alice@example.com".into());
    m.select_dns_provider("gandi".into());
    m.set_dns_creds(
        [
            ("personal-access-token".into(), "stub".into()),
            ("api-base".into(), server.uri()),
        ]
        .into_iter()
        .collect(),
    );
    m.set_handle_check_outcome_for_test(unregistered_outcome());
    m.verify_dns().await.expect("verify must succeed");

    let dns = m.dns_config();
    let c = dns.contact.expect("contact must be prefilled");
    assert_eq!(c.first_name, "Gerry");
    assert_eq!(c.last_name, "Mander");
    assert_eq!(c.email, "g@m.com");
    assert_eq!(c.country, "US");
    // Availability also written from the registrar enrichment path.
    assert!(matches!(
        dns.current_availability,
        Some(RegistrarAvailability::Buyable { .. })
    ));
}

/// Test 12: when fetch_default_contact's organizations call fails,
/// verify_dns still succeeds and dns.contact stays None.
#[tokio::test]
async fn verify_dns_prefill_failure_is_not_fatal() {
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/domain/domains"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    // availability still succeeds — only the prefill leg fails. This
    // isolates the "prefill failure is non-fatal" claim from any
    // availability-side noise.
    Mock::given(method("GET"))
        .and(path("/domain/check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "products": [{ "status": "unknown" }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/organization/user-info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "55555555-5555-5555-5555-555555555555"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/organization/organizations/.*$"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let m = machine();
    m.set_current_handle("alice@example.com".into());
    m.select_dns_provider("gandi".into());
    m.set_dns_creds(
        [
            ("personal-access-token".into(), "stub".into()),
            ("api-base".into(), server.uri()),
        ]
        .into_iter()
        .collect(),
    );
    m.set_handle_check_outcome_for_test(unregistered_outcome());
    m.verify_dns()
        .await
        .expect("verify must succeed despite prefill failure");

    let dns = m.dns_config();
    assert!(
        dns.contact.is_none(),
        "contact stays None on prefill failure"
    );
}

/// Test 13: start_provisioning passes the contact to provision_with_registration.
///
/// Drives the real orchestrator end-to-end against wiremock Gandi (DNS +
/// registrar) and Hetzner (VPS). Gandi is the registrar under test here
/// because its `register()` is the one adapter that actually places the
/// WHOIS contact on the wire — Porkbun's `register()` ignores the `contact`
/// arg entirely (it uses the account-level contact instead), which is why
/// test 14 below uses Porkbun for the *price* assertion in reverse: Gandi's
/// `register()` drops `agreed_price_cents` on the floor (Gandi's own API has
/// no price field), so only Porkbun's `cost` field can prove AC5's price
/// half. Each provider proves the half of AC5 its wire format can observe.
///
/// Spec: DNS config state-machine design, acceptance criterion 5
/// (tracked internally).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_provisioning_passes_contact_to_orchestrator() {
    let server = MockServer::start().await;
    mount_hetzner_vps_and_health(&server).await;

    // DnsProvider::verify() (GET /domain/domains): empty on the pre-flight
    // call, then reports the domain once registered.
    Mock::given(method("GET"))
        .and(path("/domain/domains"))
        .respond_with(SequencedResponder::new(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([])),
            ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": "example.com", "fqdn": "example.com"}
            ])),
        ))
        .mount(&server)
        .await;

    // The assertion under test: Registrar::register()'s POST body carries
    // the contact's fields through from `snapshot.dns.contact`.
    Mock::given(method("POST"))
        .and(path("/domain/domains"))
        .and(body_partial_json(serde_json::json!({
            "owner": {
                "given": "Jane",
                "family": "Doe",
                "email": "jane@example.com",
            },
        })))
        .respond_with(
            ResponseTemplate::new(202).set_body_json(serde_json::json!({"message": "Created"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    // Step-3 DNS record creation isn't the assertion under test — a loose
    // catch-all lets the run reach completion.
    Mock::given(method("POST"))
        .and(path("/domain/domains/example.com/records"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;

    let m = machine_with_provider_urls(&server);
    m.set_current_handle("alice@example.com".into());
    m.select_dns_provider("gandi".into());
    m.set_dns_creds(
        [
            ("personal-access-token".into(), "test-token".into()),
            ("api-base".into(), server.uri()),
        ]
        .into_iter()
        .collect(),
    );
    m.set_dns_state_for_test(|d| {
        d.buy_domain = true;
        d.price_agreed = true;
        d.contact = Some(fixture_contact());
        d.current_availability = Some(RegistrarAvailability::Buyable {
            price_cents: 1234,
            currency: Some("EUR".into()),
            renewal_cents: None,
        });
    });
    seed_hetzner_vps(&m);

    m.clone().start_provisioning();
    wait_for_provisioning_to_finish(&m).await;

    let snap = m.provisioning_snapshot();
    assert_eq!(
        snap.overall,
        fauna_provisioning::progress::OverallStatus::Succeeded,
        "provisioning did not succeed: {snap:#?}"
    );
    // The register mock's `.expect(1)` + `body_partial_json` match (verified
    // at `server`'s drop) IS the AC5 proof: the POST body contained the
    // contact fields, so `snapshot.dns.contact` reached the orchestrator.
}

/// Test 14: start_provisioning reads price from current_availability,
/// not from a price_quote.
///
/// Same setup as #13 but with Porkbun as the DNS+registrar provider (its
/// `register()` places `agreed_price_cents` on the wire as `cost` — Gandi's
/// doesn't take a price field at all, see test 13's doc comment) and no
/// contact (Porkbun's `requires_contact()` is `false`; it ignores the arg).
/// Asserts the register call's `cost` is `1234` — the value from
/// `current_availability`'s `Buyable { price_cents: 1234, .. }` — proving
/// `run_provisioning_inner` reads price from that field and not some other
/// (stale/removed) `price_quote` source.
///
/// Spec: DNS config state-machine design, acceptance criterion 5
/// (tracked internally).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_provisioning_uses_availability_price_not_quote() {
    let server = MockServer::start().await;
    mount_hetzner_vps_and_health(&server).await;

    // DnsProvider::verify() (POST /domain/listAll): no domains on the
    // pre-flight call, then reports the domain once registered.
    Mock::given(method("POST"))
        .and(path("/domain/listAll"))
        .respond_with(SequencedResponder::new(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"status": "SUCCESS", "domains": []})),
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "SUCCESS",
                "domains": [{"domain": "example.com"}],
            })),
        ))
        .mount(&server)
        .await;

    // The assertion under test: Registrar::register()'s `cost` field carries
    // `agreed_price_cents` through from `snapshot.dns.current_availability`.
    Mock::given(method("POST"))
        .and(path("/domain/create/example.com"))
        .and(body_partial_json(serde_json::json!({"cost": 1234})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "SUCCESS"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    // Step-3 DNS record creation isn't the assertion under test — a loose
    // catch-all lets the run reach completion.
    Mock::given(method("POST"))
        .and(path("/dns/create/example.com"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "SUCCESS"})),
        )
        .mount(&server)
        .await;

    let m = machine_with_provider_urls(&server);
    m.set_current_handle("alice@example.com".into());
    m.select_dns_provider("porkbun".into());
    m.set_dns_creds(
        [
            ("api-key".into(), "test-key".into()),
            ("secret-api-key".into(), "test-secret".into()),
            ("api-base".into(), server.uri()),
        ]
        .into_iter()
        .collect(),
    );
    m.set_dns_state_for_test(|d| {
        d.buy_domain = true;
        d.price_agreed = true;
        d.current_availability = Some(RegistrarAvailability::Buyable {
            price_cents: 1234,
            currency: Some("EUR".into()),
            renewal_cents: None,
        });
    });
    seed_hetzner_vps(&m);

    m.clone().start_provisioning();
    wait_for_provisioning_to_finish(&m).await;

    let snap = m.provisioning_snapshot();
    assert_eq!(
        snap.overall,
        fauna_provisioning::progress::OverallStatus::Succeeded,
        "provisioning did not succeed: {snap:#?}"
    );
    // The register mock's `.expect(1)` + `body_partial_json({"cost":1234})`
    // match (verified at `server`'s drop) IS the AC5 proof.
}

// ── captured_dns_credential() — the onboarding→launch hand-off channel ──────
//
// Spec: docs/goal/behavior/dns-management.md § Where the credential lives +
// docs/goal/behavior/onboarding.md § 4 DNS configuration. The credential the
// admin verified at the DNS step is exposed (not sealed here) so the launched
// client can seal it into `fauna.state.dns` via the post-onboarding
// `DnsManagementMachine::PutCredentials` path — one store, one writer.

/// A verified provider credential is surfaced in the shape `PutCredentials`
/// consumes (provider id + the entered field bag + a default label).
#[test]
fn captured_dns_credential_some_when_verified() {
    let m = machine();
    m.set_current_handle("admin@example.com".into());
    m.select_dns_provider("hetzner".into());
    m.set_dns_creds(
        [("api-token".into(), "secret-token".into())]
            .into_iter()
            .collect(),
    );
    m.set_dns_state_for_test(|d| {
        d.verified = true;
        d.current_zones = vec![DnsZone {
            id: "z1".into(),
            name: "example.com".into(),
        }];
    });

    let cred = m
        .captured_dns_credential()
        .expect("verified credential must be surfaced for the launch-time seal");
    assert_eq!(cred.provider_id, "hetzner");
    assert_eq!(
        cred.fields.get("api-token").map(|v| v.as_str()),
        Some("secret-token"),
        "the entered secret field bag rides verbatim to PutCredentials"
    );
    assert_eq!(
        cred.label, "hetzner (example.com)",
        "default label disambiguates by provider + onboarded domain"
    );
}

/// Regression guard for the `SecretString` lift: the captured credential's
/// secret values must serialize to JSON as **plain strings** (`SecretString` is
/// serde-transparent), so the onboarding→launch hand-off (`captured_dns_credential_json`
/// in `fauna-wasm-onboarding`, and the uniffi::Record marshalling) is byte-identical
/// to the pre-lift plain-`String` shape — the lift is wire-preserving.
#[test]
fn captured_dns_credential_secret_serializes_transparently() {
    let m = machine();
    m.set_current_handle("admin@example.com".into());
    m.select_dns_provider("hetzner".into());
    m.set_dns_creds(
        [("api-token".into(), "secret-token".into())]
            .into_iter()
            .collect(),
    );
    m.set_dns_state_for_test(|d| {
        d.verified = true;
        d.current_zones = vec![DnsZone {
            id: "z1".into(),
            name: "example.com".into(),
        }];
    });

    let cred = m.captured_dns_credential().expect("verified credential");
    let json = serde_json::to_value(&cred).expect("serialize CapturedDnsCredential");
    assert_eq!(
        json["fields"]["api-token"],
        serde_json::Value::String("secret-token".to_string()),
        "SecretString must serialize as a plain JSON string, not a wrapped object"
    );
}

/// "Set up later" is the manual-DNS path — nothing to seal.
#[test]
fn captured_dns_credential_none_when_set_up_later() {
    let m = machine();
    m.set_current_handle("admin@example.com".into());
    m.select_dns_provider("hetzner".into());
    m.set_dns_creds(
        [("api-token".into(), "secret-token".into())]
            .into_iter()
            .collect(),
    );
    m.set_dns_state_for_test(|d| d.verified = true);
    m.dns_set_up_later();

    assert!(
        m.captured_dns_credential().is_none(),
        "the manual-DNS path captures no credential to seal"
    );
}

/// A credential that never passed `verify_dns()` must not be sealed.
#[test]
fn captured_dns_credential_none_when_unverified() {
    let m = machine();
    m.set_current_handle("admin@example.com".into());
    m.select_dns_provider("hetzner".into());
    m.set_dns_creds(
        [("api-token".into(), "secret-token".into())]
            .into_iter()
            .collect(),
    );
    // verified left false.

    assert!(
        m.captured_dns_credential().is_none(),
        "an unverified credential is not surfaced for sealing"
    );
}

/// No provider selected → nothing captured.
#[test]
fn captured_dns_credential_none_when_no_provider() {
    let m = machine();
    m.set_current_handle("admin@example.com".into());

    assert!(
        m.captured_dns_credential().is_none(),
        "no provider selected means no credential"
    );
}

/// The cross-app E2E bridge route (`call_machine_method`) must surface the
/// captured credential too: iOS / macOS / Windows inject the onboarding-captured
/// credential through this JSON dispatch (web has its own wasm
/// `setCapturedDnsCredentialForTest` binding), so the native onboarding-launch-glue
/// e2e (`test_onboarding_dns_glue.py --client windows`) can drive the seal without
/// the real `proxy.fauna.social` verify. Without the
/// `set_captured_dns_credential_for_test` arm the call is a silent no-op (unknown
/// names fall through) and the native glue can never be exercised. Mirrors the wasm
/// `set_captured_dns_credential_for_test` in `fauna-wasm-onboarding`.
#[test]
fn call_machine_method_dispatches_set_captured_dns_credential() {
    let m = machine();
    m.set_current_handle("admin@example.com".into());
    m.call_machine_method(
        "set_captured_dns_credential_for_test".into(),
        r#"{"provider_id":"cloudflare","fields":{"api-token":"fake-dns-ok:zone.test"}}"#.into(),
    );

    let cred = m
        .captured_dns_credential()
        .expect("the bridge route must surface the injected credential at LoggedIn");
    assert_eq!(cred.provider_id, "cloudflare");
    assert_eq!(
        cred.fields.get("api-token").map(|v| v.as_str()),
        Some("fake-dns-ok:zone.test"),
        "the injected secret field bag rides verbatim to captured_dns_credential"
    );
}
