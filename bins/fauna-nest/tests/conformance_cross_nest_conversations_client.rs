//! **Spec Y2 slice 3 step 3** — real-wire (tier_3) two-nest *client-driven*
//! cross-nest conversation. The slice-2 capstone
//! (`conformance_cross_nest_conversations`) drives the MLS engines + relay
//! *directly*; **this** test drives the identical cross-nest flow through the
//! production client stack — `FaunaMlsBackend` over `NestConversationsRpc` over
//! an authenticated `NestClient` — exactly as the linux picker will. It is the
//! fixed destination of the federation client-integration work
//! (`docs/goal/architecture/federation.md` § Implementation status today →
//! "client re-consumption (slice 3 step 3)").
//!
//! Topology — two in-process nests on distinct sockets with **distinct** nest
//! identities (a faithful two-nest federation):
//!
//! * **Home H** (Alice's nest): serves the discovery + conversations WS kinds
//!   under `require_registration`, plus the originating federation relay. Alice
//!   authenticates here over the real socket and drives the backend.
//! * **Foreign F** (Bob's nest): serves anonymous discovery (`by_handle`) + the
//!   receiving federation HTTP routes (`/api/v1/{keypackage,welcome}/…`). Bob is
//!   registered with one published key package, so he is `addressable`.
//!
//! What only this test catches over slice-2's engine-direct proof: the
//! `FaunaMlsBackend::resolve_address` → `resolve_foreign` →
//! `actor_by_handle_remote` anon-discovery hop, AND the `bootstrap_group` →
//! `keypackage_fetch` / `welcome_deliver` data plane carrying a `peer_domain`
//! that the seam maps to the home-nest relay's `nest_url`. A break anywhere in
//! that client→seam→relay→peer chain fails here but passes slice-2.
//!
//! ## Loopback vs. canonical domain (the slice-3 design pin)
//!
//! The data plane derives the relay `nest_url` from the **resolved chip's handle
//! domain** (`peer_domain_for` → `resolve_handle_domain`), and `resolve_foreign`
//! builds that handle from F's `by_handle` reply `domain` — *not* from the typed
//! authority — matching the same-nest path (`resolve_address` uses `reply.domain`
//! too). In production F's `handle_domain` **is** its reachable domain, so reply
//! domain == typed authority == relay target; they only diverge for a loopback
//! test. We therefore keep the production path untouched and make the test
//! faithful: F's `handle_domain` is set to its own loopback authority
//! `127.0.0.1:<F-port>`, so `by_handle` replies with that authority, the chip is
//! `bob@127.0.0.1:<F-port>`, and `resolve_handle_domain` maps it straight back to
//! F's loopback URL. (Preserving the *typed* authority over `reply.domain` in
//! `resolve_foreign` was the rejected alternative — it would diverge the foreign
//! display handle from the canonical same-nest one for no production benefit.)
//!
//! ## Floor TLS (the Pillar-C follow-through)
//!
//! Since Pillar C (`730303718`) `resolve_handle_domain` derives **uniform
//! https** for every host-class — loopback included — so the derived discovery
//! and relay targets here are `https://127.0.0.1:<port>`. Both in-process nests
//! therefore serve the **self-signed floor TLS** with the channel binding wired
//! (`nest_signing_key` + `served_cert_spki`), mirroring
//! `onboarding_bare_localhost_tls_e2e.rs` / `tls_channel_binding_roundtrip.rs`:
//! the clients' authed connects graduate the SPKI pin through
//! `fauna.auth.handshake`, the anon discovery hop rides the capturing verifier,
//! and the H→F federation relay dials `wss://` (loopback-https rides
//! `validate_peer_url`'s test carve-out). That makes this journey cover
//! the production TLS trust path end-to-end — a surface the pre-Pillar-C
//! plain-HTTP fixture never exercised.

mod common;
use common::FixedSpki;
use common::connected_client;
use common::group_thread;
use common::one_to_one_thread;
use common::welcome_bytes_from_inbox;

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use fauna_client_conversations::NestConversationsRpc;

/// The wire code behind a refusal — the stable, contractual half of the wire
/// error, and what these relay tests are actually about.
///
/// They used to pin substrings of `err.to_string()` (`"forbidden"`,
/// `"federation"`, `"peer_nest_outdated"`). Since 2026-08-25
/// that renders the LOCALIZED sentence, which carries no wire vocabulary by
/// design (`version-compatibility.md` § Dimension 4) — so the old assertions
/// could not have passed for any code, mapped or not.
fn refusal_code(err: &fauna_client::NestClientError) -> &str {
    match err {
        fauna_client::NestClientError::Rpc(e) => e.code.as_str(),
        other => panic!("expected a wire-level refusal, got a transport fault: {other:?}"),
    }
}
use fauna_conversations::address::TypedAddress;
use fauna_conversations::backend::{
    ConversationsRpc, RailBackend, ResolveResult, ResolvedAttachment, RoomCeremonyRpc,
    RoomPolicyEdit, SchedulingSink,
};
use fauna_conversations::backends::fauna_mls::{
    FaunaMlsBackend, ingest_scheduling_welcome, ingest_welcome, poll_inbound_conv,
    poll_inbound_scheduling,
};
use fauna_conversations::compose::ComposeState;
use fauna_conversations::manager::ConversationsManager;
use fauna_conversations::thread::ThreadId;
use fauna_core::identity::ActorKeypair;
use fauna_mls::engine::MlsEngine;
use fauna_nest::acme::spki_sha256_of_cert_der;
use fauna_nest::db::CacheDb;
use fauna_nest::db::channels::RebindPower;
use fauna_nest::db::rooms::ReportedMember;
use fauna_nest::federation_channel::dial;
use fauna_nest::federation_handlers::{FedChannelFetchReply, FedChannelFetchRequest};
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

const FAR_FUTURE: u64 = u64::MAX / 2;

/// A fresh, distinct nest key pair — the channel-binding deployment key and the
/// federation identity derived from the SAME seed, exactly as production boot
/// does (`box-recovery.md` § Single-identity unification: `nest_identity` is a
/// view over the deployment signing key).
fn random_nest_keys() -> (SigningKey, NestIdentity) {
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let signing_key = SigningKey::from_bytes(&secret);
    let identity = NestIdentity::from_seed(&secret);
    (signing_key, identity)
}

/// A self-signed floor cert for the loopback authority + the TLS acceptor
/// serving it + the leaf's real SPKI (for the channel binding).
fn floor_tls() -> (tokio_rustls::TlsAcceptor, [u8; 32]) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into(), "localhost".into()])
        .expect("self-signed floor cert");
    let cert_der: CertificateDer<'static> = cert.cert.der().clone();
    let spki = spki_sha256_of_cert_der(cert_der.as_ref()).expect("served leaf SPKI");
    let key_der: PrivateKeyDer<'static> =
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));
    let server_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .expect("server tls config");
    (tokio_rustls::TlsAcceptor::from(Arc::new(server_cfg)), spki)
}

/// Home nest H: discovery + conversations WS kinds under `require_registration`
/// (so Alice must be a registered actor to auth), a distinct nest identity (to
/// sign the originating relay requests), and `handle_domain` set to its own
/// authority for faithfulness. Returns `(https_base, authority, state)`.
async fn start_home_nest() -> (String, String, Arc<AppState>) {
    let (listener, authority) = common::nest_listener().await;
    start_home_nest_on(listener, authority).await
}

/// [`start_home_nest`] on a listener the caller already bound and declared
/// fresh ([`common::fresh_nest_listener`]).
async fn start_home_nest_on(
    listener: tokio::net::TcpListener,
    authority: String,
) -> (String, String, Arc<AppState>) {
    let (acceptor, spki) = floor_tls();
    let (nest_signing_key, nest_identity) = random_nest_keys();

    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    // A real DiskBlobStore-backed BackupService so the public chunk/manifest
    // byte routes serve — the capstone's bytes-direct-from-the-home-nest leg
    // (passthrough store: no at-rest encryption/compression, exactly what a
    // content-addressed store is to client-sealed ciphertext).
    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    std::mem::forget(blob_dir); // outlives the test process; never deleted under test
    let backup_svc = Arc::new(
        fauna_nest::backup::service::BackupService::new(db.clone(), None, false, blob_path, None)
            .unwrap(),
    );
    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        nest_identity: Arc::new(nest_identity),
        // Floor-TLS channel binding: the auth handshake signs the SPKI the
        // listener actually serves, so `NestClient::connect` graduates the pin.
        nest_signing_key: Some(nest_signing_key),
        served_cert_spki: Some(Arc::new(FixedSpki(spki))),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            // The authenticated `NestClient` mints its bearer over the
            // pre-identity `fauna.auth.handshake` (WsChallengeBearer), so the
            // home nest's anon endpoint must serve the auth-bootstrap kinds.
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::conversations_handlers::register_conversations_handlers(&mut b);
            // Phase 2 cross-nest shared folders (the client-level capstone):
            // owner share / content_key.put / roster / evict + the member-side
            // relayed reads all ride these kinds; the custody + foreign-set
            // record is client-side account-plane state.
            fauna_nest::folder_handlers::register_folders_handlers(&mut b);
            fauna_nest::sync_handlers::register_sync_handlers(&mut b);
            fauna_nest::filesync_handlers::register_filesync_handlers(&mut b);
            // The inbox plane: a cross-nest community invitation lands as an
            // `InboxKind::RoomInvite` knock the invitee's client lists through
            // `fauna.inbox.fetch` (the room plane mints no read kind of its own).
            fauna_nest::inbox_handlers::register_inbox_handlers(&mut b);
            b.build()
        }),
        // The federation channel is the sole Fauna↔Fauna carrier (slice 5);
        // populate the channel serving handlers so the relay rides the channel
        // (the empty `for_test` router would reject every federation kind).
        federation_router: Arc::new({
            let mut b = fauna_nest::federation_router::FederationRouter::builder();
            fauna_nest::federation_handlers::register_federation_handlers(&mut b);
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
        ..AppState::for_test(db)
    });

    let app = fauna_nest::build_router(state.clone());
    tokio::spawn(fauna_nest::serve_tls(
        listener,
        acceptor,
        app.into_make_service(),
        // Loopback client → bounded by the loopback ceiling (1024), so any admin cap works.
        fauna_conn_limit::PerIpConnLimit::new(fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP),
    ));
    (format!("https://{authority}"), authority, state)
}

/// Foreign nest F: anonymous discovery (`fauna.actor.by_handle`) + the receiving
/// federation channel handlers, a distinct nest identity (to mutually
/// authenticate the channel handshake), and — the slice-3 pin — `handle_domain`
/// set to **its own loopback authority** so `by_handle` reports a domain the home
/// relay can actually reach. Returns `(https_base, authority, state)`.
async fn start_foreign_nest() -> (String, String, Arc<AppState>) {
    let (listener, authority) = common::nest_listener().await;
    let (acceptor, spki) = floor_tls();
    let (nest_signing_key, nest_identity) = random_nest_keys();

    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    // A real DiskBlobStore-backed BackupService so the public chunk/manifest
    // byte routes serve — the capstone's bytes-direct-from-the-home-nest leg
    // (passthrough store: no at-rest encryption/compression, exactly what a
    // content-addressed store is to client-sealed ciphertext).
    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    std::mem::forget(blob_dir); // outlives the test process; never deleted under test
    let backup_svc = Arc::new(
        fauna_nest::backup::service::BackupService::new(db.clone(), None, false, blob_path, None)
            .unwrap(),
    );
    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        nest_identity: Arc::new(nest_identity),
        // Floor-TLS channel binding (see `start_home_nest`).
        nest_signing_key: Some(nest_signing_key),
        served_cert_spki: Some(Arc::new(FixedSpki(spki))),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            // A full nest: F's own members (Bob) authenticate here and drive their
            // conversations backend — the cross-nest *message receipt* test has Bob
            // run a real `FaunaMlsBackend` over an authed `NestClient` to F, whose
            // `channel.fetch` relays to the channel's home nest H.
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::conversations_handlers::register_conversations_handlers(&mut b);
            // Phase 2 cross-nest shared folders (the client-level capstone):
            // owner share / content_key.put / roster / evict + the member-side
            // relayed reads all ride these kinds; the custody + foreign-set
            // record is client-side account-plane state.
            fauna_nest::folder_handlers::register_folders_handlers(&mut b);
            fauna_nest::sync_handlers::register_sync_handlers(&mut b);
            fauna_nest::filesync_handlers::register_filesync_handlers(&mut b);
            // The inbox plane: a cross-nest community invitation lands as an
            // `InboxKind::RoomInvite` knock the invitee's client lists through
            // `fauna.inbox.fetch` (the room plane mints no read kind of its own).
            fauna_nest::inbox_handlers::register_inbox_handlers(&mut b);
            // F is its members' OWN nest, so the client-facing `fauna.posts.*`
            // plane lands here — including the room-post verdict read and the
            // relayed twin F originates to a room's home for a member seated
            // on a room it does not home.
            fauna_nest::posts_handlers::register_posts_handlers(&mut b);
            b.build()
        }),
        // Serve the relayed federation kinds over the channel (slice 5: the sole
        // Fauna↔Fauna carrier; the empty `for_test` router would reject them).
        federation_router: Arc::new({
            let mut b = fauna_nest::federation_router::FederationRouter::builder();
            fauna_nest::federation_handlers::register_federation_handlers(&mut b);
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
        ..AppState::for_test(db)
    });

    let app = fauna_nest::build_router(state.clone());
    tokio::spawn(fauna_nest::serve_tls(
        listener,
        acceptor,
        app.into_make_service(),
        // Loopback client → bounded by the loopback ceiling (1024), so any admin cap works.
        fauna_conn_limit::PerIpConnLimit::new(fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP),
    ));
    (format!("https://{authority}"), authority, state)
}

#[tokio::test]
async fn alice_resolves_and_messages_bob_across_nests_through_backend() {
    let (h_base, h_authority, h_state) = start_home_nest().await;
    let (_f_base, f_authority, f_state) = start_foreign_nest().await;

    // Bob lives on F: real MLS engine, registered handle, one published key
    // package (so F reports `addressable = true`).
    let bob_kp = ActorKeypair::generate();
    let bob_id = bob_kp.actor_id();
    let bob_hex = hex::encode(bob_id.0);
    f_state
        .db
        .create_user_with_handle(&bob_id.0, "free", "bob", None)
        .await
        .unwrap();
    let bob_engine = MlsEngine::new_in_memory(bob_kp).expect("bob engine");
    let bob_pkgs = bob_engine.generate_key_packages_bytes(1).expect("bob KP");
    f_state
        .db
        .put_key_package("bob-kp-0", &bob_id.0, &bob_pkgs[0], 0, FAR_FUTURE)
        .await
        .unwrap();

    // Alice lives on H. The same Ed25519 identity backs both her authenticated
    // `NestClient` (WS auth signs with the actor key) and the local `MlsEngine`
    // the backend uses to form the group — so reconstruct two keypairs from one
    // secret (`ActorKeypair` is not `Clone`).
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();
    let alice_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).expect("engine"),
    );

    let nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let rpc: Arc<dyn ConversationsRpc> = Arc::new(NestConversationsRpc::new(nest));
    let backend = FaunaMlsBackend::new(
        alice_engine.clone(),
        rpc,
        format!("alice@{h_authority}"),
        alice_id,
    );

    // ── Part 1 — resolve the foreign handle (the primary step-3 deliverable) ──
    //
    // The same-nest probe on H finds no `bob`; the typed domain is F's loopback
    // authority (≠ H's), so the backend routes to `resolve_foreign`, which opens
    // an anon connection to F and reads `addressable` off `by_handle`. The chip's
    // handle echoes F's reply domain (its authority), so the data plane can reach
    // it back.
    let typed = format!("bob@{f_authority}");
    let chip = match backend.resolve_address(&typed).await {
        ResolveResult::Resolved(addr @ TypedAddress::Fauna { .. }) => addr,
        other => panic!("expected Resolved(Fauna), got {other:?}"),
    };
    match &chip {
        TypedAddress::Fauna { actor_id, handle } => {
            assert_eq!(*actor_id, bob_id, "resolved to the wrong actor");
            assert_eq!(
                handle, &typed,
                "chip handle carries F's reachable authority"
            );
        }
        _ => unreachable!(),
    }

    // An unknown handle on F is `fauna.actor.not_found` over the anon hop →
    // `NotFound` (the manager's chain falls through to the SMTP rail).
    assert_eq!(
        backend
            .resolve_address(&format!("nobody@{f_authority}"))
            .await,
        ResolveResult::NotFound,
    );

    // ── Part 2 — message the resolved foreign chip (the integrated proof) ─────
    //
    // `send` on an unbound thread lazily bootstraps the group: fetch Bob's KP
    // (home relay → F), `create_group`, deliver the Welcome (home relay → F),
    // then post the chat message to H's channel log. We assert the Welcome
    // crossed to Bob's inbox on F and that joining it forms one shared MLS group.
    let thread = one_to_one_thread(ThreadId("t-xnest".into()), chip);
    let compose = ComposeState {
        body_draft: "hello across nests".into(),
        ..Default::default()
    };
    backend
        .send(&thread, &compose, &[])
        .await
        .expect("cross-nest send (bootstrap + relay)");

    // Exactly one Welcome was relayed into Bob's inbox on F.
    let inbox = f_state.db.list_inbox_all(&bob_id.0).await.unwrap();
    assert_eq!(inbox.len(), 1, "exactly one welcome relayed to Bob on F");
    let welcome = welcome_bytes_from_inbox(&inbox[0].1);
    let bob_channel = bob_engine
        .join_from_welcome_bytes(&welcome)
        .expect("Bob joins the cross-nest group from the relayed Welcome");

    // End goal: one MLS group spanning H and F, driven entirely through the
    // production client stack.
    let bound = backend.bound_channels();
    assert_eq!(bound.len(), 1, "the send bound exactly one channel");
    let alice_channel = bound[0];
    assert_eq!(
        bob_channel.0, alice_channel.0,
        "Alice and Bob share the same MLS channel id across nests"
    );
    let alice_members = alice_engine.group_members(&alice_channel);
    assert!(
        alice_members.iter().any(|m| m.0 == bob_id.0),
        "Alice's group view includes Bob"
    );
    assert_eq!(alice_members.len(), 2, "the group is exactly Alice + Bob");
    assert_eq!(
        bob_engine.group_members(&bob_channel).len(),
        2,
        "Bob's group view also has both members"
    );

    // (The chat `channel_send` to H's log succeeding is implicit in `send`
    // returning `Ok` above — `post_app_message` is its final step.)
    let _ = (h_state, bob_hex);
}

/// The add-participant heal's roster discipline for a **cross-nest** member,
/// driven from the channel's home nest (`mls-group-key-material.md` § M2, chat
/// bullet — the cross-nest leg, slice 1):
///
/// 1. H's `fauna.conversations.channel.actors` answers the **union** of its
///    `actor_channels` rows and its `channel_foreign_members` rows — so Bob,
///    whose row H wrote when it relayed his Welcome to F, reads as ON the
///    roster despite living on another nest.
/// 2. A duplicate add of that healthy foreign member is the **idempotent
///    no-op** arm. Red-first (union disabled): the partial roster read
///    `Some(false)`, the heal evicted + re-admitted the working member, and a
///    SECOND Welcome landed in Bob's inbox on F.
#[tokio::test]
async fn duplicate_add_of_a_healthy_cross_nest_member_is_a_no_op_on_the_home_nest() {
    let (h_base, h_authority, h_state) = start_home_nest().await;
    let (_f_base, f_authority, f_state) = start_foreign_nest().await;

    // Bob lives on F (two published KPs: the bootstrap consumes one, and a
    // wrongful evict + re-admit would consume the second — the test must not
    // fail on KP exhaustion instead of the assertion).
    let bob_kp = ActorKeypair::generate();
    let bob_id = bob_kp.actor_id();
    let bob_hex = hex::encode(bob_id.0);
    f_state
        .db
        .create_user_with_handle(&bob_id.0, "free", "bob", None)
        .await
        .unwrap();
    let bob_engine = MlsEngine::new_in_memory(bob_kp).expect("bob engine");
    for (i, pkg) in bob_engine
        .generate_key_packages_bytes(2)
        .expect("bob KPs")
        .iter()
        .enumerate()
    {
        f_state
            .db
            .put_key_package(&format!("bob-kp-{i}"), &bob_id.0, pkg, 0, FAR_FUTURE)
            .await
            .unwrap();
    }

    // Alice lives on H.
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();
    let alice_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).expect("engine"),
    );
    let nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let rpc: Arc<dyn ConversationsRpc> = Arc::new(NestConversationsRpc::new(nest));
    let rpc_probe = Arc::clone(&rpc);
    let backend = FaunaMlsBackend::new(
        alice_engine.clone(),
        rpc,
        format!("alice@{h_authority}"),
        alice_id,
    );

    // Bootstrap the cross-nest group (fetch Bob's KP via relay, create, relay
    // the Welcome) — same lazy path test 1 proves.
    let typed = format!("bob@{f_authority}");
    let chip = match backend.resolve_address(&typed).await {
        ResolveResult::Resolved(addr @ TypedAddress::Fauna { .. }) => addr,
        other => panic!("expected Resolved(Fauna), got {other:?}"),
    };
    let thread_id = ThreadId("t-xnest-dup".into());
    let thread = one_to_one_thread(thread_id.clone(), chip.clone());
    let compose = ComposeState {
        body_draft: "hello across nests".into(),
        ..Default::default()
    };
    backend
        .send(&thread, &compose, &[])
        .await
        .expect("cross-nest send (bootstrap + relay)");
    assert_eq!(
        f_state.db.list_inbox_all(&bob_id.0).await.unwrap().len(),
        1,
        "exactly one Welcome relayed to Bob on F after the bootstrap"
    );
    let alice_channel = backend.bound_channels()[0];

    // (1) The union: the home nest's roster read lists Bob — his row lives in
    // `channel_foreign_members` (written at relay time), not `actor_channels`.
    let actors = rpc_probe
        .channel_actors(alice_channel.to_string(), None)
        .await
        .expect("channel.actors transport")
        .expect("the home nest knows the kind");
    assert!(
        actors.iter().any(|a| a.eq_ignore_ascii_case(&bob_hex)),
        "the home nest's roster union must include the cross-nest member; got {actors:?}"
    );

    // (2) The duplicate add is the idempotent no-op arm — not an evict +
    // re-admit of a working member.
    let epoch_before = alice_engine.current_epoch(&alice_channel).unwrap();
    backend
        .add_participant(thread_id, chip)
        .await
        .expect("a duplicate add of a healthy member is a no-op, not an error");
    assert_eq!(
        alice_engine.current_epoch(&alice_channel).unwrap(),
        epoch_before,
        "no-op means no commit: the epoch is untouched (an evict + re-admit advances it twice)"
    );
    assert_eq!(
        f_state.db.list_inbox_all(&bob_id.0).await.unwrap().len(),
        1,
        "no second Welcome in Bob's inbox — the healthy member was not re-invited"
    );
    assert_eq!(
        alice_engine.group_members(&alice_channel).len(),
        2,
        "the group is still exactly Alice + Bob"
    );
}

