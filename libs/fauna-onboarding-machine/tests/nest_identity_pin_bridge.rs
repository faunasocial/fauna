//! The E2E bridge's nest-identity (TOFU) pin seam.
//!
//! `docs/goal/behavior/onboarding.md` § E2E bridge contract;
//! `docs/goal/architecture/security.md` § Transport trust.
//!
//! A cross-app e2e must be able to seed a pin the nest cannot prove, so the
//! next launch's silent challenge routes to `LaunchPhase::IdentityChanged` instead
//! of auto-entering. The pin store is **process-global** and each app installs
//! its own backend at startup (`DiskPinStore` native, `LocalStoragePinStore` web),
//! so the bridge exposes ONE dispatcher arm that writes whatever store is
//! installed — no per-app harness code, and the test harness never learns
//! either backend's on-disk shape.
//!
//! These pin the two halves of that arm against a `MemoryPinStore`:
//!   * the setter reaches the installed store, keyed the way the *production*
//!     native path keys it (the nest URL's **authority** — not the whole URL), and
//!   * the reader hands the pin back as the JSON hex string the driver ships home.
//!
//! Native-only: the wasm twin writes `LocalStoragePinStore` (localStorage), which
//! has no native test double. It is covered by the web e2e journey.

use std::sync::Arc;

use fauna_anon_client::{MemoryPinStore, trust};
use fauna_onboarding_machine::nest_api::FakeNestApi;
use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{NestApi, OnboardingMachine, OnboardingObserver};

const NEST_URL: &str = "https://nest.example.com:8443/some/path";
/// The authority the production native path pins by (`trust::authority_of`) — the
/// key the bridge arm must land on, NOT the full URL.
const NEST_AUTHORITY: &str = "nest.example.com:8443";
/// A `nest_actor_id` no nest can ever prove possession of.
const BOGUS_PIN_HEX: &str = "abababababababababababababababababababababababababababababababab";

fn machine() -> Arc<OnboardingMachine> {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let fake = Arc::new(FakeNestApi::new());
    OnboardingMachine::with_nest_api(observer, fake as Arc<dyn NestApi>)
}

/// Install the `MemoryPinStore` **once for the whole binary**, not once per test.
///
/// Every case shares the one process-global trust state, and the discipline that
/// keeps them independent is that each works on a **distinct nest authority** —
/// which needs them to share one store, not to keep swapping it. The previous
/// shape called `trust::install_pin_store` per test, and that is a genuine race:
/// `install_pin_store` REPLACES the store (`*state().pins.write() = store`), so a
/// sibling installing between this test's write and its read-back left the reader
/// looking at a fresh, empty store.
///
/// Measured 2026-08-30 while arming `onboarding-machine-integration-test-check`
/// (the gate that first RUNS this directory): **1 failure in 400 runs** of the
/// unchanged binary — `nest_identity_pin_for_test_reads_back_what_the_setter_wrote`
/// asserting `Some("null")` against the hex it had just written. Rare enough that
/// no single run would ever show it, and frequent enough that a per-push gate
/// would eventually redden on nobody's change — which is worse than a dark
/// suite. `Once` removes the swap entirely; the distinct-authority rule below is
/// what still keeps the cases from seeing each other's pins.
fn fresh_pin_store() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| trust::install_pin_store(Arc::new(MemoryPinStore::new())));
}

#[test]
fn set_nest_identity_pin_for_test_writes_the_installed_store_keyed_by_authority() {
    fresh_pin_store();
    let m = machine();

    m.call_machine_method(
        "set_nest_identity_pin_for_test".into(),
        format!(r#"{{"nest_url":"{NEST_URL}","actor_id":"{BOGUS_PIN_HEX}"}}"#),
    );

    // Landed on the AUTHORITY, which is what the native launch path looks up
    // (`auth.rs` passes `authority_of(nest_url)` as the host). Keying by the whole
    // URL would silently seed a pin nothing ever reads — the test would then go
    // green having proven nothing.
    let pinned =
        trust::pinned_identity(NEST_AUTHORITY).expect("pin must reach the installed store");
    assert_eq!(hex::encode(pinned), BOGUS_PIN_HEX);
}

#[test]
fn nest_identity_pin_for_test_reads_back_what_the_setter_wrote() {
    fresh_pin_store();
    let m = machine();
    let url = "https://readback.example.com:9000";

    assert_eq!(
        m.call_machine_method_with_result(
            "nest_identity_pin_for_test".into(),
            format!(r#"{{"nest_url":"{url}"}}"#),
        )
        .as_deref(),
        Some("null"),
        "an unpinned nest must read back as JSON null, not an error or a blank"
    );

    m.call_machine_method(
        "set_nest_identity_pin_for_test".into(),
        format!(r#"{{"nest_url":"{url}","actor_id":"{BOGUS_PIN_HEX}"}}"#),
    );

    // The driver ships this string home verbatim, so it must be JSON — a *quoted*
    // hex string, not a bare one.
    assert_eq!(
        m.call_machine_method_with_result(
            "nest_identity_pin_for_test".into(),
            format!(r#"{{"nest_url":"{url}"}}"#),
        )
        .as_deref(),
        Some(format!("\"{BOGUS_PIN_HEX}\"").as_str()),
    );
}

#[test]
fn forgetting_the_pin_is_visible_through_the_reader() {
    fresh_pin_store();
    let m = machine();
    let url = "https://forget.example.com:7000";
    let authority = "forget.example.com:7000";

    m.call_machine_method(
        "set_nest_identity_pin_for_test".into(),
        format!(r#"{{"nest_url":"{url}","actor_id":"{BOGUS_PIN_HEX}"}}"#),
    );
    // What the "trust this nest" button does (`trust_nest_identity()` →
    // `forget_identity_pin`). The e2e asserts the pin is gone afterwards, so the
    // reader has to actually observe the removal.
    trust::forget_identity_pin(authority);

    assert_eq!(
        m.call_machine_method_with_result(
            "nest_identity_pin_for_test".into(),
            format!(r#"{{"nest_url":"{url}"}}"#),
        )
        .as_deref(),
        Some("null"),
        "a forgotten pin must read back as null — this is the assertion the \
         warn-and-recover journey ends on"
    );
}

#[test]
fn a_malformed_pin_arg_is_ignored_rather_than_panicking() {
    fresh_pin_store();
    let m = machine();

    // The bridge is forward-compatible: a bad arg must never take the app down
    // mid-test (it would look like a client crash, not a bad fixture).
    m.call_machine_method(
        "set_nest_identity_pin_for_test".into(),
        r#"{"nest_url":"https://bad.example.com","actor_id":"not-hex"}"#.into(),
    );
    m.call_machine_method("set_nest_identity_pin_for_test".into(), "{}".into());

    assert!(trust::pinned_identity("bad.example.com").is_none());
}
