//! UniFFI façade for the shared canonical handle-format validator
//! ([`fauna_protocol::handle::validate_handle`]).
//!
//! The nest enforces these exact rules on every handle-bearing RPC, and the
//! Rust-native Linux app calls `fauna_protocol::handle::validate_handle`
//! directly; this export gives Apple / Windows / Android the identical check so
//! no client re-derives the handle rules per platform (priority #2/#1 — the
//! handle analog of `spam::spam_threshold_band` / `email::encode_filter_rule`).
//! Clients call it pre-submit for instant, uniform feedback (a *taken* handle
//! stays server-authoritative). See `docs/goal/ui/settings.md` § Where logic
//! lives → Handle change.

/// UniFFI face of [`fauna_protocol::handle::validate_handle`]. Returns `None`
/// when the handle is well-formed, or `Some(message)` with a terse, already
/// user-facing error to show in the client's `error-message` element.
#[uniffi::export]
pub fn validate_handle(handle: String) -> Option<String> {
    fauna_protocol::handle::validate_handle(&handle)
        .err()
        .map(|m| m.to_string())
}

#[cfg(test)]
mod tests {
    use super::validate_handle;

    #[test]
    fn none_when_valid_some_message_when_invalid() {
        assert_eq!(validate_handle("alice".into()), None);
        assert_eq!(
            validate_handle("ab".into()),
            Some("handle must be at least 3 characters".to_string())
        );
        assert_eq!(
            validate_handle("Alice".into()),
            Some("handle must be lowercase alphanumeric or hyphens".to_string())
        );
    }
}
