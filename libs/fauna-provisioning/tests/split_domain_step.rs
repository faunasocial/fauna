//! The Domain step when DNS runs at another provider
//! (`docs/goal/behavior/onboarding-provisioning.md` § 6): register at the
//! registrar, create the zone at the DNS provider (skipped when a zone of
//! exactly that name exists), set the domain's nameservers to it (never
//! skipped — a replace), then hand the zone to the Dns step. Each of the three
//! calls fails the step before any server is bought, and a Retry resumes the
//! run from the top, its pre-flights skipping what exists and a remembered
//! registration skipping the purchase. Plus the registration-only deferred run
//! (the manual DNS cover while buying): register, build, publish nothing.
//!
//! Driven through `provision_with_purchase_snapshot` and
//! `provision_registered_no_dns` with in-memory DNS and registrar fakes and a
//! wiremock Hetzner server API.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use fauna_provisioning::cloud_init::CloudInitParams;
use fauna_provisioning::dns::{DnsProvider, DnsRecord, DnsZone};
use fauna_provisioning::error::ProvisionError;
use fauna_provisioning::orchestrator::{
    DomainEvent, DomainPurchase, provision_registered_no_dns, provision_with_purchase_snapshot,
};
use fauna_provisioning::progress::{
    CancelFlag, OverallStatus, ProvisionStep, ProvisioningSnapshot, StepStatus,
};
use fauna_provisioning::registrar::{
    ContactInfo, DomainAvailability, Registrar, RegistrationResult,
};
use fauna_provisioning::vps::hetzner::Hetzner;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DOMAIN: &str = "fresh.example";
const NS: [&str; 2] = ["hydrogen.ns.fake", "oxygen.ns.fake"];

fn refused(what: &str) -> ProvisionError {
    ProvisionError::Provider {
        status: 403,
        body: format!("{what} refused"),
    }
}

