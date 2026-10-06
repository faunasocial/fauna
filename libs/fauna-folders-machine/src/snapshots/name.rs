use serde::{Deserialize, Serialize};

/// Step 1 — the name input (`wizard-name-input`). A folder has no type, so
/// the step carries no mode picker (`folders.md` § Target re-model).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct NameSnapshot {
    pub name: String,
    /// `wizard-next-button` enabled — true once the name is non-empty.
    pub continue_enabled: bool,
}
