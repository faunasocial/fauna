#![cfg(feature = "nostr")]
//! **Nostr Phase-2 proxy delegation — tier_3 end-to-end proofs (spec slice
//! P2.8; the implementation spec is tracked internally, ratified 2026-07-22 —
//! mechanism prose in `docs/goal/ui/nostr.md` § The bridging gate → Phase 2).**
//!
//! Two in-process nests over the real federation WS-RPC channel: a keyless
//! public *serving* box `PUB` and a paired *head* `HEAD` holding the deposited
//! nsec. Unlike the wire-happy-path companion
//! (`nostr_federation_legs.rs`, which proves the two legs move class-1 rows
//! over the channel), these five drive the **production entry points at the
//! next level of faithfulness** — real materialization/sign
//! (`materialize_all_exposed`), the real unauthenticated gift-wrap inbox
//! (`handle_gift_wrap_inbox`), the real seal seam (`process_gift_wrap_inbound`
//! via the pull arm), the real relay REQ machinery (`store::query_events` +
//! `matches_any` + `gift_wrap_visible_to`, the exact functions the WS REQ arm
//! runs), and the real relay HTTP endpoints (`/nostr`, `/nostr/info`) over a
//! socket — to pin the spec's four fixed constraints (`nostr.md` § The relay
//! event store → Wider posture part 3):
//!
//!   (i)   the nsec never reaches the keyless public box — signing, gift-wrap
//!         unwrap, and DM sealing happen only on the head (T5 probes PUB's DB:
//!         no key material, no DM rows, no DM plaintext at rest);
//!   (ii)  the public box holds/serves only what any relay-store box holds
//!         (class-1 signed events + opaque kind-1059 wraps) with every serving
//!         invariant unchanged — recipient-gated 1059, NIP-50 index-time
//!         exclusion, hard caps (T1 serves a pushed class-1 row over REQ incl.
//!         FTS; T2 keeps a pulled wrap recipient-gated on PUB);
//!   (iii) `nostr_events` is location-independent — federation legs move rows,
//!         never a second store (the ingested row IS the self-authenticating
//!         wire event; provenance is the `origin` column only);
//!   (iv)  the keyless serving box's availability predicate is pairing-derived
//!         (T3: 503 → seed `nostr_push` pairing → serve → revoke → 503, one
//!         process, no restart).
//!
//! Only compiled under `--features nostr`.

mod common;

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_bridge_nostr::filter::matches_any;
use fauna_bridge_nostr::nip17::wrap_dm;
use fauna_bridge_nostr::signing::{Keypair, verify_event};
use fauna_bridge_nostr::types::{Event, Filter};
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::nostr::db as nostr_db;
use fauna_nest::nostr::key_crypto::encrypt_nostr_privkey;
use fauna_nest::nostr::relay_endpoint::handle_gift_wrap_inbox;
use fauna_nest::nostr::{self, store};
use fauna_nest::routes::AppState;
use fauna_protocol::pair::capability::NOSTR_PUSH;

/// Spin a real in-process nest (its full `build_router`, incl.
/// `/api/v1/federation/ws` **and** the `/nostr` relay endpoints) on a loopback
/// socket with a distinct nest identity and the Nostr tables initialised.
/// Copied from `nostr_federation_legs.rs::start_nest`: `for_test`'s routers are
/// empty, so we register the federation handlers (the `nostr_push`/`nostr_pull`
/// serving side) and the anon discovery handlers (peer `nest_id` resolution the
/// channel pool needs before dialing).
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
    fauna_nest::test_support::seat_own_deployment_seed(&state).await;
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

/// Read a stored event's `origin` on a box, or `None` if absent.
async fn origin_of(state: &Arc<AppState>, id: &str) -> Option<String> {
    let conn = state.db.conn().await;
    conn.prepare("SELECT origin FROM nostr_events WHERE id = ?1")
        .unwrap()
        .query_row([id], |r| r.get::<_, String>(0))
        .ok()
}

/// A scalar `COUNT(*)`-style i64 over `sql` with no bound params.
async fn scalar(state: &Arc<AppState>, sql: &str) -> i64 {
    let conn = state.db.conn().await;
    conn.prepare(sql)
        .unwrap()
        .query_row([], |r| r.get::<_, i64>(0))
        .unwrap()
}

