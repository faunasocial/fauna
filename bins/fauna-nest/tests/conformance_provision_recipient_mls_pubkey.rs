//! End-to-end round-trip for the `fauna.bridges.provision_recipient_mls_pubkey`
//! admin RPC.
//!
//! Drives the production writer for `actor_mls_pubkeys` through the
//! live `RpcRouter`: Admin actor calls `provision_recipient_mls_pubkey`
//! → row lands in the DB → MTA actor's `fetch_recipient_mls_pubkey`
//! returns the same bytes. Without this writer, every inbound DATA
//! rejects with "recipient has not provisioned an MLS pubkey": this door
//! is the only writer of the table.

use std::sync::Arc;

use bytes::Bytes;
use fauna_nest::{
    bridge_routing_handlers::register_bridge_routing_handlers,
    db::{CacheDb, bridge_service_users::BridgeRole},
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    bridge_routing::{
        FetchRecipientMlsPubkeyReply, FetchRecipientMlsPubkeyRequest,
        ProvisionRecipientMlsPubkeyRequest,
    },
    decode_strict as decode, encode_canonical,
    wrapped_blob::ProvisionReply,
};
use serde_bytes::ByteBuf;

async fn router_with_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    register_bridge_routing_handlers(&mut b);
    (b.build(), state)
}

async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, fauna_protocol::RpcError> {
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state, actor, payload).await
}

