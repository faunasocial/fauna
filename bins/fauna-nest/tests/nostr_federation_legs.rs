//! **Nostr proxy-delegation federation legs — tier_3 wire proof (spec P2.3/P2.4).**
//! Two in-process nests over the real federation WS-RPC channel: a keyless public
//! *serving* box `PUB` and a paired *head* `HEAD` holding the deposited nsec. The
//! head's `nest_sync_worker::relay_actor_nostr` pushes its own `origin='ingest'`
//! rows public-ward over `fauna.federation.sync.nostr_push` (the public box
//! auto-provisions the proxied account, spec R9 (account-data-plane.md § The ratified decisions), and stores each `origin=
//! 'federation'`), then pulls the public box's externally-deposited rows head-ward
//! over `fauna.federation.sync.nostr_pull`.
//!
//! This is the wire-level companion to the in-crate handler tests
//! (`federation_handlers::nostr_fed_tests`, which cover the serving side directly
//! without the channel). The kind-1059 gift-wrap seal round-trip and the serving-
//! predicate flip are the broader tier_3 slice P2.8
//! (`conformance_nostr_proxy_delegation.rs`); this file proves the two legs'
//! happy path + the capability gate over the real channel, with class-1 events.
//!
//! Goal: `docs/goal/ui/nostr.md` § The bridging gate → Phase 2 (R4/R5/R6/R9).

#![cfg(feature = "nostr")]

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_bridge_nostr::signing::Keypair;
use fauna_bridge_nostr::types::{Event, UnsignedEvent};
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::nostr;
use fauna_nest::nostr::db as nostr_db;
use fauna_nest::nostr::key_crypto::encrypt_nostr_privkey;
use fauna_nest::routes::AppState;

/// Spin a real in-process nest (its full router, incl. `/api/v1/federation/ws`)
/// on a loopback socket with a distinct nest identity, with the Nostr tables
/// initialised. Modeled on `conformance_cross_nest_mail_relay.rs::start_nest`:
/// `for_test`'s routers are empty, so we register the federation handlers (the
/// `nostr_push`/`nostr_pull` serving side) and the anon discovery handlers (peer
/// `nest_id` resolution the channel pool needs before dialing).
async fn start_nest() -> (String, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    nostr::init_db(&db).await.expect("init nostr tables");
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity {
            signing_key,
            verifying_key,
        }),
        federation_router: Arc::new({
            let mut b = fauna_nest::federation_router::FederationRouter::builder();
            fauna_nest::federation_handlers::register_federation_handlers(&mut b);
            b.build()
        }),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        ..AppState::for_test(db)
    });
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), state)
}

fn keypair(seed: u8) -> Keypair {
    Keypair::from_secret_bytes([seed.max(1); 32]).unwrap()
}
fn pubkey_hex(kp: &Keypair) -> String {
    hex::encode(kp.public_key_bytes())
}
fn signed(kp: &Keypair, kind: u64, created_at: u64, content: &str) -> Event {
    kp.sign_event(UnsignedEvent {
        pubkey: kp.public_key_bytes(),
        created_at,
        kind,
        tags: vec![],
        content: content.to_string(),
    })
}

/// Read a stored event's `origin` on a box, or `None` if absent.
async fn origin_of(state: &Arc<AppState>, id: &str) -> Option<String> {
    let conn = state.db.conn().await;
    conn.prepare("SELECT origin FROM nostr_events WHERE id = ?1")
        .unwrap()
        .query_row([id], |r| r.get::<_, String>(0))
        .ok()
}

