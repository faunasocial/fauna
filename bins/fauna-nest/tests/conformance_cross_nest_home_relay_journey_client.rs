//! **The home-with-public-relay beginner journey — production-faithful,
//! client-driven** (tier_3). One composed end-to-end test that walks the real
//! product path a non-technical user follows — **no seal-helper, no raw
//! `fauna.pair.add`** — and proves relayed mail is actually **readable** on the
//! home box with the auto-provisioned credential.
//!
//! This is the production-faithful companion to the seal-helper tier_4
//! `tests/e2e-unified/tests/platform/docker/test_mail_relay_two_nest.py`, which
//! provisions via the `provision_relay_user` **seal-helper** (mints an MSEK
//! test-side + calls the raw `provision_*` RPCs on both boxes) and seeds pairing
//! with **raw `fauna.pair.add`** — bypassing the real client `enable_mail` /
//! `LinkBoth` path. That test is the crypto-level guard and stays; this is the
//! product-path guard (`docs/goal/architecture/nest/deployment-home-with-public-relay.md`
//! § Done definition checkbox 5).
//!
//! It composes three existing client-driven / relay conformance proofs into one
//! journey and **closes the loop neither of them closes** — that real sealed
//! mail, relayed through, opens with the key the home box actually stored:
//!
//!   * `conformance_cross_nest_relay_provision_client.rs` proves the home box
//!     ends up with the **same MSEK-derived recipient pubkey** as the public box
//!     after a `LinkBoth` — but delivers no mail, so never opens anything.
//!   * `conformance_cross_nest_mail_relay.rs` proves a sealed record relays
//!     verbatim + the public box holds no readable copy — but seeds **opaque**
//!     bytes (no real seal), so never decrypts.
//!
//! The journey here (steps map to `deployment-home-with-public-relay.md`
//! § Pairing + § Inbound mail + § Done definition):
//!
//!   1. Two of the user's own nests: `A` = the public relay box, `B` = the home
//!      box.
//!   2. **Enable mail on `A` via the onboarding auto-mint path** — the shared
//!      `MailSettingsMachine::enable_mail_with_generated_password` (the exact
//!      helper the onboarding-autocomplete launch glue runs), which mints the
//!      fleet MSEK, registers `A`'s recipient pubkey, and returns the generated
//!      password **once** (no hand-entered credential). *(The bridge
//!      auto-approval half of onboarding-autocomplete is separately tier_3-proven
//!      by `tests/e2e-unified/tests/api/test_mail_bridge_approval.py`; this
//!      journey exercises the mailbox-mint half + the relay it feeds.)*
//!   3. **One `LinkBoth`** over the shared
//!      `build_linked_nests_machine_with_mail_relay` seeds the reciprocal pairing
//!      rows on **both** nests (no raw `fauna.pair.add`) and the post-link hook
//!      **auto-provisions `B`'s mailbox** with `A`'s MSEK (read recipe, no
//!      submission token — `B` runs no MTA).
//!   4. A **genuinely-sealed** inbound record lands on `A` — the MTA-ingest
//!      analogue: `seal_to_recipient` to `A`'s *published* recipient pubkey (the
//!      exact Rust core the Go MTA's `EncryptToRecipient` wraps via FFI), plus
//!      the INBOX placement a real ingest writes.
//!   5. **The real residential worker cycle** (`run_sync_cycle`, gated on the
//!      both-ends pairing rows the `LinkBoth` seeded) relays it `A`→`B`, places it
//!      in `B`'s INBOX (IMAP-visible), and acks → `A` holds **no readable copy**
//!      (segment purged + INBOX placement expunged).
//!   6. **The home box reads it decrypted** — open `B`'s *stored* wrapped-MSEK
//!      with the generated password (the production MDA-AUTH path), derive the
//!      recipient secret, and open the relayed seal → byte-equal the sent body. A
//!      wrong password must fail (negative control).
//!
//! What only this test catches: that the *whole chain* lines up — the password
//! `enable_mail_with_generated_password` generated, sealed under it onto the home
//! box by the auto-provision hook, unwraps to the MSEK whose derived secret opens
//! mail the MTA sealed to the *public* box's *published* pubkey. A drift at any
//! link (the hook re-mints a fresh MSEK instead of reusing the fleet one; it
//! seals `B`'s MSEK under the wrong credential; the relay corrupts the envelope)
//! passes the narrower per-stage tests but leaves mail "delivered but unreadable"
//! here.
//!
//! *(Step 6 of the README journey — a **new non-admin user** auto-getting a
//! mailbox via the deployment-wide default-on policy — is
//! the default-on-mail track's Done definition, a single-box scenario
//! orthogonal to the relay; not duplicated here.)*
//!
//! Mirrors `conformance_cross_nest_relay_provision_client.rs`'s harness (auth +
//! discovery + pairing + `BackupService` + bridge-blob + bridge-routing),
//! adding the **federation** router + handlers (the `mail_pull`/`mail_ack`
//! channel the residential worker drains over) so the relay resolves over the
//! real wire.