/// The **message-receipt** counterpart of the test above (the goal here —
/// `direct-messages.md` § Technical Flow — Cross-Nest, step 3; closes
/// `caldav-server.md` Slice 6b for conversations). The test above proves the
/// Welcome crosses + the group spans both nests; **this** proves Bob, a member on
/// the **unpaired** foreign nest F, actually *receives + decrypts* Alice's
/// application message through the normal receive loop — the gap the
/// `fauna.federation.channel.fetch` membership-gated relay closes.
///
/// Bob runs a real `FaunaMlsBackend` over an authed `NestClient` to **his own**
/// nest F. When Alice (on H) sends, H buffers the ciphertext on its channel log and
/// records Bob as a foreign member (home nest F). Bob ingests the relayed Welcome
/// with the channel's home nest URL (H), then `poll_inbound_conv` drains it: the
/// `channel.fetch` carries that home URL, so F relays it as
/// `fauna.federation.channel.fetch` to H, which authorizes Bob's membership and
/// returns the ciphertext F could never hold. Bob decrypts it locally.
#[tokio::test]
async fn bob_receives_alices_cross_nest_message_through_the_relay() {
    let (h_base, h_authority, h_state) = start_home_nest().await;
    let (f_base, f_authority, f_state) = start_foreign_nest().await;

    // Bob lives on F: real MLS engine, registered handle, one published key package
    // (so he is `addressable` + can join the relayed Welcome).
    let mut bob_secret = [0u8; 32];
    getrandom::fill(&mut bob_secret).unwrap();
    let bob_id = ActorKeypair::from_secret(bob_secret).actor_id();
    f_state
        .db
        .create_user_with_handle(&bob_id.0, "free", "bob", None)
        .await
        .unwrap();
    let bob_engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(bob_secret)).expect("engine"));
    let bob_pkgs = bob_engine.generate_key_packages_bytes(1).expect("bob KP");
    f_state
        .db
        .put_key_package("bob-kp-0", &bob_id.0, &bob_pkgs[0], 0, FAR_FUTURE)
        .await
        .unwrap();

    // Alice lives on H.
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();
    let alice_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).expect("engine"),
    );

    // Alice's production backend on H, Bob's on F — each over its own authed nest.
    let alice_nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let alice_nest_handle = Arc::clone(&alice_nest);
    let alice_backend = Arc::new(FaunaMlsBackend::new(
        alice_engine.clone(),
        Arc::new(NestConversationsRpc::new(alice_nest)) as Arc<dyn ConversationsRpc>,
        format!("alice@{h_authority}"),
        alice_id,
    ));
    let bob_nest = connected_client(&f_base, ActorKeypair::from_secret(bob_secret)).await;
    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob_engine.clone(),
        Arc::new(NestConversationsRpc::new(bob_nest)) as Arc<dyn ConversationsRpc>,
        format!("bob@{f_authority}"),
        bob_id,
    ));
    // The manager must know the FaunaMls backend to route an inbound message into a
    // thread (else `ingest_inbound_to_thread` is `NotSupported`) — the production
    // wiring `ConversationsSession` does.
    bob_manager.register_backend(bob_backend.clone());

    // ── Alice resolves bob@F and sends — bootstrap relays the Welcome to F AND
    //    records Bob as a foreign member of the channel on H, then `channel.send`
    //    buffers the ciphertext on H's channel log. ──
    let chip = match alice_backend
        .resolve_address(&format!("bob@{f_authority}"))
        .await
    {
        ResolveResult::Resolved(addr @ TypedAddress::Fauna { .. }) => addr,
        other => panic!("expected Resolved(Fauna), got {other:?}"),
    };
    let thread = one_to_one_thread(ThreadId("t-xnest-msg".into()), chip.clone());
    let compose = ComposeState {
        body_draft: "hello across nests".into(),
        ..Default::default()
    };
    alice_backend
        .send(&thread, &compose, &[])
        .await
        .expect("cross-nest send (bootstrap + relay + channel.send)");

    // ── Bob ingests the relayed Welcome (with H as the channel's home nest URL),
    //    then drains via the production receive path. `poll_inbound_conv`'s
    //    `channel.fetch` carries the home URL, so F relays it to H and Bob decrypts
    //    the message F never held. ──
    let inbox = f_state.db.list_inbox_all(&bob_id.0).await.unwrap();
    assert_eq!(inbox.len(), 1, "exactly one welcome relayed to Bob on F");
    let welcome = welcome_bytes_from_inbox(&inbox[0].1);
    let channel_hex = alice_backend.bound_channels()[0].to_string();

    let bob_thread = ingest_welcome(&bob_backend, &bob_manager, &channel_hex, &welcome, &h_base)
        .await
        .expect("bob joins the cross-nest group from the relayed Welcome");
    let bob_channel = bob_backend.bound_channels()[0];

    let mut after = 0i64;
    let ingested = poll_inbound_conv(&bob_backend, &bob_manager, &bob_channel, &mut after, 0)
        .await
        .expect("bob drains the cross-nest channel via the federation relay")
        .ingested;
    assert_eq!(
        ingested, 1,
        "Bob receives exactly one cross-nest application message"
    );

    // ── The drain crossed `require_foreign_member` on H, so Bob's home-nest
    // binding must now be PINNED — first-use confirmation through the real
    // relayed federated fetch, not a test shim (`federation.md` § Cross-nest
    // shared folders + channel append, the TOFU bullet's two-layer bound). ──
    let channel_bytes: [u8; 32] = hex::decode(&channel_hex)
        .expect("channel hex")
        .try_into()
        .expect("32 bytes");
    assert_eq!(
        h_state
            .db
            .foreign_member_binding(&channel_bytes, &bob_id.0)
            .await
            .unwrap()
            .map(|(_, confirmed)| confirmed),
        Some(true),
        "Bob's first federated drain must pin his home-nest binding on H \
         (first-use confirmation)"
    );

    let detail = bob_manager
        .thread_detail(bob_thread.clone())
        .expect("thread");
    assert!(
        detail
            .messages
            .iter()
            .any(|m| m.body == "hello across nests"),
        "Alice's message decrypted + landed in Bob's thread; got {:?}",
        detail.messages.iter().map(|m| &m.body).collect::<Vec<_>>()
    );

    // A second drain with the advanced cursor ingests nothing (no double-count over
    // the relay).
    let again = poll_inbound_conv(&bob_backend, &bob_manager, &bob_channel, &mut after, 0)
        .await
        .expect("second drain ok")
        .ingested;
    assert_eq!(again, 0, "the cursor prevents re-ingest over the relay");

    // ── Success (d) round-trip: Bob REPLIES through the production send path.
    // The channel's home is H (recorded at Welcome ingest), so
    // `FaunaMlsBackend::send_on_channel` routes the distinct
    // `fauna.conversations.channel.send_remote` kind to Bob's OWN nest F, which
    // originates `fauna.federation.channel.append` to H; H's structural gate
    // admits Bob's foreign-member row and the ciphertext lands on H's log —
    // then Alice drains her home log and decrypts Bob's reply locally
    // (`direct-messages.md` § step 3b). Before send_remote
    // a foreign member's plain `channel.send` blackholed on F's local log. ──
    let bob_detail = bob_manager.thread_detail(bob_thread).expect("bob thread");
    bob_backend
        .send(
            &bob_detail,
            &ComposeState {
                body_draft: "hello back across nests".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("bob's cross-nest reply (send_remote -> federated append)");

    // Alice's drain side: a FRESH backend over her same engine + connection
    // (the relaunch shape) holding exactly ONE binding — the
    // manager-materialized thread. (Re-binding on her send backend would leave
    // two thread ids mapped to the channel, and `thread_for_channel`'s scan
    // order is not defined.)
    let alice_channel = alice_backend.bound_channels()[0];
    let alice_drain = Arc::new(FaunaMlsBackend::new(
        alice_engine.clone(),
        Arc::new(NestConversationsRpc::new(Arc::clone(&alice_nest_handle)))
            as Arc<dyn ConversationsRpc>,
        format!("alice@{h_authority}"),
        alice_id,
    ));
    let alice_manager = ConversationsManager::new();
    alice_manager.register_backend(alice_drain.clone());
    let alice_thread =
        alice_manager.materialize_conv_thread(alice_channel.to_string(), vec![chip.clone()]);
    alice_drain.bind_channel(alice_thread.clone(), alice_channel);

    let mut a_after = 0i64;
    let a_out = poll_inbound_conv(
        &alice_drain,
        &alice_manager,
        &alice_channel,
        &mut a_after,
        0,
    )
    .await
    .expect("alice drains her home log");
    assert!(
        a_out.ingested >= 1,
        "alice ingested nothing (cursor at {a_after}) — did bob's send_remote reach H's log?"
    );
    let a_detail = alice_manager
        .thread_detail(alice_thread)
        .expect("alice thread");
    assert!(
        a_detail
            .messages
            .iter()
            .any(|m| m.body == "hello back across nests"),
        "Bob's send_remote reply landed on H's log and decrypted for Alice; got {:?}",
        a_detail
            .messages
            .iter()
            .map(|m| &m.body)
            .collect::<Vec<_>>()
    );

    let _ = (h_state, f_state);
}

/// Encode a typed payload into an L3 `Value` for a raw federation request.
fn fed_value<T: serde::Serialize>(t: &T) -> fauna_protocol::Value {
    fauna_protocol::decode_strict::<fauna_protocol::Value>(
        &fauna_protocol::encode_canonical(t).unwrap(),
    )
    .unwrap()
}

/// Deadline-poll a foreign member's recorded `(handle, domain)`. The id→handle
/// announce's verification runs off the fetch path — a spawned task the reply
/// never awaits (`federation_handlers::record_announced_handle`,
/// ) — so the DB write lands
/// sometime after the fetch that triggered it, not necessarily before it
/// replies. Poll rather than assert immediately (`e2e-conventions.md`
/// convention 14: latency-independent assertions, never wall-clock timing).
async fn poll_foreign_member_handle(
    db: &CacheDb,
    channel: &[u8; 32],
    actor: &[u8; 32],
    expected: Option<(&str, &str)>,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let got = db.foreign_member_handle(channel, actor).await.unwrap();
        if got.as_ref().map(|(h, d)| (h.as_str(), d.as_str())) == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for foreign_member_handle to become {expected:?}, \
             last saw {got:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// **The id→handle ruling, end to end** (`federation.md` § Cross-nest shared
/// folders + channel append, the id→handle bullet, ratified 2026-09-10): a member homed on another nest is named on the
/// room's home not because anybody *asked* its nest, but because its own home
/// nest — the authority for handles at its domain — volunteers `handle@domain`
/// on the `channel.fetch` it already relays for that member, and the room's
/// home records it beside the binding only after binding the domain to the
/// announcing nest's key by discovery. The roster read then names the member
/// exactly as it names a local one, and the client seam seats it in the same
/// canonical `handle@domain` form.
///
/// Four pins, in order:
/// 1. Before Bob's first drain, H holds no handle for him — the announce is
///    the member's own act, never a lookup H makes on its own.
/// 2. Bob's honest drain (F relays `channel.fetch` to H carrying `bob@F`)
///    lands `bob@F` on H, and Alice's roster read through the production
///    client seam seats him as `bob@<F's domain>`.
/// 3. **The spoof is refused**: F asserting its member at H's own domain
///    (a domain that resolves to another nest's key) changes nothing — the
///    fetch itself still serves, the name is simply not believed.
/// 4. A rename at F's own domain lands (the announce rides every drain, so a
///    rename heals on the next one), and a malformed handle is ignored.
#[tokio::test]
async fn a_foreign_members_handle_rides_its_own_drain_and_the_room_home_names_it() {
    let (h_base, h_authority, h_state) = start_home_nest().await;
    let (f_base, f_authority, f_state) = start_foreign_nest().await;

    // Bob lives on F with a registered handle and one published key package.
    let mut bob_secret = [0u8; 32];
    getrandom::fill(&mut bob_secret).unwrap();
    let bob_id = ActorKeypair::from_secret(bob_secret).actor_id();
    f_state
        .db
        .create_user_with_handle(&bob_id.0, "free", "bob", None)
        .await
        .unwrap();
    let bob_engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(bob_secret)).expect("engine"));
    let bob_pkgs = bob_engine.generate_key_packages_bytes(1).expect("bob KP");
    f_state
        .db
        .put_key_package("bob-kp-0", &bob_id.0, &bob_pkgs[0], 0, FAR_FUTURE)
        .await
        .unwrap();

    // Alice lives on H, with a handle of her own so the local join is visible
    // beside the foreign one.
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user_with_handle(&alice_id.0, "free", "alice", None)
        .await
        .unwrap();
    let alice_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).expect("engine"),
    );

    let alice_nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let alice_backend = Arc::new(FaunaMlsBackend::new(
        alice_engine.clone(),
        Arc::new(NestConversationsRpc::new(Arc::clone(&alice_nest))) as Arc<dyn ConversationsRpc>,
        format!("alice@{h_authority}"),
        alice_id,
    ));
    let bob_nest = connected_client(&f_base, ActorKeypair::from_secret(bob_secret)).await;
    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob_engine.clone(),
        Arc::new(NestConversationsRpc::new(bob_nest)) as Arc<dyn ConversationsRpc>,
        format!("bob@{f_authority}"),
        bob_id,
    ));
    bob_manager.register_backend(bob_backend.clone());

    // Alice resolves bob@F and sends: the Welcome relays to F and H records Bob
    // as a foreign member of the channel.
    let chip = match alice_backend
        .resolve_address(&format!("bob@{f_authority}"))
        .await
    {
        ResolveResult::Resolved(addr @ TypedAddress::Fauna { .. }) => addr,
        other => panic!("expected Resolved(Fauna), got {other:?}"),
    };
    let thread = one_to_one_thread(ThreadId("t-xnest-name".into()), chip.clone());
    alice_backend
        .send(
            &thread,
            &ComposeState {
                body_draft: "who is this?".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("cross-nest send (bootstrap + relay + channel.send)");
    let channel_hex = alice_backend.bound_channels()[0].to_string();
    let channel_bytes: [u8; 32] = hex::decode(&channel_hex)
        .expect("channel hex")
        .try_into()
        .expect("32 bytes");

    // 1. Nobody has announced anything yet: H knows Bob only as an actor id.
    assert_eq!(
        h_state
            .db
            .foreign_member_handle(&channel_bytes, &bob_id.0)
            .await
            .unwrap(),
        None,
        "before the member's first drain the room's home holds no handle — \
         it never asks; the member's own nest tells"
    );

    // 2. Bob's first drain: F relays `channel.fetch` to H, and — being Bob's
    //    home and the authority for handles at its domain — announces
    //    `bob@F` on it. H binds F's domain to F's key by discovery and
    //    records the pair on Bob's binding row.
    let inbox = f_state.db.list_inbox_all(&bob_id.0).await.unwrap();
    assert_eq!(inbox.len(), 1, "exactly one welcome relayed to Bob on F");
    let welcome = welcome_bytes_from_inbox(&inbox[0].1);
    ingest_welcome(&bob_backend, &bob_manager, &channel_hex, &welcome, &h_base)
        .await
        .expect("bob joins the cross-nest group from the relayed Welcome");
    let bob_channel = bob_backend.bound_channels()[0];
    let mut after = 0i64;
    poll_inbound_conv(&bob_backend, &bob_manager, &bob_channel, &mut after, 0)
        .await
        .expect("bob drains the cross-nest channel via the federation relay");
    // Bob's own drain carried his home nest's announce; H's verification runs
    // off the fetch path now, so the binding lands sometime after the drain
    // replied, not necessarily before — poll (see `poll_foreign_member_handle`).
    poll_foreign_member_handle(
        &h_state.db,
        &channel_bytes,
        &bob_id.0,
        Some(("bob", &f_authority)),
    )
    .await;

    // The floor roster seats both. (A 1:1 thread is not a governed room, so
    // no device reports one; the report door is pinned elsewhere — what this
    // test pins is the read's join.)
    h_state
        .db
        .replace_floor_roster(
            &channel_bytes,
            "end_to_end",
            "",
            None,
            None,
            &[
                ReportedMember {
                    principal_id: alice_id.0,
                    principal_kind: "user".into(),
                    role: Some("owner".into()),
                    home_node_url: String::new(),
                    reception_pubkey: Vec::new(),
                },
                ReportedMember {
                    principal_id: bob_id.0,
                    principal_kind: "user".into(),
                    role: Some("member".into()),
                    home_node_url: f_base.clone(),
                    reception_pubkey: Vec::new(),
                },
            ],
        )
        .await
        .expect("seat alice and bob on the floor");

    // Alice reads the roster through the production client seam — the same
    // `RoomRosterReader` the poll loop's nameless-member resolution drives.
    let alice_reader = NestConversationsRpc::new(Arc::clone(&alice_nest));
    let read = || {
        let channel_hex = channel_hex.clone();
        let reader = &alice_reader;
        async move {
            let floor = fauna_conversations::backend::RoomRosterReader::read_roster(
                reader,
                channel_hex,
                None,
            )
            .await
            .or_absent()
            .expect("a member reads its own room's floor");
            let of = |actor: fauna_core::identity::ActorId| {
                floor
                    .members
                    .iter()
                    .find(|m| m.actor == actor)
                    .expect("on the roster")
                    .qualified_handle()
            };
            (of(alice_id), of(bob_id))
        }
    };
    let (alice_seat, bob_seat) = read().await;
    assert_eq!(
        alice_seat.as_deref(),
        Some(format!("alice@{h_authority}").as_str()),
        "the local member is named from H's own users row, as before"
    );
    assert_eq!(
        bob_seat.as_deref(),
        Some(format!("bob@{f_authority}").as_str()),
        "the foreign member is named from his own home nest's verified \
         announce — the same canonical handle@domain a typed recipient \
         resolves to, from a nest that holds no users row for him"
    );

    // 3. The spoof. F (authenticated as itself) asserts Bob at H's OWN domain
    //    — a domain that resolves to H's key, not F's. The fetch still serves
    //    (a name never gates a read); the assertion is not believed.
    let h_nest_hex = hex::encode(h_state.nest_identity.public_key_bytes());
    let conn = dial(&f_state, &h_base, &h_nest_hex)
        .await
        .expect("F dials H over the federation channel");
    let raw_fetch = |key: u8, handle: &str, domain: &str| {
        let conn = &conn;
        let req = FedChannelFetchRequest {
            requesting_actor_id: hex::encode(bob_id.0),
            channel_id: channel_hex.clone(),
            after: 0,
            limit: 0,
            requesting_handle: Some(handle.to_string()),
            requesting_domain: Some(domain.to_string()),
        };
        async move {
            let value = conn
                .dispatcher
                .request_raw(
                    "fauna.federation.channel.fetch",
                    [key; 16],
                    fed_value(&req),
                    None,
                )
                .await
                .expect("request accepted")
                .await_reply()
                .await
                .expect("channel.fetch serves whatever the announce says");
            let reply: FedChannelFetchReply =
                fauna_protocol::decode_strict(&fauna_protocol::encode_canonical(&value).unwrap())
                    .unwrap();
            reply.messages.len()
        }
    };
    assert_eq!(
        raw_fetch(0x51, "mallory", &h_authority).await,
        1,
        "the drain serves its one record regardless of the announce"
    );
    assert_eq!(
        h_state
            .db
            .foreign_member_handle(&channel_bytes, &bob_id.0)
            .await
            .unwrap(),
        Some(("bob".to_string(), f_authority.clone())),
        "a nest naming its member at a domain that resolves to ANOTHER nest's \
         key is refused: bob@F stands, mallory@H never lands"
    );
    let (_, bob_seat) = read().await;
    assert_eq!(
        bob_seat.as_deref(),
        Some(format!("bob@{f_authority}").as_str()),
        "and Alice keeps seeing the verified name"
    );

    // 4. A rename at F's own domain is believed — the announce rides every
    //    drain, so a rename heals on the member's next one — and a malformed
    //    handle is ignored rather than stored.
    assert_eq!(raw_fetch(0x52, "robert", &f_authority).await, 1);
    // Same async-landing note as pin 2 — poll for the rename rather than
    // asserting immediately.
    poll_foreign_member_handle(
        &h_state.db,
        &channel_bytes,
        &bob_id.0,
        Some(("robert", &f_authority)),
    )
    .await;
    assert_eq!(raw_fetch(0x53, "Not A Handle!", &f_authority).await, 1);
    assert_eq!(
        h_state
            .db
            .foreign_member_handle(&channel_bytes, &bob_id.0)
            .await
            .unwrap(),
        Some(("robert".to_string(), f_authority.clone())),
        "a handle that fails the shared handle grammar is ignored, never stored"
    );
    let (_, bob_seat) = read().await;
    assert_eq!(
        bob_seat.as_deref(),
        Some(format!("robert@{f_authority}").as_str()),
        "the roster read follows the rename on its next read"
    );

    let _ = (h_state, f_state);
}

/// **The alternating-domain announce is bounded — , LOW.** A peer that alternates its announce
/// between two domains that never resolve costs the home nest at most one
/// discovery attempt **per domain** per `ANNOUNCE_VERIFY_TTL` window, never
/// one per fetch — the pre-fix code re-attempted on every distinct
/// `(handle, domain)` pair, so alternating cost one discovery per *fetch*.
/// `federation_handlers::record_announced_handle` also moved the
/// verification off the fetch path (a spawned task the reply never awaits),
/// which is a code-shape guarantee this test does not re-time — the bound
/// under test is the attempt *count*, read off
/// `FederationChannelPool::domain_resolve_attempt_count` rather than a
/// stopwatch (`e2e-conventions.md` convention 14: assert latency-independent
/// state, never wall-clock timing).
#[tokio::test]
async fn an_alternating_unresolvable_announce_costs_one_discovery_per_domain_not_per_fetch() {
    let (h_base, h_authority, h_state) = start_home_nest().await;
    let (f_base, f_authority, f_state) = start_foreign_nest().await;

    // Bob lives on F with a registered handle and one published key package.
    let mut bob_secret = [0u8; 32];
    getrandom::fill(&mut bob_secret).unwrap();
    let bob_id = ActorKeypair::from_secret(bob_secret).actor_id();
    f_state
        .db
        .create_user_with_handle(&bob_id.0, "free", "bob", None)
        .await
        .unwrap();
    let bob_engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(bob_secret)).expect("engine"));
    let bob_pkgs = bob_engine.generate_key_packages_bytes(1).expect("bob KP");
    f_state
        .db
        .put_key_package("bob-kp-0", &bob_id.0, &bob_pkgs[0], 0, FAR_FUTURE)
        .await
        .unwrap();

    // Alice lives on H.
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user_with_handle(&alice_id.0, "free", "alice", None)
        .await
        .unwrap();
    let alice_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).expect("engine"),
    );

    let alice_nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let alice_backend = Arc::new(FaunaMlsBackend::new(
        alice_engine.clone(),
        Arc::new(NestConversationsRpc::new(Arc::clone(&alice_nest))) as Arc<dyn ConversationsRpc>,
        format!("alice@{h_authority}"),
        alice_id,
    ));
    let bob_nest = connected_client(&f_base, ActorKeypair::from_secret(bob_secret)).await;
    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob_engine.clone(),
        Arc::new(NestConversationsRpc::new(bob_nest)) as Arc<dyn ConversationsRpc>,
        format!("bob@{f_authority}"),
        bob_id,
    ));
    bob_manager.register_backend(bob_backend.clone());

    // Alice resolves bob@F and sends: the Welcome relays to F and H records
    // Bob as a foreign member of the channel — the ordinary bootstrap, exactly
    // as the honest-drain witness above.
    let chip = match alice_backend
        .resolve_address(&format!("bob@{f_authority}"))
        .await
    {
        ResolveResult::Resolved(addr @ TypedAddress::Fauna { .. }) => addr,
        other => panic!("expected Resolved(Fauna), got {other:?}"),
    };
    let thread = one_to_one_thread(ThreadId("t-xnest-dead-domain".into()), chip.clone());
    alice_backend
        .send(
            &thread,
            &ComposeState {
                body_draft: "who is this?".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("cross-nest send (bootstrap + relay + channel.send)");
    let channel_hex = alice_backend.bound_channels()[0].to_string();
    let channel_bytes: [u8; 32] = hex::decode(&channel_hex)
        .expect("channel hex")
        .try_into()
        .expect("32 bytes");

    let inbox = f_state.db.list_inbox_all(&bob_id.0).await.unwrap();
    assert_eq!(inbox.len(), 1, "exactly one welcome relayed to Bob on F");
    let welcome = welcome_bytes_from_inbox(&inbox[0].1);
    ingest_welcome(&bob_backend, &bob_manager, &channel_hex, &welcome, &h_base)
        .await
        .expect("bob joins the cross-nest group from the relayed Welcome");
    let bob_channel = bob_backend.bound_channels()[0];
    let mut after = 0i64;
    poll_inbound_conv(&bob_backend, &bob_manager, &bob_channel, &mut after, 0)
        .await
        .expect("bob drains the cross-nest channel via the federation relay");
    poll_foreign_member_handle(
        &h_state.db,
        &channel_bytes,
        &bob_id.0,
        Some(("bob", &f_authority)),
    )
    .await;

    // F dials H directly and crafts raw `channel.fetch` requests alternating
    // between two domains that never resolve: loopback authorities with
    // nothing listening, so the connect refuses fast and deterministically —
    // no real DNS, no dependence on outbound network access (the same
    // dead-loopback shape `federation_pool.rs`'s own
    // `a_peer_that_never_reads_is_evicted_on_originate_deadline_not_keepalive`
    // uses).
    let dead_a = "127.0.0.1:1";
    let dead_b = "127.0.0.1:2";
    let h_nest_hex = hex::encode(h_state.nest_identity.public_key_bytes());
    let conn = dial(&f_state, &h_base, &h_nest_hex)
        .await
        .expect("F dials H over the federation channel");
    let raw_fetch = |key: u8, handle: &str, domain: &str| {
        let conn = &conn;
        let req = FedChannelFetchRequest {
            requesting_actor_id: hex::encode(bob_id.0),
            channel_id: channel_hex.clone(),
            after: 0,
            limit: 0,
            requesting_handle: Some(handle.to_string()),
            requesting_domain: Some(domain.to_string()),
        };
        async move {
            let value = conn
                .dispatcher
                .request_raw(
                    "fauna.federation.channel.fetch",
                    [key; 16],
                    fed_value(&req),
                    None,
                )
                .await
                .expect("request accepted")
                .await_reply()
                .await
                .expect("channel.fetch serves whatever the announce says");
            let reply: FedChannelFetchReply =
                fauna_protocol::decode_strict(&fauna_protocol::encode_canonical(&value).unwrap())
                    .unwrap();
            reply.messages.len()
        }
    };

    let attempts_before = h_state.federation_pool.domain_resolve_attempt_count();

    // Alternate 6 times between the two dead domains — a hostile peer
    // re-asserting neither. Every fetch still serves its page.
    for i in 0..6u8 {
        let domain = if i % 2 == 0 { dead_a } else { dead_b };
        assert_eq!(
            raw_fetch(0x60 + i, "mallory", domain).await,
            1,
            "the drain serves its one record regardless of the announce"
        );
    }

    // Wait for every verification this loop spawned to finish — quiescence,
    // not a fixed sleep — before reading the attempt count.
    let quiesce_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while h_state.federation_pool.in_flight_verification_count() > 0 {
        assert!(
            tokio::time::Instant::now() < quiesce_deadline,
            "spawned verifications never quiesced"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let attempts = h_state.federation_pool.domain_resolve_attempt_count() - attempts_before;
    assert!(
        attempts <= 2,
        "expected at most 2 resolver attempts for 2 distinct domains across 6 \
         alternating fetches (found {attempts}) — the negative cache must be \
         keyed on the domain, not re-attempted on every distinct assertion"
    );

    // Neither dead domain is ever believed: Bob's own verified binding never
    // changes.
    assert_eq!(
        h_state
            .db
            .foreign_member_handle(&channel_bytes, &bob_id.0)
            .await
            .unwrap(),
        Some(("bob".to_string(), f_authority.clone())),
        "a domain that never resolves is never believed — bob@F stands"
    );

    let _ = (h_state, f_state);
}

/// **Off the fetch path, witnessed — not just argued from the code shape**: a domain
/// whose host accepts the TCP connection and never answers must not delay
/// the `channel.fetch` reply that triggered its verification. The two
/// existing "off the fetch path" tests can't catch a regression here — their
/// dead-loopback domains refuse the connect immediately, so an inline
/// `.await` and a spawned one are indistinguishable to them. Mutate it
/// here and the reply blocks on the hung connect for
/// `fauna-anon-client`'s 30 s deadline instead of returning immediately —
/// the outer timeout below turns that into a failing test rather than an
/// actual 30 s hang.
#[tokio::test]
async fn a_hanging_domain_never_delays_the_fetch_reply_that_triggered_it() {
    let (h_base, h_authority, h_state) = start_home_nest().await;
    let (f_base, f_authority, f_state) = start_foreign_nest().await;

    // Bob lives on F with a registered handle and one published key package.
    let mut bob_secret = [0u8; 32];
    getrandom::fill(&mut bob_secret).unwrap();
    let bob_id = ActorKeypair::from_secret(bob_secret).actor_id();
    f_state
        .db
        .create_user_with_handle(&bob_id.0, "free", "bob", None)
        .await
        .unwrap();
    let bob_engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(bob_secret)).expect("engine"));
    let bob_pkgs = bob_engine.generate_key_packages_bytes(1).expect("bob KP");
    f_state
        .db
        .put_key_package("bob-kp-0", &bob_id.0, &bob_pkgs[0], 0, FAR_FUTURE)
        .await
        .unwrap();

    // Alice lives on H.
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user_with_handle(&alice_id.0, "free", "alice", None)
        .await
        .unwrap();
    let alice_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).expect("engine"),
    );

    let alice_nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let alice_backend = Arc::new(FaunaMlsBackend::new(
        alice_engine.clone(),
        Arc::new(NestConversationsRpc::new(Arc::clone(&alice_nest))) as Arc<dyn ConversationsRpc>,
        format!("alice@{h_authority}"),
        alice_id,
    ));
    let bob_nest = connected_client(&f_base, ActorKeypair::from_secret(bob_secret)).await;
    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob_engine.clone(),
        Arc::new(NestConversationsRpc::new(bob_nest)) as Arc<dyn ConversationsRpc>,
        format!("bob@{f_authority}"),
        bob_id,
    ));
    bob_manager.register_backend(bob_backend.clone());

    // Alice resolves bob@F and sends: the Welcome relays to F and H records
    // Bob as a foreign member of the channel — the ordinary bootstrap.
    let chip = match alice_backend
        .resolve_address(&format!("bob@{f_authority}"))
        .await
    {
        ResolveResult::Resolved(addr @ TypedAddress::Fauna { .. }) => addr,
        other => panic!("expected Resolved(Fauna), got {other:?}"),
    };
    let thread = one_to_one_thread(ThreadId("t-xnest-hanging-domain".into()), chip.clone());
    alice_backend
        .send(
            &thread,
            &ComposeState {
                body_draft: "who is this?".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("cross-nest send (bootstrap + relay + channel.send)");
    let channel_hex = alice_backend.bound_channels()[0].to_string();
    let channel_bytes: [u8; 32] = hex::decode(&channel_hex)
        .expect("channel hex")
        .try_into()
        .expect("32 bytes");

    let inbox = f_state.db.list_inbox_all(&bob_id.0).await.unwrap();
    assert_eq!(inbox.len(), 1, "exactly one welcome relayed to Bob on F");
    let welcome = welcome_bytes_from_inbox(&inbox[0].1);
    ingest_welcome(&bob_backend, &bob_manager, &channel_hex, &welcome, &h_base)
        .await
        .expect("bob joins the cross-nest group from the relayed Welcome");
    let bob_channel = bob_backend.bound_channels()[0];
    let mut after = 0i64;
    poll_inbound_conv(&bob_backend, &bob_manager, &bob_channel, &mut after, 0)
        .await
        .expect("bob drains the cross-nest channel via the federation relay");
    poll_foreign_member_handle(
        &h_state.db,
        &channel_bytes,
        &bob_id.0,
        Some(("bob", &f_authority)),
    )
    .await;

    // A real listener that accepts the connect and then answers nothing —
    // the announced domain's own "peer": the TLS ClientHello H's discovery
    // client sends lands in this socket's buffer and nothing ever reads or
    // replies to it, so the handshake hangs until we release the accepted
    // stream (closing it) or the client's own 30 s connect deadline fires.
    let hang_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hang_domain = format!("127.0.0.1:{}", hang_listener.local_addr().unwrap().port());
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let accept_task = tokio::spawn(async move {
        let (stream, _) = hang_listener
            .accept()
            .await
            .expect("accept the hanging connection");
        let _ = release_rx.await;
        drop(stream); // release: the peer sees the connection close, not an answer
    });

    // F dials H directly and crafts one raw `channel.fetch` naming the
    // hanging domain — the same shape the alternating-domain test uses, but
    // this domain actually accepts instead of refusing.
    let h_nest_hex = hex::encode(h_state.nest_identity.public_key_bytes());
    let conn = dial(&f_state, &h_base, &h_nest_hex)
        .await
        .expect("F dials H over the federation channel");
    let req = FedChannelFetchRequest {
        requesting_actor_id: hex::encode(bob_id.0),
        channel_id: channel_hex.clone(),
        after: 0,
        limit: 0,
        requesting_handle: Some("mallory".to_string()),
        requesting_domain: Some(hang_domain),
    };

    // The bootstrap's own (successful) verification of `bob@F` may not have
    // fully unwound its `VerificationInFlight` guard the instant
    // `poll_foreign_member_handle` observed the DB write above — quiesce
    // before asserting a clean baseline (no fixed sleep, convention 14).
    let baseline_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while h_state.federation_pool.in_flight_verification_count() > 0 {
        assert!(
            tokio::time::Instant::now() < baseline_deadline,
            "the bootstrap's own verification never quiesced"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let reply = tokio::time::timeout(Duration::from_secs(10), async {
        let value = conn
            .dispatcher
            .request_raw(
                "fauna.federation.channel.fetch",
                [0x70; 16],
                fed_value(&req),
                None,
            )
            .await
            .expect("request accepted")
            .await_reply()
            .await
            .expect("channel.fetch serves whatever the announce says");
        let reply: FedChannelFetchReply =
            fauna_protocol::decode_strict(&fauna_protocol::encode_canonical(&value).unwrap())
                .unwrap();
        reply.messages.len()
    })
    .await
    .expect(
        "the fetch reply must return promptly — never wait on the hanging \
         domain's own 30 s connect deadline",
    );
    assert_eq!(
        reply, 1,
        "the drain serves its one record regardless of the announce"
    );

    // The reply is already back — read this on the SAME turn, before
    // yielding, so a regression that inlined the verification (making the
    // reply itself wait for it) cannot slip past by finishing before this
    // check runs.
    assert_eq!(
        h_state.federation_pool.in_flight_verification_count(),
        1,
        "the reply returned while the verification of the hanging domain is \
         still in flight — proof the resolve genuinely runs off the fetch \
         path, not merely that it is coded to look that way"
    );

    // Release the accepted connection so the hung handshake fails fast
    // (connection closed) instead of idling out the full connect deadline,
    // then wait for quiescence — no fixed sleep (convention 14).
    let _ = release_tx.send(());
    accept_task.await.expect("accept task did not panic");
    let quiesce_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while h_state.federation_pool.in_flight_verification_count() > 0 {
        assert!(
            tokio::time::Instant::now() < quiesce_deadline,
            "the verification of the released hanging domain never quiesced"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // The hanging domain was never believed: Bob's own verified binding
    // never changes.
    assert_eq!(
        h_state
            .db
            .foreign_member_handle(&channel_bytes, &bob_id.0)
            .await
            .unwrap(),
        Some(("bob".to_string(), f_authority.clone())),
        "a domain that never resolves is never believed — bob@F stands"
    );

    let _ = (h_state, f_state);
}

/// The one seat the id→handle ruling could not reach: the **foreign member's
/// own device** (`conversation-rooms.md` § The home nest; `federation.md`
/// § Federation residue surface, the `conversation.roster.fetch` row).
///
/// A room's floor roster lives on its home nest alone, so Bob — homed on F —
/// has no roster read at all until one is relayed for him: his own nest holds
/// no record of the room, answers "not a member of this room", and every
/// co-member he has not met renders as an elided actor id — the announce that
/// named him for everyone on H notwithstanding. This pins the relay end to
/// end, and with it the property the shared core exists for: **Bob's roster is
/// Alice's roster**, name for name, both joins included (Alice's from H's own
/// `users` row, Bob's from the handle F announced on his drain).
///
/// Discriminating by construction: the very same read with no home URL — the
/// same-nest kind, which is exactly what an old own-nest would fall back to if
/// this were an additive field rather than a distinct kind — answers `None` on
/// this same tree.
#[tokio::test]
async fn a_foreign_member_reads_the_rooms_floor_through_its_own_nest() {
    let (h_base, h_authority, h_state) = start_home_nest().await;
    let (f_base, f_authority, f_state) = start_foreign_nest().await;

    // Bob lives on F with a registered handle and one published key package.
    let mut bob_secret = [0u8; 32];
    getrandom::fill(&mut bob_secret).unwrap();
    let bob_id = ActorKeypair::from_secret(bob_secret).actor_id();
    f_state
        .db
        .create_user_with_handle(&bob_id.0, "free", "bob", None)
        .await
        .unwrap();
    let bob_engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(bob_secret)).expect("engine"));
    let bob_pkgs = bob_engine.generate_key_packages_bytes(1).expect("bob KP");
    f_state
        .db
        .put_key_package("bob-kp-0", &bob_id.0, &bob_pkgs[0], 0, FAR_FUTURE)
        .await
        .unwrap();

    // Alice lives on H, the room's home, with a handle of her own.
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user_with_handle(&alice_id.0, "free", "alice", None)
        .await
        .unwrap();
    let alice_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).expect("engine"),
    );

    let alice_nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let alice_backend = Arc::new(FaunaMlsBackend::new(
        alice_engine.clone(),
        Arc::new(NestConversationsRpc::new(Arc::clone(&alice_nest))) as Arc<dyn ConversationsRpc>,
        format!("alice@{h_authority}"),
        alice_id,
    ));
    let bob_nest = connected_client(&f_base, ActorKeypair::from_secret(bob_secret)).await;
    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob_engine.clone(),
        Arc::new(NestConversationsRpc::new(Arc::clone(&bob_nest))) as Arc<dyn ConversationsRpc>,
        format!("bob@{f_authority}"),
        bob_id,
    ));
    bob_manager.register_backend(bob_backend.clone());

    // Alice resolves bob@F and sends: the Welcome relays to F and H records
    // Bob as a foreign member of the channel.
    let chip = match alice_backend
        .resolve_address(&format!("bob@{f_authority}"))
        .await
    {
        ResolveResult::Resolved(addr @ TypedAddress::Fauna { .. }) => addr,
        other => panic!("expected Resolved(Fauna), got {other:?}"),
    };
    let thread = one_to_one_thread(ThreadId("t-xnest-roster".into()), chip.clone());
    alice_backend
        .send(
            &thread,
            &ComposeState {
                body_draft: "who else is here?".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("cross-nest send (bootstrap + relay + channel.send)");
    let channel_hex = alice_backend.bound_channels()[0].to_string();
    let channel_bytes: [u8; 32] = hex::decode(&channel_hex)
        .expect("channel hex")
        .try_into()
        .expect("32 bytes");

    // Bob joins from the relayed Welcome and drains once, which is what
    // carries F's announce of `bob@F` to H and confirms his binding there.
    let inbox = f_state.db.list_inbox_all(&bob_id.0).await.unwrap();
    assert_eq!(inbox.len(), 1, "exactly one welcome relayed to Bob on F");
    let welcome = welcome_bytes_from_inbox(&inbox[0].1);
    ingest_welcome(&bob_backend, &bob_manager, &channel_hex, &welcome, &h_base)
        .await
        .expect("bob joins the cross-nest group from the relayed Welcome");
    let bob_channel = bob_backend.bound_channels()[0];
    let mut after = 0i64;
    poll_inbound_conv(&bob_backend, &bob_manager, &bob_channel, &mut after, 0)
        .await
        .expect("bob drains the cross-nest channel via the federation relay");

    // The channel is foreign-homed on Bob's device, and THAT is the signal the
    // backend hands the roster seam — the same one that routes his
    // `channel.fetch` and `channel.actors` relays.
    assert_eq!(
        bob_backend.channel_home_url(&bob_channel).as_deref(),
        Some(h_base.as_str()),
        "bob's device records H as the channel's home, so the roster read is \
         routed by the same signal as every other cross-nest read"
    );

    // Seat both on H's floor. (A 1:1 thread is not a governed room, so no
    // device reports one; the report door is pinned elsewhere — what this
    // test pins is who can READ the result, and through which nest.)
    h_state
        .db
        .replace_floor_roster(
            &channel_bytes,
            "end_to_end",
            "",
            None,
            None,
            &[
                ReportedMember {
                    principal_id: alice_id.0,
                    principal_kind: "user".into(),
                    role: Some("owner".into()),
                    home_node_url: String::new(),
                    reception_pubkey: Vec::new(),
                },
                ReportedMember {
                    principal_id: bob_id.0,
                    principal_kind: "user".into(),
                    role: Some("member".into()),
                    home_node_url: f_base.clone(),
                    reception_pubkey: Vec::new(),
                },
            ],
        )
        .await
        .expect("seat alice and bob on the floor");

    let named = |floor: fauna_conversations::backend::RoomFloor| {
        let mut out: Vec<(String, Option<String>)> = floor
            .members
            .into_iter()
            .map(|m| (hex::encode(m.actor.0), m.qualified_handle()))
            .collect();
        out.sort();
        out
    };

    // Bob reads his own room's floor through F, which relays to H.
    let bob_reader = NestConversationsRpc::new(Arc::clone(&bob_nest));
    let bob_view = named(
        fauna_conversations::backend::RoomRosterReader::read_roster(
            &bob_reader,
            channel_hex.clone(),
            bob_backend.channel_home_url(&bob_channel),
        )
        .await
        .or_absent()
        .expect("a foreign member reads its own room's floor through its own nest's relay"),
    );

    // Alice reads the same floor the same-nest way, on H.
    let alice_reader = NestConversationsRpc::new(Arc::clone(&alice_nest));
    let alice_view = named(
        fauna_conversations::backend::RoomRosterReader::read_roster(
            &alice_reader,
            channel_hex.clone(),
            None,
        )
        .await
        .or_absent()
        .expect("a home member reads its own room's floor"),
    );

    assert_eq!(
        bob_view, alice_view,
        "the foreign member sees the same names the home member sees — one \
         shared core answers both doors, so the two replies cannot drift"
    );
    assert_eq!(
        bob_view,
        {
            let mut want = vec![
                (
                    hex::encode(alice_id.0),
                    Some(format!("alice@{h_authority}")),
                ),
                (hex::encode(bob_id.0), Some(format!("bob@{f_authority}"))),
            ];
            want.sort();
            want
        },
        "and both joins survive the relay: Alice from H's own users row, Bob \
         from the handle his home nest announced on his drain"
    );

    // Discriminator: the same read without the home URL is the same-nest kind,
    // which is what an old own-nest would silently fall back to if this were
    // an additive field. F holds no floor for a room homed on H.
    assert!(
        fauna_conversations::backend::RoomRosterReader::read_roster(
            &bob_reader,
            channel_hex.clone(),
            None,
        )
        .await
        .or_absent()
        .is_none(),
        "the same-nest read finds no room on F — the reason the relay is a \
         distinct kind and not an additive nest_url"
    );

    let _ = (h_state, f_state);
}

/// **A community room's verdicts reach a member homed elsewhere**. The room's home nest serves what the room's named
/// labelers derived "as metadata beside the message" to its live floor
/// members (`community-rooms.md` § The three classes → *What the home nest
/// does with its read*, purpose 2), and a member homed on another nest reaches
/// the room only through its own nest, which relays the drain
/// (`conversation-rooms.md` § The home nest). So the verdicts have to survive
/// the whole relay: H's federation reply, the federation channel's wire, F's
/// pass-through onto its member's `channel.fetch` reply, and the production
/// client seam — the `FetchedRecord.labels` the chat poll hands the manager
/// for the bubble (that last merge is pinned against a mock nest by
/// `a_community_messages_server_labels_merge_into_the_bubbles_own`).
///
/// The room and its verdict are seeded on H at the storage layer — exactly
/// the rows the label pass writes, which `conformance_conversation_rooms.rs`
/// pins end to end — because what this pins is the relay, not the labeller.
///
/// Discriminating by construction: Carol, homed on F too and bound to the
/// room's channel exactly as Bob is but never seated on its floor, drains the
/// same page through the same relay and receives the envelope with no
/// verdict. And F keeps none of what it forwarded.
#[tokio::test]
async fn a_foreign_members_relayed_read_carries_the_community_rooms_verdicts() {
    let (h_base, _h_authority, h_state) = start_home_nest().await;
    let (f_base, _f_authority, f_state) = start_foreign_nest().await;
    let f_nest_id = f_state.nest_identity.public_key_bytes();

    // Bob and Carol live on F; Alice, the room's owner, lives on H.
    let bob = ActorKeypair::generate();
    let carol = ActorKeypair::generate();
    for (who, handle) in [(&bob, "bob"), (&carol, "carol")] {
        f_state
            .db
            .create_user_with_handle(&who.actor_id().0, "free", handle, None)
            .await
            .unwrap();
    }
    let alice = ActorKeypair::generate();
    h_state
        .db
        .create_user_with_handle(&alice.actor_id().0, "free", "alice", None)
        .await
        .unwrap();

    // A community room homed on H — the founding rows `room.create` writes
    // (its floor carries the home nest as a reader principal, which is what
    // makes the class `community`) — with Bob seated on its floor.
    let mut room_id = [0u8; 32];
    getrandom::fill(&mut room_id).unwrap();
    let room_hex = hex::encode(room_id);
    let seat = |principal_id: [u8; 32], kind: &str, role: &str, home: &str| ReportedMember {
        principal_id,
        principal_kind: kind.into(),
        role: Some(role.into()),
        home_node_url: home.into(),
        reception_pubkey: Vec::new(),
    };
    let founded = h_state
        .db
        .found_room(
            &room_id,
            "community",
            &alice.actor_id().0,
            1,
            b"the founding policy",
            &[0x86u8; 32],
            &[
                seat(alice.actor_id().0, "user", "owner", ""),
                seat(bob.actor_id().0, "user", "member", &f_base),
                seat(
                    h_state.nest_identity.public_key_bytes(),
                    "nest",
                    "member",
                    "",
                ),
            ],
        )
        .await
        .expect("found the room");
    assert!(founded);

    // Bob and Carol are both bound to the room's channel as F's members —
    // the row H writes when it relays each one's Welcome, and what
    // `require_foreign_member` admits the drain on.
    for who in [&bob, &carol] {
        h_state
            .db
            .register_foreign_channel_member(
                &room_id,
                &who.actor_id().0,
                &f_nest_id,
                None,
                RebindPower::InsertOnly,
            )
            .await
            .expect("bind the foreign member");
    }

    // One sealed message in the room's log, and what the room's labelers
    // derived from it when H opened it to index it.
    let envelope = b"a community message, sealed under the room's generation".to_vec();
    let appended = fauna_nest::segments::conv::append(
        &h_state.conv_segments,
        &h_state.db,
        &room_id,
        &envelope,
        1_757_000_000_000,
    )
    .await
    .expect("append the message to the room's log");
    let labeler = [0x4Cu8; 32];
    let verdict_score = fauna_core::scoring::ScoreEntry {
        factor: fauna_core::scoring::labeler_factor(&fauna_core::identity::ActorId(labeler)),
        score: 900,
        tier: fauna_core::scoring::TIER_COMMUNITY,
        scorer_version: 1,
    };
    h_state
        .db
        .record_room_message_bus(
            &room_id,
            appended.seq,
            &[fauna_nest::db::room_labels::RoomLabel {
                category: "spam".into(),
                confidence: 0.9,
                labeler_id: labeler,
                labeler_version: 1,
            }],
            std::slice::from_ref(&verdict_score),
            &h_state.nest_identity.public_key_bytes(),
        )
        .await
        .expect("record the message's verdicts");
    let spam = fauna_core::content_category::ContentLabelEntry {
        category: "spam".into(),
        confidence_per_mille: 900,
    };

    // Bob drains through his own nest, F, which relays to H — the production
    // client seam his chat poll reads.
    let bob_nest = connected_client(&f_base, ActorKeypair::from_secret(*bob.secret_bytes())).await;
    let bob_rpc = NestConversationsRpc::new(Arc::clone(&bob_nest));
    let records = bob_rpc
        .channel_fetch(room_hex.clone(), 0, 0, Some(h_base.clone()))
        .await
        .expect("a foreign member drains its room through its own nest");
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].envelope, envelope,
        "the sealed envelope, verbatim"
    );
    assert_eq!(
        records[0].labels,
        vec![spam.clone()],
        "the room's verdict crosses both hops into the seam the bubble is \
         painted from"
    );

    // The factor plane rides the same relay (the seam's record carries only
    // the category plane, so read F's own reply for it).
    let raw: fauna_protocol::conversations::ChannelFetchReply = bob_nest
        .request(
            "fauna.conversations.channel.fetch",
            fauna_protocol::conversations::ChannelFetchRequest {
                channel_id: room_hex.clone(),
                after: 0,
                limit: 0,
                nest_url: Some(h_base.clone()),
                extra: Default::default(),
            },
        )
        .await
        .expect("F's own channel.fetch reply");
    assert_eq!(raw.messages[0].labels, vec![spam]);
    assert_eq!(raw.messages[0].scores, vec![verdict_score]);

    // Carol holds the same binding and no seat: the envelope, and no verdict.
    let carol_nest =
        connected_client(&f_base, ActorKeypair::from_secret(*carol.secret_bytes())).await;
    let carol_records = NestConversationsRpc::new(carol_nest)
        .channel_fetch(room_hex.clone(), 0, 0, Some(h_base.clone()))
        .await
        .expect("a bound actor drains the sealed log through its own nest");
    assert_eq!(carol_records.len(), 1, "the sealed envelope is served");
    assert_eq!(carol_records[0].envelope, envelope);
    assert!(
        carol_records[0].labels.is_empty(),
        "a binding with no seat on the floor reads no verdict, through the \
         relay as at home"
    );

    // F forwarded the verdicts and kept none: it holds no room record and no
    // verdict row for a room homed on H.
    assert!(
        f_state.db.get_room(&room_id).await.unwrap().is_none(),
        "the relaying nest holds no room record"
    );
    assert!(
        f_state
            .db
            .room_message_bus(&room_id, &[appended.seq])
            .await
            .unwrap()
            .is_empty(),
        "and stores none of the verdicts it relayed"
    );

    let _ = (h_state, f_state);
}
/// **A room post's verdicts ride their own relay, exactly as a room message's
/// do**.
///
/// The test above pins the *message* half: a community room's verdicts reach a
/// member homed elsewhere because `channel.fetch` has a federation twin and the
/// verdicts ride its page. A room **post**'s verdicts leave by a door of their
/// own — `fauna.posts.room_labels`, post-scoped because a post is its author's
/// ordinary post and reaches every follower through reads that carry no verdict
/// (`ui/feed.md` § Encryption at rest → *Room-restricted — the ruling*, *Built*
/// detail (v)) — and that door had no twin. A foreign member asked its **own**
/// nest, which holds no `room_post_views` row for a room it does not home
/// (`room_post_view::index_room_post` runs on the storing home alone), so the
/// answer came back empty and the card rendered with no verdicts: silently,
/// since an empty reply is how "nobody labelled" and "not yours to read" are
/// deliberately made indistinguishable.
///
/// So the read relays like its three siblings — `channel.fetch`,
/// `generations.fetch`, `roster.fetch` — through the distinct client kind
/// `fauna.posts.room_labels_remote` to the home's
/// `fauna.federation.conversation.room_labels.fetch`.
///
/// **The gate is the binding AND the seat**, and that is not a new decision:
/// `conversations_handlers::page_verdicts` is already "the one home for both
/// rules, shared by the two doors a page is served through" — the same-nest
/// `channel.fetch` and the room home's federated twin — so the message verdict
/// read across this very relay already stacks `is_live_floor_member` on
/// `require_foreign_member`. `roster.fetch`'s ratified refusal to stack a seat
/// check cannot apply here: it feared denying the newest-seated member off a
/// member-*reported* mirror, and `is_live_floor_member` refuses any room that is
/// not floor-authoritative outright, so the only rooms it passes are ones whose
/// floor this nest writes itself.
///
/// Discriminating by construction, the shape the test above uses: Carol holds
/// the same binding as Bob and no seat on the floor. Bob's relayed read carries
/// both verdict planes; Carol's carries nothing.
#[tokio::test]
async fn a_foreign_members_room_post_verdicts_ride_their_own_relay() {
    let (h_base, _h_authority, h_state) = start_home_nest().await;
    let (f_base, _f_authority, f_state) = start_foreign_nest().await;
    let f_nest_id = f_state.nest_identity.public_key_bytes();

    // Bob and Carol live on F; Alice, the room's owner, lives on H.
    let bob = ActorKeypair::generate();
    let carol = ActorKeypair::generate();
    for (who, handle) in [(&bob, "bob"), (&carol, "carol")] {
        f_state
            .db
            .create_user_with_handle(&who.actor_id().0, "free", handle, None)
            .await
            .unwrap();
    }
    let alice = ActorKeypair::generate();
    h_state
        .db
        .create_user_with_handle(&alice.actor_id().0, "free", "alice", None)
        .await
        .unwrap();

    // A community room homed on H, Bob seated on its floor and Carol not —
    // the founding rows `room.create` writes (the home nest sits on the floor
    // as a reader principal, which is what makes the class `community`).
    let mut room_id = [0u8; 32];
    getrandom::fill(&mut room_id).unwrap();
    let room_hex = hex::encode(room_id);
    let seat = |principal_id: [u8; 32], kind: &str, role: &str, home: &str| ReportedMember {
        principal_id,
        principal_kind: kind.into(),
        role: Some(role.into()),
        home_node_url: home.into(),
        reception_pubkey: Vec::new(),
    };
    let founded = h_state
        .db
        .found_room(
            &room_id,
            "community",
            &alice.actor_id().0,
            1,
            b"the founding policy",
            &[0x87u8; 32],
            &[
                seat(alice.actor_id().0, "user", "owner", ""),
                seat(bob.actor_id().0, "user", "member", &f_base),
                seat(
                    h_state.nest_identity.public_key_bytes(),
                    "nest",
                    "member",
                    "",
                ),
            ],
        )
        .await
        .expect("found the room");
    assert!(founded);

    // Both are bound to the room's channel as F's members — the row H writes
    // when it relays each one's Welcome, and what `require_foreign_member`
    // admits the relayed read on.
    for who in [&bob, &carol] {
        h_state
            .db
            .register_foreign_channel_member(
                &room_id,
                &who.actor_id().0,
                &f_nest_id,
                None,
                RebindPower::InsertOnly,
            )
            .await
            .expect("bind the foreign member");
    }

    // One room post of Alice's, as H's reception pass leaves it: the
    // post → room map row it writes when it indexes the post it stores, and
    // what the room's named labelers derived from the plaintext it opened.
    let mut post_id = [0u8; 32];
    getrandom::fill(&mut post_id).unwrap();
    h_state
        .db
        .record_room_post_view(
            &room_id,
            &post_id,
            &[0x5Au8; 32],
            // The generation the reception pass indexed the post under. It
            // bounds the SEARCH door's hits (schema 63); the verdict read this
            // test exercises resolves the post's room through the map row and
            // never joins on it, so any fixture generation serves here.
            &[0x6Bu8; 32],
        )
        .await
        .expect("index the room post");
    let labeler = [0x4Du8; 32];
    let verdict_score = fauna_core::scoring::ScoreEntry {
        factor: fauna_core::scoring::labeler_factor(&fauna_core::identity::ActorId(labeler)),
        score: 880,
        tier: fauna_core::scoring::TIER_COMMUNITY,
        scorer_version: 1,
    };
    h_state
        .db
        .record_room_post_bus(
            &room_id,
            &post_id,
            &[fauna_nest::db::room_labels::RoomLabel {
                category: "spam".into(),
                confidence: 0.88,
                labeler_id: labeler,
                labeler_version: 1,
            }],
            std::slice::from_ref(&verdict_score),
            &h_state.nest_identity.public_key_bytes(),
        )
        .await
        .expect("record the post's verdicts");
    let spam = fauna_core::content_category::ContentLabelEntry {
        category: "spam".into(),
        confidence_per_mille: 880,
    };
    let post_hex = hex::encode(post_id);

    // Bob's own nest holds no map row for a room it does not home, so the
    // same-nest door answers him nothing — the gap this relay closes, pinned
    // so a future session cannot mistake the relay for a redundant path.
    let bob_nest = connected_client(&f_base, ActorKeypair::from_secret(*bob.secret_bytes())).await;
    let local: fauna_protocol::posts::PostRoomLabelsReply = bob_nest
        .request(
            "fauna.posts.room_labels",
            fauna_protocol::posts::PostRoomLabelsRequest {
                post_ids: vec![post_hex.clone()],
                extra: Default::default(),
            },
        )
        .await
        .expect("the same-nest verdict read answers any caller");
    assert!(
        local.posts.is_empty(),
        "a member's own nest indexes no post of a room it does not home, so \
         the same-nest read is empty — not a verdict, and not a refusal"
    );

    // Relayed, it carries both planes.
    let relayed: fauna_protocol::posts::PostRoomLabelsReply = bob_nest
        .request(
            "fauna.posts.room_labels_remote",
            fauna_protocol::posts::PostRoomLabelsRemoteRequest {
                room_id: room_hex.clone(),
                post_ids: vec![post_hex.clone()],
                nest_url: h_base.clone(),
                extra: Default::default(),
            },
        )
        .await
        .expect("a foreign member reads its room post's verdicts through its own nest");
    assert_eq!(relayed.posts.len(), 1, "{relayed:?}");
    assert_eq!(relayed.posts[0].post_id, post_hex);
    assert_eq!(
        relayed.posts[0].labels,
        vec![spam],
        "the room's verdict crosses both hops into the plane the badge is \
         painted from"
    );
    assert_eq!(
        relayed.posts[0].scores,
        vec![verdict_score],
        "and the factor plane rides with it, as on the message read"
    );

    // Carol holds the same binding and no seat: no verdict, through the relay
    // as at home. The binding says which nest may ask for whom; the floor says
    // what they may read.
    let carol_nest =
        connected_client(&f_base, ActorKeypair::from_secret(*carol.secret_bytes())).await;
    let carol_read: fauna_protocol::posts::PostRoomLabelsReply = carol_nest
        .request(
            "fauna.posts.room_labels_remote",
            fauna_protocol::posts::PostRoomLabelsRemoteRequest {
                room_id: room_hex.clone(),
                post_ids: vec![post_hex.clone()],
                nest_url: h_base.clone(),
                extra: Default::default(),
            },
        )
        .await
        .expect("a bound actor's relayed read is answered, not refused");
    assert!(
        carol_read.posts.is_empty(),
        "a binding with no seat on the floor reads no verdict, through the \
         relay as at home"
    );

    // F forwarded the verdicts and kept none: it holds no room record, no map
    // row and no verdict row for a room homed on H.
    assert!(
        f_state.db.get_room(&room_id).await.unwrap().is_none(),
        "the relaying nest holds no room record"
    );
    assert!(
        f_state
            .db
            .rooms_indexing_posts(&[post_id])
            .await
            .unwrap()
            .is_empty(),
        "and indexes none of the posts it relayed verdicts for"
    );
    assert!(
        f_state
            .db
            .room_post_bus(&room_id, &[post_id])
            .await
            .unwrap()
            .is_empty(),
        "and stores none of the verdicts it relayed"
    );

    let _ = (h_state, f_state);
}

