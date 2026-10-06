//! Which identity the wizard authenticates with, when both slots are set.
//!
//! `docs/goal/behavior/onboarding.md` § Long-term store contract premises that
//! `effective_secret()` *is* the identity the wizard just authenticated with
//! ("The terminal reads the secret from the MACHINE, never from the store",
//! ratified 2026-08-27). That only holds if one rule decides which of
//! `generated_secret` / `imported_secret` is canonical and **every** wire call
//! reads it — the accessor the terminal reads and the accessor the silent
//! challenge authenticates with cannot disagree.
//!
//! Nothing clears the other slot: `begin_create_identity` mints `generated_secret`
//! (once — re-entry re-shows the same key), `begin_import_identity` +
//! `confirm_imported_identity` set `imported_secret`, and a user who visits both
//! screens in one session ends up with both. The rule is `identity_origin` — the
//! screen the user actually committed on decides — with the other slot as the
//! fallback for a screen entered but never confirmed, and the `seed_identity`
//! case (origin deliberately `None`, § 1 Identity) reading the imported slot it
//! seeds. Which is why *entering* a screen never writes the origin on the side
//! that mints: import writes it at the door and fills its slot at the confirm,
//! create fills its slot at the door and writes the origin at the confirm.
//!
//! These drive the REAL handle-check phases to the silent sign-in, so what is
//! asserted is the secret that reaches the wire — not just the accessor's return.
//! A wrong-but-valid key is invisible in the outcome (the nest simply answers
//! `NotRegistered`), which is why this reads `FakeNestApi::silent_challenge_secrets()`.

use std::collections::HashMap;
use std::sync::Arc;

use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{
    FakeNestApi, OnboardingMachine, OnboardingObserver, OnboardingStep,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const IMPORTED: &str = "aa11bb22cc33dd44ee55ff6600778899aa11bb22cc33dd44ee55ff6600778899";
const SEEDED: &str = "1122334455667788990011223344556677889900112233445566778899001122";

/// The one leg of the handle check still on HTTP; the silent challenge and
/// setup-status ride WS-RPC and are answered by the injected `FakeNestApi`
/// (same split as `handle_check_local_nest.rs`).
///
/// ⚠ **Not a member of the `mount_nest` byte-plane family** that
/// `fauna_sync_engine::test_support::MockNest` consolidated —
/// a name collision, nothing more. That family mocks the chunk/manifest plane
/// (`/chunks/check`, `/chunks`, `/manifests`); this mounts `GET /api/v1/health` and nothing
/// else. It needs no `fauna-sync-engine` dependency edge and must not grow one: this crate is
/// deliberately lean and wasm-clean.
async fn mount_nest(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/v1/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "ok"})))
        .mount(server)
        .await;
}

fn machine(fake: Arc<FakeNestApi>) -> Arc<OnboardingMachine> {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    OnboardingMachine::with_nest_api(observer, fake)
}

/// Run the real handle check against a local nest and return the secret the
/// silent sign-in authenticated with.
async fn authenticating_secret(m: &Arc<OnboardingMachine>, fake: &FakeNestApi) -> String {
    let server = MockServer::start().await;
    mount_nest(&server).await;
    let port = server.address().port();
    // Plain-HTTP wiremock behind the production same-box `nest` override, exactly
    // as `handle_check_local_nest.rs` does.
    m.set_provider_base_urls(HashMap::from([("nest".to_string(), server.uri())]));
    m.start_handle_check(format!("admin@127.0.0.1:{port}"))
        .await;

    let seen = fake.silent_challenge_secrets();
    assert_eq!(
        seen.len(),
        1,
        "the handle check must authenticate exactly once (got {seen:?})"
    );
    seen.into_iter().next().unwrap()
}

