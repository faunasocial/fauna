//! An account index this build cannot read must reach the user as itself —
//! `docs/goal/behavior/onboarding.md` § App-launch routing, and
//! `docs/goal/architecture/version-compatibility.md` § 5 item 9 (the two
//! verdicts and their opposite remedies) / Dim 4 (a refusal is typed,
//! actionable and distinguishable from a transient one "so the client can
//! route the user correctly").
//!
//! Without this, both verdicts read as *no identity* — `secrets()` answers
//! nothing on an unreadable index, so `load_identity()` is `None` — and the
//! routing table's last row drops the user into **fresh onboarding**, with
//! their accounts sitting intact behind a blob this build merely cannot parse.
//!
//! Run with `cargo test -p fauna-launch-machine --features test-helpers,test-observer`.
#![cfg(all(feature = "test-helpers", feature = "test-observer"))]

use std::sync::Arc;

use fauna_launch_machine::{
    AccountIndexRefusal, InMemoryPersistence, LaunchMachine, LaunchPhase, LaunchWizardEntry,
    NullObserver,
};

#[tokio::test]
async fn a_newer_builds_index_is_a_terminal_update_surface_not_fresh_onboarding() {
    let persistence = Arc::new(InMemoryPersistence::new().with_account_index_refusal(
        AccountIndexRefusal::NewerBuild {
            index_v: 2,
            index_min: 2,
            bin_v: 1,
        },
    ));
    let machine = LaunchMachine::new(Arc::new(NullObserver), persistence);
    machine.start().await;

    let snap = machine.snapshot();
    assert_ne!(
        snap.phase,
        LaunchPhase::WizardAt {
            entry: LaunchWizardEntry::IdentityChoice
        },
        "the accounts are intact behind a blob this build cannot parse — \
         offering to make a new identity is the one thing that must not happen"
    );
    assert_eq!(
        snap.phase,
        LaunchPhase::Offline { transient: false },
        "terminal, never a retry loop: no amount of retrying reparses the blob"
    );
    assert_eq!(
        snap.account_index_refusal,
        Some(AccountIndexRefusal::NewerBuild {
            index_v: 2,
            index_min: 2,
            bin_v: 1,
        }),
        "the numbers reach the app as numbers it can render, not a string"
    );
    assert!(
        snap.last_error.is_some(),
        "an app that never reads the new field still shows the user something"
    );
}

#[tokio::test]
async fn a_malformed_index_is_terminal_too_and_says_so_distinctly() {
    let persistence = Arc::new(
        InMemoryPersistence::new().with_account_index_refusal(AccountIndexRefusal::Malformed),
    );
    let machine = LaunchMachine::new(Arc::new(NullObserver), persistence);
    machine.start().await;

    let snap = machine.snapshot();
    assert_eq!(snap.phase, LaunchPhase::Offline { transient: false });
    assert_eq!(
        snap.account_index_refusal,
        Some(AccountIndexRefusal::Malformed),
        "distinct from the version case: updating the app cannot help here, \
         so an app must be able to offer a different action"
    );
    assert!(snap.last_error.is_some());
}

#[tokio::test]
async fn the_refusal_outranks_every_other_launch_row() {
    // An identity and a nest url are both readable from the persistence seam
    // while the index itself is not. The refusal is still what the user is
    // shown: a silent challenge for one account would otherwise present a
    // single-account app on an install that has several.
    let persistence = Arc::new(
        InMemoryPersistence::new()
            .with_identity(vec![7u8; 32])
            .with_nest_url("https://nest.example")
            .with_account_index_refusal(AccountIndexRefusal::Malformed),
    );
    let machine = LaunchMachine::new(Arc::new(NullObserver), persistence);
    machine.start().await;

    assert_eq!(
        machine.snapshot().phase,
        LaunchPhase::Offline { transient: false },
        "checked before the silent-challenge row, like the factory-reset row"
    );
}

#[tokio::test]
async fn no_refusal_leaves_every_existing_route_untouched() {
    let persistence = Arc::new(InMemoryPersistence::new());
    let machine = LaunchMachine::new(Arc::new(NullObserver), persistence);
    machine.start().await;

    let snap = machine.snapshot();
    assert_eq!(
        snap.phase,
        LaunchPhase::WizardAt {
            entry: LaunchWizardEntry::IdentityChoice
        }
    );
    assert_eq!(snap.account_index_refusal, None);
}
