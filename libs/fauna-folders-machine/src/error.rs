use thiserror::Error;

/// Errors surfaced by the folder wizard machine. Most user-facing wizard
/// failures are carried *in the snapshots* as [`fauna_core::localized::LocalizedText`]
/// (so each app localizes them); this enum is for the few FFI methods that
/// return `Result` for caller-side handling.
#[derive(Debug, Error)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
pub enum FolderWizardError {
    /// A gesture was invoked in a state that doesn't allow it (e.g. advancing
    /// past the name step with an empty name).
    ///
    /// Field is `detail` (not `message`) so the UniFFI Kotlin generator doesn't
    /// collide with `Throwable.message` on the generated exception class.
    #[error("invalid transition: {detail}")]
    InvalidTransition { detail: String },

    /// Generic fallback.
    #[error("{detail}")]
    Other { detail: String },
}
