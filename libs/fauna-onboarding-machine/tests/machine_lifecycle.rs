use fauna_onboarding_machine::{
    IdentityOrigin, OnboardingMachine, OnboardingObserver, OnboardingStep,
};
use fauna_provisioning::{Capability, PROVIDERS};
use std::sync::Arc;

#[cfg(feature = "test-observer")]
use fauna_onboarding_machine::observer::CountingObserver;
use fauna_onboarding_machine::observer::NullObserver;

fn test_machine() -> Arc<OnboardingMachine> {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    OnboardingMachine::new(observer)
}

#[test]
fn new_machine_starts_at_identity_choice() {
    let m = test_machine();
    assert_eq!(m.step(), OnboardingStep::IdentityChoice);
}

#[test]
fn reset_clears_state() {
    let m = test_machine();
    m.set_current_handle("alice@example.com".into());
    m.begin_create_identity();
    assert_eq!(m.step(), OnboardingStep::IdentityCreated);
    assert!(m.generated_secret().is_some());
    assert_eq!(m.current_handle(), "alice@example.com");

    m.reset();

    // Local state cleared.
    assert_eq!(m.step(), OnboardingStep::IdentityChoice);
    assert_eq!(m.current_handle(), "");
    assert!(m.generated_secret().is_none());
}

#[cfg(feature = "test-observer")]
#[test]
fn observer_notified_on_mutation() {
    let observer = CountingObserver::new();
    let observer_dyn: Arc<dyn fauna_onboarding_machine::OnboardingObserver> =
        Arc::clone(&observer) as _;

    let m = OnboardingMachine::new(observer_dyn);
    let before = observer.count();
    m.set_current_handle("alice@example.com".into());
    assert!(observer.count() > before);
}

// ── Task 7: identity stage ──────────────────────────────────────────────

#[test]
fn begin_create_identity_generates_secret_and_advances() {
    let m = test_machine();
    m.begin_create_identity();
    assert_eq!(m.step(), OnboardingStep::IdentityCreated);
    assert!(m.generated_secret().is_some());
    // Entry mints but does not commit: `identity_origin` is what
    // `canonical_secret` reads as "the screen the user committed on"
    // (onboarding.md § 1 Identity), and this screen's slot is full from the
    // moment it opens, so writing the origin at the door would make a curious
    // tap outrank an identity the user already confirmed elsewhere. The
    // create-side commit point is `confirm_generated_identity`; the back-out
    // this protects is pinned by
    // `identity_precedence.rs::abandoning_the_create_screen_keeps_the_imported_key`.
    assert_eq!(m.identity_origin(), None);

    m.confirm_generated_identity()
        .expect("generated identity confirms");
    assert_eq!(m.identity_origin(), Some(IdentityOrigin::Created));
}

#[test]
fn begin_import_identity_only_changes_step() {
    let m = test_machine();
    m.begin_import_identity();
    assert_eq!(m.step(), OnboardingStep::IdentityImport);
    assert!(m.generated_secret().is_none());
    assert_eq!(m.identity_origin(), Some(IdentityOrigin::Imported));
}

/// The launch flow's `superseded` affordance (`identity-succession.md`
/// § Propagation → *Own device fleet*) lands the user on the import screen and
/// must explain why. The reason has to be *machine* state: the per-app views
/// re-read `error_message()` on every observer tick, so a reason written to a
/// widget would be erased by the tick this very transition fires.
#[test]
fn begin_import_identity_with_reason_carries_the_reason_into_machine_state() {
    let m = test_machine();
    m.begin_import_identity_with_reason("this identity was succeeded".into());

    assert_eq!(m.step(), OnboardingStep::IdentityImport);
    assert_eq!(m.identity_origin(), Some(IdentityOrigin::Imported));
    assert_eq!(
        m.error_message().as_deref(),
        Some("this identity was succeeded"),
        "the reason must survive as machine state, not as a widget write"
    );
}

