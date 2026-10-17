//! Typed calls for the user's half of nest-hosted plugins — the settings
//! card a plugin gets on the Bridges page (`docs/goal/architecture/third-party.md`
//! § The principal model → *Hosted principals*; `docs/goal/behavior/bridges.md`
//! § Bridge settings).
//!
//! An admin's install grants nothing over any user's data, so each account
//! that wants a plugin binds it by its own consent: *Connect* is
//! [`PluginsClient::bind`], which opens the one consent card (approved
//! through `fauna.bridges.atproto.resolve_consent` like every card, after the
//! approving app's consent-time mint), and *Disconnect* is the roster's one
//! verb, `fauna.principals.revoke` on the row's
//! [`AvailablePlugin::binding_principal_id`]. The card's settings rows are the
//! `BridgeProvider` settings vocabulary every app already renders.
//!
//! The admin's half — the installed-plugins list on the Bridges page —
//! is [`PluginsClient::list`] / [`PluginsClient::install`] /
//! [`PluginsClient::uninstall`], and [`installed_plugin_row`] composes each
//! listed plugin's row once for all seven apps (`third-party.md` § The
//! principal model → *Hosted principals*: the install row is managed only by
//! the admin's `fauna.plugins.{install,list,uninstall}`).
//!
//! Transport-agnostic over [`RpcRequester`], wasm-clean; errors propagate as
//! the transport's `R::Error`.

use std::collections::BTreeMap;

use fauna_core::control_chars::strip_control_chars;
use fauna_protocol::plugins::{
    AvailablePlugin, BindPluginReply, BindPluginRequest, DEVELOPMENT_PLUGIN_NAME,
    InstallPluginReply, InstallPluginRequest, ListAvailablePluginsReply,
    ListAvailablePluginsRequest, ListPluginsReply, ListPluginsRequest, PluginInfo,
    SetPluginSettingsReply, SetPluginSettingsRequest, UninstallPluginReply, UninstallPluginRequest,
};
use fauna_protocol::{RpcRequester, Value};

pub struct PluginsClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> PluginsClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.plugins.available` — every plugin installed on this nest as
    /// this account's settings card shows it, oldest install first.
    pub async fn available(&self) -> Result<Vec<AvailablePlugin>, R::Error> {
        let reply: ListAvailablePluginsReply = self
            .nest
            .request(
                "fauna.plugins.available",
                ListAvailablePluginsRequest::default(),
            )
            .await?;
        Ok(reply.plugins)
    }

    /// `fauna.plugins.bind` — *Connect*: open this account's bind card for
    /// the plugin whose install row is `principal_id`.
    pub async fn bind(&self, principal_id: Vec<u8>) -> Result<BindPluginReply, R::Error> {
        self.nest
            .request(
                "fauna.plugins.bind",
                BindPluginRequest {
                    principal_id,
                    extra: BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.plugins.set_settings` — merge `settings` (setting key to value)
    /// over this account's own settings for a plugin it has bound.
    pub async fn set_settings(
        &self,
        principal_id: Vec<u8>,
        settings: BTreeMap<String, Value>,
    ) -> Result<SetPluginSettingsReply, R::Error> {
        self.nest
            .request(
                "fauna.plugins.set_settings",
                SetPluginSettingsRequest {
                    principal_id,
                    settings,
                    extra: BTreeMap::new(),
                },
            )
            .await
    }
}

impl<R: RpcRequester> PluginsClient<R> {
    /// `fauna.plugins.list` — ADMIN: every installed plugin with its
    /// runner's state, oldest install first.
    pub async fn list(&self) -> Result<Vec<PluginInfo>, R::Error> {
        let reply: ListPluginsReply = self
            .nest
            .request("fauna.plugins.list", ListPluginsRequest::default())
            .await?;
        Ok(reply.plugins)
    }

