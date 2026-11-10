use serde::{Deserialize, Serialize};

/// Step 1 — the name input (`wizard-name-input`) and, beside it, the
/// create-time audience (`wizard-audience-select`; `ui/folders.md`
/// § Audience and website serving → *Born public*). A folder has no type, so
/// the step carries no mode picker (`folders.md` § Target re-model).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct NameSnapshot {
    pub name: String,
    /// `wizard-next-button` enabled — true once the name is non-empty. The
    /// audience never gates it.
    pub continue_enabled: bool,
    /// The chosen audience, normalized: `private` (default) or `public`. The
    /// select's options and hint come from `audience_options(false, …)` /
    /// `audience_hint(false, …)`, a new folder being unbound.
    #[cfg_attr(feature = "uniffi", uniffi(default = "private"))]
    #[serde(default = "default_audience")]
    pub audience: String,
    /// Whether the select paints at all — false on a seam that cannot attest
    /// a born-public create (a control that cannot work must not paint).
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    #[serde(default)]
    pub audience_offered: bool,
}

pub(crate) fn default_audience() -> String {
    crate::state::AUDIENCE_PRIVATE.to_string()
}
