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
use fauna_core::localized::LocalizedText;
use fauna_protocol::bridges_ui::BridgeSetting;
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

/// The `bridge-plugin-publisher` text: the verified domain and the `did:key`,
/// the domain alone when the nest sent no key, or the development name — a
/// development plugin never paints an empty key.
#[must_use]
pub fn plugin_publisher_text(row: &InstalledPluginRow) -> LocalizedText {
    match (&row.publisher_key, &row.publisher_domain, row.development) {
        (_, _, true) => LocalizedText::key("bridges.plugin_publisher_development"),
        (Some(key), Some(domain), false) => LocalizedText::key_args(
            "bridges.plugin_publisher",
            [("domain", domain.as_str()), ("key", key.as_str())],
        ),
        (_, domain, false) => LocalizedText::key_arg(
            "bridges.plugin_state_other",
            "state",
            domain.as_deref().unwrap_or(&row.client_id),
        ),
    }
}

/// The `bridge-plugin-status` lines, top to bottom: the runner's state in
/// words (an unknown state verbatim), the fault count when any, the last
/// fault, and a stopped plugin's reason. Each resolves plainly — every
/// argument is data.
#[must_use]
pub fn plugin_status_lines(row: &InstalledPluginRow) -> Vec<LocalizedText> {
    let state = match row.state.as_str() {
        "running" => LocalizedText::key("bridges.plugin_state_running"),
        "restarting" => LocalizedText::key("bridges.plugin_state_restarting"),
        "stopped" => LocalizedText::key("bridges.plugin_state_stopped"),
        other => LocalizedText::key_arg("bridges.plugin_state_other", "state", other),
    };
    let mut lines = vec![state];
    if row.faults > 0 {
        lines.push(LocalizedText::key_arg(
            "bridges.plugin_faults",
            "count",
            row.faults.to_string(),
        ));
    }
    lines.extend(
        row.last_fault
            .as_deref()
            .map(|f| LocalizedText::key_arg("bridges.plugin_last_fault", "text", f)),
    );
    lines.extend(
        row.reason
            .as_deref()
            .map(|r| LocalizedText::key_arg("bridges.plugin_reason", "text", r)),
    );
    lines
}

/// One installed plugin as the apps that cannot call the composition
/// functions directly paint it (the UniFFI apps, web's wasm door): the row's
/// texts already composed, so a shell resolves two [`LocalizedText`]s through
/// its own string table and never re-composes a line (priority #2). The
/// direct-Rust apps (tui, linux) call [`plugin_publisher_text`] and
/// [`plugin_status_lines`] on the [`InstalledPluginRow`] itself.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstalledPluginText {
    /// The install row's id, hex — what [`PluginsClient::uninstall`] takes,
    /// through [`principal_id_from_hex`].
    pub principal_id_hex: String,
    pub name: String,
    pub client_id: String,
    /// The runner's state, verbatim — the `bridge-plugin-status` leaf's
    /// `state` attr.
    pub state: String,
    /// The `bridge-plugin-publisher` text.
    pub publisher: LocalizedText,
    /// The `bridge-plugin-status` lines, top to bottom.
    pub status: Vec<LocalizedText>,
}

/// The paint-ready texts of one listed plugin row.
#[must_use]
pub fn installed_plugin_text(row: &InstalledPluginRow) -> InstalledPluginText {
    InstalledPluginText {
        principal_id_hex: fauna_core::format::hex_full(&row.principal_id),
        name: row.name.clone(),
        client_id: row.client_id.clone(),
        state: row.state.clone(),
        publisher: plugin_publisher_text(row),
        status: plugin_status_lines(row),
    }
}

/// An install row's id from the hex [`InstalledPluginText::principal_id_hex`]
/// carries; `None` for anything that is not even-length hex.
#[must_use]
pub fn principal_id_from_hex(hex: &str) -> Option<Vec<u8>> {
    fauna_core::format::hex_decode(hex)
}