/// The reason-less twin must stay reason-less — otherwise a user who *chose* to
/// import would inherit whatever error the previous screen left behind.
#[test]
fn begin_import_identity_carries_no_reason() {
    let m = test_machine();
    m.begin_import_identity_with_reason("stale".into());
    m.clear_error();
    m.begin_import_identity();
    assert_eq!(m.error_message(), None);
}

#[test]
fn confirm_generated_advances_to_handle_entry() {
    let m = test_machine();
    m.begin_create_identity();
    m.confirm_generated_identity()
        .expect("confirm should succeed");
    assert_eq!(m.step(), OnboardingStep::HandleEntry);
}

#[test]
fn confirm_imported_validates_secret_format() {
    let m = test_machine();
    m.begin_import_identity();

    let valid = "0".repeat(64);
    m.confirm_imported_identity(valid)
        .expect("64-hex secret accepted");
    assert_eq!(m.step(), OnboardingStep::HandleEntry);

    let m = test_machine();
    m.begin_import_identity();
    let err = m.confirm_imported_identity("notvalid".into());
    assert!(err.is_err());
    assert_eq!(m.step(), OnboardingStep::IdentityImport);
}

// ── Task 8: handle entry stage ──────────────────────────────────────────

// TODO(task 11): submit_handle deleted — format validation now happens in
// start_handle_check. Add a test against start_handle_check when the
// Task 13 handle-check-continues wiring lands.

#[test]
fn handle_entry_back_routes_by_origin() {
    let m = test_machine();
    m.begin_import_identity();
    m.confirm_imported_identity("0".repeat(64)).unwrap();
    m.back();
    assert_eq!(m.step(), OnboardingStep::IdentityImport);

    let m = test_machine();
    m.begin_create_identity();
    m.confirm_generated_identity().unwrap();
    m.back();
    assert_eq!(m.step(), OnboardingStep::IdentityCreated);
}

// ── Task 9: nest select / connect ───────────────────────────────────────

// ── Task 10: DNS config stage ───────────────────────────────────────────

#[test]
fn dns_config_provider_selection_clears_creds_and_verified() {
    let m = test_machine();
    m.set_step_for_test(OnboardingStep::DnsConfig);
    m.set_dns_cred("api-token".into(), "stale".into());
    // Select Cloudflare (this test only cares that selecting clears stale creds).
    m.select_dns_provider("cloudflare".into());
    let dns = m.dns_config();
    assert_eq!(dns.selected_provider_id.as_deref(), Some("cloudflare"));
    assert!(dns.creds.is_empty());
    assert!(!dns.verified);
}

#[test]
fn visible_dns_fields_hetzner_single_cloud_token() {
    let m = test_machine();
    m.set_step_for_test(OnboardingStep::DnsConfig);
    m.select_dns_provider("hetzner".into());

    // Hetzner exposes ONE Cloud API token with kinds:[vps,dns]. Because it
    // is Dns-kinded it shows on the dns_config form regardless of the
    // same_provider_for_vps toggle; the legacy separate dns-api-token field
    // is gone (collapsed into api-token).
    let visible = m.visible_dns_fields();
    let ids: Vec<&str> = visible.iter().map(|f| f.id.as_str()).collect();
    assert!(ids.contains(&"api-token"));
    assert!(!ids.contains(&"dns-api-token"));

    // Still visible with same_provider_for_vps off (it's the DNS-kinded field).
    m.toggle_same_provider_for_vps(false);
    let visible = m.visible_dns_fields();
    let ids: Vec<&str> = visible.iter().map(|f| f.id.as_str()).collect();
    assert!(ids.contains(&"api-token"));
    assert!(!ids.contains(&"dns-api-token"));
}

