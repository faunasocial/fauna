//! **TLS Phase 3, S1 — the producer RPC.** Proves the
//! `fauna.tls.publish_cert` WS-RPC is the production producer the Slice-4
//! namespace-sync cert consumer awaits: an **admin** client delivers a sealed
//! LAN-TLS cert to a public/relay nest `H` via the RPC (instead of the
//! in-process `db.namespace_put` the Slice-4 tier_3 fixture
//! `conformance_cross_nest_lan_cert.rs` used), and a paired **private** nest `P`
//! pulls it over `fauna.federation.sync.pull` and writes the PEM to its
//! `acme_dir` — the same end-state the Slice-4 test asserts, now reached through
//! the real client-facing RPC. Plus the admin gate: a non-admin caller is
//! denied and nothing is stored.
//!
//! The sealed blob is opaque to `H` (sealed to `P`'s identity x25519 key); the
//! producer RPC stores it under the caller's own namespace + the well-known
//! `LAN_TLS_CERT_ENTRY_ID`, and the consumer
//! (`lan_cert::install_client_issued_cert`) verifies the actor signature +
//! unseals + installs it.
//!
//! Since 2026-07-22 the same installer also runs on the **direct** delivery, so
//! the two tests added below pin the pair of behaviours that makes one seam serve
//! both topologies: a nest **installs** a cert sealed to itself (the standalone
//! deployment — `tls-certificates.md` § B tier 2), and a relay **does not**
//! install one sealed to its peer.

mod common;
use common::{encode, lan_cert_bundle};

use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use ed25519_dalek::SigningKey;
use fauna_core::identity::ActorId;
use fauna_mls::wrapped_blob::format::TlsCertBundle;
use fauna_mls::wrapped_blob::{LAN_TLS_CERT_ENTRY_ID, seal_lan_tls_cert_entry};
use fauna_nest::config::NodeMode;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::nest_sync_worker::sync_actor_namespace_once;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::storage::SealedStorage;
use fauna_nest::tls_handlers::register_tls_handlers;
use fauna_protocol::pair::capability;
use fauna_protocol::tls::{PublishCertReply, PublishCertRequest};
use fauna_protocol::{ByteBuf, RpcError, decode_strict as decode};

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
        let mut b = RpcRouter::builder();
        fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
        register_tls_handlers(&mut b);
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

/// A public (relay-side) nest that receives the cert via the producer RPC and
/// serves it over `sync.pull`.
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

