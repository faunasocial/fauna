//! S4b — the **pebble real-wire acceptance test** for the DNS-01 order core.
//!
//! `obtain_certificate_dns01` (S4a) and the `IssueCert` orchestration (S5) are
//! unit-tested only up to the publish/teardown choreography and the post-CA
//! seal/deliver — both stop short of a real CA, because the
//! account→order→authorizations→finalize half cannot be exercised without one.
//! This test closes that gap: it drives the order end to end against a local
//! [pebble](https://github.com/letsencrypt/pebble) ACME server, with an
//! in-process authoritative DNS server answering the `_acme-challenge` TXT that
//! pebble's validation queries.
//!
//! Shape (the pieces and why each exists):
//! - **In-process DNS responder** ([`spawn_dns_responder`]) — a raw `tokio` UDP
//!   socket + hickory-proto `Message` codec. Pebble is started with
//!   `-dnsserver <this>`, so every name it resolves comes from here; it answers
//!   the published `_acme-challenge` TXT and returns empty-NOERROR for everything
//!   else (so the CAA pre-check finds no restriction).
//! - **DNS-writing seam** ([`DnsWritingSeam`]) — a [`DnsProviderSeam`] whose
//!   `publish`/`teardown` write/remove the TXT in the responder's shared map, so
//!   the order's *real* publish path (`publish_acme_challenge` over the seam)
//!   lands the record pebble then reads. No provider API is involved.
//! - **CA-trusting HTTP client** ([`pebble_http_client`]) — pebble serves ACME
//!   over HTTPS signed by its own throwaway CA, which the native-roots default
//!   client won't trust; this builds a hyper client whose rustls roots include
//!   pebble's `pebble.minica.pem`, injected through the `test-helpers`-only
//!   [`obtain_certificate_dns01_with_http`] entry (production never trusts it).
//! - **Pebble lifecycle** ([`Pebble`]) — `docker run`/`rm -f` (RAII) on the host
//!   network, the closest in-CI mirror of a real CA conversation.
//!
//! `#[ignore]` — it needs Docker and pulls the pebble image, so it is opt-in
//! (`just e2e-pebble-dns01`, or `cargo test -p fauna-client-dns --test
//! pebble_dns01 -- --ignored`), never in the `cargo test` inner loop. Pebble
//! binds fixed host ports (14000), so the tests in this file serialize on
//! [`PEBBLE_SERIAL`] — safe to run the whole file in one invocation.
//!
//! Besides the issuance acceptance, this file holds the **minimum reproduction
//! of the 2026-07-23 live example.com renewal failure**
//! ([`dns01_order_goes_invalid_when_published_txt_is_not_yet_resolvable`]): the
//! provider's control plane accepts the `_acme-challenge` publish but its
//! authoritative data plane does not serve it by validation time, so the CA
//! marks the order `Invalid` — reproduced here against a real CA conversation
//! with zero Let's Encrypt traffic, no VPS, and no client.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::net::UdpSocket;

use hickory_proto::op::{Message, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::rdata::{CNAME, TXT};
use hickory_proto::rr::{Name, RData, Record, RecordType};

use fauna_client_dns::{
    Dns01OrderConfig, Dns01ResolvabilityProbe, DnsAction, DnsManagementMachine, DnsNest,
    DnsNestError, DnsProviderError, DnsProviderSeam, ManualOrderTestSeam, PublishRecord,
    begin_dns01_order_with_http, complete_dns01_order, obtain_certificate_dns01_with_http,
    pure_begin_dns01_order_with_http, pure_complete_dns01_order,
    pure_obtain_certificate_dns01_with_http,
};
use fauna_core::data::DnsZoneRef;
use fauna_core::secret::SecretString;
use fauna_pebble_testkit::{PEBBLE_DIR_URL, Pebble};
use fauna_protocol::dns::{DomainDns, DomainVerifyStatus};
use fauna_protocol::tls::{CertStatusReply, PublishCertRequest};

/// Serializes the tests in this file: pebble always binds host port 14000, so
/// two instances cannot coexist. A tokio mutex (held across `.await`s in each
/// test body) rather than a std one.
static PEBBLE_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `_acme-challenge.<domain>` (bare, lowercase) → the TXT values currently
/// published for it. Shared by the DNS responder (reads) and the seam (writes).
type Records = Arc<Mutex<HashMap<String, Vec<String>>>>;

/// `_acme-challenge.<domain>` (bare, lowercase) → its CNAME target (bare). The
/// one-time delegation the admin sets at their no-API registrar (S6b); seeded by
/// the test, read by the responder so a TXT lookup for the delegated name follows
/// the CNAME to the controlled-zone target where the renewal TXT was published.
type Cnames = Arc<Mutex<HashMap<String, String>>>;

// ── in-process authoritative DNS responder ──────────────────────────────────

/// Strip a trailing dot and lowercase, so a seam-written bare name
/// (`_acme-challenge.home.example.test`) and a query's FQDN
/// (`_acme-challenge.home.example.test.`) key the same map entry.
fn normalize(name: &str) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

/// Bind an ephemeral UDP socket, spawn the responder loop, and return the
/// address to hand pebble's `-dnsserver`.
async fn spawn_dns_responder(records: Records, cnames: Cnames) -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind dns responder udp socket");
    let addr = socket.local_addr().expect("dns responder local addr");
    tokio::spawn(run_dns(socket, records, cnames));
    addr
}

