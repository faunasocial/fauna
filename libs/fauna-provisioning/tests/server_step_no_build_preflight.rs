//! The Server step buys nothing for a tag with no build
//! (`docs/goal/behavior/onboarding-provisioning.md` § 5 → *What exists decides
//! the default, and nothing is bought for a channel with no build*; § 6
//! *Provisioning model*): after its name pre-flight and before `create_server`
//! it resolves the box's image tag, and a registry `404` fails the run before
//! any resource exists, while any other registry outcome proceeds. A run that
//! buys its domain asks the same question in the Domain step, before the
//! registrar's `register`, so the domain is not bought either.
//!
//! Driven through `provision_nest_no_dns` (its only network traffic is the
//! Server step) and `provision_with_registration_snapshot` (with a counting
//! registrar) against the Hetzner provider and a wiremock registry.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use fauna_provisioning::cloud_init::CloudInitParams;
use fauna_provisioning::dns::{DnsProvider, DnsRecord, DnsZone};
use fauna_provisioning::error::ProvisionError;
use fauna_provisioning::orchestrator::{
    provision_nest_no_dns, provision_with_registration_snapshot,
};
use fauna_provisioning::progress::{CancelFlag, OverallStatus, ProvisioningSnapshot};
use fauna_provisioning::registrar::{
    ContactInfo, DomainAvailability, Registrar, RegistrationResult,
};
use fauna_provisioning::update_channel::NEST_IMAGE_REPOSITORY;
use fauna_provisioning::update_channel::registry::Registry;
use fauna_provisioning::vps::hetzner::Hetzner;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A Hetzner fake with no server of the name yet; `create_server` is expected
/// `creates` times.
async fn hetzner(creates: u64) -> MockServer {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"servers":[]}"#))
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/servers"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_string(r#"{"server":{"id":1,"public_net":{"ipv4":{"ip":"1.2.3.4"}}}}"#),
        )
        .expect(creates)
        .mount(&mock)
        .await;
    mock
}

/// A registry answering `status` for the `latest` manifest's `HEAD`.
async fn registry_answering(status: u16) -> MockServer {
    let reg = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&reg)
        .await;
    let head = ResponseTemplate::new(status);
    let head = if status == 200 {
        // A digest that does not resolve further is an error past the `HEAD` —
        // not a `404`, so it proceeds like any other non-404 outcome.
        head.insert_header(
            "docker-content-digest",
            format!("sha256:{}", "a".repeat(64)).as_str(),
        )
    } else {
        head
    };
    Mock::given(method("HEAD"))
        .and(path("/v2/faunasocial/nest/manifests/latest"))
        .respond_with(head)
        .mount(&reg)
        .await;
    reg
}

async fn run(
    vps: &MockServer,
    registry: Option<&Registry>,
) -> (Result<(), String>, ProvisioningSnapshot) {
    let hetzner = Hetzner::with_base_url("test-token".into(), vps.uri());
    let params = CloudInitParams {
        domain: "x.com".into(),
        image_tag: "latest".into(),
        claim_code: "claim".into(),
        enable_mail: false,
        deployment_seed: None,
        ssh_door: None,
    };
    let state = Mutex::new(ProvisioningSnapshot::idle());
    let result = provision_nest_no_dns(
        &reqwest::Client::new(),
        registry,
        &hetzner,
        "x.com",
        "box-1",
        "fsn1",
        "cx23",
        &params,
        &[],
        &state,
        || {},
        |_ip, _origin| {},
        &CancelFlag::new(),
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string());
    (result, state.into_inner().unwrap())
}

#[tokio::test]
async fn a_tag_with_no_build_fails_the_server_step_before_create_server() {
    let vps = hetzner(0).await;
    let reg = registry_answering(404).await;
    let registry = Registry::new(reqwest::Client::new(), &reg.uri(), NEST_IMAGE_REPOSITORY);
    let (result, snap) = run(&vps, Some(&registry)).await;
    let err = result.expect_err("a 404 tag must fail the run");
    assert!(
        err.contains("no build is published on the Stable channel yet")
            && err.contains("ghcr.io/faunasocial/nest:latest")
            && err.contains("choose another channel"),
        "{err}"
    );
    assert_eq!(snap.overall, OverallStatus::Failed);
    // Retry re-runs the pre-flight: still nothing bought.
    let (again, _) = run(&vps, Some(&registry)).await;
    assert!(again.is_err());
}

#[tokio::test]
async fn a_tag_the_registry_answers_proceeds_to_create_server() {
    let vps = hetzner(1).await;
    let reg = registry_answering(200).await;
    let registry = Registry::new(reqwest::Client::new(), &reg.uri(), NEST_IMAGE_REPOSITORY);
    let (result, _) = run(&vps, Some(&registry)).await;
    result.expect("a tag the registry answers for proceeds");
}

#[tokio::test]
async fn an_unreachable_registry_proceeds_to_create_server() {
    let vps = hetzner(1).await;
    let registry = Registry::new(
        reqwest::Client::new(),
        "http://127.0.0.1:9",
        NEST_IMAGE_REPOSITORY,
    );
    let (result, _) = run(&vps, Some(&registry)).await;
    result.expect("an unknown registry outcome proceeds — the box retries its own pull");
}