/// The verb a plugin card's one action button runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginCardAction {
    /// Unbound: [`PluginsClient::bind`] on the install row
    /// ([`PluginCard::principal_id`]), whose card the user then answers in
    /// Connected apps → Requests like every card.
    Connect,
    /// Bound: the roster's one verb, `fauna.principals.revoke`, on this
    /// account's binding row — never the install row, which only the admin's
    /// uninstall ends.
    Disconnect { binding_principal_id: Vec<u8> },
}

/// One account's settings card for an installed plugin on the Bridges page
/// (`third-party.md` § The principal model → *Hosted principals*), composed
/// once for all seven apps from a `fauna.plugins.available` row. Apps render
/// it as one more `bridge-card`: the name, the badge and the publisher as
/// text, [`Self::action`] as the card's action button, and
/// [`Self::settings`] through the settings rows every bridge card renders.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginCard {
    /// The install row's id — what [`PluginsClient::bind`] and
    /// [`PluginsClient::set_settings`] take.
    pub principal_id: Vec<u8>,
    /// The name the install card showed, else the `client_id`.
    pub name: String,
    /// The metadata document's URL, verbatim.
    pub client_id: String,
    /// The execution form, verbatim (`wasm` today).
    pub execution_form: String,
    /// The execution-form badge, in words.
    pub badge: LocalizedText,
    /// The publisher: the `client_id`'s host and the verified `did:key`.
    pub publisher: LocalizedText,
    pub action: PluginCardAction,
    /// The plugin's settings with this account's own values, labels fenced.
    /// Empty while unbound: `fauna.plugins.set_settings` refuses an account
    /// that has not bound the plugin, so the rows would be inert — the
    /// bridge card's own rule, which shows settings only while linked.
    pub settings: Vec<BridgeSetting>,
}

/// The settings card for one `fauna.plugins.available` row. Every
/// publisher-authored string is control-stripped here.
#[must_use]
pub fn plugin_card(p: AvailablePlugin) -> PluginCard {
    let strip = |s: &str| strip_control_chars(s).into_owned();
    let name = match &p.label {
        Some(label) if !label.trim().is_empty() => strip(label),
        _ => p.client_id.clone(),
    };
    let domain = fauna_protocol::kind_manifest::client_id_host(&p.client_id)
        .map(|d| strip(&d))
        .unwrap_or_else(|| p.client_id.clone());
    let key = p
        .publisher_key
        .as_ref()
        .and_then(|k| <[u8; 32]>::try_from(k.as_slice()).ok())
        .map(|k| fauna_protocol::kind_manifest::ed25519_did_key(&k));
    let publisher = match key {
        Some(key) => LocalizedText::key_args(
            "bridges.plugin_publisher",
            [("domain", domain.as_str()), ("key", key.as_str())],
        ),
        None => LocalizedText::key_arg("bridges.plugin_state_other", "state", domain),
    };
    let (action, settings) = match p.binding_principal_id {
        Some(binding) => (
            PluginCardAction::Disconnect {
                binding_principal_id: binding.into_vec(),
            },
            p.settings
                .into_iter()
                .map(|mut s| {
                    s.label = strip(&s.label);
                    for o in s.options.iter_mut().flatten() {
                        o.label = strip(&o.label);
                    }
                    s
                })
                .collect(),
        ),
        None => (PluginCardAction::Connect, Vec::new()),
    };
    PluginCard {
        principal_id: p.principal_id,
        name,
        badge: LocalizedText::key_arg("bridges.plugin_card_badge", "form", &p.execution_form),
        client_id: p.client_id,
        execution_form: p.execution_form,
        publisher,
        action,
        settings,
    }
}