#[test]
fn buy_domain_toggle_disables_non_registrar_providers() {
    // Hetzner is the DNS-capable provider with no registrar capability
    // (`capabilities: [dns, vps]`). This test used to name Cloudflare, which was
    // dns-only until a change made it the fourth in-wizard registrar — at which
    // point the assert started failing against *correct* behaviour. So the premise
    // is now checked rather than assumed: if Hetzner ever gains a registrar
    // capability this fails on the premise line, naming the real reason, instead of
    // looking like a deselect regression.
    const NO_REGISTRAR: &str = "hetzner";
    let p = PROVIDERS
        .iter()
        .find(|p| p.id.as_str() == NO_REGISTRAR)
        .expect("the registry still carries the provider this test is premised on");
    assert!(
        p.capabilities.contains(&Capability::Dns)
            && !p.capabilities.contains(&Capability::Registrar),
        "{NO_REGISTRAR} must still be DNS-capable and registrar-less for this test to mean \
         anything — if that changed, re-premise it on another registrar-less DNS provider"
    );

    let m = test_machine();
    m.set_step_for_test(OnboardingStep::DnsConfig);
    m.select_dns_provider(NO_REGISTRAR.into());
    m.toggle_buy_domain(true);
    // Auto-deselected: buying a domain in the wizard needs a provider that can
    // actually register one.
    let dns = m.dns_config();
    assert!(
        dns.selected_provider_id.is_none(),
        "{NO_REGISTRAR} should be deselected when buy_domain is on"
    );
}

/// The other half of the same rule, and the one that was silently unguarded while
/// the test above was red: a provider that **can** register keeps its selection.
#[test]
fn buy_domain_toggle_keeps_registrar_capable_providers() {
    const REGISTRAR: &str = "cloudflare";
    let p = PROVIDERS
        .iter()
        .find(|p| p.id.as_str() == REGISTRAR)
        .expect("the registry still carries the provider this test is premised on");
    assert!(
        p.capabilities.contains(&Capability::Registrar),
        "{REGISTRAR} must still be registrar-capable for this test to mean anything"
    );

    let m = test_machine();
    m.set_step_for_test(OnboardingStep::DnsConfig);
    m.select_dns_provider(REGISTRAR.into());
    m.toggle_buy_domain(true);
    let dns = m.dns_config();
    assert_eq!(
        dns.selected_provider_id.as_deref(),
        Some(REGISTRAR),
        "a registrar-capable provider must survive the buy-domain toggle"
    );
}

#[test]
fn dns_set_up_later_advances_to_vps_config() {
    let m = test_machine();
    m.set_step_for_test(OnboardingStep::DnsConfig);
    m.dns_set_up_later();
    assert!(m.dns_config().set_up_later);
    assert_eq!(m.step(), OnboardingStep::VpsConfig);
}

// ── Task 11: verify_dns + continue_from_dns ─────────────────────────────

#[tokio::test]
async fn verify_dns_with_no_provider_errors() {
    let m = test_machine();
    m.set_step_for_test(OnboardingStep::DnsConfig);
    let err = m.verify_dns().await;
    assert!(matches!(
        err,
        Err(fauna_onboarding_machine::OnboardingError::InvalidTransition { .. })
    ));
}

// ── Task 12: VPS config stage ───────────────────────────────────────────

#[test]
fn select_vps_provider_clears_creds() {
    let m = test_machine();
    m.set_step_for_test(OnboardingStep::VpsConfig);
    m.set_vps_cred("api-token".into(), "stale".into());
    m.select_vps_provider("hetzner".into());
    assert_eq!(
        m.vps_config().selected_provider_id.as_deref(),
        Some("hetzner")
    );
    assert!(m.vps_config().creds.is_empty());
}

#[tokio::test]
async fn verify_vps_with_no_provider_errors() {
    let m = test_machine();
    m.set_step_for_test(OnboardingStep::VpsConfig);
    let err = m.verify_vps().await;
    assert!(err.is_err());
}

// ── verifying flag is cleared on every return path ──────────────────────
//
// Cross-app verify-button gating: `can_verify_dns()` / `can_verify_vps()`
// return false while the corresponding `verifying` flag is true so the
// wizard's verify button stays disabled during the in-flight probe across
// all 7 apps (no per-platform in-flight tracking). The flag MUST be
// cleared on every return path (success and every error variant) or the
// button stays stuck disabled. These tests pin the early-return paths
// (the success path is exercised by `verify_dns_prefills_contact_from_gandi`
// in `tests/dns_config_state.rs`).

