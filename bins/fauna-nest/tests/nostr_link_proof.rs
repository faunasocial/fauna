#![cfg(feature = "nostr")]
//! Tier_3 — proof of possession for a `remote` (NIP-46) Nostr account link
//! (`docs/goal/ui/nostr.md` § Errors & edge cases → *Proof of possession*).
//!
//! The bunker under test is the nest's OWN bunker (`nostr::bunker`, the
//! richest signer in the tree), hosted by a first nest on a bound `/nostr`
//! relay: a custodial user there mints an invite, and its `bunker://…&secret=`
//! string is what a user of a SECOND nest pastes into the `remote` link form.
//! The second nest must complete the real handshake over the real relay —
//! `connect` with the secret, `get_public_key`, `sign_event` over its own
//! challenge — and link the pubkey the SIGNATURE names:
//!
//!  * the linked row carries the bunker user's key, not the signer pubkey the
//!    URL spells (signer ≠ user by NIP-46 design), and the bunker URL at rest
//!    has lost its one-time secret;
//!  * a wrong invite secret is `proof_required` and writes nothing;
//!  * an unreachable relay is `provider_error` and writes nothing.
//!
//! The `nip07` arm's proof is pinned by `bridge_provider.rs`'s unit tests.
//! Only compiled under `--features nostr`.

use std::collections::BTreeMap;
use std::sync::Arc;

use fauna_bridge_nostr::signing::Keypair;
use fauna_nest::bridge_management::BridgeProvider;
use fauna_nest::nostr::bridge_provider::NostrProvider;
use fauna_nest::nostr::key_crypto::encrypt_nostr_privkey;
use fauna_nest::nostr::{self, bunker, db};
use fauna_nest::routes::AppState;
use fauna_protocol::Value;

/// Nest 1: a bound relay whose domain is its own `addr`, so NIP-42 and the
/// bunker's connect string both name a reachable host.
struct BoundRelay {
    state: Arc<AppState>,
    url: String,
}