async fn run_dns(socket: UdpSocket, records: Records, cnames: Cnames) {
    let mut buf = [0u8; 1500];
    loop {
        let (n, src) = match socket.recv_from(&mut buf).await {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        let Ok(request) = Message::from_vec(&buf[..n]) else {
            continue;
        };
        let response = build_response(&request, &records, &cnames);
        if let Ok(bytes) = response.to_vec() {
            let _ = socket.send_to(&bytes, src).await;
        }
    }
}

/// Answer a query: the published TXT values for a `_acme-challenge.*` TXT lookup,
/// empty-NOERROR for everything else (so pebble's CAA walk finds no restriction).
/// **CNAME delegation (S6b):** if the queried name has a seeded CNAME, return the
/// CNAME RR **and chase it in-zone** — appending the target's TXT values — so the
/// CA reads the renewal TXT published at the controlled-zone target name, exactly
/// as a real authoritative server would (one-shot CNAME chasing).
fn build_response(request: &Message, records: &Records, cnames: &Cnames) -> Message {
    // hickory-proto 0.26: header fields moved onto the public `Message::metadata`
    // (`Metadata`); the `set_*` accessors and `Message::new()` (no-arg) are gone —
    // id/type/op_code go through the constructor, the rest are public fields.
    let mut response = Message::new(request.metadata.id, MessageType::Response, OpCode::Query);
    response.metadata.recursion_desired = request.metadata.recursion_desired;
    response.metadata.recursion_available = true;
    response.metadata.authoritative = true;
    response.metadata.response_code = ResponseCode::NoError;
    for query in &request.queries {
        response.add_query(query.clone());
    }

    if let Some(query) = request.queries.first()
        && query.query_type() == RecordType::TXT
    {
        let owner = query.name().clone();
        let key = normalize(&owner.to_ascii());

        // Follow a one-time delegation CNAME, if any: emit the CNAME RR and chase
        // to the target name for the TXT lookup.
        let (txt_key, txt_owner) = match cnames.lock().unwrap().get(&key) {
            Some(target) => {
                if let Ok(target_name) = Name::from_ascii(format!("{target}.")) {
                    response.add_answer(Record::from_rdata(
                        owner.clone(),
                        5,
                        RData::CNAME(CNAME(target_name.clone())),
                    ));
                    (normalize(target), target_name)
                } else {
                    (key.clone(), owner.clone())
                }
            }
            None => (key.clone(), owner.clone()),
        };

        if let Some(values) = records.lock().unwrap().get(&txt_key) {
            for value in values {
                response.add_answer(Record::from_rdata(
                    txt_owner.clone(),
                    5,
                    RData::TXT(TXT::new(vec![value.clone()])),
                ));
            }
        }
    }
    response
}

// ── Resolvability probe fake ─────────────────────────────────────────────────

/// A [`Dns01ResolvabilityProbe`] that reports **not visible** for its first
/// `visible_after` calls, then visible — the behavioural stand-in for a registrar
/// whose authoritative NS has not caught up with the admin's paste yet.
///
/// Used by the manual-mode phase to prove the completion path really *waits* on
/// the probe before signalling the CA (FINDING C, 2026-07-24 live failure). A
/// green run with `calls() > visible_after` is the assertion that the gate polled
/// through a not-visible phase rather than validating immediately.
struct LateVisibleProbe {
    calls: Arc<std::sync::atomic::AtomicU32>,
    visible_after: u32,
}

impl LateVisibleProbe {
    fn visible_after(n: u32) -> Self {
        Self {
            calls: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            visible_after: n,
        }
    }
    fn calls(&self) -> u32 {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl Dns01ResolvabilityProbe for LateVisibleProbe {
    async fn txt_visible(&self, _zone: &str, _name: &str, _value: &str) -> bool {
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        n >= self.visible_after
    }
}

// ── DNS-writing provider seam ────────────────────────────────────────────────

/// A [`DnsProviderSeam`] that lands `_acme-challenge` records in the responder's
/// map instead of calling a real provider API — so the order's production publish
/// path (`publish_acme_challenge`) writes exactly what pebble then resolves.
struct DnsWritingSeam {
    records: Records,
    zone: DnsZoneRef,
}

#[async_trait]
impl DnsProviderSeam for DnsWritingSeam {
    async fn verify(
        &self,
        _provider_id: &str,
        _fields: &[(String, SecretString)],
    ) -> Result<Vec<DnsZoneRef>, DnsProviderError> {
        Ok(vec![self.zone.clone()])
    }

    async fn publish(
        &self,
        _provider_id: &str,
        _fields: &[(String, SecretString)],
        _zone: &DnsZoneRef,
        records: &[PublishRecord],
    ) -> Result<(), DnsProviderError> {
        let mut map = self.records.lock().unwrap();
        for record in records {
            let values = map.entry(normalize(&record.name)).or_default();
            if !values.contains(&record.value) {
                values.push(record.value.clone());
            }
        }
        Ok(())
    }

    async fn teardown(
        &self,
        _provider_id: &str,
        _fields: &[(String, SecretString)],
        _zone: &DnsZoneRef,
        records: &[PublishRecord],
    ) -> Result<(), DnsProviderError> {
        let mut map = self.records.lock().unwrap();
        for record in records {
            if let Some(values) = map.get_mut(&normalize(&record.name)) {
                values.retain(|value| value != &record.value);
            }
        }
        Ok(())
    }

    async fn find_records(
        &self,
        _provider_id: &str,
        _fields: &[(String, SecretString)],
        _zone: &DnsZoneRef,
        name: &str,
        record_type: &str,
    ) -> Result<Vec<PublishRecord>, DnsProviderError> {
        // Faithfully reflect the in-memory zone (the map is name-keyed; this test
        // only writes `_acme-challenge` TXT, so the type is echoed back).
        let map = self.records.lock().unwrap();
        Ok(map
            .get(&normalize(name))
            .into_iter()
            .flatten()
            .map(|value| PublishRecord {
                name: name.to_string(),
                record_type: record_type.to_string(),
                value: value.clone(),
                ttl_seconds: 5,
                priority: None,
            })
            .collect())
    }
}

// ── CA-trusting HTTP client (the pebble wrinkle) ─────────────────────────────
//
// The instant-acme driver's CA-trusting, UA-tagged `HttpClient` is
// `fauna_pebble_testkit::pebble_http_client` — shared with
// `pebble_http01.rs`'s identical wrinkle (was two byte-identical copies).
// This thin wrapper just names this file's own UA string, so every call site
// below stays unchanged.
fn pebble_http_client(ca_pem: &[u8]) -> Box<dyn instant_acme::HttpClient> {
    fauna_pebble_testkit::pebble_http_client(ca_pem, "fauna-pebble-dns01-test/0")
}

/// The **pure** (`acme_pure`) driver's pebble client — a `reqwest::Client` with
/// pebble's throwaway CA (`ca_pem`) added to the root store (so its self-signed
/// ACME endpoint verifies) and the `User-Agent` pebble requires (RFC 8555 §6.1).
/// Far simpler than the instant-acme client above: reqwest takes both directly on
/// the builder, no hyper/rustls plumbing or `HttpClient` wrapper. This is exactly
/// the wasm production path's client minus the CA pin (the browser trusts LE's
/// public chain natively).
fn pure_pebble_http_client(ca_pem: &[u8]) -> reqwest::Client {
    let cert = reqwest::Certificate::from_pem(ca_pem).expect("parse pebble CA pem");
    reqwest::Client::builder()
        .add_root_certificate(cert)
        .user_agent("fauna-pebble-dns01-pure-test/0")
        .build()
        .expect("build pebble-trusting reqwest client")
}

// ── pebble lifecycle ─────────────────────────────────────────────────────────
//
// `Pebble` (docker RAII lifecycle) and `ensure_docker` are
// `fauna_pebble_testkit` — shared with `pebble_http01.rs`'s identical
// lifecycle (was two byte-identical copies). This thin wrapper just names
// this file's own message, so the three `ensure_docker();` call sites below
// stay unchanged; `Pebble::start` call sites gained a `"dns01"` flow arg.
fn ensure_docker() {
    fauna_pebble_testkit::ensure_docker("the pebble DNS-01 acceptance test")
}

/// A fixed test signing key for [`DnsManagementMachine::with_credentials`]'s
/// actor-key seal input — the manual resume test doesn't assert on the seal
/// itself (that's `deliver_issued_cert_seals_to_target_and_publishes` in the
/// crate's own unit tests), just that delivery happened.
fn test_signing_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])
}