/// **The two legs over the real channel.** HEAD holds a deposited nsec + one
/// locally-ingested authored event; PUB authorizes HEAD with `nostr_push`. One
/// `relay_actor_nostr` cycle pushes the event to PUB (auto-provisioning the
/// proxied account, storing it `origin='federation'`) and pulls nothing back (the
/// pushed row is `origin='federation'` — never re-exported, spec R4). Then an
/// externally-deposited `origin='ingest'` event lands on PUB; a second cycle pulls
/// it head-ward and HEAD ingests it `origin='federation'`. Cursors are DB-persisted
/// and idempotent (a third cycle moves nothing).
#[tokio::test]
async fn head_pushes_ingest_rows_and_pulls_external_deposits() {
    let (pub_url, pub_state) = start_nest().await;
    let (_head_url, head_state) = start_nest().await;
    let head_nest_id = head_state.nest_identity.public_key_bytes();

    let actor = [0x42u8; 32];
    let actor_hex = hex::encode(actor);
    let kp = keypair(7);
    let pubkey = pubkey_hex(&kp);

    // HEAD: deposit the actor's nsec (custodial) + one locally-authored ingest row.
    let head_authored = signed(&kp, 1, 1000, "authored on the head");
    {
        let nest_key = head_state.nest_identity.signing_key.to_bytes();
        let enc = encrypt_nostr_privkey(&nest_key, &kp.secret_bytes()).unwrap();
        let conn = head_state.db.conn().await;
        nostr_db::link_account(
            &conn,
            &actor_hex,
            &pubkey,
            "custodial",
            Some(&enc),
            None,
            None,
        )
        .unwrap();
        nostr::store::store_event(&conn, &head_authored, false).unwrap();
    }
    // HEAD also holds a local pairing row for the actor (its worker fires per
    // `list_pairing_targets`), and PUB authorizes HEAD with `nostr_push`.
    let pub_nest_id = pub_state.nest_identity.public_key_bytes();
    head_state
        .db
        .store_pairing(
            &actor,
            &pub_nest_id,
            &fauna_protocol::pair::default_self_sync(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    pub_state
        .db
        .store_pairing(
            &actor,
            &head_nest_id,
            &[fauna_protocol::pair::capability::NOSTR_PUSH.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    // ── Cycle 1: push delivers the authored row; pull returns nothing. ──
    let n =
        fauna_nest::nest_sync_worker::relay_actor_nostr(&head_state, &pub_url, &actor, &actor_hex)
            .await;
    assert_eq!(n, 1, "one event pushed public-ward this cycle");

    // PUB auto-provisioned the proxied account (keyless).
    {
        let conn = pub_state.db.conn().await;
        let acct = nostr_db::get_account(&conn, &actor_hex).unwrap().unwrap();
        assert_eq!(acct.signing_mode, "proxied");
        assert_eq!(acct.nostr_pubkey, pubkey);
        assert!(
            acct.encrypted_privkey.is_none(),
            "no key crosses the wire (R11)"
        );
    }
    // The pushed row rests on PUB as ORIGIN_FEDERATION.
    assert_eq!(
        origin_of(&pub_state, &head_authored.id).await.as_deref(),
        Some(nostr::store::ORIGIN_FEDERATION)
    );
    // HEAD's push cursor advanced; the pull cursor is untouched (pull found none).
    {
        let conn = head_state.db.conn().await;
        let c = nostr_db::get_federation_cursors(&conn, &actor_hex, &hex::encode(pub_nest_id))
            .unwrap()
            .unwrap();
        assert!(
            c.push_id == head_authored.id,
            "push cursor at the delivered row"
        );
        assert_eq!((c.pull_stored_at, c.pull_id.as_str()), (0, ""));
    }

    // ── An external client deposits an ingest row directly on PUB. ──
    let external = signed(
        &kp,
        1,
        2000,
        "published by the user's phone to the public relay",
    );
    {
        let conn = pub_state.db.conn().await;
        nostr::store::store_event(&conn, &external, false).unwrap(); // origin='ingest'
    }

    // ── Cycle 2: push moves nothing new; pull ingests the external row. ──
    fauna_nest::nest_sync_worker::relay_actor_nostr(&head_state, &pub_url, &actor, &actor_hex)
        .await;
    // HEAD ingested the external row as ORIGIN_FEDERATION.
    assert_eq!(
        origin_of(&head_state, &external.id).await.as_deref(),
        Some(nostr::store::ORIGIN_FEDERATION),
        "the external deposit was pulled head-ward"
    );
    // HEAD's own authored row is still ORIGIN_INGEST on the head (never mutated).
    assert_eq!(
        origin_of(&head_state, &head_authored.id).await.as_deref(),
        Some(nostr::store::ORIGIN_INGEST)
    );

    // ── Cycle 3: idempotent — nothing new moves either way. ──
    let head_events_before = {
        let conn = head_state.db.conn().await;
        conn.prepare("SELECT COUNT(*) FROM nostr_events")
            .unwrap()
            .query_row([], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    let pub_events_before = {
        let conn = pub_state.db.conn().await;
        conn.prepare("SELECT COUNT(*) FROM nostr_events")
            .unwrap()
            .query_row([], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    fauna_nest::nest_sync_worker::relay_actor_nostr(&head_state, &pub_url, &actor, &actor_hex)
        .await;
    let head_events_after = {
        let conn = head_state.db.conn().await;
        conn.prepare("SELECT COUNT(*) FROM nostr_events")
            .unwrap()
            .query_row([], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    let pub_events_after = {
        let conn = pub_state.db.conn().await;
        conn.prepare("SELECT COUNT(*) FROM nostr_events")
            .unwrap()
            .query_row([], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    assert_eq!(
        head_events_before, head_events_after,
        "head store stable on re-run"
    );
    assert_eq!(
        pub_events_before, pub_events_after,
        "public store stable on re-run"
    );
}

/// A keyless/proxied actor (no deposited nsec on the head) makes the arm no-op —
/// nothing is pushed and no cursor row is written.
#[tokio::test]
async fn head_arm_skips_an_actor_without_a_deposited_nsec() {
    let (pub_url, _pub_state) = start_nest().await;
    let (_head_url, head_state) = start_nest().await;

    let actor = [0x51u8; 32];
    let actor_hex = hex::encode(actor);
    // A proxied (keyless) account on the head — no deposited key.
    let kp = keypair(9);
    {
        let conn = head_state.db.conn().await;
        nostr_db::link_account(
            &conn,
            &actor_hex,
            &pubkey_hex(&kp),
            "proxied",
            None,
            None,
            None,
        )
        .unwrap();
    }

    let n =
        fauna_nest::nest_sync_worker::relay_actor_nostr(&head_state, &pub_url, &actor, &actor_hex)
            .await;
    assert_eq!(
        n, 0,
        "no deposited nsec → the arm returns 0 without dialing"
    );
}

/// The capability gate holds over the real channel: a `nostr_push` originated
/// without the pairing capability is refused by the public serving handler.
#[tokio::test]
async fn push_without_capability_is_refused_over_the_channel() {
    let (pub_url, _pub_state) = start_nest().await;
    let (_head_url, head_state) = start_nest().await;

    let actor = [0x42u8; 32];
    let kp = keypair(7);
    let req = fauna_nest::federation_handlers::FedNostrPushRequest {
        actor_id: hex::encode(actor),
        pubkey: pubkey_hex(&kp),
        relay_list: None,
        events: vec![fauna_nest::federation_handlers::FedNostrEvent {
            raw_json: serde_json::to_string(&signed(&kp, 1, 1000, "hi")).unwrap(),
        }],
    };
    // No pairing row on PUB → the handler forbids → the originator surfaces an error.
    let err = fauna_nest::federation_pool::originate_nostr_push(
        &head_state.federation_pool,
        &head_state,
        &pub_url,
        req,
    )
    .await
    .unwrap_err();
    assert!(
        format!("{err}").contains("forbidden"),
        "unauthorized push must be refused, got: {err}"
    );
}
