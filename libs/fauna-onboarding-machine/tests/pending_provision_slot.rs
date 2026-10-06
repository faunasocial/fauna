//! The **pending-provision slot** — `docs/goal/behavior/onboarding.md` § 6 *The
//! pending-provision slot*.
//!
//! The claim code exists only in the client until the box is claimed, so a
//! client that dies between minting it and claiming would orphan a box nobody
//! can claim and a bill nobody can stop from the app. The discipline is CR-1's:
//! **custody precedes dispatch** — the slot is written *before* `create_server`,
//! and completed with the box's reach address the moment it returns.
//!
//! These drive the real `run_provisioning_inner` against a wiremock cloud, over
//! the deferred-DNS path (`provision_nest_no_dns`) — the cheapest path that
//! still exercises both write moments, and the path where the slot matters most,
//! since a deferred box is unreachable by domain for hours and the reach address
//! is the only way back to it — and, for the Retry family at the bottom, over
//! the standard path (`provision_with_snapshot` + the in-run claim against a
//! `FakeNestApi`), the one path where a failed run has a Retry to resume from.

use std::sync::{Arc, Mutex};

use fauna_launch_machine::{AwaitingDnsRecord, PendingProvisionStore};
use fauna_onboarding_machine::nest_api::{ClaimAdminError, ClaimAdminResponse};
use fauna_onboarding_machine::{FakeNestApi, OnboardingMachine, observer::CountingObserver};
use fauna_provisioning::progress::OverallStatus;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

// ── stores ───────────────────────────────────────────────────────────────

/// A store that takes the write, like a healthy platform keystore.
#[derive(Default)]
struct TakingStore(Mutex<Option<AwaitingDnsRecord>>);

impl TakingStore {
    fn row(&self) -> Option<AwaitingDnsRecord> {
        self.0.lock().unwrap().clone()
    }
}

impl PendingProvisionStore for TakingStore {
    fn save_awaiting_dns(
        &self,
        _secret_hex: String,
        record: AwaitingDnsRecord,
    ) -> Option<AwaitingDnsRecord> {
        *self.0.lock().unwrap() = Some(record.clone());
        Some(record)
    }

    fn clear_awaiting_dns(&self, _secret_hex: String) {
        *self.0.lock().unwrap() = None;
    }
}

/// A store that **swallows** the write and reports nothing back — the shape
/// every platform can really take (`SecretStore::set` is infallible by
/// signature; the wasm shim drops a `QuotaExceededError` on the floor). The
/// whole read-back contract exists to tell this apart from a durable write.
#[derive(Default)]
struct SwallowingStore;

impl PendingProvisionStore for SwallowingStore {
    fn save_awaiting_dns(
        &self,
        _secret_hex: String,
        _record: AwaitingDnsRecord,
    ) -> Option<AwaitingDnsRecord> {
        None
    }

    fn clear_awaiting_dns(&self, _secret_hex: String) {}
}

// ── harness ──────────────────────────────────────────────────────────────

/// Hetzner's create/read endpoints. `POST /servers` carries an `expect`, so the
/// count is asserted at `MockServer` drop — which is how "the box was never
/// built" becomes a real assertion rather than an absence nobody checks.
async fn mount_hetzner(server: &MockServer, expect_creates: u64) {
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
        .expect(expect_creates)
        .mount(server)
        .await;
}

fn machine(server: &MockServer) -> Arc<OnboardingMachine> {
    let urls: std::collections::HashMap<String, String> = [
        ("vps".to_string(), server.uri()),
        ("dns".to_string(), server.uri()),
        ("nest".to_string(), server.uri()),
    ]
    .into_iter()
    .collect();
    OnboardingMachine::new_with_provider_base_urls(CountingObserver::new(), Some(urls))
}

/// The deferred-DNS run: identity + handle + a selected VPS, "set up DNS later".
fn seed_deferred_run(m: &OnboardingMachine) {
    m.begin_create_identity();
    m.set_current_handle("alice@example.com".into());
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
    m.set_dns_state_for_test(|d| d.set_up_later = true);
}