// ── assertions ───────────────────────────────────────────────────────────────

/// Assert the issued chain's leaf is a real pebble-CA-issued cert (not a
/// self-signed floor) covering exactly `expected_domains` — the proof the
/// account→order→finalize half ran against the CA.
fn assert_pebble_issued(cert_chain_pem: &str, expected_domains: &[String]) {
    use x509_parser::extensions::GeneralName;
    use x509_parser::pem::parse_x509_pem;

    let (_, pem) = parse_x509_pem(cert_chain_pem.as_bytes()).expect("leaf parses as PEM");
    let leaf = pem.parse_x509().expect("leaf parses as X.509");

    let issuer = leaf.issuer().to_string();
    assert!(
        issuer.contains("Pebble"),
        "leaf must be issued by pebble's CA (not a self-signed floor); issuer = {issuer}"
    );
    assert!(
        leaf.validity().not_after.timestamp() > leaf.validity().not_before.timestamp(),
        "issued cert has a sane validity window"
    );

    let san = leaf
        .subject_alternative_name()
        .expect("SAN extension readable")
        .expect("leaf has a SAN extension")
        .value;
    let mut dns_names: Vec<String> = san
        .general_names
        .iter()
        .filter_map(|name| match name {
            GeneralName::DNSName(n) => Some(n.to_ascii_lowercase()),
            _ => None,
        })
        .collect();
    dns_names.sort();
    let mut want: Vec<String> = expected_domains
        .iter()
        .map(|d| d.to_ascii_lowercase())
        .collect();
    want.sort();
    assert_eq!(
        dns_names, want,
        "leaf SANs cover exactly the ordered identifiers"
    );
}

// ── the test ─────────────────────────────────────────────────────────────────