/// A **public** nest with a caller-known `acme_dir` — the standalone deployment
/// (one nest, no pair), where the admin publishes a cert to the very nest it was
/// issued for.
async fn start_public_nest_with_acme_dir(acme_dir: PathBuf) -> (String, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();
    let (federation_router, rpc_router) = registered_routers();

    let mut state = AppState::for_test(db.clone());
    state.nest_identity = Arc::new(NestIdentity {
        signing_key,
        verifying_key,
    });
    state.federation_router = federation_router;
    state.rpc_router = rpc_router;
    state.install_storage_for_test(Arc::new(SealedStorage::new(db.clone(), acme_dir)));

    let state = Arc::new(state);
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

async fn dispatch(
    state: &Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = state
        .rpc_router
        .kind_meta(kind)
        .expect("kind registered with router");
    (meta.handler)(state.clone(), actor, payload).await
}

/// Build the `PublishCertRequest` for `bundle`, sealed to `p` and actor-signed.
fn publish_request(
    p: &Arc<AppState>,
    actor_sk: &SigningKey,
    bundle: &TlsCertBundle,
) -> PublishCertRequest {
    let p_nest_id = p.nest_identity.public_key_bytes();
    let p_x25519_pub = ActorId(p_nest_id).to_x25519_public().to_bytes();
    let (ciphertext, actor_sig) =
        seal_lan_tls_cert_entry(bundle, "home.example.com", &p_x25519_pub, actor_sk).unwrap();
    PublishCertRequest {
        extra: Default::default(),
        ciphertext: ByteBuf::from(ciphertext),
        actor_sig: ByteBuf::from(actor_sig),
    }
}

/// **The standalone deployment.** One nest, no pair: the admin drives a
/// client-side DNS-01 order for the nest they are connected to and publishes the
/// result straight back to it. Nothing will ever pull this entry over
/// namespace-sync, so the receiving nest must install it **itself** — otherwise a
/// public box whose :80 is unreachable (a cloud firewall, an ISP block) has no
/// route to a trusted cert at all and is stranded on the self-signed floor with
/// no client-side recovery (`tls-certificates.md` § B tier 2).
///
/// Note what is *not* required: no pairing, no `NodeMode::Private`, no sync
/// cycle. The seal target is the authorization — this nest can open the blob, so
/// the cert is for it.
#[tokio::test]
async fn admin_publish_cert_installs_on_the_nest_it_was_sealed_to() {
    let acme = tempfile::tempdir().unwrap();
    let (_url, state) = start_public_nest_with_acme_dir(acme.path().to_path_buf()).await;

    let actor_sk = SigningKey::from_bytes(&[0x33u8; 32]);
    let actor: [u8; 32] = actor_sk.verifying_key().to_bytes();
    state.db.add_admin_actor(&actor[..]).await.unwrap();

    // Sealed to THIS nest (the standalone case), not to a peer.
    let bundle = lan_cert_bundle("home.example.com");
    let req = publish_request(&state, &actor_sk, &bundle);
    let reply_bytes = dispatch(&state, actor, "fauna.tls.publish_cert", encode(&req))
        .await
        .expect("publish_cert ok for admin");
    let reply: PublishCertReply = decode(&reply_bytes).unwrap();
    assert!(reply.ok, "publish_cert reply ok");
    assert!(
        reply.installed,
        "a nest must report installing a cert sealed to itself"
    );

    // Installed on disk immediately — no sync cycle ran.
    let cert_on_disk =
        std::fs::read(acme.path().join("fullchain.pem")).expect("fullchain.pem written");
    assert_eq!(
        cert_on_disk, bundle.cert_chain,
        "served cert == issued cert"
    );
    let key_on_disk = std::fs::read(acme.path().join("privkey.pem")).expect("privkey.pem written");
    assert_eq!(key_on_disk, bundle.priv_key, "served key == issued key");
}

/// **The relay must stay blind.** In the paired topology the client publishes to
/// the reachable relay `H` a cert sealed to the private nest `P`. `H` stores the
/// blob but must not install it: it is not `H`'s cert, and `H` cannot even open
/// it. This is what makes self-install above safe to run unconditionally — the
/// seal, not a NAT-axis flag, is what decides.
#[tokio::test]
async fn relay_stores_but_does_not_install_a_cert_sealed_to_its_peer() {
    let h_acme = tempfile::tempdir().unwrap();
    let p_acme = tempfile::tempdir().unwrap();
    let (_h_url, h_state) = start_public_nest_with_acme_dir(h_acme.path().to_path_buf()).await;
    let (_p_url, p_state) = start_private_nest(p_acme.path().to_path_buf()).await;

    let actor_sk = SigningKey::from_bytes(&[0x44u8; 32]);
    let actor: [u8; 32] = actor_sk.verifying_key().to_bytes();
    h_state.db.add_admin_actor(&actor[..]).await.unwrap();

    // Sealed to P, published to H.
    let bundle = lan_cert_bundle("home.example.com");
    let req = publish_request(&p_state, &actor_sk, &bundle);
    let reply_bytes = dispatch(&h_state, actor, "fauna.tls.publish_cert", encode(&req))
        .await
        .expect("publish_cert ok for admin");
    let reply: PublishCertReply = decode(&reply_bytes).unwrap();
    assert!(reply.ok, "the entry is still stored for P to pull");
    assert!(
        !reply.installed,
        "a relay must not install a cert sealed to its peer"
    );
    assert!(
        !h_acme.path().join("fullchain.pem").exists(),
        "the relay's own cert must be untouched by a peer's cert"
    );

    // ...and the entry really is there, so P's pull still works.
    let entries = h_state
        .db
        .namespace_entries_by_id(&actor, LAN_TLS_CERT_ENTRY_ID)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1, "the sealed entry is stored for the peer");
}

