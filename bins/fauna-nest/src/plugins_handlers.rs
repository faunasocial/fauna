//! WS-RPC handlers for nest-hosted plugins (`fauna.plugins.*`), per
//! `docs/goal/architecture/third-party.md` § The principal model → *Hosted
//! principals* and § The runner contract → *The install-approval leg*.
//!
//! ADMIN class: install, list, uninstall. The install leg is two acts:
//! `fauna.plugins.install` verifies everything and opens the one consent card
//! (assigned to the calling admin, with its install section), and the admin's
//! approval of that card through `fauna.bridges.atproto.resolve_consent` mints
//! the install row and starts the runner
//! (`bridge_atproto_handlers::resolve_install`). Install grants nothing over
//! any user's data; each user binds the plugin by their own consent.
//!
//! USER class, self-scoped: available, bind, set_settings — the user's half,
//! behind the plugin's settings card. `fauna.plugins.bind` opens the bind card
//! over the install's ceiling scopes, carrying the install's holder key and
//! verified manifest so the approving app mints the account's grant to the
//! plugin's key exactly as for every consent; the approval through
//! `resolve_consent` mints the binding row
//! (`bridge_atproto_handlers::bind_plugin`).

use std::sync::Arc;

use fauna_protocol::atproto_pds::{AtprotoConsentRequestedPush, ConsentInstallInfo};
use fauna_protocol::kind_manifest::{
    EXECUTION_FORM_WASM, IngressEntry, VerifiedManifest, ed25519_did_key,
    parse_development_manifest,
};
use fauna_protocol::plugins::{
    AvailablePlugin, BindPluginReply, BindPluginRequest, DEVELOPMENT_PLUGIN_NAME,
    DEVELOPMENT_PUBLISHER, InstallPluginReply, InstallPluginRequest, ListAvailablePluginsReply,
    ListAvailablePluginsRequest, ListPluginsReply, ListPluginsRequest, PluginInfo,
    SetPluginSettingsReply, SetPluginSettingsRequest, UninstallPluginReply, UninstallPluginRequest,
    development_client_id, is_development_client_id, plugin_settings_rows,
};
use fauna_protocol::{RpcError, decode_strict as decode};
use serde_bytes::ByteBuf;
use sha2::Digest as _;

use crate::bridge_atproto_handlers::{ConsentStart, open_consent_request, project_consent_row};
use crate::bridge_method_allowlist::require_permission_default as require_permission;
use crate::db::third_party_principals::{BindRefused, HostedPluginRow};
use crate::plugin_runner::{PendingInstall, PendingMint};
use crate::routes::AppState;
use crate::rpc_errors::{conflict_ns, encode_reply, internal, invalid_request_ns, malformed};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

fn refuse(why: impl std::fmt::Display) -> RpcError {
    invalid_request_ns("plugins", why)
}

/// `fauna.plugins.install` — resolve the document through the one resolver
/// (whose guarded fetch verified its `fauna` manifest against its host),
/// require the `wasm` execution form, fetch the module through the same
/// guarded fetcher capped at [`fauna_plugin_host::PLUGIN_MODULE_MAX_BYTES`],
/// refuse unless it hashes to the manifest's pinned digest, compile it once so
/// a malformed component refuses before any card, then open the install card.
///
/// Refusals name their reason: the caller is the nest's admin, not an
/// anonymous client, so the fetch-failure oracle `/oauth/par` guards against
/// does not apply.
fn install_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.plugins.install").await?;
            let req: InstallPluginRequest = decode(&payload).map_err(malformed)?;
            match (&req.development, req.document_url.is_empty()) {
                (None, false) => install(&state, actor_id, &req.document_url).await,
                (Some(dev), true) => install_development(&state, actor_id, &dev.module_path).await,
                _ => Err(refuse("name exactly one of document_url and development")),
            }
        })
    })
}

