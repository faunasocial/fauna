//! A known e2e bridge method with a MALFORMED payload warns instead of
//! silently dropping the call.
//!
//! `docs/goal/architecture/e2e-conventions.md` convention 11 (a test agent
//! must not silently drop a command); `docs/goal/behavior/onboarding.md` §
//! E2E bridge contract.
//!
//! The silent-unknown-NAME rule stays (forward compatibility): this is about
//! a KNOWN name whose payload doesn't parse. Reproduces the exact shape that
//! burned two e2e run cycles 2026-08-30 — a bare string where the enum wants
//! a struct variant — and asserts the state a caller would actually observe
//! (`handle_check_snapshot()`) is untouched rather than silently
//! half-applied. Asserting the `tracing::warn!` itself fired would need a
//! subscriber-capture dev-dependency this crate does not otherwise carry;
//! the row's own success criterion allows the state-unchanged form as the
//! minimum bar.

use std::sync::Arc;

use fauna_onboarding_machine::nest_api::FakeNestApi;
use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::snapshots::HandleCheckOutcome;
use fauna_onboarding_machine::{NestApi, OnboardingMachine, OnboardingObserver};

fn machine() -> Arc<OnboardingMachine> {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let fake = Arc::new(FakeNestApi::new());
    OnboardingMachine::with_nest_api(observer, fake as Arc<dyn NestApi>)
}

#[test]
fn a_bare_string_outcome_leaves_the_snapshot_untouched() {
    let m = machine();
    assert_eq!(
        m.handle_check_snapshot().outcome,
        HandleCheckOutcome::None,
        "must start idle"
    );

    // The exact malformed shape from the item's own measured cost: a bare
    // string where `HandleCheckOutcome::DomainAvailable` wants a struct
    // variant (`{"buyable_via_provider": …, "price": …}`).
    m.call_machine_method(
        "set_handle_check_outcome_for_test".into(),
        "\"DomainAvailable\"".into(),
    );

    assert_eq!(
        m.handle_check_snapshot().outcome,
        HandleCheckOutcome::None,
        "a malformed payload for a KNOWN method must not corrupt state — it \
         must warn and drop, exactly as an unparseable payload always has, \
         not half-apply"
    );
}

#[test]
fn a_malformed_step_payload_does_not_panic_or_change_the_step() {
    let m = machine();
    let before = m.step();

    // Not a JSON-encoded `OnboardingStep` variant at all.
    m.call_machine_method("set_step_for_test".into(), "\"not-a-real-step\"".into());

    assert_eq!(
        m.step(),
        before,
        "a malformed step payload must not move the wizard"
    );
}
