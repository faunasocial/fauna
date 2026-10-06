//! Photo-library set model — the adopt-or-create decision + its resolver.
//!
//! Authority: `docs/goal/ui/folders.md` § Photo backup → *Target set model*
//! (the wizard-created "Photo Library" preset).
//!
//! The rule's failure mode is orphaning a user's photo library, so it lives in
//! shared Rust with one implementation for apple + android (priority #2/#4),
//! not once per client.

use std::sync::Arc;

use fauna_core::folder_keys::FolderRef;
use fauna_folders_machine::nest_api::{FakeCall, FolderRow};
use fauna_folders_machine::observer::NullObserver;
use fauna_folders_machine::photo_library::{
    PHOTO_LIBRARY_SET_NAME, PhotoLibraryDecision, PhotoLibrarySet, decide_photo_library_set,
    ensure_photo_library_set,
};

/// The pre-B3 hardcoded set name, spelled here only to pin that it is no longer
/// adopted.
const PRE_SWEEP_PHOTO_SET_NAME: &str = "photos";
use fauna_folders_machine::{DeviceOption, FakeFolderNestApi, FolderApiError};

/// Rows with ids `1..` in order — the shape the fake lists.
fn rows(v: &[&str]) -> Vec<FolderRow> {
    v.iter()
        .enumerate()
        .map(|(i, s)| FolderRow {
            id: i as i64 + 1,
            name: s.to_string(),
        })
        .collect()
}

fn set(id: i64, name: &str) -> PhotoLibrarySet {
    PhotoLibrarySet {
        name: name.to_string(),
        folder_ref: FolderRef::Local(id),
    }
}

// ── The pure decision ───────────────────────────────────────────────────────

#[test]
fn a_fresh_actor_creates_the_photo_library_preset() {
    assert_eq!(
        decide_photo_library_set(&rows(&[]), None),
        PhotoLibraryDecision::Create(PHOTO_LIBRARY_SET_NAME.to_string())
    );
}

/// An existing `"photos"` set is **not adopted**: the pre-B3 hardcoded set's
/// adoption rule was retired by the compat-remnant sweep (program 4,
/// `version-compatibility.md` § Dimension 2) — no pre-sweep installation holds
/// photos there — so it is an ordinary user folder and the decision creates the
/// "Photo Library" preset beside it (or uses it, if it already exists).
#[test]
fn a_pre_sweep_photos_set_is_not_adopted() {
    assert_eq!(
        decide_photo_library_set(&rows(&["docs", PRE_SWEEP_PHOTO_SET_NAME]), None),
        PhotoLibraryDecision::Create(PHOTO_LIBRARY_SET_NAME.to_string())
    );
    assert_eq!(
        decide_photo_library_set(
            &rows(&[PHOTO_LIBRARY_SET_NAME, PRE_SWEEP_PHOTO_SET_NAME]),
            None
        ),
        PhotoLibraryDecision::Use(set(1, PHOTO_LIBRARY_SET_NAME))
    );
}

#[test]
fn an_existing_photo_library_set_is_reused_not_duplicated() {
    // A duplicate-name create is a hard `Conflict`, never an idempotent success.
    assert_eq!(
        decide_photo_library_set(&rows(&[PHOTO_LIBRARY_SET_NAME]), None),
        PhotoLibraryDecision::Use(set(1, PHOTO_LIBRARY_SET_NAME))
    );
}

#[test]
fn a_live_device_binding_pins_the_set() {
    // Once bound, the binding is authoritative — a set appearing later must not
    // silently re-target this device's camera-roll ingress.
    assert_eq!(
        decide_photo_library_set(
            &rows(&["Holiday", PHOTO_LIBRARY_SET_NAME]),
            Some(FolderRef::Local(1))
        ),
        PhotoLibraryDecision::Use(set(1, "Holiday"))
    );
}