/// A DNS provider with the zone door. `fail_create` refuses the next create.
#[derive(Default)]
struct ZoneHost {
    zones: Mutex<Vec<DnsZone>>,
    creates: AtomicUsize,
    fail_create: AtomicBool,
    records: AtomicUsize,
}
impl DnsProvider for ZoneHost {
    async fn verify(&self, _: &reqwest::Client) -> Result<Vec<DnsZone>, ProvisionError> {
        Ok(self.zones.lock().unwrap().clone())
    }
    async fn create_zone(
        &self,
        _: &reqwest::Client,
        domain: &str,
    ) -> Result<DnsZone, ProvisionError> {
        if self.fail_create.swap(false, Ordering::SeqCst) {
            return Err(refused("zone create"));
        }
        self.creates.fetch_add(1, Ordering::SeqCst);
        let zone = DnsZone {
            id: "z-1".into(),
            name: domain.into(),
            nameservers: NS.iter().map(|s| s.to_string()).collect(),
        };
        self.zones.lock().unwrap().push(zone.clone());
        Ok(zone)
    }
    async fn create_record(
        &self,
        _: &reqwest::Client,
        zone_id: &str,
        _: &DnsRecord,
    ) -> Result<(), ProvisionError> {
        assert_eq!(zone_id, "z-1", "records go into the created zone");
        self.records.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn find_records(
        &self,
        _: &reqwest::Client,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<Vec<DnsRecord>, ProvisionError> {
        Ok(vec![])
    }
    async fn delete_record(
        &self,
        _: &reqwest::Client,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<(), ProvisionError> {
        Ok(())
    }
    fn base_url(&self) -> &str {
        ""
    }
}

/// A registrar with the nameserver door; `fail_register` / `fail_ns` refuse
/// the next call.
#[derive(Default)]
struct Registry_ {
    registered: AtomicUsize,
    pointed: Mutex<Vec<Vec<String>>>,
    fail_register: AtomicBool,
    fail_ns: AtomicBool,
}
impl Registrar for Registry_ {
    async fn verify(&self, _: &reqwest::Client) -> Result<(), ProvisionError> {
        Ok(())
    }
    async fn check(
        &self,
        _: &reqwest::Client,
        domain: &str,
    ) -> Result<DomainAvailability, ProvisionError> {
        Ok(DomainAvailability {
            domain: domain.into(),
            available: true,
            price_first_year_cents: Some(1000),
            price_renewal_cents: Some(1000),
            currency: Some("EUR".into()),
        })
    }
    async fn register(
        &self,
        _: &reqwest::Client,
        domain: &str,
        _: u32,
        _: u64,
        _: Option<&ContactInfo>,
    ) -> Result<RegistrationResult, ProvisionError> {
        if self.fail_register.swap(false, Ordering::SeqCst) {
            return Err(refused("registration"));
        }
        self.registered.fetch_add(1, Ordering::SeqCst);
        Ok(RegistrationResult {
            domain: domain.into(),
            nameservers: vec![],
        })
    }
    async fn set_nameservers(
        &self,
        _: &reqwest::Client,
        domain: &str,
        nameservers: &[String],
    ) -> Result<(), ProvisionError> {
        assert_eq!(domain, DOMAIN);
        if self.fail_ns.swap(false, Ordering::SeqCst) {
            return Err(refused("nameserver set"));
        }
        self.pointed.lock().unwrap().push(nameservers.to_vec());
        Ok(())
    }
}

/// Hetzner's server API (create, PTR) and the new box's `/api/v1/health`.
async fn cloud() -> MockServer {
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
                .set_body_string(r#"{"server":{"id":42,"public_net":{"ipv4":{"ip":"5.6.7.8"}}}}"#),
        )
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/servers/42/actions/change_dns_ptr"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/servers/42"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"server":{"public_net":{"ipv4":{"dns_ptr":null}}}}"#),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/health"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock)
        .await;
    mock
}

async fn servers_created(mock: &MockServer) -> usize {
    mock.received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/servers")
        .count()
}

fn params() -> CloudInitParams {
    CloudInitParams {
        domain: DOMAIN.into(),
        image_tag: "latest".into(),
        claim_code: "claim".into(),
        enable_mail: false,
        deployment_seed: None,
        ssh_door: None,
    }
}

/// What the run told its caller, in order.
#[derive(Default)]
struct Events(Mutex<Vec<DomainEvent>>);

/// One split run: `already_registered` is the caller's wizard-life memory.
async fn split_run(
    mock: &MockServer,
    dns: &ZoneHost,
    reg: &Registry_,
    already_registered: bool,
    events: &Events,
) -> (Result<(), String>, ProvisioningSnapshot) {
    let hetzner = Hetzner::with_base_url("t".into(), mock.uri());
    let on_event = |e: DomainEvent| events.0.lock().unwrap().push(e);
    let purchase = DomainPurchase {
        registrar: reg,
        years: 1,
        agreed_price_cents: 1000,
        contact: None,
        already_registered,
        delegate: true,
        on_event: &on_event,
    };
    let state = Mutex::new(ProvisioningSnapshot::idle());
    let result = provision_with_purchase_snapshot(
        &reqwest::Client::new(),
        |_ip: &str| reqwest::Client::new(),
        None,
        &hetzner,
        dns,
        &purchase,
        DOMAIN,
        &mock.uri(),
        "box-1",
        "fsn1",
        "cx23",
        &params(),
        &[],
        &state,
        || {},
        |_ip, _origin| {},
        &CancelFlag::new(),
        |_ms: u64| async move {},
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string());
    (result, state.into_inner().unwrap())
}

fn step_status(snap: &ProvisioningSnapshot, kind: ProvisionStep) -> StepStatus {
    snap.steps.iter().find(|s| s.kind == kind).unwrap().status
}

#[tokio::test]
async fn a_split_run_registers_creates_the_zone_points_the_domain_and_publishes_into_it() {
    let mock = cloud().await;
    let (dns, reg, events) = (ZoneHost::default(), Registry_::default(), Events::default());
    let (result, snap) = split_run(&mock, &dns, &reg, false, &events).await;
    result.expect("the split run succeeds");
    assert_eq!(snap.overall, OverallStatus::Succeeded);
    assert_eq!(reg.registered.load(Ordering::SeqCst), 1);
    assert_eq!(dns.creates.load(Ordering::SeqCst), 1);
    assert_eq!(
        *reg.pointed.lock().unwrap(),
        vec![NS.iter().map(|s| s.to_string()).collect::<Vec<_>>()],
        "the registrar is given the zone's own nameservers"
    );
    assert!(
        dns.records.load(Ordering::SeqCst) > 0,
        "the Dns step publishes into the new zone at once"
    );
    assert_eq!(
        *events.0.lock().unwrap(),
        vec![DomainEvent::Registered, DomainEvent::Pointed]
    );
}

#[tokio::test]
async fn a_failed_registration_stops_the_run_before_anything_else() {
    let mock = cloud().await;
    let (dns, reg, events) = (ZoneHost::default(), Registry_::default(), Events::default());
    reg.fail_register.store(true, Ordering::SeqCst);
    let (result, snap) = split_run(&mock, &dns, &reg, false, &events).await;
    assert!(result.is_err());
    assert_eq!(
        step_status(&snap, ProvisionStep::Domain),
        StepStatus::Failed
    );
    assert_eq!(dns.creates.load(Ordering::SeqCst), 0);
    assert_eq!(servers_created(&mock).await, 0);
    assert!(events.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_failed_zone_create_is_resumed_by_a_retry_without_a_second_purchase() {
    let mock = cloud().await;
    let (dns, reg, events) = (ZoneHost::default(), Registry_::default(), Events::default());
    dns.fail_create.store(true, Ordering::SeqCst);
    let (result, snap) = split_run(&mock, &dns, &reg, false, &events).await;
    assert!(result.is_err());
    assert_eq!(
        step_status(&snap, ProvisionStep::Domain),
        StepStatus::Failed
    );
    assert_eq!(reg.registered.load(Ordering::SeqCst), 1);
    assert!(
        reg.pointed.lock().unwrap().is_empty(),
        "nothing points anywhere new"
    );
    assert_eq!(servers_created(&mock).await, 0, "no server is bought");
    assert_eq!(*events.0.lock().unwrap(), vec![DomainEvent::Registered]);

    // Retry, with the caller's memory of the registration.
    let (result, _) = split_run(&mock, &dns, &reg, true, &events).await;
    result.expect("the retry finishes the run");
    assert_eq!(reg.registered.load(Ordering::SeqCst), 1, "bought once");
    assert_eq!(dns.creates.load(Ordering::SeqCst), 1);
    assert_eq!(reg.pointed.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_failed_nameserver_set_is_repeated_on_retry_though_the_zone_exists() {
    let mock = cloud().await;
    let (dns, reg, events) = (ZoneHost::default(), Registry_::default(), Events::default());
    reg.fail_ns.store(true, Ordering::SeqCst);
    let (result, _) = split_run(&mock, &dns, &reg, false, &events).await;
    assert!(result.is_err());
    assert_eq!(
        dns.creates.load(Ordering::SeqCst),
        1,
        "a zone nothing uses yet"
    );
    assert_eq!(servers_created(&mock).await, 0);
    assert_eq!(*events.0.lock().unwrap(), vec![DomainEvent::Registered]);

    let (result, _) = split_run(&mock, &dns, &reg, true, &events).await;
    result.expect("the retry finishes the run");
    assert_eq!(
        dns.creates.load(Ordering::SeqCst),
        1,
        "the existing zone is reused, not created again"
    );
    assert_eq!(
        reg.pointed.lock().unwrap().len(),
        1,
        "the nameserver set is repeated, never skipped on the zone's strength"
    );
    assert_eq!(reg.registered.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_registration_only_run_registers_builds_and_publishes_nothing() {
    let mock = cloud().await;
    let reg = Registry_::default();
    let events = Events::default();
    let hetzner = Hetzner::with_base_url("t".into(), mock.uri());
    let on_event = |e: DomainEvent| events.0.lock().unwrap().push(e);
    let state = Mutex::new(ProvisioningSnapshot::idle());
    let run = |already_registered: bool| {
        let purchase = DomainPurchase {
            registrar: &reg,
            years: 1,
            agreed_price_cents: 1000,
            contact: None,
            already_registered,
            delegate: false,
            on_event: &on_event,
        };
        let hetzner = &hetzner;
        let state = &state;
        async move {
            provision_registered_no_dns(
                &reqwest::Client::new(),
                None,
                hetzner,
                &purchase,
                DOMAIN,
                "box-1",
                "fsn1",
                "cx23",
                &params(),
                &[],
                state,
                || {},
                |_ip, _origin| {},
                &CancelFlag::new(),
            )
            .await
        }
    };
    let deferred = run(false).await.expect("the deferred run succeeds");
    assert!(!deferred.records.is_empty(), "the records to add by hand");
    let snap = state.lock().unwrap().clone();
    assert_eq!(snap.overall, OverallStatus::Succeeded);
    assert_eq!(
        step_status(&snap, ProvisionStep::Domain),
        StepStatus::Succeeded
    );
    assert_eq!(step_status(&snap, ProvisionStep::Dns), StepStatus::Skipped);
    assert_eq!(
        step_status(&snap, ProvisionStep::Online),
        StepStatus::Skipped
    );
    assert_eq!(reg.registered.load(Ordering::SeqCst), 1);
    assert!(reg.pointed.lock().unwrap().is_empty(), "nothing re-pointed");
    assert_eq!(*events.0.lock().unwrap(), vec![DomainEvent::Registered]);

    // A Retry with the registration remembered buys nothing again.
    *state.lock().unwrap() = ProvisioningSnapshot::idle();
    run(true).await.expect("the retry succeeds");
    assert_eq!(reg.registered.load(Ordering::SeqCst), 1);
    let snap = state.lock().unwrap().clone();
    assert_eq!(
        step_status(&snap, ProvisionStep::Domain),
        StepStatus::Skipped
    );
}

#[tokio::test]
async fn a_failed_registration_on_the_registration_only_run_buys_no_server() {
    let mock = cloud().await;
    let reg = Registry_::default();
    reg.fail_register.store(true, Ordering::SeqCst);
    let hetzner = Hetzner::with_base_url("t".into(), mock.uri());
    let on_event = |_: DomainEvent| {};
    let purchase = DomainPurchase {
        registrar: &reg,
        years: 1,
        agreed_price_cents: 1000,
        contact: None,
        already_registered: false,
        delegate: false,
        on_event: &on_event,
    };
    let state = Mutex::new(ProvisioningSnapshot::idle());
    let result = provision_registered_no_dns(
        &reqwest::Client::new(),
        None,
        &hetzner,
        &purchase,
        DOMAIN,
        "box-1",
        "fsn1",
        "cx23",
        &params(),
        &[],
        &state,
        || {},
        |_ip, _origin| {},
        &CancelFlag::new(),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(servers_created(&mock).await, 0);
}
