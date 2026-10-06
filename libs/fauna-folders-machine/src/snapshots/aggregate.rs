use serde::{Deserialize, Serialize};

use super::{DevicePlacesSnapshot, NameSnapshot, ReviewSnapshot};
use crate::state::FolderWizardStep;

/// The full renderable wizard state in one record. This is the serializable
/// form that slots into the broader `DevicesSnapshot { …, wizard:
/// Option<FolderWizardSnapshot> }` (see `docs/goal/ui/devices.md`
/// § State & data shape) — `None` when no wizard is open, `Some(_)` while one
/// is. Clients that prefer per-step access can instead call the machine's
/// individual `*_snapshot()` getters; this aggregate carries all of them so a
/// single tick fully describes the wizard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FolderWizardSnapshot {
    pub step: FolderWizardStep,
    pub name: NameSnapshot,
    /// Step 2 as the place-flag checkboxes render it (phase 2 slice e).
    pub device_places: DevicePlacesSnapshot,
    pub review: ReviewSnapshot,
}