async fn install(
    state: &Arc<AppState>,
    admin: [u8; 32],
    document_url: &str,
) -> Result<bytes::Bytes, RpcError> {
    if state
        .db
        .get_hosted_plugin(document_url)
        .await
        .map_err(internal)?
        .is_some()
    {
        return Err(conflict_ns(
            "plugins",
            "this document is already installed on this nest",
        ));
    }
    let runtime = &state.oauth_as;
    let client = crate::oauth_as_client::resolve_client(
        &runtime.clients,
        runtime.fetcher.as_ref(),
        document_url,
        fauna_core::data::Timestamp::now_secs_or_zero(),
    )
    .await
    .map_err(|d| refuse(format!("{}: {}", d.error, d.description)))?;
    let manifest = crate::oauth_as_client::verified_manifest(&client)
        .map_err(|d| refuse(d.description))?
        .ok_or_else(|| refuse("the document carries no `fauna` manifest"))?;
    let exec = manifest
        .wasm_execution()
        .map_err(refuse)?
        .ok_or_else(|| refuse("the manifest's execution form is not wasm"))?;

    let module = runtime
        .fetcher
        .fetch_bytes(&exec.module, fauna_plugin_host::PLUGIN_MODULE_MAX_BYTES)
        .await
        .map_err(|e| refuse(format!("the module could not be fetched: {e}")))?;
    let digest: [u8; 32] = sha2::Sha256::digest(&module).into();
    if digest != exec.digest_bytes() {
        return Err(refuse(
            "the module does not hash to the manifest's pinned digest",
        ));
    }
    open_install_card(
        state,
        admin,
        InstallCandidate {
            client_id: client.client_id.clone(),
            client_name: client.client_name.clone(),
            declared_scopes: client.declared_scopes.clone(),
            manifest,
            module,
            module_digest: exec.digest.clone(),
            hosts: exec.hosts.clone(),
            development: false,
        },
    )
    .await
}

/// The cap on a development plugin's sibling `<stem>.fauna.json` — a manifest
/// object, far smaller than any module.
const DEVELOPMENT_MANIFEST_MAX_BYTES: usize = 64 * 1024;

/// The development form of `fauna.plugins.install` (`third-party.md`
/// § Developer loop): a component on the nest's own filesystem, admitted only
/// when the request arrived over a connection whose peer is a loopback
/// address — a dev nest is one you talk to from its own box; there is no
/// setting and no flag (`principles.md` § One configuration surface). The
/// nest synthesizes the document the way the loopback OAuth client is
/// synthesized, here and never through its resolver (so a development plugin
/// can never double as an OAuth client): the identity
/// `http://localhost/plugins/<stem>`, the displayed name
/// [`DEVELOPMENT_PLUGIN_NAME`], no signature, the loopback host as publisher,
/// and whatever an unsigned sibling `<stem>.fauna.json` declares — nothing
/// when it is absent. The same card opens. An already-installed stem is NOT
/// refused: approving the card replaces the module in place
/// (`PluginRunner::complete_install`), the edit-run cycle.
async fn install_development(
    state: &Arc<AppState>,
    admin: [u8; 32],
    module_path: &str,
) -> Result<bytes::Bytes, RpcError> {
    if !crate::dispatch_core::current_caller_is_loopback() {
        return Err(refuse(
            "the development form is admitted only from the nest's own box (a loopback peer)",
        ));
    }
    let path = std::path::Path::new(module_path);
    if !path.is_absolute() || path.extension().and_then(|e| e.to_str()) != Some("wasm") {
        return Err(refuse(
            "module_path must be an absolute path to a .wasm file",
        ));
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let client_id = development_client_id(stem).ok_or_else(|| {
        refuse("the module's file name must be one label of a-z 0-9 _ - before .wasm")
    })?;
    let module = read_capped(path, fauna_plugin_host::PLUGIN_MODULE_MAX_BYTES)
        .await
        .map_err(|e| refuse(format!("the module could not be read: {e}")))?
        .ok_or_else(|| refuse("no module file at module_path"))?;
    let sibling = path.with_file_name(format!("{stem}.fauna.json"));
    let payload = match read_capped(&sibling, DEVELOPMENT_MANIFEST_MAX_BYTES)
        .await
        .map_err(|e| refuse(format!("{stem}.fauna.json could not be read: {e}")))?
    {
        Some(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| refuse(format!("{stem}.fauna.json is not a JSON object")))?,
        None => serde_json::Map::from_iter([(
            "version".to_string(),
            serde_json::Value::from(fauna_protocol::kind_manifest::MANIFEST_VERSION),
        )]),
    };
    let manifest = parse_development_manifest(payload)
        .map_err(|e| refuse(format!("{stem}.fauna.json: {e}")))?;
    let hosts = manifest.development_hosts().map_err(refuse)?;
    let module_digest = format!("sha256:{}", hex::encode(sha2::Sha256::digest(&module)));
    open_install_card(
        state,
        admin,
        InstallCandidate {
            client_id,
            client_name: Some(DEVELOPMENT_PLUGIN_NAME.to_string()),
            // The loopback OAuth client's default, chosen here as its name is.
            declared_scopes: vec![fauna_bridge_atproto::authz::SCOPE_ATPROTO_BASE.to_string()],
            manifest,
            module,
            module_digest,
            hosts,
            development: true,
        },
    )
    .await
}

/// Read `path` whole when it is a regular file of at most `max` bytes —
/// `None` when nothing is there. The read itself is bounded, so a file that
/// grows (or a device posing as one) never reads past the cap.
async fn read_capped(path: &std::path::Path, max: usize) -> Result<Option<Vec<u8>>, String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        use std::io::Read as _;
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("not a regular file".into());
        }
        let mut bytes = Vec::new();
        file.take(u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > max {
            return Err(format!("larger than {max} bytes"));
        }
        Ok(Some(bytes))
    })
    .await
    .map_err(|e| format!("read task failed: {e}"))?
}