/// One plugin settings card as the apps that cannot call [`plugin_card`]
/// directly paint it (the UniFFI apps, web's wasm door): [`PluginCard`] with
/// the two ids as hex, so a shell hands them back to
/// `ConnectedAppsMachine::{bind_plugin, revoke_principal}` and
/// `PluginsClient::set_settings` without holding bytes, and the action as the
/// `bound` flag the card's `bridge-action-button` carries. The direct-Rust
/// apps (tui, linux) read [`PluginCard`] itself.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginCardText {
    /// The install row's id, hex — what *Connect* and a settings edit take
    /// (through [`principal_id_from_hex`]).
    pub principal_id_hex: String,
    pub name: String,
    pub client_id: String,
    pub badge: LocalizedText,
    pub publisher: LocalizedText,
    /// `false` while unbound (the button reads Connect), `true` while bound
    /// (Disconnect).
    pub bound: bool,
    /// The account's own binding row, hex, while bound — what *Disconnect*
    /// takes; `None` while unbound.
    pub binding_principal_id_hex: Option<String>,
    pub settings: Vec<BridgeSetting>,
}

/// The paint-ready form of one settings card.
#[must_use]
pub fn plugin_card_text(card: PluginCard) -> PluginCardText {
    let binding_principal_id_hex = match &card.action {
        PluginCardAction::Connect => None,
        PluginCardAction::Disconnect {
            binding_principal_id,
        } => Some(fauna_core::format::hex_full(binding_principal_id)),
    };
    PluginCardText {
        principal_id_hex: fauna_core::format::hex_full(&card.principal_id),
        name: card.name,
        client_id: card.client_id,
        badge: card.badge,
        publisher: card.publisher,
        bound: binding_principal_id_hex.is_some(),
        binding_principal_id_hex,
        settings: card.settings,
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

    fn lookup(key: &str) -> Option<&'static str> {
        fauna_i18n::strings::lookup(key)
    }

    /// The paint-ready form carries the same texts the direct-Rust apps
    /// compose from the row, and its hex id round-trips to the install row's
    /// id — the one value an uninstall sends back.
    #[test]
    fn an_installed_plugin_text_is_the_rows_own_composition() {
        let row = installed_plugin_row(listed(Some([9u8; 32])));
        let text = installed_plugin_text(&row);
        assert_eq!(text.publisher, plugin_publisher_text(&row));
        assert_eq!(text.status, plugin_status_lines(&row));
        assert_eq!(
            (text.name.as_str(), text.state.as_str()),
            (row.name.as_str(), "restarting")
        );
        assert_eq!(
            principal_id_from_hex(&text.principal_id_hex),
            Some(row.principal_id)
        );
        assert_eq!(principal_id_from_hex("abc"), None);
        assert_eq!(principal_id_from_hex("zz"), None);
    }

    fn available(binding: Option<Vec<u8>>) -> AvailablePlugin {
        AvailablePlugin {
            principal_id: vec![3; 16],
            client_id: "https://plugins.example/hello.json".into(),
            label: Some("Hello\u{1b}[2J Plugin".into()),
            execution_form: "wasm".into(),
            publisher_key: Some(fauna_protocol::ByteBuf::from(vec![9u8; 32])),
            binding_principal_id: binding.map(fauna_protocol::ByteBuf::from),
            settings: vec![fauna_protocol::bridges_ui::BridgeSetting {
                key: "greeting".into(),
                label: "Greet\ning".into(),
                setting_type: "string".into(),
                value: Value::String("hi".into()),
                options: None,
                extra: Default::default(),
            }],
            ..Default::default()
        }
    }

    /// Unbound, the card offers *Connect* on the install row and no settings
    /// — `set_settings` refuses an account that has not bound the plugin, so
    /// the rows would be inert; it names the plugin and its publisher by the
    /// `client_id`'s host and the `did:key`, with every publisher string
    /// fenced.
    #[test]
    fn an_unbound_plugin_card_offers_connect_and_no_settings() {
        let card = plugin_card(available(None));
        assert_eq!(card.action, PluginCardAction::Connect);
        assert!(card.settings.is_empty());
        assert!(!card.name.chars().any(char::is_control), "{:?}", card.name);
        assert_eq!(
            card.publisher.resolve(lookup),
            format!(
                "plugins.example — {}",
                fauna_protocol::kind_manifest::ed25519_did_key(&[9; 32])
            )
        );
        assert_eq!(card.badge.resolve(lookup), "Plugin · wasm");
        assert_eq!(card.principal_id, vec![3; 16]);
    }

    /// Bound, the card's verb is the roster's one — *Disconnect* on the
    /// binding row, never the install row — and its settings are the
    /// account's own values, labels fenced.
    #[test]
    fn a_bound_plugin_card_disconnects_its_binding_and_shows_its_settings() {
        let card = plugin_card(available(Some(vec![7; 16])));
        assert_eq!(
            card.action,
            PluginCardAction::Disconnect {
                binding_principal_id: vec![7; 16]
            }
        );
        assert_eq!(card.settings.len(), 1);
        assert_eq!(card.settings[0].key, "greeting");
        assert!(!card.settings[0].label.chars().any(char::is_control));
        assert_eq!(card.settings[0].value, Value::String("hi".into()));
    }

    /// The paint-ready card carries the card's own composition, and its hex
    /// ids round-trip to the bytes the verbs take: unbound it names no
    /// binding, bound it names the account's binding row.
    #[test]
    fn a_plugin_card_text_is_the_cards_own_composition_with_hex_ids() {
        let unbound = plugin_card_text(plugin_card(available(None)));
        assert!(!unbound.bound);
        assert_eq!(unbound.binding_principal_id_hex, None);
        assert!(unbound.settings.is_empty());
        assert_eq!(
            principal_id_from_hex(&unbound.principal_id_hex),
            Some(vec![3; 16])
        );

        let card = plugin_card(available(Some(vec![7; 16])));
        let bound = plugin_card_text(card.clone());
        assert!(bound.bound);
        assert_eq!(
            bound
                .binding_principal_id_hex
                .as_deref()
                .and_then(principal_id_from_hex),
            Some(vec![7; 16])
        );
        assert_eq!((bound.name, bound.client_id), (card.name, card.client_id));
        assert_eq!(bound.publisher, card.publisher);
        assert_eq!(bound.settings, card.settings);
    }

    /// The publisher reads domain and key, or the development name; the
    /// status reads the state in words, then the faults, the last fault and
    /// the reason — and an unknown state paints verbatim, not a raw key.
    #[test]
    fn a_plugin_rows_publisher_and_status_read_in_words() {
        let key = [9u8; 32];
        let row = installed_plugin_row(listed(Some(key)));
        assert_eq!(
            plugin_publisher_text(&row).resolve(lookup),
            format!(
                "plugins.example — {}",
                fauna_protocol::kind_manifest::ed25519_did_key(&key)
            )
        );
        let status: Vec<String> = plugin_status_lines(&row)
            .iter()
            .map(|l| l.resolve(lookup))
            .collect();
        assert_eq!(status[0], "Restarting");
        assert_eq!(status[1], "Faults since it started: 2");
        assert!(status[2].starts_with("Last fault: trap"), "{status:?}");

        let mut dev = listed(None);
        dev.development = true;
        let dev = installed_plugin_row(dev);
        assert_eq!(
            plugin_publisher_text(&dev).resolve(lookup),
            "Development plugin"
        );

        let mut odd = listed(None);
        odd.status.state = "paused".into();
        odd.status.faults = 0;
        odd.status.last_fault = None;
        let odd = installed_plugin_row(odd);
        assert_eq!(
            plugin_status_lines(&odd)
                .iter()
                .map(|l| l.resolve(lookup))
                .collect::<Vec<_>>(),
            ["paused"]
        );
        assert_eq!(
            plugin_publisher_text(&odd).resolve(lookup),
            "plugins.example"
        );
    }
}
