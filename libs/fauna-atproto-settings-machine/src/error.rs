//! Errors the machine's **value-returning** methods surface.
//!
//! Gestures that return nothing (`refresh`, `revoke`, `set_external_apps_enabled`,
//! `revoke_session`) follow the labeler-catalog convention: they never return a
//! `Result`, they set `snapshot().error` and tick the observer, so the page's
//! `error-message` element is the single place a failure is rendered
//! (cross-app e2e convention 2). Only `mint` and `reveal_secret` return a
//! `Result`, because they must hand back a `SecretString` the snapshot
//! deliberately cannot carry.

/// A failure of a value-returning machine method.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
#[cfg_attr(feature = "uniffi", uniffi(flat_error))]
pub enum AtprotoSettingsError {
    /// The nest call failed (transport fault, refusal, or a missing row).
    #[error("{detail}")]
    Nest { detail: String },
    /// The credential store failed to load or persist.
    #[error("{detail}")]
    Store { detail: String },
    /// No `fauna.state.atproto` credential row matches the requested
    /// `credential_id`, so this device cannot reveal its secret.
    ///
    /// **Not necessarily an error state to escalate.** The nest is the
    /// authority on which credentials exist but can never recover a secret (the
    /// D3 custody split stores only the Argon2id PHC verifier), so this is the
    /// expected outcome for a credential minted on a sibling device whose
    /// `fauna.state.atproto` has not synced here yet — the snapshot's
    /// [`crate::snapshots::AppCredentialRow::revealable`] flag is what keeps a
    /// client from offering the affordance in that case. The only recovery is
    /// revoke + re-mint; there is no nest-side lookup, by design.
    #[error("no locally-held secret for credential {credential_id}")]
    SecretUnavailable { credential_id: String },
}

impl AtprotoSettingsError {
    /// The human-readable detail, for logging and for composing the page-level
    /// `error-message`.
    pub fn detail(&self) -> String {
        self.to_string()
    }
}