/// What either install form verified, ready for its card.
struct InstallCandidate {
    client_id: String,
    client_name: Option<String>,
    declared_scopes: Vec<String>,
    manifest: VerifiedManifest,
    module: Vec<u8>,
    module_digest: String,
    hosts: Vec<String>,
    development: bool,
}

/// Compile the module once, so a malformed component refuses before any
/// card, then open the install card both forms share and hold the verified
/// install for its approval.
async fn open_install_card(
    state: &Arc<AppState>,
    admin: [u8; 32],
    candidate: InstallCandidate,
) -> Result<bytes::Bytes, RpcError> {
    let InstallCandidate {
        client_id,
        client_name,
        declared_scopes,
        manifest,
        module,
        module_digest,
        hosts,
        development,
    } = candidate;
    let compiled = state
        .plugins
        .compile(module.clone())
        .await
        .map_err(|e| refuse(format!("the module is not a valid plugin component: {e:#}")))?;

    // The shape was refused at resolution (`kind_manifest::parse_ingress`);
    // what only the nest knows is which prefixes and names are its own or a
    // user's, and which an installed plugin already answers for.
    let paths: Vec<&str> = manifest
        .ingress
        .iter()
        .filter_map(IngressEntry::path)
        .collect();
    let labels: Vec<&str> = manifest
        .ingress
        .iter()
        .filter_map(IngressEntry::name)
        .collect();
    for path in &paths {
        if let Some(reserved) = crate::plugin_ingress::reserved_prefix_clash(path) {
            return Err(refuse(format!(
                "ingress path {path:?} collides with the nest's own {reserved:?}"
            )));
        }
    }
    // A whole name is `<label>.<apex>`, the apex being the live one the
    // whole-name gate matches against (the host resolver's, which follows a
    // post-boot claim): a domainless box has none to give, and a name a
    // user's site already answers on is the user's (the opt-in and the
    // custom-domain registration refuse the reverse).
    let apex = live_apex(state);
    if !labels.is_empty() && apex.is_empty() {
        return Err(refuse(format!(
            "the document claims the name {:?} under this nest's domain, and this nest has none",
            labels[0]
        )));
    }
    let names: Vec<String> = labels.iter().map(|l| format!("{l}.{apex}")).collect();
    if let Some(resolver) = &state.host_resolver {
        for name in &names {
            if resolver.user_site_for(name).await.is_some() {
                return Err(conflict_ns(
                    "plugins",
                    format!("{name} is a user's published site"),
                ));
            }
        }
    }
    let installed = state.db.list_hosted_plugins().await.map_err(internal)?;
    // A development reinstall replaces its own install, so its old paths are
    // not an overlap.
    for other in installed.iter().filter(|p| p.client_id != client_id) {
        for theirs in crate::plugin_runner::ingress_prefixes(&other.ingress) {
            if let Some(path) = paths
                .iter()
                .find(|p| p.starts_with(&theirs) || theirs.starts_with(*p))
            {
                return Err(conflict_ns(
                    "plugins",
                    format!(
                        "ingress path {path:?} overlaps {theirs:?}, which {} already serves",
                        other.client_id
                    ),
                ));
            }
        }
        for theirs in crate::plugin_runner::ingress_labels(&other.ingress) {
            if labels.contains(&theirs.as_str()) {
                return Err(conflict_ns(
                    "plugins",
                    format!("{theirs}.{apex} is already claimed by {}", other.client_id),
                ));
            }
        }
    }
    let ingress = manifest
        .payload
        .get("ingress")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
    let settings_schema = manifest.payload.get("settings_schema").cloned();
    let declared_kinds: Vec<String> = manifest.kinds.iter().map(|k| k.kind.to_string()).collect();
    let info = install_info(InstallFacts {
        kinds: &declared_kinds,
        scopes: &declared_scopes,
        settings_schema: settings_schema.as_ref(),
        publisher_domain: &manifest.publisher_domain,
        publisher_key: manifest.publisher_key.as_ref(),
        module_digest: &module_digest,
        hosts: &hosts,
        ingress: &ingress,
        development,
        apex: &apex,
    });

    let row = open_consent_request(
        state,
        ConsentStart::Install { admin },
        &client_id,
        client_name.as_deref(),
        &declared_scopes,
        &[],
        &crate::db::atproto_pds::ConsentBinding {
            attested: Default::default(),
            // The development manifest is unsigned: no JWS to carry.
            fauna_manifest: (!manifest.jws.is_empty()).then(|| manifest.jws.clone()),
        },
    )
    .await
    .map_err(internal)?
    .ok_or_else(|| internal("the install card did not open"))?;
    state.plugins.hold_pending(
        row.consent_id.clone(),
        PendingInstall {
            expires_at: row.expires_at,
            installed_by: admin,
            mint: PendingMint {
                client_id,
                label: client_name,
                publisher_key: manifest.publisher_key,
                declared_kinds,
                requested_scopes: declared_scopes.join(" "),
                module_digest,
                hosts,
                ingress,
                settings_schema,
                manifest_jws: manifest.jws.clone(),
            },
            module,
            compiled,
            info,
        },
    );
    let consent = project_consent_row(state, &row).await;
    // The admin's own other devices, as every assigned card fans out — sent
    // only now, so the push carries the install section.
    state.ws.notify_push(
        &admin,
        fauna_protocol::PushEvent::AtprotoConsentRequested(AtprotoConsentRequestedPush {
            consent: consent.clone(),
            extra: Default::default(),
        }),
    );
    encode_reply(&InstallPluginReply {
        consent,
        extra: Default::default(),
    })
}