#[tokio::test]
async fn verify_dns_clears_verifying_on_no_provider_error() {
    let m = test_machine();
    m.set_step_for_test(OnboardingStep::DnsConfig);
    assert!(!m.dns_config().verifying, "starts not-verifying");
    let _ = m.verify_dns().await;
    assert!(
        !m.dns_config().verifying,
        "verifying must be cleared even after early-return error"
    );
}

#[tokio::test]
async fn verify_vps_clears_verifying_on_no_provider_error() {
    let m = test_machine();
    m.set_step_for_test(OnboardingStep::VpsConfig);
    assert!(!m.vps_config().verifying, "starts not-verifying");
    let _ = m.verify_vps().await;
    assert!(
        !m.vps_config().verifying,
        "verifying must be cleared even after early-return error"
    );
}

#[tokio::test]
async fn can_verify_dns_returns_false_when_verifying() {
    let m = test_machine();
    m.set_step_for_test(OnboardingStep::DnsConfig);
    m.select_dns_provider("cloudflare".into());
    m.set_dns_cred("api-token".into(), "filled".into());
    assert!(m.can_verify_dns(), "creds present → can verify");

    // Manually flip the in-flight flag (faster than driving a real probe)
    // to verify the can_verify_dns predicate honors it.
    m.set_dns_state_for_test(|s| s.verifying = true);
    assert!(
        !m.can_verify_dns(),
        "verifying=true must gate the verify-button predicate"
    );
}

#[tokio::test]
async fn can_verify_vps_returns_false_when_verifying() {
    let m = test_machine();
    m.set_step_for_test(OnboardingStep::VpsConfig);
    m.select_vps_provider("hetzner".into());
    m.set_vps_cred("api-token".into(), "filled".into());
    assert!(m.can_verify_vps(), "creds present → can verify");

    m.set_vps_state_for_test(|s| s.verifying = true);
    assert!(
        !m.can_verify_vps(),
        "verifying=true must gate the verify-button predicate"
    );
}

#[test]
fn select_vps_server_type_sets_id() {
    let m = test_machine();
    m.set_step_for_test(OnboardingStep::VpsConfig);
    m.select_vps_server_type("cx22".into());
    assert_eq!(
        m.vps_config().selected_server_type_id.as_deref(),
        Some("cx22")
    );
}

// ── Integration-test helper ─────────────────────────────────────────────

/// Minimal factory used by Flow A/B/C/D integration tests.
/// `identity`: if true, seeds a generated secret (advances wizard to HandleEntry).
/// `_has_nest` / `_has_pending`: retained for signature parity with the backup
/// branch; store-based pre-seeding is now explicit in each test (e.g. via
/// `seed_pending_invite`).
fn make(identity: bool, _has_nest: bool, _has_pending: bool) -> Arc<OnboardingMachine> {
    let m = OnboardingMachine::new(Arc::new(NullObserver));
    if identity {
        m.seed_identity("0".repeat(64));
    }
    m
}

#[tokio::test]
async fn flow_a_provision_new_domain() {
    use fauna_onboarding_machine::state::LocalizedText;
    use fauna_onboarding_machine::{HandleCheckOutcome, HandleCheckPhase, HandleCheckSnapshot};
    let m = make(true, false, false);

    // Fixture handle-check to DomainAvailable directly (bypassing network).
    m.set_step_for_test(OnboardingStep::HandleEntry);
    m.set_current_handle("alice@newnest.test-fauna-tld.example".into());
    m.set_handle_check_snapshot_for_test(HandleCheckSnapshot {
        phase: HandleCheckPhase::Complete,
        outcome: HandleCheckOutcome::DomainAvailable {
            buyable_via_provider: true,
            price: None,
        },
        message: LocalizedText {
            key: "x".into(),
            args: Default::default(),
        },
        continue_enabled: true,
        control_checkbox_visible: false,
        control_checkbox_checked: false,
    });

    // Submit Continue → expect DnsConfig with buy_domain=true.
    let next = m.submit_handle_check_continue().await;
    assert_eq!(next, OnboardingStep::DnsConfig);
    assert!(
        m.dns_config().buy_domain,
        "buy_domain should be true on the DomainAvailable path"
    );
    // Continue must ALSO persist domain_status so dns_config's buy-domain
    // checkbox is enabled (web `disabled={domainStatus !== 'Unregistered'}`)
    // and verify_dns/provider_status see the registration status
    // (onboarding.md §4). Previously domain_status was only ever set by the
    // test-only setter, leaving it None in production.
    assert_eq!(
        m.domain_status(),
        Some(fauna_provisioning::probe::DomainStatus::Unregistered),
        "DomainAvailable Continue must persist domain_status=Unregistered"
    );
}

