//! `fauna.plugins.*` — the admin's surface over nest-hosted plugins
//! (`docs/goal/architecture/third-party.md` § The principal model → *Hosted
//! principals*, § The runner contract → *The install-approval leg*).
//!
//! ADMIN class, all three. An install is the nest's act and no account's: the
//! admin names a metadata document, the nest verifies it and its module, and
//! the admin approves the one consent card that opens; the approval mints the
//! plugin's install row and starts it. Install grants nothing over any user's
//! data — each user binds the plugin by their own consent.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

use crate::Value;
use crate::atproto_pds::PendingConsentRow;

/// `fauna.plugins.install` — resolve `document_url`, verify its manifest,
/// fetch and verify its module, and open the install card; or, in the
/// `development` form, synthesize the document of a component on the nest's
/// own filesystem (`third-party.md` § Developer loop). Exactly one of the two.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct InstallPluginRequest {
    /// The plugin's Client ID Metadata Document URL — its identity. Empty in
    /// the `development` form.
    #[serde(default)]
    pub document_url: String,
    /// The developer carve-out: admitted only over a connection whose peer
    /// is a loopback address — a dev nest is one you talk to from its own box.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub development: Option<DevelopmentInstall>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The `development` form of [`InstallPluginRequest`]: a local component the
/// nest reads itself. Its identity is [`development_client_id`] of the file
/// stem, its publisher the loopback host literally ([`DEVELOPMENT_PUBLISHER`]),
/// its declarations whatever an unsigned sibling `<stem>.fauna.json` manifest
/// object says (none when absent). Reinstalling the same stem replaces the
/// module and keeps the plugin's state scope — the edit-run cycle.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DevelopmentInstall {
    /// Absolute path of a `.wasm` component on the nest's filesystem.
    pub module_path: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The displayed name of every development plugin — chosen by the nest, as
/// the loopback OAuth client's is, never read from the plugin.
pub const DEVELOPMENT_PLUGIN_NAME: &str = "Development plugin";

/// The publisher of a development plugin: the loopback host, literally — so
/// its `ext.localhost.*` kinds can never collide with a real publisher's
/// (`third-party-kinds.md` § The grammar).
pub const DEVELOPMENT_PUBLISHER: &str = "localhost";

/// The `client_id` prefix of a development plugin. Never the loopback OAuth
/// client (whose identity has an empty path), so a development plugin can
/// never double as an OAuth client.
pub const DEVELOPMENT_CLIENT_ID_PREFIX: &str = "http://localhost/plugins/";

/// The synthesized `client_id` of the development plugin whose file stem is
/// `stem` — `None` unless the stem is one kind-name label (`a–z 0–9 _ -`),
/// so the identity is always a clean URL.
#[must_use]
pub fn development_client_id(stem: &str) -> Option<String> {
    fauna_core::ext_kind::is_valid_kind_name(stem)
        .then(|| format!("{DEVELOPMENT_CLIENT_ID_PREFIX}{stem}"))
}

/// Is `client_id` a development plugin's synthesized identity?
#[must_use]
pub fn is_development_client_id(client_id: &str) -> bool {
    client_id
        .strip_prefix(DEVELOPMENT_CLIENT_ID_PREFIX)
        .is_some_and(fauna_core::ext_kind::is_valid_kind_name)
}

