//! WS-RPC handlers for nest-hosted plugins (`fauna.plugins.*`), per
//! `docs/goal/architecture/third-party.md` § The principal model → *Hosted
//! principals* and § The runner contract → *The install-approval leg*.
//!
//! ADMIN class, all three. The install leg is two acts: `fauna.plugins.install`
//! verifies everything and opens the one consent card (assigned to the calling
//! admin, with its install section), and the admin's approval of that card
//! through `fauna.bridges.atproto.resolve_consent` mints the install row and
//! starts the runner (`bridge_atproto_handlers::resolve_install`). Install
//! grants nothing over any user's data; each user binds the plugin by their
//! own consent.

use std::sync::Arc;

use fauna_protocol::atproto_pds::{AtprotoConsentRequestedPush, ConsentInstallInfo};
use fauna_protocol::kind_manifest::{EXECUTION_FORM_WASM, ed25519_did_key};
use fauna_protocol::plugins::{
    InstallPluginReply, InstallPluginRequest, ListPluginsReply, ListPluginsRequest, PluginInfo,
    UninstallPluginReply, UninstallPluginRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode};
use serde_bytes::ByteBuf;
use sha2::Digest as _;

use crate::bridge_atproto_handlers::{ConsentStart, open_consent_request, project_consent_row};
use crate::bridge_method_allowlist::require_permission_default as require_permission;
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
            install(&state, actor_id, &req.document_url).await
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
    let compiled = state
        .plugins
        .compile(module.clone())
        .await
        .map_err(|e| refuse(format!("the module is not a valid plugin component: {e:#}")))?;

    let ingress = manifest
        .payload
        .get("ingress")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
    let settings_schema = manifest.payload.get("settings_schema").cloned();
    let declared_kinds: Vec<String> = manifest.kinds.iter().map(|k| k.kind.to_string()).collect();
    let info = ConsentInstallInfo {
        execution_form: EXECUTION_FORM_WASM.to_string(),
        requested_kinds: declared_kinds.clone(),
        requested_scopes: client.declared_scopes.clone(),
        settings: settings_schema
            .as_ref()
            .and_then(serde_json::Value::as_object)
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default(),
        publisher_domain: manifest.publisher_domain.clone(),
        publisher_key: ed25519_did_key(&manifest.publisher_key),
        module_digest: exec.digest.clone(),
        hosts: exec.hosts.clone(),
        ingress_paths: ingress
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|e| e.get("path")?.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        extra: Default::default(),
    };

    let row = open_consent_request(
        state,
        ConsentStart::Install { admin },
        &client.client_id,
        client.client_name.as_deref(),
        &client.declared_scopes,
        &[],
        &crate::db::atproto_pds::ConsentBinding {
            attested: Default::default(),
            fauna_manifest: Some(manifest.jws.clone()),
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
                client_id: client.client_id.clone(),
                label: client.client_name.clone(),
                publisher_key: manifest.publisher_key,
                declared_kinds,
                requested_scopes: client.declared_scopes.join(" "),
                module_digest: exec.digest.clone(),
                hosts: exec.hosts.clone(),
                ingress,
                settings_schema,
            },
            module,
            compiled,
            info,
        },
    );
    let consent = project_consent_row(state, &row);
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
}
