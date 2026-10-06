//! Lifecycle tests for `FolderWizardMachine`. Mirrors
//! `fauna-onboarding-machine/tests/machine_lifecycle.rs`: navigation, gating,
//! and `submit()` wire-shape assertions via the fake nest API.

use std::sync::Arc;

use fauna_folders_machine::nest_api::{FakeCall, FakeFolderNestApi, FolderApiError};
use fauna_folders_machine::observer::{CountingObserver, NullObserver};
use fauna_folders_machine::{
    DeviceOption, FolderWizardMachine, FolderWizardObserver, FolderWizardStep, SubmitPhase,
};
use fauna_protocol::folders::PlaceFlags;

fn devices() -> Vec<DeviceOption> {
    vec![
        DeviceOption {
            device_id: "aa".repeat(32),
            label: "laptop".into(),
        },
        DeviceOption {
            device_id: "bb".repeat(32),
            label: "phone".into(),
        },
    ]
}

fn machine_with(fake: Arc<FakeFolderNestApi>) -> Arc<FolderWizardMachine> {
    let obs: Arc<dyn FolderWizardObserver> = Arc::new(NullObserver);
    FolderWizardMachine::new(obs, devices(), fake)
}

fn machine() -> Arc<FolderWizardMachine> {
    machine_with(Arc::new(FakeFolderNestApi::new()))
}

// ── Construction + defaults ────────────────────────────────────────────────

#[test]
fn new_wizard_starts_at_name_with_web_defaults() {
    let m = machine();
    assert_eq!(m.step(), FolderWizardStep::Name);
    let nm = m.name_snapshot();
    assert_eq!(nm.name, "");
    assert!(!nm.continue_enabled); // empty name
    // A plain create stamps no retention — only the Photo Library preset does
    // (`tests/photo_library.rs`).
    assert!(m.review_snapshot().retention.is_none());
    let dp = m.device_places_snapshot();
    assert_eq!(dp.devices.len(), 2);
    assert!(dp.devices.iter().all(|d| !d.selected));
    assert!(
        dp.devices
            .iter()
            .all(|d| d.place_flags() == PlaceFlags::default_place())
    );
}

#[test]
fn observer_notified_on_mutation() {
    let observer = CountingObserver::new();
    let observer_dyn: Arc<dyn FolderWizardObserver> = Arc::clone(&observer) as _;
    let m = FolderWizardMachine::new(observer_dyn, devices(), Arc::new(FakeFolderNestApi::new()));
    let before = observer.count();
    m.set_name("docs".into());
    assert!(observer.count() > before);
}

// ── Name gating + navigation ────────────────────────────────────────────────

#[test]
fn name_gates_continue_and_next() {
    let m = machine();
    assert!(!m.name_snapshot().continue_enabled);
    m.next(); // blocked
    assert_eq!(m.step(), FolderWizardStep::Name);

    m.set_name("  ".into()); // whitespace-only is still invalid
    assert!(!m.name_snapshot().continue_enabled);
    m.next();
    assert_eq!(m.step(), FolderWizardStep::Name);

    m.set_name("photos".into());
    assert!(m.name_snapshot().continue_enabled);
    m.next();
    assert_eq!(m.step(), FolderWizardStep::Devices);
}

/// The wizard is three steps — name → devices → review — since phase 5 of the
/// folders re-model retired the scan-frequency step (`file-sync.md` § Config,
/// the phase-5 block): the reconcile cadence is a constant, not a choice.
#[test]
fn forward_and_back_walk_the_full_flow() {
    let m = machine();
    m.set_name("docs".into());
    m.next();
    assert_eq!(m.step(), FolderWizardStep::Devices);
    m.next();
    assert_eq!(m.step(), FolderWizardStep::Review);
    m.next(); // no-op on Review (create is submit())
    assert_eq!(m.step(), FolderWizardStep::Review);

    m.back();
    assert_eq!(m.step(), FolderWizardStep::Devices);
    m.back();
    assert_eq!(m.step(), FolderWizardStep::Name);
    m.back(); // no-op on first step
    assert_eq!(m.step(), FolderWizardStep::Name);
}

// ── Device gestures ─────────────────────────────────────────────────────────

#[test]
fn toggle_and_flag_gestures_update_devices() {
    let m = machine();
    m.toggle_device_member(0);
    m.set_device_flags(0, true, false, false);
    let dp = m.device_places_snapshot();
    assert!(dp.devices[0].selected);
    assert_eq!(
        dp.devices[0].place_flags(),
        PlaceFlags::new(true, false, false)
    );
    assert!(!dp.devices[1].selected);

    // Out-of-range is ignored.
    m.toggle_device_member(99);
    assert_eq!(m.device_places_snapshot().devices.len(), 2);

    m.toggle_device_member(0); // untoggle
    assert!(!m.device_places_snapshot().devices[0].selected);
}

#[test]
fn review_lists_enrolled_devices() {
    let m = machine();
    m.set_name("docs".into());
    m.toggle_device_member(1);
    m.set_device_flags(1, true, true, false);
    let r = m.review_snapshot();
    assert_eq!(r.enrolled.len(), 1);
    assert_eq!(r.enrolled[0].label, "phone");
    assert!(r.create_enabled);
}

// ── submit() wire shape ─────────────────────────────────────────────────────