/// Drive DNS-01 orders against pebble end to end with **both** order drivers on one
/// CA instance — the native `acme_order` (instant-acme; the 5 native apps) and
/// the wasm-safe `acme_pure` (RustCrypto + reqwest; web — W3 (account-data-plane.md § Workstreams)/W6). One pebble
/// instance binds host port 14000, so both drivers run sequentially here rather than
/// in two racing test fns.
///
/// **Native driver (phases 1–4):** (1) **managed**, fresh account — exercises
/// account→order→authorizations→DNS-01-validate→finalize→fetch via the provider
/// seam; (2) **managed**, persisted account reused (D6) — proves the returned
/// credentials are reused verbatim, not churned into a new account; (3) **manual**
/// (tier 3, S6) — `begin_dns01_order` → simulate the admin pasting the surfaced
/// `_acme-challenge` TXT (no provider seam) → `complete_dns01_order`, proving the
/// suspend/resume two-phase split round-trips; (4) **CNAME-delegated** (S6b).
///
/// **Pure driver (phases 5–8):** (5) **managed**, fresh account — the pure
/// account→order→authz→finalize→fetch flow end to end; (6) **cross-driver D6
/// interop** — the pure driver reuses the account the *native* (instant-acme/ring)
/// driver created in phase 1, proving the `AccountCredentials` blob is genuinely
/// byte-interoperable against a real CA (the W1 unit test proves it parses; this
/// proves it *issues*); (7) **manual** two-phase on the pure driver; (8) the pure
/// driver's **CNAME-delegated** redirect (`challenge_publish_names`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker + pulls ghcr.io/letsencrypt/pebble; run via `just e2e-pebble-dns01` or `cargo test -p fauna-client-dns --test pebble_dns01 -- --ignored`"]
async fn pebble_dns01_native_and_pure_drivers_issue_real_certs() {
    ensure_docker();
    let _serial = PEBBLE_SERIAL.lock().await;

    let records: Records = Arc::new(Mutex::new(HashMap::new()));
    let cnames: Cnames = Arc::new(Mutex::new(HashMap::new()));
    let dns_addr = spawn_dns_responder(records.clone(), cnames.clone()).await;
    let pebble = Pebble::start("dns01", "issue", &dns_addr.to_string());

    let zone = DnsZoneRef {
        id: "z-test".to_string(),
        name: "example.test".to_string(),
    };
    let seam = DnsWritingSeam {
        records: records.clone(),
        zone: zone.clone(),
    };
    // Two SANs under the one zone — exercises the multi-authorization loop.
    let domains = vec![
        "home.example.test".to_string(),
        "mail.example.test".to_string(),
    ];
    let cfg = Dns01OrderConfig {
        directory_url: PEBBLE_DIR_URL.to_string(),
        contact_email: String::new(),
        domains: domains.clone(),
        // The responder serves the TXT the instant the seam writes it (same
        // process), so no propagation delay is needed.
        propagation_wait: Duration::ZERO,
        resolvability_poll_interval: Duration::ZERO,
        resolvability_deadline: Duration::ZERO,
        challenge_publish_names: Vec::new(),
    };

    // First issuance — fresh account (None). Pebble validates DNS-01 against the
    // in-process responder, then finalizes and issues.
    let issued = obtain_certificate_dns01_with_http(
        &cfg,
        None,
        &seam,
        "pebble",
        &[],
        &zone,
        None,
        pebble_http_client(&pebble.ca_pem),
    )
    .await
    .expect("first DNS-01 issuance against pebble");

    assert!(
        !issued.account_credentials.is_empty(),
        "a fresh ACME account was created and its credentials returned for persistence"
    );
    assert_pebble_issued(&issued.cert_chain_pem, &domains);
    assert!(
        issued.privkey_pem.contains("PRIVATE KEY"),
        "the leaf private key is returned in PEM form"
    );

    // Second issuance — reuse the persisted account (D6). The order must succeed
    // again and hand back the SAME credentials (no new-account churn).
    let reissued = obtain_certificate_dns01_with_http(
        &cfg,
        Some(&issued.account_credentials),
        &seam,
        "pebble",
        &[],
        &zone,
        None,
        pebble_http_client(&pebble.ca_pem),
    )
    .await
    .expect("second DNS-01 issuance reusing the persisted account");

    assert_eq!(
        reissued.account_credentials, issued.account_credentials,
        "the persisted ACME account is reused, not recreated (D6)"
    );
    assert_pebble_issued(&reissued.cert_chain_pem, &domains);

    // Third issuance — the MANUAL two-phase path (tier 3, S6): begin the order, then
    // — instead of the seam auto-publishing — simulate the admin pasting the surfaced
    // `_acme-challenge` TXT into their registrar (write it straight into the responder
    // map), then complete. Proves `begin_dns01_order`/`complete_dns01_order` round-trip
    // against the real CA with NO provider seam. A distinct single-SAN name, reusing
    // the persisted account (D6 holds on the manual path too).
    let manual_domains = vec!["manual.example.test".to_string()];
    let manual_cfg = Dns01OrderConfig {
        directory_url: PEBBLE_DIR_URL.to_string(),
        contact_email: String::new(),
        domains: manual_domains.clone(),
        propagation_wait: Duration::ZERO,
        resolvability_poll_interval: Duration::ZERO,
        resolvability_deadline: Duration::ZERO,
        challenge_publish_names: Vec::new(),
    };
    let in_progress = begin_dns01_order_with_http(
        &manual_cfg,
        Some(&issued.account_credentials),
        pebble_http_client(&pebble.ca_pem),
    )
    .await
    .expect("begin the manual DNS-01 order against pebble");

    // The exact paste surface the admin would see — the transient `_acme-challenge`
    // TXT row(s) the `admin-dns` page renders for manual mode.
    let to_paste = in_progress.challenges_to_publish();
    assert_eq!(
        to_paste.len(),
        1,
        "one challenge for the single-SAN manual order"
    );
    assert_eq!(to_paste[0].name, "_acme-challenge.manual.example.test");
    assert_eq!(to_paste[0].record_type, "TXT");
    assert_eq!(to_paste[0].ttl_seconds, 120);
    assert!(
        !to_paste[0].value.is_empty(),
        "the key-authorization value to paste is present"
    );

    // Simulate the admin publishing the TXT by hand (no provider seam in manual mode).
    {
        let mut map = records.lock().unwrap();
        for rec in &to_paste {
            map.entry(normalize(&rec.name))
                .or_default()
                .push(rec.value.clone());
        }
    }

    // FINDING C real-wire proof: the manual completion runs the propagation gate.
    // The probe reports not-visible once, so a green run means the path polled
    // again (paying one `DEFAULT_RESOLVABILITY_POLL_INTERVAL`) *before* telling the
    // CA to validate — the wait that was missing when the live 2026-07-24 order
    // went `Invalid` on its first attempt.
    let manual_probe = LateVisibleProbe::visible_after(1);
    let manual_issued = complete_dns01_order(
        in_progress,
        Duration::ZERO,
        "manual.example.test",
        Some(&manual_probe),
    )
    .await
    .expect("complete the manual DNS-01 order after the admin pasted the TXT");
    assert!(
        manual_probe.calls() >= 2,
        "the manual path polled the resolvability probe through a not-visible \
         phase before signalling the CA (got {} calls)",
        manual_probe.calls()
    );
    assert_eq!(
        manual_issued.account_credentials, issued.account_credentials,
        "the manual path reuses the persisted account too (D6)"
    );
    assert_pebble_issued(&manual_issued.cert_chain_pem, &manual_domains);

    // Fourth issuance — the CNAME-DELEGATED renewal (tier 3, S6b): a manual-mode
    // domain whose `_acme-challenge` the admin has CNAME-delegated, **once**, into a
    // zone they DO control. Renewal then auto-publishes the TXT at the delegated
    // target name inside the controlled zone (a managed-style order with a publish
    // redirect) and the CA follows the CNAME — no manual paste. Proves the
    // `challenge_publish_names` redirect end-to-end against the real CA.
    let delegated_domain = "delegated.example.test".to_string();
    let controlled_zone = DnsZoneRef {
        id: "z-controlled".to_string(),
        name: "controlled.example".to_string(),
    };
    // The re-homing convention: `_acme-challenge.<domain>.<controlled-zone>`.
    let target_name = format!(
        "_acme-challenge.{delegated_domain}.{}",
        controlled_zone.name
    );

    // Simulate the admin's ONE-TIME CNAME paste at their no-API registrar:
    // `_acme-challenge.delegated.example.test` → the controlled-zone target.
    cnames.lock().unwrap().insert(
        normalize(&format!("_acme-challenge.{delegated_domain}")),
        target_name.clone(),
    );

    // The renewal order redirects this domain's `_acme-challenge` publish to the
    // delegated target name (what `DnsManagementMachine::issue_cert` sets from the
    // persisted `CnameDelegation`); the seam writes the TXT at the controlled-zone
    // target, where the responder's CNAME chase finds it.
    let delegated_cfg = Dns01OrderConfig {
        directory_url: PEBBLE_DIR_URL.to_string(),
        contact_email: String::new(),
        domains: vec![delegated_domain.clone()],
        propagation_wait: Duration::ZERO,
        resolvability_poll_interval: Duration::ZERO,
        resolvability_deadline: Duration::ZERO,
        challenge_publish_names: vec![(delegated_domain.clone(), target_name.clone())],
    };
    let delegated_issued = obtain_certificate_dns01_with_http(
        &delegated_cfg,
        Some(&issued.account_credentials),
        &seam,
        "pebble",
        &[],
        &controlled_zone,
        None,
        pebble_http_client(&pebble.ca_pem),
    )
    .await
    .expect("complete the CNAME-delegated DNS-01 renewal against pebble");
    assert_eq!(
        delegated_issued.account_credentials, issued.account_credentials,
        "the delegated renewal reuses the persisted account too (D6)"
    );
    assert_pebble_issued(
        &delegated_issued.cert_chain_pem,
        std::slice::from_ref(&delegated_domain),
    );

    // ───────────────────────────────────────────────────────────────────────
    // PURE DRIVER (acme_pure — the wasm production path, proven here on native).
    // ───────────────────────────────────────────────────────────────────────

    // Phase 5 — pure, MANAGED, fresh account: the pure account→order→authz→
    // DNS-01-validate→finalize→fetch flow end to end, fresh (None) account.
    let pure_domains = vec![
        "pure.example.test".to_string(),
        "mail.pure.example.test".to_string(),
    ];
    let pure_cfg = Dns01OrderConfig {
        directory_url: PEBBLE_DIR_URL.to_string(),
        contact_email: String::new(),
        domains: pure_domains.clone(),
        propagation_wait: Duration::ZERO,
        resolvability_poll_interval: Duration::ZERO,
        resolvability_deadline: Duration::ZERO,
        challenge_publish_names: Vec::new(),
    };
    let pure_issued = pure_obtain_certificate_dns01_with_http(
        &pure_cfg,
        None,
        &seam,
        "pebble",
        &[],
        &zone,
        None,
        pure_pebble_http_client(&pebble.ca_pem),
    )
    .await
    .expect("pure driver: managed DNS-01 issuance against pebble (fresh account)");
    assert!(
        !pure_issued.account_credentials.is_empty(),
        "the pure driver created an ACME account and returned its credentials"
    );
    assert_pebble_issued(&pure_issued.cert_chain_pem, &pure_domains);
    assert!(
        pure_issued.privkey_pem.contains("PRIVATE KEY"),
        "the pure driver's leaf private key is returned in PEM form"
    );

    // Phase 6 — CROSS-DRIVER D6 INTEROP: the pure driver reuses the account the
    // NATIVE instant-acme/ring driver created in phase 1. A fresh SAN (so pebble has
    // no cached-valid authorization for it — the full challenge path runs) under the
    // native account proves the `AccountCredentials` blob is byte-interoperable
    // against a real CA: the native-written PKCS#8 key + kid restore in the pure
    // RustCrypto driver and *issue*.
    let cross_domains = vec!["cross.example.test".to_string()];
    let cross_cfg = Dns01OrderConfig {
        directory_url: PEBBLE_DIR_URL.to_string(),
        contact_email: String::new(),
        domains: cross_domains.clone(),
        propagation_wait: Duration::ZERO,
        resolvability_poll_interval: Duration::ZERO,
        resolvability_deadline: Duration::ZERO,
        challenge_publish_names: Vec::new(),
    };
    let cross_issued = pure_obtain_certificate_dns01_with_http(
        &cross_cfg,
        Some(&issued.account_credentials),
        &seam,
        "pebble",
        &[],
        &zone,
        None,
        pure_pebble_http_client(&pebble.ca_pem),
    )
    .await
    .expect("pure driver: issuance reusing the NATIVE-created ACME account (D6 interop)");
    assert_eq!(
        cross_issued.account_credentials, issued.account_credentials,
        "the pure driver reuses the native account's credentials verbatim (no new-account churn)"
    );
    assert_pebble_issued(&cross_issued.cert_chain_pem, &cross_domains);

    // Phase 7 — pure, MANUAL two-phase (tier 3, S6): begin → simulate the admin
    // pasting the surfaced `_acme-challenge` TXT → complete. Proves the pure
    // suspend/resume split round-trips against the real CA (reusing the account).
    let pure_manual_domains = vec!["pure-manual.example.test".to_string()];
    let pure_manual_cfg = Dns01OrderConfig {
        directory_url: PEBBLE_DIR_URL.to_string(),
        contact_email: String::new(),
        domains: pure_manual_domains.clone(),
        propagation_wait: Duration::ZERO,
        resolvability_poll_interval: Duration::ZERO,
        resolvability_deadline: Duration::ZERO,
        challenge_publish_names: Vec::new(),
    };
    let pure_in_progress = pure_begin_dns01_order_with_http(
        &pure_manual_cfg,
        Some(&issued.account_credentials),
        pure_pebble_http_client(&pebble.ca_pem),
    )
    .await
    .expect("pure driver: begin the manual DNS-01 order against pebble");
    let pure_to_paste = pure_in_progress.challenges_to_publish();
    assert_eq!(
        pure_to_paste.len(),
        1,
        "one challenge for the single-SAN order"
    );
    assert_eq!(
        pure_to_paste[0].name,
        "_acme-challenge.pure-manual.example.test"
    );
    assert_eq!(pure_to_paste[0].record_type, "TXT");
    {
        let mut map = records.lock().unwrap();
        for rec in &pure_to_paste {
            map.entry(normalize(&rec.name))
                .or_default()
                .push(rec.value.clone());
        }
    }
    // `None` probe — the pure driver is the wasm twin, and the browser has no
    // raw-DNS probe today (the separately-queued wasm gap). This pins that the
    // probe-less manual path still completes exactly as before.
    let pure_manual_issued = pure_complete_dns01_order(
        pure_in_progress,
        Duration::ZERO,
        "pure-manual.example.test",
        None,
    )
    .await
    .expect("pure driver: complete the manual order after the admin pasted the TXT");
    assert_pebble_issued(&pure_manual_issued.cert_chain_pem, &pure_manual_domains);

    // Phase 8 — pure, CNAME-DELEGATED (S6b): the pure driver redirects the
    // `_acme-challenge` publish to the delegated target name and the CA follows the
    // admin's one-time CNAME — same `challenge_publish_names` redirect as the native
    // phase 4, driven by the pure order.
    let pure_delegated_domain = "pure-delegated.example.test".to_string();
    let pure_target_name = format!(
        "_acme-challenge.{pure_delegated_domain}.{}",
        controlled_zone.name
    );
    cnames.lock().unwrap().insert(
        normalize(&format!("_acme-challenge.{pure_delegated_domain}")),
        pure_target_name.clone(),
    );
    let pure_delegated_cfg = Dns01OrderConfig {
        directory_url: PEBBLE_DIR_URL.to_string(),
        contact_email: String::new(),
        domains: vec![pure_delegated_domain.clone()],
        propagation_wait: Duration::ZERO,
        resolvability_poll_interval: Duration::ZERO,
        resolvability_deadline: Duration::ZERO,
        challenge_publish_names: vec![(pure_delegated_domain.clone(), pure_target_name.clone())],
    };
    let pure_delegated_issued = pure_obtain_certificate_dns01_with_http(
        &pure_delegated_cfg,
        Some(&issued.account_credentials),
        &seam,
        "pebble",
        &[],
        &controlled_zone,
        None,
        pure_pebble_http_client(&pebble.ca_pem),
    )
    .await
    .expect("pure driver: complete the CNAME-delegated DNS-01 renewal against pebble");
    assert_pebble_issued(
        &pure_delegated_issued.cert_chain_pem,
        std::slice::from_ref(&pure_delegated_domain),
    );
}

