//! Read-time helpers over a `fauna.bridges.list` reply's `settings[]`
//! (`bridges_ui::BridgeSetting`) — pure, transport-free lookups every native
//! bridge-settings page (tui, linux) needs, so "what's this setting's current
//! value?" has one answer everywhere (priority #1). web/android carry their
//! own typed twins (`boolSetting`/`parseRelayList`) since they don't share
//! this Rust.

use fauna_protocol::Value;
use fauna_protocol::bridges_ui::BridgeSetting;

/// Read a boolean setting off the bridge's settings vec, falling back when the
/// key is absent **or** carries a non-boolean value. Pure — unit-tested
/// without a transport.
pub fn bool_setting(settings: &[BridgeSetting], key: &str, default: bool) -> bool {
    settings
        .iter()
        .find(|s| s.key == key)
        .and_then(|s| match &s.value {
            Value::Bool(v) => Some(*v),
            _ => None,
        })
        .unwrap_or(default)
}

/// The wire key a `relay_list` setting is stored under, shared with the
/// write side ([`relay_list_setting`]'s callers persist under the same key).
pub const RELAY_LIST_KEY: &str = "relay_list";

/// Parse the `relay_list` setting (a JSON array *encoded as a string* — the
/// shape the nest stores). Absent/blank/malformed all read as "no explicit
/// list" — the nest falls back to its own default relay set in that case, so
/// degrading to empty is correct rather than merely safe.
pub fn relay_list_setting(settings: &[BridgeSetting]) -> Vec<String> {
    settings
        .iter()
        .find(|s| s.key == RELAY_LIST_KEY)
        .and_then(|s| match &s.value {
            Value::String(json) if !json.is_empty() => serde_json::from_str(json).ok(),
            _ => None,
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setting(key: &str, value: Value) -> BridgeSetting {
        BridgeSetting {
            key: key.to_string(),
            label: key.to_string(),
            setting_type: "bool".to_string(),
            value,
            options: None,
            extra: Default::default(),
        }
    }

    /// `bool_setting` falls back on an absent key AND on a wrong-typed value.
    #[test]
    fn bool_setting_reads_the_key_and_degrades_safely() {
        let settings = vec![
            setting("auto_publish", Value::Bool(true)),
            setting("expose_content", Value::String("yes".to_string())),
        ];
        assert!(bool_setting(&settings, "auto_publish", false));
        assert!(
            !bool_setting(&settings, "expose_content", false),
            "a non-boolean value falls back to the default"
        );
        assert!(bool_setting(&settings, "absent_key", true));
    }

    /// `relay_list` is a JSON array encoded as a STRING (the shape the nest
    /// stores); absent/blank/malformed all read as "no explicit list".
    #[test]
    fn relay_list_parses_the_json_string_and_degrades_safely() {
        let good = vec![setting(
            RELAY_LIST_KEY,
            Value::String(r#"["wss://a.example","wss://b.example"]"#.to_string()),
        )];
        assert_eq!(
            relay_list_setting(&good),
            vec!["wss://a.example".to_string(), "wss://b.example".to_string()]
        );
        assert!(relay_list_setting(&[]).is_empty());
        assert!(
            relay_list_setting(&[setting(RELAY_LIST_KEY, Value::String(String::new()))]).is_empty()
        );
        assert!(
            relay_list_setting(&[setting(
                RELAY_LIST_KEY,
                Value::String("not json".to_string())
            )])
            .is_empty(),
            "malformed reads as no explicit list, never a panic"
        );
    }
}