/// What a plugin card's install section says, from the verified install.
struct InstallFacts<'a> {
    kinds: &'a [String],
    scopes: &'a [String],
    settings_schema: Option<&'a serde_json::Value>,
    publisher_domain: &'a str,
    /// `None` for a development plugin, whose manifest is unsigned.
    publisher_key: Option<&'a [u8; 32]>,
    module_digest: &'a str,
    hosts: &'a [String],
    ingress: &'a serde_json::Value,
    development: bool,
    /// The nest's live apex (`HostResolver::nest_domain`; empty on a
    /// domainless box), under which each whole-name label resolves.
    apex: &'a str,
}

/// The install section a plugin card carries — one composition for the
/// admin's install card and every user's bind card, so both say the same
/// thing about the same plugin.
fn install_info(f: InstallFacts<'_>) -> ConsentInstallInfo {
    ConsentInstallInfo {
        execution_form: EXECUTION_FORM_WASM.to_string(),
        requested_kinds: f.kinds.to_vec(),
        requested_scopes: f.scopes.to_vec(),
        settings: f
            .settings_schema
            .and_then(serde_json::Value::as_object)
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default(),
        // The card's typed summary, through the settings card's own parse;
        // no account has set a value yet, so each row carries the default.
        settings_fields: plugin_settings_rows(f.settings_schema, &serde_json::Map::new()),
        publisher_domain: f.publisher_domain.to_string(),
        // A development plugin has no key: empty, never a made-up one.
        publisher_key: f.publisher_key.map(ed25519_did_key).unwrap_or_default(),
        module_digest: f.module_digest.to_string(),
        hosts: f.hosts.to_vec(),
        ingress_paths: crate::plugin_runner::ingress_prefixes(f.ingress),
        development: f.development,
        ingress_names: crate::plugin_runner::ingress_labels(f.ingress)
            .iter()
            .map(|label| format!("{label}.{}", f.apex))
            .collect(),
        extra: Default::default(),
    }
}