/// Deadline poll on a named condition, with a budget sized far above any
/// non-pathological delay on a busy box (convention 14: assert
/// latency-independent state, never a fixed settle).
async fn wait_until(label: &str, mut cond: impl FnMut() -> bool) {
    for _ in 0..600 {
        if cond() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for: {label}");
}

async fn wait_for_provisioning_to_finish(m: &Arc<OnboardingMachine>) {
    wait_until("the provisioning run to leave Idle/Running", || {
        !matches!(
            m.provisioning_snapshot().overall,
            OverallStatus::Idle | OverallStatus::Running
        )
    })
    .await;
}

// ── tests ────────────────────────────────────────────────────────────────

/// **The ordering the whole slot rests on.** A store that swallows the write
/// must stop the run *before* a box exists — otherwise the client holds a claim
/// code for a real, billing box, and that code survives nowhere: precisely the
/// orphan the slot exists to prevent, reintroduced through the store's own
/// silence.
///
/// `POST /servers` is mounted with `.expect(0)`, so this asserts the ordering
/// rather than merely the failure: it is not enough that the run failed, the box
/// must never have been created.
#[tokio::test]
async fn a_swallowed_slot_write_stops_the_run_before_the_box_is_built() {
    let server = MockServer::start().await;
    mount_hetzner(&server, 0).await;

    let m = machine(&server);
    m.set_pending_provision_store_for_test(Arc::new(SwallowingStore));
    seed_deferred_run(&m);

    m.clone().start_provisioning();
    // The refusal happens before the run touches the provisioning snapshot, so
    // the observable is the wizard's own error, not a snapshot transition.
    wait_until("the wizard to refuse the run", || {
        m.error_message().is_some()
    })
    .await;

    assert!(
        m.provisioning_snapshot().result.is_none(),
        "the run must not have recorded a built box"
    );
    // The `.expect(0)` on POST /servers is verified at `server`'s drop, and it
    // IS the ordering assertion: the refusal landed before `create_server`.
    //
    // Deliberately no assertion on `overall` here. Which terminal status a
    // pre-orchestrator refusal leaves on the snapshot is internal bookkeeping
    // no goal doc fixes, and pinning a guess about it would make this test fail
    // for a reason that has nothing to do with the slot.
}

/// The happy path's two write moments, in one run: the mint lands the claim code
/// before the box exists, and `on_server_ready` completes the row with the
/// address the moment `create_server` returns.
///
/// The address is the half that cannot come from the snapshot — nothing
/// publishes the instance there until the whole run succeeds, and the crash
/// window the slot serves is *before* that — so a row carrying `5.6.7.8` is the
/// proof that the orchestrator's callback fired at the right moment.
#[tokio::test]
async fn a_run_writes_the_claim_code_first_and_completes_it_with_the_reach_address() {
    let server = MockServer::start().await;
    mount_hetzner(&server, 1).await;
    Mock::given(method("POST"))
        .and(path("/servers/42/actions/change_dns_ptr"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/servers/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "server": {"public_net": {"ipv4": {"dns_ptr": null}}}
        })))
        .mount(&server)
        .await;

    let store = Arc::new(TakingStore::default());
    let m = machine(&server);
    m.set_pending_provision_store_for_test(store.clone());
    seed_deferred_run(&m);

    m.clone().start_provisioning();
    wait_for_provisioning_to_finish(&m).await;

    let row = store.row().expect("the run must have written the slot");
    assert!(
        !row.claim_code.is_empty(),
        "the slot carries the code the box was built with — it exists nowhere else"
    );
    assert_eq!(
        row.reach_ipv4.as_deref(),
        Some("5.6.7.8"),
        "the slot must be completed with the box's reach address the moment \
         `create_server` returns; without it a resumed run has to wait for DNS"
    );
    assert_eq!(
        m.provision_reach_ipv4().as_deref(),
        Some("5.6.7.8"),
        "and the machine publishes the same address for the rest of the run"
    );
}