/// The binding is by IDENTITY, not by name: the bound set renamed to anything
/// — even while another set now wears the preset name — stays the target, and
/// the resolver hands back its CURRENT name for the control-plane calls.
#[test]
fn a_bound_set_stays_the_target_across_a_rename() {
    let existing = vec![
        FolderRow {
            id: 7,
            name: "Camera Roll 2026".into(),
        },
        FolderRow {
            id: 9,
            name: PHOTO_LIBRARY_SET_NAME.into(),
        },
    ];
    assert_eq!(
        decide_photo_library_set(&existing, Some(FolderRef::Local(7))),
        PhotoLibraryDecision::Use(set(7, "Camera Roll 2026")),
        "the ref pins the set; the same-named newcomer is not adopted"
    );
}

#[test]
fn a_binding_whose_set_was_deleted_re_resolves() {
    // Self-heal: a stale binding must not fail closed forever. (The user deleted
    // the set from the Folders page.)
    assert_eq!(
        decide_photo_library_set(&rows(&["docs"]), Some(FolderRef::Local(99))),
        PhotoLibraryDecision::Create(PHOTO_LIBRARY_SET_NAME.to_string())
    );
}

// ── The resolver (drives the ordinary wizard on the create arm) ─────────────

fn device() -> Vec<DeviceOption> {
    vec![DeviceOption {
        device_id: "ab".repeat(32),
        label: "fauna-macos".into(),
    }]
}

#[tokio::test]
async fn resolving_on_a_fresh_actor_creates_the_preset_through_the_wizard() {
    let api = Arc::new(FakeFolderNestApi::new());
    api.set_folder_names(vec!["docs".into()]);

    let set = ensure_photo_library_set(api.clone(), Arc::new(NullObserver), device(), None)
        .await
        .expect("create succeeds");

    assert_eq!(set.name, PHOTO_LIBRARY_SET_NAME);
    assert_eq!(
        set.folder_ref,
        FolderRef::Local(2),
        "the created row's identity comes back from the post-create list"
    );

    // It must go through the ordinary `FolderWizardMachine` create — never a
    // client-invented bespoke path (`folders.md` § Photo backup).
    let creates: Vec<_> = api
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            FakeCall::CreateFolder { req } => Some(req),
            _ => None,
        })
        .collect();
    assert_eq!(creates.len(), 1, "exactly one create");
    assert_eq!(creates[0].name, PHOTO_LIBRARY_SET_NAME);
    // The preset's shape without a mode (`ui/folders.md` § Photo backup): it
    // stamps the former Backup default EXPLICITLY — a plain create stamps none.
    let r = creates[0]
        .retention_policy
        .expect("the preset stamps its snapshot policy explicitly");
    assert_eq!((r.max_snapshots, r.max_age_days), (7, 30));
}

#[tokio::test]
async fn a_failed_create_surfaces_the_error_rather_than_returning_a_phantom_set() {
    // Returning a set name the nest does not have would send every subsequent
    // `changes.record` into `not_found` — the exact silent failure B3 fixes.
    let api = Arc::new(FakeFolderNestApi::new());
    api.set_folder_names(vec![]);
    api.set_create_response(Err(FolderApiError::Transient {
        detail: "nest down".into(),
    }));

    let err = ensure_photo_library_set(api, Arc::new(NullObserver), device(), None)
        .await
        .expect_err("a failed create must surface");
    assert!(err.detail().contains("nest down"));
}

#[tokio::test]
async fn a_list_failure_refuses_rather_than_guessing() {
    // Fail closed: guessing "Photo Library" against an unreadable list could
    // create a duplicate set beside the user's real one.
    let api = Arc::new(FakeFolderNestApi::new());
    api.set_list_response(Err(FolderApiError::Transient {
        detail: "offline".into(),
    }));

    let err = ensure_photo_library_set(api, Arc::new(NullObserver), device(), None)
        .await
        .expect_err("an unreadable list must refuse");
    assert!(err.detail().contains("offline"));
}
