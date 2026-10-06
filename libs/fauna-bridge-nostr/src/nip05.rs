use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// NIP-05 response body for `/.well-known/nostr.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Nip05Response {
    pub names: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relays: Option<HashMap<String, Vec<String>>>,
}

/// Build a NIP-05 JSON response from a list of (name, hex_pubkey) pairs.
pub fn build_nip05_response(names: &[(&str, &str)]) -> String {
    let map: HashMap<String, String> = names
        .iter()
        .map(|(name, pubkey)| (name.to_string(), pubkey.to_string()))
        .collect();
    let response = Nip05Response {
        names: map,
        relays: None,
    };
    serde_json::to_string(&response).expect("NIP-05 JSON serialization should not fail")
}

/// Build a NIP-05 JSON response with optional per-pubkey relay hints
/// (NIP-05 § "Showing users' relays").
///
/// Each entry is `(name, hex_pubkey, relay_hints)`: the `name → pubkey` mapping
/// always lands in `names`; a non-empty `relay_hints` list adds a `relays`
/// entry keyed by the pubkey. An empty `entries` yields `{"names":{}}` (the
/// `relays` object is omitted entirely) — the no-enumeration answer a serving
/// route returns for an unknown or absent `?name=`.
pub fn build_nip05_response_with_relays(entries: &[(&str, &str, &[&str])]) -> String {
    let mut names: HashMap<String, String> = HashMap::new();
    let mut relays: HashMap<String, Vec<String>> = HashMap::new();
    for (name, pubkey, hints) in entries {
        names.insert((*name).to_string(), (*pubkey).to_string());
        if !hints.is_empty() {
            relays
                .entry((*pubkey).to_string())
                .or_default()
                .extend(hints.iter().map(|h| (*h).to_string()));
        }
    }
    let response = Nip05Response {
        names,
        relays: (!relays.is_empty()).then_some(relays),
    };
    serde_json::to_string(&response).expect("NIP-05 JSON serialization should not fail")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_nip05_response_produces_valid_json() {
        let json = build_nip05_response(&[("alice", &"a".repeat(64)), ("bob", &"b".repeat(64))]);
        let parsed: Nip05Response = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.names.len(), 2);
        assert_eq!(parsed.names["alice"], "a".repeat(64));
        assert_eq!(parsed.names["bob"], "b".repeat(64));
    }

    #[test]
    fn build_nip05_response_empty() {
        let json = build_nip05_response(&[]);
        let parsed: Nip05Response = serde_json::from_str(&json).unwrap();
        assert!(parsed.names.is_empty());
    }

    #[test]
    fn nip05_response_no_relays_by_default() {
        let json = build_nip05_response(&[("alice", "abc")]);
        // relays field should not appear in JSON
        assert!(!json.contains("relays"));
    }

    #[test]
    fn with_relays_carries_name_and_relay_hint() {
        let pubkey = "a".repeat(64);
        let json =
            build_nip05_response_with_relays(&[("alice", &pubkey, &["wss://nest.test/nostr"])]);
        let parsed: Nip05Response = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.names["alice"], pubkey);
        assert_eq!(
            parsed.relays.unwrap()[&pubkey],
            vec!["wss://nest.test/nostr".to_string()]
        );
    }

    #[test]
    fn with_relays_omits_relays_when_no_hints() {
        let json = build_nip05_response_with_relays(&[("alice", "abc", &[])]);
        assert!(
            !json.contains("relays"),
            "empty hints omit the relays object"
        );
        let parsed: Nip05Response = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.names["alice"], "abc");
        assert!(parsed.relays.is_none());
    }

    #[test]
    fn with_relays_empty_entries_is_empty_names() {
        let json = build_nip05_response_with_relays(&[]);
        let parsed: Nip05Response = serde_json::from_str(&json).unwrap();
        assert!(parsed.names.is_empty());
        assert!(parsed.relays.is_none());
        assert!(!json.contains("relays"));
    }
}
