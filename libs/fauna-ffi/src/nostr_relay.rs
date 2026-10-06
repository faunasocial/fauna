//! UniFFI façade for the shared Nostr relay-URL validator and list-add rule
//! ([`fauna_protocol::nostr_relay`]).
//!
//! Gives Apple / Android / Windows the identical "Add relay" format check,
//! trim/empty rule, and dedup/append rule that web and (natively) linux/tui
//! use, so no client re-derives them (priority #1/#2 — the relay analog of
//! `handle::validate_handle`). Each app keeps only its own already-localized
//! invalid-url error copy. See `docs/goal/ui/nostr.md`.

/// UniFFI face of [`fauna_protocol::nostr_relay::relay_url_error`] — the
/// localized message for a relay URL the user may not add (malformed, or a
/// private-network address — `nest/network-exposure.md` § Rulings F7), `None`
/// when it is acceptable. Gated like `nostr_client.rs::bunker_app_label`: a
/// bare `fauna_core::LocalizedText` crosses the boundary.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn relay_url_error(url: String) -> Option<fauna_core::localized::LocalizedText> {
    fauna_protocol::nostr_relay::relay_url_error(&url)
}

/// UniFFI face of [`fauna_protocol::nostr_relay::trimmed_relay_input`].
#[uniffi::export]
pub fn trimmed_relay_input(input: String) -> Option<String> {
    fauna_protocol::nostr_relay::trimmed_relay_input(&input)
}

/// UniFFI face of [`fauna_protocol::nostr_relay::relay_list_appending`].
#[uniffi::export]
pub fn relay_list_appending(existing: Vec<String>, url: String) -> Option<Vec<String>> {
    fauna_protocol::nostr_relay::relay_list_appending(&existing, &url)
}

#[cfg(test)]
mod tests {
    use super::{relay_list_appending, trimmed_relay_input};

    #[cfg(feature = "value-format")]
    #[test]
    fn error_none_for_wss_or_ws_some_otherwise() {
        use super::relay_url_error;
        assert!(relay_url_error("wss://relay.damus.io".into()).is_none());
        assert!(relay_url_error("ws://relay.example.com:7777".into()).is_none());
        assert!(relay_url_error("ws://localhost:7777".into()).is_some());
        assert!(relay_url_error("https://relay.damus.io".into()).is_some());
        assert!(relay_url_error("".into()).is_some());
    }

    #[test]
    fn trims_and_rejects_empty() {
        assert_eq!(
            trimmed_relay_input("  wss://relay.damus.io  ".into()),
            Some("wss://relay.damus.io".to_string())
        );
        assert_eq!(trimmed_relay_input("   ".into()), None);
    }

    #[test]
    fn appends_and_dedups() {
        let existing = vec!["wss://a".to_string()];
        assert_eq!(
            relay_list_appending(existing.clone(), "wss://b".into()),
            Some(vec!["wss://a".to_string(), "wss://b".to_string()])
        );
        assert_eq!(relay_list_appending(existing, "wss://a".into()), None);
    }
}