/// The relayed roster read exactly as a foreign member's own device makes it:
/// F relays `room.list_roster_remote` to H's `conversation.roster.fetch`, and
/// the seam degrades **any** refusal to `None` — "this member stays elided"
/// ([`fauna_conversations::backend::RoomRosterReader`]). Answers the floor's
/// principals, sorted, so two reads compare.
async fn roster_through_own_nest(
    f_base: &str,
    h_base: &str,
    room_hex: &str,
    who: &ActorKeypair,
) -> Option<Vec<String>> {
    let nest = connected_client(f_base, ActorKeypair::from_secret(*who.secret_bytes())).await;
    fauna_conversations::backend::RoomRosterReader::read_roster(
        &NestConversationsRpc::new(nest),
        room_hex.to_string(),
        Some(h_base.to_string()),
    )
    .await
    .or_absent()
    .map(|floor| {
        let mut out: Vec<String> = floor
            .members
            .iter()
            .map(|m| hex::encode(m.actor.0))
            .collect();
        out.sort();
        out
    })
}

/// **A removal ends the ADMISSION, not just the seat**.
///
/// The relayed roster read's gate is the `channel_foreign_members` binding
/// *alone*, and that is ratified, not an oversight (`federation.md`
/// § Federation residue surface, the `conversation.roster.fetch` row): stacking
/// `is_room_member` on top would deny the read to the newest-seated member,
/// whose co-members are still nameless, and "nothing is disclosed by that
/// choice: an **admitted** channel member already reads every member's actor id
/// off the MLS ratchet tree". The load-bearing word is *admitted* — so a
/// removal has to end the admission, where the folder plane already ends it:
/// S8, "the fetch authorization must die with the membership"
/// (`members.evict` purges the foreign row).
///
/// Before the purge landed, `room.remove` unseated the target from the floor
/// and left the binding standing, so a removed foreign member's home nest kept
/// passing `require_foreign_member` and kept being served the room's **live**
/// floor — every later join's `handle@domain`, roles, the signed policy, the
/// labeler set and each member's reception key — precisely the read the
/// same-nest twin refuses with "membership is not public". The write-token mint
/// rides the same gate and stayed open with it.
///
/// The room is seeded on H at the storage layer and both foreign members bound
/// directly, the way the community-room test above does it: a cross-nest
/// community *invite* is unbuilt (`community-rooms.md` § Implementation status
/// today — the invite handler admits a same-nest invitee only), so the Welcome
/// relay that writes the binding in production has no community leg to ride
/// yet. That is also what makes this hole **latent** rather than reachable
/// today, and why it is graded MEDIUM: the doors are wrong, the path to them
/// is not built.
///
/// Discriminating by construction: Carol is bound exactly as Bob is, seated
/// exactly as Bob is, and never removed. Every door that closes for Bob stays
/// open for her — so what the removal purges is one member's admission, not the
/// room's relay. And the refusal is pinned at the wire (`fauna.federation.
/// forbidden` off H's own gate) as well as at the client seam (`None`), because
/// the seam's degradation alone cannot tell a refusal from a broken transport.
#[tokio::test]
async fn a_removed_foreign_members_relayed_roster_read_and_mint_die_with_the_binding() {
    let (h_base, _h_authority, h_state) = start_home_nest().await;
    let (f_base, _f_authority, f_state) = start_foreign_nest().await;
    let f_nest_id = f_state.nest_identity.public_key_bytes();

    // Bob and Carol live on F; Alice, the room's owner, lives on H.
    let bob = ActorKeypair::generate();
    let carol = ActorKeypair::generate();
    for (who, handle) in [(&bob, "bob"), (&carol, "carol")] {
        f_state
            .db
            .create_user_with_handle(&who.actor_id().0, "free", handle, None)
            .await
            .unwrap();
    }
    let alice = ActorKeypair::generate();
    h_state
        .db
        .create_user_with_handle(&alice.actor_id().0, "free", "alice", None)
        .await
        .unwrap();

    // A community room homed on H — the founding rows `room.create` writes, its
    // floor carrying the home nest as a reader principal (which is what makes
    // the class `community`, and so floor-authoritative) — with both foreign
    // members seated.
    let mut room_id = [0u8; 32];
    getrandom::fill(&mut room_id).unwrap();
    let room_hex = hex::encode(room_id);
    let seat = |principal_id: [u8; 32], kind: &str, role: &str, home: &str| ReportedMember {
        principal_id,
        principal_kind: kind.into(),
        role: Some(role.into()),
        home_node_url: home.into(),
        reception_pubkey: Vec::new(),
    };
    let founded = h_state
        .db
        .found_room(
            &room_id,
            "community",
            &alice.actor_id().0,
            1,
            b"the founding policy",
            &[0x86u8; 32],
            &[
                seat(alice.actor_id().0, "user", "owner", ""),
                seat(bob.actor_id().0, "user", "member", &f_base),
                seat(carol.actor_id().0, "user", "member", &f_base),
                seat(
                    h_state.nest_identity.public_key_bytes(),
                    "nest",
                    "member",
                    "",
                ),
            ],
        )
        .await
        .expect("found the room");
    assert!(founded);

    // Both are bound to the room's channel as F's members — the row H writes
    // when it relays a Welcome, and what `require_foreign_member` admits on.
    for who in [&bob, &carol] {
        h_state
            .db
            .register_foreign_channel_member(
                &room_id,
                &who.actor_id().0,
                &f_nest_id,
                None,
                RebindPower::InsertOnly,
            )
            .await
            .expect("bind the foreign member");
    }

    let bob_hex = hex::encode(bob.actor_id().0);
    let carol_hex = hex::encode(carol.actor_id().0);
    // ── Before: an admitted member reads the floor and mints. ──
    let bob_before = roster_through_own_nest(&f_base, &h_base, &room_hex, &bob)
        .await
        .expect("an admitted foreign member reads the room's floor through its own nest");
    assert_eq!(
        bob_before.len(),
        4,
        "the live floor: Alice, Bob, Carol and the home nest's reader seat"
    );
    assert!(
        bob_before.contains(&bob_hex) && bob_before.contains(&carol_hex),
        "and it names the co-members by id, which is what the handles ride"
    );
    fauna_nest::federation_pool::originate_conversation_write_token_mint(
        &f_state.federation_pool,
        &f_state,
        &h_base,
        &bob_hex,
        &room_hex,
    )
    .await
    .expect("the federation channel itself is healthy")
    .expect("an admitted foreign member mints an attachment write token");

    // ── The removal, through the owner's own client stack. ──
    // (`room.remove` only unseats; the client's rotation that follows is the
    // severance for CIPHERTEXT and is pinned in the rooms suite. What this test
    // is about is the plaintext door, which no rotation covers.)
    let alice_nest =
        connected_client(&h_base, ActorKeypair::from_secret(*alice.secret_bytes())).await;
    NestConversationsRpc::new(alice_nest)
        .room_remove(room_hex.clone(), bob_hex.clone())
        .await
        .expect("the owner removes the foreign member");

    // ── After: the binding died with the seat, so every door on it is shut. ──
    let refused = fauna_nest::federation_pool::originate_room_roster(
        &f_state.federation_pool,
        &f_state,
        &h_base,
        &bob_hex,
        &room_hex,
        None,
    )
    .await
    .expect("the federation channel itself is healthy")
    .expect_err("a removed member's relayed roster read is refused by the room's home");
    assert_eq!(
        refused.code, "fauna.federation.forbidden",
        "refused by the same structural gate that admitted it — the binding is gone"
    );
    assert!(
        roster_through_own_nest(&f_base, &h_base, &room_hex, &bob)
            .await
            .is_none(),
        "and at the client seam the refusal degrades to `None`: the removed \
         member's device renders the room's members elided, as it did before it \
         was ever admitted"
    );
    let mint_refused = fauna_nest::federation_pool::originate_conversation_write_token_mint(
        &f_state.federation_pool,
        &f_state,
        &h_base,
        &bob_hex,
        &room_hex,
    )
    .await
    .expect("the federation channel itself is healthy")
    .expect_err("a removed member's write-token mint is refused too");
    assert_eq!(
        mint_refused.code, "fauna.federation.forbidden",
        "the mint rides the `channel.fetch` gate verbatim, so it dies with it"
    );

    // ── Carol, bound and seated exactly as Bob was, is untouched. ──
    let carol_after = roster_through_own_nest(&f_base, &h_base, &room_hex, &carol)
        .await
        .expect("a member who was NOT removed still reads the floor");
    assert!(
        !carol_after.contains(&bob_hex),
        "the floor she reads has lost Bob — the unseat landed"
    );
    assert!(
        carol_after.contains(&carol_hex),
        "and still names her, so the purge was scoped to the removed member"
    );
    fauna_nest::federation_pool::originate_conversation_write_token_mint(
        &f_state.federation_pool,
        &f_state,
        &h_base,
        &carol_hex,
        &room_hex,
    )
    .await
    .expect("the federation channel itself is healthy")
    .expect("her mint is untouched: the removal ended one admission, not the relay");

    // ── And re-admission still admits: the purge left no row to collide with. ──
    // The Welcome relay's insert arm is what writes a binding in production and
    // holds only `InsertOnly` power — it may write a FIRST grant and never move
    // an existing one. So the purge is what makes a re-invite possible at all:
    // the stale row the removal used to leave behind was a *confirmed* grant
    // (Bob's own reads above confirmed it), which `InsertOnly` cannot move, so a
    // member re-admitted from a different home nest would have been refused by
    // its own ghost. Here the row is gone, the insert lands, and the door the
    // removal shut opens again for a member the room chose to take back.
    h_state
        .db
        .register_foreign_channel_member(
            &room_id,
            &bob.actor_id().0,
            &f_nest_id,
            None,
            RebindPower::InsertOnly,
        )
        .await
        .expect("the Welcome relay's insert arm rebinds a re-invited member");
    let bob_readmitted = roster_through_own_nest(&f_base, &h_base, &room_hex, &bob)
        .await
        .expect(
            "a re-admitted member reads the floor again — the removal revoked an              admission, it did not blacklist an actor",
        );
    assert!(
        !bob_readmitted.contains(&bob_hex),
        "though the floor it reads still lacks his seat: the binding is          admission to the RELAY, and the seat is a separate act the re-invite          performs on the floor"
    );

    let _ = (h_state, f_state);
}

/// The three relayed room doors that read the `channel_foreign_members` row
/// **alone** — the roster read, `channel.actors` and the attachment write-token
/// mint (`federation.md` § Federation residue surface, the *room roster read*
/// row: "this gate closes with the membership, and the doors beside it that
/// read the same row alone — the write-token mint and `channel.actors` — close
/// with it"). Each entry is `Ok(())` for served or `Err(<wire code>)` for a
/// refusal, so one call pins all three and names whichever drifted.
///
/// `channel.fetch` is deliberately absent: its ciphertext residue on an
/// end-to-end room is ratified (the removing commit re-keys the MLS group, so
/// what a removed member may still fetch it cannot open), and these three are
/// the PLAINTEXT doors.
async fn three_binding_only_doors(
    f_state: &Arc<AppState>,
    h_base: &str,
    who_hex: &str,
    room_hex: &str,
) -> Vec<(&'static str, Result<(), String>)> {
    let pool = &f_state.federation_pool;
    let healthy = "the federation channel itself is healthy";
    let roster = fauna_nest::federation_pool::originate_room_roster(
        pool, f_state, h_base, who_hex, room_hex, None,
    )
    .await
    .expect(healthy);
    let actors = fauna_nest::federation_pool::originate_channel_actors(
        pool, f_state, h_base, who_hex, room_hex,
    )
    .await
    .expect(healthy);
    let mint = fauna_nest::federation_pool::originate_conversation_write_token_mint(
        pool, f_state, h_base, who_hex, room_hex,
    )
    .await
    .expect(healthy);
    vec![
        (
            "conversation.roster.fetch",
            roster.map(|_| ()).map_err(|e| e.code),
        ),
        ("channel.actors", actors.map(|_| ()).map_err(|e| e.code)),
        (
            "conversation.write_token.mint",
            mint.map(|_| ()).map_err(|e| e.code),
        ),
    ]
}