/// The card the install opened, assigned to the calling admin — approved or
/// declined through `fauna.bridges.atproto.resolve_consent` like every other
/// card; its `install` section says what approval installs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct InstallPluginReply {
    pub consent: PendingConsentRow,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.plugins.list` — every installed plugin with its runner's state.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListPluginsRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListPluginsReply {
    /// Oldest install first.
    pub plugins: Vec<PluginInfo>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One installed plugin.
// `Default` so fixtures can grow this type with `..Default::default()`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PluginInfo {
    /// The install row's id — what `fauna.plugins.uninstall` takes.
    #[serde(with = "serde_bytes")]
    pub principal_id: Vec<u8>,
    /// The metadata document's URL.
    pub client_id: String,
    /// The resolved name the install card showed.
    #[serde(default)]
    pub label: Option<String>,
    /// `wasm` today.
    pub execution_form: String,
    /// The verified manifest's publisher key, raw.
    #[serde(default)]
    pub publisher_key: Option<ByteBuf>,
    /// The verified manifest's `publisher.domain` — the host the install
    /// proved it against; the loopback host for a development plugin.
    /// Additive: `None` from a nest that predates it.
    #[serde(default)]
    pub publisher_domain: Option<String>,
    /// A development plugin (`third-party.md` § Developer loop): no
    /// publisher key, shown as [`DEVELOPMENT_PLUGIN_NAME`]. Additive.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub development: bool,
    #[serde(default)]
    pub declared_kinds: Vec<String>,
    /// The ceiling the admin approved users may grant it.
    #[serde(default)]
    pub requested_scopes: Vec<String>,
    pub module_digest: String,
    #[serde(default)]
    pub hosts: Vec<String>,
    /// The approving admin's actor id.
    #[serde(with = "serde_bytes")]
    pub installed_by: Vec<u8>,
    pub installed_at: i64,
    /// How many accounts have bound it by their own consent.
    pub bound_accounts: u32,
    /// The runner's state right now.
    pub status: PluginStatus,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// A plugin's runner state. `state` is one of [`PLUGIN_STATE_RUNNING`],
/// [`PLUGIN_STATE_RESTARTING`] or [`PLUGIN_STATE_STOPPED`]; an app meeting a
/// value it does not know renders `reason` alone.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PluginStatus {
    pub state: String,
    /// Faults since the runner started it — a plugin that faults repeatedly
    /// is the admin's to uninstall, never the nest's to keep retrying blind.
    pub faults: u32,
    /// The last fault's text, if it ever faulted.
    #[serde(default)]
    pub last_fault: Option<String>,
    /// Why a `stopped` plugin is not running: its own refusal to start, a
    /// missing or no-longer-compiling module.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The plugin is serving.
pub const PLUGIN_STATE_RUNNING: &str = "running";
/// A fault ended its instance; it re-instantiates after a backoff.
pub const PLUGIN_STATE_RESTARTING: &str = "restarting";
/// It is not running and will not restart on its own (`reason` says why).
pub const PLUGIN_STATE_STOPPED: &str = "stopped";

/// `fauna.plugins.uninstall` — the admin's one verb: every user's binding
/// ends, the plugin's state and rows are deleted, the runner stops and the
/// module is removed from disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UninstallPluginRequest {
    #[serde(with = "serde_bytes")]
    pub principal_id: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// An unknown id answers `uninstalled: false` — the end state holds either way.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UninstallPluginReply {
    pub uninstalled: bool,
    /// Users' binding rows ended.
    #[serde(default)]
    pub bindings_ended: u32,
    /// Capability grants those users had minted to the plugin's key.
    #[serde(default)]
    pub capability_grants_ended: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── The user's half: binding an installed plugin ────────────────────────────
//
// USER class, self-scoped (`third-party.md` § The principal model → *Hosted
// principals*): install grants nothing, so each account that wants a plugin
// binds it by its own consent, from the plugin's settings card on the Bridges
// page (`behavior/bridges.md` § Bridge settings).

/// `fauna.plugins.available` — the plugins installed on this nest, as the
/// calling user's settings cards render them: what each is, whether this
/// account has bound it, and its settings with this account's values.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListAvailablePluginsRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListAvailablePluginsReply {
    /// Oldest install first.
    pub plugins: Vec<AvailablePlugin>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One installed plugin, seen by one account.
// `Default` so fixtures can grow this type with `..Default::default()`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AvailablePlugin {
    /// The install row's id — what `fauna.plugins.bind` and
    /// `fauna.plugins.set_settings` take.
    #[serde(with = "serde_bytes")]
    pub principal_id: Vec<u8>,
    /// The metadata document's URL.
    pub client_id: String,
    /// The resolved name the install card showed.
    #[serde(default)]
    pub label: Option<String>,
    /// `wasm` today — the badge every roster row carries.
    pub execution_form: String,
    /// The verified manifest's publisher key, raw.
    #[serde(default)]
    pub publisher_key: Option<ByteBuf>,
    #[serde(default)]
    pub declared_kinds: Vec<String>,
    /// The ceiling the admin approved: what a bind card asks for.
    #[serde(default)]
    pub requested_scopes: Vec<String>,
    /// This account's binding row (its roster row's id, what
    /// `fauna.principals.revoke` takes) — `None` while unbound.
    #[serde(default)]
    pub binding_principal_id: Option<ByteBuf>,
    /// The plugin's `settings_schema` in the `BridgeProvider` settings
    /// vocabulary, each `value` this account's own (the schema's default
    /// until the account sets one). Empty for a plugin that declares none.
    #[serde(default)]
    pub settings: Vec<crate::bridges_ui::BridgeSetting>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.plugins.bind` — open the bind card for the plugin whose install
