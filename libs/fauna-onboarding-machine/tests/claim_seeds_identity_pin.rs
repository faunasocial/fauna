//! The claim ceremony seeds the durable nest-identity pin from the
//! possession-proven first-contact root — so the first post-claim dial (usually
//! over the reach hint, before the domain resolves) **verifies** against the
//! seeded pin instead of TOFU-ing whatever answers.
//!
//! `docs/goal/architecture/security.md` § Transport trust (the reach-hint
//! paragraph's first-contact residual, closed by this seeding);
//! `docs/goal/behavior/onboarding.md` § Reach hint.
//!
//! The wizard proves the box's identity *before* the claim — every pre-claim
//! dial is graduated against the held injected-seed / pasted-URI root
//! (`WsNestApi::core`, both arms) — and before this seam it then threw the
//! proof away: the root was in-memory and wizard-scoped, so the account left
//! the wizard having proved the box's identity and the first post-claim launch
//! re-established it from scratch by TOFU. These pin the closing rule: **claim
//! success with a held root writes that root through the installed
//! `NestIdentityPinStore`, keyed exactly as the launch path reads it** (the
//! nest URL's authority — native), authoritatively (a start-over onto a new
//! box on the same domain replaces the stale pin rather than wedging the first
//! launch on `IdentityChanged`), and writes **nothing** when no root is held
//! (bare-code claims stay TOFU, exactly as today).
//!
//! These cases are native-only, like `nest_identity_pin_bridge.rs`: the wasm
//! twin writes `LocalStoragePinStore` keyed by the nest URL verbatim (the key
//! `run_pinned_silent_challenge` is handed), through the same machine helper
//! (`seed_identity_pin_at_claim`). The wasm arm's own guard property — a
//! same-root re-seed must not erase a chain-accepted `rotation_seq`, a
//! differing root still overwrites authoritatively — is witnessed separately,
//! through a real browser claim rather than `FakeNestApi`:
//! `tests/e2e-unified/tests/test_web_claim_pin_wasm_witness.py`.
//!
//! The store is installed **once for the whole binary** and every case works on
//! a distinct nest authority — `install_pin_store` REPLACES the process-global
//! store, so a per-test install races a sibling's write/read-back (measured on
//! `nest_identity_pin_bridge.rs`: 1 failure in 400 runs).

use std::sync::{Arc, OnceLock};

use fauna_anon_client::{MemoryPinStore, NestIdentityPinStore, trust};
use fauna_core::claim_code::claim_uri;
use fauna_onboarding_machine::nest_api::{FakeNestApi, SetupStatus, SilentChallengeOutcome};
use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{
    DnsRecordPlain, OnboardingMachine, OnboardingObserver, OnboardingStep,
};

// The two identities below are built from a repeated byte and hex-encoded on
// demand, never written out as a 64-hex literal. That is not a style
// preference. A secret scanner cannot tell a 32-byte key from a 32-byte test
// fixture — both are just 64 hex characters — so the shape alone is what trips
// it, and the shape detector for this width carries no inline opt-out a fixture
// could claim. Deriving the hex from bytes removes the run from the source text
// entirely, which is why `fixture_secret_hex()` below already does the same
// thing, and why `hex32::encode` off a byte array is the fixture shape the other
// shared-Rust suites use.

/// The box identity the console/seed vouched for — the root a claim proves.
const ROOT_BYTE: u8 = 0xab;
/// A different identity: the stale pin of a prior box on the same domain.
const STALE_BYTE: u8 = 0xcd;

fn root() -> [u8; 32] {
    [ROOT_BYTE; 32]
}

fn root_hex() -> String {
    fauna_core::hex32::encode(&root())
}

fn stale() -> [u8; 32] {
    [STALE_BYTE; 32]
}

fn fixture_secret_hex() -> String {
    "01".repeat(32)
}

/// Install a `MemoryPinStore` once per binary and hand back the same handle, so
/// a test can pre-write a stale pin directly and assert through
/// `trust::pinned_identity` (which reads the installed store).
fn pin_store() -> &'static Arc<MemoryPinStore> {
    static STORE: OnceLock<Arc<MemoryPinStore>> = OnceLock::new();
    STORE.get_or_init(|| {
        let store = Arc::new(MemoryPinStore::new());
        trust::install_pin_store(store.clone());
        store
    })
}