    /// `fauna.plugins.install` — ADMIN: resolve and verify the plugin's
    /// metadata document and open the install card (the reply's `consent`),
    /// which the admin answers like any card.
    pub async fn install(&self, document_url: String) -> Result<InstallPluginReply, R::Error> {
        self.nest
            .request(
                "fauna.plugins.install",
                InstallPluginRequest {
                    document_url,
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.plugins.uninstall` — ADMIN: every user's binding ends, the
    /// plugin's state and rows are deleted and its runner stops.
    pub async fn uninstall(&self, principal_id: Vec<u8>) -> Result<UninstallPluginReply, R::Error> {
        self.nest
            .request(
                "fauna.plugins.uninstall",
                UninstallPluginRequest {
                    principal_id,
                    extra: BTreeMap::new(),
                },
            )
            .await
    }
}

/// One row of the admin's installed-plugins list on the Bridges page,
/// composed once for all seven apps. Every publisher-authored or
/// runner-reported string is control-stripped here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPluginRow {
    /// The install row's id — what [`PluginsClient::uninstall`] takes.
    pub principal_id: Vec<u8>,
    /// The name the install card showed, else the `client_id`.
    pub name: String,
    /// The metadata document's URL, verbatim.
    pub client_id: String,
    /// The execution form — the badge (`wasm` today).
    pub execution_form: String,
    /// The publisher's verified domain, else the `client_id`'s host.
    pub publisher_domain: Option<String>,
    /// The publisher's `did:key`, from the listed key bytes. `None` for a
    /// development plugin, which has none and is named
    /// [`DEVELOPMENT_PLUGIN_NAME`] instead.
    pub publisher_key: Option<String>,
    pub development: bool,
    /// The runner's state (`running` | `restarting` | `stopped`, or a value
    /// an app does not know, which it renders through `reason` alone).
    pub state: String,
    /// Faults since the runner started it.
    pub faults: u32,
    pub last_fault: Option<String>,
    /// Why a stopped plugin is not running.
    pub reason: Option<String>,
    /// How many accounts have bound it by their own consent.
    pub bound_accounts: u32,
}

/// The Bridges page's row for one listed plugin.
#[must_use]
pub fn installed_plugin_row(p: PluginInfo) -> InstalledPluginRow {
    let strip = |s: String| strip_control_chars(&s).into_owned();
    let development = p.development;
    let publisher_key = p
        .publisher_key
        .filter(|_| !development)
        .and_then(|k| <[u8; 32]>::try_from(k.as_slice()).ok())
        .map(|k| fauna_protocol::kind_manifest::ed25519_did_key(&k));
    let publisher_domain = p
        .publisher_domain
        .or_else(|| fauna_protocol::kind_manifest::client_id_host(&p.client_id))
        .map(strip);
    let name = match p.label {
        Some(label) if !label.trim().is_empty() => strip(label),
        _ if development => DEVELOPMENT_PLUGIN_NAME.to_string(),
        _ => p.client_id.clone(),
    };
    InstalledPluginRow {
        principal_id: p.principal_id,
        name,
        client_id: p.client_id,
        execution_form: p.execution_form,
        publisher_domain,
        publisher_key,
        development,
        state: p.status.state,
        faults: p.status.faults,
        last_fault: p.status.last_fault.map(strip),
        reason: p.status.reason.map(strip),
        bound_accounts: p.bound_accounts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{RecordingRequester, block_on};

    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.plugins.available" => {
                fauna_protocol::encode_canonical(&ListAvailablePluginsReply::default())
            }
            "fauna.plugins.bind" => fauna_protocol::encode_canonical(&BindPluginReply::default()),
            "fauna.plugins.set_settings" => {
                fauna_protocol::encode_canonical(&SetPluginSettingsReply::default())
            }
            "fauna.plugins.list" => fauna_protocol::encode_canonical(&ListPluginsReply::default()),
            "fauna.plugins.install" => {
                fauna_protocol::encode_canonical(&InstallPluginReply::default())
            }
            "fauna.plugins.uninstall" => {
                fauna_protocol::encode_canonical(&UninstallPluginReply::default())
            }
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    /// Each verb names its kind and carries exactly the request the nest's
    /// handler decodes.
    #[test]
    fn each_verb_sends_its_kind_and_request() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = PluginsClient::new(rec.clone());
        assert!(block_on(client.available()).unwrap().is_empty());

        block_on(client.bind(vec![1; 16])).unwrap();
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.plugins.bind");
        let bind: BindPluginRequest = fauna_protocol::decode_strict(&payload).unwrap();
        assert_eq!(bind.principal_id, vec![1; 16]);

        let settings = BTreeMap::from([("greeting".to_string(), Value::String("hi".into()))]);
        block_on(client.set_settings(vec![1; 16], settings.clone())).unwrap();
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.plugins.set_settings");
        let set: SetPluginSettingsRequest = fauna_protocol::decode_strict(&payload).unwrap();
        assert_eq!(set.settings, settings);

        assert_eq!(
            rec.kinds(),
            [
                "fauna.plugins.available",
                "fauna.plugins.bind",
                "fauna.plugins.set_settings"
            ]
        );
    }

    /// The admin's three verbs name their kinds and carry exactly what the
    /// nest's handlers decode.
    #[test]
    fn the_admin_verbs_send_their_kinds_and_requests() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = PluginsClient::new(rec.clone());
        assert!(block_on(client.list()).unwrap().is_empty());

        block_on(client.install("https://plugins.example/hello.json".into())).unwrap();
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.plugins.install");
        let install: InstallPluginRequest = fauna_protocol::decode_strict(&payload).unwrap();
        assert_eq!(install.document_url, "https://plugins.example/hello.json");
        assert_eq!(install.development, None);

        block_on(client.uninstall(vec![2; 16])).unwrap();
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.plugins.uninstall");
        let un: UninstallPluginRequest = fauna_protocol::decode_strict(&payload).unwrap();
        assert_eq!(un.principal_id, vec![2; 16]);
    }

    fn listed(publisher_key: Option<[u8; 32]>) -> PluginInfo {
        PluginInfo {
            principal_id: vec![3; 16],
            client_id: "https://plugins.example/hello.json".into(),
            label: Some("Hello\nPlugin".into()),
            execution_form: "wasm".into(),
            publisher_key: publisher_key.map(|k| fauna_protocol::ByteBuf::from(k.to_vec())),
            publisher_domain: Some("plugins.example".into()),
            status: fauna_protocol::plugins::PluginStatus {
                state: "restarting".into(),
                faults: 2,
                last_fault: Some("trap\u{1b}[2J".into()),
                ..Default::default()
            },
            bound_accounts: 1,
            ..Default::default()
        }
    }

    /// The row names the publisher by its verified domain and its `did:key`
    /// (built from the listed key bytes), and fences the name and the fault
    /// text the runner reported.
    #[test]
    fn a_listed_plugin_row_names_its_publisher_and_fences_its_text() {
        let key = [9u8; 32];
        let row = installed_plugin_row(listed(Some(key)));
        assert_eq!(row.publisher_domain.as_deref(), Some("plugins.example"));
        assert_eq!(
            row.publisher_key,
            Some(fauna_protocol::kind_manifest::ed25519_did_key(&key))
        );
        assert!(!row.name.chars().any(char::is_control), "{:?}", row.name);
        assert!(row.name.contains("Plugin"));
        let fault = row.last_fault.unwrap();
        assert!(!fault.chars().any(char::is_control), "{fault:?}");
        assert_eq!((row.state.as_str(), row.faults), ("restarting", 2));
        assert_eq!(row.bound_accounts, 1);
        assert!(!row.development);
    }

    /// A development plugin has no `did:key` row; a nest that sends no domain
    /// falls back to the `client_id`'s host.
    #[test]
    fn a_development_plugin_has_no_key_and_a_missing_domain_falls_back() {
        let mut dev = listed(None);
        dev.development = true;
        dev.label = None;
        dev.client_id = "http://localhost/plugins/hello_dev".into();
        dev.publisher_domain = Some("localhost".into());
        let row = installed_plugin_row(dev);
        assert_eq!(row.publisher_key, None);
        assert_eq!(row.name, DEVELOPMENT_PLUGIN_NAME);
        assert!(row.development);

        let mut old = listed(Some([9; 32]));
        old.publisher_domain = None;
        assert_eq!(
            installed_plugin_row(old).publisher_domain.as_deref(),
            Some("plugins.example")
        );
    }
}