/// row is `principal_id`, assigned to the caller, over the install's
/// ceiling scopes. Approving it through `resolve_consent` mints the
/// account's binding row.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BindPluginRequest {
    #[serde(with = "serde_bytes")]
    pub principal_id: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BindPluginReply {
    /// The card, with its `install` section, `holder_x25519` and
    /// `fauna_manifest` — what the approving app's consent-time mint reads.
    pub consent: PendingConsentRow,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.plugins.set_settings` — write the caller's own settings for a
/// plugin it has bound. `settings` is a map of setting key to value, merged
/// over what the account set before; a key the plugin's schema does not
/// declare refuses the whole write.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SetPluginSettingsRequest {
    #[serde(with = "serde_bytes")]
    pub principal_id: Vec<u8>,
    pub settings: BTreeMap<String, Value>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SetPluginSettingsReply {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The plugin's `settings_schema` (`third-party.md` § The manifest: the
/// `BridgeProvider` settings vocabulary — an object of setting key to
/// `{ "label", "type", "value", "options" }`, `value` the default) read into
/// the settings rows a card renders, each value taken from `values` when the
/// account set one. An entry that is not an object is skipped, and a missing
/// `label` reads as the key.
pub fn plugin_settings_rows(
    schema: Option<&serde_json::Value>,
    values: &serde_json::Map<String, serde_json::Value>,
) -> Vec<crate::bridges_ui::BridgeSetting> {
    let Some(schema) = schema.and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    let to_value = |v: &serde_json::Value| serde_json::from_value::<Value>(v.clone()).ok();
    schema
        .iter()
        .filter_map(|(key, entry)| {
            let entry = entry.as_object()?;
            let value = values
                .get(key)
                .or_else(|| entry.get("value"))
                .and_then(to_value)
                .unwrap_or(Value::Null);
            let options = entry.get("options").and_then(|o| {
                o.as_array()?
                    .iter()
                    .map(|opt| {
                        Some(crate::bridges_ui::BridgeSettingOption {
                            value: to_value(opt.get("value")?)?,
                            label: opt.get("label")?.as_str()?.to_string(),
                            extra: BTreeMap::new(),
                        })
                    })
                    .collect::<Option<Vec<_>>>()
            });
            Some(crate::bridges_ui::BridgeSetting {
                key: key.clone(),
                label: entry
                    .get("label")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(key)
                    .to_string(),
                setting_type: entry
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("string")
                    .to_string(),
                value,
                options,
                extra: BTreeMap::new(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_rows_take_the_accounts_value_over_the_default() {
        let schema = serde_json::json!({
            "room": { "label": "Room", "type": "string", "value": "lobby" },
            "mode": {
                "label": "Mode", "type": "enum", "value": 0,
                "options": [{ "value": 0, "label": "Off" }, { "value": 1, "label": "On" }],
            },
            "bad": 7,
        });
        let mut values = serde_json::Map::new();
        values.insert("mode".into(), serde_json::json!(1));
        let rows = plugin_settings_rows(Some(&schema), &values);
        assert_eq!(rows.len(), 2, "a non-object entry is skipped");
        let mode = rows.iter().find(|r| r.key == "mode").unwrap();
        assert_eq!(mode.value, Value::Integer(1));
        assert_eq!(mode.options.as_ref().unwrap().len(), 2);
        let room = rows.iter().find(|r| r.key == "room").unwrap();
        assert_eq!(room.label, "Room");
        assert_eq!(room.value, Value::String("lobby".into()));
        assert!(plugin_settings_rows(None, &values).is_empty());
    }

    #[test]
    fn the_user_kinds_round_trip() {
        let req = SetPluginSettingsRequest {
            principal_id: vec![1; 16],
            settings: BTreeMap::from([("room".to_string(), Value::String("x".into()))]),
            extra: BTreeMap::new(),
        };
        let bytes = crate::encode_canonical(&req).unwrap();
        assert_eq!(
            crate::decode_strict::<SetPluginSettingsRequest>(&bytes).unwrap(),
            req
        );
        let row = AvailablePlugin {
            principal_id: vec![2; 16],
            binding_principal_id: Some(ByteBuf::from(vec![3; 16])),
            ..Default::default()
        };
        let bytes = crate::encode_canonical(&row).unwrap();
        assert_eq!(
            crate::decode_strict::<AvailablePlugin>(&bytes).unwrap(),
            row
        );
    }
}