// ── the FINDING C2 machine-level resume proof ────────────────────────────────

/// A [`DnsNest`] recording every `publish_cert` delivery; `list_records`/
/// `verify_records`/`cert_status` are unreached by the manual path and return
/// empty/default replies.
#[derive(Default)]
struct RecordingNest {
    published_certs: Mutex<Vec<PublishCertRequest>>,
}

#[async_trait]
impl DnsNest for RecordingNest {
    async fn list_records(&self, _domain: Option<String>) -> Result<Vec<DomainDns>, DnsNestError> {
        Ok(vec![])
    }
    async fn verify_records(
        &self,
        _domain: Option<String>,
    ) -> Result<Vec<DomainVerifyStatus>, DnsNestError> {
        Ok(vec![])
    }
    async fn publish_cert(&self, req: PublishCertRequest) -> Result<bool, DnsNestError> {
        self.published_certs.lock().unwrap().push(req);
        Ok(true)
    }
    async fn cert_status(&self, _domains: Vec<String>) -> Result<CertStatusReply, DnsNestError> {
        Ok(CertStatusReply {
            desired_sans: vec![],
            extra: Default::default(),
            statuses: vec![],
        })
    }
    async fn probe_txt_visible(
        &self,
        _zone_name: String,
        _record_name: String,
        _txt_value: String,
    ) -> Result<bool, DnsNestError> {
        // Native pebble flows probe in-process (`AuthoritativeNsProbe`, or the
        // seam's injected probe); the relayed seam is never reached here.
        unreachable!("the native pebble flows never relay the probe through the nest")
    }
}