/// **An end-to-end room's removal closes the same three doors**.
///
/// The test above is the **community** class: `room.remove` purges the binding
/// and every door on it shuts. That door is community-only by construction —
/// `floor_authoritative_room` refuses a room whose membership authority is its
/// MLS group — and an **end-to-end** room removes a member the other way: a
/// membership commit, plus the committing device's roster report, whose absorb
/// stamps `removed_at` on every live row the new roster did not name
/// (`db::rooms::replace_floor_roster`) and purges no binding at all. So
/// `federation.md`'s "**admitted** is a live fact, not a historical one" was
/// true of one class and false of the other — and the false half is the one
/// that has been **built and reachable** since the 2026-07-29 chat slice (the
/// community half was latent, a cross-nest community invite being unbuilt),
/// where a removed foreign member's home nest kept passing
/// `require_foreign_member` and kept being served the room's LIVE floor: every
/// later join's `handle@domain`, roles, the signed policy, the labeler set and
/// each member's reception key.
///
/// **What closes it here is a floor READ, not the community class's purge**,
/// and that is `refuse_removed_room_member`'s whole reason for existing rather
/// than a `remove_foreign_channel_member` inside the absorb. The report ratchet
/// deliberately admits a departing member's final report, so one live member's
/// false report naming only itself would sever EVERY co-member's binding, and
/// the Welcome relay's `InsertOnly` power means no honest report could write
/// one back. The last arm below is that difference, asserted: the binding
/// survives the removal, and a later honest report returns the seat and the
/// reach together.
///
/// Discriminating by construction: Bob and Carol are both foreign members on F,
/// bound by the same founding send's relayed Welcomes, seated on the same
/// floor, and only Carol is removed. Every door that shuts for her stays open
/// for him — so what the removal ends is one member's admission, not the room's
/// relay. And Dave, bound by the relay's insert arm and never seated, is served
/// by all three: absence from the floor is not a removal, and he is the
/// newest-seated member the binding-only gate is ratified to protect. The
/// refusal is pinned at the wire (`fauna.federation.forbidden`, off
/// H's own gate) as well as at the client seam (`None`), because the seam
/// degrades **any** refusal and so cannot alone tell one from a broken
/// transport.
#[tokio::test]
async fn a_removed_foreign_member_of_an_end_to_end_room_loses_the_three_binding_only_doors() {
    let (h_base, h_authority, h_state) = start_home_nest().await;
    let (f_base, f_authority, f_state) = start_foreign_nest().await;

    // Bob and Carol both live on F, each with a registered handle and one
    // published key package. Two foreign members of one room is what makes the
    // unremoved one a discriminator rather than a different test.
    let mut bob_secret = [0u8; 32];
    getrandom::fill(&mut bob_secret).unwrap();
    let bob_id = ActorKeypair::from_secret(bob_secret).actor_id();
    let mut carol_secret = [0u8; 32];
    getrandom::fill(&mut carol_secret).unwrap();
    let carol_id = ActorKeypair::from_secret(carol_secret).actor_id();
    let bob_engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(bob_secret)).expect("engine"));
    let carol_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(carol_secret)).expect("engine"),
    );
    for (id, handle, engine) in [
        (bob_id, "bob", &bob_engine),
        (carol_id, "carol", &carol_engine),
    ] {
        f_state
            .db
            .create_user_with_handle(&id.0, "free", handle, None)
            .await
            .unwrap();
        let pkgs = engine.generate_key_packages_bytes(1).expect("key package");
        f_state
            .db
            .put_key_package(&format!("{handle}-kp-0"), &id.0, &pkgs[0], 0, FAR_FUTURE)
            .await
            .unwrap();
    }

    // Alice lives on H, the room's home, and owns it.
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user_with_handle(&alice_id.0, "free", "alice", None)
        .await
        .unwrap();
    let alice_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).expect("engine"),
    );

    // Every backend carries the report seam on the same glue object the apps'
    // session builders wire — the object that drains is the object that reports.
    let alice_nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let alice_rpc = Arc::new(NestConversationsRpc::new(Arc::clone(&alice_nest)));
    let alice_backend = Arc::new(FaunaMlsBackend::new(
        alice_engine.clone(),
        alice_rpc.clone() as Arc<dyn ConversationsRpc>,
        format!("alice@{h_authority}"),
        alice_id,
    ));
    alice_backend.set_room_roster_reporter(alice_rpc.clone());

    let bob_nest = connected_client(&f_base, ActorKeypair::from_secret(bob_secret)).await;
    let bob_rpc = Arc::new(NestConversationsRpc::new(Arc::clone(&bob_nest)));
    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob_engine.clone(),
        bob_rpc.clone() as Arc<dyn ConversationsRpc>,
        format!("bob@{f_authority}"),
        bob_id,
    ));
    bob_backend.set_room_roster_reporter(bob_rpc.clone());
    bob_manager.register_backend(bob_backend.clone());

    let carol_nest = connected_client(&f_base, ActorKeypair::from_secret(carol_secret)).await;
    let carol_rpc = Arc::new(NestConversationsRpc::new(Arc::clone(&carol_nest)));
    let carol_manager = ConversationsManager::new();
    let carol_backend = Arc::new(FaunaMlsBackend::new(
        carol_engine.clone(),
        carol_rpc.clone() as Arc<dyn ConversationsRpc>,
        format!("carol@{f_authority}"),
        carol_id,
    ));
    carol_backend.set_room_roster_reporter(carol_rpc.clone());
    carol_manager.register_backend(carol_backend.clone());

    // Alice founds the three-seat room by sending: the Welcomes relay to F for
    // both foreign members (H records each as a foreign member of the channel,
    // which is the binding under test) and the room is born governed, so a
    // removal is a ranked act and owes a report.
    let resolve = |handle: String| {
        let backend = alice_backend.clone();
        async move {
            match backend.resolve_address(&handle).await {
                ResolveResult::Resolved(addr @ TypedAddress::Fauna { .. }) => addr,
                other => panic!("expected Resolved(Fauna) for {handle}, got {other:?}"),
            }
        }
    };
    let bob_chip = resolve(format!("bob@{f_authority}")).await;
    let carol_chip = resolve(format!("carol@{f_authority}")).await;
    let thread = group_thread(
        ThreadId("t-xnest-e2e-removal".into()),
        vec![bob_chip, carol_chip.clone()],
    );
    alice_backend
        .send(
            &thread,
            &ComposeState {
                body_draft: "three of us".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("cross-nest group send (bootstrap + two relays + channel.send)");
    let channel = alice_backend.bound_channels()[0];
    let channel_hex = channel.to_string();
    let channel_bytes: [u8; 32] = hex::decode(&channel_hex)
        .expect("channel hex")
        .try_into()
        .expect("32 bytes");
    assert!(
        matches!(alice_engine.room_policy(&channel), Some(Ok(_))),
        "a three-seat group is born governed — the case a removal owes a report for"
    );
    assert_eq!(
        h_state
            .db
            .get_room(&channel_bytes)
            .await
            .unwrap()
            .expect("the creating device's birth report bootstrapped H's floor")
            .class,
        "end_to_end",
        "fixture: the class whose removal ceremony is a report, not `room.remove`"
    );

    // Both foreign members join from their relayed Welcome and drain once,
    // which CONFIRMS each binding on H — the binding every door below reads.
    for (id, backend, manager) in [
        (bob_id, &bob_backend, &bob_manager),
        (carol_id, &carol_backend, &carol_manager),
    ] {
        let inbox = f_state.db.list_inbox_all(&id.0).await.unwrap();
        assert_eq!(
            inbox.len(),
            1,
            "exactly one welcome relayed to each foreign member on F"
        );
        let welcome = welcome_bytes_from_inbox(&inbox[0].1);
        ingest_welcome(backend, manager, &channel_hex, &welcome, &h_base)
            .await
            .expect("the foreign member joins the cross-nest group from the relayed Welcome");
        let mut after = 0i64;
        poll_inbound_conv(backend, manager, &channel, &mut after, 0)
            .await
            .expect("and drains the channel through the federation relay, confirming its binding");
    }

    let alice_hex = hex::encode(alice_id.0);
    let bob_hex = hex::encode(bob_id.0);
    let carol_hex = hex::encode(carol_id.0);
    let seated = |rows: Vec<fauna_nest::db::rooms::RoomMemberRow>| {
        let mut out: Vec<String> = rows
            .into_iter()
            .map(|m| hex::encode(m.principal_id))
            .collect();
        out.sort();
        out
    };
    let mut three = vec![alice_hex.clone(), bob_hex.clone(), carol_hex.clone()];
    three.sort();
    let mut two = vec![alice_hex.clone(), bob_hex.clone()];
    two.sort();
    assert_eq!(
        seated(h_state.db.list_floor_roster(&channel_bytes).await.unwrap()),
        three,
        "H's floor holds all three seats before the removal"
    );

    // ── Before: both admitted foreign members hold all three doors. ──
    for who_hex in [&bob_hex, &carol_hex] {
        for (door, outcome) in
            three_binding_only_doors(&f_state, &h_base, who_hex, &channel_hex).await
        {
            assert_eq!(
                outcome,
                Ok(()),
                "an admitted foreign member is served {door} before the removal"
            );
        }
    }

    // ── The removal. This class's ONLY removal ceremony: an MLS membership
    // commit (which re-keys the group, so the ciphertext follows the epoch)
    // plus the committing device's roster report. `room.remove` is not
    // available here at all — it refuses a room whose membership authority is
    // its MLS group — which is exactly why the binding purge built into that
    // door never fired on this class.
    RailBackend::remove_participant(
        &*alice_backend,
        thread.thread_id.clone(),
        carol_chip.clone(),
    )
    .await
    .expect("the owner removes a foreign member by MLS commit");
    let alice_counts = alice_backend.roster_report_counts();
    assert_eq!(
        alice_counts.undelivered, 0,
        "and the report the commit owes reached the room's home: {alice_counts:?}"
    );
    assert_eq!(
        seated(h_state.db.list_floor_roster(&channel_bytes).await.unwrap()),
        two,
        "H's floor lost Carol — the report's absorb landed"
    );
    assert!(
        h_state
            .db
            .room_member_removed(&channel_bytes, &carol_id.0)
            .await
            .unwrap(),
        "as `removed_at` on her row, which is the verdict the doors now read"
    );
    assert!(
        h_state
            .db
            .foreign_member_binding(&channel_bytes, &carol_id.0)
            .await
            .unwrap()
            .is_some(),
        "and her binding SURVIVED the absorb: the community class's purge is not what          shuts the doors on this class, which is what makes the refusal recoverable below"
    );

    // ── After: every door that reads the binding alone is shut for her. ──
    for (door, outcome) in
        three_binding_only_doors(&f_state, &h_base, &carol_hex, &channel_hex).await
    {
        assert_eq!(
            outcome,
            Err("fauna.federation.forbidden".to_string()),
            "a removed foreign member's {door} is refused by the room's home"
        );
    }
    assert!(
        roster_through_own_nest(
            &f_base,
            &h_base,
            &channel_hex,
            &ActorKeypair::from_secret(carol_secret)
        )
        .await
        .is_none(),
        "and at the client seam the refusal degrades to `None`: the removed member's          device renders the room's members elided, as it did before she was admitted"
    );

    // ── Bob, bound and seated exactly as she was and never removed, is
    // untouched: the removal ended one admission, not the room's relay. ──
    for (door, outcome) in three_binding_only_doors(&f_state, &h_base, &bob_hex, &channel_hex).await
    {
        assert_eq!(
            outcome,
            Ok(()),
            "an unremoved co-member on the same nest still holds {door}"
        );
    }

    // ── Absence is not a removal. A member bound and never seated is still
    // served by all three doors: the newest-seated member the binding-only gate
    // is ratified to protect, admitted by a membership commit whose relayed
    // Welcome wrote H's binding, before that commit's roster report has seated
    // him on H's floor. Dave is seeded the way that relay writes a binding (the
    // insert arm, `InsertOnly`) and no report follows, so the only thing between
    // him and the doors is `refuse_removed_room_member` reading a floor that has
    // never heard of him. A helper reading "not live" where it must read
    // "positively removed" — the seat check the gate is ratified against —
    // refuses him here, and nowhere else in this file. ──
    let dave_id = ActorKeypair::generate().actor_id();
    let dave_hex = hex::encode(dave_id.0);
    h_state
        .db
        .register_foreign_channel_member(
            &channel_bytes,
            &dave_id.0,
            &f_state.nest_identity.public_key_bytes(),
            None,
            RebindPower::InsertOnly,
        )
        .await
        .expect("the Welcome relay's insert arm binds a newly admitted member");
    assert!(
        !seated(
            h_state
                .db
                .list_floor_roster_including_removed(&channel_bytes)
                .await
                .unwrap()
        )
        .contains(&dave_hex),
        "fixture: H's floor holds no row for him at all, live or removed"
    );
    for (door, outcome) in
        three_binding_only_doors(&f_state, &h_base, &dave_hex, &channel_hex).await
    {
        assert_eq!(
            outcome,
            Ok(()),
            "a bound member the floor has never heard of is served {door}"
        );
    }

    // ── Recoverable, which is the whole reason the doors read the floor rather
    // than the absorb purging the binding. A later honest report naming her
    // again clears `removed_at` (`replace_floor_roster`'s upsert) and her reach
    // returns with her seat. It is called here directly, at the body BOTH
    // report doors run, because what is under test is the doors' reading of the
    // floor and not which door moved it. Had the absorb purged her binding
    // instead, this arm would be unreachable: `InsertOnly` lets no honest
    // report write back a destroyed grant, and a member still inside the MLS
    // group has no re-Welcome ceremony to be re-admitted by. ──
    let stored_policy = h_state
        .db
        .get_room(&channel_bytes)
        .await
        .unwrap()
        .unwrap()
        .policy_version;
    let seat = |principal_id: [u8; 32], role: &str, home: &str| ReportedMember {
        principal_id,
        principal_kind: "user".into(),
        role: Some(role.into()),
        home_node_url: home.into(),
        reception_pubkey: Vec::new(),
    };
    h_state
        .db
        .replace_floor_roster(
            &channel_bytes,
            "end_to_end",
            "",
            stored_policy,
            None,
            &[
                seat(alice_id.0, "owner", ""),
                seat(bob_id.0, "member", &f_base),
                seat(carol_id.0, "member", &f_base),
            ],
        )
        .await
        .expect("a later honest report re-seats her");
    assert!(
        !h_state
            .db
            .room_member_removed(&channel_bytes, &carol_id.0)
            .await
            .unwrap(),
        "the floor no longer says she is gone"
    );
    for (door, outcome) in
        three_binding_only_doors(&f_state, &h_base, &carol_hex, &channel_hex).await
    {
        assert_eq!(
            outcome,
            Ok(()),
            "so {door} opens again — the refusal was a live read of the floor, not a              destroyed grant"
        );
    }

    let _ = (h_state, f_state);
}

/// **A foreign member's DEPARTURE ends its seat, not just its binding** — the voluntary twin of
/// the removal above, and the door that did not exist before it.
///
/// § Roles and authorization grants *leave (remove self)* to every role but
/// owner with no homing carve-out, and § The home nest has a member on a
/// foreign nest reach the room "only through their own home nest, which
/// originates the leg to the room's home". `room.leave` is a same-nest door on
/// a nest that holds no room record for a room homed elsewhere, so what a
/// departing foreign member could actually reach was the generic
/// `fauna.federation.channel.leave` — which drops the relay binding and never
/// touches the floor.
///
/// That is not half a fix, it is the wrong half. The **seat** is what the room
/// keys against: a live floor entry carrying a reception key *must* be wrapped
/// or the mint is refused (the roster-coverage gate), so the room could not
/// mint a generation afterwards without handing it to a member who had left —
/// and the same ghost refuses the re-admission that would heal it, `room.invite`
/// declining a principal that is already a member. Meanwhile the binding was
/// gone, so the departed member could not even read the floor it was still on.
///
/// Discriminating by construction: before the relay existed the leave seam sent
/// to the leaver's OWN nest, which homes no room here and answers "no such
/// room"; both halves are asserted — H's floor lost Bob, and the binding went
/// with it — plus the two consequences the ghost seat used to carry.
#[tokio::test]
async fn a_foreign_member_leaves_a_community_room_through_its_own_nest() {
    let (h_base, _h_authority, h_state) = start_home_nest().await;
    let (f_base, _f_authority, f_state) = start_foreign_nest().await;
    let f_nest_id = f_state.nest_identity.public_key_bytes();

    // Bob and Carol live on F; Alice, the room's owner, lives on H. (A room is
    // born on its owner's home and a transfer to a member homed elsewhere
    // re-homes it — § The home nest — so a room's owner is never one of its
    // foreign members, which is why the owner's refusal cannot fire here.)
    let bob = ActorKeypair::generate();
    let carol = ActorKeypair::generate();
    for (who, handle) in [(&bob, "bob"), (&carol, "carol")] {
        f_state
            .db
            .create_user_with_handle(&who.actor_id().0, "free", handle, None)
            .await
            .unwrap();
    }
    let alice = ActorKeypair::generate();
    h_state
        .db
        .create_user_with_handle(&alice.actor_id().0, "free", "alice", None)
        .await
        .unwrap();

    let mut room_id = [0u8; 32];
    getrandom::fill(&mut room_id).unwrap();
    let room_hex = hex::encode(room_id);
    let seat = |principal_id: [u8; 32], kind: &str, role: &str, home: &str| ReportedMember {
        principal_id,
        principal_kind: kind.into(),
        role: Some(role.into()),
        home_node_url: home.into(),
        reception_pubkey: Vec::new(),
    };
    let founded = h_state
        .db
        .found_room(
            &room_id,
            "community",
            &alice.actor_id().0,
            1,
            b"the founding policy",
            &[0x87u8; 32],
            &[
                seat(alice.actor_id().0, "user", "owner", ""),
                seat(bob.actor_id().0, "user", "member", &f_base),
                seat(carol.actor_id().0, "user", "member", &f_base),
                seat(
                    h_state.nest_identity.public_key_bytes(),
                    "nest",
                    "member",
                    "",
                ),
            ],
        )
        .await
        .expect("found the room");
    assert!(founded);

    for who in [&bob, &carol] {
        h_state
            .db
            .register_foreign_channel_member(
                &room_id,
                &who.actor_id().0,
                &f_nest_id,
                None,
                RebindPower::InsertOnly,
            )
            .await
            .expect("bind the foreign member");
    }

    let bob_hex = hex::encode(bob.actor_id().0);
    let carol_hex = hex::encode(carol.actor_id().0);

    // ── Before: seated, bound, and un-re-invitable. ──
    let entry_before = floor_entry_id(&h_state, &room_id, &bob.actor_id().0)
        .await
        .expect("a seated member holds a roster entry — the slot its wraps are sealed to");
    assert!(
        h_state
            .db
            .is_room_member(&room_id, &bob.actor_id().0)
            .await
            .unwrap(),
        "the predicate `room.invite` reads before opening an invitation"
    );
    let bob_before = roster_through_own_nest(&f_base, &h_base, &room_hex, &bob)
        .await
        .expect("an admitted foreign member reads the room's floor through its own nest");
    assert!(bob_before.contains(&bob_hex));

    // ── The departure, through Bob's OWN nest. ──
    // `room.leave_remote` on F → `fauna.federation.conversation.room.leave` on
    // H, under the same `require_foreign_member` binding that admits his
    // `channel.fetch`.
    let bob_nest = connected_client(&f_base, ActorKeypair::from_secret(*bob.secret_bytes())).await;
    let left = NestConversationsRpc::new(bob_nest)
        .room_leave(room_hex.clone(), Some(h_base.clone()))
        .await
        .expect("a foreign member leaves through its own nest");
    assert_eq!(
        left, 3,
        "the ack is the room home's own live floor count: Alice, Carol and the home nest's \
         reader seat"
    );

    // ── After: the seat is gone from the floor that counts. ──
    assert!(
        !h_state
            .db
            .is_room_member(&room_id, &bob.actor_id().0)
            .await
            .unwrap(),
        "the unseat landed on H — the ghost that would have drawn every later generation's \
         wrap, and refused the re-invite, is gone"
    );
    let carol_after = roster_through_own_nest(&f_base, &h_base, &room_hex, &carol)
        .await
        .expect("a member who did NOT leave still reads the floor");
    assert!(
        !carol_after.contains(&bob_hex),
        "and the floor she reads has lost him"
    );
    assert!(
        carol_after.contains(&carol_hex),
        "while still naming her: the departure was scoped to its own caller"
    );

    // ── And the binding went with it, in the same act. ──
    let refused = fauna_nest::federation_pool::originate_room_roster(
        &f_state.federation_pool,
        &f_state,
        &h_base,
        &bob_hex,
        &room_hex,
        None,
    )
    .await
    .expect("the federation channel itself is healthy")
    .expect_err("a departed member's relayed roster read is refused by the room's home");
    assert_eq!(
        refused.code, "fauna.federation.forbidden",
        "refused by the same structural gate that admitted it — the binding is gone, exactly \
         as a removal ends it"
    );
    let mint_refused = fauna_nest::federation_pool::originate_conversation_write_token_mint(
        &f_state.federation_pool,
        &f_state,
        &h_base,
        &bob_hex,
        &room_hex,
    )
    .await
    .expect("the federation channel itself is healthy")
    .expect_err("and the write-token mint with it");
    assert_eq!(mint_refused.code, "fauna.federation.forbidden");

    // Carol, bound and seated exactly as Bob was, is untouched: a self-leave
    // ends one membership, not the relay.
    fauna_nest::federation_pool::originate_conversation_write_token_mint(
        &f_state.federation_pool,
        &f_state,
        &h_base,
        &carol_hex,
        &room_hex,
    )
    .await
    .expect("the federation channel itself is healthy")
    .expect("her mint is untouched");

    // ── Re-admission is possible again, and lands on a FRESH entry. ──
    // Possible: the ghost seat used to make `room.invite` answer "that
    // principal is already a member", so a room could never take a departed
    // foreign member back. Fresh: "re-admission is a fresh entry id, so
    // add-wins resurrection is unrepresentable" (`db::rooms`) — the wraps
    // minted for the old seat stay bound to the old entry id and open nothing
    // at the new one.
    h_state
        .db
        .record_room_invite_and_deliver(
            &room_id,
            &bob.actor_id().0,
            &alice.actor_id().0,
            "member",
            &f_base,
            b"the re-invitation",
            b"the inbox envelope",
            false, // enforce_quota: the fixture nest tracks no inbox quota
        )
        .await
        .expect("the room takes a departed member back");
    assert!(
        h_state
            .db
            .accept_room_invite(&room_id, &bob.actor_id().0, &[], None)
            .await
            .expect("accept the re-invitation")
            .seated(),
        "and the acceptance seats him"
    );
    let entry_after = floor_entry_id(&h_state, &room_id, &bob.actor_id().0)
        .await
        .expect("a re-seated member holds a roster entry");
    assert_ne!(
        entry_before, entry_after,
        "a re-admission is a FRESH entry id — the departure's own guarantee, and what stops a \
         returning member replaying a wrap minted for the seat it left"
    );

    let _ = (h_state, f_state);
}

/// **The GENERIC `channel.leave` converges a room the same way**, and a relayed departure is idempotent.
///
/// Two properties of one door, both of which exist because a peer nest is not
/// obliged to pick our newest kind:
///
/// 1. `fauna.federation.channel.leave` is generic by ratified design
///    (`federation.md` § Federation residue surface — "conversations and folder
///    channels alike"), and its single write is the binding delete. On a room
///    that is half a departure, and a door that converges only when the caller
///    happens to pick the right kind is not converged — so on a
///    floor-authoritative room it runs the room's own leave body. Nothing of
///    ours calls it with a room channel id; a peer nest can.
/// 2. The relayed room leave answers a caller already off the floor with the
///    live count rather than a refusal — as the same-nest door does too for a
///    caller its floor stamped departed, though it still refuses one it never
///    seated. This
///    is the door a §4.D auto re-send arrives at, so a refusal here would
///    report a departure that landed as a failure that never happened.
#[tokio::test]
async fn the_generic_channel_leave_unseats_on_a_room_and_the_relayed_leave_is_idempotent() {
    let (h_base, _h_authority, h_state) = start_home_nest().await;
    let (f_base, _f_authority, f_state) = start_foreign_nest().await;
    let f_nest_id = f_state.nest_identity.public_key_bytes();

    let bob = ActorKeypair::generate();
    let carol = ActorKeypair::generate();
    for (who, handle) in [(&bob, "bob"), (&carol, "carol")] {
        f_state
            .db
            .create_user_with_handle(&who.actor_id().0, "free", handle, None)
            .await
            .unwrap();
    }
    let alice = ActorKeypair::generate();
    h_state
        .db
        .create_user_with_handle(&alice.actor_id().0, "free", "alice", None)
        .await
        .unwrap();

    let mut room_id = [0u8; 32];
    getrandom::fill(&mut room_id).unwrap();
    let room_hex = hex::encode(room_id);
    let seat = |principal_id: [u8; 32], kind: &str, role: &str, home: &str| ReportedMember {
        principal_id,
        principal_kind: kind.into(),
        role: Some(role.into()),
        home_node_url: home.into(),
        reception_pubkey: Vec::new(),
    };
    assert!(
        h_state
            .db
            .found_room(
                &room_id,
                "community",
                &alice.actor_id().0,
                1,
                b"the founding policy",
                &[0x88u8; 32],
                &[
                    seat(alice.actor_id().0, "user", "owner", ""),
                    seat(bob.actor_id().0, "user", "member", &f_base),
                    seat(carol.actor_id().0, "user", "member", &f_base),
                    seat(
                        h_state.nest_identity.public_key_bytes(),
                        "nest",
                        "member",
                        "",
                    ),
                ],
            )
            .await
            .expect("found the room")
    );
    for who in [&bob, &carol] {
        h_state
            .db
            .register_foreign_channel_member(
                &room_id,
                &who.actor_id().0,
                &f_nest_id,
                None,
                RebindPower::InsertOnly,
            )
            .await
            .expect("bind the foreign member");
    }

    // ── (1) The OLD generic kind, aimed at a room channel id. ──
    let removed = fauna_nest::federation_pool::originate_channel_leave(
        &f_state.federation_pool,
        &f_state,
        &h_base,
        &hex::encode(bob.actor_id().0),
        &room_hex,
    )
    .await
    .expect("the federation channel itself is healthy")
    .expect("the generic leave still serves a room");
    assert!(
        removed,
        "its `removed` flag keeps naming the BINDING — the kind's own contract, unchanged"
    );
    assert!(
        !h_state
            .db
            .is_room_member(&room_id, &bob.actor_id().0)
            .await
            .unwrap(),
        "but the SEAT went with it: the generic door runs the room's own leave body, so a peer \
         that picks the older kind converges to the same state rather than to half of it"
    );

    // ── (2) Carol departs twice on the room's own kind. ──
    let first = fauna_nest::federation_pool::originate_room_leave(
        &f_state.federation_pool,
        &f_state,
        &h_base,
        &hex::encode(carol.actor_id().0),
        &room_hex,
    )
    .await
    .expect("the federation channel itself is healthy")
    .expect("a foreign member leaves");
    assert_eq!(first.members, 2, "Alice and the home nest's reader seat");

    // The binding is gone with the seat, so the gate — not the body — is what
    // a re-send now meets. Re-bind, the way a Welcome relay would, to reach
    // the body's own idempotence.
    h_state
        .db
        .register_foreign_channel_member(
            &room_id,
            &carol.actor_id().0,
            &f_nest_id,
            None,
            RebindPower::InsertOnly,
        )
        .await
        .expect("rebind");
    let again = fauna_nest::federation_pool::originate_room_leave(
        &f_state.federation_pool,
        &f_state,
        &h_base,
        &hex::encode(carol.actor_id().0),
        &room_hex,
    )
    .await
    .expect("the federation channel itself is healthy")
    .expect("a departure that already landed is a converged success, not a refusal");
    assert_eq!(
        again.members, 2,
        "the same live count: the postcondition held before the call and holds after it"
    );

    // The gate still fails loud for a member this nest never bound — the
    // idempotence is the body's, never the authorization's.
    let stranger = ActorKeypair::generate();
    let refused = fauna_nest::federation_pool::originate_room_leave(
        &f_state.federation_pool,
        &f_state,
        &h_base,
        &hex::encode(stranger.actor_id().0),
        &room_hex,
    )
    .await
    .expect("the federation channel itself is healthy")
    .expect_err("a nest cannot retire a seat for a member it does not carry");
    assert_eq!(refused.code, "fauna.federation.forbidden");

    let _ = (h_state, f_state);
}

/// **A cross-nest invitation seats a member of another nest through its own
/// nest** — the last open clause of
/// `conversation-rooms.md` § Done definition's community box, and the ruling
/// § Join rules and invites → *A cross-nest invitation* makes: the home judges,
/// the invitee's nest relays and decides nothing.
///
/// Two real nests. Alice founds a community room on H through the production
/// ceremony seam and invites `bob@F` — `room.invite` on H judges the offer,
/// records the row with F's VERIFIED identity, and pushes the signed act to F
/// over `fauna.federation.conversation.room.invite`; F runs Bob's own reach
/// policy against Alice, binds `room_node` to H's verified identity, and
/// delivers one knock. Bob's client on F lists it through the same door a
/// same-nest invitation takes, and accepts through F, which relays
/// `fauna.federation.conversation.room.accept` to H.
///
/// What is pinned, in order:
///
/// 1. the knock reaches Bob on F naming H as the room's home;
/// 2. **an invitee who has not accepted has no relayed reach** — the floor
///    read a seated foreign member gets is refused, because the binding is
///    written by the accept, never by the delivery;
/// 3. the relayed accept seats Bob on H's floor AND binds him as a foreign
///    member from F, and his own nest now reads him the room's floor;
/// 4. the re-sent accept (the §4.D re-send the relay adds) converges rather
///    than refusing — the door is idempotent;
/// 5. the same-nest accept aimed at Bob's own nest fails loud, which is why
///    the relay is a distinct kind;
/// 6. **a nest the invitation was not delivered to cannot seat the invitee**,
///    whatever it claims about its member — the gate is the recorded
///    delivery, not the caller's word;
/// 7. and the room's own pending list (Alice's view) shows the offer before
///    the accept and not after.
///
/// Discriminating by construction: before this leg the invite handler
/// admitted a same-nest invitee only, so step 1 had no knock to find, and no
/// accept kind existed for step 3 to ride.
#[tokio::test]
async fn a_cross_nest_invitation_seats_the_foreign_member_through_its_own_nest() {
    let (h_base, h_authority, h_state) = start_home_nest().await;
    let (f_base, f_authority, f_state) = start_foreign_nest().await;
    let f_nest_id = f_state.nest_identity.public_key_bytes();

    // H runs the boot mint the fixture skips: the founding ceremony seats the
    // home nest with the room-read key minted at boot and refuses without it
    // (`room_create_handler`, a lookup never a mint).
    {
        let key = h_state
            .nest_signing_key
            .as_ref()
            .expect("the fixture's deployment key");
        h_state
            .db
            .set_nest_keypair(&key.to_bytes(), &key.verifying_key().to_bytes())
            .await
            .unwrap();
        fauna_nest::nest_kek::mint_at_boot(&h_state.db)
            .await
            .unwrap()
            .expect("a nest holding a deployment keypair runs the boot mint");
    }

    // Alice, the room's owner, lives on H; Bob and Carol live on F.
    let alice = ActorKeypair::generate();
    h_state
        .db
        .create_user_with_handle(&alice.actor_id().0, "free", "alice", None)
        .await
        .unwrap();
    let bob = ActorKeypair::generate();
    let carol = ActorKeypair::generate();
    for (who, handle) in [(&bob, "bob"), (&carol, "carol")] {
        f_state
            .db
            .create_user_with_handle(&who.actor_id().0, "free", handle, None)
            .await
            .unwrap();
        // Alice is an accepted contact of each — F's reach policy for its
        // own member, judged against the SIGNED inviter, is what admits the
        // knock under the default inbox mode.
        f_state
            .db
            .upsert_contact(&who.actor_id().0, &alice.actor_id().0, "accepted")
            .await
            .unwrap();
    }
    let _ = (h_authority.as_str(), f_authority.as_str());

    let alice_rpc = NestConversationsRpc::new(
        connected_client(&h_base, ActorKeypair::from_secret(*alice.secret_bytes())).await,
    );
    let bob_rpc = NestConversationsRpc::new(
        connected_client(&f_base, ActorKeypair::from_secret(*bob.secret_bytes())).await,
    );

    // ── Alice founds a community room on H, through the ceremony seam. ──
    let mut entropy = [0u8; 24];
    getrandom::fill(&mut entropy).unwrap();
    let salt = fauna_mls::room_policy::binding_birth_salt(&entropy);
    let policy =
        fauna_mls::room_policy::RoomPolicy::initial(alice.actor_id(), Some("the square".into()))
            .sign(&alice)
            .expect("the owner signs its own birth policy");
    let room_hex = RoomCeremonyRpc::room_create(
        &alice_rpc,
        hex::encode(salt),
        fauna_protocol::encode_canonical(&policy).unwrap().to_vec(),
        Vec::new(),
    )
    .await
    .expect("the founding ceremony");
    let room_id: [u8; 32] = hex::decode(&room_hex).unwrap().try_into().unwrap();
    let bob_hex = hex::encode(bob.actor_id().0);

    // ── The invitation crosses: H judges and pushes, F gates and delivers. ──
    let invite = fauna_mls::room_policy::RoomInvite {
        room_id: room_id.to_vec(),
        invitee: bob.actor_id(),
        role: fauna_mls::room_policy::RoomRole::Member,
        policy_version: 1,
    };
    let signed = invite.sign(&alice).expect("the inviter signs its own act");
    let role = RoomCeremonyRpc::room_invite(
        &alice_rpc,
        fauna_protocol::encode_canonical(&signed).unwrap().to_vec(),
        // The chip's handle domain, as the backend hands it to the seam; the
        // glue derives F's base URL from it exactly as for a Welcome relay.
        f_authority.clone(),
        // Alice is homed on the room's home: the same-nest kind.
        None,
    )
    .await
    .expect("H judges the invitation and F accepts the delivery");
    assert_eq!(role, "member");

    // (1) The knock stands on F, naming H as the next hop.
    let standing = RoomCeremonyRpc::room_pending_invitations(&bob_rpc)
        .await
        .expect("bob reads his own inbox");
    assert_eq!(standing.len(), 1, "exactly one knock, delivered once");
    assert_eq!(
        standing[0].room_node.as_deref(),
        Some(h_base.as_str()),
        "F wrote the room's home from H's verified identity, and it is what the accept relays to"
    );
    let delivered: fauna_mls::room_policy::SignedRoomInvite =
        fauna_core::encoding::canonical_decode(&standing[0].signed_invite)
            .expect("the signed act, verbatim");
    delivered
        .verify_signature()
        .expect("alice's signature crossed intact");
    assert_eq!(delivered.invite.room_id, room_id.to_vec());
    assert!(
        f_state
            .db
            .room_invite_home_binding(&room_id, &bob.actor_id().0)
            .await
            .unwrap()
            .is_none(),
        "F holds no invitation row: it stores one envelope and decides nothing about the room"
    );
    let recorded = h_state
        .db
        .room_invite_home_binding(&room_id, &bob.actor_id().0)
        .await
        .unwrap()
        .expect("H recorded the row it delivered");
    assert_eq!(
        recorded.invitee_nest_id,
        Some(f_nest_id),
        "the row carries F's VERIFIED identity, resolved from the delivery dial"
    );
    assert!(!recorded.accepted);

    // (7a) Alice's own pending list shows the offer.
    let pending = RoomCeremonyRpc::room_list_invites(&alice_rpc, room_hex.clone())
        .await
        .expect("the owner reads what stands pending");
    assert!(pending.iter().any(|p| p.invitee == bob.actor_id()));

    // (2) An invitee who has not accepted has NO relayed reach.
    assert!(
        roster_through_own_nest(&f_base, &h_base, &room_hex, &bob)
            .await
            .is_none(),
        "the delivery writes no binding, so the binding-only roster read refuses an unseated invitee"
    );
    assert!(
        h_state
            .db
            .foreign_member_binding(&room_id, &bob.actor_id().0)
            .await
            .unwrap()
            .is_none()
    );

    // (5) The same-nest accept aimed at Bob's own nest fails loud: F homes no
    // such room, which is why the relay is a distinct kind.
    let bob_recv = GroupReceptionKeyRecord::mint(1_700_000_000_000);
    let bob_key = bob_recv.reception_pubkey().unwrap();
    let local =
        RoomCeremonyRpc::room_accept_invite(&bob_rpc, room_hex.clone(), bob_key.clone(), None)
            .await
            .expect_err("bob's own nest holds no floor for a room homed on H");
    assert!(
        matches!(
            local,
            fauna_conversations::backend::ConvRpcError::Rejected { .. }
        ),
        "a definite refusal, not a transport fault: {local:?}"
    );

    // (3) The relayed accept: bob → F → H. Seats AND binds.
    let seated_as = RoomCeremonyRpc::room_accept_invite(
        &bob_rpc,
        room_hex.clone(),
        bob_key.clone(),
        Some(h_base.clone()),
    )
    .await
    .expect("F relays the acceptance and H seats him");
    assert_eq!(seated_as, "member");
    assert!(
        h_state
            .db
            .is_room_member(&room_id, &bob.actor_id().0)
            .await
            .unwrap(),
        "bob holds a live seat on H's floor"
    );
    let floor = h_state.db.list_floor_roster(&room_id).await.unwrap();
    let bob_row = floor
        .iter()
        .find(|m| m.principal_id == bob.actor_id().0)
        .expect("bob's floor row");
    assert_eq!(
        bob_row.reception_pubkey.as_deref(),
        Some(bob_key.as_slice()),
        "the seat carries the wrap target the accept brought — the founder's tend pass keys it in \
         exactly as a same-nest seat"
    );
    assert_eq!(
        bob_row.home_node_url, f_base,
        "and the URL the room reaches his nest through"
    );
    assert_eq!(
        h_state
            .db
            .foreign_member_binding(&room_id, &bob.actor_id().0)
            .await
            .unwrap()
            .map(|(home, _)| home),
        Some(f_nest_id),
        "bound as a foreign member from F — the row every relayed room door serves off"
    );
    let through_f = roster_through_own_nest(&f_base, &h_base, &room_hex, &bob)
        .await
        .expect("a seated foreign member reads the room's floor through its own nest");
    assert!(through_f.contains(&bob_hex));
    assert!(through_f.contains(&hex::encode(alice.actor_id().0)));
    // The knock still stands on F: it is the CLIENT's to settle
    // (`fauna.inbox.ack`, as after a same-nest accept) — H acks no row it does
    // not hold, and F decided nothing it could settle on.
    assert_eq!(
        RoomCeremonyRpc::room_pending_invitations(&bob_rpc)
            .await
            .unwrap()
            .len(),
        1
    );

    // (7b) Accepted invitations are history on the roster, never listed.
    let pending_after = RoomCeremonyRpc::room_list_invites(&alice_rpc, room_hex.clone())
        .await
        .unwrap();
    assert!(!pending_after.iter().any(|p| p.invitee == bob.actor_id()));

    // (4) The re-sent accept converges: same role, binding intact.
    let again = RoomCeremonyRpc::room_accept_invite(
        &bob_rpc,
        room_hex.clone(),
        bob_key.clone(),
        Some(h_base.clone()),
    )
    .await
    .expect("a re-sent accept reports the seating that landed");
    assert_eq!(again, "member");
    assert!(
        roster_through_own_nest(&f_base, &h_base, &room_hex, &bob)
            .await
            .is_some(),
        "and touched nothing"
    );

    // (6) A nest the invitation was NOT delivered to cannot seat the invitee.
    // Carol's row on H names some other nest as her home; F — her actual
    // home — relays her accept and is refused, whatever it asserts.
    let mut elsewhere = [0u8; 32];
    getrandom::fill(&mut elsewhere).unwrap();
    assert!(
        h_state
            .db
            .record_room_invite_for_foreign_delivery(
                &room_id,
                &carol.actor_id().0,
                &alice.actor_id().0,
                "member",
                "https://elsewhere.example",
                &fauna_protocol::encode_canonical(
                    &fauna_mls::room_policy::RoomInvite {
                        room_id: room_id.to_vec(),
                        invitee: carol.actor_id(),
                        role: fauna_mls::room_policy::RoomRole::Member,
                        policy_version: 1,
                    }
                    .sign(&alice)
                    .unwrap()
                )
                .unwrap(),
                &elsewhere,
            )
            .await
            .unwrap()
    );
    let carol_rpc = NestConversationsRpc::new(
        connected_client(&f_base, ActorKeypair::from_secret(*carol.secret_bytes())).await,
    );
    let refused = RoomCeremonyRpc::room_accept_invite(
        &carol_rpc,
        room_hex.clone(),
        Vec::new(),
        Some(h_base.clone()),
    )
    .await
    .expect_err("an invitation delivered elsewhere cannot be accepted from F");
    assert!(
        matches!(
            refused,
            fauna_conversations::backend::ConvRpcError::Rejected { .. }
        ),
        "refused by the home's invitation gate: {refused:?}"
    );
    assert!(
        !h_state
            .db
            .is_room_member(&room_id, &carol.actor_id().0)
            .await
            .unwrap()
    );
    assert!(
        h_state
            .db
            .foreign_member_binding(&room_id, &carol.actor_id().0)
            .await
            .unwrap()
            .is_none()
    );

    let _ = (h_state, f_state);
}

/// **A foreign member invites through its own nest under `member-invite`** —
/// the cross-nest invitation's *issuing* leg (`conversation-rooms.md` § Join
/// rules and invites → *A cross-nest invitation*, the foreign-inviter leg;
/// § Roles and authorization grants *invite* to any member under
/// `member-invite` with no homing carve-out; § The home nest — every other
/// nest relays).
///
/// Three real nests: H homes the room (Alice owns it, Dave lives there), F is
/// Bob's and Carol's, G is Erin's. Alice seats Bob through the delivery leg
/// the test above proves; from then on Bob is a foreign member, and his
/// invitations ride `room.invite_remote` → F →
/// `fauna.federation.conversation.room.invite_issue` → H, which gates on his
/// binding and runs the same-nest invite body. **The three deliveries are
/// three distinct arms of that one body, and each is pinned:**
///
/// - (a) an invitee on the INVITER's nest (Carol, F) — the empty node the
///   inviter's client leaves, resolved by H to F's verified identity, so the
///   knock lands on F with H bound as `room_node`, and H's row names F as
///   the nest her relayed accept must come from;
/// - (b) an invitee on the ROOM's HOME (Dave, H) — the node the inviter's
///   client derives from Dave's handle domain names H itself, which H serves
///   as a same-nest delivery: a knock in Dave's own inbox with no
///   `room_node`, a row with no delivered-to nest, a plain same-nest accept;
/// - (c) an invitee on a THIRD nest (Erin, G) — the push `room.invite`
///   itself uses, aimed at G.
///
/// And the gate and the judgement: an unseated principal on F issues
/// nothing (no binding — `forbidden` at H's gate, nothing recorded); a
/// signed act relayed under another member's name is refused on F before it
/// travels; F's same-nest door fails loud for a room it does not home; the
/// already-a-member refusal holds for a foreign inviter; and once Alice
/// flips the join rule to `invite`, a foreign plain member is refused by the
/// same judgement a home member is.
#[tokio::test]
async fn a_foreign_member_invites_through_its_own_nest_under_member_invite() {
    let (h_base, h_authority, h_state) = start_home_nest().await;
    let (f_base, f_authority, f_state) = start_foreign_nest().await;
    let (g_base, g_authority, g_state) = start_foreign_nest().await;
    let f_nest_id = f_state.nest_identity.public_key_bytes();
    let g_nest_id = g_state.nest_identity.public_key_bytes();

    {
        let key = h_state
            .nest_signing_key
            .as_ref()
            .expect("the fixture's deployment key");
        h_state
            .db
            .set_nest_keypair(&key.to_bytes(), &key.verifying_key().to_bytes())
            .await
            .unwrap();
        fauna_nest::nest_kek::mint_at_boot(&h_state.db)
            .await
            .unwrap()
            .expect("a nest holding a deployment keypair runs the boot mint");
    }

    // Alice (the owner) and Dave live on H; Bob, Carol and Frank on F; Erin
    // on G.
    let alice = ActorKeypair::generate();
    let dave = ActorKeypair::generate();
    for (who, handle) in [(&alice, "alice"), (&dave, "dave")] {
        h_state
            .db
            .create_user_with_handle(&who.actor_id().0, "free", handle, None)
            .await
            .unwrap();
    }
    let bob = ActorKeypair::generate();
    let carol = ActorKeypair::generate();
    let frank = ActorKeypair::generate();
    for (who, handle) in [(&bob, "bob"), (&carol, "carol"), (&frank, "frank")] {
        f_state
            .db
            .create_user_with_handle(&who.actor_id().0, "free", handle, None)
            .await
            .unwrap();
    }
    let erin = ActorKeypair::generate();
    g_state
        .db
        .create_user_with_handle(&erin.actor_id().0, "free", "erin", None)
        .await
        .unwrap();
    // Each invitee's OWN nest judges the knock by its member's reach policy
    // against the signed inviter: Alice reaches Bob, and Bob reaches Carol
    // (on F), Dave (on H — the same-nest arm, judged at federation origin)
    // and Erin (on G).
    f_state
        .db
        .upsert_contact(&bob.actor_id().0, &alice.actor_id().0, "accepted")
        .await
        .unwrap();
    f_state
        .db
        .upsert_contact(&carol.actor_id().0, &bob.actor_id().0, "accepted")
        .await
        .unwrap();
    // Frank reaches Alice: the closing pin has the owner invite him.
    f_state
        .db
        .upsert_contact(&frank.actor_id().0, &alice.actor_id().0, "accepted")
        .await
        .unwrap();
    h_state
        .db
        .upsert_contact(&dave.actor_id().0, &bob.actor_id().0, "accepted")
        .await
        .unwrap();
    g_state
        .db
        .upsert_contact(&erin.actor_id().0, &bob.actor_id().0, "accepted")
        .await
        .unwrap();

    let alice_rpc = NestConversationsRpc::new(
        connected_client(&h_base, ActorKeypair::from_secret(*alice.secret_bytes())).await,
    );
    let dave_rpc = NestConversationsRpc::new(
        connected_client(&h_base, ActorKeypair::from_secret(*dave.secret_bytes())).await,
    );
    let bob_rpc = NestConversationsRpc::new(
        connected_client(&f_base, ActorKeypair::from_secret(*bob.secret_bytes())).await,
    );
    let carol_rpc = NestConversationsRpc::new(
        connected_client(&f_base, ActorKeypair::from_secret(*carol.secret_bytes())).await,
    );
    let erin_rpc = NestConversationsRpc::new(
        connected_client(&g_base, ActorKeypair::from_secret(*erin.secret_bytes())).await,
    );

    // ── Alice founds a `member-invite` community room on H. ──
    let mut entropy = [0u8; 24];
    getrandom::fill(&mut entropy).unwrap();
    let salt = fauna_mls::room_policy::binding_birth_salt(&entropy);
    let mut policy =
        fauna_mls::room_policy::RoomPolicy::initial(alice.actor_id(), Some("the commons".into()));
    policy.join_rule = fauna_mls::room_policy::JoinRule::MemberInvite;
    let signed_policy = policy
        .sign(&alice)
        .expect("the owner signs its own birth policy");
    let room_hex = RoomCeremonyRpc::room_create(
        &alice_rpc,
        hex::encode(salt),
        fauna_protocol::encode_canonical(&signed_policy)
            .unwrap()
            .to_vec(),
        Vec::new(),
    )
    .await
    .expect("the founding ceremony");
    let room_id: [u8; 32] = hex::decode(&room_hex).unwrap().try_into().unwrap();

    let sign_invite = |inviter: &ActorKeypair, invitee: &ActorKeypair, policy_version: u64| {
        let invite = fauna_mls::room_policy::RoomInvite {
            room_id: room_id.to_vec(),
            invitee: invitee.actor_id(),
            role: fauna_mls::room_policy::RoomRole::Member,
            policy_version,
        };
        fauna_protocol::encode_canonical(&invite.sign(inviter).expect("signs"))
            .unwrap()
            .to_vec()
    };
    let rejected = |err: &fauna_conversations::backend::ConvRpcError| {
        matches!(
            err,
            fauna_conversations::backend::ConvRpcError::Rejected { .. }
        )
    };

    // ── Alice seats Bob: the delivery leg, then the relayed accept. ──
    RoomCeremonyRpc::room_invite(
        &alice_rpc,
        sign_invite(&alice, &bob, 1),
        f_authority.clone(),
        None,
    )
    .await
    .expect("H judges Alice's invitation and F accepts the delivery");
    let bob_recv = GroupReceptionKeyRecord::mint(1_700_000_000_000);
    let bob_key = bob_recv.reception_pubkey().unwrap();
    let seated_as = RoomCeremonyRpc::room_accept_invite(
        &bob_rpc,
        room_hex.clone(),
        bob_key.clone(),
        Some(h_base.clone()),
    )
    .await
    .expect("F relays the acceptance and H seats him");
    assert_eq!(seated_as, "member");

    // ── The gate: an UNSEATED principal on F issues nothing. ──
    // Carol holds no binding on H, so H's gate refuses her relayed act
    // before any judgement — and records nothing for Dave.
    let refused = RoomCeremonyRpc::room_invite(
        &carol_rpc,
        sign_invite(&carol, &dave, 1),
        h_authority.clone(),
        Some(h_base.clone()),
    )
    .await
    .expect_err("a principal with no binding on the room is not its member from F");
    assert!(
        rejected(&refused),
        "H's gate, a definite refusal: {refused:?}"
    );
    assert!(
        h_state
            .db
            .room_invite_home_binding(&room_id, &dave.actor_id().0)
            .await
            .unwrap()
            .is_none(),
        "nothing recorded behind a refused gate"
    );

    // ── The binding: F relays no act under another member's name. ──
    let refused = RoomCeremonyRpc::room_invite(
        &bob_rpc,
        sign_invite(&alice, &carol, 1),
        String::new(),
        Some(h_base.clone()),
    )
    .await
    .expect_err("Alice's signed act is not Bob's to relay");
    assert!(
        rejected(&refused),
        "refused on F, before it travels: {refused:?}"
    );
    assert!(
        RoomCeremonyRpc::room_pending_invitations(&carol_rpc)
            .await
            .unwrap()
            .is_empty(),
        "nothing was delivered"
    );

    // ── F's same-nest door fails loud for a room it does not home. ──
    let loud =
        RoomCeremonyRpc::room_invite(&bob_rpc, sign_invite(&bob, &carol, 1), String::new(), None)
            .await
            .expect_err("F holds no floor for a room homed on H");
    assert!(
        rejected(&loud),
        "a definite refusal, not a transport fault: {loud:?}"
    );

    // ── (a) Bob invites Carol — an invitee on the INVITER's nest. ──
    // The inviter's client leaves the node empty for a member of its own
    // nest; H resolves it to F's verified identity and pushes the knock back
    // to F, exactly as it pushes a knock to any foreign invitee.
    let role = RoomCeremonyRpc::room_invite(
        &bob_rpc,
        sign_invite(&bob, &carol, 1),
        String::new(),
        Some(h_base.clone()),
    )
    .await
    .expect("F relays Bob's act, H judges it under member-invite and delivers to F");
    assert_eq!(role, "member");
    let standing = RoomCeremonyRpc::room_pending_invitations(&carol_rpc)
        .await
        .expect("carol reads her own inbox on F");
    assert_eq!(standing.len(), 1, "exactly one knock, delivered once");
    assert_eq!(
        standing[0].room_node.as_deref(),
        Some(h_base.as_str()),
        "F bound the room's home from H's verified identity"
    );
    let delivered: fauna_mls::room_policy::SignedRoomInvite =
        fauna_core::encoding::canonical_decode(&standing[0].signed_invite).unwrap();
    delivered
        .verify_signature()
        .expect("bob's signature crossed intact");
    assert_eq!(delivered.inviter, bob.actor_id());
    let recorded = h_state
        .db
        .room_invite_home_binding(&room_id, &carol.actor_id().0)
        .await
        .unwrap()
        .expect("H recorded the row it delivered");
    assert_eq!(
        recorded.invitee_nest_id,
        Some(f_nest_id),
        "the row names F's VERIFIED identity — resolved from the origin, never inviter-declared"
    );
    assert_eq!(recorded.invitee_node_url, f_base);
    assert!(
        f_state
            .db
            .room_invite_home_binding(&room_id, &carol.actor_id().0)
            .await
            .unwrap()
            .is_none(),
        "F relayed and stored nothing of the room"
    );
    let carol_recv = GroupReceptionKeyRecord::mint(1_700_000_000_001);
    let carol_key = carol_recv.reception_pubkey().unwrap();
    let seated_as = RoomCeremonyRpc::room_accept_invite(
        &carol_rpc,
        room_hex.clone(),
        carol_key.clone(),
        Some(h_base.clone()),
    )
    .await
    .expect("Carol accepts through F and H seats her");
    assert_eq!(seated_as, "member");
    assert_eq!(
        h_state
            .db
            .foreign_member_binding(&room_id, &carol.actor_id().0)
            .await
            .unwrap()
            .map(|(home, _)| home),
        Some(f_nest_id),
        "bound as a foreign member from F"
    );
    let through_f = roster_through_own_nest(&f_base, &h_base, &room_hex, &carol)
        .await
        .expect("a seated foreign member reads the floor through its own nest");
    assert!(through_f.contains(&hex::encode(bob.actor_id().0)));
    assert!(through_f.contains(&hex::encode(carol.actor_id().0)));

    // ── (b) Bob invites Dave — an invitee on the ROOM's HOME. ──
    // Bob's client derives Dave's node from his handle domain, which is H's
    // own: H serves it as a same-nest delivery rather than dialling itself.
    let role = RoomCeremonyRpc::room_invite(
        &bob_rpc,
        sign_invite(&bob, &dave, 1),
        h_authority.clone(),
        Some(h_base.clone()),
    )
    .await
    .expect("H judges Bob's act and delivers into Dave's own inbox");
    assert_eq!(role, "member");
    let standing = RoomCeremonyRpc::room_pending_invitations(&dave_rpc)
        .await
        .expect("dave reads his own inbox on H");
    assert_eq!(standing.len(), 1, "one knock, in Dave's inbox on H");
    assert!(
        standing[0].room_node.is_none(),
        "a same-nest knock: the room is homed on the nest Dave is already talking to"
    );
    let recorded = h_state
        .db
        .room_invite_home_binding(&room_id, &dave.actor_id().0)
        .await
        .unwrap()
        .expect("H recorded the row");
    assert_eq!(
        recorded.invitee_nest_id, None,
        "delivered to nobody's nest but H's own"
    );
    assert!(recorded.invitee_node_url.is_empty());
    let dave_recv = GroupReceptionKeyRecord::mint(1_700_000_000_002);
    let dave_key = dave_recv.reception_pubkey().unwrap();
    let seated_as =
        RoomCeremonyRpc::room_accept_invite(&dave_rpc, room_hex.clone(), dave_key.clone(), None)
            .await
            .expect("the plain same-nest accept");
    assert_eq!(seated_as, "member");
    assert!(
        h_state
            .db
            .is_room_member(&room_id, &dave.actor_id().0)
            .await
            .unwrap()
    );
    assert!(
        h_state
            .db
            .foreign_member_binding(&room_id, &dave.actor_id().0)
            .await
            .unwrap()
            .is_none(),
        "a member homed on the room's home is no foreign member"
    );

    // ── (c) Bob invites Erin — an invitee on a THIRD nest. ──
    let role = RoomCeremonyRpc::room_invite(
        &bob_rpc,
        sign_invite(&bob, &erin, 1),
        g_authority.clone(),
        Some(h_base.clone()),
    )
    .await
    .expect("H judges Bob's act and pushes the knock to G");
    assert_eq!(role, "member");
    let standing = RoomCeremonyRpc::room_pending_invitations(&erin_rpc)
        .await
        .expect("erin reads her own inbox on G");
    assert_eq!(standing.len(), 1);
    assert_eq!(standing[0].room_node.as_deref(), Some(h_base.as_str()));
    let recorded = h_state
        .db
        .room_invite_home_binding(&room_id, &erin.actor_id().0)
        .await
        .unwrap()
        .expect("H recorded the row");
    assert_eq!(recorded.invitee_nest_id, Some(g_nest_id));
    assert_eq!(recorded.invitee_node_url, g_base);
    let erin_recv = GroupReceptionKeyRecord::mint(1_700_000_000_003);
    let erin_key = erin_recv.reception_pubkey().unwrap();
    let seated_as = RoomCeremonyRpc::room_accept_invite(
        &erin_rpc,
        room_hex.clone(),
        erin_key.clone(),
        Some(h_base.clone()),
    )
    .await
    .expect("Erin accepts through G and H seats her");
    assert_eq!(seated_as, "member");
    assert_eq!(
        h_state
            .db
            .foreign_member_binding(&room_id, &erin.actor_id().0)
            .await
            .unwrap()
            .map(|(home, _)| home),
        Some(g_nest_id),
        "bound as a foreign member from G"
    );
    let through_g = roster_through_own_nest(&g_base, &h_base, &room_hex, &erin)
        .await
        .expect("Erin reads the floor through G");
    assert_eq!(
        through_g.len(),
        6,
        "alice, bob, carol, dave, erin and the home nest"
    );
    let floor = h_state.db.list_floor_roster(&room_id).await.unwrap();
    for (who, home) in [(&carol, f_base.as_str()), (&erin, g_base.as_str())] {
        let row = floor
            .iter()
            .find(|m| m.principal_id == who.actor_id().0)
            .expect("a seat");
        assert_eq!(
            row.home_node_url, home,
            "the seat carries the nest it is reached through"
        );
    }

    // ── The same body: the already-a-member refusal holds for a foreign inviter. ──
    let refused = RoomCeremonyRpc::room_invite(
        &bob_rpc,
        sign_invite(&bob, &carol, 1),
        String::new(),
        Some(h_base.clone()),
    )
    .await
    .expect_err("Carol is already seated");
    assert!(rejected(&refused), "{refused:?}");

    // ── The same judgement: under `invite`, a foreign plain member is refused. ──
    let mut policy_v2 = policy.clone();
    policy_v2.version = 2;
    policy_v2.join_rule = fauna_mls::room_policy::JoinRule::Invite;
    let stored = RoomCeremonyRpc::room_set_policy(
        &alice_rpc,
        room_hex.clone(),
        fauna_protocol::encode_canonical(
            &policy_v2
                .sign_community(
                    &<[u8; 32]>::try_from(hex::decode(&room_hex).unwrap()).unwrap(),
                    &alice,
                )
                .unwrap(),
        )
        .unwrap()
        .to_vec(),
    )
    .await
    .expect("the owner re-signs the policy");
    assert_eq!(stored, 2);
    let refused = RoomCeremonyRpc::room_invite(
        &bob_rpc,
        sign_invite(&bob, &frank, 2),
        String::new(),
        Some(h_base.clone()),
    )
    .await
    .expect_err("the join rule now reserves invitations to the owner and admins");
    assert!(rejected(&refused), "{refused:?}");
    assert!(
        RoomCeremonyRpc::room_pending_invitations(&NestConversationsRpc::new(
            connected_client(&f_base, ActorKeypair::from_secret(*frank.secret_bytes())).await
        ))
        .await
        .unwrap()
        .is_empty(),
        "nothing was delivered to Frank"
    );
    // And the owner, homed on H, still invites — the rule is a rank, not a
    // homing.
    RoomCeremonyRpc::room_invite(
        &alice_rpc,
        sign_invite(&alice, &frank, 2),
        f_authority.clone(),
        None,
    )
    .await
    .expect("the owner invites under `invite`");

    let _ = (h_state, f_state, g_state);
}

use fauna_core::group_generation::GroupReceptionKeyRecord;
use fauna_core::group_scope::RosterMember;
use fauna_core::identity::ActorId;
use fauna_mls::wrapped_blob::group_generation_wraps;

/// The room's live floor as the recipient-set scheme's wrap-target set —
/// exactly what an honest minter reads back before building a mint, and the
/// same read the nest's coverage gate makes when it admits one. A member with
/// no reception key yet is not coverable and is skipped, here and there alike.
async fn live_wrap_targets(state: &Arc<AppState>, room_id: &[u8; 32]) -> Vec<RosterMember> {
    state
        .db
        .list_floor_roster(room_id)
        .await
        .expect("floor roster")
        .into_iter()
        .filter_map(|m| {
            let entry = m.entry_id?;
            let recv = m.reception_pubkey?;
            if recv.is_empty() {
                return None;
            }
            Some(RosterMember {
                entry_id: entry.try_into().ok()?,
                member_actor: ActorId(m.principal_id),
                reception_pubkey: recv,
                enrolled_at_ms: m.joined_at,
            })
        })
        .collect()
}

/// Build one mint over `targets`, parented on the room's current tip — the
/// minting device's own act, which the nest admits and never performs.
async fn build_mint_over(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    minter: &ActorKeypair,
    targets: &[RosterMember],
) -> (Vec<u8>, [u8; 32]) {
    let parents = state
        .db
        .room_generation_tip(room_id)
        .await
        .expect("tip read")
        .map(|t| vec![t.generation_id])
        .unwrap_or_default();
    let built = group_generation_wraps::build_group_mint(
        targets,
        parents,
        Vec::new(),
        minter.signing_key(),
        Vec::new(),
        1_700_000_000_000,
    )
    .expect("the mint assembles");
    (
        fauna_protocol::encode_canonical(&built.record)
            .unwrap()
            .to_vec(),
        built.generation_id,
    )
}

/// **A departure's FRESH entry is what keeps the old wraps from following it
/// back** — the consequence the case above asserts only the *input* of.
///
/// That case pins the input: a re-admission derives a new roster entry from
/// its own seating stamp (`db::rooms::accept_room_invite` →
/// `fauna_mls::room_policy::derive_room_entry_id`). This one pins what that id
/// is FOR, at the doors, in both directions:
///
/// 1. **While the seat is gone, the floor stops obliging a wrap to it.** The
///    roster-coverage gate reads the live floor *as it stands*, so a mint
///    naming only the remaining members is admitted — where a mint that skips
///    a member who is still seated is refused. That obligation is precisely
///    what the binding-only `channel.leave` left behind: a ghost
///    seat every later mint was *obliged* to wrap to, so the room could not
///    key itself without handing the key to someone who had left.
/// 2. **When the seat comes back, it is a different slot.** Every wrap is
///    sealed to `(generation, entry)` — AAD-bound, so a wrap lifted to another
///    slot fails at the opener
///    (`group_generation_wraps::a_built_mint_opens_at_each_members_own_slot_and_nowhere_else`)
///    — and the generations read serves a member exactly the wraps its
///    CURRENT entry holds (`conversations_handlers::room_generations_for_principal`).
///    So the returning member reads neither the generation minted while it was
///    away nor the one it held before it left. The old wraps are not deleted;
///    they stay bound to a slot that is gone, which is the scheme's
///    "re-admission is a fresh entry id, so add-wins resurrection is
///    unrepresentable" seen from the door.
#[tokio::test]
async fn a_generation_minted_while_a_foreign_member_is_away_does_not_open_at_the_seat_it_returns_to()
 {
    use fauna_protocol::RpcRequester;
    use fauna_protocol::conversations::{
        RoomGenerationsRemoteRequest, RoomGenerationsReply, RoomPublishGenerationReply,
        RoomPublishGenerationRequest,
    };

    let (h_base, _h_authority, h_state) = start_home_nest().await;
    let (f_base, _f_authority, f_state) = start_foreign_nest().await;
    let f_nest_id = f_state.nest_identity.public_key_bytes();

    let bob = ActorKeypair::generate();
    let carol = ActorKeypair::generate();
    for (who, handle) in [(&bob, "bob"), (&carol, "carol")] {
        f_state
            .db
            .create_user_with_handle(&who.actor_id().0, "free", handle, None)
            .await
            .unwrap();
    }
    let alice = ActorKeypair::generate();
    h_state
        .db
        .create_user_with_handle(&alice.actor_id().0, "free", "alice", None)
        .await
        .unwrap();

    // Reception keys, unlike the two cases above: this one is about the wraps
    // themselves, and a member holding no wrap target is not coverable at all.
    // The home nest's seat keeps none — it is deliberately outside the
    // coverage requirement (its wrap is the materialization grant, whose
    // absence is a revoke rather than a coverage failure).
    let alice_recv = GroupReceptionKeyRecord::mint(1_700_000_000_000);
    let bob_recv = GroupReceptionKeyRecord::mint(1_700_000_000_000);
    let carol_recv = GroupReceptionKeyRecord::mint(1_700_000_000_000);

    let mut room_id = [0u8; 32];
    getrandom::fill(&mut room_id).unwrap();
    let room_hex = hex::encode(room_id);
    let seat = |principal_id: [u8; 32], kind: &str, role: &str, home: &str, recv: Vec<u8>| {
        ReportedMember {
            principal_id,
            principal_kind: kind.into(),
            role: Some(role.into()),
            home_node_url: home.into(),
            reception_pubkey: recv,
        }
    };
    assert!(
        h_state
            .db
            .found_room(
                &room_id,
                "community",
                &alice.actor_id().0,
                1,
                b"the founding policy",
                &[0x89u8; 32],
                &[
                    seat(
                        alice.actor_id().0,
                        "user",
                        "owner",
                        "",
                        alice_recv.reception_pubkey().unwrap(),
                    ),
                    seat(
                        bob.actor_id().0,
                        "user",
                        "member",
                        &f_base,
                        bob_recv.reception_pubkey().unwrap(),
                    ),
                    seat(
                        carol.actor_id().0,
                        "user",
                        "member",
                        &f_base,
                        carol_recv.reception_pubkey().unwrap(),
                    ),
                    seat(
                        h_state.nest_identity.public_key_bytes(),
                        "nest",
                        "member",
                        "",
                        Vec::new(),
                    ),
                ],
            )
            .await
            .expect("found the room")
    );
    for who in [&bob, &carol] {
        h_state
            .db
            .register_foreign_channel_member(
                &room_id,
                &who.actor_id().0,
                &f_nest_id,
                None,
                RebindPower::InsertOnly,
            )
            .await
            .expect("bind the foreign member");
    }

    let alice_nest =
        connected_client(&h_base, ActorKeypair::from_secret(*alice.secret_bytes())).await;
    let bob_nest = connected_client(&f_base, ActorKeypair::from_secret(*bob.secret_bytes())).await;
    let entry_before = floor_entry_id(&h_state, &room_id, &bob.actor_id().0)
        .await
        .expect("a seated member holds a roster entry");

    // ── G1, minted while Bob is seated: coverage obliges his slot. ──
    let (g1_mint, g1) = build_mint_over(
        &h_state,
        &room_id,
        &alice,
        &live_wrap_targets(&h_state, &room_id).await,
    )
    .await;
    let published: RoomPublishGenerationReply = alice_nest
        .request(
            "fauna.conversations.room.publish_generation",
            RoomPublishGenerationRequest {
                room_id: room_hex.clone(),
                mint: g1_mint,
                extra: Default::default(),
            },
        )
        .await
        .expect("the owner keys the room");
    assert_eq!(
        published.covered, 3,
        "Alice, Bob and Carol — the room's whole coverable floor while he is seated"
    );
    let before: RoomGenerationsReply = bob_nest
        .request(
            "fauna.conversations.room.generations_remote",
            RoomGenerationsRemoteRequest {
                room_id: room_hex.clone(),
                nest_url: h_base.clone(),
                extra: Default::default(),
            },
        )
        .await
        .expect("a seated foreign member reads its generations through its own nest");
    assert_eq!(
        before.generations.len(),
        1,
        "the generation minted while he sat there is his to open"
    );
    let commitment: [u8; 32] = before.generations[0]
        .key_commitment
        .clone()
        .try_into()
        .unwrap();
    group_generation_wraps::open_group_generation_key_as_entry(
        &before.generations[0].wrap,
        &bob_recv.keypair().unwrap().secret,
        &g1,
        &entry_before
            .clone()
            .try_into()
            .expect("a roster entry id is 32 bytes"),
        &commitment,
    )
    .expect("and it really opens with his own reception secret, at his own slot");

    // ── The departure, through Bob's own nest. ──
    let left = NestConversationsRpc::new(bob_nest.clone())
        .room_leave(room_hex.clone(), Some(h_base.clone()))
        .await
        .expect("a foreign member leaves through its own nest");
    assert_eq!(left, 3, "Alice, Carol and the home nest's reader seat");

    // ── (1) The floor no longer obliges a wrap to the seat that left. ──
    // The control first, so the admission below cannot be a gate that never
    // refuses: a mint that skips Carol, who IS still seated, is refused.
    let live = live_wrap_targets(&h_state, &room_id).await;
    let (skips_carol, _) = build_mint_over(
        &h_state,
        &room_id,
        &alice,
        &live
            .iter()
            .filter(|t| t.member_actor.0 != carol.actor_id().0)
            .cloned()
            .collect::<Vec<_>>(),
    )
    .await;
    let refused = alice_nest
        .request::<_, RoomPublishGenerationReply>(
            "fauna.conversations.room.publish_generation",
            RoomPublishGenerationRequest {
                room_id: room_hex.clone(),
                mint: skips_carol,
                extra: Default::default(),
            },
        )
        .await
        .expect_err("a mint that leaves a live member unwrapped is refused");
    assert_eq!(
        refusal_code(&refused),
        "fauna.conversations.invalid_params",
        "the roster-coverage gate is live — so the admission below is a fact about the FLOOR, \
         not about a gate that admits everything"
    );
    let (g2_mint, g2) = build_mint_over(&h_state, &room_id, &alice, &live).await;
    let during: RoomPublishGenerationReply = alice_nest
        .request(
            "fauna.conversations.room.publish_generation",
            RoomPublishGenerationRequest {
                room_id: room_hex.clone(),
                mint: g2_mint,
                extra: Default::default(),
            },
        )
        .await
        .expect(
            "a mint naming only the members who remain is ADMITTED — the departed seat obliged \
             this mint to wrap to it right up until the unseat landed, which is the ghost a \
             binding-only departure left behind",
        );
    assert_eq!(
        during.covered, 2,
        "Alice and Carol: coverage counts the floor as it stands, and he is not on it"
    );
    let tip_entries = h_state
        .db
        .room_tip_wrapped_entries(&room_id)
        .await
        .expect("tip wrap entries")
        .expect("the room is keyed");
    assert!(
        !tip_entries.contains(&entry_before),
        "and the tip's wraps do not name the slot he left"
    );

    // ── (2) He comes back — on a different slot. ──
    h_state
        .db
        .record_room_invite_and_deliver(
            &room_id,
            &bob.actor_id().0,
            &alice.actor_id().0,
            "member",
            &f_base,
            b"the re-invitation",
            b"the inbox envelope",
            false, // enforce_quota: the fixture nest tracks no inbox quota
        )
        .await
        .expect("the room takes a departed member back");
    assert!(
        h_state
            .db
            .accept_room_invite(&room_id, &bob.actor_id().0, &[], None)
            .await
            .expect("accept the re-invitation")
            .seated(),
        "the acceptance seats him"
    );
    // The leave purged his binding with his seat, so his relayed reads meet
    // the federation gate rather than the body. Re-bind the way the Welcome
    // relay's insert arm would, to reach the read this case is about.
    h_state
        .db
        .register_foreign_channel_member(
            &room_id,
            &bob.actor_id().0,
            &f_nest_id,
            None,
            RebindPower::InsertOnly,
        )
        .await
        .expect("rebind");
    // Deliberately NOT re-asserting `entry_before != entry_after` here: the
    // case above owns that inequality, and restating it would stop this case
    // before the door it is about. With the fresh-entry rule broken — the
    // entry no longer depending on the seating stamp — it is the read below
    // that fails: the returning member is served the wrap of the seat he
    // left, which is exactly the replay a reused entry id would allow.
    let entry_after = floor_entry_id(&h_state, &room_id, &bob.actor_id().0)
        .await
        .expect("a re-seated member holds a roster entry");

    let after: RoomGenerationsReply = bob_nest
        .request(
            "fauna.conversations.room.generations_remote",
            RoomGenerationsRemoteRequest {
                room_id: room_hex.clone(),
                nest_url: h_base.clone(),
                extra: Default::default(),
            },
        )
        .await
        .expect("a re-seated foreign member reads through its own nest again");
    assert!(
        after.generations.is_empty(),
        "and reads NOTHING: not the generation minted while he was away, and not the one he \
         opened before he left — both wraps are sealed to slots that are not his any more. A \
         returning member is keyed by a top-up, never by the wraps of the seat it left"
    );

    // The control: the member who never left still reads both, so the empty
    // answer above is Bob's own standing, not a door that stopped serving.
    let carol_nest =
        connected_client(&f_base, ActorKeypair::from_secret(*carol.secret_bytes())).await;
    let carol_gens: RoomGenerationsReply = carol_nest
        .request(
            "fauna.conversations.room.generations_remote",
            RoomGenerationsRemoteRequest {
                room_id: room_hex.clone(),
                nest_url: h_base.clone(),
                extra: Default::default(),
            },
        )
        .await
        .expect("a member who never left reads her generations");
    assert_eq!(
        carol_gens.generations.len(),
        2,
        "both generations, on the one slot she has held throughout"
    );

    // At the store: the old wraps are kept and inert, exactly as the scheme
    // says (severance is wrap *targeting* on future mints, never deletion of
    // past ones) — they are simply bound to a slot the floor no longer names.
    let entry_before_32: [u8; 32] = entry_before.clone().try_into().unwrap();
    let entry_after_32: [u8; 32] = entry_after.try_into().unwrap();
    assert!(
        h_state
            .db
            .get_room_generation_wrap(&room_id, &g1, &entry_before_32)
            .await
            .unwrap()
            .is_some(),
        "G1's wrap for the departed slot is still stored"
    );
    for (label, generation) in [
        ("the one he held before leaving", g1),
        ("the one minted while he was away", g2),
    ] {
        assert!(
            h_state
                .db
                .get_room_generation_wrap(&room_id, &generation, &entry_after_32)
                .await
                .unwrap()
                .is_none(),
            "{label} has no wrap at the slot he returned to"
        );
    }

    let _ = (h_state, f_state);
}

/// One principal's roster-entry id on a room's floor, or `None` when it holds
/// no live seat — the slot every wrap of a room generation key is sealed to.
async fn floor_entry_id(
    state: &Arc<fauna_nest::routes::AppState>,
    room_id: &[u8; 32],
    principal_id: &[u8; 32],
) -> Option<Vec<u8>> {
    state
        .db
        .list_floor_roster(room_id)
        .await
        .expect("floor read")
        .into_iter()
        .find(|m| &m.principal_id == principal_id)
        .and_then(|m| m.entry_id)
}

/// The WRITE twin of the read above:
/// `conversation-rooms.md` § The floor roster has "the committing device
/// report the resulting roster to the **home** nest", and § The home nest has
/// a member homed elsewhere reach the room only through its own nest, which
/// originates the leg. So a membership commit authored on the **foreign** side
/// must land on the room home's floor — through the foreign member's own nest,
/// on the distinct kind `room.roster_report_remote` →
/// `fauna.federation.conversation.roster.report`, under the same
/// `require_foreign_member` binding that admits its `channel.fetch`.
///
/// Three seats make the room governed (`bootstrap_group`: more than one peer,
/// every key package carrying the policy extension): Alice (H, owner), Bob (F)
/// and Carol (H). Alice appoints Bob admin — an owner's policy commit whose
/// same-nest report is what bootstraps H's floor — then **Bob removes Carol**,
/// a commit authored on F's side of the federation, and H's floor follows.
///
/// Discriminating by construction: before the relay existed the report seam
/// always sent to the reporter's OWN nest, and Bob is on F's routing roster (his
/// relayed Welcome registered him there), so F's same-nest door would have
/// bootstrapped a stray floor on F — a nest that holds no room — while H's
/// floor kept naming Carol. Both halves are asserted: H's floor lost Carol,
/// and F holds no room record at all.
#[tokio::test]
async fn a_foreign_members_membership_commit_reaches_the_rooms_home_floor() {
    let (h_base, h_authority, h_state) = start_home_nest().await;
    let (f_base, f_authority, f_state) = start_foreign_nest().await;

    // Bob lives on F with a registered handle and one published key package.
    let mut bob_secret = [0u8; 32];
    getrandom::fill(&mut bob_secret).unwrap();
    let bob_id = ActorKeypair::from_secret(bob_secret).actor_id();
    f_state
        .db
        .create_user_with_handle(&bob_id.0, "free", "bob", None)
        .await
        .unwrap();
    let bob_engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(bob_secret)).expect("engine"));
    let bob_pkgs = bob_engine.generate_key_packages_bytes(1).expect("bob KP");
    f_state
        .db
        .put_key_package("bob-kp-0", &bob_id.0, &bob_pkgs[0], 0, FAR_FUTURE)
        .await
        .unwrap();

    // Carol lives on H — the third seat that makes the room governed. She
    // never has to join: what the test pins is who may REPORT her removal, and
    // through which nest.
    let mut carol_secret = [0u8; 32];
    getrandom::fill(&mut carol_secret).unwrap();
    let carol_id = ActorKeypair::from_secret(carol_secret).actor_id();
    h_state
        .db
        .create_user_with_handle(&carol_id.0, "free", "carol", None)
        .await
        .unwrap();
    // A same-nest stranger is contact-gated by default; the room's founding
    // Welcome to Carol is a new conversation, so her inbox is open (the
    // `conformance_custody_ceremony_client.rs` fixture).
    h_state
        .db
        .set_inbox_mode(&carol_id.0, "open")
        .await
        .unwrap();
    let carol_engine =
        MlsEngine::new_in_memory(ActorKeypair::from_secret(carol_secret)).expect("engine");
    let carol_pkgs = carol_engine
        .generate_key_packages_bytes(1)
        .expect("carol KP");
    h_state
        .db
        .put_key_package("carol-kp-0", &carol_id.0, &carol_pkgs[0], 0, FAR_FUTURE)
        .await
        .unwrap();

    // Alice lives on H, the room's home.
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user_with_handle(&alice_id.0, "free", "alice", None)
        .await
        .unwrap();
    let alice_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).expect("engine"),
    );

    // Both backends carry the report seam on the same glue object the apps'
    // session builders wire — the object that drains is the object that
    // reports.
    let alice_nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let alice_rpc = Arc::new(NestConversationsRpc::new(Arc::clone(&alice_nest)));
    let alice_backend = Arc::new(FaunaMlsBackend::new(
        alice_engine.clone(),
        alice_rpc.clone() as Arc<dyn ConversationsRpc>,
        format!("alice@{h_authority}"),
        alice_id,
    ));
    alice_backend.set_room_roster_reporter(alice_rpc.clone());
    let bob_nest = connected_client(&f_base, ActorKeypair::from_secret(bob_secret)).await;
    let bob_rpc = Arc::new(NestConversationsRpc::new(Arc::clone(&bob_nest)));
    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob_engine.clone(),
        bob_rpc.clone() as Arc<dyn ConversationsRpc>,
        format!("bob@{f_authority}"),
        bob_id,
    ));
    bob_backend.set_room_roster_reporter(bob_rpc.clone());
    bob_manager.register_backend(bob_backend.clone());

    // Alice founds the three-seat room by sending: the Welcome relays to F for
    // Bob (H records him as a foreign member of the channel) and lands in
    // Carol's inbox on H.
    let resolve = |handle: String| {
        let backend = alice_backend.clone();
        async move {
            match backend.resolve_address(&handle).await {
                ResolveResult::Resolved(addr @ TypedAddress::Fauna { .. }) => addr,
                other => panic!("expected Resolved(Fauna) for {handle}, got {other:?}"),
            }
        }
    };
    let bob_chip = resolve(format!("bob@{f_authority}")).await;
    let carol_chip = resolve(format!("carol@{h_authority}")).await;
    let thread = group_thread(
        ThreadId("t-xnest-roster-report".into()),
        vec![bob_chip, carol_chip.clone()],
    );
    alice_backend
        .send(
            &thread,
            &ComposeState {
                body_draft: "three of us".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("cross-nest group send (bootstrap + relay + channel.send)");
    let channel = alice_backend.bound_channels()[0];
    let channel_hex = channel.to_string();
    let channel_bytes: [u8; 32] = hex::decode(&channel_hex)
        .expect("channel hex")
        .try_into()
        .expect("32 bytes");
    assert!(
        matches!(alice_engine.room_policy(&channel), Some(Ok(_))),
        "a three-seat group is born governed — the case a report is owed for"
    );

    // Bob joins from the relayed Welcome and drains once, which confirms his
    // binding on H — the binding the relayed report is admitted under.
    let inbox = f_state.db.list_inbox_all(&bob_id.0).await.unwrap();
    assert_eq!(inbox.len(), 1, "exactly one welcome relayed to Bob on F");
    let welcome = welcome_bytes_from_inbox(&inbox[0].1);
    let bob_tid = ingest_welcome(&bob_backend, &bob_manager, &channel_hex, &welcome, &h_base)
        .await
        .expect("bob joins the cross-nest group from the relayed Welcome");
    let mut after = 0i64;
    poll_inbound_conv(&bob_backend, &bob_manager, &channel, &mut after, 0)
        .await
        .expect("bob drains the cross-nest channel via the federation relay");
    assert_eq!(
        bob_backend.channel_home_url(&channel).as_deref(),
        Some(h_base.as_str()),
        "bob's device records H as the channel's home — the signal that routes his report"
    );

    let seated = |rows: Vec<fauna_nest::db::rooms::RoomMemberRow>| {
        let mut out: Vec<String> = rows
            .into_iter()
            .map(|m| hex::encode(m.principal_id))
            .collect();
        out.sort();
        out
    };
    let mut three = vec![
        hex::encode(alice_id.0),
        hex::encode(bob_id.0),
        hex::encode(carol_id.0),
    ];
    three.sort();
    let mut two = vec![hex::encode(alice_id.0), hex::encode(bob_id.0)];
    two.sort();

    // The owner appoints Bob admin: a policy commit on H's own side, reported
    // to H as every same-nest commit is. It is the owner's SECOND report, not
    // the room's first -- since a later change every end-to-end room's creating
    // device makes a BIRTH report riding its first send
    // (`conversation-rooms.md` § The floor roster), so Alice's creation
    // already bootstrapped H's floor and this one carries the appointment.
    // Bob's count below is untouched by that ruling: he joined the room, he
    // did not create it, so his device owes no birth report.
    RailBackend::update_room_policy(
        &*alice_backend,
        thread.thread_id.clone(),
        RoomPolicyEdit::AppointAdmin(bob_id),
    )
    .await
    .expect("the owner appoints an admin");
    let alice_counts = alice_backend.roster_report_counts();
    assert_eq!(
        (alice_counts.owed, alice_counts.delivered),
        (2, 2),
        "the owner's policy commit is reported to her own nest, the room's home          -- her second report, after the birth report her creation owed"
    );
    assert_eq!(
        seated(h_state.db.list_floor_roster(&channel_bytes).await.unwrap()),
        three,
        "H's floor holds all three seats after the owner's report"
    );
    // The appointment was the newest commit on H's log, and its report named
    // exactly that position — the one a stale replay below will claim.
    let appointed_at = h_state
        .db
        .channel_commit_watermark(&channel_bytes)
        .await
        .unwrap();
    assert_eq!(
        h_state
            .db
            .get_room(&channel_bytes)
            .await
            .unwrap()
            .unwrap()
            .roster_commit_seq,
        Some(appointed_at),
        "the owner's report named the position its policy commit landed at"
    );

    // Bob catches up on the policy commit, so his engine holds the admin rank
    // the remove below is judged against.
    poll_inbound_conv(&bob_backend, &bob_manager, &channel, &mut after, 0)
        .await
        .expect("bob drains the policy commit");

    // Bob removes Carol — a membership commit authored on F's side of the
    // federation. The commit rides his `send_remote` relay to H's log; the
    // ROSTER REPORT it owes must ride the relay too.
    bob_manager
        .remove_participant(bob_tid.clone(), carol_chip.clone())
        .await;
    assert!(
        bob_manager.snapshot().error.is_none(),
        "an admin removes a member: {:?}",
        bob_manager.snapshot().error
    );
    let bob_counts = bob_backend.roster_report_counts();
    assert_eq!(
        (
            bob_counts.owed,
            bob_counts.delivered,
            bob_counts.undelivered
        ),
        (1, 1, 0),
        "the foreign member's report is DELIVERED — to the room's home, through F"
    );
    assert_eq!(
        seated(h_state.db.list_floor_roster(&channel_bytes).await.unwrap()),
        two,
        "H's floor follows the foreign member's Remove: Carol is off the room's home floor"
    );

    // The report is ORDERED by the commit it follows (`conversation-rooms.md`
    // § The floor roster), and the position crossed both hops: Bob's device
    // named what his relayed Remove got back — H's own log position — and F
    // forwarded it, so H's floor now sits at the newest commit H's log has
    // carried. Had either hop dropped it, the floor would still read the
    // appointment's position (an unordered report leaves it untouched).
    let removed_at = h_state
        .db
        .channel_commit_watermark(&channel_bytes)
        .await
        .unwrap();
    assert!(
        removed_at > appointed_at,
        "the Remove landed after the appointment"
    );
    assert_eq!(
        h_state
            .db
            .get_room(&channel_bytes)
            .await
            .unwrap()
            .unwrap()
            .roster_commit_seq,
        Some(removed_at),
        "H's floor holds the relayed Remove's position"
    );

    // WHOSE commit that position names crossed both hops too. H recorded the
    // sender at the same append it recorded the position, and for a relayed
    // append that sender is the `requesting_actor_id` the home bound at
    // `require_foreign_member` — Bob, homed on F
    // (`federation.md` § the room roster report row).
    assert_eq!(
        h_state
            .db
            .channel_commit_watermark_with_sender(&channel_bytes)
            .await
            .unwrap(),
        (removed_at, Some(bob_id.0)),
        "H records the FOREIGN member as the sender of the commit it relayed"
    );

    // So H's SAME-NEST door refuses Alice at Bob's position, though she is the
    // room's owner, a live member of H's floor and on its routing roster. This
    // is the authorship rule reaching across the federation boundary: the
    // sender was recorded on a relayed append, the reporter is compared on the
    // same-nest door, and the two must still name one actor. Were they to
    // drift, the member a foreign admin removes could be re-seated from the
    // home side at the very position the removal named.
    let not_hers = fauna_conversations::backend::RoomRosterReport {
        channel_hex: channel_hex.clone(),
        members: [
            (alice_id, fauna_conversations::room::RoomRole::Owner),
            (bob_id, fauna_conversations::room::RoomRole::Admin),
            (carol_id, fauna_conversations::room::RoomRole::Member),
        ]
        .into_iter()
        .map(
            |(actor, role)| fauna_conversations::backend::RoomRosterEntry {
                actor,
                role: Some(role),
            },
        )
        .collect(),
        policy_version: Some(2),
        commit_seq: Some(removed_at),
        home_nest_url: None,
    };
    assert_eq!(
        fauna_conversations::backend::RoomRosterReporter::report(&*alice_rpc, not_hers).await,
        fauna_conversations::backend::RoomRosterReportOutcome::Undelivered,
        "a report at the foreign member's commit position is refused from anyone else"
    );
    assert_eq!(
        seated(h_state.db.list_floor_roster(&channel_bytes).await.unwrap()),
        two,
        "the refused report puts nobody back on the home floor"
    );

    // And the relayed door is ordered like the same-nest one — one shared
    // body. The appointment's roster, replayed through F now, is a report of
    // an OLDER commit than the floor holds: delivered (the floor is newer than
    // it), and applied not at all — Carol stays off.
    let stale = fauna_conversations::backend::RoomRosterReport {
        channel_hex: channel_hex.clone(),
        members: [
            (alice_id, fauna_conversations::room::RoomRole::Owner),
            (bob_id, fauna_conversations::room::RoomRole::Admin),
            (carol_id, fauna_conversations::room::RoomRole::Member),
        ]
        .into_iter()
        .map(
            |(actor, role)| fauna_conversations::backend::RoomRosterEntry {
                actor,
                role: Some(role),
            },
        )
        .collect(),
        policy_version: Some(2),
        commit_seq: Some(appointed_at),
        home_nest_url: Some(h_base.clone()),
    };
    // Delivered, and distinguishable from stored: the ack's position reaches
    // the reporting device now rather than being dropped by the glue.
    assert_eq!(
        fauna_conversations::backend::RoomRosterReporter::report(&*bob_rpc, stale).await,
        fauna_conversations::backend::RoomRosterReportOutcome::Superseded {
            by: Some(removed_at)
        },
        "a superseded report is delivered, and names the position the floor holds"
    );
    assert_eq!(
        seated(h_state.db.list_floor_roster(&channel_bytes).await.unwrap()),
        two,
        "a stale relayed report puts nobody back on the home floor"
    );

    // The relay stored nothing: F — a nest that holds no room — must not have
    // been handed the report as if it were the home. Bob IS on F's routing
    // roster, so the pre-relay same-nest door would have bootstrapped a stray
    // floor there; its absence is the discriminator.
    assert!(
        f_state
            .db
            .is_actor_in_channel(&bob_id.0, &channel_bytes)
            .await
            .unwrap(),
        "fixture: bob sits on F's routing roster (his relayed Welcome registered him)"
    );
    assert!(
        f_state.db.get_room(&channel_bytes).await.unwrap().is_none(),
        "the relaying nest holds no room record — the report went to the home, not to F"
    );
}

/// Slice 2 (`federation.md` § Cross-nest — the `fauna.federation.channel.actors`
/// row): the add-participant heal's roster discipline now works from the
/// **non-home** side too. Bob's channel is foreign-homed (he joined from the
/// relayed Welcome, recording H as home), so his duplicate add of healthy
/// Alice rides the seam's home URL → F's relay-only
/// `fauna.conversations.channel.actors_remote` handler →
/// `fauna.federation.channel.actors` → H's structural foreign-member gate →
/// H's authoritative union — and lands in the idempotent no-op arm.
///
/// Discriminating by construction: before slice 2 the foreign-homed guard
/// declared the roster unreadable and this add REFUSED (an error); and a wrong
/// same-nest pick would read F's partial roster (Bob's federation welcome
/// auto-registered him; Alice absent), read `Some(false)`, and evict healthy
/// Alice — failing loudly at the re-admit's key-package fetch and moving the
/// epoch. `Ok` + an untouched epoch is reachable only through the relay.
#[tokio::test]
async fn duplicate_add_from_the_non_home_member_heals_via_the_actors_relay() {
    let (h_base, h_authority, h_state) = start_home_nest().await;
    let (f_base, f_authority, f_state) = start_foreign_nest().await;

    // Bob lives on F: real MLS engine, registered handle, one published KP.
    let mut bob_secret = [0u8; 32];
    getrandom::fill(&mut bob_secret).unwrap();
    let bob_id = ActorKeypair::from_secret(bob_secret).actor_id();
    f_state
        .db
        .create_user_with_handle(&bob_id.0, "free", "bob", None)
        .await
        .unwrap();
    let bob_engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(bob_secret)).expect("engine"));
    let bob_pkgs = bob_engine.generate_key_packages_bytes(1).expect("bob KP");
    f_state
        .db
        .put_key_package("bob-kp-0", &bob_id.0, &bob_pkgs[0], 0, FAR_FUTURE)
        .await
        .unwrap();

    // Alice lives on H.
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();
    let alice_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).expect("engine"),
    );

    let alice_nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let alice_backend = Arc::new(FaunaMlsBackend::new(
        alice_engine.clone(),
        Arc::new(NestConversationsRpc::new(alice_nest)) as Arc<dyn ConversationsRpc>,
        format!("alice@{h_authority}"),
        alice_id,
    ));
    let bob_nest = connected_client(&f_base, ActorKeypair::from_secret(bob_secret)).await;
    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob_engine.clone(),
        Arc::new(NestConversationsRpc::new(bob_nest)) as Arc<dyn ConversationsRpc>,
        format!("bob@{f_authority}"),
        bob_id,
    ));
    bob_manager.register_backend(bob_backend.clone());

    // Alice bootstraps the cross-nest group; her `channel.send` registers her
    // on H's `actor_channels`, and the Welcome relay writes Bob's
    // `channel_foreign_members` row on H — the two halves of H's union.
    let chip = match alice_backend
        .resolve_address(&format!("bob@{f_authority}"))
        .await
    {
        ResolveResult::Resolved(addr @ TypedAddress::Fauna { .. }) => addr,
        other => panic!("expected Resolved(Fauna), got {other:?}"),
    };
    let thread = one_to_one_thread(ThreadId("t-xnest-dup-remote".into()), chip.clone());
    alice_backend
        .send(
            &thread,
            &ComposeState {
                body_draft: "hello across nests".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("cross-nest send (bootstrap + relay + channel.send)");

    // Bob ingests the relayed Welcome — recording H as the channel's home.
    let inbox = f_state.db.list_inbox_all(&bob_id.0).await.unwrap();
    assert_eq!(inbox.len(), 1, "exactly one welcome relayed to Bob on F");
    let welcome = welcome_bytes_from_inbox(&inbox[0].1);
    let channel_hex = alice_backend.bound_channels()[0].to_string();
    let bob_thread = ingest_welcome(&bob_backend, &bob_manager, &channel_hex, &welcome, &h_base)
        .await
        .expect("bob joins the cross-nest group from the relayed Welcome");
    let bob_channel = bob_backend.bound_channels()[0];

    // Bob duplicate-adds healthy Alice from the NON-home side.
    let epoch_before = bob_engine.current_epoch(&bob_channel).unwrap();
    bob_backend
        .add_participant(
            bob_thread,
            TypedAddress::Fauna {
                handle: format!("alice@{h_authority}"),
                actor_id: alice_id,
            },
        )
        .await
        .expect("a duplicate add of a healthy member heals as a no-op from the non-home nest");
    assert_eq!(
        bob_engine.current_epoch(&bob_channel).unwrap(),
        epoch_before,
        "no-op means no commit: the epoch is untouched (an evict + re-admit advances it twice)"
    );
    assert_eq!(
        bob_engine.group_members(&bob_channel).len(),
        2,
        "the group is still exactly Alice + Bob"
    );
    assert!(
        h_state
            .db
            .list_inbox_all(&alice_id.0)
            .await
            .unwrap()
            .is_empty(),
        "no Welcome was minted for the healthy member — she was never evicted + re-invited"
    );

    let _ = f_authority;
}

/// Captures the raw iMIPs a scheduling drain hands out, so the test asserts the
/// bytes crossed the unpaired-nest boundary verbatim (the same shape the same-nest
/// `conformance_caldav_scheduling_mailbox_less::CapturingSink` uses).
#[derive(Default)]
struct CapturingSink {
    imips: std::sync::Mutex<Vec<Vec<u8>>>,
}

#[async_trait::async_trait]
impl SchedulingSink for CapturingSink {
    async fn apply_scheduling_imip(
        &self,
        raw_rfc5322: Vec<u8>,
        _origin: fauna_conversations::backend::SchedulingOrigin,
    ) -> Result<(), String> {
        self.imips.lock().unwrap().push(raw_rfc5322);
        Ok(())
    }
}

/// The **scheduling-rail** twin of the message-receipt test above — closes
/// `caldav-server.md` Slice 6b (the mailbox-less CalDAV iMIP for a cross-nest,
/// **unpaired** attendee). The same-nest proof is
/// `conformance_caldav_scheduling_mailbox_less`; this proves the *cross-nest* case
/// the federation relay unblocks: an organizer (Alice on H) delivers a scheduling
/// iMIP to a mailbox-less attendee (Bob on the unpaired nest F) over the one-off
/// `WelcomeKind::Scheduling` MLS channel, and Bob drains the iMIP off the rail via
/// `poll_inbound_scheduling` — whose `channel.fetch` relays to H, exactly as the
/// chat drain does. The iMIP application message (not just the Welcome) crosses.
#[tokio::test]
async fn mailbox_less_attendee_receives_scheduling_imip_across_unpaired_nests() {
    let (h_base, h_authority, h_state) = start_home_nest().await;
    let (f_base, f_authority, f_state) = start_foreign_nest().await;

    // Bob (the mailbox-less attendee) lives on F: real MLS engine, registered, one KP.
    let mut bob_secret = [0u8; 32];
    getrandom::fill(&mut bob_secret).unwrap();
    let bob_id = ActorKeypair::from_secret(bob_secret).actor_id();
    f_state
        .db
        .create_user_with_handle(&bob_id.0, "free", "bob", None)
        .await
        .unwrap();
    let bob_engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(bob_secret)).expect("engine"));
    let bob_pkgs = bob_engine.generate_key_packages_bytes(1).expect("bob KP");
    f_state
        .db
        .put_key_package("bob-kp-0", &bob_id.0, &bob_pkgs[0], 0, FAR_FUTURE)
        .await
        .unwrap();

    // Alice (the organizer) lives on H.
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();
    let alice_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).expect("engine"),
    );

    let alice_nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let alice_backend = FaunaMlsBackend::new(
        alice_engine.clone(),
        Arc::new(NestConversationsRpc::new(alice_nest)) as Arc<dyn ConversationsRpc>,
        format!("alice@{h_authority}"),
        alice_id,
    );
    let bob_nest = connected_client(&f_base, ActorKeypair::from_secret(bob_secret)).await;
    let bob_backend = FaunaMlsBackend::new(
        bob_engine.clone(),
        Arc::new(NestConversationsRpc::new(bob_nest)) as Arc<dyn ConversationsRpc>,
        format!("bob@{f_authority}"),
        bob_id,
    );

    // ── Alice delivers the iMIP over the scheduling rail to bob on F
    //    (`peer_domain = Some(F)` → the Welcome relays to F + Bob is recorded as a
    //    foreign member of the one-off channel on H; the iMIP is `channel.send`'d to
    //    H's channel log). ──
    let imip: Vec<u8> = b"BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n".to_vec();
    alice_backend
        .deliver_scheduling_imip(bob_id, Some(f_authority.clone()), imip.clone())
        .await
        .expect("deliver scheduling iMIP cross-nest (welcome relay + channel.send)");

    // ── Bob ingests the relayed scheduling Welcome (with H as the channel's home
    //    nest URL) and drains the iMIP off the rail — `poll_inbound_scheduling`
    //    relays `channel.fetch` to H, which authorizes Bob and returns the
    //    ciphertext F never held. ──
    let inbox = f_state.db.list_inbox_all(&bob_id.0).await.unwrap();
    assert_eq!(
        inbox.len(),
        1,
        "exactly one scheduling welcome relayed to Bob"
    );
    let welcome = welcome_bytes_from_inbox(&inbox[0].1);
    let bob_channel = ingest_scheduling_welcome(&bob_backend, "", &welcome, &h_base)
        .await
        .expect("bob joins the cross-nest scheduling group");

    let sink = CapturingSink::default();
    let mut after = 0i64;
    let drained = poll_inbound_scheduling(&bob_backend, &sink, &bob_channel, &mut after, 0)
        .await
        .expect("bob drains the cross-nest scheduling channel via the relay");
    assert_eq!(
        drained, 1,
        "exactly one iMIP drained over the unpaired-nest relay"
    );
    assert_eq!(
        sink.imips.lock().unwrap().as_slice(),
        &[imip],
        "the raw iMIP crossed the sealed MLS scheduling rail verbatim, cross-nest"
    );

    let _ = (h_state, f_state);
}

