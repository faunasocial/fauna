//! The `"nest"` base-URL override reaches the **launch-side** dial seam.
//!
//! `docs/goal/behavior/onboarding.md` § Implementation status today — *§ 3b's
//! derived-ON branch is e2e-proven locally*, first bullet.
//!
//! The harness installs one map (`set_provider_base_urls`, or
//! `new_with_provider_base_urls` on web) and expects it to redirect **both**
//! halves of a claim: the pre-identity calls this machine makes itself, and the
//! authenticated session an app establishes afterwards from the *store*, where
//! no onboarding machine is in scope to ask. The second half lives in
//! `fauna_launch_machine::dial`, so these two objects have to be kept in step by
//! a mirror — and a mirror is exactly the kind of wiring that rots silently.
//!
//! **The clear is pinned as hard as the install.** The override outlives the
//! wizard that installed it: the `app` fixture resets between tests without
//! relaunching, so a `set_provider_base_urls({})` teardown that failed to reach
//! the dial seam would leave every later test's launch dialing a torn-down
//! fixture nest — with the *pre-identity* map correctly cleared, so the symptom
//! would look nothing like its cause. That split brain is why the mirror is not
//! gated more narrowly than the setter that feeds it.
//!
//! Tiered down deliberately (`e2e-conventions.md` convention 14): the e2e can
//! only observe this wiring indirectly, as a later test in the same module
//! failing for an unrelated-looking reason. Here it is one assertion naming the
//! mechanism.

use std::collections::HashMap;
use std::sync::Arc;

use fauna_onboarding_machine::nest_api::FakeNestApi;
use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{NestApi, OnboardingMachine, OnboardingObserver};

const HARNESS: &str = "http://127.0.0.1:8099";

fn machine() -> Arc<OnboardingMachine> {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let fake = Arc::new(FakeNestApi::new());
    OnboardingMachine::with_nest_api(observer, fake as Arc<dyn NestApi>)
}

