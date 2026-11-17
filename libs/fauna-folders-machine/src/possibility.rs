//! The create wizard's Next and Create answers (`docs/goal/ui/README.md`
//! § Offer only what is possible, rules 2 and 6; the ruling is
//! `docs/goal/ui/folders.md` § Errors & edge cases, *Own-tier and mail
//! gates*).
//!
//! The snapshots' `continue_enabled` / `create_enabled` bools say only
//! whether; these say why not, from the same snapshot, so every app paints the
//! disabled button with its reason and the press asks the same answer. They are
//! free functions over the snapshot rather than snapshot fields because the
//! snapshots also cross into the web SPA as decoded JSON and `Possibility` is
//! serialize-only (its own doc).

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;

use crate::snapshots::{DevicePlacesSnapshot, FolderWizardSnapshot, NameSnapshot, ReviewSnapshot};
use crate::state::{FolderWizardStep, SubmitPhase};

/// Blocked reason: the folder's name is blank.
pub const NAME_REQUIRED: &str = "devices.wizard.name_required";
/// Blocked reason: the create is already in flight.
pub const CREATING: &str = "devices.wizard.creating_folder";
/// Blocked reason: a public create waits on its declassify confirm.
pub const CONFIRM_PUBLIC_PENDING: &str = "devices.wizard.confirm_public_pending";

fn blocked(key: &str) -> Possibility {
    Possibility::blocked(LocalizedText::key(key))
}

/// `wizard-next-button` on the Name step: a name typed (the audience never
/// gates it).
pub fn name_continue_possibility(name: &NameSnapshot) -> Possibility {
    if name.name.trim().is_empty() {
        return blocked(NAME_REQUIRED);
    }
    Possibility::Possible
}

/// `wizard-next-button` on the Devices step: every flag point is a valid
/// place, so nothing blocks it (the snapshot's field stays `true`).
pub fn device_places_continue_possibility(places: &DevicePlacesSnapshot) -> Possibility {
    if places.continue_enabled {
        Possibility::Possible
    } else {
        // No state produces this today; were one added without a reason it
        // still says something rather than nothing.
        Possibility::from_read(fauna_core::possibility::ReadState::Pending)
    }
}

/// `wizard-create-button`: a name, no create already in flight, and — for a
/// public create — the declassify confirm answered (answering it is the
/// gesture that creates). Absent once the wizard is done: there is nothing
/// left to create.
pub fn review_create_possibility(review: &ReviewSnapshot) -> Possibility {
    match review.phase {
        SubmitPhase::Done => return Possibility::Absent,
        SubmitPhase::Submitting => return blocked(CREATING),
        SubmitPhase::Idle | SubmitPhase::Failed => {}
    }
    if review.name.trim().is_empty() {
        return blocked(NAME_REQUIRED);
    }
    if review.declassify_armed {
        return blocked(CONFIRM_PUBLIC_PENDING);
    }
    Possibility::Possible
}

/// `wizard-next-button` on whichever step the wizard shows — the step's own
/// predicate above, absent on the Review and Done steps (which have no Next).
/// The one dispatch every app calls, so no app re-derives which step's answer
/// applies.
pub fn wizard_next_possibility(wizard: &FolderWizardSnapshot) -> Possibility {
    match wizard.step {
        FolderWizardStep::Name => name_continue_possibility(&wizard.name),
        FolderWizardStep::Devices => device_places_continue_possibility(&wizard.device_places),
        FolderWizardStep::Review | FolderWizardStep::Done => Possibility::Absent,
    }
}

/// `wizard-create-button` — [`review_create_possibility`] over the wizard's
/// Review snapshot (the aggregate form, beside [`wizard_next_possibility`]).
pub fn wizard_create_possibility(wizard: &FolderWizardSnapshot) -> Possibility {
    review_create_possibility(&wizard.review)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(n: &str) -> NameSnapshot {
        NameSnapshot {
            name: n.into(),
            continue_enabled: !n.trim().is_empty(),
            audience: "private".into(),
            audience_offered: true,
        }
    }

    fn review(n: &str, phase: SubmitPhase, armed: bool) -> ReviewSnapshot {
        ReviewSnapshot {
            name: n.into(),
            retention: None,
            enrolled: Vec::new(),
            create_enabled: false,
            phase,
            created: false,
            failed_members: Vec::new(),
            error: None,
            audience: "private".into(),
            declassify_armed: armed,
        }
    }

    fn key(p: &Possibility) -> Option<&str> {
        p.reason().map(|r| r.key.as_str())
    }

    #[test]
    fn next_on_the_name_step_says_a_name_is_needed() {
        assert_eq!(
            key(&name_continue_possibility(&name("  "))),
            Some(NAME_REQUIRED)
        );
        assert!(name_continue_possibility(&name("Photos")).is_possible());
    }

    #[test]
    fn the_answer_agrees_with_the_snapshots_bool() {
        for n in ["", " ", "x"] {
            let s = name(n);
            assert_eq!(
                name_continue_possibility(&s).is_possible(),
                s.continue_enabled
            );
        }
    }

    #[test]
    fn create_says_why_it_is_not_live() {
        use SubmitPhase::*;
        assert_eq!(
            key(&review_create_possibility(&review("", Idle, false))),
            Some(NAME_REQUIRED)
        );
        assert_eq!(
            key(&review_create_possibility(&review("a", Submitting, false))),
            Some(CREATING)
        );
        assert_eq!(
            key(&review_create_possibility(&review("a", Idle, true))),
            Some(CONFIRM_PUBLIC_PENDING)
        );
        assert_eq!(
            review_create_possibility(&review("a", Done, false)),
            Possibility::Absent
        );
        assert!(review_create_possibility(&review("a", Idle, false)).is_possible());
        // A failed submit may be retried.
        assert!(review_create_possibility(&review("a", Failed, false)).is_possible());
    }

    #[test]
    fn next_follows_the_step_and_is_absent_past_devices() {
        let places = DevicePlacesSnapshot {
            devices: Vec::new(),
            continue_enabled: true,
        };
        let mut w = FolderWizardSnapshot {
            step: FolderWizardStep::Name,
            name: name(""),
            device_places: places,
            review: review("", SubmitPhase::Idle, false),
        };
        assert_eq!(key(&wizard_next_possibility(&w)), Some(NAME_REQUIRED));
        w.step = FolderWizardStep::Devices;
        assert!(wizard_next_possibility(&w).is_possible());
        for step in [FolderWizardStep::Review, FolderWizardStep::Done] {
            w.step = step;
            assert_eq!(wizard_next_possibility(&w), Possibility::Absent);
        }
        assert_eq!(key(&wizard_create_possibility(&w)), Some(NAME_REQUIRED));
    }

    #[test]
    fn device_places_never_blocks() {
        let places = DevicePlacesSnapshot {
            devices: Vec::new(),
            continue_enabled: true,
        };
        assert!(device_places_continue_possibility(&places).is_possible());
    }
}