#[tokio::test]
async fn submit_builds_create_then_member_calls() {
    let fake = Arc::new(FakeFolderNestApi::new());
    let m = machine_with(Arc::clone(&fake));
    m.set_name("docs".into());
    m.toggle_device_member(0);
    m.set_device_flags(0, true, false, false);
    m.toggle_device_member(1); // stays at the default point
    // walk to review
    m.next();
    m.next();
    assert_eq!(m.step(), FolderWizardStep::Review);

    let step = m.submit().await;
    assert_eq!(step, FolderWizardStep::Done);
    assert_eq!(m.review_snapshot().phase, SubmitPhase::Done);

    let calls = fake.calls();
    // 1 create + 2 members
    assert_eq!(calls.len(), 3);
    match &calls[0] {
        FakeCall::CreateFolder { req } => {
            assert_eq!(req.name, "docs");
            assert!(req.retention_policy.is_none()); // a plain create stamps none
        }
        other => panic!("expected create, got {other:?}"),
    }
    let places: Vec<(&str, (bool, bool, bool))> = calls[1..]
        .iter()
        .map(|c| match c {
            FakeCall::SetPlace { folder, place, .. } => {
                assert_eq!(folder, "docs");
                (place.device_id.as_str(), place.flags.point())
            }
            other => panic!("expected set_place, got {other:?}"),
        })
        .collect();
    assert!(places.contains(&(("aa".repeat(32)).as_str(), (true, false, false))));
    assert!(places.contains(&(("bb".repeat(32)).as_str(), (true, true, true))));
}

#[tokio::test]
async fn submit_stamps_injected_default_conflict_policy() {
    // The client glue injects the user's global default
    // (the fauna.state.sync-prefs default conflict policy) after opening the wizard;
    // submit stamps it onto the create request so a new set starts on the
    // preferred policy atomically. Un-injected (None) sends no policy — the
    // nest column default (auto) applies.
    let fake = Arc::new(FakeFolderNestApi::new());
    let m = machine_with(Arc::clone(&fake));
    m.set_name("docs".into());
    m.set_default_conflict_policy(Some("latest_wins_always".into()));
    m.next();
    m.next();
    let step = m.submit().await;
    assert_eq!(step, FolderWizardStep::Done);

    match &fake.calls()[0] {
        FakeCall::CreateFolder { req, .. } => {
            assert_eq!(req.conflict_policy.as_deref(), Some("latest_wins_always"));
        }
        other => panic!("expected create, got {other:?}"),
    }
}

#[tokio::test]
async fn submit_blocked_off_review() {
    let m = machine();
    m.set_name("docs".into());
    // still on Name
    let step = m.submit().await;
    assert_eq!(step, FolderWizardStep::Name);
    let fake_calls_empty = m.review_snapshot().phase;
    assert_eq!(fake_calls_empty, SubmitPhase::Idle);
}

#[tokio::test]
async fn submit_create_failure_stays_on_review() {
    let fake = Arc::new(FakeFolderNestApi::new());
    fake.set_create_response(Err(FolderApiError::Conflict {
        detail: "folder with that name already exists".into(),
    }));
    let m = machine_with(Arc::clone(&fake));
    m.set_name("dupe".into());
    m.next();
    m.next();
    let step = m.submit().await;
    assert_eq!(step, FolderWizardStep::Review);
    let r = m.review_snapshot();
    assert_eq!(r.phase, SubmitPhase::Failed);
    assert!(!r.created);
    let err = r.error.expect("error set");
    assert_eq!(err.key, "devices.wizard.create_error");
    assert_eq!(
        err.args.get("message").map(String::as_str),
        Some("folder with that name already exists")
    );
    // No member calls attempted when create fails.
    assert_eq!(fake.calls().len(), 1);
}

#[tokio::test]
async fn submit_partial_member_failure_records_and_retries_only_failed() {
    let fake = Arc::new(FakeFolderNestApi::new());
    // device "bb..." fails to enroll; "aa..." succeeds.
    fake.set_place_response(
        &"bb".repeat(32),
        Err(FolderApiError::NotFound {
            detail: "device not found".into(),
        }),
    );
    let m = machine_with(Arc::clone(&fake));
    m.set_name("docs".into());
    m.toggle_device_member(0); // aa
    m.toggle_device_member(1); // bb (will fail)
    m.next();
    m.next();

    let step = m.submit().await;
    assert_eq!(step, FolderWizardStep::Review);
    let r = m.review_snapshot();
    assert_eq!(r.phase, SubmitPhase::Failed);
    assert!(r.created); // folder was created
    assert_eq!(r.failed_members, vec!["phone".to_string()]);
    // The member-failure branch uses its own key (create succeeded), distinct
    // from the create-failure key asserted in submit_create_failure_stays_on_review.
    let err = r.error.as_ref().expect("error set");
    assert_eq!(err.key, "devices.wizard.create_member_error");
    assert_eq!(
        err.args.get("message").map(String::as_str),
        Some("device not found")
    );
    // create + 2 member attempts
    assert_eq!(fake.calls().len(), 3);

    // Retry: bb now succeeds. submit() must NOT re-create and must only retry bb.
    fake.set_place_response(&"bb".repeat(32), Ok(()));
    let step = m.submit().await;
    assert_eq!(step, FolderWizardStep::Done);
    assert_eq!(m.review_snapshot().phase, SubmitPhase::Done);

    let calls = fake.calls();
    // Previous 3 + exactly one more member retry (no second create).
    assert_eq!(calls.len(), 4);
    assert!(
        matches!(&calls[3], FakeCall::SetPlace { place, .. } if place.device_id == "bb".repeat(32))
    );
}

// ── Aggregate snapshot ──────────────────────────────────────────────────────

#[test]
fn aggregate_snapshot_carries_every_step() {
    let m = machine();
    m.set_name("docs".into());
    let snap = m.snapshot();
    assert_eq!(snap.step, FolderWizardStep::Name);
    assert_eq!(snap.name.name, "docs");
    assert_eq!(snap.device_places.devices.len(), 2);
    assert!(snap.review.retention.is_none());
}
