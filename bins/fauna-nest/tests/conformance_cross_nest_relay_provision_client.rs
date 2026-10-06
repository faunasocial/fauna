//! **One-action mailbox provisioning on link** (tier_3) — real-wire, two-nest,
//! *client-driven*. Proves the home-with-public-relay auto-trigger end-to-end
//! through the production client stack: a single `LinkBoth` (linking the user's
//! home box to their public relay box) **auto-provisions the home box's mailbox**
//! with the **public box's MSEK**, so a non-technical user links once and relayed
//! mail is immediately readable there — no separate "enable mail" step on the home
//! box (`docs/goal/architecture/nest/deployment-home-with-public-relay.md`
//! § Pairing; `docs/goal/behavior/mail-credentials.md` § Trigger taxonomy).
//!
//! Flow: enable mail on box A (the public relay) via the shared
//! `MailSettingsMachine` — mints the fleet MSEK, provisions A's mailbox, persists
//! the MSEK into A's `fauna.state.mail` plane entry. Then drive the shared
//! `build_linked_nests_machine_with_mail_relay`'s `LinkBoth` against box B (the
//! home box). The mail relay post-link hook fires after the reciprocal pairing
//! rows are seeded — it reads the MSEK from **A's** mail custody, opens a second
//! authenticated connection to **B**, and dispatches `ProvisionRelayMailbox` on a
//! peer-bound machine (mail custody = A, nest seam = B). B ends up with the **same
//! MSEK-derived recipient pubkey** A registered.
//!
//! What only this test catches over the crate's fake-seam unit tests
//! (`fauna-client-pair`'s `link_both_fires_post_link_hook…`,
//! `fauna-client-mail-settings`'s `provision_relay_mailbox_reuses_fleet_msek…`):
//! that the hook's real chain — read A's mail custody, open a
//! *second* `NestClient` to B, and push the read recipe onto **B's** bridge store
//! (not A's) — actually wires custody-source ≠ provision-target correctly. A
//! mistakenly self-bound machine (minting a fresh MSEK on B) passes the fakes but
//! fails the `pubkey_b == pubkey_a` assertion here.
//!
//! Mirrors `conformance_cross_nest_pairing_client.rs`'s two-nest harness, with the
//! `BackupService` + bridge-blob + bridge-routing kinds added so the
//! client mail-enable + provisioning RPCs resolve over the real wire.

mod common;
use common::connected_client;

use std::sync::Arc;

use fauna_client_mail_settings::rpc_glue::{
    build_linked_nests_machine_with_mail_relay, build_mail_settings_machine,
};
use fauna_client_mail_settings::{
    CredentialKind, MailSettingsAction, SecretBytes, derive_credential_id,
};
use fauna_client_pair::LinkedNestsAction;
use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;

/// One of the user's nests: auth-bootstrap + anonymous discovery + the pairing
/// kinds (so `LinkBoth` seeds both ends), plus a real `BackupService`, bridge-blob, and bridge-routing kinds
/// (so the client mail-enable + relay provisioning RPCs resolve). Returns
/// `(http_base, state, tempdir)` — the `TempDir` guard keeps the `BackupService`
/// blob dir alive for the test's lifetime.
async fn start_relay_nest() -> (String, Arc<AppState>, tempfile::TempDir) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let authority = format!("127.0.0.1:{}", addr.port());

    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, tmp.path().to_path_buf(), None).unwrap(),
    );
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity::generate()),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::pair_handlers::register_pair_handlers(&mut b);
            fauna_nest::bridge_blob_handlers::register_bridge_blob_handlers(&mut b);
            fauna_nest::bridge_routing_handlers::register_bridge_routing_handlers(&mut b);
            // The RecoveryKey registration chain the link reconciles before
            // it writes a row.
            fauna_nest::recovery_handlers::register_recovery_handlers(&mut b);
            b.build()
        }),
        auth: AuthState {
            token_store: Arc::new(TokenStore::new()),
            registration: RegistrationConfig {
                handle_domain: Some(authority.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
        enforce_tier_quotas: std::sync::Arc::new(tokio::sync::RwLock::new(true)),
        backup_service: Some(backup_svc),
        ..AppState::for_test(db)
    });

    let app = fauna_nest::build_router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{authority}"), state, tmp)
}