fn map(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// One test, not several: the dial override is process-global, so separate
/// `#[test]` functions in this binary would race and the verdict would depend on
/// the scheduler. The sequence below is the order the harness itself drives —
/// install at fixture setup, clear at teardown.
#[test]
fn set_provider_base_urls_installs_and_clears_the_launch_side_dial_override() {
    let m = machine();

    m.set_provider_base_urls(map(&[("nest", HARNESS), ("dns", "http://127.0.0.1:8100")]));
    assert_eq!(
        fauna_launch_machine::nest_dial_override(),
        Some(HARNESS.to_string()),
        "installing the map must mirror its `nest` entry into the launch-side \
         dial seam — without it the post-LoggedIn session dials the literal \
         typed URL and a domain-shaped claim can never connect"
    );

    // The teardown gesture: an empty map is the clear.
    m.set_provider_base_urls(HashMap::new());
    assert_eq!(
        fauna_launch_machine::nest_dial_override(),
        None,
        "clearing the map must clear the dial override too. A stale one points \
         every later launch at a torn-down fixture nest, while the pre-identity \
         map reads correctly cleared — a split brain no symptom would explain"
    );

    // Web's channel is the constructor, not the setter (the SPA rebuilds the
    // machine on reload), so it needs its own mirror or web is the one app
    // whose store-read dial never resolves.
    let _web = OnboardingMachine::new_with_provider_base_urls(
        Arc::new(NullObserver),
        Some(map(&[("nest", HARNESS)])),
    );
    assert_eq!(
        fauna_launch_machine::nest_dial_override(),
        Some(HARNESS.to_string()),
        "the construction-time channel must mirror too — it is the only one web \
         uses"
    );

    // ── The read-back half: a machine minted AFTER the install ─────────────
    //
    // The mirror above is only half a seam. A native app that rebuilds its
    // wizard mints a **fresh** `OnboardingMachine` — linux does exactly this on
    // every `driver.reset()` (`apps/fauna-linux/src/main.rs`'s "reset" arm →
    // `views::onboarding::build_onboarding_window` → `machine_glue::make_machine`
    // → `OnboardingMachine::new`, whose map starts empty). The harness installs
    // the map ONCE per fixture, so after such a reset the pre-identity half went
    // dark while the mirrored launch-side override stayed installed: the exact
    // split brain this file's header forbids, running in the other direction.
    //
    // Measured cost of leaving it unpinned: the surviving
    // case, `test_a_returning_admin_sign_in_issues_no_deployment_enable[linux]`.
    // Its leg 2 resets mid-body, so the returning admin's handle check probed
    // the unresolvable `https://fauna.test` instead of the fixture nest, the
    // outcome never reached `AlreadyOnNest`, Continue never enabled, and the
    // wizard sat on `handle_entry` until the 120s poll gave up — with no error,
    // no log line, and nothing happening app-side to find. A whole live-debugging
    // session was spent looking for a hang that was never there.
    //
    // `OnboardingMachine::new`, not this file's `machine()` helper: the read-back
    // is deliberately confined to the production constructors, because a machine
    // handed its OWN `NestApi` shares no nest with whatever installed the global
    // and must not inherit it (`machine.rs`'s `inherits_dial_override` field).
    // `new` is exactly what `build_onboarding_window` reaches — through
    // `new_with_persistence`, the same `build` — so constructing it that way here
    // makes this assertion a closer model of the case it is pinning, not a weaker
    // one.
    let after_reset = OnboardingMachine::new(Arc::new(NullObserver));
    assert_eq!(
        after_reset.provider_base_url("nest".into()),
        Some(HARNESS.to_string()),
        "a machine minted AFTER the install must still resolve the `nest` \
         override: the harness installs the map once per fixture, but an app \
         that rebuilds its wizard mints a fresh machine, and the pre-identity \
         probe must not go dark while the mirrored launch-side override is \
         still pointing at the fixture nest"
    );

    fauna_launch_machine::set_nest_dial_override(None);

    // ...and the clear reaches the fallback too, or the read-back would resurrect
    // a torn-down fixture nest for every later test — the very leak the teardown
    // assertion above exists to prevent.
    assert_eq!(
        OnboardingMachine::new(Arc::new(NullObserver)).provider_base_url("nest".into()),
        None,
        "clearing the dial override must clear what a freshly minted machine \
         reads back, or the teardown gesture leaks across tests"
    );

    // ── Who does NOT inherit: a machine handed its own transport ───────────
    //
    // The read-back above is a process-global read, so without a bound it
    // reaches every `OnboardingMachine` in the binary — including the ones unit
    // tests mint with a `FakeNestApi`, which share no nest with whatever
    // installed the override. Those machines must read `None`.
    //
    // Measured cost of leaving THIS half unpinned (2026-08-30): one test in
    // `recovery_entry_ceremony.rs` installed a `"nest"` override, and the other
    // 20 in that binary — each with its own fresh machine and its own fake nest
    // — read it back and asserted against the leaked `http://127.0.0.1:9123`.
    // 8 of 21 failed, and it was a RACE, not a constant: 2 runs in 40 of the
    // same unchanged binary on the same box. So it could not have been found by
    // running the suite once, and `--test-threads=1` passed every time — the
    // configuration that hides the class (`e2e-conventions.md` convention 10).
    // Nothing reported any of it, because no gate ran this crate's `tests/`
    // directory at all until `onboarding-machine-integration-test-check` landed
    // with this assertion.
    //
    // This is the deterministic witness that flaky suite could never be: one
    // install, one read, no scheduler involved.
    fauna_launch_machine::set_nest_dial_override(Some(HARNESS.to_string()));
    assert_eq!(
        machine().provider_base_url("nest".into()),
        None,
        "a machine handed its OWN NestApi must NOT inherit the process-global \
         dial override: it shares no nest with whatever installed one, and the \
         global's blast radius is the whole process, so inheriting turns one \
         test's override into every sibling test's wrong answer"
    );

    // Leave the process as we found it: a later addition to this file (or a
    // `new`-built machine in it) would otherwise read a stale override.
    fauna_launch_machine::set_nest_dial_override(None);
}