/// A machine on the claim-code page for `domain`, with a valid identity and a
/// handle — everything `wizard_submit_claim_code` needs except the code.
fn machine_at_claim_code(domain: &str) -> (Arc<OnboardingMachine>, Arc<FakeNestApi>) {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let fake = Arc::new(FakeNestApi::new());
    let m = OnboardingMachine::with_nest_api(observer, fake.clone());
    m.seed_identity(fixture_secret_hex());
    m.set_current_handle(format!("alice@{domain}"));
    m.set_nest_url(format!("https://{domain}"));
    (m, fake)
}

/// A `fauna://claim` URI claim holds the console-vouched root, the claim
/// succeeds over a connection graduated against it — and the proof must now
/// outlive the wizard as the domain's durable pin, so the first post-claim
/// dial verifies instead of pinning a stranger.
#[tokio::test]
async fn a_uri_claim_with_a_held_root_seeds_the_domain_pin() {
    pin_store();
    let (m, _fake) = machine_at_claim_code("seed-one.test");

    let step = m
        .wizard_submit_claim_code(claim_uri("ABC123", &root_hex()))
        .await;
    assert_eq!(step, OnboardingStep::NatModeChoice);

    assert_eq!(
        trust::pinned_identity("seed-one.test"),
        Some(root()),
        "claim success with a held root must seed the domain pin — the \
         possession-proven first-contact root may not be discarded at wizard exit"
    );
}

/// A bare-code claim holds no root; nothing is provable, so nothing may be
/// pinned — the first launch stays TOFU exactly as today, never a pin minted
/// from an unproven connection.
#[tokio::test]
async fn a_bare_code_claim_with_no_root_pins_nothing() {
    pin_store();
    let (m, _fake) = machine_at_claim_code("seed-two.test");

    let step = m.wizard_submit_claim_code("ABC123".into()).await;
    assert_eq!(step, OnboardingStep::NatModeChoice);

    assert_eq!(
        trust::pinned_identity("seed-two.test"),
        None,
        "a claim with no held root must not invent a pin"
    );
}

/// "Start over onto a new box, same domain": the claim ceremony is the
/// sanctioned authoritative pin writer, so the new box's proven root REPLACES
/// the prior box's pin — a legitimate re-provision must not wedge the first
/// launch on an `IdentityChanged` the user cannot clear.
#[tokio::test]
async fn a_claim_overwrites_a_stale_pin_from_a_prior_box() {
    let store = pin_store();
    store.set("seed-three.test", stale());
    let (m, _fake) = machine_at_claim_code("seed-three.test");

    let step = m
        .wizard_submit_claim_code(claim_uri("ABC123", &root_hex()))
        .await;
    assert_eq!(step, OnboardingStep::NatModeChoice);

    assert_eq!(
        trust::pinned_identity("seed-three.test"),
        Some(root()),
        "the claim's possession-proven root must replace a prior box's stale pin"
    );
}

/// The recovery edge seeds too: the app crashed after the claim landed, the
/// resumed "Almost ready" poll finds `claimed: true` and confirms ownership by
/// the silent challenge — a claim success like any other, on a connection
/// graduated against the slot's held root, so the same seeding applies.
#[tokio::test]
async fn the_resumed_already_claimed_recovery_arm_seeds_the_pin() {
    pin_store();
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let fake = Arc::new(FakeNestApi::new());
    let m = OnboardingMachine::with_nest_api(observer, fake.clone());
    m.seed_identity(fixture_secret_hex());
    m.seed_awaiting_manual_dns_record(fauna_launch_machine::AwaitingDnsRecord {
        nest_url: "https://seed-four.test".into(),
        handle: "alice@seed-four.test".into(),
        dns_records_json: serde_json::to_string(&Vec::<DnsRecordPlain>::new()).unwrap(),
        claim_code: "CLAIM-XYZ".into(),
        reach_ipv4: None,
        nest_actor_id: Some(root_hex()),
    });
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: true,
        ..Default::default()
    }));
    fake.set_silent_challenge_response(SilentChallengeOutcome::Success(
        fauna_protocol::auth::VerifyReply {
            token: "bearer".into(),
            token_id: "0".repeat(16),
            handle: "alice".into(),
            domain: "seed-four.test".into(),
            tier: "free".into(),
            expires_at: 0,
            ..Default::default()
        },
    ));

    let step = m.recheck_manual_dns().await;
    assert_eq!(step, OnboardingStep::Done);

    assert_eq!(
        trust::pinned_identity("seed-four.test"),
        Some(root()),
        "the already-claimed-ours recovery arm is a claim success and must seed \
         the pin like the fresh-claim arm"
    );
}
