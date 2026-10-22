//! The nest-hosted WASM plugin, end to end (`docs/goal/architecture/third-party.md`
//! § The runner contract → *The install-approval leg*; § The principal model →
//! *Hosted principals*; § Execution forms → *WASM components* → *The contract
//! as built*).
//!
//! Over the in-repo hello plugin (`fauna_plugin_host::fixture::hello_plugin`)
//! and a test fetcher behind the nest's one guarded-fetch seam: an admin's
//! `fauna.plugins.install` verifies the document and its module and opens the
//! install card; the admin's approval mints the install row, writes the
//! plugin's files and starts the runner; the plugin's `start` reaches the nest
//! through the principal chokepoint; a runaway ingress call faults without
//! taking the nest with it and the plugin comes back; a user binds it from
//! the settings card's start (`fauna.plugins.bind`) — a card carrying the
//! install's key and manifest, approved into a binding row with no grant
//! family — and that row is the one the plugin then calls under; the user's
//! settings rest in the plugin's state scope; the user's revoke ends the
//! binding and leaves the plugin installed; uninstall leaves nothing behind.
//! Over the public router: the declared ingress prefixes reach the plugin
//! with the nest-asserted actor (`security.md` § Co-resident process trust
//! boundary → *Hosted third-party code*), and a prefix the nest serves
//! refuses at install.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fauna_nest::db::CacheDb;
use fauna_nest::db::third_party_principals::{ExecutionForm, NEST_OWNER_ACTOR};
use fauna_nest::oauth_as_client::{ClientMetadataFetcher, MetadataFetchError};
use fauna_nest::plugin_runner::{HOLDER_FILE, MODULE_FILE, PluginRunner};
use fauna_nest::routes::AppState;
use fauna_plugin_host::{HttpRequest, HttpResponse, IngressRequest};
use fauna_protocol::Value;
use fauna_protocol::atproto_pds::{ResolveConsentReply, ResolveConsentRequest};
use fauna_protocol::kind_manifest::{ed25519_did_key, sign_manifest};
use fauna_protocol::plugins::{
    BindPluginReply, BindPluginRequest, DevelopmentInstall, InstallPluginReply,
    InstallPluginRequest, ListAvailablePluginsReply, ListAvailablePluginsRequest, ListPluginsReply,
    ListPluginsRequest, PLUGIN_STATE_RUNNING, PLUGIN_STATE_STOPPED, SetPluginSettingsReply,
    SetPluginSettingsRequest, UninstallPluginReply, UninstallPluginRequest,
};
use fauna_protocol::principals::{RevokePrincipalReply, RevokePrincipalRequest};
use fauna_protocol::web::{WebSetSubdomainEnabledReply, WebSetSubdomainEnabledRequest};
use fauna_protocol::{RpcError, decode_strict, encode_canonical};
use sha2::Digest as _;

const DOC_URL: &str = "https://plugins.example/hello.json";
const MODULE_URL: &str = "https://plugins.example/hello.wasm";
const ADMIN: [u8; 32] = [0xAD; 32];
const USER: [u8; 32] = [0x5E; 32];
/// The ceiling the document declares: its own kinds, and the AT Protocol
/// transport scope.
const SCOPES: &str = "atproto fauna:records:rw:ext.plugins.example.*";

/// The guarded-fetch seam's fake: serves the document and the module, and
/// records every URL any of the three methods was asked for.
struct TestFetcher {
    doc: String,
    module: Vec<u8>,
    calls: Mutex<Vec<String>>,
}

impl TestFetcher {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl ClientMetadataFetcher for TestFetcher {
    async fn fetch(&self, url: &str) -> Result<String, MetadataFetchError> {
        self.calls.lock().unwrap().push(url.to_string());
        match url {
            DOC_URL => Ok(self.doc.clone()),
            _ => Err(MetadataFetchError::Status(404)),
        }
    }

    async fn fetch_bytes(&self, url: &str, max: usize) -> Result<Vec<u8>, MetadataFetchError> {
        self.calls.lock().unwrap().push(url.to_string());
        match url {
            MODULE_URL if self.module.len() <= max => Ok(self.module.clone()),
            MODULE_URL => Err(MetadataFetchError::TooLarge),
            _ => Err(MetadataFetchError::Status(404)),
        }
    }