/// The two boxes plus the one proxied actor, wired to the point where the real
/// production entry points run: HEAD holds the deposited (custodial) nsec and
/// the actor's MSEK-derived seal key (so the seal seam can run head-side); PUB
/// authorizes HEAD to push/pull for the actor via a `nostr_push` pairing.
struct Fixture {
    pub_url: String,
    pub_state: Arc<AppState>,
    head_state: Arc<AppState>,
    actor: [u8; 32],
    actor_hex: String,
    kp: Keypair,         // the actor's Nostr keypair — deposited on HEAD only.
    pubkey: String,      // the actor's Nostr pubkey (hex), the #p / author scope key.
    seal_msek: [u8; 32], // the recipient's MSEK, for the at-rest DM open.
}

impl Fixture {
    async fn new() -> Fixture {
        let (pub_url, pub_state) = start_nest().await;
        let (_head_url, head_state) = start_nest().await;
        fauna_nest::test_support::seat_own_deployment_seed(&head_state).await;
        let head_nest_id = head_state.nest_identity.public_key_bytes();
        let pub_nest_id = pub_state.nest_identity.public_key_bytes();

        let actor = [0x42u8; 32];
        let actor_hex = hex::encode(actor);
        let kp = keypair(7);
        let pubkey = kp.public_key_hex();

        // HEAD deposits the actor's nsec (custodial — encrypted under HEAD's own
        // nest key so the seal seam can decrypt it) and provisions the actor's
        // MSEK-derived seal key (the D2 row the seal seam seals to).
        let msek = [0x88u8; 32];
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
        }
        common::seed_recipient_seal_key(&head_state.db, &actor, &msek).await;

        // HEAD holds a local self-pairing row (the two-row model) whose
        // `nest_url` names PUB — that URL is what the head-side worker's
        // bunker-subscription reconciler and `preferred_public_relay_url`
        // resolve (T6); PUB authorizes HEAD to push/pull for the actor with
        // `nostr_push`. The PUB pairing is what makes the proxied row a local
        // inbox AND gates both legs.
        head_state
            .db
            .store_pairing(
                &actor,
                &pub_nest_id,
                &fauna_protocol::pair::default_self_sync(),
                None,
                Some(&pub_url),
                None,
            )
            .await
            .unwrap();
        pub_state
            .db
            .store_pairing(
                &actor,
                &head_nest_id,
                &[NOSTR_PUSH.to_string()],
                None,
                None,
                None,
            )
            .await
            .unwrap();