/// A [`DnsProviderSeam`] the manual path never reaches (it has no covering
/// credential, by construction — S6) — panics if ever called, so a future
/// regression that routes manual issuance through a provider seam fails loudly
/// instead of silently returning empty data.
struct UnreachableProviderSeam;

#[async_trait]
impl DnsProviderSeam for UnreachableProviderSeam {
    async fn verify(
        &self,
        _provider_id: &str,
        _fields: &[(String, SecretString)],
    ) -> Result<Vec<DnsZoneRef>, DnsProviderError> {
        unreachable!("the manual DNS-01 path has no covering credential and never verifies one")
    }
    async fn publish(
        &self,
        _provider_id: &str,
        _fields: &[(String, SecretString)],
        _zone: &DnsZoneRef,
        _records: &[PublishRecord],
    ) -> Result<(), DnsProviderError> {
        unreachable!("the manual DNS-01 path publishes nothing — the admin pastes by hand")
    }
    async fn teardown(
        &self,
        _provider_id: &str,
        _fields: &[(String, SecretString)],
        _zone: &DnsZoneRef,
        _records: &[PublishRecord],
    ) -> Result<(), DnsProviderError> {
        unreachable!("the manual DNS-01 path tears down nothing — the admin owns the record")
    }
    async fn find_records(
        &self,
        _provider_id: &str,
        _fields: &[(String, SecretString)],
        _zone: &DnsZoneRef,
        _name: &str,
        _record_type: &str,
    ) -> Result<Vec<PublishRecord>, DnsProviderError> {
        unreachable!("the manual DNS-01 path never reads a provider's zone")
    }
}

/// The in-memory DNS record double, holding one record across the
/// drop+rebuild this test performs on [`DnsManagementMachine`] itself (a
/// clone shares the record).
type SharedConfigStore = fauna_client_dns::FakeDnsStore;

/// A real Ed25519 actor id (not an arbitrary 32 bytes) — `deliver_issued_cert`
/// converts it to its x25519 seal target, which requires a valid curve point.
fn valid_target_id() -> Vec<u8> {
    fauna_core::identity::ActorKeypair::from_secret([9u8; 32])
        .actor_id()
        .0
        .to_vec()
}

