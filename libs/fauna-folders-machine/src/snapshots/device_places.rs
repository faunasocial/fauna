use serde::{Deserialize, Serialize};

use crate::state::WizardDevice;

/// Step 2 — per-device enrollment (`wizard-device-check` indexed) + the place
/// flags (`wizard-device-originates` / `-accepts` / `-applies-deletes`,
/// indexed). Every folder gets this step; there is no mode to vary it by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DevicePlacesSnapshot {
    pub devices: Vec<WizardDevice>,
    /// `wizard-next-button` enabled. Enrolling zero devices is allowed (matches
    /// the web wizard, which lets you advance regardless), and no flag point
    /// blocks the step either — every point the three checkboxes can reach is a
    /// valid place. Always `true` today; kept because every other wizard step's
    /// snapshot carries it.
    pub continue_enabled: bool,
}