#[tokio::test]
async fn no_registry_skips_the_pre_flight() {
    let vps = hetzner(1).await;
    let (result, _) = run(&vps, None).await;
    result.expect("no registry, no pre-flight");
}

// --- A run that buys its domain: the refusal comes before `register` --------

/// A DNS provider with no zone for the domain, so the Domain step reaches the
/// registrar; nothing past `verify` is expected.
struct NoZones;
impl DnsProvider for NoZones {
    async fn verify(&self, _client: &reqwest::Client) -> Result<Vec<DnsZone>, ProvisionError> {
        Ok(vec![])
    }
    async fn create_record(
        &self,
        _client: &reqwest::Client,
        _zone_id: &str,
        _record: &DnsRecord,
    ) -> Result<(), ProvisionError> {
        unreachable!("no record is published on these runs")
    }
    async fn find_records(
        &self,
        _client: &reqwest::Client,
        _zone_id: &str,
        _name: &str,
        _record_type: &str,
    ) -> Result<Vec<DnsRecord>, ProvisionError> {
        unreachable!("no record is published on these runs")
    }
    async fn delete_record(
        &self,
        _client: &reqwest::Client,
        _zone_id: &str,
        _name: &str,
        _record_type: &str,
        _value: &str,
    ) -> Result<(), ProvisionError> {
        unreachable!("no record is published on these runs")
    }
    fn base_url(&self) -> &str {
        ""
    }
}

/// A registrar that counts the registrations it is asked to buy.
#[derive(Default)]
struct CountingRegistrar {
    registered: AtomicUsize,
}
impl Registrar for CountingRegistrar {
    async fn verify(&self, _client: &reqwest::Client) -> Result<(), ProvisionError> {
        Ok(())
    }
    async fn check(
        &self,
        _client: &reqwest::Client,
        domain: &str,
    ) -> Result<DomainAvailability, ProvisionError> {
        Ok(DomainAvailability {
            domain: domain.into(),
            available: true,
            price_first_year_cents: Some(1000),
            price_renewal_cents: Some(1000),
            currency: Some("USD".into()),
        })
    }
    async fn register(
        &self,
        _client: &reqwest::Client,
        domain: &str,
        _years: u32,
        _agreed_price_cents: u64,
        _contact: Option<&ContactInfo>,
    ) -> Result<RegistrationResult, ProvisionError> {
        self.registered.fetch_add(1, Ordering::SeqCst);
        Ok(RegistrationResult {
            domain: domain.into(),
            nameservers: vec![],
        })
    }
}

async fn run_buying_the_domain(
    vps: &MockServer,
    registry: Option<&Registry>,
    registrar: &CountingRegistrar,
) -> Result<(), String> {
    let hetzner = Hetzner::with_base_url("test-token".into(), vps.uri());
    let params = CloudInitParams {
        domain: "x.com".into(),
        image_tag: "latest".into(),
        claim_code: "claim".into(),
        enable_mail: false,
        deployment_seed: None,
        ssh_door: None,
    };
    let state = Mutex::new(ProvisioningSnapshot::idle());
    provision_with_registration_snapshot(
        &reqwest::Client::new(),
        |_ip: &str| reqwest::Client::new(),
        registry,
        &hetzner,
        &NoZones,
        registrar,
        "x.com",
        "https://x.com",
        1,
        1000,
        None,
        "box-1",
        "fsn1",
        "cx23",
        &params,
        &[],
        &state,
        || {},
        |_ip, _origin| {},
        &CancelFlag::new(),
        |_secs: u64| async move {},
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[tokio::test]
async fn a_tag_with_no_build_fails_the_run_before_the_domain_is_registered() {
    let vps = hetzner(0).await;
    let reg = registry_answering(404).await;
    let registry = Registry::new(reqwest::Client::new(), &reg.uri(), NEST_IMAGE_REPOSITORY);
    let registrar = CountingRegistrar::default();
    let err = run_buying_the_domain(&vps, Some(&registry), &registrar)
        .await
        .expect_err("a 404 tag must fail the run");
    assert!(
        err.contains("no build is published on the Stable channel yet"),
        "{err}"
    );
    assert_eq!(
        registrar.registered.load(Ordering::SeqCst),
        0,
        "nothing is bought"
    );
    // Retry re-runs the pre-flight: still nothing bought.
    run_buying_the_domain(&vps, Some(&registry), &registrar)
        .await
        .expect_err("Retry refuses again");
    assert_eq!(
        registrar.registered.load(Ordering::SeqCst),
        0,
        "nothing is bought"
    );
}

#[tokio::test]
async fn an_unknown_registry_outcome_still_registers_the_domain() {
    let vps = hetzner(0).await;
    let registry = Registry::new(
        reqwest::Client::new(),
        "http://127.0.0.1:9",
        NEST_IMAGE_REPOSITORY,
    );
    let registrar = CountingRegistrar::default();
    // The fake DNS provider never shows the bought zone, so the Domain step
    // fails just after `register` — the point is that it got there. That
    // failure stops the run: `hetzner(0)` checks no server is bought after it.
    let err = run_buying_the_domain(&vps, Some(&registry), &registrar)
        .await
        .expect_err("a Domain step that finds no zone fails the run");
    assert!(
        err.contains("not found via DNS provider post-registration"),
        "{err}"
    );
    assert_eq!(registrar.registered.load(Ordering::SeqCst), 1);
}