/// The manual DNS-01 issuance, phase 2's TDD owed test: everything CA-free was unit-tested when C2 landed, but
/// `resume_manual_issue`'s live re-open had no end-to-end run, because
/// [`DnsManagementMachine`] hardcoded [`Dns01OrderConfig::lets_encrypt`] and the
/// rest of this file drives `begin_dns01_order`/`complete_dns01_order` directly,
/// never the machine. [`fauna_client_dns::ManualOrderTestSeam`] closes that gap:
/// begin through a machine carrying it, DROP the machine (the process-local
/// [`fauna_client_dns::Dns01OrderInProgress`] does not survive a page
/// navigation or app restart — that is the whole C2 bug), REBUILD a fresh one
/// over the SAME config store, and complete.
///
/// **Measured against pebble, not assumed: a resume's re-opened order gets a
/// brand-new challenge, every time — pebble never reuses a pending
/// authorization across separate `new-order` calls, even for the identical
/// restored account.** That klaxon surfaced a real gap this test's first draft
/// exposed and this file's account-persistence fix (`persist_pending_manual_issue`)
/// then closed: `open_manual_order` reads the record's `acme_account`, but the
/// **begin** step never used to persist the account it had just created —
/// only a **successful completion** did — so a resume after a real drop always
/// built a fresh account too, and RFC 8555 §7.4 scopes authorization reuse to
/// the *same* account, so `resume_manual_issue`'s "the CA asks for the same
/// challenge, reuse it" fast path could not even get the chance to fire. Fixed
/// by persisting the account alongside the breadcrumb at **begin** time, so a
/// resume restores the identical account `open_manual_order` had already used
/// (verified below in `saved.acme_account`) — whether or not the specific
/// CA in front of it then chooses to hand back the same challenge is a CA
/// policy question this test cannot settle for every CA, so it exercises the
/// path this codebase can control and pebble reliably demonstrates: the
/// **mismatch branch**, unverified against a real CA until now. A resume that
/// gets a changed challenge surfaces the new value and keeps the freshly
/// re-opened order live in memory (`stash_manual_order`), so the **next**
/// completion — once the admin re-pastes — finds a live order and finalizes
/// directly, no further CA round-trip needed. That is exactly what actually
/// happened live on 2026-07-24 (attempt 1 lost the race, attempt 2 succeeded).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker + pulls ghcr.io/letsencrypt/pebble; run via `just e2e-pebble-dns01` or `cargo test -p fauna-client-dns --test pebble_dns01 -- --ignored"]
async fn manual_resume_survives_a_machine_rebuild_against_pebble() {
    ensure_docker();
    let _serial = PEBBLE_SERIAL.lock().await;

    let records: Records = Arc::new(Mutex::new(HashMap::new()));
    let cnames: Cnames = Arc::new(Mutex::new(HashMap::new()));
    let dns_addr = spawn_dns_responder(records.clone(), cnames.clone()).await;
    let pebble = Pebble::start("dns01", "resume", &dns_addr.to_string());

    let store: Arc<SharedConfigStore> = Arc::new(SharedConfigStore::default());
    let nest = Arc::new(RecordingNest::default());
    let target_nest_id = valid_target_id();
    let domain = "resume.example.test".to_string();

    let seam = || {
        let ca_pem = pebble.ca_pem.clone();
        ManualOrderTestSeam {
            directory_url: PEBBLE_DIR_URL.to_string(),
            http_client: Box::new(move || pebble_http_client(&ca_pem)),
            probe: None,
        }
    };

    // Phase 1: begin the manual order through a machine carrying the pebble seam
    // — the admin opened the DNS page and pressed "begin".
    let machine1 = DnsManagementMachine::with_credentials(
        nest.clone(),
        store.clone(),
        Arc::new(UnreachableProviderSeam),
        test_signing_key(),
    )
    .with_manual_order_test_seam(seam());

    machine1
        .dispatch(DnsAction::BeginManualIssueCert {
            domain: domain.clone(),
            target_nest_id: target_nest_id.clone(),
        })
        .await
        .expect("begin the manual DNS-01 order against pebble through the machine");

    let pending = machine1
        .snapshot()
        .pending_cert
        .expect("the paste card is up after begin");
    assert_eq!(pending.domain, domain);
    assert_eq!(
        pending.challenges.len(),
        1,
        "one challenge for the single-SAN manual order"
    );
    let txt_name = pending.challenges[0].name.clone();
    let first_txt_value = pending.challenges[0].expected.clone();
    assert_eq!(txt_name, format!("_acme-challenge.{domain}"));
    let account_after_begin = store
        .current()
        .acme_account
        .clone()
        .expect("begin persists the ACME account it just created, not just the breadcrumb");

    // Simulate the admin pasting the first TXT at their registrar.
    records
        .lock()
        .unwrap()
        .entry(normalize(&txt_name))
        .or_default()
        .push(first_txt_value);

    // Phase 2: DROP the machine — the live `Dns01OrderInProgress` is
    // process-local and does not survive a page navigation, an app restart, or
    // a move to another device (the exact C2 failure). REBUILD a fresh machine
    // over the SAME config store and attempt completion: `resume_manual_issue`
    // re-opens the order from the persisted breadcrumb, restoring the SAME
    // account (asserted above) — but pebble hands back a fresh challenge for
    // the re-opened order regardless, so this attempt must fail with the
    // documented "value changed, re-paste" error, not silently succeed or crash.
    drop(machine1);

    let machine2 = DnsManagementMachine::with_credentials(
        nest.clone(),
        store.clone(),
        Arc::new(UnreachableProviderSeam),
        test_signing_key(),
    )
    .with_manual_order_test_seam(seam());

    machine2
        .hydrate()
        .await
        .expect("hydrate the rebuilt machine");
    assert!(
        machine2.snapshot().pending_cert.is_some(),
        "the rebuilt machine re-surfaces the paste card from the persisted breadcrumb, \
         not an empty page"
    );

    let first_complete_err = machine2
        .dispatch(DnsAction::CompleteManualIssueCert)
        .await
        .expect_err(
            "pebble hands the re-opened order a fresh challenge, so the first post-rebuild \
             completion must report the documented mismatch rather than succeed on a stale \
             assumption",
        );
    assert!(
        first_complete_err
            .to_string()
            .contains("issued a new challenge value"),
        "got a different error than the documented mismatch path: {first_complete_err}"
    );

    let refreshed = machine2
        .snapshot()
        .pending_cert
        .expect("the mismatch keeps the paste card up, now showing the new value");
    assert_eq!(
        refreshed.challenges.len(),
        1,
        "still one challenge for the single-SAN order"
    );
    let second_txt_value = refreshed.challenges[0].expected.clone();
    assert_ne!(
        second_txt_value,
        records.lock().unwrap()[&normalize(&txt_name)][0],
        "pebble's re-opened order must genuinely differ from the first, or this test isn't \
         proving what it claims"
    );
    assert_eq!(
        store.current().acme_account.as_deref(),
        Some(account_after_begin.as_slice()),
        "the mismatch re-persists the SAME restored account, not a third fresh one"
    );

    // The admin updates the record to the new value the page now shows.
    records
        .lock()
        .unwrap()
        .get_mut(&normalize(&txt_name))
        .unwrap()[0] = second_txt_value;

    // The retry: `complete_manual_issue` now finds the LIVE order the mismatch
    // branch stashed in memory (`stash_manual_order`) and finalizes it directly
    // — no third CA round-trip, mirroring the live 2026-07-24 incident (attempt
    // 2 succeeded off the record attempt 1's failure had already surfaced).
    machine2
        .dispatch(DnsAction::CompleteManualIssueCert)
        .await
        .expect("complete against pebble with the corrected record");

    assert!(
        machine2.snapshot().pending_cert.is_none(),
        "a successful completion retires the paste card"
    );
    assert_eq!(
        nest.published_certs.lock().unwrap().len(),
        1,
        "the issued cert was sealed and delivered to the target nest exactly once"
    );

    let saved = store.current();
    assert!(
        saved.pending_manual_issue.is_none(),
        "a successful completion retires the persisted breadcrumb too — nothing left to resume"
    );
    assert!(
        saved.acme_account.is_some(),
        "the ACME account is persisted for a future renewal"
    );
}

// ── the 2026-07-23 live-failure reproduction ─────────────────────────────────

