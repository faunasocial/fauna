//! **B5 tier_3** — the content-sealing-epochs design's § 4/§ 8 success bar,
//! end-to-end over a real in-process nest (real WS-RPC mint/renew/provision,
//! real SQLite storage, real HPKE grant unseal via
//! `fauna_capability_holder::Registry`, real per-epoch mail-record seal/open
//! — no fakes): a grant minted with window `[t1, t2]` cryptographically
//! cannot open content sealed after `t2`, even though the holder kept every
//! wrapped key it ever fetched; the owner's own client (MSEK-derived,
//! off-grant) still opens everything; `renew` extends the held epoch set.
//!
//! Data flow asserted:
//!   owner enables mail (MSEK) → publishes an epoch pubkey schedule
//!   (`fauna.bridges.provision_recipient_mls_pubkey` with `epoch_keys`, real
//!   B3a `actor_epoch_seal_keys` rows) → mints a BOUNDED
//!   `content.read{mail}` grant covering 3 epochs (`fauna.capabilities.mint`,
//!   B2's `mint_bounded_mail_grant`) → a real `fauna_capability_holder::
//!   Registry` fetches + HPKE-unseals it (fetch seam reads
//!   `CacheDb::fetch_capability_grants_for_holder` directly — the same
//!   verification shortcut `conformance_capability_trust_client.rs` uses; the
//!   bridge-authenticated fetch+open loop is proven separately by the Python
//!   `test_capability_rescore_drain.py`) → content sealed inside the held
//!   epoch set opens via `GrantSet::keys_for_mail_epoch`'s candidate chain;
//!   content sealed one epoch past it stays dark despite the holder trying
//!   every key it holds (nearest-first) → the owner derives the same
//!   post-window epoch's secret directly from MSEK and opens it anyway →
//!   `fauna.capabilities.renew` (B3c's renew arm) appends the missing
//!   epoch's wrap → the SAME holder registry, refreshed, now opens it too.
//!
//! Content is planted via the new `test-hooks`-gated
//! `POST /api/v1/test/content/inject_epoch_sealed_mail` (seals to the
//! actor's REAL published epoch pubkey via the same
//! `bridge_routing_handlers::seal_recipient_blob` production call, at a
//! test-chosen timestamp) rather than waiting real wall-clock weeks for an
//! epoch boundary to pass — the design's epoch index is `floor(unix_secs /
//! 604800)`, so crossing one for real would take the tier_3 seven days.
//!
//! Harness mirrors `conformance_capability_trust_client.rs` (in-process
//! `axum::serve` + `AppState` skeleton + `connected_client`), extended with
//! `bridge_routing_handlers::register_bridge_routing_handlers` (for
//! `provision_recipient_mls_pubkey`) and built with `test-hooks` so
//! `build_router` merges the injection endpoint.

mod common;
use common::connected_client;

use std::sync::Arc;

use base64::Engine as _;
use fauna_client_capabilities::{bounded_mail_renewal_keys, mint_bounded_mail_grant};
use fauna_core::data::MailConfig;
use fauna_core::identity::ActorKeypair;
use fauna_mls::wrapped_blob::{
    GrantWindow, derive_recipient_epoch_hpke_keypair, derive_recipient_epoch_xwing_keypair,
    derive_recipient_hpke_keypair, derive_recipient_mail_epoch_capability_secret,
    derive_recipient_xwing_keypair, mail_sealing_epoch_of, unseal_mail_record_hybrid,
};
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::bridge_routing::{EpochSealKey, ProvisionRecipientMlsPubkeyRequest};
use fauna_protocol::wrapped_blob::{
    MintGrantReply, MintGrantRequest, ProvisionReply, RenewGrantReply, RenewGrantRequest,
};
use serde_bytes::ByteBuf;

/// One mail content-sealing epoch, in seconds (mirrors the design's
/// hard-coded weekly constant; kept as a local literal so this test doesn't
/// need `fauna-mls`'s `test-helpers`/internal visibility for the constant —
/// `mail_sealing_epoch_of` is the function under test, not re-derived here).
const WEEK: u64 = 7 * 24 * 60 * 60;

const OWNER_SEED: [u8; 32] = [51u8; 32];
const OWNER_MSEK: [u8; 32] = [0x9Du8; 32];

fn now_epoch() -> i64 {
    fauna_core::data::Timestamp::now_secs()
}