async fn approve_mta(state: &Arc<AppState>, bridge_actor: [u8; 32]) {
    state
        .db
        .create_pending_bridge_service_user(&bridge_actor, BridgeRole::Mta, "mta-1")
        .await
        .unwrap();
    state
        .db
        .upsert_bridge_x25519(&bridge_actor, &[1u8; 32])
        .await
        .unwrap();
    state
        .db
        .approve_bridge_service_user(&bridge_actor, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn admin_provision_recipient_mls_pubkey_round_trip() {
    let (router, state) = router_with_state().await;

    // Admin actor (the only caller class permitted to provision).
    let admin_actor = [0xA0u8; 32];
    state.db.add_admin_actor(&admin_actor[..]).await.unwrap();

    // Target recipient + their MLS pubkey (32-byte X25519 public key —
    // nest stores opaque 32-byte material; semantic validation is up to
    // the client that produced it).
    let recipient = [0x42u8; 32];
    let pubkey = [0xDDu8; 32];

    // 1. Admin provisions.
    let provision_req = ProvisionRecipientMlsPubkeyRequest {
        actor_id: ByteBuf::from(recipient.to_vec()),
        mls_pubkey: ByteBuf::from(pubkey.to_vec()),
        mlkem_ek: ByteBuf::from(vec![0x55u8; 1184]),
        ..Default::default()
    };
    let provision_payload = Bytes::from(encode_canonical(&provision_req).unwrap().to_vec());
    let provision_reply_bytes = dispatch(
        &router,
        state.clone(),
        admin_actor,
        "fauna.bridges.provision_recipient_mls_pubkey",
        provision_payload,
    )
    .await
    .expect("admin provision must succeed");
    let provision_reply: ProvisionReply = decode(&provision_reply_bytes).unwrap();
    assert!(provision_reply.ok);

    // 2. MTA-class bridge fetches the pubkey through the existing read
    //    surface — the same wire path the MTA traverses on every inbound
    //    DATA to seal `encrypted_body`. This is the production data flow
    //    that this RPC unblocks.
    let mta_actor = [0x11u8; 32];
    approve_mta(&state, mta_actor).await;

    let fetch_req = FetchRecipientMlsPubkeyRequest {
        actor_id: recipient.to_vec(),
        mail_new_ingest: false,
    };
    let fetch_payload = Bytes::from(encode_canonical(&fetch_req).unwrap().to_vec());
    let fetch_reply_bytes = dispatch(
        &router,
        state.clone(),
        mta_actor,
        "fauna.bridges.fetch_recipient_mls_pubkey",
        fetch_payload,
    )
    .await
    .expect("MTA fetch must succeed after admin provision");
    let fetch_reply: FetchRecipientMlsPubkeyReply = decode(&fetch_reply_bytes).unwrap();
    let got = fetch_reply
        .key
        .expect("fetch must return Some after admin provision");
    assert_eq!(
        got.mls_pubkey.as_slice(),
        &pubkey[..],
        "fetched bytes must equal provisioned bytes"
    );
}

#[tokio::test]
async fn non_admin_cannot_provision_recipient_mls_pubkey() {
    // Caller-class gate: only Admin may call this RPC. An MTA bridge
    // (the natural mis-call) must be rejected with permission_denied
    // even though it's an approved bridge service user.
    let (router, state) = router_with_state().await;
    let mta_actor = [0x11u8; 32];
    approve_mta(&state, mta_actor).await;

    let req = ProvisionRecipientMlsPubkeyRequest {
        actor_id: ByteBuf::from(vec![0x42u8; 32]),
        mls_pubkey: ByteBuf::from(vec![0xDDu8; 32]),
        mlkem_ek: ByteBuf::from(vec![0x55u8; 1184]),
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        mta_actor,
        "fauna.bridges.provision_recipient_mls_pubkey",
        payload,
    )
    .await
    .expect_err("MTA must not be permitted to provision recipient MLS pubkey");
    assert_eq!(err.code, "fauna.bridges.permission_denied");
}

#[tokio::test]
async fn provision_stores_epoch_seal_key_schedule() {
    // Content-sealing epochs B3 (design 2026-07-18 § 3): the additive
    // `epoch_keys` field lands the published horizon in
    // `actor_epoch_seal_keys`, selectable current-first with the
    // newest-earlier degradation, without touching the standing row.
    use fauna_protocol::bridge_routing::EpochSealKey;
    let (router, state) = router_with_state().await;
    let admin_actor = [0xA0u8; 32];
    state.db.add_admin_actor(&admin_actor[..]).await.unwrap();

    let recipient = [0x42u8; 32];
    // Clock-derived epochs so the sanity window never rots this test.
    let e_now = fauna_mls::wrapped_blob::mail_sealing_epoch_of(unix_now_secs());
    let req = ProvisionRecipientMlsPubkeyRequest {
        actor_id: ByteBuf::from(recipient.to_vec()),
        mls_pubkey: ByteBuf::from(vec![0xDDu8; 32]),
        mlkem_ek: ByteBuf::from(vec![0x55u8; 1184]),
        epoch_keys: Some(vec![
            EpochSealKey {
                epoch: e_now,
                mls_pubkey: ByteBuf::from(vec![0x01u8; 32]),
                mlkem_ek: ByteBuf::from(vec![0x02u8; 1184]),
            },
            EpochSealKey {
                epoch: e_now + 1,
                mls_pubkey: ByteBuf::from(vec![0x03u8; 32]),
                mlkem_ek: ByteBuf::from(vec![0x04u8; 1184]),
            },
        ]),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        admin_actor,
        "fauna.bridges.provision_recipient_mls_pubkey",
        payload,
    )
    .await
    .expect("provision with epoch schedule succeeds");
    let reply: ProvisionReply = decode(&reply_bytes).unwrap();
    assert!(reply.ok);

    // Standing row landed as ever.
    assert_eq!(
        state.db.get_actor_mls_pubkey(&recipient).await.unwrap(),
        Some([0xDDu8; 32])
    );
    // Schedule rows landed: current epoch selects itself…
    let (e, key) = state
        .db
        .get_actor_epoch_seal_key(&recipient, e_now + 1)
        .await
        .unwrap()
        .expect("schedule published");
    assert_eq!(e, e_now + 1);
    assert_eq!(key.mls_pubkey, [0x03u8; 32]);
    assert_eq!(key.mlkem_ek, vec![0x04u8; 1184]);
    // …and past the horizon the newest earlier row serves (degradation).
    let (e, key) = state
        .db
        .get_actor_epoch_seal_key(&recipient, e_now + 2000)
        .await
        .unwrap()
        .expect("stale schedule still serves");
    assert_eq!(e, e_now + 1);
    assert_eq!(key.mls_pubkey, [0x03u8; 32]);
}

#[tokio::test]
async fn provision_rejects_malformed_epoch_keys() {
    use fauna_protocol::bridge_routing::EpochSealKey;
    let (router, state) = router_with_state().await;
    let admin_actor = [0xA0u8; 32];
    state.db.add_admin_actor(&admin_actor[..]).await.unwrap();

    // Wrong pubkey length inside the schedule → malformed, nothing stored.
    // (Clock-derived in-window epoch so it's the LENGTH check that fires,
    // not the epoch-index sanity bound.)
    let e_now = fauna_mls::wrapped_blob::mail_sealing_epoch_of(unix_now_secs());
    let req = ProvisionRecipientMlsPubkeyRequest {
        actor_id: ByteBuf::from(vec![0x42u8; 32]),
        mls_pubkey: ByteBuf::from(vec![0xDDu8; 32]),
        mlkem_ek: ByteBuf::from(vec![0x55u8; 1184]),
        epoch_keys: Some(vec![EpochSealKey {
            epoch: e_now,
            mls_pubkey: ByteBuf::from(vec![0x01u8; 31]),
            mlkem_ek: ByteBuf::from(vec![0x04u8; 1184]),
        }]),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(
        &router,
        state.clone(),
        admin_actor,
        "fauna.bridges.provision_recipient_mls_pubkey",
        payload,
    )
    .await
    .expect_err("31-byte epoch pubkey must be malformed");
    assert_eq!(err.code, "fauna.protocol.malformed");

    // Over the per-request cap (64) → malformed.
    let too_many: Vec<EpochSealKey> = (0..65u64)
        .map(|e| EpochSealKey {
            epoch: e,
            mls_pubkey: ByteBuf::from(vec![0x01u8; 32]),
            mlkem_ek: ByteBuf::from(vec![0x04u8; 1184]),
        })
        .collect();
    let req = ProvisionRecipientMlsPubkeyRequest {
        actor_id: ByteBuf::from(vec![0x42u8; 32]),
        mls_pubkey: ByteBuf::from(vec![0xDDu8; 32]),
        mlkem_ek: ByteBuf::from(vec![0x55u8; 1184]),
        epoch_keys: Some(too_many),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        admin_actor,
        "fauna.bridges.provision_recipient_mls_pubkey",
        payload,
    )
    .await
    .expect_err("65 epochs must exceed the per-provision cap");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

#[tokio::test]
async fn provision_rejects_epoch_index_outside_sanity_bound() {
    // Security hardening: the epoch index is
    // untrusted wire input — lengths were validated, the index was not. An
    // honest publisher writes `[e_now, e_now + MAIL_EPOCH_PUBLISH_HORIZON]`;
    // anything far outside `[e_now − 2H, e_now + 2H]` is a client bug or an
    // adversarial distinct-epoch table-growth write, rejected at provision.
    use fauna_protocol::bridge_routing::EpochSealKey;
    let (router, state) = router_with_state().await;
    let admin_actor = [0xA0u8; 32];
    state.db.add_admin_actor(&admin_actor[..]).await.unwrap();

    let e_now = fauna_mls::wrapped_blob::mail_sealing_epoch_of(unix_now_secs());
    let slack = 2 * fauna_mls::wrapped_blob::MAIL_EPOCH_PUBLISH_HORIZON;

    let provision_with_epoch = |epoch: u64| {
        let req = ProvisionRecipientMlsPubkeyRequest {
            actor_id: ByteBuf::from(vec![0x42u8; 32]),
            mls_pubkey: ByteBuf::from(vec![0xDDu8; 32]),
            mlkem_ek: ByteBuf::from(vec![0x55u8; 1184]),
            epoch_keys: Some(vec![EpochSealKey {
                epoch,
                mls_pubkey: ByteBuf::from(vec![0x01u8; 32]),
                mlkem_ek: ByteBuf::from(vec![0x04u8; 1184]),
            }]),
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    };

    // Far past and far future → malformed, nothing stored.
    for wild in [
        0u64,
        e_now.saturating_sub(slack + 1),
        e_now + slack + 1,
        u64::MAX,
    ] {
        let err = dispatch(
            &router,
            state.clone(),
            admin_actor,
            "fauna.bridges.provision_recipient_mls_pubkey",
            provision_with_epoch(wild),
        )
        .await
        .expect_err("out-of-sanity-window epoch index must be malformed");
        assert_eq!(err.code, "fauna.protocol.malformed");
        assert!(
            state
                .db
                .get_actor_epoch_seal_key(&[0x42u8; 32], u64::MAX)
                .await
                .unwrap()
                .is_none(),
            "a rejected provision must store no schedule rows"
        );
    }

    // Boundary values are accepted (clock skew / stale-but-honest republish).
    for edge in [e_now.saturating_sub(slack), e_now + slack] {
        dispatch(
            &router,
            state.clone(),
            admin_actor,
            "fauna.bridges.provision_recipient_mls_pubkey",
            provision_with_epoch(edge),
        )
        .await
        .expect("boundary epoch index must be accepted");
    }
}

#[tokio::test]
async fn provision_accepts_honest_horizon_publish_and_refresh() {
    // The honest client path must never trip the sanity bound or the row
    // cap: enable-mail publishes `[e_now, e_now + 26]` (27 entries), and
    // every connect republishes the refreshed horizon as an idempotent
    // upsert (design § 3). Both must stay green, and selection stays
    // current-first afterward.
    use fauna_protocol::bridge_routing::EpochSealKey;
    let (router, state) = router_with_state().await;
    let admin_actor = [0xA0u8; 32];
    state.db.add_admin_actor(&admin_actor[..]).await.unwrap();
    let recipient = [0x42u8; 32];

    let e_now = fauna_mls::wrapped_blob::mail_sealing_epoch_of(unix_now_secs());
    let horizon = fauna_mls::wrapped_blob::MAIL_EPOCH_PUBLISH_HORIZON;

    let publish = |start: u64| {
        let keys: Vec<EpochSealKey> = (start..=start + horizon)
            .map(|e| EpochSealKey {
                epoch: e,
                mls_pubkey: ByteBuf::from(vec![(e % 251) as u8; 32]),
                mlkem_ek: ByteBuf::from(vec![0x04u8; 1184]),
            })
            .collect();
        let req = ProvisionRecipientMlsPubkeyRequest {
            actor_id: ByteBuf::from(recipient.to_vec()),
            mls_pubkey: ByteBuf::from(vec![0xDDu8; 32]),
            mlkem_ek: ByteBuf::from(vec![0x55u8; 1184]),
            epoch_keys: Some(keys),
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    };

    // Enable-mail publish, then a next-connect refresh one epoch later.
    for start in [e_now, e_now + 1] {
        dispatch(
            &router,
            state.clone(),
            admin_actor,
            "fauna.bridges.provision_recipient_mls_pubkey",
            publish(start),
        )
        .await
        .expect("honest horizon publish must succeed");
    }

    // Selection still current-first after the refresh republish.
    let (e, _) = state
        .db
        .get_actor_epoch_seal_key(&recipient, e_now)
        .await
        .unwrap()
        .expect("current epoch still covered");
    assert_eq!(e, e_now);
}

fn unix_now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

#[tokio::test]
async fn provision_recipient_mls_pubkey_rejects_malformed_actor_id() {
    let (router, state) = router_with_state().await;
    let admin_actor = [0xA0u8; 32];
    state.db.add_admin_actor(&admin_actor[..]).await.unwrap();

    // actor_id must be exactly 32 bytes; any other length is malformed.
    let req = ProvisionRecipientMlsPubkeyRequest {
        actor_id: ByteBuf::from(vec![0u8; 31]),
        mls_pubkey: ByteBuf::from(vec![0xDDu8; 32]),
        mlkem_ek: ByteBuf::from(vec![0x55u8; 1184]),
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        admin_actor,
        "fauna.bridges.provision_recipient_mls_pubkey",
        payload,
    )
    .await
    .expect_err("31-byte actor_id must be rejected as malformed");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

// ── Slice 1 — the handle IS the email address ──
//
// Enabling mail (the user provisioning their own recipient MLS pubkey) must
// create the canonical exact alias `<handle-localpart>@<domain>` so the user's
// registration handle is immediately routable for inbound mail with zero
// alias management (`mail-aliases.md` § Kind 1 — Exact: "the canonical address
// each user gets at signup"). Realizes that spec at enable-time (the mail
// domain must exist first, so signup is too early on a fresh nest).

#[tokio::test]
async fn provision_creates_canonical_handle_alias() {
    let (router, state) = router_with_state().await;
    let admin_actor = [0xA0u8; 32];
    state.db.add_admin_actor(&admin_actor[..]).await.unwrap();

    // A user whose registration handle is `alice@fauna.test`, on a nest whose
    // primary mail domain is `fauna.test` — so the handle IS the email address.
    let recipient = [0x42u8; 32];
    state
        .db
        .create_user_with_handle(&recipient, "free", "alice@fauna.test", None)
        .await
        .unwrap();
    state
        .db
        .add_mail_domain("fauna.test", true, "testing", "per_host", None, None)
        .await
        .unwrap();

    // Before enable, the handle is not routable.
    assert_eq!(
        state
            .db
            .lookup_exact_alias("fauna.test", "alice")
            .await
            .unwrap(),
        None,
        "no canonical alias should exist before mail is enabled",
    );

    // Enable mail = the user provisions their own recipient MLS pubkey.
    let req = ProvisionRecipientMlsPubkeyRequest {
        actor_id: ByteBuf::from(recipient.to_vec()),
        mls_pubkey: ByteBuf::from(vec![0xDDu8; 32]),
        mlkem_ek: ByteBuf::from(vec![0x55u8; 1184]),
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    dispatch(
        &router,
        state.clone(),
        admin_actor,
        "fauna.bridges.provision_recipient_mls_pubkey",
        payload,
    )
    .await
    .expect("provision must succeed");

    // The canonical exact alias now routes to the recipient: their handle is
    // their email address, with no user-facing alias step.
    assert_eq!(
        state
            .db
            .lookup_exact_alias("fauna.test", "alice")
            .await
            .unwrap(),
        Some(recipient),
        "enabling mail must create the canonical <handle-localpart>@<primary-domain> exact alias",
    );
}

#[tokio::test]
async fn provision_without_mail_domain_creates_no_alias() {
    // No mail domain registered yet → there is no domain to route the handle
    // on, so alias creation is a graceful no-op (not an error). The provision
    // itself still succeeds.
    let (router, state) = router_with_state().await;
    let admin_actor = [0xA0u8; 32];
    state.db.add_admin_actor(&admin_actor[..]).await.unwrap();

    let recipient = [0x43u8; 32];
    state
        .db
        .create_user_with_handle(&recipient, "free", "alice@fauna.test", None)
        .await
        .unwrap();

    let req = ProvisionRecipientMlsPubkeyRequest {
        actor_id: ByteBuf::from(recipient.to_vec()),
        mls_pubkey: ByteBuf::from(vec![0xDDu8; 32]),
        mlkem_ek: ByteBuf::from(vec![0x55u8; 1184]),
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    dispatch(
        &router,
        state.clone(),
        admin_actor,
        "fauna.bridges.provision_recipient_mls_pubkey",
        payload,
    )
    .await
    .expect("provision must succeed even with no mail domain registered");

    assert_eq!(
        state
            .db
            .lookup_exact_alias("fauna.test", "alice")
            .await
            .unwrap(),
        None,
        "no mail domain → no canonical alias (graceful no-op)",
    );
}

#[tokio::test]
async fn provision_canonical_alias_is_idempotent() {
    // Re-enabling (re-provisioning) must not error and must keep the alias
    // pointing at the same actor.
    let (router, state) = router_with_state().await;
    let admin_actor = [0xA0u8; 32];
    state.db.add_admin_actor(&admin_actor[..]).await.unwrap();
    let recipient = [0x42u8; 32];
    state
        .db
        .create_user_with_handle(&recipient, "free", "alice@fauna.test", None)
        .await
        .unwrap();
    state
        .db
        .add_mail_domain("fauna.test", true, "testing", "per_host", None, None)
        .await
        .unwrap();

    let req = ProvisionRecipientMlsPubkeyRequest {
        actor_id: ByteBuf::from(recipient.to_vec()),
        mls_pubkey: ByteBuf::from(vec![0xDDu8; 32]),
        mlkem_ek: ByteBuf::from(vec![0x55u8; 1184]),
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    for _ in 0..2 {
        dispatch(
            &router,
            state.clone(),
            admin_actor,
            "fauna.bridges.provision_recipient_mls_pubkey",
            payload.clone(),
        )
        .await
        .expect("re-provision must succeed");
    }
    assert_eq!(
        state
            .db
            .lookup_exact_alias("fauna.test", "alice")
            .await
            .unwrap(),
        Some(recipient),
    );
}

#[tokio::test]
async fn provision_does_not_clobber_another_actors_localpart() {
    // If `alice@fauna.test` is already owned by another actor, enabling mail
    // for a second user whose handle localpart is also "alice" must NOT steal
    // the address (mail-aliases.md § Kind 1 uniqueness on (local_domain,
    // pattern)). The pre-existing owner keeps it.
    let (router, state) = router_with_state().await;
    let admin_actor = [0xA0u8; 32];
    state.db.add_admin_actor(&admin_actor[..]).await.unwrap();
    state
        .db
        .add_mail_domain("fauna.test", true, "testing", "per_host", None, None)
        .await
        .unwrap();

    let owner = [0x77u8; 32];
    state
        .db
        .put_exact_alias("fauna.test", "alice", "exact", &owner)
        .await
        .unwrap();

    let newcomer = [0x42u8; 32];
    state
        .db
        .create_user_with_handle(&newcomer, "free", "alice@fauna.test", None)
        .await
        .unwrap();
    let req = ProvisionRecipientMlsPubkeyRequest {
        actor_id: ByteBuf::from(newcomer.to_vec()),
        mls_pubkey: ByteBuf::from(vec![0xDDu8; 32]),
        mlkem_ek: ByteBuf::from(vec![0x55u8; 1184]),
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    dispatch(
        &router,
        state.clone(),
        admin_actor,
        "fauna.bridges.provision_recipient_mls_pubkey",
        payload,
    )
    .await
    .expect("provision must succeed");

    assert_eq!(
        state
            .db
            .lookup_exact_alias("fauna.test", "alice")
            .await
            .unwrap(),
        Some(owner),
        "enabling mail must not clobber a localpart already owned by another actor",
    );
}

#[tokio::test]
async fn provision_recipient_mls_pubkey_rejects_malformed_pubkey() {
    let (router, state) = router_with_state().await;
    let admin_actor = [0xA0u8; 32];
    state.db.add_admin_actor(&admin_actor[..]).await.unwrap();

    // mls_pubkey must be exactly 32 bytes; any other length is malformed.
    let req = ProvisionRecipientMlsPubkeyRequest {
        actor_id: ByteBuf::from(vec![0x42u8; 32]),
        mls_pubkey: ByteBuf::from(vec![0xDDu8; 16]),
        mlkem_ek: ByteBuf::from(vec![0x55u8; 1184]),
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        admin_actor,
        "fauna.bridges.provision_recipient_mls_pubkey",
        payload,
    )
    .await
    .expect_err("16-byte mls_pubkey must be rejected as malformed");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

// ── S3c: post-quantum ML-KEM ek publication (additive sibling field) ──
//
// The recipient's 1184-byte ML-KEM-768 encapsulation key rides the same
// `provision`/`fetch` RPCs as the classical X25519 pubkey. The MTA pairs it
// with `pubkey` to seal mail with the X-Wing hybrid suite. Goal:
// `docs/goal/architecture/security/post-quantum.md` § Post-quantum key
// publication and derivation.

#[tokio::test]
async fn admin_provision_recipient_mlkem_ek_round_trip() {
    let (router, state) = router_with_state().await;
    let admin_actor = [0xA0u8; 32];
    state.db.add_admin_actor(&admin_actor[..]).await.unwrap();

    let recipient = [0x42u8; 32];
    let pubkey = [0xDDu8; 32];
    // Distinct, non-trivial 1184-byte ek so the round-trip is meaningful.
    let mlkem_ek: Vec<u8> = (0..1184).map(|i| (i % 251) as u8).collect();

    // 1. Admin provisions both the classical pubkey and the PQ ek.
    let provision_req = ProvisionRecipientMlsPubkeyRequest {
        actor_id: ByteBuf::from(recipient.to_vec()),
        mls_pubkey: ByteBuf::from(pubkey.to_vec()),
        mlkem_ek: ByteBuf::from(mlkem_ek.clone()),
        ..Default::default()
    };
    let provision_payload = Bytes::from(encode_canonical(&provision_req).unwrap().to_vec());
    let provision_reply_bytes = dispatch(
        &router,
        state.clone(),
        admin_actor,
        "fauna.bridges.provision_recipient_mls_pubkey",
        provision_payload,
    )
    .await
    .expect("admin provision with ek must succeed");
    let provision_reply: ProvisionReply = decode(&provision_reply_bytes).unwrap();
    assert!(provision_reply.ok);

    // 2. MTA fetch returns BOTH keys: classical pubkey + the PQ ek.
    let mta_actor = [0x11u8; 32];
    approve_mta(&state, mta_actor).await;
    let fetch_req = FetchRecipientMlsPubkeyRequest {
        actor_id: recipient.to_vec(),
        mail_new_ingest: false,
    };
    let fetch_payload = Bytes::from(encode_canonical(&fetch_req).unwrap().to_vec());
    let fetch_reply_bytes = dispatch(
        &router,
        state.clone(),
        mta_actor,
        "fauna.bridges.fetch_recipient_mls_pubkey",
        fetch_payload,
    )
    .await
    .expect("MTA fetch must succeed");
    let fetch_reply: FetchRecipientMlsPubkeyReply = decode(&fetch_reply_bytes).unwrap();
    let key = fetch_reply.key.expect("both halves present");
    assert_eq!(key.mls_pubkey.as_slice(), &pubkey[..]);
    assert_eq!(
        key.mlkem_ek.as_slice(),
        mlkem_ek.as_slice(),
        "fetched ek must equal provisioned ek",
    );
}

#[tokio::test]
async fn provision_rejects_malformed_mlkem_ek() {
    // The ek length is structurally checked at provision time (1184 B for
    // ML-KEM-768), so garbage is rejected before it can break the MTA's
    // X-Wing key assembly.
    let (router, state) = router_with_state().await;
    let admin_actor = [0xA0u8; 32];
    state.db.add_admin_actor(&admin_actor[..]).await.unwrap();

    let req = ProvisionRecipientMlsPubkeyRequest {
        actor_id: ByteBuf::from(vec![0x42u8; 32]),
        mls_pubkey: ByteBuf::from(vec![0xDDu8; 32]),
        // One byte short of a valid ML-KEM-768 ek.
        mlkem_ek: ByteBuf::from(vec![0x7u8; 1183]),
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        admin_actor,
        "fauna.bridges.provision_recipient_mls_pubkey",
        payload,
    )
    .await
    .expect_err("1183-byte mlkem_ek must be rejected as malformed");
    assert_eq!(err.code, "fauna.protocol.malformed");
}