/// A [`DnsProviderSeam`] modeling the Hetzner Cloud DNS shape that broke the
/// live example.com renewal: `publish` **succeeds** — the provider's control
/// plane accepted the write (the 2xx `create_record` checks; the record shows
/// on an API GET, which `find_records` mirrors here) — but the authoritative
/// data plane the CA queries (the in-process responder) never serves it within
/// the order's lifetime. Measured live 2026-07-23: a fresh `_acme-challenge`
/// TXT took ≥15 min to reach Hetzner's authoritative NS, past any practical
/// propagation wait (15 s and 180 s both failed identically).
struct ControlPlaneOnlySeam {
    /// The control-plane view: what the provider API accepted and returns on a
    /// GET. The DNS responder never reads this map — that is the whole bug.
    accepted: Records,
    zone: DnsZoneRef,
    /// Names publish was called for, in order — proves the order's publish leg
    /// ran and succeeded even though the CA later found nothing.
    published_log: Arc<Mutex<Vec<String>>>,
    /// Names teardown was called for — proves the always-teardown choreography
    /// held on the failure path.
    teardown_log: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl DnsProviderSeam for ControlPlaneOnlySeam {
    async fn verify(
        &self,
        _provider_id: &str,
        _fields: &[(String, SecretString)],
    ) -> Result<Vec<DnsZoneRef>, DnsProviderError> {
        Ok(vec![self.zone.clone()])
    }

    async fn publish(
        &self,
        _provider_id: &str,
        _fields: &[(String, SecretString)],
        _zone: &DnsZoneRef,
        records: &[PublishRecord],
    ) -> Result<(), DnsProviderError> {
        let mut map = self.accepted.lock().unwrap();
        for record in records {
            self.published_log
                .lock()
                .unwrap()
                .push(normalize(&record.name));
            let values = map.entry(normalize(&record.name)).or_default();
            if !values.contains(&record.value) {
                values.push(record.value.clone());
            }
        }
        Ok(())
    }

    async fn teardown(
        &self,
        _provider_id: &str,
        _fields: &[(String, SecretString)],
        _zone: &DnsZoneRef,
        records: &[PublishRecord],
    ) -> Result<(), DnsProviderError> {
        let mut map = self.accepted.lock().unwrap();
        for record in records {
            self.teardown_log
                .lock()
                .unwrap()
                .push(normalize(&record.name));
            if let Some(values) = map.get_mut(&normalize(&record.name)) {
                values.retain(|value| value != &record.value);
            }
        }
        Ok(())
    }

    async fn find_records(
        &self,
        _provider_id: &str,
        _fields: &[(String, SecretString)],
        _zone: &DnsZoneRef,
        name: &str,
        record_type: &str,
    ) -> Result<Vec<PublishRecord>, DnsProviderError> {
        // Control-plane GET: faithfully reflects the accepted write — exactly
        // how the live incident's stray TXTs were visible via the API while
        // still unresolvable at the authoritative NS.
        let map = self.accepted.lock().unwrap();
        Ok(map
            .get(&normalize(name))
            .into_iter()
            .flatten()
            .map(|value| PublishRecord {
                name: name.to_string(),
                record_type: record_type.to_string(),
                value: value.clone(),
                ttl_seconds: 5,
                priority: None,
            })
            .collect())
    }
}

/// **Minimum reproduction of the 2026-07-23 live example.com renewal failure**: the provider accepts the
/// `_acme-challenge` publish (control plane) but its authoritative NS does not
/// serve the TXT by the time the CA validates → both authorizations fail →
/// the order goes `Invalid` → the driver surfaces the exact diagnostic the
/// live run logged ("order became invalid …; the CA could not validate the
/// DNS-01 authorization(s) for [example.com, mail.example.com]").
///
/// `propagation_wait: ZERO` stands for *any wait shorter than the provider's
/// data-plane serve lag* — the seam never serves the record, so the outcome is
/// identical for every finite wait, which is the live shape (15 s and 180 s
/// both failed) and keeps the test latency-independent (no timing knife-edge:
/// the record's absence is causal, not scheduled — testing.md point 14).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker + the pebble image; run via `just e2e-pebble-dns01` or `cargo test -p fauna-client-dns --test pebble_dns01 -- --ignored`"]
async fn dns01_order_goes_invalid_when_published_txt_is_not_yet_resolvable() {
    ensure_docker();
    let _serial = PEBBLE_SERIAL.lock().await;

    // The data plane pebble resolves against — never receives the TXT.
    let records: Records = Arc::new(Mutex::new(HashMap::new()));
    let cnames: Cnames = Arc::new(Mutex::new(HashMap::new()));
    let dns_addr = spawn_dns_responder(records.clone(), cnames.clone()).await;
    let pebble = Pebble::start("dns01", "invalid", &dns_addr.to_string());

    let zone = DnsZoneRef {
        id: "z-test".to_string(),
        name: "example.test".to_string(),
    };
    let seam = ControlPlaneOnlySeam {
        accepted: Arc::new(Mutex::new(HashMap::new())),
        zone: zone.clone(),
        published_log: Arc::new(Mutex::new(Vec::new())),
        teardown_log: Arc::new(Mutex::new(Vec::new())),
    };
    // Two SANs, mirroring the live order ([example.com, mail.example.com]) — both
    // authorizations fail together, as observed.
    let domains = vec![
        "home.example.test".to_string(),
        "mail.example.test".to_string(),
    ];
    let cfg = Dns01OrderConfig {
        directory_url: PEBBLE_DIR_URL.to_string(),
        contact_email: String::new(),
        domains: domains.clone(),
        propagation_wait: Duration::ZERO,
        resolvability_poll_interval: Duration::ZERO,
        resolvability_deadline: Duration::ZERO,
        challenge_publish_names: Vec::new(),
    };

    // (`Dns01Issued` has no `Debug` — it carries the private key — so no
    // `expect_err`.)
    let err = match obtain_certificate_dns01_with_http(
        &cfg,
        None,
        &seam,
        "pebble",
        &[],
        &zone,
        None,
        pebble_http_client(&pebble.ca_pem),
    )
    .await
    {
        Ok(_) => panic!("the order must fail: the CA validates before the TXT is resolvable"),
        Err(err) => err,
    };

    // The publish leg ran and succeeded for both SANs — the failure is NOT a
    // publish failure (matching the live incident, where `create_record` 2xx'd
    // and the strays were later API-visible).
    // (The CA returns authorizations in unspecified order — compare sorted.)
    let mut published = seam.published_log.lock().unwrap().clone();
    published.sort();
    assert_eq!(
        published,
        vec![
            "_acme-challenge.home.example.test".to_string(),
            "_acme-challenge.mail.example.test".to_string(),
        ],
        "both challenge TXTs were published (control-plane accepted)"
    );

    // The driver surfaced the CA-invalid diagnostic naming the failed DNS-01
    // identifiers — the exact error shape the 2026-07-23 live run logged.
    let msg = err.to_string();
    assert!(
        msg.contains("order became invalid"),
        "error names the order-invalid outcome; got: {msg}"
    );
    assert!(
        msg.contains("could not validate the DNS-01 authorization"),
        "error names the failed-validation cause; got: {msg}"
    );
    for domain in &domains {
        assert!(
            msg.contains(domain.as_str()),
            "error names the failed identifier {domain}; got: {msg}"
        );
    }

    // Always-teardown held on the failure path: both TXTs were torn down and
    // the control plane is clean (no stray `_acme-challenge` rows left).
    let mut torn_down = seam.teardown_log.lock().unwrap().clone();
    torn_down.sort();
    assert_eq!(
        torn_down, published,
        "every published challenge TXT was torn down after the failed order"
    );
    assert!(
        seam.accepted
            .lock()
            .unwrap()
            .values()
            .all(|values| values.is_empty()),
        "no stray _acme-challenge values remain at the provider"
    );
}