/// Spin a real in-process nest serving auth, discovery, `fauna.config` CAS,
/// the capability kinds (`mint`/`fetch`/`renew`), and
/// `provision_recipient_mls_pubkey` — built with `test-hooks` so
/// `fauna_nest::build_router` also merges
/// `POST /api/v1/test/content/inject_epoch_sealed_mail`.
async fn start_epoch_nest() -> (String, Arc<AppState>, tempfile::TempDir) {
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
            fauna_nest::bridge_blob_handlers::register_capability_handlers(&mut b);
            fauna_nest::bridge_routing_handlers::register_bridge_routing_handlers(&mut b);
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
        enforce_tier_quotas: Arc::new(tokio::sync::RwLock::new(true)),
        backup_service: Some(backup_svc),
        ..AppState::for_test(db)
    });

    let app = fauna_nest::build_router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{authority}"), state, tmp)
}

/// Try every candidate key (nearest-epoch-first, per `keys_for_mail_epoch`)
/// against `envelope`, returning the first successful open — mirrors
/// `fauna_ffi::open_mail_record_with_key`'s 2432-byte hybrid dispatch (the
/// capability payload contract every mail scope key carries) without adding
/// a `fauna-ffi` dev-dependency just for this one call shape.
fn try_open_with_candidates(
    envelope: &fauna_mls::wrapped_blob::MailRecordEnvelope,
    candidates: &[&[u8]],
) -> Option<Vec<u8>> {
    for key in candidates {
        if key.len() != 32 + fauna_mls::wrapped_blob::MLKEM768_DECAPS_KEY_LEN {
            continue;
        }
        let mut x25519 = [0u8; 32];
        x25519.copy_from_slice(&key[..32]);
        let mut dk = [0u8; fauna_mls::wrapped_blob::MLKEM768_DECAPS_KEY_LEN];
        dk.copy_from_slice(&key[32..]);
        if let Ok(pt) = unseal_mail_record_hybrid(envelope, &x25519, &dk) {
            return Some(pt);
        }
    }
    None
}

