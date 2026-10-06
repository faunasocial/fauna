//! **Slice 4 (optional LAN-TLS cert distribution) — tier_3 capstone.** Two
//! in-process nests over the federation WS-RPC channel: a public (issuing-side)
//! nest `H` and a paired **private** nest `P`. A publicly-trusted
//! LAN-TLS cert is placed on `H` as a `namespace_entries` payload — sealed to
//! `P`'s identity-derived x25519 pubkey and Ed25519-signed by the actor. `P`
//! pulls it over the existing `fauna.federation.sync.pull` channel (no new
//! transport), verifies the actor signature, unseals it with its identity key,
//! and writes the PEM **plaintext** to its `acme_dir` — where the existing MDA
//! `fetch_tls_cert_blob` seal-on-read serves it on the LAN IMAP/CalDAV listener.
//!
//! This is the wire-level proof for `deployment-home-with-public-relay.md`
//! § Done definition checkbox 2 (cert distribution channel) + § MUA reach.
//! The crypto round-trip is the in-crate unit test
//! `fauna_mls::wrapped_blob::lan_cert::tests`.
//!
//! Harness mirrors `conformance_cross_nest_mail_relay.rs`: `for_test`'s routers
//! are empty, so each nest registers the federation + anon-discovery handlers
//! (the channel pool resolves a peer's `nest_id` from its URL before dialing).
//! `P` is built with `NodeMode::Private` (so the cert-apply hook fires) and a
//! caller-known `acme_dir` (so the test can read the written PEM back).

mod common;
use common::lan_cert_bundle;

use std::path::PathBuf;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_core::identity::ActorId;
use fauna_mls::wrapped_blob::format::TlsCertBundle;
use fauna_mls::wrapped_blob::{LAN_TLS_CERT_ENTRY_ID, seal_lan_tls_cert_entry};
use fauna_nest::config::NodeMode;
use fauna_nest::db::CacheDb;
use fauna_nest::federation_pool::{originate_sync_pull, originate_sync_push};
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::nest_sync_worker::{run_sync_cycle, sync_actor_namespace_once};
use fauna_nest::routes::AppState;
use fauna_nest::storage::SealedStorage;
use fauna_protocol::pair::capability;

fn registered_routers() -> (
    Arc<fauna_nest::federation_router::FederationRouter>,
    Arc<fauna_nest::rpc_router::RpcRouter>,
) {
    let federation = Arc::new({
        let mut b = fauna_nest::federation_router::FederationRouter::builder();
        fauna_nest::federation_handlers::register_federation_handlers(&mut b);
        b.build()
    });
    let rpc = Arc::new({
        let mut b = fauna_nest::rpc_router::RpcRouter::builder();
        fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
        b.build()
    });
    (federation, rpc)
}

async fn serve(state: Arc<AppState>) -> String {
    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("http://{addr}")
}

/// A public (issuing-side) nest with a distinct identity. Carries the cert
/// entry in its `namespace_entries` and serves it over `sync.pull`.
async fn start_public_nest() -> (String, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();
    let (federation_router, rpc_router) = registered_routers();
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity {
            signing_key,
            verifying_key,
        }),
        federation_router,
        rpc_router,
        ..AppState::for_test(db)
    });
    let url = serve(state.clone()).await;
    (url, state)
}

/// A **private** nest with a caller-known `acme_dir` (so the test can read
/// the cert the worker writes there).
async fn start_private_nest(acme_dir: PathBuf) -> (String, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();
    let (federation_router, rpc_router) = registered_routers();

    let mut state = AppState::for_test_with_node_mode(db.clone(), NodeMode::Private);
    state.nest_identity = Arc::new(NestIdentity {
        signing_key,
        verifying_key,
    });
    state.federation_router = federation_router;
    state.rpc_router = rpc_router;
    // Swap in a `SealedStorage` rooted at the caller's acme_dir (the default
    // `for_test` storage uses its own throwaway acme_dir the test can't see).
    state.install_storage_for_test(Arc::new(SealedStorage::new(db.clone(), acme_dir)));

    let state = Arc::new(state);
    let url = serve(state.clone()).await;
    (url, state)
}

/// Place a cert namespace entry on `h`, sealed to `p`'s identity x25519 and
/// signed by `actor_sk`, under the actor's self-namespace. Returns the bundle.
async fn seed_cert_entry(
    h: &Arc<AppState>,
    p: &Arc<AppState>,
    actor: &[u8; 32],
    actor_sk: &SigningKey,
    tamper_sig: bool,
) -> TlsCertBundle {
    let p_nest_id = p.nest_identity.public_key_bytes();
    let p_x25519_pub = ActorId(p_nest_id).to_x25519_public().to_bytes();
    let bundle = lan_cert_bundle("home.example.com");
    let (ciphertext, mut actor_sig) =
        seal_lan_tls_cert_entry(&bundle, "home.example.com", &p_x25519_pub, actor_sk).unwrap();
    if tamper_sig {
        actor_sig[0] ^= 0xff;
    }
    h.db.namespace_put(actor, LAN_TLS_CERT_ENTRY_ID, &ciphertext, &actor_sig)
        .await
        .unwrap();
    bundle
}