/// A name with no delegation that sits inside a zone (`dev.example.com` under
/// `example.com`) lands on dns_config with `buy_domain` OFF: every DNS
/// provider stays selectable, so the admin can pick the one holding the
/// parent zone (`onboarding-provisioning.md` § 4).
#[tokio::test]
async fn domain_available_inside_a_zone_lands_with_buy_domain_off() {
    use fauna_onboarding_machine::state::LocalizedText;
    use fauna_onboarding_machine::{HandleCheckOutcome, HandleCheckPhase, HandleCheckSnapshot};
    let m = make(true, false, false);

    m.set_step_for_test(OnboardingStep::HandleEntry);
    m.set_current_handle("admin@dev.example.com".into());
    m.set_handle_check_snapshot_for_test(HandleCheckSnapshot {
        phase: HandleCheckPhase::Complete,
        outcome: HandleCheckOutcome::DomainAvailable {
            buyable_via_provider: true,
            price: None,
        },
        message: LocalizedText {
            key: "handle_check.outcome.domain_available_inside_zone".into(),
            args: Default::default(),
        },
        continue_enabled: true,
        control_checkbox_visible: false,
        control_checkbox_checked: false,
    });
    m.set_handle_enclosing_zone_for_test(Some("example.com".into()));

    let next = m.submit_handle_check_continue().await;
    assert_eq!(next, OnboardingStep::DnsConfig);
    assert!(
        !m.dns_config().buy_domain,
        "a name inside a zone must land with buy_domain off"
    );
    // The checkbox stays enabled (Unregistered) so a buyer of a name under a
    // multi-label suffix (`foo.co.uk`) can still tick it.
    assert_eq!(
        m.domain_status(),
        Some(fauna_provisioning::probe::DomainStatus::Unregistered),
    );
}

#[tokio::test]
async fn registered_no_nest_continue_sets_domain_status() {
    use fauna_onboarding_machine::state::LocalizedText;
    use fauna_onboarding_machine::{HandleCheckOutcome, HandleCheckPhase, HandleCheckSnapshot};
    let m = make(true, false, false);

    m.set_step_for_test(OnboardingStep::HandleEntry);
    m.set_current_handle("alice@owned.example".into());
    // RegisteredNoNest with the control checkbox checked routes to DnsConfig
    // with buy_domain=false (the user owns the domain, isn't buying it).
    m.set_handle_check_snapshot_for_test(HandleCheckSnapshot {
        phase: HandleCheckPhase::Complete,
        outcome: HandleCheckOutcome::RegisteredNoNest,
        message: LocalizedText {
            key: "x".into(),
            args: Default::default(),
        },
        continue_enabled: false,
        control_checkbox_visible: true,
        control_checkbox_checked: true,
    });

    let next = m.submit_handle_check_continue().await;
    assert_eq!(next, OnboardingStep::DnsConfig);
    assert!(
        !m.dns_config().buy_domain,
        "buy_domain should be false on the RegisteredNoNest (owned-domain) path"
    );
    assert_eq!(
        m.domain_status(),
        Some(fauna_provisioning::probe::DomainStatus::RegisteredNoNest),
        "RegisteredNoNest Continue must persist domain_status=RegisteredNoNest"
    );
}