#[tokio::test]
async fn windowed_mail_grant_opens_in_window_dark_past_held_epochs_renew_extends() {
    let (base, state, _tmp) = start_epoch_nest().await;
    let http = reqwest::Client::new();

    // ── Owner: enable mail (real config CAS) then publish a PUBLIC epoch
    // schedule wider than the grant this test mints — exactly the real
    // shape: a client's own publish horizon (~26 epochs) is independent of
    // any one grant's window.
    let owner = ActorKeypair::from_secret(OWNER_SEED);
    let owner_id = owner.actor_id().0;
    state
        .db
        .create_user(&owner_id, "free", "epochowner")
        .await
        .unwrap();

    // The owner's mail custody — mail enabled (the MSEK the bounded grant's
    // epoch wraps derive from).
    let mail = MailConfig {
        msek: Some(OWNER_MSEK.into()),
        mail_enabled: Some(true),
        ..MailConfig::default()
    };

    let e_now = mail_sealing_epoch_of(now_epoch() as u64);
    // Held by the GRANT: [e_now, e_now+2]. Published (schedule): [e_now,
    // e_now+3] — one epoch WIDER, so the "post-window" record below is
    // genuinely sealable (a real published key exists) yet the grant never
    // holds that epoch's wrapped key.
    let (_, standing_pubkey) = derive_recipient_hpke_keypair(&OWNER_MSEK);
    let epoch_keys: Vec<EpochSealKey> = (e_now..=e_now + 3)
        .map(|e| {
            let (_, pk) = derive_recipient_epoch_hpke_keypair(&OWNER_MSEK, e);
            let ek = derive_recipient_epoch_xwing_keypair(&OWNER_MSEK, e)
                .public
                .mlkem_encaps_key()
                .to_vec();
            EpochSealKey {
                epoch: e,
                mls_pubkey: ByteBuf::from(pk.to_vec()),
                mlkem_ek: ByteBuf::from(ek),
            }
        })
        .collect();
    let nest = connected_client(&base, ActorKeypair::from_secret(OWNER_SEED)).await;
    let provision_req = ProvisionRecipientMlsPubkeyRequest {
        actor_id: ByteBuf::from(owner_id.to_vec()),
        mls_pubkey: ByteBuf::from(standing_pubkey.to_vec()),
        mlkem_ek: ByteBuf::from(
            derive_recipient_xwing_keypair(&OWNER_MSEK)
                .public
                .mlkem_encaps_key()
                .to_vec(),
        ),
        epoch_keys: Some(epoch_keys),
    };
    let reply: ProvisionReply = nest
        .request(
            "fauna.bridges.provision_recipient_mls_pubkey",
            provision_req,
        )
        .await
        .expect("publish the epoch schedule");
    assert!(reply.ok);

    // ── Mint a BOUNDED content.read{mail} grant to a fresh holder, window =
    // exactly epochs [e_now, e_now+2].
    let grant_id = [0x51u8; 16];
    let (holder_sec, holder_pub) = fauna_mls::wrapped_blob::generate_x25519_keypair();
    let window = GrantWindow(e_now * WEEK, (e_now + 2) * WEEK + WEEK - 1);
    let window_end = window.1;
    let blob = mint_bounded_mail_grant(
        &mail,
        &owner_id,
        &grant_id,
        &holder_pub,
        None,
        window,
        false,
    )
    .expect("mint bounded mail grant");
    let blob_bytes = blob.to_canonical_bytes().expect("encode grant blob");
    let mint_reply: MintGrantReply = nest
        .request(
            "fauna.capabilities.mint",
            MintGrantRequest {
                grant_blob: ByteBuf::from(blob_bytes),
                extra: Default::default(),
            },
        )
        .await
        .expect("mint the bounded grant");
    assert!(mint_reply.ok);
    let stored_grant_id = mint_reply.grant_id.to_vec();

    // ── The holder's registry: a real fauna_capability_holder::Registry,
    // fetching directly off the nest's DB (the verification shortcut
    // conformance_capability_trust_client.rs also uses — the authenticated
    // bridge-side fetch+open loop is the Python tier_3's job) and HPKE-
    // unsealing for real.
    let db_for_fetch = state.db.clone();
    let registry = fauna_capability_holder::Registry::new(
        holder_sec,
        None,
        Box::new(move || {
            let db = db_for_fetch.clone();
            Box::pin(async move {
                db.fetch_capability_grants_for_holder(&holder_pub, now_epoch())
                    .await
                    .map_err(|e| e.to_string())
            })
        }),
    );
    registry.refresh().await.expect("holder refresh");
    assert_eq!(
        registry.current().len(),
        1,
        "the holder holds the one grant"
    );

    // ── Plant IN-WINDOW content: epoch e_now+1 (held).
    let in_window_ts = (e_now + 1) * WEEK + 100;
    let resp = http
        .post(format!(
            "{base}/api/v1/test/content/inject_epoch_sealed_mail"
        ))
        .json(&serde_json::json!({
            "actor_id": hex::encode(owner_id),
            "epoch": e_now + 1,
            "raw_rfc5322_b64": base64::engine::general_purpose::STANDARD
                .encode(b"Subject: in-window\r\n\r\nheld epoch body"),
            "timestamp": in_window_ts as i64,
        }))
        .send()
        .await
        .expect("inject in-window mail");
    assert!(resp.status().is_success(), "{:?}", resp.text().await);
    let in_window_msg = resp.json::<serde_json::Value>().await.unwrap();
    let in_window_msg_id: [u8; 32] = hex::decode(in_window_msg["message_id_hex"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();

    // ── Plant POST-WINDOW content: epoch e_now+3 (published, but one epoch
    // past the grant's held set — the crypto bound's whole point).
    let post_window_ts = (e_now + 3) * WEEK + 100;
    let resp = http
        .post(format!(
            "{base}/api/v1/test/content/inject_epoch_sealed_mail"
        ))
        .json(&serde_json::json!({
            "actor_id": hex::encode(owner_id),
            "epoch": e_now + 3,
            "raw_rfc5322_b64": base64::engine::general_purpose::STANDARD
                .encode(b"Subject: post-window\r\n\r\nnever-held epoch body"),
            "timestamp": post_window_ts as i64,
        }))
        .send()
        .await
        .expect("inject post-window mail");
    assert!(resp.status().is_success(), "{:?}", resp.text().await);
    let post_window_msg = resp.json::<serde_json::Value>().await.unwrap();
    let post_window_msg_id: [u8; 32] =
        hex::decode(post_window_msg["message_id_hex"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();

    // ── Read both sealed envelopes back out of the real segment store.
    let (in_window_env, _) = fauna_nest::segments::mail::read_record_with_floor(
        &state.mail_segments,
        &state.db,
        &owner_id,
        &in_window_msg_id,
    )
    .await
    .expect("read in-window record")
    .expect("in-window record present");
    let (post_window_env, _) = fauna_nest::segments::mail::read_record_with_floor(
        &state.mail_segments,
        &state.db,
        &owner_id,
        &post_window_msg_id,
    )
    .await
    .expect("read post-window record")
    .expect("post-window record present");
    let in_window_envelope = fauna_mls::wrapped_blob::MailRecordEnvelope::from_canonical_bytes(
        &in_window_env.encrypted_body,
    )
    .expect("decode in-window envelope");
    let post_window_envelope = fauna_mls::wrapped_blob::MailRecordEnvelope::from_canonical_bytes(
        &post_window_env.encrypted_body,
    )
    .expect("decode post-window envelope");

    // ── THE BOUND: the holder opens in-window content...
    let set = registry.current();
    let in_window_candidates =
        set.keys_for_mail_epoch(&owner_id, None, in_window_ts, now_epoch() as u64);
    assert!(
        !in_window_candidates.is_empty(),
        "the holder holds a candidate key for the in-window epoch"
    );
    let opened = try_open_with_candidates(&in_window_envelope, &in_window_candidates)
        .expect("the holder opens in-window content");
    assert_eq!(opened, b"Subject: in-window\r\n\r\nheld epoch body");

    // ...but the post-window record stays dark despite the holder trying
    // every key it holds (the fallback candidates: e_now+2, e_now+1, e_now —
    // all <= target — per the stale-schedule tolerance; none is the record's
    // true epoch-3 key, so every open fails).
    let post_window_candidates =
        set.keys_for_mail_epoch(&owner_id, None, post_window_ts, now_epoch() as u64);
    assert_eq!(
        post_window_candidates.len(),
        3,
        "3 fallback candidates (e_now, e_now+1, e_now+2) — none is epoch e_now+3's key"
    );
    assert!(
        try_open_with_candidates(&post_window_envelope, &post_window_candidates).is_none(),
        "the holder must NOT be able to open content sealed one epoch past its held set"
    );

    // ── The OWNER's own client still opens everything: it derives the
    // epoch-3 secret directly from MSEK (never through the grant) and opens
    // the same post-window record the holder cannot.
    let owner_secret = derive_recipient_mail_epoch_capability_secret(&OWNER_MSEK, e_now + 3);
    let owner_opened = try_open_with_candidates(&post_window_envelope, &[&owner_secret])
        .expect("the owner's own MSEK-derived key opens everything");
    assert_eq!(
        owner_opened,
        b"Subject: post-window\r\n\r\nnever-held epoch body"
    );

    // ── RENEW: extend the grant to cover epoch e_now+3 too.
    let new_window_end = (e_now + 3) * WEEK + WEEK - 1;
    let appended = bounded_mail_renewal_keys(
        &mail,
        &owner_id,
        &holder_pub,
        None,
        window_end,
        new_window_end,
        None,
    )
    .expect("compute the renewal's appended epoch wraps");
    assert_eq!(
        appended.len(),
        1,
        "extending by exactly one epoch appends exactly one wrap"
    );
    let renew_reply: RenewGrantReply = nest
        .request(
            "fauna.capabilities.renew",
            RenewGrantRequest {
                grant_id: ByteBuf::from(stored_grant_id.clone()),
                new_epoch_start: None,
                new_epoch_end: new_window_end,
                appended_keys: appended
                    .iter()
                    .map(|k| ByteBuf::from(k.to_canonical_bytes().expect("encode appended key")))
                    .collect(),
                extra: Default::default(),
            },
        )
        .await
        .expect("renew the grant to cover epoch e_now+3");
    assert!(renew_reply.ok);

    // ── The SAME holder registry, refreshed, now opens the previously-dark
    // record too — renew delivers the next window's keys (design § 5).
    registry
        .refresh()
        .await
        .expect("holder re-fetch after renew");
    let set = registry.current();
    let post_renew_candidates =
        set.keys_for_mail_epoch(&owner_id, None, post_window_ts, now_epoch() as u64);
    let post_renew_opened = try_open_with_candidates(&post_window_envelope, &post_renew_candidates)
        .expect("after renew, the holder opens the previously-dark post-window content");
    assert_eq!(
        post_renew_opened,
        b"Subject: post-window\r\n\r\nnever-held epoch body"
    );
}
