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
/// fetch and verify its module, and open the install card.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct InstallPluginRequest {
    /// The plugin's Client ID Metadata Document URL — its identity.
    pub document_url: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
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
