//! Errors surfaced to clients via observer callbacks and the
//! `LaunchSnapshot::last_error` field.

use thiserror::Error;

#[derive(Debug, Error)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
pub enum LaunchError {
    /// Network / HTTP transport failure. `detail` is named `detail` (not
    /// `message`) so the UniFFI Kotlin generator doesn't collide with
    /// `Throwable.message` on the generated `LaunchException.Network`.
    #[error("network error: {detail}")]
    Network { detail: String },

    /// Server returned a 4xx/5xx status the launch flow doesn't recognize.
    #[error("server error: HTTP {status}: {detail}")]
    ServerError { status: u16, detail: String },

    /// `/auth/verify` returned 404 "actor not registered" — the launch
    /// flow uses this to drop to the wizard at `invite_request`.
    #[error("actor not registered on this nest")]
    ActorNotRegistered,

    /// Account is locked (HTTP 423 from `/auth/token`). `locked_until_secs`
    /// is unix seconds; clients display a countdown.
    #[error("account locked until {locked_until_secs}")]
    AccountLocked { locked_until_secs: u64 },

    /// Server returned a body we couldn't parse (missing field, wrong shape).
    #[error("invalid server response: {detail}")]
    InvalidResponse { detail: String },

    /// Local Ed25519 signature failed to construct (corrupt secret bytes,
    /// length mismatch, etc.).
    #[error("signature failed: {reason}")]
    SignatureFailed { reason: String },

    /// Caller invoked a transition that the current phase doesn't allow
    /// (e.g. `notify_401()` while in `Boot`).
    #[error("invalid transition from {from}: {reason}")]
    InvalidTransition { from: String, reason: String },

    /// Generic fallback. Field is `detail` for Kotlin-collision reasons.
    #[error("{detail}")]
    Other { detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_error_message_includes_detail() {
        let e = LaunchError::Network {
            detail: "DNS timeout".into(),
        };
        assert!(e.to_string().contains("DNS timeout"));
    }

    #[test]
    fn account_locked_error_carries_locked_until() {
        let e = LaunchError::AccountLocked {
            locked_until_secs: 1_700_000_000,
        };
        assert!(e.to_string().contains("1700000000"));
    }

    #[test]
    fn invalid_response_error_includes_detail() {
        let e = LaunchError::InvalidResponse {
            detail: "missing field token".into(),
        };
        assert!(e.to_string().contains("missing field token"));
    }

    #[test]
    fn signature_error_includes_reason() {
        let e = LaunchError::SignatureFailed {
            reason: "wrong key".into(),
        };
        assert!(e.to_string().contains("wrong key"));
    }
}