/// **The producer RPC end to end.** An admin client publishes a sealed cert to
/// `H` via `fauna.tls.publish_cert`; the paired private nest `P` pulls it over
/// the real namespace-sync channel and installs the PEM to its `acme_dir`.
#[tokio::test]
async fn admin_publish_cert_rpc_relays_to_private_disk() {
    let acme = tempfile::tempdir().unwrap();
    let (h_url, h_state) = start_public_nest().await;
    let (_p_url, p_state) = start_private_nest(acme.path().to_path_buf()).await;

    let actor_sk = SigningKey::from_bytes(&[0x11u8; 32]);
    let actor: [u8; 32] = actor_sk.verifying_key().to_bytes();
    let actor_hex = hex::encode(actor);

    // The issuing actor is the deployment admin on H (the cert publisher) —
    // and on P, whose installer takes a listener cert from its own admins only.
    h_state.db.add_admin_actor(&actor[..]).await.unwrap();
    p_state.db.add_admin_actor(&actor[..]).await.unwrap();

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

    // Deliver the cert through the REAL producer RPC (not an in-process db write).
    let bundle = lan_cert_bundle("home.example.com");
    let req = publish_request(&p_state, &actor_sk, &bundle);
    let reply_bytes = dispatch(&h_state, actor, "fauna.tls.publish_cert", encode(&req))
        .await
        .expect("publish_cert ok for admin");
    assert!(
        decode::<PublishCertReply>(&reply_bytes).unwrap().ok,
        "publish_cert reply ok"
    );

    // P pulls the actor's self-namespace and installs the cert to acme_dir.
    let up_to = sync_actor_namespace_once(&p_state, &h_url, &actor, &actor_hex, &actor, 0).await;
    assert!(up_to.is_some(), "pull should have advanced the watermark");

    let cert_on_disk =
        std::fs::read(acme.path().join("fullchain.pem")).expect("fullchain.pem written");
    assert_eq!(
        cert_on_disk, bundle.cert_chain,
        "served cert == issued cert"
    );
    let key_on_disk = std::fs::read(acme.path().join("privkey.pem")).expect("privkey.pem written");
    assert_eq!(key_on_disk, bundle.priv_key, "served key == issued key");
}

/// A non-admin caller is denied the producer RPC, and no namespace entry is
/// stored — the deployment's TLS cert is an admin-only concern.
#[tokio::test]
async fn non_admin_publish_cert_is_denied_and_nothing_stored() {
    let acme = tempfile::tempdir().unwrap();
    let (_h_url, h_state) = start_public_nest().await;
    let (_p_url, p_state) = start_private_nest(acme.path().to_path_buf()).await;

    // A non-admin actor (never added via add_admin_actor) → User class.
    let actor_sk = SigningKey::from_bytes(&[0x22u8; 32]);
    let actor: [u8; 32] = actor_sk.verifying_key().to_bytes();

    let bundle = lan_cert_bundle("home.example.com");
    let req = publish_request(&p_state, &actor_sk, &bundle);
    let err = dispatch(&h_state, actor, "fauna.tls.publish_cert", encode(&req))
        .await
        .expect_err("non-admin must be denied");
    assert!(
        err.code.contains("permission") || err.code.contains("denied"),
        "expected a permission error, got {:?}",
        err.code
    );

    // Nothing was written to the actor's namespace.
    let entries = h_state
        .db
        .namespace_entries_by_id(&actor, LAN_TLS_CERT_ENTRY_ID)
        .await
        .unwrap();
    assert!(entries.is_empty(), "denied publish must not store an entry");
}