        Fixture {
            pub_url,
            pub_state,
            head_state,
            actor,
            actor_hex,
            kp,
            pubkey,
            seal_msek: msek,
        }
    }

    /// Seed one exposed Fauna post authored by the actor on HEAD and run the real
    /// periodic materialization sweep (`materialize_all_exposed`) — it decrypts
    /// the deposited nsec and signs a real derived kind-1 event. Returns that
    /// event's id (the head-authored `origin='ingest'` row the push leg selects).
    async fn seed_and_materialize_outbound(&self, content: &str) -> String {
        use fauna_core::data::{Post, PostBody, Timestamp};
        use fauna_core::identity::ActorId;

        let post = Post {
            author: ActorId(self.actor),
            created_at: Timestamp(1_000_000_000_000),
            body: PostBody::Text {
                content: content.to_string(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let payload = fauna_core::encoding::canonical_encode(&post).unwrap();
        let id = blake3::hash(&payload);
        fauna_nest::segments::post::store_post(
            &self.head_state.post_segments,
            &self.head_state.db,
            id.as_bytes(),
            &payload,
            None,
        )
        .await
        .unwrap();
        // Expose the account so the sweep picks it up (its account filter is
        // `expose_content = 1 AND encrypted_privkey IS NOT NULL`).
        {
            let conn = self.head_state.db.conn().await;
            nostr_db::update_settings(
                &conn,
                &self.actor_hex,
                &nostr_db::NostrSettings {
                    expose_content: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let nest_key = self.head_state.nest_identity.signing_key.to_bytes();
        let n = store::materialize_all_exposed(
            &self.head_state.db,
            &self.head_state.post_segments,
            &nest_key,
        )
        .await
        .expect("materialize sweep");
        assert_eq!(n, 1, "the exposed post materialized into one signed event");

        // The derived event now rests on HEAD as ORIGIN_INGEST (the push scope).
        let conn = self.head_state.db.conn().await;
        let derived = store::query_events(
            &conn,
            &[Filter {
                authors: Some(vec![self.pubkey.clone()]),
                kinds: Some(vec![1]),
                ..Default::default()
            }],
            100,
        )
        .unwrap();
        assert_eq!(derived.len(), 1, "exactly one derived kind-1 event on HEAD");
        assert!(
            verify_event(&derived[0]) && derived[0].content.contains(content),
            "the derived event is genuinely signed and carries the post text"
        );
        derived[0].id.clone()
    }

    /// Ensure PUB knows the proxied account (a real push auto-provisions it via
    /// R9 (account-data-plane.md § The ratified decisions); when a test deposits a wrap without pushing first, seed it here), then
    /// deposit a NIP-17 gift wrap addressed to the actor through the REAL
    /// unauthenticated inbox `handle_gift_wrap_inbox`. Returns the wrap event.
    async fn deposit_wrap_at_public(&self, plaintext: &str) -> Event {
        let needs_proxied_account = {
            let conn = self.pub_state.db.conn().await;
            nostr_db::get_account(&conn, &self.actor_hex)
                .unwrap()
                .is_none()
        };
        if needs_proxied_account {
            let conn = self.pub_state.db.conn().await;
            nostr_db::link_account(
                &conn,
                &self.actor_hex,
                &self.pubkey,
                "proxied",
                None,
                None,
                None,
            )
            .unwrap();
        }
        let sender = Keypair::generate();
        let wrap = wrap_dm(&sender, &self.kp.public_key_bytes(), plaintext).unwrap();
        let reply = handle_gift_wrap_inbox(&self.pub_state, wrap.clone()).await;
        assert!(
            matches!(
                reply,
                fauna_bridge_nostr::nip01::RelayMessage::Ok { accepted: true, .. }
            ),
            "a wrap for the proxied inbox is accepted on the keyless public box: {reply:?}"
        );
        wrap
    }

    /// One real head-side worker cycle (`relay_actor_nostr`): push then pull.
    async fn cycle(&self) -> usize {
        fauna_nest::nest_sync_worker::relay_actor_nostr(
            &self.head_state,
            &self.pub_url,
            &self.actor,
            &self.actor_hex,
        )
        .await
    }
}

/// **T1 — outbound.** HEAD deposits an nsec + exposes a post; the real
/// materialization sweep signs it; one worker cycle pushes it public-ward. The
/// row arrives on PUB as `origin='federation'` (auto-provisioning the proxied
/// account, no key crossing the wire) and serves over the real relay REQ
/// machinery — by-author, with the NIP-50 FTS index intact (constraint (ii)).
#[tokio::test]
async fn outbound_head_authored_event_materializes_pushes_and_serves_over_req_with_fts_intact() {
    let fx = Fixture::new().await;
    let content = "materialized federated note alpha";
    let derived_id = fx.seed_and_materialize_outbound(content).await;

    let n = fx.cycle().await;
    assert!(n >= 1, "the push leg delivered the derived row this cycle");

    // PUB auto-provisioned the proxied account — knows the pubkey, holds no key.
    {
        let conn = fx.pub_state.db.conn().await;
        let acct = nostr_db::get_account(&conn, &fx.actor_hex)
            .unwrap()
            .unwrap();
        assert_eq!(acct.signing_mode, "proxied");
        assert_eq!(acct.nostr_pubkey, fx.pubkey);
        assert!(
            acct.encrypted_privkey.is_none(),
            "constraint (i): no key material crosses the wire"
        );
    }

    // The row rests on PUB as ORIGIN_FEDERATION (constraint (iii): provenance is
    // the column, not a second store).
    assert_eq!(
        origin_of(&fx.pub_state, &derived_id).await.as_deref(),
        Some(store::ORIGIN_FEDERATION)
    );

    // It serves over the REAL relay REQ path: the exact functions the WS REQ arm
    // runs — `query_events` narrows in SQL, then `matches_any` + the recipient
    // gate are the two authoritative in-memory checks.
    let by_author = Filter {
        authors: Some(vec![fx.pubkey.clone()]),
        kinds: Some(vec![1]),
        ..Default::default()
    };
    {
        let conn = fx.pub_state.db.conn().await;
        let served = store::query_events(&conn, std::slice::from_ref(&by_author), 100).unwrap();
        assert_eq!(served.len(), 1, "PUB serves the federated row over REQ");
        let ev = &served[0];
        assert_eq!(ev.id, derived_id);
        assert!(verify_event(ev), "PUB never emits an unsigned event");
        assert!(
            matches_any(std::slice::from_ref(&by_author), ev)
                && store::gift_wrap_visible_to(ev, None),
            "a class-1 note passes the REQ's own match + is publicly visible"
        );
    }

    // FTS intact: a NIP-50 search resolves the federation-ingested row — the
    // store's FTS trigger indexed it exactly as for a locally-ingested row
    // (constraint (ii): every serving invariant unchanged).
    {
        let conn = fx.pub_state.db.conn().await;
        let hit = store::query_events(
            &conn,
            &[Filter {
                search: Some("federated".into()),
                ..Default::default()
            }],
            100,
        )
        .unwrap();
        assert!(
            hit.iter().any(|e| e.id == derived_id),
            "the pushed row is discoverable via the NIP-50 FTS index on PUB"
        );
    }
}

/// **T2 — inbound wrap.** A wrap for the proxied recipient is deposited at the
/// keyless PUB via the real unauthenticated inbox — accepted, stored verbatim,
/// and NOT sealed there (the seam self-gates keyless: no DM row rests
/// on PUB). One worker cycle pulls it head-ward; HEAD ingests it
/// (`origin='federation'`) and the seal seam — running with the deposited key —
/// deposits the sealed DM row (the bridged family, through the Nostr leg)
/// that opens to the plaintext under the
/// recipient's MSEK-derived secret. The wrap stays served recipient-gated on PUB.
#[tokio::test]
async fn inbound_wrap_deposited_keyless_pulls_head_ward_and_the_seal_seam_produces_the_dm_row() {
    let fx = Fixture::new().await;
    let plaintext = "rendezvous at the lighthouse pier at dusk";
    let wrap = fx.deposit_wrap_at_public(plaintext).await;

    // On PUB the wrap rests verbatim (ORIGIN_INGEST — an external deposit) and
    // the seal seam self-gated: NO plaintext-derived DM row on the keyless box.
    assert_eq!(
        origin_of(&fx.pub_state, &wrap.id).await.as_deref(),
        Some(store::ORIGIN_INGEST)
    );
    let pub_dms = scalar(
        &fx.pub_state,
        "SELECT COUNT(*) FROM bridge_conversation_messages",
    )
    .await;
    assert_eq!(pub_dms, 0, "constraint (i): the keyless box seals nothing");

    // One cycle: push moves nothing new (no head-ingest content here); pull
    // ingests the wrap head-ward and the seal seam runs with the deposited key.
    fx.cycle().await;

    // HEAD ingested the wrap as ORIGIN_FEDERATION.
    assert_eq!(
        origin_of(&fx.head_state, &wrap.id).await.as_deref(),
        Some(store::ORIGIN_FEDERATION),
        "the external deposit was pulled head-ward"
    );

    // HEAD produced exactly one sealed DM row (the seam ran with the deposited
    // key), and it opens to the plaintext under the recipient's MSEK secret while
    // NOT embedding the plaintext at rest.
    let (dm_count, sealed, direction): (i64, Vec<u8>, String) = {
        let conn = fx.head_state.db.conn().await;
        let count: i64 = conn
            .prepare("SELECT COUNT(*) FROM bridge_conversation_messages WHERE actor_id = ?1")
            .unwrap()
            .query_row([&fx.actor[..]], |r| r.get(0))
            .unwrap();
        let (sealed, direction): (Vec<u8>, String) = conn
            .prepare(
                "SELECT sealed_content, direction FROM bridge_conversation_messages
                  WHERE actor_id = ?1",
            )
            .unwrap()
            .query_row([&fx.actor[..]], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        (count, sealed, direction)
    };
    assert_eq!(dm_count, 1, "the seal seam produced the head's DM row");
    assert_eq!(direction, "in");
    assert!(
        !sealed
            .windows(plaintext.len())
            .any(|w| w == plaintext.as_bytes()),
        "the stored DM bytes must not embed the plaintext"
    );
    let opened = common::open_recipient_record(&sealed, &fx.seal_msek);
    assert_eq!(
        opened,
        plaintext.as_bytes(),
        "the seal round-trips to the DM plaintext"
    );

    // The wrap stays served recipient-gated on PUB (pull is non-destructive).
    {
        let conn = fx.pub_state.db.conn().await;
        let served = store::query_events(
            &conn,
            &[Filter {
                kinds: Some(vec![1059]),
                ..Default::default()
            }],
            100,
        )
        .unwrap();
        assert_eq!(served.len(), 1, "PUB still holds the wrap after the pull");
        let ev = &served[0];
        assert!(
            store::gift_wrap_visible_to(ev, Some(&fx.pubkey)),
            "served only to the authed recipient"
        );
        assert!(
            !store::gift_wrap_visible_to(ev, None),
            "hidden from an anonymous reader — no ciphertext/metadata leak (constraint (ii))"
        );
    }
}

/// **T3 — predicate lifecycle (constraint (iv)).** A keyless public box's real
/// relay HTTP endpoints (`/nostr` same-URI NIP-11 + `/nostr/info`) 503 before
/// any pairing, serve (200) the moment a `nostr_push` pairing lands, and 503
/// again the instant it is revoked — one process, no restart (the predicate is
/// per-request-derived, never cached / boot-reconciled).
#[tokio::test]
async fn keyless_relay_endpoints_503_until_a_nostr_push_pairing_then_503_again_after_revocation() {
    let (url, state) = start_nest().await;
    let client = reqwest::Client::new();
    let info_url = format!("{url}/nostr/info");
    let ws_url = format!("{url}/nostr");

    // GET `/nostr/info` (the convenience route) — 503/200 straight off the gate.
    let info_status = |c: reqwest::Client, u: String| async move {
        c.get(u).send().await.unwrap().status().as_u16()
    };
    // The `/nostr` same-URI NIP-11 resolution (how real clients fetch the info
    // doc): a plain GET with the nostr+json Accept header. The serving gate runs
    // before the WS-upgrade rejection, so this reads the predicate directly.
    let ws_status = |c: reqwest::Client, u: String| async move {
        c.get(u)
            .header("accept", "application/nostr+json")
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    };

    // Keyless, no pairing → both endpoints 503.
    assert_eq!(info_status(client.clone(), info_url.clone()).await, 503);
    assert_eq!(ws_status(client.clone(), ws_url.clone()).await, 503);

    // A paired head enrolls this box as its serving face (a `nostr_push`
    // pairing). The very next request flips both endpoints to serving.
    state
        .db
        .store_pairing(
            &[0x51u8; 32],
            &[0x62u8; 32],
            &[NOSTR_PUSH.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        info_status(client.clone(), info_url.clone()).await,
        200,
        "a nostr_push pairing flips /nostr/info to serving (no deposit, no restart)"
    );
    assert_eq!(
        ws_status(client.clone(), ws_url.clone()).await,
        200,
        "and the same-URI NIP-11 doc serves on /nostr"
    );

    // Revoke the pairing → the very next request 503s again, same process.
    state
        .db
        .revoke_pairing(&[0x51u8; 32], &[0x62u8; 32])
        .await
        .unwrap();
    assert_eq!(
        info_status(client.clone(), info_url).await,
        503,
        "revocation flips /nostr/info back to 503 immediately"
    );
    assert_eq!(
        ws_status(client, ws_url).await,
        503,
        "and /nostr with no restart / no boot reconcile"
    );
}

/// **T4 — echo / idempotency (spec R4/R5).** After the outbound + inbound flows,
/// re-running push+pull cycles moves nothing on either box (cursors + `origin` +
/// event-id dedup). And a **pushed** row is never returned by the public box's
/// pull: a fresh from-zero pull off PUB returns its externally-deposited
/// `origin='ingest'` wrap but never the head-authored `origin='federation'` row
/// PUB received — so it can never echo back head-ward and re-enter as federation.
#[tokio::test]
async fn re_running_push_and_pull_cycles_moves_nothing_and_a_pushed_row_is_never_pulled_back() {
    let fx = Fixture::new().await;
    let derived_id = fx
        .seed_and_materialize_outbound("federated note for idempotency")
        .await;
    fx.cycle().await; // push the authored row public-ward.
    let wrap = fx.deposit_wrap_at_public("one inbound message only").await;
    fx.cycle().await; // pull the wrap head-ward + seal it on HEAD.

    // Snapshot both stores + HEAD's DM plane after the two flows.
    let head_events_before = scalar(&fx.head_state, "SELECT COUNT(*) FROM nostr_events").await;
    let pub_events_before = scalar(&fx.pub_state, "SELECT COUNT(*) FROM nostr_events").await;
    let head_dms_before = scalar(
        &fx.head_state,
        "SELECT COUNT(*) FROM bridge_conversation_messages",
    )
    .await;

    // Two more cycles must be pure no-ops.
    fx.cycle().await;
    fx.cycle().await;
    assert_eq!(
        scalar(&fx.head_state, "SELECT COUNT(*) FROM nostr_events").await,
        head_events_before,
        "HEAD store stable across re-runs"
    );
    assert_eq!(
        scalar(&fx.pub_state, "SELECT COUNT(*) FROM nostr_events").await,
        pub_events_before,
        "PUB store stable across re-runs"
    );
    assert_eq!(
        scalar(
            &fx.head_state,
            "SELECT COUNT(*) FROM bridge_conversation_messages"
        )
        .await,
        head_dms_before,
        "no duplicate sealed DM row from a re-pulled wrap"
    );

    // The head-authored row rests on PUB as federation; HEAD's own copy stays
    // ORIGIN_INGEST — it was never re-ingested via pull.
    assert_eq!(
        origin_of(&fx.pub_state, &derived_id).await.as_deref(),
        Some(store::ORIGIN_FEDERATION)
    );
    assert_eq!(
        origin_of(&fx.head_state, &derived_id).await.as_deref(),
        Some(store::ORIGIN_INGEST),
        "the pushed row is never pulled back into HEAD as a federation row"
    );

    // Direct proof over the wire: a from-zero pull off PUB returns the external
    // wrap but NOT the pushed (federation-origin) row — PUB's pull scope is
    // `origin='ingest'` only (spec R4, kills echo/loops).
    let reply = fauna_nest::federation_pool::originate_nostr_pull(
        &fx.head_state.federation_pool,
        &fx.head_state,
        &fx.pub_url,
        fauna_nest::federation_handlers::FedNostrPullRequest {
            actor_id: fx.actor_hex.clone(),
            since_stored_at: 0,
            since_id: String::new(),
        },
    )
    .await
    .expect("channel nostr_pull");
    let returned_ids: Vec<String> = reply
        .events
        .iter()
        .filter_map(|e| {
            serde_json::from_str::<Event>(&e.raw_json)
                .ok()
                .map(|ev| ev.id)
        })
        .collect();
    assert!(
        returned_ids.contains(&wrap.id),
        "PUB's pull returns its externally-deposited ingest wrap"
    );
    assert!(
        !returned_ids.contains(&derived_id),
        "PUB's pull never returns the pushed federation-origin row (no echo, spec R4)"
    );
}

/// **T5 — at-rest probe (constraint (i) / spec R11).** After the outbound +
/// inbound flows, the keyless public box's DB holds no key material for the
/// proxied actor: no non-NULL `encrypted_privkey`, zero DM rows, zero bunker
/// roster rows — and the DM plaintext appears nowhere in any stored row (only the
/// opaque kind-1059 ciphertext rests). Every `FedNostr*` shape makes the rest
/// unspellable; this asserts the shapes held in practice.
#[tokio::test]
async fn the_public_box_holds_no_key_material_no_dm_rows_and_no_dm_plaintext_at_rest() {
    let fx = Fixture::new().await;
    fx.seed_and_materialize_outbound("federated note for the at-rest probe")
        .await;
    fx.cycle().await;
    let plaintext = "the vault combination is seven three nine one";
    fx.deposit_wrap_at_public(plaintext).await;
    fx.cycle().await;

    // No key material rests on the public box.
    assert_eq!(
        scalar(
            &fx.pub_state,
            "SELECT COUNT(*) FROM nostr_accounts WHERE encrypted_privkey IS NOT NULL"
        )
        .await,
        0,
        "no deposited nsec ciphertext rests on the public box (R11)"
    );
    // No sealed-DM plane and no bunker roster for the proxied actor.
    assert_eq!(
        scalar(
            &fx.pub_state,
            "SELECT COUNT(*) FROM bridge_conversation_messages"
        )
        .await,
        0,
        "no DM rows on the public box (constraint (i))"
    );
    assert_eq!(
        scalar(&fx.pub_state, "SELECT COUNT(*) FROM nostr_bunker_signers").await,
        0,
        "no bunker signer rows on the public box"
    );
    assert_eq!(
        scalar(&fx.pub_state, "SELECT COUNT(*) FROM nostr_bunker_apps").await,
        0,
        "no bunker app rows on the public box"
    );

    // The DM plaintext appears in NO stored row — the wrap rests as opaque
    // kind-1059 ciphertext, its inner content NIP-44-sealed end-to-end.
    let plaintext_hits: i64 = {
        let conn = fx.pub_state.db.conn().await;
        conn.prepare("SELECT COUNT(*) FROM nostr_events WHERE raw_json LIKE ?1")
            .unwrap()
            .query_row([format!("%{plaintext}%")], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(
        plaintext_hits, 0,
        "the DM plaintext never appears in any stored row on the public box"
    );
    // Sanity: the wrap really is present (so the probe above is not vacuously
    // green) — one opaque 1059 row rests, ciphertext only.
    assert_eq!(
        scalar(
            &fx.pub_state,
            "SELECT COUNT(*) FROM nostr_events WHERE kind = 1059"
        )
        .await,
        1,
        "the opaque wrap is present (the probe is non-vacuous)"
    );
}

/// **T6 — the full live-WS bunker round-trip through the public box, and the
/// interactive-latency pin (the last P2.8 proof).** A real third-party NIP-46
/// app (`nostr-connect`, the SDK Amethyst-class apps embed — conventions per
/// `nostr_relay_interop.rs`) consumes a `bunker://` invite whose `relay=`
/// names the KEYLESS public box: the app's kind-24133 request rides PUB's
/// ephemeral fall-through broadcast (R10 part A), HEAD's standing proxy
/// subscription picks it up, HEAD executes under the deposited nsec and
/// publishes the signed response back through PUB, and the app's open REQ
/// receives it — app→public→head→public→app, with no key material ever
/// leaving HEAD (re-pinned at the end).
///
/// The elapsed-time assertion is the **cadence pin**: NIP-46 is interactive
/// (SDK client timeouts are ~10s), so the head must answer from a dedicated
/// low-latency bunker drain — a drain that only runs on the worker's 60s tick
/// cannot pass this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t6_live_ws_bunker_round_trip_through_the_public_box_is_interactive() {
    use std::time::Duration;

    use nostr_connect::prelude::*;

    let fx = Fixture::new().await;

    // Mint the invite on HEAD exactly as `fauna.nostr.bunker.create_invite`
    // does, and compose the connect string against PUB's relay — the R10 shape
    // `preferred_public_relay_url` produces for a paired head.
    let inv = {
        let conn = fx.head_state.db.conn().await;
        fauna_nest::nostr::bunker::create_invite(
            &conn,
            &fx.actor_hex,
            fauna_core::data::Timestamp::now_secs() as u64,
        )
        .unwrap()
    };
    let pub_relay_ws = format!("{}/nostr", fx.pub_url.replace("http://", "ws://"));

    // Spawn HEAD's REAL sync worker — the same long-lived task production boot
    // spawns. Its reconciler must find the `nostr_push` pairing's `nest_url`
    // and hold the standing kind-24133 subscription on PUB. The wake channel
    // is the one `state.nostr.bunker_wake_tx` feeds in production (phase 2
    // drives it the way `create_invite` does).
    let (_outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(8);
    let (bunker_wake_tx, bunker_wake_rx) = tokio::sync::mpsc::channel(1);
    let worker = fauna_nest::nostr::sync_worker::NostrSyncWorker::new(
        fx.head_state.db.clone(),
        fx.head_state.post_segments.clone(),
        outbound_rx,
        bunker_wake_rx,
        fx.head_state.nest_identity.signing_key.to_bytes(),
        // PUB is an in-process peer on 127.0.0.1, which the production policy
        // refuses outright; this dependency build carries neither `test-hooks`
        // nor the e2e env, so the loopback allowance is passed in (the one
        // in-tree reason the policy is a constructor argument).
        fauna_bridge_nostr::relay_client::RelayDialPolicy::PublicOrLoopback,
    );
    let worker_task = tokio::spawn(worker.run());
    // Let the worker's startup pass establish the subscription on PUB before
    // the app fires its (ephemeral, never-replayed) request.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    let bunker_uri = format!(
        "bunker://{}?relay={}&secret={}",
        inv.signer_pubkey, pub_relay_ws, inv.secret
    );
    let uri = NostrConnectUri::parse(&bunker_uri).expect("parse bunker URI");
    let app_keys = Keys::generate();
    let signer =
        NostrConnect::new(uri, app_keys, Duration::from_secs(10), None).expect("nip46 client");

    let started = std::time::Instant::now();
    let user_pk = tokio::time::timeout(Duration::from_secs(15), signer.get_public_key_async())
        .await
        .expect("get_public_key within 15s")
        .expect("get_public_key through the proxied relay");
    assert_eq!(
        user_pk.to_hex(),
        fx.pubkey,
        "get_public_key returns the USER pubkey (deposited on HEAD), not the signer's"
    );

    let unsigned = EventBuilder::new(Kind::TextNote, "signed at home, served in public")
        .finalize_unsigned(user_pk);
    let signed = tokio::time::timeout(Duration::from_secs(15), signer.sign_event_async(unsigned))
        .await
        .expect("sign_event within 15s")
        .expect("sign_event through the proxied relay");
    assert_eq!(signed.content, "signed at home, served in public");
    signed.verify().expect("app-side signature verify");

    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "interactive NIP-46 through the proxy answers within SDK timeouts (took {elapsed:?})"
    );

    // ── Phase 2: mint→wake→respawn. A SECOND invite minted while the worker
    // runs must become usable immediately — `create_invite` nudges
    // `bunker_wake_tx` (driven here exactly as the handler does), the
    // reconciler respawns the drain with the grown `#p` roster, and a fresh
    // app connects through PUB within the same interactive bound. Without the
    // wake, the new signer's filter would go live only at the next 60s tick
    // and this connect would time out.
    let inv2 = {
        let conn = fx.head_state.db.conn().await;
        fauna_nest::nostr::bunker::create_invite(
            &conn,
            &fx.actor_hex,
            fauna_core::data::Timestamp::now_secs() as u64,
        )
        .unwrap()
    };
    bunker_wake_tx.try_send(()).expect("wake the reconciler");
    // The respawned drain needs a beat to reconnect + resubscribe on PUB.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    let bunker_uri2 = format!(
        "bunker://{}?relay={}&secret={}",
        inv2.signer_pubkey, pub_relay_ws, inv2.secret
    );
    let uri2 = NostrConnectUri::parse(&bunker_uri2).expect("parse second bunker URI");
    let signer2 = NostrConnect::new(uri2, Keys::generate(), Duration::from_secs(10), None)
        .expect("second nip46 client");
    let user_pk2 = tokio::time::timeout(Duration::from_secs(15), signer2.get_public_key_async())
        .await
        .expect("second get_public_key within 15s")
        .expect("a freshly-minted signer is reachable through the proxy without waiting a tick");
    assert_eq!(user_pk2.to_hex(), fx.pubkey);

    // The round-trip moved no key material to PUB (T5's core probes, re-run
    // after live bunker traffic).
    assert_eq!(
        scalar(
            &fx.pub_state,
            "SELECT COUNT(*) FROM nostr_accounts WHERE encrypted_privkey IS NOT NULL"
        )
        .await,
        0,
        "no nsec ciphertext on the public box after live bunker traffic"
    );
    assert_eq!(
        scalar(&fx.pub_state, "SELECT COUNT(*) FROM nostr_bunker_signers").await,
        0,
        "no bunker signer rows on the public box after live bunker traffic"
    );
    // Ephemeral 24133s were transported, never stored (constraint (ii)).
    assert_eq!(
        scalar(
            &fx.pub_state,
            "SELECT COUNT(*) FROM nostr_events WHERE kind = 24133"
        )
        .await,
        0,
        "kind-24133 transport events are never stored on the public box"
    );

    worker_task.abort();
}