#[tokio::test]
async fn link_both_auto_provisions_home_box_mailbox_with_public_box_msek() {
    let (a_base, a_state, _a_tmp) = start_relay_nest().await;
    let (b_base, b_state, _b_tmp) = start_relay_nest().await;

    // Alice's single identity, registered on BOTH nests (the home-relay premise:
    // one user, two of their own boxes).
    let alice = ActorKeypair::generate();
    let secret = *alice.secret_bytes();
    let alice_id = alice.actor_id();
    a_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();
    b_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();

    // Connect to A (the public relay box) and enable mail there via the production
    // shared machine — this mints the fleet MSEK, provisions A's mailbox, and
    // persists the MSEK into A's `fauna.state.mail` plane entry.
    let client_a = connected_client(&a_base, ActorKeypair::from_secret(secret)).await;
    // The account's mail custody — one store the enabling machine and the
    // link hook share, as one seat's account runtime is in production.
    let mail_store: Arc<dyn fauna_client_config::MailStore> =
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty());
    let mail = build_mail_settings_machine(
        client_a.clone(),
        ActorKeypair::from_secret(secret),
        Arc::clone(&mail_store),
        &a_base,
        Arc::new(fauna_client_config::NoLedgerStore),
        None,
    );
    mail.dispatch(MailSettingsAction::EnableMail {
        display_name: "Default".into(),
        kind: CredentialKind::Plain,
        secret: SecretBytes::from(b"home-relay-read-pw-24chars!!".to_vec()),
    })
    .await
    .expect("enable mail on the public relay box");

    // A's recipient pubkey is now the MSEK-derived standing HPKE key.
    let pubkey_a = a_state
        .db
        .get_actor_mls_pubkey(&alice_id.0)
        .await
        .unwrap()
        .expect("public box registered a recipient pubkey at enable-mail");

    // ONE action: link A and B by B's address. The machine seeds the reciprocal
    // pairing rows, then the mail relay post-link hook auto-provisions B's mailbox
    // (config read from A, blobs pushed to B).
    let linked = build_linked_nests_machine_with_mail_relay(
        client_a.clone(),
        ActorKeypair::from_secret(secret),
        Arc::new(fauna_client_config::NoLedgerStore),
        Arc::clone(&mail_store),
    );
    linked
        .dispatch(LinkedNestsAction::LinkBoth {
            other_nest_url: b_base.clone(),
            capabilities: vec![], // → default_self_sync()
            expires_at: None,
            label: Some("public relay".into()),
        })
        .await
        .expect("LinkBoth + auto-provision should succeed");

    // The link itself seeded both reciprocal pairing rows …
    let a_nest_id = a_state.nest_identity.public_key_bytes();
    let b_nest_id = b_state.nest_identity.public_key_bytes();
    assert!(
        a_state.db.is_paired(&alice_id.0, &b_nest_id).await.unwrap(),
        "public box authorizes the home box (A→B pull-gate row)"
    );
    assert!(
        b_state.db.is_paired(&alice_id.0, &a_nest_id).await.unwrap(),
        "home box authorizes the public box (the reciprocal relay-worker row)"
    );

    // … and — the load-bearing property — the HOME box (B) now holds the **same**
    // MSEK-derived recipient pubkey as the public box (A). A *different* MSEK on B
    // would silently break home-box decrypt (mail looks delivered but unreadable).
    let pubkey_b = b_state
        .db
        .get_actor_mls_pubkey(&alice_id.0)
        .await
        .unwrap()
        .expect("the home box was auto-provisioned with a recipient pubkey on link");
    assert_eq!(
        pubkey_b, pubkey_a,
        "home box reuses the public box's MSEK-derived recipient pubkey"
    );

    // The home box got the full READ recipe: the MLS snapshot + the wrapped-MSEK
    // read credential under the default credential …
    let cred = derive_credential_id("Default", &[]);
    assert!(
        b_state
            .db
            .get_mls_snapshot_blob(&alice_id.0)
            .await
            .unwrap()
            .is_some(),
        "home box got the MLS snapshot"
    );
    assert!(
        b_state
            .db
            .get_wrapped_mls_blob(&alice_id.0, &cred)
            .await
            .unwrap()
            .is_some(),
        "home box got the wrapped-MSEK read credential"
    );
    // … and NO submission token — the home box runs no MTA (it never submits).
    assert!(
        b_state
            .db
            .get_wrapped_submission_token(&alice_id.0, &cred)
            .await
            .unwrap()
            .is_none(),
        "home box must not get a submission token — it runs no MTA"
    );

    // Finally — the client-UI-read property (deployment-home-with-public-relay.md
    // § Pairing): a fauna app logged into the home box reads the SAME MSEK from
    // the account's mail custody — the account plane is the account's, whichever
    // box the app is connected to.
    assert!(
        mail_store.load().await.expect("custody").msek.is_some(),
        "the account's mail custody holds the one fleet MSEK both boxes serve"
    );
}