// ─────────────────────────────────────────────────────────────────────────────
// Phase 2 capstone — cross-nest SHARED FILE SET at client level (Success (a)–(c); (d) is the send_remote round-trip in the receive test above;
// (e)'s two commit-gate halves are pinned in `conversations_handlers` +
// `conformance_federation_channel`).
//
// Owner Alice on H shares a set with Bob on the unpaired nest F through the
// PRODUCTION owner path (`FoldersAuthor::share_set` with `member_nest_url`):
// Bob's KeyPackage is fetched over the H→F relay, the claimed channel binds on
// H, the genesis content key publishes, and the Folder-tagged Welcome relays
// to F carrying H's claimed-row-resolved `set_name`. Bob accepts through the
// production member path (`join_folder_welcome` + the real
// `NestFolderCustodySink`): the foreign-set custody record is written on F,
// custody ingests over the F→H `content_key.fetch` relay, the change log reads
// over the F→H `changes.fetch` relay, and the sealed bytes come DIRECT from
// H's public chunk routes (integrity by content address) and decrypt under the
// ingested custody. Then the containment half: evict purges the foreign row
// (S8) and the relayed read refuses; a voluntary leave (the client
// `leave_with_home` relay) does the same.
// ─────────────────────────────────────────────────────────────────────────────
/// The cross-nest owner label's trust rule on the Welcome relay
/// (`federation.md` § Cross-nest shared folders + channel append → *The
/// cross-nest owner label*): the receiving nest F forwards the origin's
/// `owner_handle`/`owner_domain` to its recipient only when the discovery chain
/// from the asserted domain lands on the authenticated origin H. H naming its
/// user at its own domain arrives paired; H naming one at F's domain — a domain
/// that resolves to ANOTHER nest's key — arrives with neither half, and with
/// `shared_by` unstamped either way (the label names, never admits). A non-folder
/// relay carries no label, whatever the origin asserted.
#[tokio::test]
async fn a_relayed_folder_welcome_carries_the_owner_label_only_when_its_domain_binds_to_the_origin()
{
    let (_h_base, h_authority, h_state) = start_home_nest().await;
    let (f_base, f_authority, f_state) = start_foreign_nest().await;

    let recipient = [0x66u8; 32];
    f_state
        .db
        .create_user_with_handle(&recipient, "free", "carol", None)
        .await
        .unwrap();

    let deliver = |channel: u8, channel_type: &str, domain: &str| {
        fauna_nest::federation_handlers::FedWelcomeDeliverRequest {
            recipient_actor_id: hex::encode(recipient),
            channel_id: Some(hex::encode([channel; 32])),
            welcome_bytes: vec![channel; 8],
            channel_type: Some(channel_type.to_string()),
            group_id: Some(format!("grp-{channel}")),
            owner_handle: Some("alice".to_string()),
            owner_domain: Some(domain.to_string()),
            ..Default::default()
        }
    };
    for req in [
        deliver(1, "folder", &h_authority),
        deliver(2, "folder", &f_authority),
        deliver(3, "dm", &h_authority),
    ] {
        fauna_nest::federation_pool::originate_welcome_deliver(
            &h_state.federation_pool,
            &h_state,
            &f_base,
            req,
        )
        .await
        .expect("welcome.deliver over the channel");
    }

    let staged: std::collections::BTreeMap<String, fauna_protocol::inbox::WelcomeInbox> = f_state
        .db
        .list_inbox_all(&recipient)
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            let w = fauna_protocol::inbox::InboxEnvelope::from_canonical_bytes(&row.1)
                .expect("canonical inbox envelope")
                .decode_welcome()
                .expect("welcome envelope");
            (w.channel_id.clone().expect("channel id"), w)
        })
        .collect();
    let at = |channel: u8| &staged[&hex::encode([channel; 32])];

    let honest = at(1);
    assert_eq!(honest.shared_by_handle.as_deref(), Some("alice"));
    assert_eq!(
        honest.shared_by_domain.as_deref(),
        Some(h_authority.as_str())
    );
    assert_eq!(honest.shared_by, None, "the label names, never admits");

    let spoof = at(2);
    assert_eq!(
        (
            spoof.shared_by_handle.as_deref(),
            spoof.shared_by_domain.as_deref()
        ),
        (None, None),
        "a domain that resolves to another nest's key forwards nothing"
    );
    assert_eq!(spoof.shared_by, None);

    let dm = at(3);
    assert_eq!(
        (
            dm.shared_by_handle.as_deref(),
            dm.shared_by_domain.as_deref()
        ),
        (None, None),
        "only a folder relay names its sharer"
    );
}