    async fn request(
        &self,
        req: HttpRequest,
        _max: usize,
    ) -> Result<HttpResponse, MetadataFetchError> {
        self.calls.lock().unwrap().push(req.url);
        Err(MetadataFetchError::Network)
    }
}

/// The plugin's document: standard client metadata plus a `fauna` manifest
/// signed by the document host's publisher key, `execution` naming the
/// module by URL and digest, no outbound hosts, one setting, and `ingress`
/// as given.
fn document(module: &[u8], ingress: serde_json::Value) -> (String, [u8; 32]) {
    let key = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
    let digest = format!("sha256:{}", hex::encode(sha2::Sha256::digest(module)));
    let jws = sign_manifest(
        &key,
        &serde_json::json!({
            "version": 1,
            "publisher": {
                "domain": "plugins.example",
                "key": ed25519_did_key(&key.verifying_key().to_bytes()),
            },
            "execution": { "form": "wasm", "module": MODULE_URL, "digest": digest, "hosts": [] },
            "kinds": [{
                "kind": "ext.plugins.example.notes",
                "class": "state", "merge": "latest-wins", "floor": "none",
            }],
            "settings_schema": {
                "greeting": { "label": "Greeting", "type": "string", "value": "hi" },
            },
            "ingress": ingress,
        }),
        None,
    );
    let doc = serde_json::json!({
        "client_id": DOC_URL,
        "client_name": "Hello Plugin",
        "redirect_uris": ["https://plugins.example/cb"],
        "scope": SCOPES,
        "grant_types": ["authorization_code"],
        "response_types": ["code"],
        "application_type": "web",
        "dpop_bound_access_tokens": true,
        "fauna": jws,
    })
    .to_string();
    (doc, key.verifying_key().to_bytes())
}

/// The kind families this journey drives: `fauna.plugins.*`, the card's
/// `resolve_consent`, the user's roster verb, and `fauna.web.*` (the
/// subdomain opt-in a plugin's whole-name claim must refuse).
fn router() -> fauna_nest::rpc_router::RpcRouter {
    let mut b = fauna_nest::rpc_router::RpcRouter::builder();
    fauna_nest::plugins_handlers::register_plugins_handlers(&mut b);
    fauna_nest::bridge_atproto_handlers::register_bridge_atproto_handlers(&mut b);
    fauna_nest::principals_handlers::register_principals_handlers(&mut b);
    fauna_nest::web_handlers::register_web_handlers(&mut b);
    b.build()
}

async fn call<Req: serde::Serialize, Reply: serde::de::DeserializeOwned>(
    state: &Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    req: &Req,
) -> Result<Reply, RpcError> {
    let meta = state.rpc_router.kind_meta(kind).expect("kind registered");
    let reply = (meta.handler)(state.clone(), actor, encode_canonical(req).unwrap()).await?;
    Ok(decode_strict(&reply).unwrap())
}

async fn eventually(what: &str, mut ok: impl AsyncFnMut() -> bool) {
    for _ in 0..200 {
        if ok().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn state_byte(state: &AppState, principal_id: &[u8], key: &str) -> Option<Vec<u8>> {
    state.db.plugin_state_get(principal_id, key).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn install_run_fault_bind_and_uninstall_a_hosted_plugin() {
    let module = fauna_plugin_host::fixture::hello_plugin().unwrap();
    let (doc, publisher_key) = document(&module, serde_json::json!([]));
    let fetcher = Arc::new(TestFetcher {
        doc,
        module,
        calls: Mutex::new(Vec::new()),
    });
    let data = tempfile::tempdir().unwrap();
    let plugins_dir = data.path().join("plugins");
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        oauth_as: Arc::new(fauna_nest::oauth_as_state::OAuthAsRuntime::new(
            fauna_core::data::Timestamp::now_secs_or_zero(),
            fetcher.clone(),
        )),
        plugins: Arc::new(PluginRunner::new(Some(plugins_dir.clone()))),
        rpc_router: Arc::new(router()),
        ..AppState::for_test(db.clone())
    });
    for actor in [ADMIN, USER] {
        db.create_user(&actor, "free", "test").await.unwrap();
    }
    db.add_admin_actor(&ADMIN).await.unwrap();

    // ── Install: the card opens, assigned to the admin, with its section ──
    let installed: InstallPluginReply = call(
        &state,
        ADMIN,
        "fauna.plugins.install",
        &InstallPluginRequest {
            document_url: DOC_URL.into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let card = installed.consent;
    let section = card
        .install
        .clone()
        .expect("the card carries its install section");
    assert_eq!(section.execution_form, "wasm");
    assert_eq!(section.requested_kinds, ["ext.plugins.example.notes"]);
    assert_eq!(section.publisher_domain, "plugins.example");
    assert_eq!(section.publisher_key, ed25519_did_key(&publisher_key));
    assert!(section.hosts.is_empty());
    // The settings summary is typed, through the settings card's own parse.
    assert_eq!(section.settings, ["greeting"]);
    assert_eq!(section.settings_fields.len(), 1);
    assert_eq!(section.settings_fields[0].key, "greeting");
    assert_eq!(section.settings_fields[0].label, "Greeting");
    assert_eq!(section.settings_fields[0].setting_type, "string");
    assert!(!section.development);
    assert!(!section.bind, "an install card is not a bind card");
    assert_eq!(fetcher.calls(), [DOC_URL, MODULE_URL]);

    // A user may not install, and may not approve an install card.
    assert!(
        call::<_, InstallPluginReply>(
            &state,
            USER,
            "fauna.plugins.install",
            &InstallPluginRequest {
                document_url: DOC_URL.into(),
                ..Default::default()
            },
        )
        .await
        .is_err()
    );
    let approve = ResolveConsentRequest {
        consent_id: card.consent_id.clone(),
        approved: true,
        ..Default::default()
    };
    assert!(
        call::<_, ResolveConsentReply>(
            &state,
            USER,
            "fauna.bridges.atproto.resolve_consent",
            &approve
        )
        .await
        .is_err(),
        "a non-admin's approval refuses"
    );

    // ── Approval: the install row, the files, the runner ──
    let resolved: ResolveConsentReply = call(
        &state,
        ADMIN,
        "fauna.bridges.atproto.resolve_consent",
        &approve,
    )
    .await
    .unwrap();
    assert!(resolved.resolved);
    // Only the development form replaces on reinstall; a document stays
    // installed until it is uninstalled.
    assert!(
        call::<_, InstallPluginReply>(
            &state,
            ADMIN,
            "fauna.plugins.install",
            &InstallPluginRequest {
                document_url: DOC_URL.into(),
                ..Default::default()
            },
        )
        .await
        .is_err_and(|e| e.code.contains("conflict")),
        "an installed document refuses a second install"
    );
    let plugins = db.list_hosted_plugins().await.unwrap();
    assert_eq!(plugins.len(), 1);
    let plugin = &plugins[0];
    assert_eq!(plugin.client_id, DOC_URL);
    assert_eq!(plugin.publisher_key, publisher_key.to_vec());
    assert_eq!(plugin.declared_kinds, ["ext.plugins.example.notes"]);
    assert_eq!(plugin.installed_by, ADMIN.to_vec());
    let install_row = db
        .list_third_party_principals(&NEST_OWNER_ACTOR)
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.principal_id == plugin.principal_id)
        .expect("the install row under the nest owner");
    assert_eq!(install_row.execution_form, ExecutionForm::Wasm.as_str());
    let pid = plugin.principal_id.clone();
    let dir = plugins_dir.join(hex::encode(&pid));
    assert!(dir.join(MODULE_FILE).is_file());
    assert!(dir.join(HOLDER_FILE).is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(dir.join(HOLDER_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the holder secret is owner-only");
    }

    let list = |state: Arc<AppState>| async move {
        call::<_, ListPluginsReply>(
            &state,
            ADMIN,
            "fauna.plugins.list",
            &ListPluginsRequest::default(),
        )
        .await
        .unwrap()
        .plugins
    };
    eventually("the plugin running with its start done", async || {
        list(state.clone()).await[0].status.state == PLUGIN_STATE_RUNNING
            && state_byte(&state, &pid, "http").await.is_some()
    })
    .await;
    // The Bridges page's list names the publisher by its verified domain.
    let listed = list(state.clone()).await;
    assert_eq!(
        listed[0].publisher_domain.as_deref(),
        Some("plugins.example")
    );
    assert_eq!(
        listed[0].publisher_key.as_ref().map(|k| k.to_vec()),
        Some(publisher_key.to_vec())
    );
    assert!(!listed[0].development);

    // ── What `start` did, through the nest ──
    assert_eq!(
        state_byte(&state, &pid, "pub").await.unwrap(),
        plugin.holder_x25519,
        "the plugin read the holder key the install minted"
    );
    assert_eq!(
        state_byte(&state, &pid, "fetch").await.unwrap(),
        [0],
        "fauna.capabilities.fetch, a session kind, served for the install row"
    );
    assert_eq!(
        state_byte(&state, &pid, "refused").await.unwrap(),
        [1],
        "fauna.feed.posts is outside the ThirdParty ceiling"
    );
    assert_eq!(
        state_byte(&state, &pid, "http").await.unwrap(),
        [1],
        "the undeclared host was refused"
    );
    assert_eq!(
        fetcher.calls(),
        [DOC_URL, MODULE_URL],
        "…before any dial: the fetcher saw no third request"
    );

    // ── A runaway call faults; the nest keeps serving; the plugin returns ──
    let spun = state
        .plugins
        .ingress(
            &pid,
            IngressRequest {
                method: "GET".into(),
                path: "/spin".into(),
                ..Default::default()
            },
        )
        .await;
    assert!(spun.is_err(), "the runaway call ends as a fault: {spun:?}");
    let after = list(state.clone()).await;
    assert_eq!(
        after[0].status.faults, 1,
        "the nest still answers a kind call"
    );
    assert!(after[0].status.last_fault.is_some());
    eventually("the plugin re-instantiated", async || {
        list(state.clone()).await[0].status.state == PLUGIN_STATE_RUNNING
    })
    .await;
    let who = state
        .plugins
        .ingress(
            &pid,
            IngressRequest {
                method: "GET".into(),
                path: "/whoami".into(),
                actor: Some(USER),
                ..Default::default()
            },
        )
        .await
        .expect("a fresh instance serves");
    assert_eq!(who.status, 200);
    assert_eq!(who.body, USER.to_vec());

    // ── The settings card: the user sees the plugin, unbound ──
    let available = |state: Arc<AppState>| async move {
        call::<_, ListAvailablePluginsReply>(
            &state,
            USER,
            "fauna.plugins.available",
            &ListAvailablePluginsRequest::default(),
        )
        .await
        .unwrap()
        .plugins
    };
    let card_row = available(state.clone()).await;
    assert_eq!(card_row.len(), 1);
    assert_eq!(card_row[0].principal_id, pid);
    assert_eq!(card_row[0].execution_form, "wasm");
    assert!(card_row[0].binding_principal_id.is_none());
    assert_eq!(card_row[0].settings.len(), 1);
    assert_eq!(card_row[0].settings[0].key, "greeting");
    assert_eq!(card_row[0].settings[0].value, Value::String("hi".into()));
    // Nothing to configure before the account binds it.
    let greeting = |v: &str| SetPluginSettingsRequest {
        principal_id: pid.clone(),
        settings: BTreeMap::from([("greeting".to_string(), Value::String(v.into()))]),
        extra: BTreeMap::new(),
    };
    assert!(
        call::<_, SetPluginSettingsReply>(
            &state,
            USER,
            "fauna.plugins.set_settings",
            &greeting("hello")
        )
        .await
        .is_err()
    );

    // ── Connect: the bind card, assigned to the user, mint-ready ──
    let bound: BindPluginReply = call(
        &state,
        USER,
        "fauna.plugins.bind",
        &BindPluginRequest {
            principal_id: pid.clone(),
            extra: BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let card = bound.consent;
    assert_eq!(card.client_id, DOC_URL);
    assert_eq!(card.scopes, SCOPES.split(' ').collect::<Vec<_>>());
    let section = card
        .install
        .clone()
        .expect("the bind card's install section");
    assert_eq!(section.execution_form, "wasm");
    assert_eq!(section.publisher_domain, "plugins.example");
    assert_eq!(section.settings, ["greeting"]);
    assert!(
        section.bind,
        "the section says it rides a bind card, whose approve mints the grant"
    );
    assert_eq!(
        card.holder_x25519.as_deref().map(Vec::as_slice),
        Some(plugin.holder_x25519.as_slice()),
        "the card wraps to the plugin's one key"
    );
    // What the approving app's consent-time mint reads: the manifest
    // verifies against the document's host and the grant wraps to the
    // plugin's key over its declared kinds.
    let plan = fauna_client_capabilities::ext_consent::plan_ext_consent(
        &card,
        &fauna_protocol::merge_policy::AdmittedKinds::new(),
    )
    .expect("the card plans a grant")
    .expect("a records scope plans a grant");
    assert_eq!(plan.holder.to_vec(), plugin.holder_x25519);
    assert_eq!(
        plan.kinds
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["ext.plugins.example.notes"]
    );
    // The card survives in the user's pending list with its section rebuilt.
    let pending: fauna_protocol::atproto_pds::ListPendingConsentsReply = call(
        &state,
        USER,
        "fauna.bridges.atproto.list_pending_consents",
        &fauna_protocol::atproto_pds::ListPendingConsentsRequest::default(),
    )
    .await
    .unwrap();
    assert_eq!(pending.consents.len(), 1);
    assert_eq!(pending.consents[0].install, card.install);

    // ── Approve: the binding row, no grant family ──
    let resolved: ResolveConsentReply = call(
        &state,
        USER,
        "fauna.bridges.atproto.resolve_consent",
        &ResolveConsentRequest {
            consent_id: card.consent_id.clone(),
            approved: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(resolved.resolved);
    let binding = db
        .list_third_party_principals(&USER)
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.client_id == DOC_URL)
        .expect("the user's binding row");
    assert_eq!(binding.execution_form, ExecutionForm::Wasm.as_str());
    assert_eq!(binding.holder_x25519, Some(plugin.holder_x25519.clone()));
    assert_eq!(binding.granted_scopes, SCOPES);
    assert_eq!(binding.live_grants, 0, "a hosted plugin holds no token");
    assert!(
        db.list_atproto_oauth_grants(&USER)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        available(state.clone()).await[0]
            .binding_principal_id
            .as_deref()
            .map(Vec::as_slice),
        Some(binding.principal_id.as_slice())
    );

    // ── The user's settings rest in the plugin's state scope ──
    call::<_, SetPluginSettingsReply>(
        &state,
        USER,
        "fauna.plugins.set_settings",
        &greeting("hello"),
    )
    .await
    .unwrap();
    let settings_key = fauna_nest::db::third_party_principals::plugin_settings_key(&USER);
    assert_eq!(
        state_byte(&state, &pid, &settings_key).await.unwrap(),
        br#"{"greeting":"hello"}"#.to_vec()
    );
    assert_eq!(
        available(state.clone()).await[0].settings[0].value,
        Value::String("hello".into())
    );
    let undeclared = SetPluginSettingsRequest {
        principal_id: pid.clone(),
        settings: BTreeMap::from([("colour".to_string(), Value::Bool(true))]),
        extra: BTreeMap::new(),
    };
    assert!(
        call::<_, SetPluginSettingsReply>(&state, USER, "fauna.plugins.set_settings", &undeclared)
            .await
            .is_err(),
        "a setting the schema does not declare refuses"
    );

    // ── The binding is the row the plugin then calls under ──

    // The binding answers `fetch` only while the account's external-apps
    // switch is on — an install-row call would not care — so turning it OFF
    // and restarting the plugin (whose `start` calls for its first bound
    // account) proves the call ran as the binding row.
    db.set_atproto_external_apps_enabled(&USER, false)
        .await
        .unwrap();
    db.plugin_state_delete(&pid, "fetch").await.unwrap();
    let _ = state
        .plugins
        .ingress(
            &pid,
            IngressRequest {
                method: "GET".into(),
                path: "/spin".into(),
                ..Default::default()
            },
        )
        .await;
    eventually(
        "the restarted plugin's start under the binding",
        async || state_byte(&state, &pid, "fetch").await.is_some(),
    )
    .await;
    assert_eq!(
        state_byte(&state, &pid, "fetch").await.unwrap(),
        [1],
        "the account's binding row decided the reach, and it is suspended"
    );

    // ── The user's revoke ends the binding; the plugin stays installed ──
    let revoked: RevokePrincipalReply = call(
        &state,
        USER,
        "fauna.principals.revoke",
        &RevokePrincipalRequest {
            principal_id: binding.principal_id.clone(),
            extra: BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    assert!(revoked.revoked);
    assert!(
        db.list_third_party_principals(&USER)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        state_byte(&state, &pid, &settings_key).await.is_none(),
        "the account's settings go with its binding"
    );
    let still = list(state.clone()).await;
    assert_eq!(still.len(), 1, "the plugin stays listed for the admin");
    assert_eq!(still[0].bound_accounts, 0);
    assert!(
        available(state.clone()).await[0]
            .binding_principal_id
            .is_none()
    );

    // ── Uninstall: nothing left ──
    let gone: UninstallPluginReply = call(
        &state,
        ADMIN,
        "fauna.plugins.uninstall",
        &UninstallPluginRequest {
            principal_id: pid.clone(),
            extra: BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    assert!(gone.uninstalled);
    assert_eq!(
        gone.bindings_ended, 0,
        "the user's own revoke already ended it"
    );
    assert!(db.list_hosted_plugins().await.unwrap().is_empty());
    assert!(
        db.list_third_party_principals(&USER)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(state_byte(&state, &pid, "pub").await.is_none());
    assert!(!dir.exists(), "the plugin's directory is deleted");
    assert_eq!(state.plugins.status(&pid).state, PLUGIN_STATE_STOPPED);
    assert!(
        state
            .plugins
            .ingress(&pid, IngressRequest::default())
            .await
            .is_err()
    );
}

/// A module whose bytes do not hash to the manifest's digest never opens a
/// card — the publisher's signature pins the exact bytes the nest runs.
#[tokio::test]
async fn a_module_off_its_pinned_digest_refuses_before_any_card() {
    let module = fauna_plugin_host::fixture::hello_plugin().unwrap();
    let (doc, _) = document(&module, serde_json::json!([]));
    let mut tampered = module.clone();
    tampered.push(0);
    let fetcher = Arc::new(TestFetcher {
        doc,
        module: tampered,
        calls: Mutex::new(Vec::new()),
    });
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        oauth_as: Arc::new(fauna_nest::oauth_as_state::OAuthAsRuntime::new(
            fauna_core::data::Timestamp::now_secs_or_zero(),
            fetcher,
        )),
        rpc_router: Arc::new(router()),
        ..AppState::for_test(db.clone())
    });
    db.create_user(&ADMIN, "free", "test").await.unwrap();
    db.add_admin_actor(&ADMIN).await.unwrap();
    let refused = call::<_, InstallPluginReply>(
        &state,
        ADMIN,
        "fauna.plugins.install",
        &InstallPluginRequest {
            document_url: DOC_URL.into(),
            ..Default::default()
        },
    )
    .await;
    assert!(refused.is_err());
    assert!(
        db.list_pending_atproto_consent_requests(&ADMIN)
            .await
            .unwrap()
            .is_empty()
    );
}

/// The boot walk lists a plugin whose module file is gone as `stopped`, with
/// the reason — never a crash of the nest.
#[tokio::test]
async fn boot_lists_a_plugin_without_its_module_as_stopped() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let data = tempfile::tempdir().unwrap();
    let state = Arc::new(AppState {
        plugins: Arc::new(PluginRunner::new(Some(data.path().join("plugins")))),
        ..AppState::for_test(db.clone())
    });
    let pid = db
        .mint_hosted_plugin(&fauna_nest::db::third_party_principals::HostedPluginMint {
            client_id: DOC_URL.into(),
            label: None,
            holder_x25519: [7; 32],
            publisher_key: Some([8; 32]),
            declared_kinds: vec![],
            requested_scopes: String::new(),
            module_digest: format!("sha256:{}", "00".repeat(32)),
            hosts: vec![],
            ingress: serde_json::json!([]),
            settings_schema: None,
            manifest_jws: None,
            installed_by: ADMIN,
        })
        .await
        .unwrap();
    state.plugins.boot(&state).await;
    let status = state.plugins.status(&pid);
    assert_eq!(status.state, PLUGIN_STATE_STOPPED);
    assert!(
        status
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("module file missing")),
        "{status:?}"
    );
}

/// A nest over the test fetcher serving `doc`, with a live token store, the
/// plugin runner under a temp dir, and web hosting on `apex` (`nest.example`;
/// empty for a domainless box).
fn ingress_state(
    doc: String,
    module: Vec<u8>,
    data: &tempfile::TempDir,
    apex: &str,
) -> (Arc<AppState>, Arc<CacheDb>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        oauth_as: Arc::new(fauna_nest::oauth_as_state::OAuthAsRuntime::new(
            fauna_core::data::Timestamp::now_secs_or_zero(),
            Arc::new(TestFetcher {
                doc,
                module,
                calls: Mutex::new(Vec::new()),
            }),
        )),
        plugins: Arc::new(PluginRunner::new(Some(data.path().join("plugins")))),
        rpc_router: Arc::new(router()),
        auth: fauna_nest::state::AuthState {
            token_store: Arc::new(fauna_nest::token_store::TokenStore::new()),
            ..Default::default()
        },
        host_resolver: Some(Arc::new(fauna_nest::web_content::serve::HostResolver::new(
            apex.into(),
        ))),
        ..AppState::for_test(db.clone())
    });
    (state, db)
}

async fn install(state: &Arc<AppState>) -> Result<InstallPluginReply, RpcError> {
    call(
        state,
        ADMIN,
        "fauna.plugins.install",
        &InstallPluginRequest {
            document_url: DOC_URL.into(),
            ..Default::default()
        },
    )
    .await
}

/// One GET through the public router: `(status, content-security-policy, body)`.
async fn get(
    app: &axum::Router,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> (u16, Option<String>, Vec<u8>) {
    use tower::ServiceExt as _;
    let mut req = axum::http::Request::get(path).header("host", host);
    for (n, v) in headers {
        req = req.header(*n, *v);
    }
    let resp = app
        .clone()
        .oneshot(req.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let csp = resp
        .headers()
        .get("content-security-policy")
        .map(|v| v.to_str().unwrap().to_string());
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, csp, body)
}

/// `security.md` § Co-resident process trust boundary → *Hosted third-party
/// code*: the nest terminates the request for a path the plugin declared and
/// hands it the actor IT authenticated — never one a header claims — and a
/// fault is the nest's 502, not the nest's end.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn declared_ingress_reaches_the_plugin_with_the_nest_asserted_actor() {
    const NEST: &str = "nest.example";
    let module = fauna_plugin_host::fixture::hello_plugin().unwrap();
    // The fixture dispatches on the path's second byte: `/w…` who-am-I,
    // `/s…` spin, anything else 404 — and the plugin sees the full path.
    let (doc, _) = document(
        &module,
        serde_json::json!([
            { "path": "/whoami/", "sni": null },
            { "path": "/spin/", "sni": null },
            { "path": "/nothing/" },
        ]),
    );
    let data = tempfile::tempdir().unwrap();
    let (state, db) = ingress_state(doc, module, &data, NEST);
    for actor in [ADMIN, USER] {
        db.create_user(&actor, "free", "test").await.unwrap();
    }
    db.add_admin_actor(&ADMIN).await.unwrap();
    let card = install(&state).await.unwrap().consent;
    assert_eq!(
        card.install.as_ref().unwrap().ingress_paths,
        ["/whoami/", "/spin/", "/nothing/"]
    );
    let _: ResolveConsentReply = call(
        &state,
        ADMIN,
        "fauna.bridges.atproto.resolve_consent",
        &ResolveConsentRequest {
            consent_id: card.consent_id.clone(),
            approved: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let pid = db.list_hosted_plugins().await.unwrap()[0]
        .principal_id
        .clone();
    eventually("the plugin running", async || {
        state.plugins.status(&pid).state == PLUGIN_STATE_RUNNING
    })
    .await;

    let app = fauna_nest::build_router(state.clone());
    let token = state
        .auth
        .token_store
        .insert(fauna_core::identity::ActorId(USER), 3600)
        .await;
    let bearer = format!("Bearer {token}");

    // A user's bearer → the plugin sees that actor; its reply is sandboxed.
    let (status, csp, body) = get(&app, NEST, "/whoami/me", &[("authorization", &bearer)]).await;
    assert_eq!((status, body.as_slice()), (200, USER.as_slice()));
    assert_eq!(
        csp.as_deref(),
        Some("sandbox"),
        "plugin output never scripts the nest origin"
    );
    // No bearer → anonymous; a bearer that does not validate → the nest's 401.
    assert_eq!(get(&app, NEST, "/whoami/me", &[]).await.0, 401);
    assert_eq!(
        get(
            &app,
            NEST,
            "/whoami/me",
            &[("authorization", "Bearer nope")]
        )
        .await
        .0,
        401
    );
    // Headers claiming an identity reach the plugin as nothing more than
    // headers: the actor field is the nest's, so the request is anonymous.
    let user_hex = hex::encode(USER);
    let forged = [
        ("x-fauna-actor", user_hex.as_str()),
        ("x-forwarded-user", user_hex.as_str()),
        ("x-remote-user", user_hex.as_str()),
    ];
    assert_eq!(get(&app, NEST, "/whoami/me", &forged).await.0, 401);
    // The plugin's own 404.
    let (status, csp, _) = get(&app, NEST, "/nothing/here", &[]).await;
    assert_eq!((status, csp.as_deref()), (404, Some("sandbox")));
    // A path the nest routes is never the plugin's.
    let (status, csp, _) = get(&app, NEST, "/api/v1/health", &[]).await;
    assert_eq!(status, 200);
    assert_ne!(csp.as_deref(), Some("sandbox"));
    // A user's published site (a `<handle>` subdomain) is never shadowed: the
    // same path on that host is not proxied.
    state
        .host_resolver
        .as_ref()
        .unwrap()
        .register_subdomain("alice", USER)
        .await;
    let (_, csp, body) = get(
        &app,
        "alice.nest.example",
        "/whoami/me",
        &[("authorization", &bearer)],
    )
    .await;
    assert_ne!(csp.as_deref(), Some("sandbox"));
    assert_ne!(body, USER.to_vec());

    // A runaway call: 502, the nest keeps serving, the plugin comes back.
    assert_eq!(get(&app, NEST, "/spin/forever", &[]).await.0, 502);
    assert_eq!(get(&app, NEST, "/api/v1/health", &[]).await.0, 200);
    eventually("the plugin re-instantiated", async || {
        state.plugins.status(&pid).state == PLUGIN_STATE_RUNNING
    })
    .await;
    let (status, _, body) = get(&app, NEST, "/whoami/again", &[("authorization", &bearer)]).await;
    assert_eq!((status, body.as_slice()), (200, USER.as_slice()));
}

/// A document declaring a prefix the nest serves, or a prefix or whole name
/// another installed plugin answers for, never opens a card; a malformed
/// `sni` (a full name, both halves at once, a reserved label) refuses at
/// resolution.
#[tokio::test]
async fn ingress_the_nest_cannot_honour_refuses_before_any_card() {
    let module = fauna_plugin_host::fixture::hello_plugin().unwrap();
    for ingress in [
        serde_json::json!([{ "path": "/api/", "sni": null }]),
        serde_json::json!([{ "path": "/api/v1/plugin/" }]),
        serde_json::json!([{ "path": "/.well-known/matrix/" }]),
        serde_json::json!([{ "path": "/matrix/", "sni": "matrix.plugins.example" }]),
        serde_json::json!([{ "path": "/matrix/", "sni": "matrix" }]),
        serde_json::json!([{ "sni": "mail" }]),
        serde_json::json!([{ "sni": "taken" }]),
        serde_json::json!([{ "path": "/taken/elsewhere/" }]),
    ] {
        let (doc, _) = document(&module, ingress.clone());
        let data = tempfile::tempdir().unwrap();
        let (state, db) = ingress_state(doc, module.clone(), &data, "nest.example");
        db.create_user(&ADMIN, "free", "test").await.unwrap();
        db.add_admin_actor(&ADMIN).await.unwrap();
        db.mint_hosted_plugin(&fauna_nest::db::third_party_principals::HostedPluginMint {
            client_id: "https://other.example/plugin.json".into(),
            label: None,
            holder_x25519: [7; 32],
            publisher_key: Some([8; 32]),
            declared_kinds: vec![],
            requested_scopes: String::new(),
            module_digest: format!("sha256:{}", "00".repeat(32)),
            hosts: vec![],
            ingress: serde_json::json!([{ "path": "/taken/", "sni": null }, { "sni": "taken" }]),
            settings_schema: None,
            manifest_jws: None,
            installed_by: ADMIN,
        })
        .await
        .unwrap();
        assert!(install(&state).await.is_err(), "{ingress} must refuse");
        assert!(
            db.list_pending_atproto_consent_requests(&ADMIN)
                .await
                .unwrap()
                .is_empty()
        );
    }
    // The control: a free prefix opens the card.
    let (doc, _) = document(&module, serde_json::json!([{ "path": "/matrix/" }]));
    let data = tempfile::tempdir().unwrap();
    let (state, db) = ingress_state(doc, module.clone(), &data, "nest.example");
    db.create_user(&ADMIN, "free", "test").await.unwrap();
    db.add_admin_actor(&ADMIN).await.unwrap();
    assert!(install(&state).await.is_ok());
}

// ── The developer loop (`third-party.md` § Developer loop) ──────────────────

/// Call `kind` as the dispatcher would for a connection whose peer was a
/// loopback address (`Some(true)`), a remote one (`Some(false)`), or with no
/// client connection at all (`None`).
async fn call_from<Req: serde::Serialize, Reply: serde::de::DeserializeOwned>(
    state: &Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    req: &Req,
    loopback_peer: Option<bool>,
) -> Result<Reply, RpcError> {
    let fut = call(state, actor, kind, req);
    match loopback_peer {
        Some(loopback_peer) => {
            fauna_nest::dispatch_core::scope_caller(
                fauna_nest::dispatch_core::CallerRef {
                    conn_id: 1,
                    correlation_id: 1,
                    peer_ip: None,
                    loopback_peer,
                    served_name: None,
                },
                fut,
            )
            .await
        }
        None => fut.await,
    }
}

fn dev_install(module_path: &std::path::Path) -> InstallPluginRequest {
    InstallPluginRequest {
        development: Some(DevelopmentInstall {
            module_path: module_path.to_string_lossy().into_owned(),
            extra: BTreeMap::new(),
        }),
        ..Default::default()
    }
}

/// A local component installs only from the nest's own box, under the
/// synthesized loopback identity, and reinstalling the same path is the
/// edit-run cycle: the module is replaced in place and the plugin's state
/// scope survives.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_development_plugin_installs_over_loopback_and_reinstalls_in_place() {
    let module = fauna_plugin_host::fixture::hello_plugin().unwrap();
    let src = tempfile::tempdir().unwrap();
    let module_path = src.path().join("hello_dev.wasm");
    std::fs::write(&module_path, &module).unwrap();
    std::fs::write(
        src.path().join("hello_dev.fauna.json"),
        serde_json::json!({
            "version": 1,
            "kinds": [{
                "kind": "ext.localhost.notes",
                "class": "state", "merge": "latest-wins", "floor": "none",
            }],
        })
        .to_string(),
    )
    .unwrap();
    let fetcher = Arc::new(TestFetcher {
        doc: String::new(),
        module: Vec::new(),
        calls: Mutex::new(Vec::new()),
    });
    let data = tempfile::tempdir().unwrap();
    let plugins_dir = data.path().join("plugins");
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        oauth_as: Arc::new(fauna_nest::oauth_as_state::OAuthAsRuntime::new(
            fauna_core::data::Timestamp::now_secs_or_zero(),
            fetcher.clone(),
        )),
        plugins: Arc::new(PluginRunner::new(Some(plugins_dir.clone()))),
        rpc_router: Arc::new(router()),
        ..AppState::for_test(db.clone())
    });
    db.create_user(&ADMIN, "free", "test").await.unwrap();
    db.add_admin_actor(&ADMIN).await.unwrap();

    // ── Off the box, or with no connection to judge, it refuses ──
    for peer in [Some(false), None] {
        let refused = call_from::<_, InstallPluginReply>(
            &state,
            ADMIN,
            "fauna.plugins.install",
            &dev_install(&module_path),
            peer,
        )
        .await;
        assert!(refused.is_err(), "peer {peer:?} must refuse");
    }
    assert!(
        db.list_pending_atproto_consent_requests(&ADMIN)
            .await
            .unwrap()
            .is_empty(),
        "a refused development install opens no card"
    );

    // ── From the loopback peer: the synthesized document's card ──
    let install = async || {
        call_from::<_, InstallPluginReply>(
            &state,
            ADMIN,
            "fauna.plugins.install",
            &dev_install(&module_path),
            Some(true),
        )
        .await
        .unwrap()
        .consent
    };
    let card = install().await;
    assert_eq!(card.client_id, "http://localhost/plugins/hello_dev");
    assert_eq!(card.client_name.as_deref(), Some("Development plugin"));
    let section = card.install.clone().expect("the install section");
    assert!(section.development);
    assert_eq!(section.publisher_domain, "localhost");
    assert_eq!(section.publisher_key, "");
    assert_eq!(section.requested_kinds, ["ext.localhost.notes"]);
    assert_eq!(
        section.module_digest,
        format!("sha256:{}", hex::encode(sha2::Sha256::digest(&module)))
    );
    assert!(section.hosts.is_empty());
    assert!(fetcher.calls().is_empty(), "nothing is fetched");

    let approve = async |consent_id: Vec<u8>| {
        let r: ResolveConsentReply = call(
            &state,
            ADMIN,
            "fauna.bridges.atproto.resolve_consent",
            &ResolveConsentRequest {
                consent_id,
                approved: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(r.resolved);
    };
    approve(card.consent_id).await;
    let plugins = db.list_hosted_plugins().await.unwrap();
    assert_eq!(plugins.len(), 1);
    let pid = plugins[0].principal_id.clone();
    assert_eq!(plugins[0].client_id, "http://localhost/plugins/hello_dev");
    assert_eq!(plugins[0].label.as_deref(), Some("Development plugin"));
    assert!(plugins[0].publisher_key.is_empty(), "no publisher key");
    assert_eq!(plugins[0].declared_kinds, ["ext.localhost.notes"]);
    eventually("the development plugin running", async || {
        state.plugins.status(&pid).state == PLUGIN_STATE_RUNNING
            && state_byte(&state, &pid, "http").await.is_some()
    })
    .await;
    let listed: ListPluginsReply = call(
        &state,
        ADMIN,
        "fauna.plugins.list",
        &ListPluginsRequest::default(),
    )
    .await
    .unwrap();
    assert!(
        listed.plugins[0].development,
        "listed as a development plugin"
    );
    assert_eq!(
        listed.plugins[0].publisher_domain.as_deref(),
        Some("localhost")
    );
    assert_eq!(listed.plugins[0].publisher_key, None);
    db.plugin_state_put(&pid, "kept", b"across the reinstall")
        .await
        .unwrap();

    // ── Edit, reinstall: the module is replaced, the state scope kept ──
    let edited_wat = format!(
        "{} (func $edited nop))",
        fauna_plugin_host::fixture::HELLO_PLUGIN_WAT
            .trim_end()
            .strip_suffix(')')
            .unwrap()
    );
    let edited = fauna_plugin_host::fixture::build_component(&edited_wat).unwrap();
    assert_ne!(edited, module);
    std::fs::write(&module_path, &edited).unwrap();
    let card = install().await;
    approve(card.consent_id).await;
    let plugins = db.list_hosted_plugins().await.unwrap();
    assert_eq!(plugins.len(), 1, "replaced, not installed twice");
    assert_eq!(plugins[0].principal_id, pid, "the same principal");
    assert_eq!(
        plugins[0].module_digest,
        format!("sha256:{}", hex::encode(sha2::Sha256::digest(&edited)))
    );
    assert_eq!(
        std::fs::read(plugins_dir.join(hex::encode(&pid)).join(MODULE_FILE)).unwrap(),
        edited,
        "the new module is the one on disk"
    );
    eventually("the replaced plugin running", async || {
        state.plugins.status(&pid).state == PLUGIN_STATE_RUNNING
    })
    .await;
    assert_eq!(
        state_byte(&state, &pid, "kept").await.as_deref(),
        Some(&b"across the reinstall"[..]),
        "the state scope survives the reinstall"
    );
}
/// `security.md` § Co-resident process trust boundary → *Hosted third-party
/// code*, the whole-name half: a plugin claiming `{ "sni": "matrix" }` is
/// handed every request whose `Host` is `matrix.<apex>` — a path the nest
/// routes included, so the name is its own origin and no sandbox is forced —
/// with the nest-asserted actor; the path half keeps its sandbox; and a
/// user whose handle is that label is refused the subdomain opt-in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claimed_whole_name_is_the_plugins_on_every_path() {
    const NEST: &str = "nest.example";
    const OTHER: [u8; 32] = [0x07; 32];
    const BOB: [u8; 32] = [0x0B; 32];
    let module = fauna_plugin_host::fixture::hello_plugin().unwrap();
    let (doc, _) = document(
        &module,
        serde_json::json!([{ "sni": "matrix" }, { "path": "/whoami/" }]),
    );
    let data = tempfile::tempdir().unwrap();
    let (state, db) = ingress_state(doc, module, &data, NEST);
    db.create_user(&ADMIN, "free", "test").await.unwrap();
    db.add_admin_actor(&ADMIN).await.unwrap();
    db.create_user(&USER, "free", "test").await.unwrap();
    db.create_user_with_handle(&OTHER, "free", "matrix", None)
        .await
        .unwrap();
    db.create_user_with_handle(&BOB, "free", "bob", None)
        .await
        .unwrap();
    let card = install(&state).await.unwrap().consent;
    let info = card.install.as_ref().unwrap();
    assert_eq!(info.ingress_paths, ["/whoami/"]);
    assert_eq!(info.ingress_names, ["matrix.nest.example"]);
    let _: ResolveConsentReply = call(
        &state,
        ADMIN,
        "fauna.bridges.atproto.resolve_consent",
        &ResolveConsentRequest {
            consent_id: card.consent_id.clone(),
            approved: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let pid = db.list_hosted_plugins().await.unwrap()[0]
        .principal_id
        .clone();
    eventually("the plugin running", async || {
        state.plugins.status(&pid).state == PLUGIN_STATE_RUNNING
    })
    .await;

    let app = fauna_nest::build_router(state.clone());
    let token = state
        .auth
        .token_store
        .insert(fauna_core::identity::ActorId(USER), 3600)
        .await;
    let bearer = format!("Bearer {token}");

    // On the claimed name: the plugin, with the nest-asserted actor, on its
    // own origin — no forced sandbox. The host is normalized as any other.
    for host in ["matrix.nest.example", "Matrix.Nest.Example:443"] {
        let (status, csp, body) =
            get(&app, host, "/whoami/me", &[("authorization", &bearer)]).await;
        assert_eq!((status, body.as_slice()), (200, USER.as_slice()), "{host}");
        assert_eq!(csp, None, "a whole name is its own origin: no sandbox");
    }
    assert_eq!(
        get(&app, "matrix.nest.example", "/whoami/me", &[]).await.0,
        401
    );
    // Every path: the nest's own health route is the plugin's 404 here, and
    // the nest's 200 on its own host.
    assert_eq!(
        get(&app, "matrix.nest.example", "/api/v1/health", &[])
            .await
            .0,
        404
    );
    assert_eq!(get(&app, NEST, "/api/v1/health", &[]).await.0, 200);
    // The path half is unchanged: sandboxed on the nest's own host.
    let (status, csp, body) = get(&app, NEST, "/whoami/me", &[("authorization", &bearer)]).await;
    assert_eq!(
        (status, body.as_slice(), csp.as_deref()),
        (200, USER.as_slice(), Some("sandbox"))
    );
    // A deeper name is not the plugin's.
    let (_, _, body) = get(
        &app,
        "a.matrix.nest.example",
        "/whoami/me",
        &[("authorization", &bearer)],
    )
    .await;
    assert_ne!(body, USER.to_vec());

    // The reverse guard: the user whose handle is the claimed label is told
    // at the opt-in; another user's opt-in goes through.
    let opt_in = WebSetSubdomainEnabledRequest {
        enabled: true,
        extra: BTreeMap::new(),
    };
    let refused: Result<WebSetSubdomainEnabledReply, RpcError> =
        call(&state, OTHER, "fauna.web.set_subdomain_enabled", &opt_in).await;
    let err = refused.expect_err("a plugin's name is not a user's subdomain");
    assert!(
        format!("{err:?}").contains("matrix.nest.example"),
        "{err:?}"
    );
    let ok: WebSetSubdomainEnabledReply =
        call(&state, BOB, "fauna.web.set_subdomain_enabled", &opt_in)
            .await
            .unwrap();
    assert!(ok.enabled);
    let resolver = state.host_resolver.as_ref().unwrap();
    assert!(resolver.user_site_for("bob.nest.example").await.is_some());
    assert!(
        resolver
            .user_site_for("matrix.nest.example")
            .await
            .is_none()
    );
}

/// The install side of the same exclusivity: a label a user's live site
/// already answers on is refused, and a domainless box has no name to give.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_whole_name_claim_yields_to_a_live_user_site_and_needs_an_apex() {
    let module = fauna_plugin_host::fixture::hello_plugin().unwrap();
    let (doc, _) = document(&module, serde_json::json!([{ "sni": "alice" }]));
    let data = tempfile::tempdir().unwrap();
    let (state, db) = ingress_state(doc, module.clone(), &data, "nest.example");
    db.create_user(&ADMIN, "free", "test").await.unwrap();
    db.add_admin_actor(&ADMIN).await.unwrap();
    db.create_user_with_handle(&USER, "free", "alice", None)
        .await
        .unwrap();
    db.set_subdomain_enabled(&USER).await.unwrap();
    state
        .host_resolver
        .as_ref()
        .unwrap()
        .register_subdomain("alice", USER)
        .await;
    let err = install(&state)
        .await
        .expect_err("alice.nest.example is alice's");
    assert!(format!("{err:?}").contains("alice.nest.example"), "{err:?}");

    let (doc, _) = document(&module, serde_json::json!([{ "sni": "matrix" }]));
    let data = tempfile::tempdir().unwrap();
    let (state, db) = ingress_state(doc, module, &data, "");
    db.create_user(&ADMIN, "free", "test").await.unwrap();
    db.add_admin_actor(&ADMIN).await.unwrap();
    let err = install(&state).await.expect_err("no apex, no name");
    assert!(format!("{err:?}").contains("has none"), "{err:?}");
}