/// The publisher's domain of an installed plugin: the verified manifest's,
/// which the install proved against the document's own host.
fn publisher_domain(plugin: &HostedPluginRow) -> Option<String> {
    if is_development_client_id(&plugin.client_id) {
        return Some(DEVELOPMENT_PUBLISHER.to_string());
    }
    let host = fauna_protocol::kind_manifest::client_id_host(&plugin.client_id)?;
    match plugin.manifest_jws.as_deref() {
        Some(jws) => fauna_protocol::kind_manifest::verify_manifest(jws, &host)
            .ok()
            .map(|m| m.publisher_domain),
        None => Some(host),
    }
}

/// A bind card's install section, rebuilt from the installed plugin's
/// persisted rows (`bridge_atproto_handlers::project_consent_row`). `None`
/// when no plugin of `client_id` is installed any more.
pub(crate) async fn installed_info(
    state: &AppState,
    client_id: &str,
) -> Option<ConsentInstallInfo> {
    let plugin = state.db.get_hosted_plugin(client_id).await.ok()??;
    // Empty for a development plugin, which the card shows keyless.
    let publisher_key = <[u8; 32]>::try_from(plugin.publisher_key.as_slice()).ok();
    let development = is_development_client_id(&plugin.client_id);
    if publisher_key.is_none() && !development {
        return None;
    }
    let scopes: Vec<String> = plugin
        .requested_scopes
        .split_whitespace()
        .map(str::to_string)
        .collect();
    Some(install_info(InstallFacts {
        kinds: &plugin.declared_kinds,
        scopes: &scopes,
        settings_schema: plugin.settings_schema.as_ref(),
        publisher_domain: &publisher_domain(&plugin).unwrap_or_default(),
        publisher_key: publisher_key.as_ref(),
        module_digest: &plugin.module_digest,
        hosts: &plugin.hosts,
        ingress: &plugin.ingress,
        development,
        apex: &live_apex(state),
    }))
}

/// The apex a whole-name claim resolves under: the host resolver's live
/// one (it follows a post-boot claim), empty on a domainless box.
fn live_apex(state: &AppState) -> String {
    state
        .host_resolver
        .as_ref()
        .map(|r| r.nest_domain().to_string())
        .unwrap_or_default()
}

/// The installed plugin whose install row is `principal_id`, or the one
/// refusal for an unknown id.
async fn installed(state: &AppState, principal_id: &[u8]) -> Result<HostedPluginRow, RpcError> {
    state
        .db
        .get_hosted_plugin_by_id(principal_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| refuse(BindRefused::NotInstalled))
}

