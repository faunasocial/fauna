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
//! taking the nest with it and the plugin comes back; a user's binding is the
//! row the plugin then calls under; uninstall leaves nothing behind.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fauna_nest::db::CacheDb;
use fauna_nest::db::third_party_principals::{
    AttestedKeys, ExecutionForm, NEST_OWNER_ACTOR, PrincipalAttestation,
};
use fauna_nest::oauth_as_client::{ClientMetadataFetcher, MetadataFetchError};
use fauna_nest::plugin_runner::{HOLDER_FILE, MODULE_FILE, PluginRunner};
use fauna_nest::routes::AppState;
use fauna_plugin_host::{HttpRequest, HttpResponse, IngressRequest};
use fauna_protocol::atproto_pds::{ResolveConsentReply, ResolveConsentRequest};
use fauna_protocol::kind_manifest::{ed25519_did_key, sign_manifest};
use fauna_protocol::plugins::{
    InstallPluginReply, InstallPluginRequest, ListPluginsReply, ListPluginsRequest,
    PLUGIN_STATE_RUNNING, PLUGIN_STATE_STOPPED, UninstallPluginReply, UninstallPluginRequest,
};
use fauna_protocol::{RpcError, decode_strict, encode_canonical};
use sha2::Digest as _;

const DOC_URL: &str = "https://plugins.example/hello.json";
const MODULE_URL: &str = "https://plugins.example/hello.wasm";
const ADMIN: [u8; 32] = [0xAD; 32];
const USER: [u8; 32] = [0x5E; 32];

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
/// module by URL and digest, and no outbound hosts.
fn document(module: &[u8]) -> (String, [u8; 32]) {
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
        }),
        None,
    );
    let doc = serde_json::json!({
        "client_id": DOC_URL,
        "client_name": "Hello Plugin",
        "redirect_uris": ["https://plugins.example/cb"],
        "scope": "atproto",
        "grant_types": ["authorization_code"],
        "response_types": ["code"],
        "application_type": "web",
        "dpop_bound_access_tokens": true,
        "fauna": jws,
    })
    .to_string();
    (doc, key.verifying_key().to_bytes())
}

/// The two kind families this journey drives: `fauna.plugins.*` and the
/// card's `resolve_consent`.
fn router() -> fauna_nest::rpc_router::RpcRouter {
    let mut b = fauna_nest::rpc_router::RpcRouter::builder();
    fauna_nest::plugins_handlers::register_plugins_handlers(&mut b);
    fauna_nest::bridge_atproto_handlers::register_bridge_atproto_handlers(&mut b);
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
    let (doc, publisher_key) = document(&module);
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
            extra: BTreeMap::new(),
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
    assert_eq!(fetcher.calls(), [DOC_URL, MODULE_URL]);

    // A user may not install, and may not approve an install card.
    assert!(
        call::<_, InstallPluginReply>(
            &state,
            USER,
            "fauna.plugins.install",
            &InstallPluginRequest {
                document_url: DOC_URL.into(),
                extra: BTreeMap::new(),
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

    // ── A user's binding is the row the plugin then calls under ──
    db.record_atproto_oauth_grant(
        &USER,
        b"binding-family",
        DOC_URL,
        Some("Hello Plugin"),
        "atproto",
        &[],
        "jkt",
        i64::MAX,
        None,
        fauna_nest::db::atproto_pds::OAUTH_GRANT_ISSUER_NEST,
        &PrincipalAttestation {
            keys: AttestedKeys::default(),
            execution_form: ExecutionForm::Device,
            manifest: None,
        },
    )
    .await
    .unwrap();
    let binding = db
        .list_third_party_principals(&USER)
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.client_id == DOC_URL)
        .expect("the user's binding row");
    assert_eq!(binding.execution_form, ExecutionForm::Wasm.as_str());
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
    assert_eq!(gone.bindings_ended, 1);
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
    let (doc, _) = document(&module);
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
            extra: BTreeMap::new(),
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
            publisher_key: [8; 32],
            declared_kinds: vec![],
            requested_scopes: String::new(),
            module_digest: format!("sha256:{}", "00".repeat(32)),
            hosts: vec![],
            ingress: serde_json::json!([]),
            settings_schema: None,
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
