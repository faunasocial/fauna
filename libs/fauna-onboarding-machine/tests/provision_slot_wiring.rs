//! Structural pin: **every production app builds the wizard with a
//! pending-provision store.**
//!
//! # Why this exists
//!
//! [`OnboardingMachine::new_with_persistence`] is a *second* constructor, not a
//! widened `new` — `new` has ~30 in-repo test call sites that want the plain
//! form, and widening it would make every one carry a `None` that says nothing.
//! The price of that choice is the failure mode this file removes: an app that
//! keeps calling bare `new` still compiles, still provisions, still passes its
//! own suite — and silently loses the crash resume, because the slot it never
//! wrote is the slot the relaunch cannot find
//! (`docs/goal/behavior/onboarding.md` § 6 *The pending-provision slot*).
//!
//! Nothing else catches that. The wizard's own tests construct their machines,
//! so they pin the test wiring, not the app's; the e2e journeys drive a machine
//! the harness built; and the user-visible symptom — a box built, billed, and
//! un-claimable after a crash — appears only on the one run nobody is watching.
//! So the check is made the way `fauna-client-accounts`'s `predecessor_actor_resolution.rs` makes its own:
//! read the **production source that ships**, so an edit on either side is red
//! immediately, with no regen step and no Swift/C#/Kotlin toolchain.
//!
//! # The two-sided ratchet
//!
//! [`WIRED`] apps must construct through the persistence-carrying form.
//! [`OWED`] apps must *not* — they are the per-app legs this change did not
//! reach (`onboarding.md` § 6; a new UI feature lands on tui first and reaches
//! the other apps in batches afterwards). Both directions are asserted, so
//! landing a leg fails here until its entry moves from `OWED` to `WIRED`: the
//! set can only shrink, and it shrinks deliberately.

use std::path::PathBuf;

/// `(app, production construction site, the call that carries the store)`.
const WIRED: &[(&str, &str, &str)] = &[
    (
        "tui",
        "apps/fauna-tui/src/wizard/mod.rs",
        "OnboardingMachine::new_with_persistence(",
    ),
    (
        "linux",
        "apps/fauna-linux/src/views/onboarding/machine_glue.rs",
        "OnboardingMachine::new_with_persistence(",
    ),
    (
        // Web's production constructor; its `test-helpers` twin next to it wires
        // the same store through `set_pending_provision_store_for_test`.
        "web",
        "libs/fauna-wasm-onboarding/src/lib.rs",
        "InnerMachine::new_with_persistence(",
    ),
    (
        // One file, both Apple targets — macOS and iOS share `OnboardingVM`.
        "macos+ios",
        "apps/fauna-apple/FaunaKit/Sources/FaunaKit/ViewModels/OnboardingVM.swift",
        "OnboardingMachine.newWithPersistence(",
    ),
    (
        // The store is the registry `OnboardingPage.xaml.cs` already injects into
        // the view model — windows mints no second `FfiAccountRegistry` for it.
        "windows",
        "apps/fauna-windows/FaunaApp/FaunaApp.Core/ViewModels/OnboardingViewModel.cs",
        "OnboardingMachine.NewWithPersistence(",
    ),
    (
        // The store comes off the `FfiAccountRegistry` `OnboardingHost` already
        // takes by constructor injection — android mints no second registry.
        "android",
        "apps/fauna-android/app/src/main/java/com/fauna/app/core/OnboardingHost.kt",
        "OnboardingMachine.newWithPersistence(",
    ),
];

/// The legs still owed. Delete a row here and add it to [`WIRED`] in the same
/// commit as the leg itself. Empty — all seven apps are wired.
const OWED: &[(&str, &str, &str)] = &[];

fn app_source(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "provision-slot pin: cannot read the production source {rel}: {e}\n\
             If the app moved this file, re-point the constant in \
             libs/fauna-onboarding-machine/tests/provision_slot_wiring.rs — do NOT delete \
             the check: it is the only thing standing between a refactor and a wizard \
             that builds boxes whose claim code does not survive a crash."
        )
    })
}

#[test]
fn provision_slot_is_wired_on_every_production_app() {
    for (app, file, call) in WIRED {
        assert!(
            app_source(file).contains(call),
            "{app} builds its onboarding machine without a pending-provision store.\n\
             \n\
             `{file}` no longer calls `{call}`. That still compiles and still \
             provisions — and silently drops the crash resume: a quit or crash \
             between minting the claim code and claiming the box leaves a box \
             built, billed, and claimable by nobody \
             (`docs/goal/behavior/onboarding.md` § 6 *The pending-provision slot*).\n\
             \n\
             Hand the constructor `<registry>.pending_provision_store()`; do not \
             relax this test."
        );
    }
}

#[test]
fn the_owed_app_legs_are_exactly_the_ones_still_owed() {
    for (app, file, call) in OWED {
        assert!(
            !app_source(file).contains(call),
            "{app} now wires the pending-provision store — good. Move its row from \
             OWED to WIRED in libs/fauna-onboarding-machine/tests/provision_slot_wiring.rs \
             (the ratchet only shrinks deliberately), and close the leg in that app's \
             own queue."
        );
    }
}
