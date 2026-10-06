//! The wizard's device step configures **place flags**, not a role noun
//! (folders re-model phase 2 slice e; element IDs `wizard-device-originates` /
//! `-accepts` / `-applies-deletes`, user-approved 2026-08-15).
//!
//! The three checkboxes are the whole device-step surface, and a place is its
//! flags: enrollment goes through `fauna.folders.places.set`, and all eight
//! points the three checkboxes span are sendable exactly as chosen.

use std::sync::Arc;

use fauna_folders_machine::nest_api::{FakeCall, FakeFolderNestApi};
use fauna_folders_machine::observer::NullObserver;
use fauna_folders_machine::{DeviceOption, FolderWizardMachine, FolderWizardStep};
use fauna_protocol::folders::PlaceFlags;

fn wizard(devices: &[&str]) -> (Arc<FolderWizardMachine>, Arc<FakeFolderNestApi>) {
    let api = Arc::new(FakeFolderNestApi::new());
    let machine = FolderWizardMachine::new(
        Arc::new(NullObserver),
        devices
            .iter()
            .map(|id| DeviceOption {
                device_id: (*id).to_string(),
                label: format!("device {id}"),
            })
            .collect(),
        api.clone(),
    );
    (machine, api)
}

/// Walk to `Review` with a valid name so `submit()` is live.
fn to_review(m: &FolderWizardMachine) {
    m.set_name("photos".to_string());
    m.next(); // Name -> Devices
    m.next(); // Devices -> Review (phase 5 retired the Frequency step)
    assert_eq!(m.step(), FolderWizardStep::Review);
}

/// A device the user has not touched enrols at the default point — what every
/// enrollment has always been.
#[test]
fn a_fresh_device_defaults_to_the_default_point() {
    let (m, _api) = wizard(&["aa"]);
    let d = &m.device_places_snapshot().devices[0];
    assert_eq!(d.place_flags(), PlaceFlags::default_place());
}

/// The archive seat is reachable by unticking exactly ONE box (peer deletes).
/// An archive seat contributes its own files and only declines peers' deletes
/// (the 2026-08-19 originates ruling).
#[tokio::test]
async fn unticking_deletes_sends_the_archive_seat() {
    let (m, api) = wizard(&["aa"]);
    m.toggle_device_member(0);
    m.set_device_flags(0, true, true, false);
    to_review(&m);
    assert_eq!(m.submit().await, FolderWizardStep::Done);

    let place = api
        .calls()
        .into_iter()
        .find_map(|c| match c {
            FakeCall::SetPlace { place, .. } => Some(place),
            _ => None,
        })
        .expect("the enrolled device was never sent");
    assert_eq!(place.flags, PlaceFlags::archive_place());
}

/// The seam carries **flags**, not a role noun — so the machine can never round
/// a point to a neighbouring one on its way out. Pins that the whole eight-point
/// space survives the trip.
#[tokio::test]
async fn every_flag_point_the_checkboxes_reach_is_sent_verbatim() {
    for originates in [false, true] {
        for accepts in [false, true] {
            for applies_deletes in [false, true] {
                let want = PlaceFlags {
                    originates,
                    accepts,
                    applies_deletes,
                    ..Default::default()
                };
                let (m, api) = wizard(&["aa"]);
                m.toggle_device_member(0);
                m.set_device_flags(0, originates, accepts, applies_deletes);
                to_review(&m);
                assert_eq!(m.submit().await, FolderWizardStep::Done, "{want:?}");

                let sent = api
                    .calls()
                    .into_iter()
                    .find_map(|c| match c {
                        FakeCall::SetPlace { place, .. } => Some(place),
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("{want:?} was never sent"));
                assert_eq!(sent.flags, want, "{want:?} did not survive the seam");
            }
        }
    }
}

/// An *unenrolled* device's flags are irrelevant — they are never sent, so they
/// must not block the wizard.
#[test]
fn an_unenrolled_device_with_odd_flags_does_not_block() {
    let (m, _api) = wizard(&["aa", "bb"]);
    m.toggle_device_member(0);
    m.set_device_flags(1, true, true, false); // never enrolled
    assert!(m.device_places_snapshot().continue_enabled);
}