#[tokio::test]
async fn cross_nest_shared_folder_reads_evict_and_leave_through_client_stack() {
    use fauna_client_conversations::ConversationsClient;
    use fauna_client_folders::MemoryFolderKeyStore;
    use fauna_client_folders::orchestration::FoldersAuthor;
    use fauna_client_folders::{FoldersClient, NestFolderCustodySink, custody};
    use fauna_conversations::backends::fauna_mls::join_folder_welcome;
    use fauna_core::file_download::FileDownloadKeys;
    use fauna_protocol::RpcRequester;
    use fauna_protocol::folders::MemberEvictRequest;
    use fauna_protocol::sync::{
        SyncChangesListReply, SyncChangesListRequest, SyncRegisterReply, SyncRegisterRequest,
    };

    let (h_base, h_authority, h_state) = start_home_nest().await;
    let (f_base, f_authority, f_state) = start_foreign_nest().await;

    // Owner Alice on H; member Bob on F (registered, one published KeyPackage so
    // the cross-nest share can admit him).
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    // Alice holds a handle on H, so H — the set's home — stamps its cross-nest
    // owner label (`alice` @ H's domain) on the relayed Welcome.
    h_state
        .db
        .create_user_with_handle(&alice_id.0, "free", "alice", None)
        .await
        .unwrap();
    let alice_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).expect("alice engine"),
    );
    let mut bob_secret = [0u8; 32];
    getrandom::fill(&mut bob_secret).unwrap();
    let bob_id = ActorKeypair::from_secret(bob_secret).actor_id();
    let bob_hex = hex::encode(bob_id.0);
    f_state
        .db
        .create_user_with_handle(&bob_id.0, "free", "bob", None)
        .await
        .unwrap();
    let bob_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(bob_secret)).expect("bob engine"),
    );
    let bob_pkgs = bob_engine.generate_key_packages_bytes(1).expect("bob KP");
    f_state
        .db
        .put_key_package("bob-kp-0", &bob_id.0, &bob_pkgs[0], 0, FAR_FUTURE)
        .await
        .unwrap();

    // Alice creates "xnest-docs" on H the production way
    // (`set_lifecycle::create_set`): its set nonce minted into her custody
    // first, then `fauna.folders.create` carrying it. The share below keys that
    // entry in place and seals the nonce into the content-key envelope, which
    // is how a member learns what every signed record binds to.
    let alice_nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let alice_custody = Arc::new(MemoryFolderKeyStore::default());
    fauna_client_folders::set_lifecycle::create_set(
        &fauna_client_folders::FoldersClient::new(Arc::clone(&alice_nest)),
        alice_custody.as_ref(),
        fauna_protocol::folders::FolderCreateRequest {
            name: "xnest-docs".into(),
            ..Default::default()
        },
    )
    .await
    .expect("the set creates, its nonce in custody");

    // ── OWNER SHARE, the production path: KP over the H→F relay, claimed-channel
    // bind on H, genesis content key publish, Folder Welcome relayed to F. ──
    let a_author = FoldersAuthor::new(
        FoldersClient::new(Arc::clone(&alice_nest)),
        ActorKeypair::from_secret(alice_secret),
        alice_custody.clone(),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        alice_engine.clone(),
    );
    let convs = ConversationsClient::new(Arc::clone(&alice_nest));
    let outcome = a_author
        .share_set(&convs, "xnest-docs", bob_id, Some(f_base.clone()), None)
        .await
        .expect("cross-nest share through the production owner path");
    let channel_id = outcome.channel_id;
    let channel_hex = hex::encode(channel_id);

    // H recorded Bob as a foreign member bound to F (the Welcome-relay upsert) —
    // the row every federated read gate checks. Capture the resolved home nest
    // id for the post-evict re-arrangement below.
    let bob_home_nest = h_state
        .db
        .foreign_member_home_nest(&channel_id, &bob_id.0)
        .await
        .unwrap()
        .expect("welcome relay registered bob as a foreign member on H");

    // ── OWNER CONTENT: a real bound SyncEngine uploads one multi-chunk file
    // through H's real chunk routes (content-key sealed), and records the change
    // row over the authed WS control plane. ──
    let alice_cfg = alice_custody.snapshot();
    let content_keys =
        custody::content_keys(&alice_cfg, &channel_id).expect("bind_set staged genesis custody");

    let device_id = [0x0du8; 32];
    let _: SyncRegisterReply = alice_nest
        .request(
            "fauna.sync.register",
            SyncRegisterRequest {
                device_id: hex::encode(device_id),
                label: "alice-dev".into(),
                capabilities: "read,write".into(),
                ..Default::default()
            },
        )
        .await
        .expect("register alice's write device");

    let http_token = h_state.auth.token_store.insert(alice_id, 3600).await;
    // The fixture nests serve a self-signed floor cert — the chunk-plane HTTP
    // legs accept it explicitly (production uses ordinary WebPKI).
    let danger_http = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let http_bearer: Arc<dyn fauna_nest_http::BearerSource> =
        Arc::new(fauna_nest_http::StaticBearer(http_token));
    let http_auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        h_base.clone(),
        ActorKeypair::from_secret(alice_secret),
        http_bearer,
        danger_http.clone(),
    ));
    let watch = tempfile::tempdir().unwrap();
    let original: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(37) % 251) as u8)
        .collect();
    let rel = "shared/blob.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    let engine = fauna_sync_engine::engine::SyncEngine::new(
        watch.path().to_path_buf(),
        fauna_sync_engine::db::SyncDb::open_in_memory().unwrap(),
        fauna_sync_engine::nest_client::SyncClient::new(http_auth, &device_id),
        Some("xnest-docs".to_string()),
        device_id,
        None, // mls — the content path uses content_keys directly
        None, // epoch_secret
        None, // backup_key — None ⇒ the content-key seal path runs
        Some(
            alice_engine
                .group_id_bytes(&fauna_mls::types::ChannelId(channel_id))
                .expect("raw group id"),
        ),
        Some(content_keys.clone()),
        fauna_core::format::ConflictPolicy::Auto,
        fauna_core::format::FormatRegistry::new(),
        fauna_sync_engine::ignore::IgnoreMatcher::default(),
        4,
        fauna_sync_engine::transfer::TransferPool::new(
            Arc::new(fauna_sync_engine::adaptive::AdaptiveConcurrency::fixed(4)),
            None,
        ),
        Arc::clone(&alice_nest),
        fauna_sync_engine::config::SyncMode::Sync,
    );
    // Writer-signed records: the owner's engine signs under the set nonce her
    // custody holds for the bound channel (what `engine_binding` resolves in
    // production) — the nest refuses an unsigned record.
    engine.set_change_signer(
        Some(Arc::new(
            fauna_protocol::sync_writer_sig::ChangeSigner::direct(&ActorKeypair::from_secret(
                alice_secret,
            )),
        )),
        Some(
            custody::set_nonce_for_channel(&alice_cfg, &channel_id)
                .expect("create_set minted the nonce and the share keyed it under the channel"),
        ),
    );
    let uploaded = engine
        .upload_file(rel)
        .await
        .expect("owner upload through H's real chunk routes");
    assert!(uploaded.recorded, "H accepts the owner's signed record");

    // ── MEMBER ACCEPT, the production path: the relayed Welcome (with H's
    // claimed-row-resolved set_name) lands in Bob's durable inbox on F; Bob
    // joins off the chat rail with the real custody sink — writing the
    // foreign-set record + ingesting custody over the F→H relay. ──
    let inbox = f_state.db.list_inbox_all(&bob_id.0).await.unwrap();
    assert_eq!(inbox.len(), 1, "exactly one folder welcome relayed to Bob");
    let env = fauna_protocol::inbox::InboxEnvelope::from_canonical_bytes(&inbox[0].1)
        .expect("canonical inbox envelope");
    let staged = env.decode_welcome().expect("welcome envelope");
    assert_eq!(
        staged.channel_type.as_deref(),
        Some("folder"),
        "routed to the folder surface, not chat"
    );
    // The set was sealed at its keyed create, so H's claimed row rests no name:
    // the relayed Welcome carries the sealed pair alone (`path-sealing.md`,
    // *The Welcome carries the pair alone for a sealed set*).
    assert_eq!(
        staged.set_name, None,
        "a sealed set's Welcome carries no plaintext name"
    );
    // The cross-nest owner label (`federation.md` § … *The cross-nest owner
    // label*): H stamped `alice` + its own domain; F bound that domain to H's
    // verified key before forwarding, so the pair arrives — and `shared_by`
    // stays unstamped, so the contact gate still reads the arrival as a knock.
    assert_eq!(staged.shared_by_handle.as_deref(), Some("alice"));
    assert_eq!(
        staged.shared_by_domain.as_deref(),
        Some(h_authority.as_str())
    );
    assert_eq!(staged.shared_by, None, "the label names, never admits");
    assert!(
        staged.set_name_sealed.is_some(),
        "H resolved the set-name seal from its own claimed row and the relay carried it"
    );
    assert_eq!(
        staged.set_name_hash.as_deref().map(|h| h.to_vec()),
        Some(fauna_core::path_crypto::set_name_hash("xnest-docs").to_vec()),
        "the seal's salt is the set's convergent name hash"
    );

    let bob_nest = connected_client(&f_base, ActorKeypair::from_secret(bob_secret)).await;
    let bob_backend = FaunaMlsBackend::new(
        bob_engine.clone(),
        Arc::new(NestConversationsRpc::new(Arc::clone(&bob_nest))) as Arc<dyn ConversationsRpc>,
        format!("bob@{f_authority}"),
        bob_id,
    );
    let bob_custody = Arc::new(MemoryFolderKeyStore::default());
    bob_backend.set_folder_custody_sink(Arc::new(NestFolderCustodySink::new(
        Arc::clone(&bob_nest),
        bob_custody.clone(),
    )));
    // The fixture nests have no configured public domain, so the relayed
    // envelope carries no origin `nest_url`; production sets it. Pass H's base
    // explicitly, exactly as the conversations tests above do.
    // This share granted no explicit access, so H holds no role row for Bob and
    // asserts nothing — the reader default, and the fail-safe direction: the
    // recipient records `None` and no client offers a binding.
    assert_eq!(
        staged.access.as_deref(),
        None,
        "a share with no access grant asserts no access — reader by default"
    );
    join_folder_welcome(
        &bob_backend,
        &channel_hex,
        &staged.welcome_bytes,
        &h_base,
        &fauna_conversations::session::FolderWelcomeContext {
            set_name: staged.set_name.clone(),
            access: staged.access.clone(),
            home_nest_actor_id: staged.home_nest_actor_id.clone(),
            shared_by_handle: staged.shared_by_handle.clone(),
            shared_by_domain: staged.shared_by_domain.clone(),
            set_name_seal: fauna_core::label_custody::SealedSetName::from_wire(
                staged.set_name_sealed.as_deref().map(|b| &b[..]),
                staged.set_name_hash.as_deref().map(|b| &b[..]),
            ),
            ..Default::default()
        },
    )
    .await
    .expect("bob joins the cross-nest shared set off the chat rail");

    // The accept durably recorded the foreign set in Bob's own account-plane custody on F —
    // identity + routing + display name — and custody ingested over the relay.
    let bob_cfg = bob_custody.snapshot();
    let record = custody::find_foreign_set(&bob_cfg, &channel_id)
        .expect("accept wrote the foreign-set record");
    assert_eq!(record.home_nest_url, h_base);
    // A sealed set's Welcome carries no plaintext name; the join opens the
    // sealed pair under the set's content keys and names the record from it
    // (`custody::name_foreign_set_from_seal` — `path-sealing.md`, the
    // auto-join paragraph). A nameless record would get no engine binding.
    assert_eq!(record.set_name.as_deref(), Some("xnest-docs"));
    // The accept recorded the verified owner label beside it.
    assert_eq!(record.owner_handle.as_deref(), Some("alice"));
    assert_eq!(record.owner_domain.as_deref(), Some(h_authority.as_str()));
    // The relayed content-key read carries the same pair: F's domain binding is
    // warm from the Welcome relay, so its reply forwards H's stamp at once.
    let relayed: fauna_protocol::folders::ContentKeyGetReply = bob_nest
        .request(
            "fauna.folders.content_key.get",
            fauna_protocol::folders::ContentKeyGetRequest {
                nest_url: Some(h_base.clone()),
                channel_id: Some(channel_hex.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("the relayed content-key read");
    assert_eq!(relayed.owner_handle.as_deref(), Some("alice"));
    assert_eq!(relayed.owner_domain.as_deref(), Some(h_authority.as_str()));
    let bob_keys = custody::content_keys(&bob_cfg, &channel_id)
        .expect("custody ingested over the F->H content_key.fetch relay");
    assert_eq!(
        bob_keys.current.version, content_keys.current.version,
        "bob holds the owner's live generation"
    );

    // ── Success (a): the member reads end-to-end — change log via the F→H
    // relay, bytes DIRECT from H's public routes, decrypt under ingested
    // custody. ──
    let listed: SyncChangesListReply = bob_nest
        .request(
            "fauna.sync.changes.list",
            SyncChangesListRequest {
                nest_url: Some(h_base.clone()),
                channel_id: Some(channel_hex.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("relayed change-log read");
    // Post-S9-flip the relay is hash-addressed too: find by `path_hash`, name
    // only in the sealed label (`file-sync.md` § Sealed names & paths).
    let change = listed
        .changes
        .iter()
        .find(|c| c.path_hash == hex::encode(fauna_core::sync::path_hash(rel)))
        .expect("the owner's recorded change rides the relay");
    assert!(
        change.path.is_none(),
        "post-flip no plaintext path may ride the relayed changes wire"
    );
    let manifest_hex = change
        .manifest_hash
        .clone()
        .expect("a create carries a manifest");
    let manifest_digest: [u8; 32] = hex::decode(&manifest_hex).unwrap().try_into().unwrap();

    let fetcher = fauna_client::ForeignPublicChunkFetcher::with_http(&h_base, danger_http.clone());
    let keys = FileDownloadKeys {
        backup_key: None,
        mls_group_id: Some(record.mls_group_id.clone()),
        content_keys: Some(bob_keys.clone()),
        ..Default::default()
    };
    let bytes = fauna_core::file_download::download_file_bytes_by_manifest(
        &fetcher,
        &keys,
        fauna_core::data::ContentHash::from_digest_raw(manifest_digest),
        change.content_key_version,
        rel,
    )
    .await
    .expect("bytes direct from the home nest + decrypt under ingested custody");
    assert_eq!(
        bytes, original,
        "the member read the owner's file end-to-end"
    );
    // The relayed name opens under the member's ingested custody — the same
    // keys that just decrypted the bytes render the owner-sealed path.
    let envelope = fauna_core::path_crypto::SealedLabel::from_bytes(
        change
            .path_sealed
            .as_ref()
            .expect("a post-flip record carries its seal"),
    )
    .expect("the relayed seal is a well-formed envelope");
    let roots = keys
        .label_open_roots(envelope.generation)
        .expect("ingested custody covers the owner's seal generation");
    let opened = fauna_core::path_crypto::open(
        roots.iter(),
        &fauna_core::sync::path_hash(rel),
        fauna_core::path_crypto::LabelField::SyncChangePath,
        &envelope,
    )
    .expect("the owner's seal opens under the member's ingested custody");
    assert_eq!(
        opened,
        rel.as_bytes(),
        "the sealed name round-trips the relay"
    );

    // ── Success (c): the owner's roster shows the remote member; evict purges
    // the foreign row (S8) and the federated read refuses thereafter. ──
    let a_files = FoldersClient::new(Arc::clone(&alice_nest));
    let roster = a_files
        .actor_members_list("xnest-docs")
        .await
        .expect("owner roster");
    let bob_row = roster
        .members
        .iter()
        .find(|m| m.actor_id == bob_hex)
        .expect("the cross-nest member appears in the owner roster");
    assert_eq!(
        bob_row.remote,
        Some(true),
        "the roster row is marked remote (union of channel_foreign_members)"
    );

    let evicted = a_files
        .members_evict(MemberEvictRequest {
            name: "xnest-docs".into(),
            member: bob_hex.clone(),
            ..Default::default()
        })
        .await
        .expect("owner evicts the remote member");
    assert!(evicted.ok);
    assert!(
        h_state
            .db
            .foreign_member_home_nest(&channel_id, &bob_id.0)
            .await
            .unwrap()
            .is_none(),
        "evict purged the foreign-member row (S8)"
    );
    let refused = bob_nest
        .request::<_, SyncChangesListReply>(
            "fauna.sync.changes.list",
            SyncChangesListRequest {
                nest_url: Some(h_base.clone()),
                channel_id: Some(channel_hex.clone()),
                ..Default::default()
            },
        )
        .await
        .expect_err("an evicted member's federated read must refuse");
    assert_eq!(
        refusal_code(&refused),
        "fauna.federation.forbidden",
        "refusal is the structural-gate forbidden"
    );

    // ── Success (b): a voluntary leave through the client relay kills the read
    // the same way. Re-arrange Bob's row (the evict above consumed it) exactly
    // as the Welcome relay wrote it, then leave via the production client call. ──
    h_state
        .db
        .register_foreign_channel_member(
            &channel_id,
            &bob_id.0,
            &bob_home_nest,
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();
    let left = FoldersClient::new(Arc::clone(&bob_nest))
        .leave_with_home(hex::encode(&record.mls_group_id), Some(h_base.clone()))
        .await
        .expect("cross-nest leave relays to the home nest");
    assert!(left.ok && left.left, "the federated leave dropped the row");
    assert!(
        h_state
            .db
            .foreign_member_home_nest(&channel_id, &bob_id.0)
            .await
            .unwrap()
            .is_none(),
        "leave deleted the foreign-member row on H"
    );
    let refused = bob_nest
        .request::<_, SyncChangesListReply>(
            "fauna.sync.changes.list",
            SyncChangesListRequest {
                nest_url: Some(h_base.clone()),
                channel_id: Some(channel_hex),
                ..Default::default()
            },
        )
        .await
        .expect_err("a left member's federated read must refuse");
    assert_eq!(
        refusal_code(&refused),
        "fauna.federation.forbidden",
        "refusal is the structural-gate forbidden"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Phase 3 capstone — cross-nest shared folder WRITE plane at client level
// (Success (a) + (e)). (b) reader-refused-at-both and (c)
// replay-idempotence are pinned at the federation-channel level in
// `conformance_federation_channel::folder_write_plane_gates_on_writer_and_is_content_idempotent`;
// (d) owner-pays cap is pinned at the DB level in
// `sync_storage::tests::writer_record_charges_owner_and_enforces_member_cap`.
// This test proves the end-to-end (a): a foreign WRITER's real SyncEngine seals
// + uploads chunks DIRECT to the home nest under a minted write token, records
// the change over the F→H relay (nest-stamped to the writer), and the owner
// reads the new version back end-to-end; then (e): after eviction a fresh mint
// is refused.
//
// Bob (writer, home nest F) writes into Alice's set homed on H:
//   - control plane: Bob's own nest F; the engine's set_foreign_routing
//     makes changes.record carry nest_url=H so F relays to H's federated
//     changes.record (require_foreign_writer + owner-pays metering + content
//     idempotence).
//   - byte plane: pointed at H with a WriteTokenBearer — write_token.get on F
//     relays write_token.mint on H, and the minted bulk token authorizes the
//     direct chunk/manifest POSTs to H's public routes.
// ─────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn cross_nest_writer_records_and_uploads_owner_reads_back_through_client_stack() {
    use fauna_client_conversations::ConversationsClient;
    use fauna_client_folders::MemoryFolderKeyStore;
    use fauna_client_folders::orchestration::FoldersAuthor;
    use fauna_client_folders::{FoldersClient, NestFolderCustodySink, custody};
    use fauna_conversations::backends::fauna_mls::join_folder_welcome;
    use fauna_core::file_download::FileDownloadKeys;
    use fauna_protocol::RpcRequester;
    use fauna_protocol::folders::{WriteTokenGetReply, WriteTokenGetRequest};
    use fauna_protocol::sync::{
        SyncChangesListReply, SyncChangesListRequest, SyncRegisterReply, SyncRegisterRequest,
    };

    let (h_base, _h_authority, h_state) = start_home_nest().await;
    let (f_base, f_authority, f_state) = start_foreign_nest().await;

    // Owner Alice on H; writer Bob on F (registered, one KeyPackage).
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();
    let alice_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).expect("alice engine"),
    );
    let mut bob_secret = [0u8; 32];
    getrandom::fill(&mut bob_secret).unwrap();
    let bob_id = ActorKeypair::from_secret(bob_secret).actor_id();
    let bob_hex = hex::encode(bob_id.0);
    f_state
        .db
        .create_user_with_handle(&bob_id.0, "free", "bob", None)
        .await
        .unwrap();
    let bob_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(bob_secret)).expect("bob engine"),
    );
    let bob_pkgs = bob_engine.generate_key_packages_bytes(1).expect("bob KP");
    f_state
        .db
        .put_key_package("bob-kp-0", &bob_id.0, &bob_pkgs[0], 0, FAR_FUTURE)
        .await
        .unwrap();

    // Alice creates "xnest-docs" on H the production way
    // (`set_lifecycle::create_set`): its set nonce minted into her custody
    // first, then `fauna.folders.create` carrying it. The share below keys that
    // entry in place and seals the nonce into the content-key envelope, which
    // is how a member learns what every signed record binds to.
    let alice_nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let alice_custody = Arc::new(MemoryFolderKeyStore::default());
    fauna_client_folders::set_lifecycle::create_set(
        &fauna_client_folders::FoldersClient::new(Arc::clone(&alice_nest)),
        alice_custody.as_ref(),
        fauna_protocol::folders::FolderCreateRequest {
            name: "xnest-docs".into(),
            ..Default::default()
        },
    )
    .await
    .expect("the set creates, its nonce in custody");

    // ── OWNER SHARE, production path, granting Bob WRITER at share time. ──
    let a_author = FoldersAuthor::new(
        FoldersClient::new(Arc::clone(&alice_nest)),
        ActorKeypair::from_secret(alice_secret),
        alice_custody.clone(),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        alice_engine.clone(),
    );
    let convs = ConversationsClient::new(Arc::clone(&alice_nest));
    let outcome = a_author
        .share_set(
            &convs,
            "xnest-docs",
            bob_id,
            Some(f_base.clone()),
            Some("writer".to_string()),
        )
        .await
        .expect("cross-nest writer share through the production owner path");
    let channel_id = outcome.channel_id;
    let channel_hex = hex::encode(channel_id);
    let bob_home_nest = h_state
        .db
        .foreign_member_home_nest(&channel_id, &bob_id.0)
        .await
        .unwrap()
        .expect("welcome relay registered bob as a foreign member on H");

    // ── MEMBER ACCEPT (the  read path): Bob joins off the chat rail, writing
    // the foreign-set record + ingesting custody over the F→H relay. ──
    let inbox = f_state.db.list_inbox_all(&bob_id.0).await.unwrap();
    let env = fauna_protocol::inbox::InboxEnvelope::from_canonical_bytes(&inbox[0].1)
        .expect("canonical inbox envelope");
    let staged = env.decode_welcome().expect("welcome envelope");
    let bob_nest = connected_client(&f_base, ActorKeypair::from_secret(bob_secret)).await;
    let bob_backend = FaunaMlsBackend::new(
        bob_engine.clone(),
        Arc::new(NestConversationsRpc::new(Arc::clone(&bob_nest))) as Arc<dyn ConversationsRpc>,
        format!("bob@{f_authority}"),
        bob_id,
    );
    let bob_custody = Arc::new(MemoryFolderKeyStore::default());
    bob_backend.set_folder_custody_sink(Arc::new(NestFolderCustodySink::new(
        Arc::clone(&bob_nest),
        bob_custody.clone(),
    )));
    // Phase 4 discovery seed: H resolved Bob's grant from its OWN
    // `folder_member_access` row at `welcome_deliver_core` and the relay
    // carried it. Without this the accept below records `access: None` and
    // every app renders a foreign writer identically to a foreign reader —
    // the gap this leg closes (`federation.md` § Cross-nest → Recipient-side
    // access discovery).
    assert_eq!(
        staged.access.as_deref(),
        Some("writer"),
        "the share-time writer grant reached the recipient's staged welcome"
    );
    join_folder_welcome(
        &bob_backend,
        &channel_hex,
        &staged.welcome_bytes,
        &h_base,
        &fauna_conversations::session::FolderWelcomeContext {
            set_name: staged.set_name.clone(),
            access: staged.access.clone(),
            home_nest_actor_id: staged.home_nest_actor_id.clone(),
            set_name_seal: fauna_core::label_custody::SealedSetName::from_wire(
                staged.set_name_sealed.as_deref().map(|b| &b[..]),
                staged.set_name_hash.as_deref().map(|b| &b[..]),
            ),
            ..Default::default()
        },
    )
    .await
    .expect("bob joins the cross-nest shared set");
    let bob_cfg = bob_custody.snapshot();
    let record = custody::find_foreign_set(&bob_cfg, &channel_id)
        .expect("accept wrote the foreign-set record");
    assert_eq!(
        record.access.as_deref(),
        Some("writer"),
        "the accept durably recorded the grant, so a cold client start still \
         knows to offer this set's folder binding"
    );
    let bob_keys = custody::content_keys(&bob_cfg, &channel_id)
        .expect("custody ingested over the F->H content_key.fetch relay");

    // ── BOB'S WRITER ENGINE: byte plane → H under a WriteTokenBearer; control
    // plane → F with cross-nest write routing to H. ──
    let bob_device = [0x0bu8; 32];
    let _: SyncRegisterReply = bob_nest
        .request(
            "fauna.sync.register",
            SyncRegisterRequest {
                device_id: hex::encode(bob_device),
                label: "bob-dev".into(),
                capabilities: "read,write".into(),
                ..Default::default()
            },
        )
        .await
        .expect("register bob's write device on his own nest");

    let danger_http = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    // The byte-plane bearer: write_token.get on F → relayed write_token.mint on H.
    // The `AccessGate` is the terminal `access-revoked` park flag (D4) the bearer
    // shares with its engine — production builds it *before* the engine because a
    // demoted writer meets the mint refusal on its first upload byte. This test
    // exercises the ordinary cross-nest byte plane, not D4's mid-life demotion,
    // so it passes a fresh, live gate nothing else observes and the bearer is
    // never parked. (Call-site repair for the change that added the park
    // flag and left this site at the old arity.)
    let write_bearer: Arc<dyn fauna_nest_http::BearerSource> = Arc::new(
        fauna_sync_engine::write_token_bearer::folder_write_token_bearer(
            Arc::clone(&bob_nest),
            h_base.clone(),
            channel_hex.clone(),
            fauna_sync_engine::access_gate::AccessGate::new(),
        ),
    );
    let byte_auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        // The byte plane targets the HOME nest H (bytes never ride the relay).
        h_base.clone(),
        ActorKeypair::from_secret(bob_secret),
        write_bearer,
        danger_http.clone(),
    ));
    let watch = tempfile::tempdir().unwrap();
    let original: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(53) % 251) as u8)
        .collect();
    let rel = "shared/from-bob.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    let engine = fauna_sync_engine::engine::SyncEngine::new(
        watch.path().to_path_buf(),
        fauna_sync_engine::db::SyncDb::open_in_memory().unwrap(),
        fauna_sync_engine::nest_client::SyncClient::new(byte_auth, &bob_device),
        Some("xnest-docs".to_string()),
        bob_device,
        None, // mls
        None, // epoch_secret
        None, // backup_key — None ⇒ the content-key seal path runs
        Some(record.mls_group_id.clone()),
        Some(bob_keys.clone()),
        fauna_core::format::ConflictPolicy::Auto,
        fauna_core::format::FormatRegistry::new(),
        fauna_sync_engine::ignore::IgnoreMatcher::default(),
        4,
        fauna_sync_engine::transfer::TransferPool::new(
            Arc::new(fauna_sync_engine::adaptive::AdaptiveConcurrency::fixed(4)),
            None,
        ),
        // Control plane rides Bob's OWN nest F …
        Arc::clone(&bob_nest),
        fauna_sync_engine::config::SyncMode::Sync,
    );
    // … which relays changes.record to H (the set's home).
    engine.set_foreign_routing(h_base.clone(), channel_hex.clone());
    // Writer-signed records: Bob signs directly with his identity key (no cert
    // to carry) under the set nonce the owner's content-key envelope delivered
    // into his custody at accept — what `engine_binding` resolves for a
    // foreign set in production.
    engine.set_change_signer(
        Some(Arc::new(
            fauna_protocol::sync_writer_sig::ChangeSigner::direct(&ActorKeypair::from_secret(
                bob_secret,
            )),
        )),
        Some(
            custody::set_nonce_for_channel(&bob_cfg, &channel_id)
                .expect("the owner's content-key envelope carried the set nonce to the member"),
        ),
    );
    let uploaded = engine
        .upload_file(rel)
        .await
        .expect("bob uploads direct to H under a minted write token + relays the record");
    assert!(
        uploaded.recorded,
        "H accepts the cross-nest writer's signed record over the relay"
    );

    // ── Success (a): the OWNER reads Bob's new version end-to-end — the change
    // rides H's local log (Alice is owner), nest-stamped to Bob; bytes decrypt
    // under the shared content key. ──
    let alice_cfg = alice_custody.snapshot();
    let owner_keys =
        custody::content_keys(&alice_cfg, &channel_id).expect("owner staged genesis custody");
    let listed: SyncChangesListReply = alice_nest
        .request(
            "fauna.sync.changes.list",
            // The set was sealed at its keyed create, so its row rests no
            // name: the request leaves through the apps' funnel, by hash.
            fauna_protocol::folders::addressed(SyncChangesListRequest {
                folder: Some("xnest-docs".to_string()),
                ..Default::default()
            }),
        )
        .await
        .expect("owner reads her set's change log locally");
    // Post-S9-flip the listing is hash-addressed: the plaintext `path` column
    // rests NULL, so the row is found by `path_hash` and its name lives only in
    // the sealed label (`file-sync.md` § Sealed names & paths → contract step).
    let change = listed
        .changes
        .iter()
        .find(|c| c.path_hash == hex::encode(fauna_core::sync::path_hash(rel)))
        .expect("the cross-nest writer's record landed on the home nest");
    assert!(
        change.path.is_none(),
        "post-flip no plaintext path may ride the changes wire for a sealed plane"
    );
    assert_eq!(
        change.author_actor_id.as_deref(),
        Some(bob_hex.as_str()),
        "the record is nest-stamped to the writer, never client-asserted"
    );
    let manifest_hex = change
        .manifest_hash
        .clone()
        .expect("a create carries a manifest");
    let manifest_digest: [u8; 32] = hex::decode(&manifest_hex).unwrap().try_into().unwrap();
    let fetcher = fauna_client::ForeignPublicChunkFetcher::with_http(&h_base, danger_http.clone());
    let keys = FileDownloadKeys {
        backup_key: None,
        mls_group_id: Some(record.mls_group_id.clone()),
        content_keys: Some(owner_keys.clone()),
        ..Default::default()
    };
    let bytes = fauna_core::file_download::download_file_bytes_by_manifest(
        &fetcher,
        &keys,
        fauna_core::data::ContentHash::from_digest_raw(manifest_digest),
        change.content_key_version,
        rel,
    )
    .await
    .expect("owner reads the writer's file end-to-end");
    assert_eq!(
        bytes, original,
        "the owner read the cross-nest writer's file byte-for-byte"
    );
    // The name opens under the same custody as the bytes: the writer's engine
    // sealed the path under the shared M2 generation, and the owner's ordinary
    // download keys open it ("anyone who can open the set's bytes can render
    // its names" — file-sync.md § Sealed names & paths).
    let envelope = fauna_core::path_crypto::SealedLabel::from_bytes(
        change
            .path_sealed
            .as_ref()
            .expect("a post-flip record carries its seal"),
    )
    .expect("the relayed seal is a well-formed envelope");
    let roots = keys
        .label_open_roots(envelope.generation)
        .expect("owner custody covers the writer's seal generation");
    let opened = fauna_core::path_crypto::open(
        roots.iter(),
        &fauna_core::sync::path_hash(rel),
        fauna_core::path_crypto::LabelField::SyncChangePath,
        &envelope,
    )
    .expect("the cross-nest writer's seal opens under the owner's download custody");
    assert_eq!(
        opened,
        rel.as_bytes(),
        "the sealed name round-trips the relay"
    );

    // ── Success (e): after eviction, a fresh write-token mint is refused (the
    // structural writer gate no longer resolves). The residual window for an
    // ALREADY-held token is one FOREIGN_WRITE_TOKEN_TTL constant — accepted, not
    // waited on. ──
    let a_files = FoldersClient::new(Arc::clone(&alice_nest));
    a_files
        .members_evict(fauna_protocol::folders::MemberEvictRequest {
            name: "xnest-docs".into(),
            member: bob_hex.clone(),
            ..Default::default()
        })
        .await
        .expect("owner evicts the remote writer");
    // The foreign-member row is gone → require_foreign_member (inside the writer
    // gate) refuses the relayed mint.
    let mint_refused = bob_nest
        .request::<_, WriteTokenGetReply>(
            "fauna.folders.write_token.get",
            WriteTokenGetRequest {
                nest_url: h_base.clone(),
                channel_id: channel_hex.clone(),
                extra: Default::default(),
            },
        )
        .await
        .expect_err("an evicted writer's mint must be refused");
    assert_eq!(
        refusal_code(&mint_refused),
        "fauna.federation.forbidden",
        "post-evict mint refusal surfaces the relayed gate error"
    );

    let _ = bob_home_nest;
}

