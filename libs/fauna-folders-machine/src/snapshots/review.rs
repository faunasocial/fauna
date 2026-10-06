use serde::{Deserialize, Serialize};

use fauna_core::localized::LocalizedText;

use crate::state::{RetentionPolicy, SubmitPhase};

/// One enrolled device in the review summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct EnrolledDeviceSummary {
    pub device_id: String,
    pub label: String,
}

/// Step 4 — read-only review + the create gesture (`wizard-create-button`).
/// Also carries the `submit()` lifecycle so clients can show progress / errors
/// and gate the button without local state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ReviewSnapshot {
    pub name: String,
    /// The retention the create stamps — `Some` only for a preset that sets
    /// one (the Photo Library); `None` for every plain wizard create.
    pub retention: Option<RetentionPolicy>,
    pub enrolled: Vec<EnrolledDeviceSummary>,
    /// `wizard-create-button` enabled — name valid and not currently submitting.
    pub create_enabled: bool,
    pub phase: SubmitPhase,
    /// True once the `POST /folders` create succeeded. On a partial failure
    /// (create ok, some member adds failed) this stays true so the client knows
    /// not to retry the create.
    pub created: bool,
    /// Labels of devices whose member-add failed during the last `submit()`.
    pub failed_members: Vec<String>,
    /// Structured submit error, `None` until a failure.
    pub error: Option<LocalizedText>,
}