#[tokio::test]
async fn flow_c_already_on_nest() {
    use fauna_onboarding_machine::WizardOutcome;
    use fauna_onboarding_machine::state::LocalizedText;
    use fauna_onboarding_machine::{HandleCheckOutcome, HandleCheckPhase, HandleCheckSnapshot};
    let m = make(true, false, false);

    m.set_step_for_test(OnboardingStep::HandleEntry);
    m.set_current_handle("alice@example.com".into());
    m.set_handle_check_snapshot_for_test(HandleCheckSnapshot {
        phase: HandleCheckPhase::Complete,
        outcome: HandleCheckOutcome::AlreadyOnNest {
            handle_matches: true,
            current_handle: Some("alice@example.com".into()),
        },
        message: LocalizedText {
            key: "x".into(),
            args: Default::default(),
        },
        continue_enabled: true,
        control_checkbox_visible: false,
        control_checkbox_checked: false,
    });

    let next = m.submit_handle_check_continue().await;
    assert_eq!(next, OnboardingStep::Done);
    let out = m.wizard_outcome().expect("outcome should be set");
    match out {
        WizardOutcome::LoggedIn { nest_url, handle } => {
            assert_eq!(nest_url, "https://example.com");
            assert_eq!(handle, "alice@example.com");
        }
        _ => panic!("expected LoggedIn"),
    }
}

/// Submitting a request leaves the wizard ON the `invite_request` page — that
/// page in `PendingReview` IS the "no nests, 1 pending invite" surface
/// (`onboarding.md` § The pending-invite surface). The poll advances it from
/// there; nothing about the submit itself exits.
///
/// Driven through the real submit rather than an injected snapshot: the fields
/// under test are exactly the ones the machine COMPUTES on that return, so
/// hand-authoring them asserts the fixture instead of the behavior — which is
/// how the old version kept asserting a `continue_enabled` it had set itself.
#[tokio::test]
async fn flow_b_invite_submit_stays_on_the_page() {
    use fauna_onboarding_machine::InviteRequestState;
    use fauna_onboarding_machine::nest_api::{FakeNestApi, InviteRequestResponse, NestApi};

    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let fake = Arc::new(FakeNestApi::new());
    let m = OnboardingMachine::with_nest_api(observer, fake.clone() as Arc<dyn NestApi>);
    m.seed_identity("0".repeat(64));
    m.set_step_for_test(OnboardingStep::InviteRequest);
    m.set_current_handle("alice@a.example".into());
    m.set_nest_url("https://a.example".into());

    // The wizard's `request_id` is the row id stringified, so the fixture's
    // numeric id is what the snapshot must carry back.
    fake.set_submit_invite_request_response(Ok(InviteRequestResponse {
        id: 4242,
        status: "pending".into(),
        denial_reason: None,
        quota: None,
    }));

    let step = m.wizard_submit_invite_request().await;

    assert_eq!(
        step,
        OnboardingStep::InviteRequest,
        "the submit itself never exits the wizard"
    );
    assert!(
        m.wizard_outcome().is_none(),
        "a pending review must publish no wizard outcome"
    );
    let snap = m.invite_request_snapshot();
    assert!(
        matches!(
            &snap.state,
            InviteRequestState::PendingReview { request_id, .. } if request_id == "4242"
        ),
        "expected PendingReview, got {:?}",
        snap.state
    );
    assert!(
        snap.recheck_visible,
        "the impatient user's manual recheck affordance stays"
    );
}