mod common;
use common::connected_client;

use std::sync::Arc;

use zeroize::Zeroizing;

use fauna_client_mail_settings::rpc_glue::{
    build_linked_nests_machine_with_mail_relay, build_mail_settings_machine,
};
use fauna_client_mail_settings::{Credential, derive_credential_id};
use fauna_client_pair::LinkedNestsAction;
use fauna_core::identity::ActorKeypair;
use fauna_mail::segments::{
    MAIL_FLOOR_FORMAT_VERSION, MailFloorMetadata, MailRecordEnvelope as SegmentMailEnvelope,
};
use fauna_mls::wrapped_blob::{
    MailRecordEnvelope as SealedMailEnvelope, WrappedMsekBlob, derive_recipient_hpke_keypair,
    seal_to_recipient, unseal_mail_record, unseal_wrapped_msek,
};
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::nest_sync_worker::{SyncCycleOutcome, SyncWatermarks, run_sync_cycle};
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;

/// One of the user's nests carrying **every** handler set the journey touches:
/// auth-bootstrap + anonymous discovery (`fauna.nest.info`) + pairing (so
/// `LinkBoth` seeds both ends) + a real `BackupService` +
/// bridge-blob + bridge-routing (the client mail-enable + relay-provision RPCs) +
/// the **federation** router (the `mail_pull`/`mail_ack` channel the residential
/// worker drains over). Returns `(http_base, state, tempdir)` — the `TempDir`
/// guard keeps the `BackupService` blob dir alive for the test's lifetime.
async fn start_journey_nest() -> (String, Arc<AppState>, tempfile::TempDir) {
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
        federation_router: Arc::new({
            let mut b = fauna_nest::federation_router::FederationRouter::builder();
            fauna_nest::federation_handlers::register_federation_handlers(&mut b);
            b.build()
        }),
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

/// An `accept`/non-own-submission floor (⇒ INBOX placement). Only `received_at`
/// is meaningful; mirror of the in-crate `segments::test_helpers::floor`, which
/// is `pub(crate)` and so unreachable from an integration test.
fn inbox_floor(received_at: i64) -> MailFloorMetadata {
    MailFloorMetadata {
        format_version: MAIL_FLOOR_FORMAT_VERSION,
        received_at,
        timestamp: received_at / 1000,
        ciphertext_size: 0,
        sender_domain: "external.test".to_string(),
        spam_disposition: "accept".to_string(),
        is_own_submission: false,
        spf: "pass".into(),
        dkim: "pass".into(),
        dmarc: "pass".into(),
        dmarc_policy: "reject".into(),
        arc: "pass".into(),
        spam_score: 0,
        seq: 0,
        continuation_role: fauna_mail::segments::CONTINUATION_ROLE_NORMAL,
        // Struct-update so the next additive floor field doesn't break this
        // fixture (the reason MailFloorMetadata carries a Default at all).
        ..Default::default()
    }
}

#[tokio::test]
async fn home_relay_beginner_journey_relayed_mail_is_readable_on_home_box() {
    // ── Step 1: two of the user's own nests ──────────────────────────────────
    let (a_base, a_state, _a_tmp) = start_journey_nest().await; // public relay box
    let (b_base, b_state, _b_tmp) = start_journey_nest().await; // home box

    // Alice's single identity, registered on BOTH (the home-relay premise: one
    // user, two of their own boxes). She is the admin of the public box she
    // claimed — so `enable_mail`'s `set_mail_enabled(true)` flips the deployment
    // mail subsystem on for real, rather than being swallowed as a non-admin
    // `Rejected`.
    let alice = ActorKeypair::generate();
    let secret = *alice.secret_bytes();
    let alice_id = alice.actor_id();
    a_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();
    a_state.db.add_admin_actor(&alice_id.0).await.unwrap();
    b_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();

    // ── Step 2: enable mail on A via the ONBOARDING AUTO-MINT path ───────────
    // The shared helper the onboarding-autocomplete launch glue runs — mints the
    // fleet MSEK, provisions A's mailbox, persists the MSEK into A's `fauna.state.mail` plane entry,
    // and returns the generated password ONCE (no hand-entered credential).
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
    let password = mail
        .enable_mail_with_generated_password("Default".into())
        .await
        .expect("onboarding auto-mint enables mail on the public relay box");
    // The deployment mail subsystem really flipped on (admin-class enable).
    assert_eq!(
        a_state.db.get_mail_enabled().await.unwrap(),
        Some(true),
        "the admin's enable flips the deployment mail subsystem on"
    );

    // A's recipient pubkey is now the MSEK-derived standing HPKE key — the pubkey
    // an inbound MTA seals to.
    let pubkey_a = a_state
        .db
        .get_actor_mls_pubkey(&alice_id.0)
        .await
        .unwrap()
        .expect("public box registered a recipient pubkey at enable-mail");

    // ── Step 3: ONE LinkBoth → both pairing rows + auto-provision B ──────────
    let linked = build_linked_nests_machine_with_mail_relay(
        client_a.clone(),
        ActorKeypair::from_secret(secret),
        Arc::new(fauna_client_config::NoLedgerStore),
        Arc::clone(&mail_store),
    );
    linked
        .dispatch(LinkedNestsAction::LinkBoth {
            other_nest_url: b_base.clone(),
            capabilities: vec![], // → default_self_sync() (incl. mail_pull)
            expires_at: None,
            label: Some("public relay".into()),
        })
        .await
        .expect("LinkBoth + auto-provision should succeed");

    let a_nest_id = a_state.nest_identity.public_key_bytes();
    let b_nest_id = b_state.nest_identity.public_key_bytes();
    // The link seeded BOTH reciprocal pairing rows over the real client machine —
    // no raw `fauna.pair.add` in this test's asserted path.
    assert!(
        a_state.db.is_paired(&alice_id.0, &b_nest_id).await.unwrap(),
        "public box authorizes the home box to pull (A→B pull-gate row)"
    );
    assert!(
        b_state.db.is_paired(&alice_id.0, &a_nest_id).await.unwrap(),
        "home box holds its own pairing row — what makes its relay worker fire"
    );
    // The home box was auto-provisioned with the SAME MSEK-derived recipient
    // pubkey (a different MSEK would silently break home-box decrypt).
    let pubkey_b = b_state
        .db
        .get_actor_mls_pubkey(&alice_id.0)
        .await
        .unwrap()
        .expect("home box was auto-provisioned with a recipient pubkey on link");
    assert_eq!(
        pubkey_b, pubkey_a,
        "home box reuses the public box's MSEK-derived recipient pubkey"
    );
    // …with the read recipe but NO submission token (the home box runs no MTA).
    let cred_id = derive_credential_id("Default", &[]);
    assert!(
        b_state
            .db
            .get_mls_snapshot_blob(&alice_id.0)
            .await
            .unwrap()
            .is_some(),
        "home box got the MLS snapshot"
    );
    let b_wrapped_msek = b_state
        .db
        .get_wrapped_mls_blob(&alice_id.0, &cred_id)
        .await
        .unwrap()
        .expect("home box got the wrapped-MSEK read credential");
    assert!(
        b_state
            .db
            .get_wrapped_submission_token(&alice_id.0, &cred_id)
            .await
            .unwrap()
            .is_none(),
        "home box must not get a submission token — it runs no MTA"
    );

    // ── Step 4: a GENUINELY-SEALED inbound record lands on A ─────────────────
    // The MTA-ingest analogue: seal a real RFC 5322 body to A's PUBLISHED
    // recipient pubkey (`seal_to_recipient` is the exact Rust core the Go MTA's
    // `EncryptToRecipient` wraps via FFI), append to A's `__mail`, and write the
    // INBOX placement a real ingest writes.
    const BODY: &[u8] = b"From: External Sender <sender@external.test>\r\n\
        To: alice@public.test\r\n\
        Subject: home-relay journey\r\n\
        \r\n\
        Hello from the production-faithful home-relay journey test.\r\n";
    let sealed = seal_to_recipient(BODY, &pubkey_a)
        .expect("seal inbound body to A's published recipient pubkey")
        .to_canonical_bytes()
        .expect("encode sealed envelope");
    let sealed_hint = seal_to_recipient(b"index-hint", &pubkey_a)
        .expect("seal hint")
        .to_canonical_bytes()
        .expect("encode sealed hint");
    let rid = fauna_nest::segments::mail::append_record(
        &a_state.mail_segments,
        &a_state.db,
        &alice_id.0,
        // Genuine seals carried verbatim into the segment (S6.12b typed gate).
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(sealed.clone()),
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(sealed_hint),
        inbox_floor(1_715_000_000_000),
    )
    .await
    .expect("public-side inbound mail append")
    .cid
    .digest();
    a_state
        .db
        .ensure_bridge_imap_mailboxes(&alice_id.0)
        .await
        .unwrap();
    a_state
        .db
        .place_inbound_mail(
            &alice_id.0,
            &rid,
            "INBOX",
            1_715_000_000,
            "",
            "external.test",
            true,
        )
        .await
        .unwrap();
    assert_eq!(
        a_state
            .db
            .count_bridge_imap_mailbox(&alice_id.0, "INBOX")
            .await
            .unwrap()
            .0,
        1,
        "the inbound message is in the public box's INBOX before the relay"
    );

    // ── Step 5: the REAL residential worker cycle relays A→B + purges A ──────
    // `run_sync_cycle` is the production entry point — gated on `B`'s own pairing
    // row, which the `LinkBoth` above seeded, and dialing the `nest_url` that
    // row recorded for `A` (no config-file pull target exists). The
    // public-side pull-gate row alone would not make it fire; both ends are
    // present, so it drains.
    // B is the home box: its admin set the NAT mode private at onboarding.
    *b_state.node_mode.write().await = fauna_nest::config::NodeMode::Private;
    let mut watermarks = SyncWatermarks::new();
    let mut mail_watermarks = SyncWatermarks::new();
    let outcome = run_sync_cycle(&b_state, &mut watermarks, &mut mail_watermarks).await;
    assert_eq!(
        outcome,
        SyncCycleOutcome::Processed(1),
        "the home box's worker relays the one paired actor (both pairing rows present)"
    );

    // The relayed record is IMAP-visible on B (placed in INBOX, not merely stored
    // in `__mail`).
    assert_eq!(
        b_state
            .db
            .count_bridge_imap_mailbox(&alice_id.0, "INBOX")
            .await
            .unwrap()
            .0,
        1,
        "the relayed record is placed in the home box's INBOX (IMAP-visible)"
    );
    // The public relay box holds NO readable copy after ack: the segment is
    // purged AND the INBOX placement is expunged.
    assert!(
        fauna_nest::segments::mail::read_after_seq(
            &a_state.mail_segments,
            &a_state.db,
            &alice_id.0,
            0,
            100,
        )
        .await
        .unwrap()
        .is_empty(),
        "public relay box holds no readable mail segment after ack + purge"
    );
    assert_eq!(
        a_state
            .db
            .count_bridge_imap_mailbox(&alice_id.0, "INBOX")
            .await
            .unwrap()
            .0,
        0,
        "public relay box shows no message in INBOX after the relay purge"
    );

    // ── Step 6: the home box reads it DECRYPTED ──────────────────────────────
    // The production MDA-AUTH read path, fully in-process: open the home box's
    // *stored* wrapped-MSEK with the generated password, derive the recipient
    // secret, and open the relayed seal → byte-equal the sent body. This is the
    // loop neither per-stage test closes — that the auto-provisioned credential
    // genuinely yields a working read key for mail the MTA sealed to the public
    // box's published pubkey.
    let blob = WrappedMsekBlob::from_canonical_bytes(&b_wrapped_msek)
        .expect("decode the home box's stored wrapped-MSEK blob");
    let pw_cred = Credential::Plain(Zeroizing::new(password.as_str().as_bytes().to_vec()));
    let recovered_msek = unseal_wrapped_msek(&blob, &pw_cred.as_input())
        .expect("home box's wrapped-MSEK opens with the generated password");
    let (recipient_secret, recovered_pubkey) = derive_recipient_hpke_keypair(&recovered_msek);
    assert_eq!(
        recovered_pubkey, pubkey_a,
        "the recovered MSEK derives the very recipient keypair the MTA sealed to"
    );

    // Read the verbatim relayed segment off B, peel the segment envelope, and open
    // the inner HPKE seal with the recovered secret.
    let stored = fauna_nest::segments::mail::read_envelope(
        &b_state.mail_segments,
        &b_state.db,
        &alice_id.0,
        &rid,
    )
    .await
    .expect("read the relayed segment on the home box")
    .expect("the relayed record is present on the home box");
    let segment_env = SegmentMailEnvelope::decode(&stored).expect("decode segment envelope");
    let sealed_inner = SealedMailEnvelope::from_canonical_bytes(&segment_env.encrypted_body)
        .expect("decode the inner HPKE seal");
    let opened = unseal_mail_record(&sealed_inner, &recipient_secret)
        .expect("home box opens the relayed mail with the auto-provisioned credential");
    assert_eq!(
        opened.as_slice(),
        BODY,
        "relayed mail decrypts byte-for-byte back to the externally-sent body"
    );

    // Negative control — a wrong password must NOT open it, so the open above
    // passed for the right reason (correct credential), not by accident.
    let (wrong_secret, _) = derive_recipient_hpke_keypair(&[0xABu8; 32]);
    assert!(
        unseal_mail_record(&sealed_inner, &wrong_secret).is_err(),
        "a non-recipient secret must fail to open the relayed mail"
    );
}

/// **The home-with-public-relay journey — relayed mail is stored SEALED at
/// rest, never as literal plaintext.** The storage-mode axis is retired
/// (`docs/goal/architecture/nest/storage-modes.md`): there is no "plaintext
/// home box" to commit any more — every nest seals from first boot, and the
/// plaintext-MSEK deposit leg is gone from the client, the wire and the
/// nest alike (the kind left the wire 2026-09-24; the
/// `actor_plaintext_msek` custody table no longer exists). Every nest is on
/// the ONE uniform path the journey above
/// (`home_relay_beginner_journey_relayed_mail_is_readable_on_home_box`)
/// already exercises. What THIS test still pins, distinctly from the journey
/// above: the relayed record's stored bytes are never literal plaintext —
/// `segment_env.encrypted_body` stays the sealed HPKE envelope, opened only
/// via the recipient's own MSEK-derived secret (the client/MDA read path), so
/// no data is lost.
#[tokio::test]
async fn home_relay_journey_stores_relayed_mail_sealed_never_literal_plaintext() {
    // ── Step 1: two of the user's own nests ──────────────────────────────────
    let (a_base, a_state, _a_tmp) = start_journey_nest().await; // public relay box
    let (b_base, b_state, _b_tmp) = start_journey_nest().await; // home box

    let alice = ActorKeypair::generate();
    let secret = *alice.secret_bytes();
    let alice_id = alice.actor_id();
    a_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();
    a_state.db.add_admin_actor(&alice_id.0).await.unwrap();
    b_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();

    // ── Step 2: enable mail on A (mints the fleet MSEK, registers A's pubkey) ──
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
    let password = mail
        .enable_mail_with_generated_password("Default".into())
        .await
        .expect("onboarding auto-mint enables mail on the public relay box");
    let pubkey_a = a_state
        .db
        .get_actor_mls_pubkey(&alice_id.0)
        .await
        .unwrap()
        .expect("public box registered a recipient pubkey at enable-mail");

    // ── Step 3: ONE LinkBoth → auto-provisions B's read recipe ───────────────
    let linked = build_linked_nests_machine_with_mail_relay(
        client_a.clone(),
        ActorKeypair::from_secret(secret),
        Arc::new(fauna_client_config::NoLedgerStore),
        Arc::clone(&mail_store),
    );
    linked
        .dispatch(LinkedNestsAction::LinkBoth {
            other_nest_url: b_base.clone(),
            capabilities: vec![], // → default_self_sync() (incl. mail_pull)
            expires_at: None,
            label: Some("public relay".into()),
        })
        .await
        .expect("LinkBoth + auto-provision should succeed");

    // There is no plaintext-MSEK deposit leg to run any more: the kind is off
    // the wire and the `actor_plaintext_msek` custody table + its DB methods
    // no longer exist to query — there is nothing left to assert here beyond the journey
    // completing, which the steps below prove.

    // ── Step 4: a GENUINELY-SEALED inbound record lands on A ─────────────────
    const BODY: &[u8] = b"From: External Sender <sender@external.test>\r\n\
        To: alice@public.test\r\n\
        Subject: sealed at rest on receipt\r\n\
        \r\n\
        Relayed sealed, never opened on receipt, never stored as literal plaintext.\r\n";
    let sealed = seal_to_recipient(BODY, &pubkey_a)
        .expect("seal inbound body to A's published recipient pubkey")
        .to_canonical_bytes()
        .expect("encode sealed envelope");
    let sealed_hint = seal_to_recipient(b"index-hint", &pubkey_a)
        .expect("seal hint")
        .to_canonical_bytes()
        .expect("encode sealed hint");
    let rid = fauna_nest::segments::mail::append_record(
        &a_state.mail_segments,
        &a_state.db,
        &alice_id.0,
        // Genuine seals carried verbatim into the segment (S6.12b typed gate).
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(sealed),
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(sealed_hint),
        inbox_floor(1_715_000_000_000),
    )
    .await
    .expect("public-side inbound mail append")
    .cid
    .digest();
    a_state
        .db
        .ensure_bridge_imap_mailboxes(&alice_id.0)
        .await
        .unwrap();
    a_state
        .db
        .place_inbound_mail(
            &alice_id.0,
            &rid,
            "INBOX",
            1_715_000_000,
            "",
            "external.test",
            true,
        )
        .await
        .unwrap();

    // ── Step 5: the REAL residential worker relays A→B ───────────────────────
    // B is the home box: its admin set the NAT mode private at onboarding.
    *b_state.node_mode.write().await = fauna_nest::config::NodeMode::Private;
    let mut watermarks = SyncWatermarks::new();
    let mut mail_watermarks = SyncWatermarks::new();
    let outcome = run_sync_cycle(&b_state, &mut watermarks, &mut mail_watermarks).await;
    assert_eq!(
        outcome,
        SyncCycleOutcome::Processed(1),
        "the home box's worker relays the one paired actor (both pairing rows present)"
    );

    // ── Step 6: the home box stored it SEALED at rest ────────────────────────
    // The relayed record lands verbatim — the home box never opens it on
    // receipt; the inner HPKE seal is intact and still opens with the
    // recipient's own MSEK-derived secret (the client/MDA read path).
    let stored = fauna_nest::segments::mail::read_envelope(
        &b_state.mail_segments,
        &b_state.db,
        &alice_id.0,
        &rid,
    )
    .await
    .expect("read the relayed segment on the home box")
    .expect("the relayed record is present on the home box");
    let segment_env = SegmentMailEnvelope::decode(&stored).expect("decode segment envelope");
    assert_ne!(
        segment_env.encrypted_body.as_slice(),
        BODY,
        "the home box stores the relayed body SEALED, never opened on receipt"
    );
    let sealed_inner = SealedMailEnvelope::from_canonical_bytes(&segment_env.encrypted_body)
        .expect("the stored body is the verbatim inner HPKE seal");
    // No data loss — the production MDA-AUTH read path: open the home box's
    // auto-provisioned wrapped-MSEK with the generated password, derive the
    // recipient secret, open the relayed seal (mirrors the encrypted journey).
    let b_wrapped_msek = b_state
        .db
        .get_wrapped_mls_blob(&alice_id.0, &derive_credential_id("Default", &[]))
        .await
        .unwrap()
        .expect("home box got the wrapped-MSEK read credential");
    let blob = WrappedMsekBlob::from_canonical_bytes(&b_wrapped_msek)
        .expect("decode the home box's stored wrapped-MSEK blob");
    let pw_cred = Credential::Plain(Zeroizing::new(password.as_str().as_bytes().to_vec()));
    let recovered_msek = unseal_wrapped_msek(&blob, &pw_cred.as_input())
        .expect("home box's wrapped-MSEK opens with the generated password");
    let (recipient_secret, _) = derive_recipient_hpke_keypair(&recovered_msek);
    let opened = unseal_mail_record(&sealed_inner, &recipient_secret)
        .expect("the recipient's own MSEK-derived secret opens the relayed record");
    assert_eq!(opened.as_slice(), BODY, "byte-for-byte after client open");
}