// ─────────────────────────────────────────────────────────────────────────────
// Residency reaches a cross-nest seat (`file-sync.md` § Relay serving → *A
// member on another nest*, step (1); `federation.md` § Cross-nest shared
// folders + channel append → *Relay serving across nests*, the `residency`
// stamp). The flow, end to end over two real nests: the owner's folder goes
// metadata-only → H's next federated content-key read reply to the member
// carries `residency` → the member's commit-poll fetch writes it into its
// `ForeignFolder` record → the member's engine arms the upload skip from that
// record → the member's write records a manifest and uploads no chunk → H's
// store holds the manifest and none of the file's chunks.
//
// The pair is seeded beneath the handlers: the folder is FULL when Alice
// shares (so the Welcome relay passes) and its column is then written
// metadata-only on H's DB. The interim refusal (`file-sync.md` § Relay serving
// → *Until that leg is built, the pair is refused*) stands; this is an
// existing pair, which it leaves as it is.
// ─────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn cross_nest_writer_into_a_metadata_only_folder_records_and_uploads_no_chunk() {
    use fauna_client_conversations::ConversationsClient;
    use fauna_client_folders::MemoryFolderKeyStore;
    use fauna_client_folders::orchestration::FoldersAuthor;
    use fauna_client_folders::{FoldersClient, NestFolderCustodySink, custody};
    use fauna_conversations::backend::FolderCustodySink;
    use fauna_conversations::backends::fauna_mls::join_folder_welcome;
    use fauna_protocol::RpcRequester;
    use fauna_protocol::sync::{SyncRegisterReply, SyncRegisterRequest};

    let (h_base, _h_authority, h_state) = start_home_nest().await;
    let (f_base, f_authority, f_state) = start_foreign_nest().await;

    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();
    let alice_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).expect("alice engine"),
    );
    let mut bob_secret = [0u8; 32];
    getrandom::fill(&mut bob_secret).unwrap();
    let bob_id = ActorKeypair::from_secret(bob_secret).actor_id();
    f_state
        .db
        .create_user_with_handle(&bob_id.0, "free", "bob", None)
        .await
        .unwrap();
    let bob_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(bob_secret)).expect("bob engine"),
    );
    let bob_pkgs = bob_engine.generate_key_packages_bytes(1).expect("bob KP");
    f_state
        .db
        .put_key_package("bob-kp-0", &bob_id.0, &bob_pkgs[0], 0, FAR_FUTURE)
        .await
        .unwrap();

    let alice_nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let alice_custody = Arc::new(MemoryFolderKeyStore::default());
    fauna_client_folders::set_lifecycle::create_set(
        &FoldersClient::new(Arc::clone(&alice_nest)),
        alice_custody.as_ref(),
        fauna_protocol::folders::FolderCreateRequest {
            name: "xnest-vault".into(),
            ..Default::default()
        },
    )
    .await
    .expect("the set creates, its nonce in custody");
    let a_author = FoldersAuthor::new(
        FoldersClient::new(Arc::clone(&alice_nest)),
        ActorKeypair::from_secret(alice_secret),
        alice_custody.clone(),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        alice_engine.clone(),
    );
    let channel_id = a_author
        .share_set(
            &ConversationsClient::new(Arc::clone(&alice_nest)),
            "xnest-vault",
            bob_id,
            Some(f_base.clone()),
            Some("writer".to_string()),
        )
        .await
        .expect("a full folder's cross-nest writer share relays")
        .channel_id;
    let channel_hex = hex::encode(channel_id);

    // ── Bob accepts: the record is written and custody ingested over the F→H
    // content-key relay, whose reply already states the folder FULL. ──
    let inbox = f_state.db.list_inbox_all(&bob_id.0).await.unwrap();
    let staged = fauna_protocol::inbox::InboxEnvelope::from_canonical_bytes(&inbox[0].1)
        .expect("canonical inbox envelope")
        .decode_welcome()
        .expect("welcome envelope");
    assert_eq!(
        staged.residency.as_deref(),
        Some("full"),
        "the Welcome relay carries the home nest's residency stamp beside `access`"
    );
    let bob_nest = connected_client(&f_base, ActorKeypair::from_secret(bob_secret)).await;
    let bob_backend = FaunaMlsBackend::new(
        bob_engine.clone(),
        Arc::new(NestConversationsRpc::new(Arc::clone(&bob_nest))) as Arc<dyn ConversationsRpc>,
        format!("bob@{f_authority}"),
        bob_id,
    );
    let bob_custody = Arc::new(MemoryFolderKeyStore::default());
    let bob_sink = Arc::new(NestFolderCustodySink::new(
        Arc::clone(&bob_nest),
        bob_custody.clone(),
    ));
    bob_backend.set_folder_custody_sink(bob_sink.clone());
    join_folder_welcome(
        &bob_backend,
        &channel_hex,
        &staged.welcome_bytes,
        &h_base,
        &fauna_conversations::session::FolderWelcomeContext {
            set_name: staged.set_name.clone(),
            access: staged.access.clone(),
            home_nest_actor_id: staged.home_nest_actor_id.clone(),
            set_name_seal: fauna_core::label_custody::SealedSetName::from_wire(
                staged.set_name_sealed.as_deref().map(|b| &b[..]),
                staged.set_name_hash.as_deref().map(|b| &b[..]),
            ),
            ..Default::default()
        },
    )
    .await
    .expect("bob joins the cross-nest shared set");
    let residency_of = || {
        custody::find_foreign_set(&bob_custody.snapshot(), &channel_id)
            .expect("accept wrote the foreign-set record")
            .metadata_only_residency()
    };
    assert_eq!(
        residency_of(),
        Some(false),
        "the accept's federated content-key read stamped the record full"
    );

    // ── The folder goes metadata-only on H, beneath the handlers — addressed by
    // its row id, since a set created the production way rests no plaintext
    // name to address it by. ──
    let folder_id = h_state
        .db
        .get_folders_for_actor_full(&alice_id.0)
        .await
        .unwrap()
        .into_iter()
        .find(|fs| {
            fs.mls_group_id
                .as_deref()
                .is_some_and(|g| fauna_mls::types::ChannelId::from_group_id(g).0 == channel_id)
        })
        .expect("H holds the claimed row")
        .id;
    assert!(
        h_state
            .db
            .update_folder_by_id(
                folder_id,
                fauna_nest::db::FolderUpdate {
                    residency: Some(Some("metadata_only")),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        "fixture: the folder is metadata-only"
    );
    // ── Bob's next commit poll: the federated content-key read carries the
    // flip into his record. ──
    assert!(
        bob_sink.fetch_sealed_envelope(&channel_hex).await.is_some(),
        "the commit poll's envelope fetch is served"
    );
    assert_eq!(
        residency_of(),
        Some(true),
        "one commit poll carries the flip into the member's custody record"
    );

    // ── Bob's writer engine, armed from his record as the foreign binding
    // arms it (`engine_lifecycle`'s foreign `ResolvedBinding`). ──
    let bob_cfg = bob_custody.snapshot();
    let record = custody::find_foreign_set(&bob_cfg, &channel_id).unwrap();
    let bob_keys = custody::content_keys(&bob_cfg, &channel_id).expect("custody ingested");
    let bob_device = [0x0cu8; 32];
    let _: SyncRegisterReply = bob_nest
        .request(
            "fauna.sync.register",
            SyncRegisterRequest {
                device_id: hex::encode(bob_device),
                label: "bob-dev".into(),
                capabilities: "read,write".into(),
                ..Default::default()
            },
        )
        .await
        .expect("register bob's write device on his own nest");
    let danger_http = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let write_bearer: Arc<dyn fauna_nest_http::BearerSource> = Arc::new(
        fauna_sync_engine::write_token_bearer::folder_write_token_bearer(
            Arc::clone(&bob_nest),
            h_base.clone(),
            channel_hex.clone(),
            fauna_sync_engine::access_gate::AccessGate::new(),
        ),
    );
    let byte_auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        h_base.clone(),
        ActorKeypair::from_secret(bob_secret),
        write_bearer,
        danger_http,
    ));
    let watch = tempfile::tempdir().unwrap();
    // Under the single-chunk threshold: one sealed body, named by its store key.
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let original: Vec<u8> = (0..1_000_000u32)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    let rel = "vault/from-bob.bin";
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, &original).unwrap();
    let engine = fauna_sync_engine::engine::SyncEngine::new(
        watch.path().to_path_buf(),
        fauna_sync_engine::db::SyncDb::open_in_memory().unwrap(),
        fauna_sync_engine::nest_client::SyncClient::new(byte_auth, &bob_device),
        Some("xnest-vault".to_string()),
        bob_device,
        None,
        None,
        None,
        Some(record.mls_group_id.clone()),
        Some(bob_keys),
        fauna_core::format::ConflictPolicy::Auto,
        fauna_core::format::FormatRegistry::new(),
        fauna_sync_engine::ignore::IgnoreMatcher::default(),
        4,
        fauna_sync_engine::transfer::TransferPool::new(
            Arc::new(fauna_sync_engine::adaptive::AdaptiveConcurrency::fixed(4)),
            None,
        ),
        Arc::clone(&bob_nest),
        fauna_sync_engine::config::SyncMode::Sync,
    )
    .with_residency_reading(record.metadata_only_residency());
    engine.set_foreign_routing(h_base.clone(), channel_hex.clone());
    engine.set_change_signer(
        Some(Arc::new(
            fauna_protocol::sync_writer_sig::ChangeSigner::direct(&ActorKeypair::from_secret(
                bob_secret,
            )),
        )),
        Some(custody::set_nonce_for_channel(&bob_cfg, &channel_id).expect("the set nonce")),
    );
    assert!(
        engine.is_metadata_only_residency(),
        "the seat is armed from its record"
    );
    let uploaded = engine
        .upload_file(rel)
        .await
        .expect("bob records the change over the relay");
    assert!(
        uploaded.recorded,
        "H accepts the cross-nest writer's record"
    );

    // ── Both sides named: the file's store keys off Bob's own index, and the
    // manifest off H's log — never the blob root weighed. ──
    let keys = engine.db().held_store_keys(rel).unwrap();
    assert!(
        !keys.is_empty(),
        "the seal indexed the file's store keys, or the absence below proves nothing"
    );
    let store = h_state
        .backup_service
        .as_ref()
        .expect("H runs a blob store")
        .local_blob_store();
    for key in &keys {
        assert!(
            !store
                .exists(&fauna_core::data::ContentHash::from_digest_raw(*key))
                .await
                .unwrap(),
            "a metadata-only folder's cross-nest seat uploads no chunk bytes to the home nest"
        );
    }
    let change = h_state
        .db
        .get_sync_changes_for_folder(folder_id, 0, None)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.path_hash == fauna_core::sync::path_hash(rel).to_vec())
        .expect("the cross-nest writer's record landed on the home nest");
    let manifest: [u8; 32] = change
        .manifest_hash
        .expect("a create carries a manifest")
        .try_into()
        .unwrap();
    assert!(
        store
            .exists(&fauna_core::data::ContentHash::from_digest_raw(manifest))
            .await
            .unwrap(),
        "the home store holds the manifest — metadata stays on the nest"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// The RELAY-side foreign-member grant is claimant-gated.
//
// `channel_foreign_members` is the cross-nest half of the one channel roster
// (`federation.md` § Cross-nest shared folders + channel append), and a row in
// it is an authorization grant, not a hint: `require_foreign_member` serves the
// peer's `channel.fetch` (the folder's application-message ciphertext) and
// `channel.actors` (its roster in the clear) off that row, and the push fan-out
// dials every home nest listed in it. The same-nest twin — `actor_channels` —
// has long been claimant-gated, but the relay branch of
// `welcome_deliver_core` wrote its grant unconditionally, so ANY member of a
// claimed folder channel (every member holds its id) could relay a Welcome to a
// puppet account on a nest of their choosing and hand that nest the folder's
// ciphertext stream, roster and push dial — owner-only share
// (`federation.md` § Security) bypassed in the metadata plane, with only MLS
// confidentiality left holding.
//
// (The same-nest gate arrived with the finding one gate over;
// the relay branch's doc comment then claimed an "identical gate" at its own
// site, naming a symbol that existed in neither module, for a site that had
// none.)
//
// This lives at tier_3 with two real in-process nests rather than in
// `conversations_handlers`' unit module on purpose: the relay branch dials the
// peer (`originate_welcome_deliver` → `resolve_peer_nest_id`) and returns before
// the grant on any dial failure, so a unit test — which can only send
// `nest_url: None` — would never reach the write and its "red" would pass
// vacuously.
//
// Three legs, one H/F pair: the abuse is refused, and neither honest path
// regresses (the claimant's folder relay still grants; an unclaimed DM relay,
// whose cross-nest delivery depends on the grant, still grants).
// ─────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn cross_nest_welcome_relay_gates_the_foreign_member_grant_by_claim() {
    use fauna_client_conversations::ConversationsClient;
    use fauna_protocol::conversations::WelcomeKind;

    let (h_base, _h_authority, h_state) = start_home_nest().await;
    let (f_base, _f_authority, f_state) = start_foreign_nest().await;

    // Alice claims a folder channel on H. Mallory is an ORDINARY MEMBER of it —
    // she knows the channel id, which is the whole of what the attack needs.
    // (Claim first, then seat Mallory: a pre-populated channel is unclaimable.)
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();
    let mut mallory_secret = [0u8; 32];
    getrandom::fill(&mut mallory_secret).unwrap();
    let mallory_id = ActorKeypair::from_secret(mallory_secret).actor_id();
    h_state
        .db
        .create_user(&mallory_id.0, "free", "mallory")
        .await
        .unwrap();

    let folder_channel = [0x7cu8; 32];
    h_state
        .db
        .claim_folder_channel(&alice_id.0, &folder_channel)
        .await
        .unwrap();
    assert_eq!(
        h_state
            .db
            .folder_channel_claimed_by(&folder_channel)
            .await
            .unwrap(),
        Some(alice_id.0),
        "fixture: alice must hold the folder channel's claim"
    );
    h_state
        .db
        .register_actor_channel(&mallory_id.0, &folder_channel)
        .await
        .unwrap();

    // Mallory's puppet on F, and Alice's honest cross-nest recipient. Both are
    // real users there, so the relay leg itself succeeds and ONLY the gate can
    // decide whether a grant lands.
    let mut puppet_secret = [0u8; 32];
    getrandom::fill(&mut puppet_secret).unwrap();
    let puppet_id = ActorKeypair::from_secret(puppet_secret).actor_id();
    f_state
        .db
        .create_user_with_handle(&puppet_id.0, "free", "puppet", None)
        .await
        .unwrap();
    let mut carol_secret = [0u8; 32];
    getrandom::fill(&mut carol_secret).unwrap();
    let carol_id = ActorKeypair::from_secret(carol_secret).actor_id();
    f_state
        .db
        .create_user_with_handle(&carol_id.0, "free", "carol", None)
        .await
        .unwrap();

    let mallory_nest = connected_client(&h_base, ActorKeypair::from_secret(mallory_secret)).await;
    let mallory_convs = ConversationsClient::new(Arc::clone(&mallory_nest));

    // ── ABUSE: a non-claimant member relays the claimed folder's Welcome to a
    // nest of their choosing. The relay itself is not an error — the Welcome is
    // opaque bytes and delivery stays best-effort — but no grant may land.
    mallory_convs
        .welcome_deliver(
            hex::encode(puppet_id.0),
            hex::encode(folder_channel),
            vec![],
            WelcomeKind::Folder {
                group_id: hex::encode(folder_channel),
            },
            Some(f_base.clone()),
        )
        .await
        .expect("the relay leg itself still runs — only the grant is gated");
    assert!(
        h_state
            .db
            .foreign_member_home_nest(&folder_channel, &puppet_id.0)
            .await
            .unwrap()
            .is_none(),
        "a NON-CLAIMANT relayed a Welcome on a claimed folder channel and won its \
         puppet nest a channel_foreign_members grant — owner-only share bypassed \
         in the metadata plane"
    );

    // ── HONEST 1: the claimant's own cross-nest share still grants. ──
    let alice_nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let alice_convs = ConversationsClient::new(Arc::clone(&alice_nest));
    alice_convs
        .welcome_deliver(
            hex::encode(carol_id.0),
            hex::encode(folder_channel),
            vec![],
            WelcomeKind::Folder {
                group_id: hex::encode(folder_channel),
            },
            Some(f_base.clone()),
        )
        .await
        .expect("the claimant's cross-nest folder share must relay");
    assert!(
        h_state
            .db
            .foreign_member_home_nest(&folder_channel, &carol_id.0)
            .await
            .unwrap()
            .is_some(),
        "the claimant's own cross-nest share lost its foreign-member grant — the \
         gate is too tight and cross-nest folder reads would break"
    );

    // ── HONEST 2: an UNCLAIMED channel (every DM / group / scheduling
    // conversation) still grants, from any caller. Cross-nest DM delivery
    // depends on it, and the initiating caller is on no roster yet — which is
    // why `Unclaimed` is permissive rather than caller-roster-checked. ──
    let dm_channel = [0x7du8; 32];
    mallory_convs
        .welcome_deliver(
            hex::encode(puppet_id.0),
            hex::encode(dm_channel),
            vec![],
            WelcomeKind::Dm,
            Some(f_base.clone()),
        )
        .await
        .expect("an unclaimed cross-nest DM welcome must relay");
    assert!(
        h_state
            .db
            .foreign_member_home_nest(&dm_channel, &puppet_id.0)
            .await
            .unwrap()
            .is_some(),
        "an unclaimed cross-nest DM welcome lost its foreign-member grant — the \
         gate touched ordinary conversation delivery, which depends on it"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// The interim cross-nest refusal, the member-moves-second direction
// (`file-sync.md` § Relay serving → *Until that leg is built, the pair is
// refused*): a Welcome for a recipient on another nest, on a channel claimed by
// a metadata-only folder, is refused BEFORE the relay — the peer nest receives
// nothing and no `channel_foreign_members` row lands. The flip-moves-second
// direction is pinned in `conformance_folder_residency`.
//
// Two real nests for the reason the claim-gate test above gives: only a real
// dial can show that the Welcome was never relayed.
// ─────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn cross_nest_welcome_onto_a_metadata_only_folder_is_refused_before_the_relay() {
    use fauna_client_conversations::ConversationsClient;
    use fauna_protocol::conversations::WelcomeKind;

    let (h_base, _h_authority, h_state) = start_home_nest().await;
    let (f_base, _f_authority, f_state) = start_foreign_nest().await;

    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();
    let mut carol_secret = [0u8; 32];
    getrandom::fill(&mut carol_secret).unwrap();
    let carol_id = ActorKeypair::from_secret(carol_secret).actor_id();
    f_state
        .db
        .create_user_with_handle(&carol_id.0, "free", "carol", None)
        .await
        .unwrap();

    // Two group-bound folders of Alice's on H, each claiming its derived
    // channel as `share_core` does: `vault` keeps its content off the nest,
    // `docs` is full.
    let mut channels = Vec::new();
    for name in ["vault", "docs"] {
        let raw_group_id = format!("raw-group-id-{name}");
        h_state.db.create_folder(name, &alice_id.0).await.unwrap();
        h_state
            .db
            .set_folder_mls_group(name, &alice_id.0, Some(raw_group_id.as_bytes()))
            .await
            .unwrap();
        let channel = fauna_mls::types::ChannelId::from_group_id(raw_group_id.as_bytes()).0;
        h_state
            .db
            .claim_folder_channel(&alice_id.0, &channel)
            .await
            .unwrap();
        channels.push((raw_group_id, channel));
    }
    assert!(
        h_state
            .db
            .update_folder_for_user(
                "vault",
                &alice_id.0,
                fauna_nest::db::FolderUpdate {
                    residency: Some(Some("metadata_only")),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        "fixture: vault is metadata-only"
    );
    let (vault_group, vault_channel) = channels[0].clone();
    let (docs_group, docs_channel) = channels[1].clone();

    let alice_nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let alice_convs = ConversationsClient::new(Arc::clone(&alice_nest));

    // ── REFUSED: the owner's own share to a member on another nest. ──
    let refused = alice_convs
        .welcome_deliver(
            hex::encode(carol_id.0),
            hex::encode(vault_channel),
            vec![],
            WelcomeKind::Folder {
                group_id: hex::encode(vault_group.as_bytes()),
            },
            Some(f_base.clone()),
        )
        .await
        .expect_err("a metadata-only folder cannot take a member on another nest yet");
    assert_eq!(
        refusal_code(&refused),
        "fauna.conversations.invalid_request"
    );
    assert!(
        f_state
            .db
            .list_inbox_all(&carol_id.0)
            .await
            .unwrap()
            .is_empty(),
        "the refused Welcome was relayed to the member's nest anyway"
    );
    assert!(
        h_state
            .db
            .foreign_member_home_nest(&vault_channel, &carol_id.0)
            .await
            .unwrap()
            .is_none(),
        "a refused Welcome wrote a channel_foreign_members row"
    );

    // ── STILL LANDS: the same Welcome on a full folder. ──
    alice_convs
        .welcome_deliver(
            hex::encode(carol_id.0),
            hex::encode(docs_channel),
            vec![],
            WelcomeKind::Folder {
                group_id: hex::encode(docs_group.as_bytes()),
            },
            Some(f_base.clone()),
        )
        .await
        .expect("a full folder's cross-nest share must still relay");
    assert_eq!(
        f_state.db.list_inbox_all(&carol_id.0).await.unwrap().len(),
        1,
        "the full folder's Welcome reached the member's nest"
    );
    assert!(
        h_state
            .db
            .foreign_member_home_nest(&docs_channel, &carol_id.0)
            .await
            .unwrap()
            .is_some(),
        "the full folder's cross-nest share lost its foreign-member grant"
    );

    // ── AN EXISTING PAIR is left as it is: a member the metadata-only folder
    // already holds keeps its in-band re-Welcomes. Seat Carol on `vault` behind
    // the refusal's back, and re-deliver. ──
    let carol_home = h_state
        .db
        .foreign_member_home_nest(&docs_channel, &carol_id.0)
        .await
        .unwrap()
        .expect("read above");
    h_state
        .db
        .register_foreign_channel_member(
            &vault_channel,
            &carol_id.0,
            &carol_home,
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();
    alice_convs
        .welcome_deliver(
            hex::encode(carol_id.0),
            hex::encode(vault_channel),
            vec![],
            WelcomeKind::Folder {
                group_id: hex::encode(vault_group.as_bytes()),
            },
            Some(f_base.clone()),
        )
        .await
        .expect("a member the folder already holds is not a new pair");
}

/// **The rebind is bounded by the inviter's power, not by knowledge of the
/// channel id** — `federation.md` § Cross-nest shared folders + channel append,
/// the *foreign-member binding is inviter-asserted (TOFU)* bullet.
///
/// That bullet accepts a residual: "read-DoS of the genuine member plus
/// last-writer-wins re-binding on re-invite — **all within the inviter's
/// existing power (they chose to add the member)**". The claimant gate
/// settled WHO MAY WRITE a grant; nothing settled WHAT A WRITE DOES TO A GRANT
/// THAT ALREADY EXISTS. On a conversation channel there is no
/// claimant to gate on, so `Unclaimed`'s permissive arm reached the UPDATE too:
/// anyone who had ever learned the 32-byte channel id — an MLS-removed
/// ex-member included, since removal advances the epoch and not the id — could
/// re-point an existing cross-nest member's `home_nest_id` at a nest they
/// control, and `require_foreign_member` would then serve that nest the
/// channel's ciphertext (`channel.fetch`), its roster in the clear
/// (`channel.actors`), appends, leaves and `folder.content_key.fetch`, while
/// refusing the genuine home nest on every one of them.
///
/// The permissive arm's own justification is an argument about the FIRST write
/// only — "a first DM Welcome's caller is on no roster yet, and its delivery
/// depends on the grant" — so insert stays open and only the rebind narrows to
/// a caller with standing on the channel.
#[tokio::test]
async fn cross_nest_welcome_relay_cannot_rebind_an_existing_foreign_member_grant() {
    use fauna_client_conversations::ConversationsClient;
    use fauna_protocol::conversations::WelcomeKind;

    let (h_base, _h_authority, h_state) = start_home_nest().await;
    // Carol's GENUINE home, and the nest Mallory controls. Two distinct peers is
    // what makes a rebind observable at all: with one, the honest and the
    // hostile binding are the same bytes.
    let (genuine_base, _g_authority, genuine_state) = start_foreign_nest().await;
    let (evil_base, _e_authority, evil_state) = start_foreign_nest().await;

    // Alice is a MEMBER of the conversation — the inviter whose power the
    // ratified bullet charges the residual to. Mallory knows the channel id and
    // nothing else: never on the roster, which is exactly where an MLS-removed
    // ex-member stands too.
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();
    let mut mallory_secret = [0u8; 32];
    getrandom::fill(&mut mallory_secret).unwrap();
    let mallory_id = ActorKeypair::from_secret(mallory_secret).actor_id();
    h_state
        .db
        .create_user(&mallory_id.0, "free", "mallory")
        .await
        .unwrap();

    let dm_channel = [0x7eu8; 32];
    h_state
        .db
        .register_actor_channel(&alice_id.0, &dm_channel)
        .await
        .unwrap();
    assert!(
        !h_state
            .db
            .is_actor_in_channel(&mallory_id.0, &dm_channel)
            .await
            .unwrap(),
        "fixture: mallory must hold the channel id WITHOUT standing on the channel"
    );

    // Carol is a real user on both peers, so each relay leg itself succeeds and
    // only the gate can decide what the grant says.
    let mut carol_secret = [0u8; 32];
    getrandom::fill(&mut carol_secret).unwrap();
    let carol_id = ActorKeypair::from_secret(carol_secret).actor_id();
    genuine_state
        .db
        .create_user_with_handle(&carol_id.0, "free", "carol", None)
        .await
        .unwrap();
    evil_state
        .db
        .create_user_with_handle(&carol_id.0, "free", "carol", None)
        .await
        .unwrap();

    let alice_nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let alice_convs = ConversationsClient::new(Arc::clone(&alice_nest));
    let mallory_nest = connected_client(&h_base, ActorKeypair::from_secret(mallory_secret)).await;
    let mallory_convs = ConversationsClient::new(Arc::clone(&mallory_nest));

    // ── The honest first invite: alice binds carol to her genuine home. ──
    alice_convs
        .welcome_deliver(
            hex::encode(carol_id.0),
            hex::encode(dm_channel),
            vec![],
            WelcomeKind::Dm,
            Some(genuine_base.clone()),
        )
        .await
        .expect("the member's cross-nest DM welcome must relay");
    let genuine_binding = h_state
        .db
        .foreign_member_home_nest(&dm_channel, &carol_id.0)
        .await
        .unwrap()
        .expect("fixture: the honest invite must have bound carol to her home nest");

    // ── ABUSE: mallory, holding only the channel id, relays a Welcome for the
    // SAME member at a nest she controls. The relay leg still runs (the Welcome
    // is opaque bytes and delivery stays best-effort) — but the existing
    // binding is not hers to move.
    mallory_convs
        .welcome_deliver(
            hex::encode(carol_id.0),
            hex::encode(dm_channel),
            vec![],
            WelcomeKind::Dm,
            Some(evil_base.clone()),
        )
        .await
        .expect("the relay leg itself still runs — only the rebind is gated");
    assert_eq!(
        h_state
            .db
            .foreign_member_home_nest(&dm_channel, &carol_id.0)
            .await
            .unwrap(),
        Some(genuine_binding),
        "a caller with no standing on the channel re-pointed an EXISTING \
         foreign-member grant at a nest of their choosing — that nest now serves \
         the channel's ciphertext, roster and content keys in the genuine home \
         nest's place"
    );

    // ── HONEST 1: first registration is untouched. A caller on no roster is
    // exactly the first-DM-initiation case the permissive arm exists for, and
    // an INSERT has no grant to overwrite.
    let fresh_channel = [0x7fu8; 32];
    mallory_convs
        .welcome_deliver(
            hex::encode(carol_id.0),
            hex::encode(fresh_channel),
            vec![],
            WelcomeKind::Dm,
            Some(evil_base.clone()),
        )
        .await
        .expect("an unclaimed cross-nest DM welcome must relay");
    assert!(
        h_state
            .db
            .foreign_member_home_nest(&fresh_channel, &carol_id.0)
            .await
            .unwrap()
            .is_some(),
        "the narrowing reached the INSERT — cross-nest DM initiation depends on \
         a caller who is on no roster yet winning the first grant"
    );

    // ── HONEST 2: the inviter's own re-invite still rebinds, which is the
    // migrated-nests flow the writer's contract names.
    alice_convs
        .welcome_deliver(
            hex::encode(carol_id.0),
            hex::encode(dm_channel),
            vec![],
            WelcomeKind::Dm,
            Some(evil_base.clone()),
        )
        .await
        .expect("the member's re-invite must relay");
    assert_ne!(
        h_state
            .db
            .foreign_member_home_nest(&dm_channel, &carol_id.0)
            .await
            .unwrap(),
        Some(genuine_binding),
        "a MEMBER's re-invite could not move the binding — the gate is too \
         tight and a member who migrates nests can never be re-bound"
    );
}

/// **First authenticated use pins the binding — the ex-member residual is
/// closed** (`federation.md` § Cross-nest shared folders + channel append,
/// the TOFU bullet's two-layer bound).
///
/// The standing gate above bounds the rebind to "a rostered actor" — but on
/// the conversation rail the roster is **monotone**: the nest never observes
/// an MLS removal (an encrypted commit advances the epoch and touches no nest
/// row), and nothing deletes conversation-rail roster rows, so standing can
/// only ever attest *was ever rostered* — a predicate the MLS-removed
/// ex-member, the attacker the bullet names, passes forever. Alice here IS
/// that attacker's shape: rostered once, never evictable by anything the nest
/// can see (HONEST 2 in the test above proves her standing still moves an
/// UNCONFIRMED grant — the healing window). Once Carol's bound home nest has
/// exercised the grant, that same standing must stop moving it.
#[tokio::test]
async fn a_confirmed_binding_refuses_even_a_rostered_rebind() {
    use fauna_client_conversations::ConversationsClient;
    use fauna_protocol::conversations::WelcomeKind;

    let (h_base, _h_authority, h_state) = start_home_nest().await;
    let (genuine_base, _g_authority, genuine_state) = start_foreign_nest().await;
    let (evil_base, _e_authority, evil_state) = start_foreign_nest().await;

    // Alice: a rostered member of the conversation — to the nest,
    // indistinguishable from an MLS-removed ex-member for the rest of time.
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();
    let dm_channel = [0x7du8; 32];
    h_state
        .db
        .register_actor_channel(&alice_id.0, &dm_channel)
        .await
        .unwrap();

    // Carol is a real user on both peers, so each relay leg itself succeeds
    // and only the gate can decide what the grant says.
    let mut carol_secret = [0u8; 32];
    getrandom::fill(&mut carol_secret).unwrap();
    let carol_id = ActorKeypair::from_secret(carol_secret).actor_id();
    genuine_state
        .db
        .create_user_with_handle(&carol_id.0, "free", "carol", None)
        .await
        .unwrap();
    evil_state
        .db
        .create_user_with_handle(&carol_id.0, "free", "carol", None)
        .await
        .unwrap();

    let alice_nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let alice_convs = ConversationsClient::new(Arc::clone(&alice_nest));

    // ── The honest first invite binds Carol to her genuine home. ──
    alice_convs
        .welcome_deliver(
            hex::encode(carol_id.0),
            hex::encode(dm_channel),
            vec![],
            WelcomeKind::Dm,
            Some(genuine_base.clone()),
        )
        .await
        .expect("the member's cross-nest DM welcome must relay");
    let genuine_binding = h_state
        .db
        .foreign_member_home_nest(&dm_channel, &carol_id.0)
        .await
        .unwrap()
        .expect("fixture: the honest invite must have bound carol to her home nest");

    // ── Carol's home nest exercises the grant: pinned. The stamp is driven
    // by a REAL relayed federated fetch in
    // `bob_receives_alices_cross_nest_message_through_the_relay` and
    // unit-pinned at the `require_foreign_member` seam; the db-level confirm
    // here isolates the WRITER's conflict arm, which is this test's subject.
    h_state
        .db
        .confirm_foreign_member(&dm_channel, &carol_id.0, &genuine_binding)
        .await
        .unwrap();

    // ── The probe, inverted: alice — rostered, which is all an
    // ex-member ever looks like to the nest — relays a Welcome for the SAME
    // member at a nest she controls. Pre-pin, this MOVED the binding. ──
    alice_convs
        .welcome_deliver(
            hex::encode(carol_id.0),
            hex::encode(dm_channel),
            vec![],
            WelcomeKind::Dm,
            Some(evil_base.clone()),
        )
        .await
        .expect("the relay leg itself still runs — only the rebind is gated");
    assert_eq!(
        h_state
            .db
            .foreign_member_home_nest(&dm_channel, &carol_id.0)
            .await
            .unwrap(),
        Some(genuine_binding),
        "a rostered actor moved a CONFIRMED foreign-member grant — the \
         first-use pin is not holding, and the MLS-removed ex-member the TOFU \
         bullet names is back inside the rebind gate"
    );
}

/// A zero-grace blob GC sweep on one nest — the admin `fauna.admin.gc` door's
/// exact call (`admin_ws_handlers.rs::gc_handler`), driven in-process.
async fn zero_grace_gc(state: &Arc<AppState>) -> fauna_nest::backup::gc::GcResult {
    let backup_svc = state.backup_service.as_ref().expect("backup service");
    let blob_store = backup_svc.local_blob_store();
    fauna_nest::backup::gc::garbage_collect(
        &state.db,
        &blob_store,
        fauna_nest::backup::gc::PostBodySource {
            segments: &state.post_segments,
        },
        0,
        backup_svc.encryption_key(),
        false,
    )
    .await
    .expect("gc sweep")
}

/// Every blob hash a nest's store currently holds.
async fn stored_blob_hashes(state: &Arc<AppState>) -> Vec<fauna_core::data::ContentHash> {
    state
        .backup_service
        .as_ref()
        .expect("backup service")
        .local_blob_store()
        .list_all_hashes()
        .await
        .expect("list blobs")
}

/// **A room's attachment bytes rest on the room's HOME nest** — the residency
/// ruling of `conversation-rooms.md` § The home nest → *Attachment bytes*
/// (ratified 2026-09-09), proven through the production client stack over two
/// real nests.
///
/// Before this ruling `blob_put` / `blob_get` read the member's OWN nest
/// whatever the channel's home, so a foreign member's attachment bytes landed
/// on a nest where no record named them (swept ~30 min later) and every other
/// member's fetch 404ed on a nest that never held them. Now:
///
/// 1. Bob (on F) sends an attachment in the channel homed on H: the sealed
///    bytes POST DIRECT to H under a write token F relayed from H
///    (`fauna.conversations.blob.write_token.get` →
///    `fauna.federation.conversation.write_token.mint`, behind H's structural
///    foreign-member gate), and the record + its plaintext `attachment_refs`
///    land on H's log through `send_remote` → `channel.append`. **F's store
///    holds nothing.**
/// 2. Alice (on H) drains and opens the attachment — the same-nest read.
/// 3. A zero-grace GC on H AND on F leaves the bytes exactly where they were:
///    H pins them by the record (step 2g), F had nothing to sweep.
/// 4. Bob's own foreign read — the path every OTHER foreign member (a member on
///    a third nest C) takes — fetches the bytes direct from H.
/// 5. A stranger's mint through the same relay is refused by H's gate.
#[tokio::test]
async fn cross_nest_attachment_bytes_rest_on_the_home_nest_and_survive_gc_through_client_stack() {
    let (h_base, h_authority, h_state) = start_home_nest().await;
    let (f_base, f_authority, f_state) = start_foreign_nest().await;

    // Bob lives on F: real MLS engine, registered handle, one published key
    // package (so he is `addressable` + can join the relayed Welcome).
    let mut bob_secret = [0u8; 32];
    getrandom::fill(&mut bob_secret).unwrap();
    let bob_id = ActorKeypair::from_secret(bob_secret).actor_id();
    f_state
        .db
        .create_user_with_handle(&bob_id.0, "free", "bob", None)
        .await
        .unwrap();
    let bob_engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(bob_secret)).expect("engine"));
    let bob_pkgs = bob_engine.generate_key_packages_bytes(1).expect("bob KP");
    f_state
        .db
        .put_key_package("bob-kp-0", &bob_id.0, &bob_pkgs[0], 0, FAR_FUTURE)
        .await
        .unwrap();

    // Alice lives on H.
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();
    let alice_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).expect("engine"),
    );

    // Both in-process nests serve the self-signed floor cert, and a foreign
    // member holds no pin for a room's home nest (production: plain WebPKI
    // against a real deployment) — so Bob's foreign byte-plane client accepts
    // the floor cert here, exactly as the shared-folder capstone's does.
    let danger_http = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();

    let alice_nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let alice_backend = Arc::new(FaunaMlsBackend::new(
        alice_engine.clone(),
        Arc::new(NestConversationsRpc::new(Arc::clone(&alice_nest))) as Arc<dyn ConversationsRpc>,
        format!("alice@{h_authority}"),
        alice_id,
    ));
    let bob_nest = connected_client(&f_base, ActorKeypair::from_secret(bob_secret)).await;
    let bob_rpc = Arc::new(NestConversationsRpc::with_foreign_http(
        Arc::clone(&bob_nest),
        danger_http.clone(),
    ));
    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob_engine.clone(),
        Arc::clone(&bob_rpc) as Arc<dyn ConversationsRpc>,
        format!("bob@{f_authority}"),
        bob_id,
    ));
    bob_manager.register_backend(bob_backend.clone());

    // ── Alice opens the channel (homed on H) and Bob joins from the relayed
    //    Welcome, recording H as the channel's home. ──
    let chip = match alice_backend
        .resolve_address(&format!("bob@{f_authority}"))
        .await
    {
        ResolveResult::Resolved(addr @ TypedAddress::Fauna { .. }) => addr,
        other => panic!("expected Resolved(Fauna), got {other:?}"),
    };
    let thread = one_to_one_thread(ThreadId("t-xnest-att".into()), chip.clone());
    alice_backend
        .send(
            &thread,
            &ComposeState {
                body_draft: "opening".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("cross-nest bootstrap send");
    let inbox = f_state.db.list_inbox_all(&bob_id.0).await.unwrap();
    assert_eq!(inbox.len(), 1, "exactly one welcome relayed to Bob on F");
    let welcome = welcome_bytes_from_inbox(&inbox[0].1);
    let channel_hex = alice_backend.bound_channels()[0].to_string();
    let bob_thread = ingest_welcome(&bob_backend, &bob_manager, &channel_hex, &welcome, &h_base)
        .await
        .expect("bob joins the cross-nest group from the relayed Welcome");
    let bob_channel = bob_backend.bound_channels()[0];
    let mut bob_after = 0i64;
    poll_inbound_conv(&bob_backend, &bob_manager, &bob_channel, &mut bob_after, 0)
        .await
        .expect("bob drains the opening message");
    assert_eq!(
        stored_blob_hashes(&h_state).await.len() + stored_blob_hashes(&f_state).await.len(),
        0,
        "no blob anywhere before the attachment send"
    );

    // ── (1) Bob, the FOREIGN member, sends an attachment. ──
    let png: &[u8] = b"\x89PNG\r\n\x1a\nbytes-from-the-foreign-member";
    let blob_hash = hex::encode(blake3::hash(png).as_bytes());
    let bob_detail = bob_manager.thread_detail(bob_thread).expect("bob thread");
    bob_backend
        .send(
            &bob_detail,
            &ComposeState {
                body_draft: "from bob's nest".into(),
                ..Default::default()
            },
            &[ResolvedAttachment {
                blob_hash: blob_hash.clone(),
                filename: "pic.png".into(),
                mime_type: "image/png".into(),
                is_image: true,
                bytes: png.to_vec(),
            }],
        )
        .await
        .expect("bob's cross-nest attachment send: token relay + direct POST to H + send_remote");

    let on_h = stored_blob_hashes(&h_state).await;
    assert_eq!(
        on_h.len(),
        1,
        "the sealed attachment rests on the room's HOME nest H"
    );
    assert!(
        stored_blob_hashes(&f_state).await.is_empty(),
        "the uploader's own nest F holds no copy — the bytes never went there"
    );
    let sealed = on_h[0];
    let sealed_cid_hex = hex::encode(sealed.digest());
    let h_store = h_state.backup_service.as_ref().unwrap().local_blob_store();
    assert_ne!(
        h_store.get(&sealed).await.unwrap().as_deref(),
        Some(png),
        "H holds the SEALED bytes, never the plaintext"
    );

    // ── (2) Alice drains on H and opens the attachment — the same-nest read. ──
    let alice_channel = alice_backend.bound_channels()[0];
    let alice_drain = Arc::new(FaunaMlsBackend::new(
        alice_engine.clone(),
        Arc::new(NestConversationsRpc::new(Arc::clone(&alice_nest))) as Arc<dyn ConversationsRpc>,
        format!("alice@{h_authority}"),
        alice_id,
    ));
    let alice_manager = ConversationsManager::new();
    alice_manager.register_backend(alice_drain.clone());
    let alice_thread =
        alice_manager.materialize_conv_thread(alice_channel.to_string(), vec![chip.clone()]);
    alice_drain.bind_channel(alice_thread.clone(), alice_channel);
    let mut a_after = 0i64;
    poll_inbound_conv(
        &alice_drain,
        &alice_manager,
        &alice_channel,
        &mut a_after,
        0,
    )
    .await
    .expect("alice drains her home log");
    let a_detail = alice_manager
        .thread_detail(alice_thread.clone())
        .expect("alice thread");
    let att = a_detail
        .messages
        .iter()
        .flat_map(|m| fauna_conversations::message::attachment_blocks(&m.document))
        .find(|a| a.blob_hash == blob_hash)
        .expect("Bob's attachment rendered in Alice's thread");
    assert_eq!(att.filename, "pic.png");
    assert_eq!(
        alice_manager.attachment_bytes(blob_hash.clone()).as_deref(),
        Some(png),
        "Alice opened the bytes she fetched from her own (the home) nest"
    );

    // ── (3) Zero-grace GC on BOTH nests: H pins the bytes by the record's
    //    `attachment_refs` (step 2g), F had nothing of the room's to sweep. ──
    let h_gc = zero_grace_gc(&h_state).await;
    let f_gc = zero_grace_gc(&f_state).await;
    assert!(
        h_store.exists(&sealed).await.unwrap(),
        "H's GC kept the attachment a live record names (deleted: {}, grace-skipped: {})",
        h_gc.deleted_blobs,
        h_gc.skipped_grace_period,
    );
    assert_eq!(
        f_gc.deleted_blobs, 0,
        "F had no attachment bytes to reclaim — they never rested there"
    );

    // ── (4) The foreign read: Bob (and any member on a third nest C, who takes
    //    exactly this path) fetches the bytes DIRECT from the home nest. ──
    let fetched = bob_rpc
        .blob_get(
            channel_hex.clone(),
            Some(h_base.clone()),
            sealed_cid_hex.clone(),
        )
        .await
        .expect("foreign blob_get transport ok")
        .expect("the bytes are present on the home nest");
    assert_eq!(
        hex::encode(blake3::hash(&fetched).as_bytes()),
        sealed_cid_hex,
        "the foreign read returns the sealed bytes under their content address"
    );
    assert!(
        bob_rpc
            .blob_get(channel_hex.clone(), None, sealed_cid_hex.clone())
            .await
            .expect("own-nest blob_get transport ok")
            .is_none(),
        "Bob's OWN nest never held the bytes — only the home-routed read finds them"
    );

    // ── (5) The gate: a stranger's mint through the same relay is refused by
    //    H's structural foreign-member check. ──
    let stranger = [0x5au8; 32];
    let refused = fauna_nest::federation_pool::originate_conversation_write_token_mint(
        &f_state.federation_pool,
        &f_state,
        &h_base,
        &hex::encode(stranger),
        &channel_hex,
    )
    .await
    .expect("the federation channel itself is healthy")
    .expect_err("a non-member's write-token mint is refused by the home nest");
    assert_eq!(
        refused.code, "fauna.federation.forbidden",
        "the mint rides the `channel.fetch` gate verbatim"
    );

    let _ = (h_state, f_state, a_detail);
}

/// A nest bound on a port that an earlier, now-gone nest held is met as a NEW
/// nest. This process's identity pin for that authority still names the gone
/// nest, as an earlier test's login in this binary leaves it, and without
/// [`common::fresh_nest_listener`] the connect fails `NestIdentityChanged`.
/// The CI runner's port recycling made that happen by chance; this test makes
/// it happen every time, by pinning a foreign identity on the very port the
/// new nest is about to serve.
#[tokio::test]
async fn a_nest_on_a_port_a_gone_nest_held_is_met_as_a_new_nest() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    // The gone nest's pin. Any identity but the new nest's stands in for it.
    fauna_anon_client::trust::seed_claimed_identity_pin(
        &format!("https://127.0.0.1:{port}"),
        [0x55; 32],
    );

    let (listener, authority) = common::fresh_nest_listener(listener);
    let (h_base, _, h_state) = start_home_nest_on(listener, authority).await;
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();

    connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
}