/// A submit the nest refuses as already registered (`fauna.account.actor_exists`
/// — a suspended key holder who took the sign-in-refused surface's "Use a
/// different nest" route, `login.md` § Errors) lands a TERMINAL error with its
/// own sentence. It used to fall into the catch-all `Transient` arm and read
/// "Try again", which is never true: only the admin's Restore lifts it.
#[tokio::test]
async fn invite_submit_already_registered_is_terminal_and_localized() {
    use fauna_onboarding_machine::nest_api::{FakeNestApi, InviteRequestError, NestApi};
    use fauna_onboarding_machine::{ErrorContext, InviteRequestState};

    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let fake = Arc::new(FakeNestApi::new());
    let m = OnboardingMachine::with_nest_api(observer, fake.clone() as Arc<dyn NestApi>);
    m.seed_identity("0".repeat(64));
    m.set_step_for_test(OnboardingStep::InviteRequest);
    m.set_current_handle("alice@a.example".into());
    m.set_nest_url("https://a.example".into());
    fake.set_submit_invite_request_response(Err(InviteRequestError::AlreadyRegistered));

    let step = m.wizard_submit_invite_request().await;

    assert_eq!(step, OnboardingStep::InviteRequest);
    let snap = m.invite_request_snapshot();
    match &snap.state {
        InviteRequestState::Error {
            transient,
            context,
            cause,
        } => {
            assert!(
                !transient,
                "an already-registered refusal never clears itself"
            );
            assert_eq!(*context, ErrorContext::Submitting);
            assert_eq!(
                cause,
                fauna_onboarding_machine::ALREADY_REGISTERED_CAUSE,
                "the cause is the sentinel, not the raw wire code"
            );
        }
        other => panic!("expected a terminal Error, got {other:?}"),
    }
    assert_eq!(
        snap.message.key, "onboarding.invite.error.already_registered",
        "the page renders the dedicated sentence, not `{{cause}}. Try again.`"
    );
    assert!(!snap.recheck_visible);
}

/// Relaunch onto a pending invite, then get approved — driven end to end
/// through the seam rather than by injecting the outcome.
///
/// This replaces the pre-2026-08-11 version, which hand-authored an
/// `InviteRequestState::Approved` snapshot and asserted it came back. That
/// pinned nothing: no live nest ever served `approved` (the admin approve
/// deletes the row), so the arm it "covered" was unreachable in production —
/// the variant survived precisely *because* a test kept injecting it
/// (`onboarding.md` § 3). The real signal is the row's ABSENCE plus a
/// registered-probe, which is what this now drives.
#[tokio::test]
async fn flow_d_resume_pending_review_then_admin_approves() {
    use fauna_onboarding_machine::nest_api::{
        FakeNestApi, InviteRequestError, NestApi, SilentChallengeOutcome,
    };
    use fauna_onboarding_machine::{InviteRequestState, WizardOutcome};

    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let fake = Arc::new(FakeNestApi::new());
    let m = OnboardingMachine::with_nest_api(observer, fake.clone() as Arc<dyn NestApi>);
    m.seed_identity("0".repeat(64));

    // Per-app launch glue would: read its long-term store, call
    // seed_identity(secret) + seed_pending_invite(...), then navigate UI.
    let pending_status = serde_json::json!({
        "PendingReview": { "request_id": "REQ1", "last_checked_ms": 0 }
    });
    m.seed_pending_invite(
        "https://a.example".into(),
        "alice@a.example".into(),
        "REQ1".into(),
        pending_status.to_string(),
    );
    assert_eq!(m.step(), OnboardingStep::InviteRequest);
    assert_eq!(m.current_handle(), "alice@a.example");
    assert!(matches!(
        m.invite_request_snapshot().state,
        InviteRequestState::PendingReview { .. }
    ));

    // The admin approves: the account is created and the request row deleted,
    // so the next poll finds nothing and the probe reports this actor is now
    // registered here.
    fake.set_recheck_invite_request_response(Err(InviteRequestError::NotFound));
    fake.set_silent_challenge_response(SilentChallengeOutcome::Success(
        fauna_protocol::auth::VerifyReply {
            token: "bearer".into(),
            token_id: "0".repeat(16),
            handle: "alice".into(),
            domain: "a.example".into(),
            tier: "free".into(),
            expires_at: 0,
            ..Default::default()
        },
    ));

    let step = m.recheck_invite_status().await;

    // The user lands in the app with no further action — no "Approved, press
    // Continue" interstitial (`admin.md` § Architectural rules 5).
    assert_eq!(step, OnboardingStep::Done);
    match m.wizard_outcome() {
        Some(WizardOutcome::LoggedIn { nest_url, handle }) => {
            assert_eq!(nest_url, "https://a.example");
            assert_eq!(handle, "alice@a.example");
        }
        other => panic!("expected LoggedIn, got {other:?}"),
    }
}