/// `reset()` — the wizard's "start over" — must **not** clear the slot
/// (`onboarding.md` § 6: *"only a completed claim or the surface's explicit exit
/// does"*). A re-provision of the same domain would otherwise race the "Almost
/// ready" surface for a box that still exists and still bills.
///
/// The slot lives in the account registry, which `reset()` cannot reach; what
/// this pins is that the machine's own *writer* survives too — a reset that
/// dropped it would leave the next run unable to write a slot at all, which is
/// the same loss by another door.
#[tokio::test]
async fn reset_keeps_both_the_slot_and_the_writer() {
    let server = MockServer::start().await;
    // Two runs: the one before the reset and the one after.
    mount_hetzner(&server, 2).await;
    Mock::given(method("POST"))
        .and(path("/servers/42/actions/change_dns_ptr"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/servers/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "server": {"public_net": {"ipv4": {"dns_ptr": null}}}
        })))
        .mount(&server)
        .await;

    let store = Arc::new(TakingStore::default());
    let m = machine(&server);
    m.set_pending_provision_store_for_test(store.clone());
    seed_deferred_run(&m);
    m.clone().start_provisioning();
    wait_for_provisioning_to_finish(&m).await;
    let before = store.row().expect("slot written");

    m.reset();

    assert_eq!(
        store.row().as_ref(),
        Some(&before),
        "\"start over\" must not clear a slot holding a live box's claim code"
    );

    // And the writer is still wired: a second run can still land a slot.
    seed_deferred_run(&m);
    m.clone().start_provisioning();
    wait_for_provisioning_to_finish(&m).await;
    assert!(
        store.row().is_some(),
        "reset() must not drop the machine's pending-provision writer"
    );
}

// ── Retry resumes the box it built ────────────────────────────────────────
//
// The orchestrator's Server step is a name-only pre-flight, so a re-run of the
// same domain finds the box an earlier run built and skips `create_server` —
// but that box boots with the EARLIER run's cloud-init. A re-run that minted a
// fresh claim code and seed could then neither verify the box's identity
// (`nest_actor_id is not the expected nest`, a hard-fail by design) nor claim
// it, and every Retry against an existing box failed deterministically. The tests below pin the fix: a re-run reuses the
// code and expects the identity the box was BUILT with — read straight out of
// the cloud-init the wiremock cloud received — and the slot never learns a code
// the box does not have.

/// Hetzner DNS for `example.com`: one zone, no existing rrsets, every create
/// accepted — the standard path's Domain + Dns steps.
async fn mount_hetzner_dns(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/zones"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "zones": [{"id": 1, "name": "example.com"}]
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/zones/1/rrsets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"rrsets": []})))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/zones/1/rrsets"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({})))
        .mount(server)
        .await;
}

/// The box's rDNS endpoints and its `/health` — the standard path's rDNS
/// pointer and Online steps. The health answers 200 at once: what a run finds
/// AFTER the box exists is the claim, which the fake nest API plays.
async fn mount_box_online(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/servers/42/actions/change_dns_ptr"))
        .respond_with(ResponseTemplate::new(201))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/servers/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "server": {"public_net": {"ipv4": {"dns_ptr": null}}}
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/health"))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
}

/// A Hetzner cloud where the FIRST `GET /servers` finds nothing and every later
/// one finds the box the first run created — the world as a Retry sees it.
/// `POST /servers` carries `.expect(1)`: the second run must NOT create.
async fn mount_hetzner_box_built_once(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"servers": []})))
        .up_to_n_times(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "servers": [{"id": 42, "public_net": {"ipv4": {"ip": "5.6.7.8"}}}]
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/servers"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_string(r#"{"server":{"id":42,"public_net":{"ipv4":{"ip":"5.6.7.8"}}}}"#),
        )
        .expect(1)
        .mount(server)
        .await;
}

/// The standard run: identity + handle + a selected VPS + Hetzner as the DNS
/// provider for a zone it already has (no domain purchase, DNS not deferred).
fn seed_standard_run(m: &OnboardingMachine) {
    seed_deferred_run(m);
    m.select_dns_provider("hetzner".into());
    m.set_dns_cred("api-token".into(), "test-token".into());
    m.set_dns_state_for_test(|d| {
        d.set_up_later = false;
        d.buy_domain = false;
    });
}