/// **The cert relay over the real channel.** `H` holds a cert entry for the
/// actor's self-namespace, sealed to `P` and actor-signed; `P` is paired with
/// `namespace_sync`. Driving `P`'s namespace-sync worker step pulls the entry,
/// verifies + unseals it, and writes the PEM to `P`'s `acme_dir`.
#[tokio::test]
async fn lan_tls_cert_relays_public_to_private_disk_via_namespace_sync() {
    let acme = tempfile::tempdir().unwrap();
    let (h_url, h_state) = start_public_nest().await;
    let (_p_url, p_state) = start_private_nest(acme.path().to_path_buf()).await;

    let actor_sk = SigningKey::from_bytes(&[0x11u8; 32]);
    let actor: [u8; 32] = actor_sk.verifying_key().to_bytes();
    let actor_hex = hex::encode(actor);

    // The signer is an admin of P: a listener cert is an admin's to install,
    // by this route exactly as by the direct one.
    p_state.db.add_admin_actor(&actor).await.unwrap();

    // H pairs the actor with P (the pulling nest), granting namespace_sync.
    h_state
        .db
        .store_pairing(
            &actor,
            &p_state.nest_identity.public_key_bytes(),
            &[capability::NAMESPACE_SYNC.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    let bundle = seed_cert_entry(&h_state, &p_state, &actor, &actor_sk, false).await;

    // Drive P's namespace-sync worker step over the channel for the actor's
    // self-namespace (= actor pubkey), from cursor 0.
    let up_to = sync_actor_namespace_once(&p_state, &h_url, &actor, &actor_hex, &actor, 0).await;
    assert!(up_to.is_some(), "pull should have advanced the watermark");

    // The cert + key landed plaintext on P's acme_dir — where the MDA's
    // fetch_tls_cert_blob seal-on-read serves it on the LAN listener.
    let cert_on_disk =
        std::fs::read(acme.path().join("fullchain.pem")).expect("fullchain.pem written");
    assert_eq!(
        cert_on_disk, bundle.cert_chain,
        "served cert == issued cert"
    );
    let key_on_disk = std::fs::read(acme.path().join("privkey.pem")).expect("privkey.pem written");
    assert_eq!(key_on_disk, bundle.priv_key, "served key == issued key");

    // From here the MDA serves it via the unchanged `fetch_tls_cert_blob`
    // seal-on-read path (`seal_current_tls_cert_for_bridge` reads exactly this
    // `acme_dir` cert and seals it to the requesting bridge) — that disk→bridge
    // step is the pre-existing MTA/MDA cert path, covered by the bridge-blob
    // tests, and needs an enrolled+attested bridge it would only duplicate here.
    // Slice 4's contribution is the disk landing asserted above.
}

/// A cert entry whose actor signature does not verify (a substituted entry from
/// a compromised relay) is **rejected** — nothing is written to disk.
#[tokio::test]
async fn forged_actor_sig_is_rejected_and_no_cert_is_written() {
    let acme = tempfile::tempdir().unwrap();
    let (h_url, h_state) = start_public_nest().await;
    let (_p_url, p_state) = start_private_nest(acme.path().to_path_buf()).await;

    let actor_sk = SigningKey::from_bytes(&[0x22u8; 32]);
    let actor: [u8; 32] = actor_sk.verifying_key().to_bytes();
    let actor_hex = hex::encode(actor);

    h_state
        .db
        .store_pairing(
            &actor,
            &p_state.nest_identity.public_key_bytes(),
            &[capability::NAMESPACE_SYNC.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    // tamper_sig = true → the actor signature won't verify.
    seed_cert_entry(&h_state, &p_state, &actor, &actor_sk, true).await;

    let up_to = sync_actor_namespace_once(&p_state, &h_url, &actor, &actor_hex, &actor, 0).await;
    // The pull itself succeeds (the entry is stored "remote"); only the
    // cert-apply step rejects it.
    assert!(up_to.is_some(), "pull still returns a watermark");

    assert!(
        !acme.path().join("fullchain.pem").exists(),
        "a forged cert entry must not be written to disk"
    );
}

/// Pair `actor` on both nests the way a user's two `fauna.pair.add` calls do:
/// `H` holds the pull-gate row naming `P`, and `P` holds the local row that
/// makes its sync worker relay for the actor at all, recording `H`'s URL as
/// the target the worker dials.
async fn pair_both_ways(h: &Arc<AppState>, h_url: &str, p: &Arc<AppState>, actor: &[u8; 32]) {
    let caps = [capability::NAMESPACE_SYNC.to_string()];
    h.db.store_pairing(
        actor,
        &p.nest_identity.public_key_bytes(),
        &caps,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    p.db.store_pairing(
        actor,
        &h.nest_identity.public_key_bytes(),
        &caps,
        None,
        Some(h_url),
        None,
    )
    .await
    .unwrap();
}

/// One real worker cycle on `p`, against the targets its pairing rows name.
async fn cycle(p: &Arc<AppState>) {
    let mut watermarks = std::collections::HashMap::new();
    let mut mail_watermarks = std::collections::HashMap::new();
    run_sync_cycle(p, &mut watermarks, &mut mail_watermarks).await;
}

fn actor_id(seed: u8) -> [u8; 32] {
    SigningKey::from_bytes(&[seed; 32])
        .verifying_key()
        .to_bytes()
}

/// **A paired NON-admin's cert is never installed.** Pairing is every user's
/// own act (`fauna.pair.add` is user-class), and the seal target is public — so
/// any paired user can put a well-signed entry, sealed to `P`, where `P`'s
/// worker will pull it. The listener cert is an admin's to set: the direct
/// route is admin-gated at its handler, and the relayed route answers to the
/// same role state. Driven through the real worker cycle, local pairing row
/// included.
#[tokio::test]
async fn a_paired_non_admins_cert_entry_is_not_installed() {
    let acme = tempfile::tempdir().unwrap();
    let (h_url, h_state) = start_public_nest().await;
    let (_p_url, p_state) = start_private_nest(acme.path().to_path_buf()).await;

    // P has an admin — somebody else.
    p_state.db.add_admin_actor(&actor_id(0x33)).await.unwrap();

    let member_sk = SigningKey::from_bytes(&[0x44u8; 32]);
    let member: [u8; 32] = member_sk.verifying_key().to_bytes();
    pair_both_ways(&h_state, &h_url, &p_state, &member).await;
    seed_cert_entry(&h_state, &p_state, &member, &member_sk, false).await;

    cycle(&p_state).await;

    assert!(
        !p_state
            .db
            .namespace_entries_since(&member, 0, 10)
            .await
            .unwrap()
            .is_empty(),
        "the entry itself was pulled — the refusal is the install's, not the channel's"
    );
    assert!(
        !acme.path().join("fullchain.pem").exists(),
        "a non-admin's cert must not become the nest's listener cert"
    );
    assert!(!acme.path().join("privkey.pem").exists());
}

/// The gate fails SAFE on a nest that has no admin yet (a fresh or
/// factory-reset private box): nobody is an admin, so nothing is installed by
/// relay. The way forward is the ordinary one, and it is the client's: once
/// the nest has its admin, that admin publishes again. The worker's pull
/// cursor is already past the refused entry (held across cycles here, as the
/// running worker holds it), so it is the fresh publish — a fresh seq — that
/// installs, never a silent retry of something refused earlier.
#[tokio::test]
async fn a_nest_with_no_admin_installs_nothing_by_relay_until_its_admin_publishes() {
    let acme = tempfile::tempdir().unwrap();
    let (h_url, h_state) = start_public_nest().await;
    let (_p_url, p_state) = start_private_nest(acme.path().to_path_buf()).await;

    let actor_sk = SigningKey::from_bytes(&[0x55u8; 32]);
    let actor: [u8; 32] = actor_sk.verifying_key().to_bytes();
    pair_both_ways(&h_state, &h_url, &p_state, &actor).await;
    seed_cert_entry(&h_state, &p_state, &actor, &actor_sk, false).await;

    let mut watermarks = std::collections::HashMap::new();
    let mut mail_watermarks = std::collections::HashMap::new();
    run_sync_cycle(&p_state, &mut watermarks, &mut mail_watermarks).await;
    assert!(
        !acme.path().join("fullchain.pem").exists(),
        "no admin, no relayed install"
    );

    p_state.db.add_admin_actor(&actor).await.unwrap();
    run_sync_cycle(&p_state, &mut watermarks, &mut mail_watermarks).await;
    assert!(
        !acme.path().join("fullchain.pem").exists(),
        "an entry refused earlier is not retried behind the admin's back"
    );

    let bundle = seed_cert_entry(&h_state, &p_state, &actor, &actor_sk, false).await;
    run_sync_cycle(&p_state, &mut watermarks, &mut mail_watermarks).await;
    assert_eq!(
        std::fs::read(acme.path().join("fullchain.pem")).expect("the admin's publish installs"),
        bundle.cert_chain
    );
}

/// `sync.pull` answers for the paired actor's OWN namespace only. A nest
/// paired with one account must not read another account's entries by naming
/// its namespace beside the actor it is paired with.
#[tokio::test]
async fn sync_pull_refuses_a_namespace_that_is_not_the_paired_actors() {
    let acme = tempfile::tempdir().unwrap();
    let (h_url, h_state) = start_public_nest().await;
    let (_p_url, p_state) = start_private_nest(acme.path().to_path_buf()).await;

    let member = actor_id(0x66);
    let other = actor_id(0x77);
    pair_both_ways(&h_state, &h_url, &p_state, &member).await;
    h_state
        .db
        .namespace_put(&other, b"theirs", b"sealed", b"sig")
        .await
        .unwrap();

    let foreign = originate_sync_pull(
        &p_state.federation_pool,
        &p_state,
        &h_url,
        &hex::encode(member),
        &other,
        0,
    )
    .await;
    assert!(
        foreign.is_err(),
        "a pull naming another account's namespace is refused, not answered with {:?} entries",
        foreign.map(|r| r.entries.len()).ok()
    );

    let own = originate_sync_pull(
        &p_state.federation_pool,
        &p_state,
        &h_url,
        &hex::encode(member),
        &member,
        0,
    )
    .await;
    assert!(own.is_ok(), "the actor's own namespace still answers");
}

/// `sync.push` writes the paired actor's OWN namespace only — `INSERT OR
/// REPLACE` on `(namespace, entry_id, source)` is otherwise a write into any
/// account's namespace for a nest paired with any one account.
#[tokio::test]
async fn sync_push_refuses_a_namespace_that_is_not_the_paired_actors() {
    let acme = tempfile::tempdir().unwrap();
    let (h_url, h_state) = start_public_nest().await;
    let (_p_url, p_state) = start_private_nest(acme.path().to_path_buf()).await;

    let member = actor_id(0x88);
    let other = actor_id(0x99);
    pair_both_ways(&h_state, &h_url, &p_state, &member).await;

    // Entries to push, read back from P's own store.
    p_state
        .db
        .namespace_put(&member, b"mine", b"sealed", b"sig")
        .await
        .unwrap();
    let entries = p_state
        .db
        .namespace_entries_since(&member, 0, 10)
        .await
        .unwrap();

    let foreign = originate_sync_push(
        &p_state.federation_pool,
        &p_state,
        &h_url,
        &hex::encode(member),
        &other,
        &entries,
    )
    .await;
    assert!(
        foreign.is_err(),
        "a push into another account's namespace is refused"
    );
    assert!(
        h_state
            .db
            .namespace_entries_since(&other, 0, 10)
            .await
            .unwrap()
            .is_empty(),
        "and nothing was written there"
    );

    originate_sync_push(
        &p_state.federation_pool,
        &p_state,
        &h_url,
        &hex::encode(member),
        &member,
        &entries,
    )
    .await
    .expect("the actor's own namespace still takes a push");
    assert_eq!(
        h_state
            .db
            .namespace_entries_since(&member, 0, 10)
            .await
            .unwrap()
            .len(),
        1
    );
}

/// The worker syncs each paired actor's OWN namespace and no other. On a
/// multi-user private nest one user's pairing must not carry a housemate's
/// namespace to that user's relay.
#[tokio::test]
async fn the_worker_never_carries_another_accounts_namespace() {
    let acme = tempfile::tempdir().unwrap();
    let (h_url, h_state) = start_public_nest().await;
    let (_p_url, p_state) = start_private_nest(acme.path().to_path_buf()).await;

    let member = actor_id(0xAA);
    let housemate = actor_id(0xBB);
    // Only `member` is paired; the housemate merely has local entries on P.
    pair_both_ways(&h_state, &h_url, &p_state, &member).await;
    p_state
        .db
        .namespace_put(&housemate, b"theirs", b"sealed", b"sig")
        .await
        .unwrap();
    p_state
        .db
        .namespace_put(&member, b"mine", b"sealed", b"sig")
        .await
        .unwrap();

    cycle(&p_state).await;

    assert_eq!(
        h_state
            .db
            .namespace_entries_since(&member, 0, 10)
            .await
            .unwrap()
            .len(),
        1,
        "the paired actor's own entries reached the relay"
    );
    assert!(
        h_state
            .db
            .namespace_entries_since(&housemate, 0, 10)
            .await
            .unwrap()
            .is_empty(),
        "the housemate's namespace did not ride the member's pairing"
    );
}