async fn spawn_relay() -> BoundRelay {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    nostr::init_db(&db).await.expect("init nostr tables");
    let base = AppState::for_test(db);
    fauna_nest::test_support::seat_own_deployment_seed(&base).await;
    let mut config = (*base.config).clone();
    config.nest.domain = Some(format!("{addr}"));
    let state = Arc::new(AppState {
        config: Arc::new(config),
        ..base
    });

    let router = nostr::routes().with_state(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    BoundRelay {
        state,
        url: format!("ws://{addr}/nostr"),
    }
}

/// Nest 2: where the `remote` link happens. No relay of its own is needed —
/// it dials nest 1's.
async fn linking_nest() -> AppState {
    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    nostr::init_db(&db).await.expect("init nostr tables");
    let base = AppState::for_test(db);
    fauna_nest::test_support::seat_own_deployment_seed(&base).await;
    // The bunker's relay is nest 1, bound on 127.0.0.1, which the production
    // dial policy refuses outright; this dependency build carries neither
    // `test-hooks` nor the e2e env, so the loopback allowance is set on the
    // linking nest's own state.
    AppState {
        nostr: fauna_nest::state::NostrState {
            relay_dial_policy: fauna_bridge_nostr::relay_client::RelayDialPolicy::PublicOrLoopback,
            ..Default::default()
        },
        ..base
    }
}

/// A custodial user on nest 1 (deposited nsec) — the bunker signs as this key.
async fn link_custodial(state: &AppState, actor_hex: &str) -> Keypair {
    let nest_key = state.nest_identity.signing_key.to_bytes();
    let kp = Keypair::generate();
    let encrypted = encrypt_nostr_privkey(&nest_key, &kp.secret_bytes()).unwrap();
    let conn = state.db.conn().await;
    db::link_account(
        &conn,
        actor_hex,
        &kp.public_key_hex(),
        "generated",
        Some(&encrypted),
        None,
        None,
    )
    .unwrap();
    kp
}

/// The `bunker://` string nest 1 would hand its user for a third-party app —
/// the same shape `fauna.nostr.bunker.create_invite` composes.
async fn bunker_url(relay: &BoundRelay, actor_hex: &str, secret: Option<&str>) -> String {
    let now = fauna_core::data::Timestamp::now_secs() as u64;
    let conn = relay.state.db.conn().await;
    let inv = bunker::create_invite(&conn, actor_hex, now).unwrap();
    let secret = secret.unwrap_or(&inv.secret);
    format!(
        "bunker://{}?relay={}&secret={secret}",
        inv.signer_pubkey, relay.url
    )
}

fn remote_params(bunker_url: &str) -> Value {
    Value::Map(BTreeMap::from([(
        "bunker_url".to_string(),
        Value::String(bunker_url.to_string()),
    )]))
}

const BUNKER_USER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const LINKER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[tokio::test]
async fn a_remote_link_completes_the_handshake_and_links_the_proven_user_key() {
    let relay = spawn_relay().await;
    let user_kp = link_custodial(&relay.state, BUNKER_USER).await;
    let url = bunker_url(&relay, BUNKER_USER, None).await;
    let signer_pubkey = url["bunker://".len()..]
        .split('?')
        .next()
        .unwrap()
        .to_string();
    assert_ne!(
        signer_pubkey,
        user_kp.public_key_hex(),
        "the invite names the signer key, not the user key"
    );

    let nest2 = linking_nest().await;
    let reply = NostrProvider
        .link(&nest2, LINKER, "remote", remote_params(&url))
        .await
        .expect("a reachable bunker that signs the challenge links");
    assert!(reply.linked);

    let conn = nest2.db.conn().await;
    let row = db::get_account(&conn, LINKER)
        .unwrap()
        .expect("row written");
    assert_eq!(
        row.nostr_pubkey,
        user_kp.public_key_hex(),
        "the row carries the key the signature proved — the bunker USER's, not the signer's"
    );
    assert_eq!(row.signing_mode, "remote");
    assert!(
        row.encrypted_privkey.is_none(),
        "a remote link deposits nothing"
    );
    let stored = row.nip46_bunker_url.expect("bunker URL kept");
    assert!(
        !stored.contains("secret="),
        "the one-time secret does not rest in the row: {stored}"
    );
    assert!(stored.starts_with(&format!("bunker://{signer_pubkey}?relay=")));
}

#[tokio::test]
async fn a_wrong_invite_secret_is_proof_required_and_writes_nothing() {
    let relay = spawn_relay().await;
    link_custodial(&relay.state, BUNKER_USER).await;
    let url = bunker_url(&relay, BUNKER_USER, Some("not-the-secret")).await;

    let nest2 = linking_nest().await;
    let err = NostrProvider
        .link(&nest2, LINKER, "remote", remote_params(&url))
        .await
        .expect_err("the bunker refuses connect, so nothing is proven");
    assert_eq!(err.code, "proof_required", "{}", err.error);

    let conn = nest2.db.conn().await;
    assert!(db::get_account(&conn, LINKER).unwrap().is_none());
}

#[tokio::test]
async fn an_unreachable_relay_is_a_provider_error_and_writes_nothing() {
    // Port 1 is reserved and never listening on a dev box. The loopback
    // literal passes the store-time relay check only because this linking
    // nest runs `PublicOrLoopback` (F7(c)); under the production policy it is
    // `invalid_params` before any dial (`bridge_provider.rs::relay_store_guard`).
    let url = format!(
        "bunker://{}?relay=ws://127.0.0.1:1/nostr&secret=whatever",
        Keypair::generate().public_key_hex()
    );
    let nest2 = linking_nest().await;
    let err = NostrProvider
        .link(&nest2, LINKER, "remote", remote_params(&url))
        .await
        .expect_err("no relay, no handshake");
    assert_eq!(err.code, "provider_error", "{}", err.error);

    let conn = nest2.db.conn().await;
    assert!(db::get_account(&conn, LINKER).unwrap().is_none());
}

/// One pubkey, one account holds for a PROVEN remote link too: the bunker
/// signs honestly as its user, but that user's key is already another account's
/// on the linking nest, so the writer refuses and the holder keeps its row.
#[tokio::test]
async fn a_proven_remote_key_another_account_holds_is_identity_in_use() {
    let relay = spawn_relay().await;
    let user_kp = link_custodial(&relay.state, BUNKER_USER).await;
    let url = bunker_url(&relay, BUNKER_USER, None).await;

    let nest2 = linking_nest().await;
    const HOLDER: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    {
        let conn = nest2.db.conn().await;
        db::link_account(
            &conn,
            HOLDER,
            &user_kp.public_key_hex(),
            "nip07",
            None,
            None,
            None,
        )
        .unwrap();
    }

    let err = NostrProvider
        .link(&nest2, LINKER, "remote", remote_params(&url))
        .await
        .expect_err("a key another account holds is refused even with a valid proof");
    assert_eq!(err.code, "identity_in_use", "{}", err.error);

    let conn = nest2.db.conn().await;
    assert!(db::get_account(&conn, LINKER).unwrap().is_none());
    let holder = db::get_account(&conn, HOLDER)
        .unwrap()
        .expect("holder survives");
    assert_eq!(holder.nostr_pubkey, user_kp.public_key_hex());
}