/// Create, back out, then import the real identity — the sequence a user takes
/// after tapping "create" to look around and then pasting the key they came
/// with. The imported key is the one they committed on, so it is the one the
/// nest must see; authenticating as the throwaway generated key makes a
/// registered user read as `NestRunningUserUnregistered`.
#[tokio::test]
async fn create_then_import_authenticates_with_the_imported_key() {
    let fake = Arc::new(FakeNestApi::new());
    let m = machine(fake.clone());

    m.begin_create_identity();
    let generated = m
        .confirm_generated_identity()
        .expect("generated identity confirms");

    // Back to the choice screen, then import — neither arm clears the other slot.
    m.begin_import_identity();
    m.confirm_imported_identity(IMPORTED.to_string())
        .expect("64-hex import confirms");

    assert_ne!(generated, IMPORTED, "fixture must distinguish the two keys");
    assert_eq!(
        m.effective_secret().as_deref(),
        Some(IMPORTED),
        "the accessor the wizard terminal reads must name the committed identity"
    );
    assert_eq!(
        authenticating_secret(&m, &fake).await,
        IMPORTED,
        "the silent challenge must authenticate with the identity the user imported, \
         not the throwaway generated key"
    );
}

/// The mirror: import, back out, then create. A fresh box claimed with a
/// brand-new identity is the case that genuinely wants generated-wins, and the
/// `identity_origin` rule keeps it — the create screen is what the user
/// committed on.
#[tokio::test]
async fn import_then_create_authenticates_with_the_generated_key() {
    let fake = Arc::new(FakeNestApi::new());
    let m = machine(fake.clone());

    m.begin_import_identity();
    m.confirm_imported_identity(IMPORTED.to_string())
        .expect("64-hex import confirms");

    m.begin_create_identity();
    let generated = m
        .confirm_generated_identity()
        .expect("generated identity confirms");

    assert_eq!(
        m.effective_secret().as_deref(),
        Some(generated.as_str()),
        "the accessor the wizard terminal reads must name the committed identity"
    );
    assert_eq!(
        authenticating_secret(&m, &fake).await,
        generated,
        "the silent challenge must authenticate with the freshly created identity"
    );
}

/// Entering the import screen and leaving without pasting anything must not
/// strand the wizard with no identity at all: origin says `Imported` but that
/// slot is empty, so the rule falls back to the key the user did commit.
#[tokio::test]
async fn abandoning_the_import_screen_falls_back_to_the_created_key() {
    let fake = Arc::new(FakeNestApi::new());
    let m = machine(fake.clone());

    m.begin_create_identity();
    let generated = m
        .confirm_generated_identity()
        .expect("generated identity confirms");

    // Enters `identity_import`, pastes nothing, goes back.
    m.begin_import_identity();

    assert_eq!(
        m.effective_secret().as_deref(),
        Some(generated.as_str()),
        "an unconfirmed import must not erase the identity the user has"
    );
    assert_eq!(
        authenticating_secret(&m, &fake).await,
        generated,
        "an unconfirmed import must not strand the handle check with an empty secret"
    );
}

/// The mirror of the test above, and the one the create screen owes: entering
/// `identity_created` and backing out without confirming must not displace the
/// key the user *did* commit. The create screen is the asymmetric one — its
/// entry mints into the slot, so unlike the import screen it can never present
/// as "origin says Created, slot is empty" and lean on the fallback. The origin
/// therefore has to be written at the commit, not at the door
/// (`onboarding.md` § 1 Identity).
///
/// Without that, this is a plain two-tap detour on the flow every new user
/// walks — import the real key, tap "create" to look around, go back — and it
/// authenticates as the throwaway, so a registered user is told they are not
/// registered, and the wizard terminal persists the throwaway as the
/// installation's long-term identity (§ Long-term store contract).
#[tokio::test]
async fn abandoning_the_create_screen_keeps_the_imported_key() {
    let fake = Arc::new(FakeNestApi::new());
    let m = machine(fake.clone());

    m.begin_import_identity();
    m.confirm_imported_identity(IMPORTED.to_string())
        .expect("64-hex import confirms");

    // Enters `identity_created`, writes nothing down, goes back.
    m.begin_create_identity();
    m.back();
    assert_eq!(
        m.step(),
        OnboardingStep::IdentityChoice,
        "backing out of the create screen returns to the choice screen"
    );

    assert_ne!(
        m.generated_secret().as_deref(),
        Some(IMPORTED),
        "fixture must distinguish the two keys"
    );
    assert_eq!(
        m.effective_secret().as_deref(),
        Some(IMPORTED),
        "an unconfirmed create must not erase the identity the user imported"
    );
    assert_eq!(
        authenticating_secret(&m, &fake).await,
        IMPORTED,
        "the silent challenge must authenticate with the imported identity, not the \
         throwaway the create screen minted on its way past"
    );
}