/// `fauna.plugins.available` — every installed plugin as the caller's
/// settings card renders it: whether this account has bound it, and its
/// settings with this account's values. No runner state and no admin's
/// identity: those are the admin's `fauna.plugins.list`.
fn available_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.plugins.available").await?;
            let _req: ListAvailablePluginsRequest = decode(&payload).map_err(malformed)?;
            let bindings = state
                .db
                .list_third_party_principals(&actor_id)
                .await
                .map_err(internal)?;
            let mut plugins = Vec::new();
            for p in state.db.list_hosted_plugins().await.map_err(internal)? {
                let binding = bindings.iter().find(|b| b.client_id == p.client_id);
                let values: serde_json::Map<String, serde_json::Value> = match binding {
                    Some(_) => state
                        .db
                        .get_plugin_account_settings(&actor_id, &p.principal_id)
                        .await
                        .map_err(internal)?
                        .and_then(|v| serde_json::from_slice(&v).ok())
                        .unwrap_or_default(),
                    None => serde_json::Map::new(),
                };
                plugins.push(AvailablePlugin {
                    settings: plugin_settings_rows(p.settings_schema.as_ref(), &values),
                    binding_principal_id: binding.map(|b| ByteBuf::from(b.principal_id.clone())),
                    principal_id: p.principal_id,
                    client_id: p.client_id,
                    label: p.label,
                    execution_form: EXECUTION_FORM_WASM.to_string(),
                    publisher_key: (!p.publisher_key.is_empty())
                        .then(|| ByteBuf::from(p.publisher_key)),
                    declared_kinds: p.declared_kinds,
                    requested_scopes: p
                        .requested_scopes
                        .split_whitespace()
                        .map(str::to_string)
                        .collect(),
                    extra: Default::default(),
                });
            }
            encode_reply(&ListAvailablePluginsReply {
                plugins,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.plugins.bind` — open the caller's bind card for an installed
/// plugin: assigned to the caller, over the install's ceiling scopes, with
/// the install's holder key and the manifest the install verified on the row
/// (what the approving app's consent-time mint verifies and wraps to) and
/// the install section the admin's card showed. A plugin whose install kept
/// no manifest refuses: the admin installs it again.
fn bind_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.plugins.bind").await?;
            let req: BindPluginRequest = decode(&payload).map_err(malformed)?;
            let plugin = installed(&state, &req.principal_id).await?;
            let jws = plugin.manifest_jws.clone().ok_or_else(|| {
                refuse("this plugin's install kept no manifest; the admin installs it again")
            })?;
            let holder = <[u8; 32]>::try_from(plugin.holder_x25519.as_slice())
                .map_err(|_| internal("the install row holds no holder key"))?;
            let scopes: Vec<String> = plugin
                .requested_scopes
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let row = open_consent_request(
                &state,
                ConsentStart::PluginBind { actor: actor_id },
                &plugin.client_id,
                plugin.label.as_deref(),
                &scopes,
                &[],
                &crate::db::atproto_pds::ConsentBinding {
                    attested: crate::db::third_party_principals::AttestedKeys {
                        holder_x25519: Some(holder),
                        writer_ed25519: None,
                    },
                    fauna_manifest: Some(jws),
                },
            )
            .await
            .map_err(internal)?
            .ok_or_else(|| internal("the bind card did not open"))?;
            encode_reply(&BindPluginReply {
                consent: project_consent_row(&state, &row).await,
                extra: Default::default(),
            })
        })
    })
}

/// The ceiling on one account's settings for one plugin, encoded — a
/// settings card's handful of values, never a store.
const MAX_PLUGIN_SETTINGS_BYTES: usize = 16 * 1024;

/// `fauna.plugins.set_settings` — merge the caller's values over its own
/// settings for a plugin it has bound, into the plugin's state scope under
/// the account's key (`settings/<actor hex>`), where the plugin reads them —
/// never to the plugin directly. A key the plugin's schema does not declare
/// refuses the whole write.
fn set_settings_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.plugins.set_settings").await?;
            let req: SetPluginSettingsRequest = decode(&payload).map_err(malformed)?;
            let plugin = installed(&state, &req.principal_id).await?;
            let declared = plugin
                .settings_schema
                .as_ref()
                .and_then(serde_json::Value::as_object);
            if let Some(key) = req
                .settings
                .keys()
                .find(|k| !declared.is_some_and(|d| d.contains_key(*k)))
            {
                return Err(refuse(format!("the plugin declares no setting `{key}`")));
            }
            let mut merged: serde_json::Map<String, serde_json::Value> = state
                .db
                .get_plugin_account_settings(&actor_id, &plugin.principal_id)
                .await
                .map_err(internal)?
                .and_then(|v| serde_json::from_slice(&v).ok())
                .unwrap_or_default();
            for (key, value) in req.settings {
                let value = serde_json::to_value(&value)
                    .map_err(|e| malformed(format!("setting `{key}`: {e}")))?;
                merged.insert(key, value);
            }
            let bytes = serde_json::to_vec(&merged).map_err(internal)?;
            if bytes.len() > MAX_PLUGIN_SETTINGS_BYTES {
                return Err(refuse(format!(
                    "settings over the {MAX_PLUGIN_SETTINGS_BYTES}-byte cap"
                )));
            }
            state
                .db
                .put_plugin_account_settings(&actor_id, &plugin.principal_id, &bytes)
                .await
                .map_err(|e| match e.downcast_ref::<BindRefused>() {
                    Some(refused) => refuse(refused),
                    None => internal(e),
                })?;
            encode_reply(&SetPluginSettingsReply::default())
        })
    })
}

/// `fauna.plugins.list` — every installed plugin with its runner's state.
fn list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.plugins.list").await?;
            let _req: ListPluginsRequest = decode(&payload).map_err(malformed)?;
            let plugins = state
                .db
                .list_hosted_plugins()
                .await
                .map_err(internal)?
                .into_iter()
                .map(|p| PluginInfo {
                    publisher_domain: publisher_domain(&p),
                    development: is_development_client_id(&p.client_id),
                    status: state.plugins.status(&p.principal_id),
                    principal_id: p.principal_id,
                    client_id: p.client_id,
                    label: p.label,
                    execution_form: EXECUTION_FORM_WASM.to_string(),
                    publisher_key: (!p.publisher_key.is_empty())
                        .then(|| ByteBuf::from(p.publisher_key)),
                    declared_kinds: p.declared_kinds,
                    requested_scopes: p
                        .requested_scopes
                        .split_whitespace()
                        .map(str::to_string)
                        .collect(),
                    module_digest: p.module_digest,
                    hosts: p.hosts,
                    installed_by: p.installed_by,
                    installed_at: p.installed_at,
                    bound_accounts: u32::try_from(p.bound_accounts.len()).unwrap_or(u32::MAX),
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&ListPluginsReply {
                plugins,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.plugins.uninstall` — the admin's one verb: one transaction ends
/// every user's binding (the revoke cascade) and deletes the plugin's state,
/// hosted half and install row; then the runner stops and the plugin's
/// directory is deleted. An unknown id answers `uninstalled: false`.
fn uninstall_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.plugins.uninstall").await?;
            let req: UninstallPluginRequest = decode(&payload).map_err(malformed)?;
            let bound: Vec<[u8; 32]> = state
                .db
                .list_hosted_plugins()
                .await
                .map_err(internal)?
                .into_iter()
                .find(|p| p.principal_id == req.principal_id)
                .map(|p| p.bound_accounts)
                .unwrap_or_default();
            let ended = state
                .db
                .uninstall_hosted_plugin(&req.principal_id)
                .await
                .map_err(internal)?;
            // After the commit: the rows are gone, so a call the plugin makes
            // while it stops is already refused at the chokepoint.
            state.plugins.stop(&req.principal_id).await;
            let Some(ended) = ended else {
                return encode_reply(&UninstallPluginReply::default());
            };
            if ended.grants_ended > 0 {
                for account in &bound {
                    crate::bridge_atproto_handlers::notify_atproto_sessions_changed(
                        &state, account, None,
                    )
                    .await;
                }
            }
            if ended.capability_grants_ended > 0 {
                state.delegation_runner_wake.notify_one();
            }
            encode_reply(&UninstallPluginReply {
                uninstalled: true,
                bindings_ended: ended.bindings_ended,
                capability_grants_ended: ended.capability_grants_ended,
                extra: Default::default(),
            })
        })
    })
}

pub fn register_plugins_handlers(b: &mut RpcRouterBuilder) {
    use std::time::Duration;
    // The client-side registry's metadata (`fauna_protocol::kind`,
    // `register_plugins_kinds`): one kind, one wire contract.
    b.add(
        "fauna.plugins.install",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(120),
            handler: install_handler(),
        },
    );
    b.add(
        "fauna.plugins.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_handler(),
        },
    );
    b.add(
        "fauna.plugins.uninstall",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: uninstall_handler(),
        },
    );
    b.add(
        "fauna.plugins.available",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: available_handler(),
        },
    );
    b.add(
        "fauna.plugins.bind",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: bind_handler(),
        },
    );
    b.add(
        "fauna.plugins.set_settings",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_settings_handler(),
        },
    );
}