fn provider_urls(server: &MockServer) -> std::collections::HashMap<String, String> {
    [
        ("vps".to_string(), server.uri()),
        ("dns".to_string(), server.uri()),
        ("nest".to_string(), server.uri()),
    ]
    .into_iter()
    .collect()
}

/// What the box was BUILT with, read out of the cloud-init the wiremock cloud
/// received on `POST /servers`: `(claim code, nest_actor_id hex derived from
/// the injected seed)`. The strongest witness there is — not what the machine
/// says it sent, but what the provider got.
async fn built_with(server: &MockServer) -> (String, String) {
    let requests = server
        .received_requests()
        .await
        .expect("the mock server records requests");
    let create = requests
        .iter()
        .find(|r| r.method.as_str() == "POST" && r.url.path() == "/servers")
        .expect("exactly one create_server");
    let body: serde_json::Value = serde_json::from_slice(&create.body).expect("Hetzner JSON body");
    let user_data = body["user_data"].as_str().expect("cloud-init user_data");
    let code = user_data
        .lines()
        .find_map(|l| l.trim().strip_prefix("content: \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("the claim-code file's content line")
        .to_string();
    let seed_hex = user_data
        .lines()
        .find_map(|l| l.trim().strip_prefix("FAUNA_DEPLOYMENT_SEED: "))
        .expect("the injected deployment seed")
        .trim();
    let seed: [u8; 32] = hex::decode(seed_hex)
        .expect("seed hex")
        .try_into()
        .expect("32-byte seed");
    let id = ed25519_dalek::SigningKey::from_bytes(&seed)
        .verifying_key()
        .to_bytes();
    (code, hex::encode(id))
}

/// **The Retry button, end to end, against the real standard path.** Run 1
/// builds the box and fails at the claim (the box is still finishing its first
/// boot — the transient the button exists for); Retry finds the box, skips the
/// create, and must claim with the code the box was built with while expecting
/// the identity it was built with — both read out of the cloud-init the cloud
/// received, not out of the machine's own bookkeeping.
///
/// Before the fix every step of this test's second half was impossible: the
/// retry minted a fresh code and seed, held the fresh seed's identity, and the
/// first pre-identity dial of the old box refused with `nest_actor_id is not
/// the expected nest`.
#[tokio::test]
async fn a_retry_after_a_failed_claim_resumes_the_box_with_the_code_and_identity_it_was_built_with()
{
    let server = MockServer::start().await;
    mount_hetzner_box_built_once(&server).await;
    mount_hetzner_dns(&server).await;
    mount_box_online(&server).await;

    let fake = Arc::new(FakeNestApi::new());
    fake.set_claim_admin_response(Err(ClaimAdminError::Invalid {
        reason: "simulated: the box is still finishing its first boot".into(),
    }));
    let m = OnboardingMachine::with_nest_api_and_provider_base_urls(
        CountingObserver::new(),
        fake.clone(),
        Some(provider_urls(&server)),
    );
    let store = Arc::new(TakingStore::default());
    m.set_pending_provision_store_for_test(store.clone());
    seed_standard_run(&m);

    // Run 1: built, then the claim is refused.
    m.clone().start_provisioning();
    wait_for_provisioning_to_finish(&m).await;
    let snap = m.provisioning_snapshot();
    assert_eq!(snap.overall, OverallStatus::Failed, "{snap:?}");
    assert!(
        snap.overall.can_retry(),
        "Retry is the user's resume out of Failed"
    );

    let (built_code, built_id) = built_with(&server).await;
    let row = store.row().expect("the slot was written");
    assert_eq!(
        row.claim_code, built_code,
        "the slot carries the code the box was built with"
    );
    assert_eq!(
        row.nest_actor_id.as_deref(),
        Some(built_id.as_str()),
        "…and the identity it was built with"
    );
    assert_eq!(row.reach_ipv4.as_deref(), Some("5.6.7.8"));
    assert_eq!(fake.claim_admin_codes(), vec![built_code.clone()]);
    assert_eq!(
        m.first_contact_identity_held(),
        Some(("example.com".to_string(), built_id.clone())),
        "the held first-contact root is the built-with identity, keyed by the domain"
    );

    // Retry: the box answers its claim now.
    fake.set_claim_admin_response(Ok(ClaimAdminResponse {
        ok: true,
        token: Some("tok".into()),
        expires_at: Some(0),
        domain: None,
        deployment_seed: None,
    }));
    m.clone().retry_provisioning();
    wait_for_provisioning_to_finish(&m).await;
    let snap = m.provisioning_snapshot();
    assert_eq!(snap.overall, OverallStatus::Succeeded, "{snap:?}");

    assert_eq!(
        fake.claim_admin_codes(),
        vec![built_code.clone(), built_code.clone()],
        "the retry re-claims with the code the box was BUILT with, never a fresh one"
    );
    assert_eq!(
        m.first_contact_identity_held(),
        Some(("example.com".to_string(), built_id.clone())),
        "…and expects the identity the box was BUILT with, never a fresh seed's"
    );
    let row = store.row().expect("the slot is still there");
    assert_eq!(
        row.claim_code, built_code,
        "the retry never wrote a code the box does not have"
    );
    assert_eq!(row.nest_actor_id.as_deref(), Some(built_id.as_str()));
    assert_eq!(row.reach_ipv4.as_deref(), Some("5.6.7.8"));
    assert!(
        m.pending_provision_row().is_none(),
        "claimed: there is nothing left to resume, so a later provisioning of the \
         same domain is a new box"
    );
    // `POST /servers` `.expect(1)` is verified at `server`'s drop: the retry
    // found the box and did not build a second one.
}

/// "Start over" onto the same domain is the same resume by another door: the
/// slot survives `reset()` (§ 6 — the box still exists and still bills) and so
/// must the machine's memory of it, else the second run would mint a fresh code
/// and identity over a box that boots with the old ones. Deferred path, no
/// claim, so what is observable is the row and the held root.
#[tokio::test]
async fn a_start_over_onto_the_same_domain_resumes_the_box_rather_than_minting_over_it() {
    let server = MockServer::start().await;
    mount_hetzner_box_built_once(&server).await;

    let store = Arc::new(TakingStore::default());
    let m = machine(&server);
    m.set_pending_provision_store_for_test(store.clone());
    seed_deferred_run(&m);
    m.clone().start_provisioning();
    wait_for_provisioning_to_finish(&m).await;
    assert_eq!(m.provisioning_snapshot().overall, OverallStatus::Succeeded);

    let (built_code, built_id) = built_with(&server).await;
    let first = store.row().expect("slot written by the first run");
    assert_eq!(first.claim_code, built_code);
    assert_eq!(first.nest_actor_id.as_deref(), Some(built_id.as_str()));

    m.reset();
    seed_deferred_run(&m);
    m.clone().start_provisioning();
    wait_for_provisioning_to_finish(&m).await;
    assert_eq!(m.provisioning_snapshot().overall, OverallStatus::Succeeded);

    let second = store.row().expect("slot after the second run");
    assert_eq!(
        second.claim_code, built_code,
        "the second run resumed the box: same code, not a fresh one"
    );
    assert_eq!(second.nest_actor_id.as_deref(), Some(built_id.as_str()));
    assert_eq!(second.reach_ipv4.as_deref(), Some("5.6.7.8"));
    assert_eq!(
        m.first_contact_identity_held(),
        Some(("example.com".to_string(), built_id)),
    );
    assert_eq!(
        m.pending_provision_row(),
        Some(second),
        "the machine's copy mirrors the slot"
    );
}

/// The boundary of reuse: a same-name box this client never built (an
/// abandoned attempt's leftover, found by the name-only pre-flight on a fresh
/// machine) gets NO identity attributed to it — the slot records none, and the
/// held root stays this run's own fresh seed's, which that box cannot match, so
/// its first contact hard-fails instead of TOFU-claiming a stranger's box with a
/// code it does not have (the fake nest here does not verify identity, so the
/// observable is what the machine records, not the refusal itself).
#[tokio::test]
async fn a_found_box_this_client_never_built_is_attributed_no_identity() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "servers": [{"id": 42, "public_net": {"ipv4": {"ip": "5.6.7.8"}}}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/servers"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let store = Arc::new(TakingStore::default());
    let m = machine(&server);
    m.set_pending_provision_store_for_test(store.clone());
    seed_deferred_run(&m);
    m.clone().start_provisioning();
    wait_for_provisioning_to_finish(&m).await;

    let row = store
        .row()
        .expect("the slot was written before the pre-flight");
    assert_eq!(
        row.nest_actor_id, None,
        "a box this client did not build gets no identity attributed to it"
    );
    assert_eq!(row.reach_ipv4.as_deref(), Some("5.6.7.8"));
    let (host, held) = m
        .first_contact_identity_held()
        .expect("this run's own fresh root stays held");
    assert_eq!(host, "example.com");
    assert_eq!(
        m.pending_provision_row().and_then(|r| r.nest_actor_id),
        None,
        "and the machine's own memory attributes none either"
    );
    assert_ne!(held, String::new());
}

/// An app that wired no store (the per-app legs still owed) keeps the
/// in-session half: the machine's own memory of the box carries Retry and
/// "start over" alike, so those apps lose only the crash resume, never the
/// Retry button.
#[tokio::test]
async fn without_a_store_the_machine_still_resumes_the_box_it_built() {
    let server = MockServer::start().await;
    mount_hetzner_box_built_once(&server).await;

    let m = machine(&server);
    seed_deferred_run(&m);
    m.clone().start_provisioning();
    wait_for_provisioning_to_finish(&m).await;
    assert_eq!(m.provisioning_snapshot().overall, OverallStatus::Succeeded);
    let (built_code, built_id) = built_with(&server).await;
    let first = m
        .pending_provision_row()
        .expect("the machine remembers the box it built");
    assert_eq!(first.claim_code, built_code);
    assert_eq!(first.nest_actor_id.as_deref(), Some(built_id.as_str()));

    m.reset();
    seed_deferred_run(&m);
    m.clone().start_provisioning();
    wait_for_provisioning_to_finish(&m).await;
    assert_eq!(m.provisioning_snapshot().overall, OverallStatus::Succeeded);
    let second = m.pending_provision_row().expect("still remembered");
    assert_eq!(second.claim_code, built_code);
    assert_eq!(second.nest_actor_id.as_deref(), Some(built_id.as_str()));
    assert_eq!(
        m.first_contact_identity_held(),
        Some(("example.com".to_string(), built_id)),
    );
}

/// **Retry pressed AFTER the run's own claim already succeeded.** The family
/// above covers a retry out of `Failed`; this is the other door, and until now
/// nothing covered it: `forget_pending_provision_row` drops the row on
/// `ClaimOutcome::Claimed` on the reasoning that "a later provisioning of the
/// same domain is a NEW box" — but the orchestrator's Server step is a
/// name-only pre-flight, so the very next run *finds that same box* and skips
/// `create_server`. The two premises cannot both hold: the run treats the box
/// as new (fresh seed, fresh code) while the cloud hands it back the OLD one.
///
/// So the post-claim re-run held a fresh seed's identity as the first-contact
/// root and the first pre-identity dial refused the box it had itself just
/// claimed — `nest_actor_id is not the expected nest`, the correct refusal
/// aimed at the wrong box, which is the same defect
/// `a_retry_after_a_failed_claim_…` fixed one door earlier.
///
/// Measured live on windows against a real Hetzner box 2026-09-04: run 1 built,
/// claimed and reached `Succeeded`; the post-claim Retry witness then failed
/// `Online … last_error: "channel binding: nest_actor_id is not the expected
/// nest"` with `skip_reason: NestAlreadyOnline` and
/// `Server … Skipped(ServerAlreadyExists)` — the cloud confirming it was the
/// same box.
///
/// This matters beyond a test witness: Retry is a live button on the
/// provisioning page, and `nest/common.md` § Client-state recoverability
/// forbids a client-reachable state that only off-box surgery can undo. A press
/// that turns an owned, claimed box into one the client refuses to talk to is
/// exactly that.
#[tokio::test]
async fn a_retry_after_a_successful_claim_still_expects_the_box_it_claimed() {
    let server = MockServer::start().await;
    mount_hetzner_box_built_once(&server).await;
    mount_hetzner_dns(&server).await;
    mount_box_online(&server).await;

    let fake = Arc::new(FakeNestApi::new());
    fake.set_claim_admin_response(Ok(ClaimAdminResponse {
        ok: true,
        token: Some("tok".into()),
        expires_at: Some(0),
        domain: None,
        deployment_seed: None,
    }));
    let m = OnboardingMachine::with_nest_api_and_provider_base_urls(
        CountingObserver::new(),
        fake.clone(),
        Some(provider_urls(&server)),
    );
    let store = Arc::new(TakingStore::default());
    m.set_pending_provision_store_for_test(store.clone());
    seed_standard_run(&m);

    // Run 1: built and claimed — the state the user is looking at when the
    // Retry button is still on screen.
    m.clone().start_provisioning();
    wait_for_provisioning_to_finish(&m).await;
    let snap = m.provisioning_snapshot();
    assert_eq!(snap.overall, OverallStatus::Succeeded, "{snap:?}");
    let (_built_code, built_id) = built_with(&server).await;

    // Retry. The cloud hands back the SAME box (`POST /servers` is
    // `.expect(1)`, verified at drop), so whatever identity this run decides to
    // expect is an assertion about a box that already exists.
    m.clone().retry_provisioning();
    wait_for_provisioning_to_finish(&m).await;

    // What is asserted here is the EXPECTED IDENTITY, not the run's verdict.
    // `FakeNestApi::probe_setup_status` replays a configured answer and never
    // graduates a connection against the held root, so it cannot produce the
    // `ProbeError::IdentityMismatch` a real box's TLS handshake produced live —
    // a `Succeeded` here would pass with the bug fully present and discriminates
    // nothing. The held root and the retained row are where the two behaviours
    // genuinely differ, and both are exactly what the real refusal is computed
    // from.
    assert_eq!(
        m.first_contact_identity_held(),
        Some(("example.com".to_string(), built_id.clone())),
        "the post-claim retry expects a FRESH seed's identity against the box it \
         just claimed — the next pre-identity dial refuses its own box"
    );
    // …and the memory that makes that possible is identity ONLY. Custody stays
    // discharged: the row the claim dropped is not resurrected, so no claim code
    // outlives the claim it was minted for (`onboarding.md` § 6 *The
    // pending-provision slot*). Remembering *which box is here* and holding *a
    // code to claim it with* are different powers, and only the first is owed.
    assert!(
        m.pending_provision_row().is_none(),
        "the fix must remember the box's identity without resurrecting custody"
    );
    // `POST /servers` `.expect(1)` is verified at `server`'s drop: the retry
    // found the box it had claimed and did not build a second one — which is
    // what makes "a later provisioning of the same domain is a NEW box" false
    // as a premise, and this whole test necessary.
}

// ── The claim call site dials the run's own URL, not a later re-read ───────
//
// `run_provisioning_claim` itself is pinned
// (`run_provisioning_claim_dials_its_own_parameter_not_the_stale_state_field`,
// `machine.rs:9136-9185`) to dial the URL it's handed rather than
// `effective_nest_url()` — but that pin calls the function directly with
// hand-built parameters, never through `run_provisioning_inner`, so the
// actual production call site (`machine.rs:8047`:
// `machine.run_provisioning_claim(&url, &claim_code).await`) has no
// regression witness of its own: a future edit swapping `&url` for
// `&machine.effective_nest_url()` there would reproduce the same bug —
// every wizard-provisioned box's automatic claim silently dialing a
// stale/empty URL — and nothing in the crate would redden.

/// Drops the `"nest"` override and seeds `state.nest_url` to a WRONG address
/// the instant the Online step's health poll succeeds.
///
/// `nest_base` (what the run's own `url` derives from) is captured ONCE, at
/// the very top of `run_provisioning_inner`, before the Online poll ever
/// runs (`machine.rs:7567`) — so it is unaffected by anything this responder
/// does. `effective_nest_url()`, by contrast, is a LIVE read: it re-checks
/// the `"nest"` override at whatever moment it's called. A *static* fixture
/// (one override installed for the whole run) cannot tell the two shapes
/// apart — `url` and a same-moment `effective_nest_url()` read coincide by
/// construction, not because the call site is correct. Firing the swap from
/// inside the health-poll response — the last HTTP call the run makes
/// before reaching the claim — makes the divergence deterministic without a
/// sleep or a retry loop (`e2e-conventions.md` convention 14): the run
/// cannot reach the claim call site without this response completing first,
/// and `nest_base` was captured long before it (the Server/Domain/Dns steps
/// all precede Online).
///
/// Dropping the override (rather than replacing it with a different value)
/// reproduces production's actual shape: a production run installs no
/// `provider_base_urls` override at all, so `effective_nest_url()` always
/// falls through to `state.nest_url` — the field `stash_provisioning_result`
/// populates only after this function returns, exactly the shape the call
/// site's own doc comment (`machine.rs:4914-4921`) warns against.
struct SwapNestOverrideOnceHealthy {
    machine: Arc<OnboardingMachine>,
}

impl wiremock::Respond for SwapNestOverrideOnceHealthy {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        self.machine.set_nest_url(
            "https://a-stale-nest-url-effective-nest-url-must-not-read.example".into(),
        );
        self.machine
            .set_provider_base_urls(std::collections::HashMap::new());
        ResponseTemplate::new(200)
    }
}

/// **Definition of success:** a test exercising
/// `run_provisioning_inner`'s success path end-to-end asserts the claim step
/// actually dialed the run's own provisioned URL, under a setup where
/// `effective_nest_url()` would read a DIFFERENT (wrong) value from the
/// run's own provisioned address by the time the call site runs.
///
/// Red-verify: revert `machine.rs:8047` from
/// `machine.run_provisioning_claim(&url, &claim_code)` to
/// `machine.run_provisioning_claim(&machine.effective_nest_url(), &claim_code)`
/// — this test reddens on the `dialed` assertion below (the run still
/// reaches `Succeeded`, since `FakeNestApi` never validates the URL it's
/// handed; only the recorded URL differs), and it is the only test in the
/// crate that does.
#[tokio::test]
async fn run_provisioning_inners_claim_call_site_dials_the_runs_own_url_not_a_stale_override() {
    let server = MockServer::start().await;
    mount_hetzner_box_built_once(&server).await;
    mount_hetzner_dns(&server).await;
    // The box-online endpoints, except `/api/v1/health`: mirrors
    // `mount_box_online`, but the health mock below carries the swap.
    Mock::given(method("POST"))
        .and(path("/servers/42/actions/change_dns_ptr"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/servers/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "server": {"public_net": {"ipv4": {"dns_ptr": null}}}
        })))
        .mount(&server)
        .await;

    let fake = Arc::new(FakeNestApi::new());
    let m = OnboardingMachine::with_nest_api_and_provider_base_urls(
        CountingObserver::new(),
        fake.clone(),
        Some(provider_urls(&server)),
    );

    Mock::given(method("GET"))
        .and(path("/api/v1/health"))
        .respond_with(SwapNestOverrideOnceHealthy { machine: m.clone() })
        .mount(&server)
        .await;

    let store = Arc::new(TakingStore::default());
    m.set_pending_provision_store_for_test(store.clone());
    seed_standard_run(&m);

    m.clone().start_provisioning();
    wait_for_provisioning_to_finish(&m).await;

    let snap = m.provisioning_snapshot();
    assert_eq!(snap.overall, OverallStatus::Succeeded, "{snap:?}");

    let dialed: Vec<_> = fake
        .base_url_calls()
        .into_iter()
        .filter(|(method, _)| method == "probe_setup_status")
        .map(|(_, url)| url)
        .collect();
    assert_eq!(
        dialed,
        vec![server.uri()],
        "run_provisioning_inner's claim call site (machine.rs:8047) must dial \
         the run's own provisioned URL, never a later effective_nest_url() \
         re-read — the health-poll response already swapped the \"nest\" \
         override away and seeded state.nest_url to a wrong address, so a \
         regression to effective_nest_url() at the call site would dial that \
         wrong address instead"
    );
}