/// A secret the user was just told to write down must survive back-navigation.
/// `begin_create_identity` runs on every entry to `identity_created`, and the
/// screen renders whatever is in the slot, so an unconditional mint means a
/// user who wrote down visit 1's key and came back is looking at a different
/// one — confirm it and the written-down key is worthless.
///
/// The crate already holds this rule one function away:
/// `confirm_generated_identity` mints the recovery-kit root only when no
/// pending root exists, for exactly this reason. The identity secret — the
/// more consequential of the two — owes the same guard.
#[tokio::test]
async fn re_entering_the_create_screen_reshows_the_same_secret() {
    let m = machine(Arc::new(FakeNestApi::new()));

    m.begin_create_identity();
    let first = m.generated_secret().expect("entry mints a secret");
    m.back();
    m.begin_create_identity();

    assert_eq!(
        m.generated_secret().as_deref(),
        Some(first.as_str()),
        "re-entry must re-show the SAME key the user may have written down"
    );
}

/// `seed_identity` deliberately leaves `identity_origin` as `None` (§ 1 Identity:
/// so Back routes to `identity_choice`) and seeds only the imported slot — the
/// launch path the paid live-provisioning e2e takes. The rule's `None` arm must
/// name that slot.
#[tokio::test]
async fn a_seeded_identity_authenticates_with_the_seeded_key() {
    let fake = Arc::new(FakeNestApi::new());
    let m = machine(fake.clone());

    m.seed_identity(SEEDED.to_string());

    assert_eq!(
        m.identity_origin(),
        None,
        "seed_identity leaves origin unset"
    );
    assert_eq!(m.effective_secret().as_deref(), Some(SEEDED));
    assert_eq!(
        authenticating_secret(&m, &fake).await,
        SEEDED,
        "the seeded identity is the only one there is — it must reach the wire"
    );
}

/// The accessor the app reads at the terminal and the secret the wire leg
/// authenticates with are the same value in every reachable arm. This is the
/// invariant onboarding.md § Long-term store contract depends on; the defect it
/// pins is two accessors with opposite precedence.
#[tokio::test]
async fn the_terminal_accessor_and_the_wire_never_disagree() {
    for (label, build) in [
        ("create-then-import", 0u8),
        ("import-then-create", 1),
        ("import-only", 2),
        ("create-only", 3),
        ("seeded", 4),
    ] {
        let fake = Arc::new(FakeNestApi::new());
        let m = machine(fake.clone());
        match build {
            0 => {
                m.begin_create_identity();
                m.confirm_generated_identity().unwrap();
                m.begin_import_identity();
                m.confirm_imported_identity(IMPORTED.to_string()).unwrap();
            }
            1 => {
                m.begin_import_identity();
                m.confirm_imported_identity(IMPORTED.to_string()).unwrap();
                m.begin_create_identity();
                m.confirm_generated_identity().unwrap();
            }
            2 => {
                m.begin_import_identity();
                m.confirm_imported_identity(IMPORTED.to_string()).unwrap();
            }
            3 => {
                m.begin_create_identity();
                m.confirm_generated_identity().unwrap();
            }
            _ => m.seed_identity(SEEDED.to_string()),
        }

        let accessor = m.effective_secret().expect("an identity is committed");
        let wire = authenticating_secret(&m, &fake).await;
        assert_eq!(
            accessor, wire,
            "{label}: the terminal accessor and the authenticating secret must agree"
        );
    }
}
