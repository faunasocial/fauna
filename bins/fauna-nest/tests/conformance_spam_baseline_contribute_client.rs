//! **Deployment-baseline contribute toggle, real-wire** (tier_3) — the one gap
//! left open by `mail-spam.md` § Encrypted-mode interaction piece (b)'s two
//! existing proofs. Drives the **production client machine**
//! (`MailSpamMachine::dispatch(SetContributeBaseline …)`) over a real
//! `fauna_client::NestClient` against an in-process `fauna-nest`, and proves the
//! contribute-toggle write path end-to-end: opt-in mints a **keyless**
//! `content.read{spam-model}` grant to the box's aggregation holder, seals the
//! initial model copy to that holder, and attaches it — so a real
//! content-processor holder can aggregate the copy and a subsequent
//! `publish_spam_baseline` **counts the contributor** (`mail-spam.md` § Wire
//! shapes, the `fetch_spam_model`/`put_spam_model`/`publish_spam_baseline` rows).
//!
//! What only this test catches over the two halves that already existed:
//! - `fauna-client-mail-settings`'s `tests/spam_model_write.rs` b3 tests assert the
//!   machine's mint/seal/attach *orchestration* — but against an in-memory
//!   `FakeNestClient`, so they can't catch a `#[derive(Serialize)]`-vs-nest wire
//!   mismatch on the real `fauna.capabilities.mint` / `put_spam_model.holder_copy`
//!   path.
//! - `tests/e2e-unified/tests/test_spam_baseline_drain.py` tier_3-proves the
//!   holder aggregation + publish counting — but every contributor there is a
//!   `seal-helper-testonly`-seeded blob (Go assembling the wire bytes), never the
//!   real client machine.
//!
//! This test swaps that one seeded contributor for the **real** machine: the
//! contributor's grant, holder copy, and sealed model are all produced by the
//! production `MailSpamMachine` / `MailSettingsMachine` over the wire, and the
//! holder leg mirrors `fauna_ffi::aggregate_spam_model_copies` (the exact
//! shared-Rust merge the Go MDA holder calls via FFI) so the copy the real client
//! sealed is unsealed + merged + counted by the holder's own x25519 key.
//!
//! (The grant's *keyless-ness* — that `content.read{spam-model}` conveys no key
//! material — is proven by `spam_model_write.rs`'s `opt_in_mints_keyless_grant…`
//! and the nest-side `grant_blob_rejects_key_bearing_spam_model_tuple`; here the
//! worklist serving the copy is the functional proof that the minted grant
//! *reaches* the holder — copy alone is never authorization.)

mod common;
use common::connected_client;

use std::sync::Arc;
use std::time::Duration;

use fauna_client_mail_settings::rpc_glue::{build_mail_settings_machine, build_mail_spam_machine};
use fauna_client_mail_settings::{
    CredentialKind, MailSettingsAction, MailSpamAction, ModelWriteOp, SecretBytes,
};
use fauna_core::identity::ActorKeypair;
use fauna_mail::spam::{SpamLabel, SpamModel};
use fauna_mls::wrapped_blob::{
    SpamModelCopyBlob, derive_recipient_hpke_keypair, unseal_spam_model_copy,
};
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::RpcRequester;
use fauna_protocol::bridge_routing::{PublishSpamBaselineReply, PublishSpamBaselineRequest};
use fauna_protocol::wrapped_blob::{
    SpamBaselineCopy, SpamBaselineWorklistReply, SpamBaselineWorklistRequest,
    SubmitSpamBaselineReply, SubmitSpamBaselineRequest,
};
use serde_bytes::ByteBuf;

/// An in-process nest carrying every kind the contribute-toggle flow touches:
/// auth-bootstrap + anonymous discovery (for the `NestClient` handshake +
/// capability probes), a real `BackupService` blob store, the bridge-blob + capability plane (grant
/// mint/revoke + the spam-baseline worklist/submit drain), bridge-routing (mail
/// provisioning + the publish push), and bridge-imap (spam model
/// fetch/put/set-baseline-contribution/publish). Returns `(http_base, state,
/// tempdir)` — the `TempDir` guard keeps the `BackupService` blob dir alive.
async fn start_spam_nest() -> (String, Arc<AppState>, tempfile::TempDir) {
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
            fauna_nest::bridge_blob_handlers::register_bridge_blob_handlers(&mut b);
            fauna_nest::bridge_blob_handlers::register_capability_handlers(&mut b);
            fauna_nest::bridge_routing_handlers::register_bridge_routing_handlers(&mut b);
            fauna_nest::bridge_imap_handlers::register_bridge_imap_handlers(&mut b);
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

/// The in-process stand-in for the granted content-processor holder — mirrors
/// `fauna_ffi::aggregate_spam_model_copies` (the exact merge the Go MDA holder
/// runs via FFI): for each copy, decode → owner-AAD-bind check → unseal with the
/// holder's own x25519 secret → merge; an undecodable/foreign copy counts as
/// `unreadable`, never merged. Returns `(merged_model_bytes, contributors,
/// unreadable)` — the `submit_spam_baseline` payload.
fn holder_aggregate(
    copies: &[SpamBaselineCopy],
    holder_secret: &[u8; 32],
) -> (Vec<u8>, u32, u32, Vec<ByteBuf>) {
    let mut merged = SpamModel::new();
    let mut contributors: u32 = 0;
    let mut unreadable: u32 = 0;
    // The owners whose copies opened — what the real holder names in
    // `merged_contributors` so the nest records exactly them as summed.
    let mut merged_contributors: Vec<ByteBuf> = Vec::new();
    for c in copies {
        let opened = SpamModelCopyBlob::from_canonical_bytes(c.sealed_copy.as_ref())
            .ok()
            .filter(|blob| blob.index.0.as_slice() == c.owner_actor_id.as_slice())
            .and_then(|blob| unseal_spam_model_copy(&blob, holder_secret, None).ok())
            .and_then(|bytes| SpamModel::from_bytes(&bytes));
        match opened {
            Some(model) => {
                merged.merge(&model);
                contributors = contributors.saturating_add(1);
                merged_contributors.push(c.owner_actor_id.clone());
            }
            None => unreadable = unreadable.saturating_add(1),
        }
    }
    (
        merged.to_bytes(),
        contributors,
        unreadable,
        merged_contributors,
    )
}

/// Poll the nest's in-memory pending-run registry for the run a concurrent
/// `publish_spam_baseline` just registered (the holder learns the id from the
/// `BridgeSpamBaselinePublish` push in production; in-process we read it straight
/// off `AppState` — the push routing itself is covered by the Python tier_3
/// drain test, and is not the client-write behavior under test here).
async fn wait_for_pending_run(state: &AppState) -> Vec<u8> {
    for _ in 0..500 {
        if let Some(id) = state.spam_baseline_runs.lock().await.keys().next().cloned() {
            return id;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no pending spam-baseline publish run appeared within ~10s");
}

#[tokio::test]
async fn real_client_contribute_toggle_mints_grant_seals_copy_and_publish_counts_it() {
    let (base, state, _tmp) = start_spam_nest().await;

    // ── Actors ────────────────────────────────────────────────────────────
    // The real contributor.
    let alice = ActorKeypair::generate();
    let alice_secret = *alice.secret_bytes();
    let alice_id = alice.actor_id();
    state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();

    // The admin who runs the publish.
    let admin = ActorKeypair::generate();
    let admin_secret = *admin.secret_bytes();
    let admin_id = admin.actor_id();
    state
        .db
        .create_user(&admin_id.0, "free", "admin")
        .await
        .unwrap();
    state.db.add_admin_actor(&admin_id.0).await.unwrap();

    // The aggregation holder — an approved **content-processor** service user with
    // an x25519 seal target (so `resolve_content_processor_holder_seal_target`
    // volunteers it in alice's `fetch_spam_model` reply, and the worklist gate
    // resolves it). The three DB calls are the direct equivalent of the Python
    // harness's `POST pending` + `UPDATE x25519` + `POST approve`; a service user
    // gets its own `users` row inside `approve_bridge_service_user`, so it must
    // NOT also be `create_user`'d (that would make it a plain `User`).
    let holder_ed = ActorKeypair::generate();
    let holder_ed_secret = *holder_ed.secret_bytes();
    let holder_id = holder_ed.actor_id();
    let (holder_x25519_secret, holder_x25519_public) = derive_recipient_hpke_keypair(&[0x5a; 32]);
    state
        .db
        .create_pending_bridge_service_user(&holder_id.0, BridgeRole::ContentProcessor, "cp-1")
        .await
        .unwrap();
    state
        .db
        .upsert_bridge_x25519(&holder_id.0, &holder_x25519_public)
        .await
        .unwrap();
    state
        .db
        .approve_bridge_service_user(&holder_id.0, None)
        .await
        .unwrap();

    // ── The real client establishes a client-sealed model ─────────────────
    let user_client = connected_client(&base, ActorKeypair::from_secret(alice_secret)).await;
    // The grant log the contribute toggle's baseline grant records on (the
    // account store's ledger in production), shared by both machines.
    let ledger = Arc::new(
        fauna_client_config::test_helpers::FakeSuccessionLedgerStore::empty(
            ActorKeypair::from_secret(alice_secret).actor_id(),
        ),
    );
    // The account's mail custody — shared by the enabling machine and the spam
    // machine, as one seat's account runtime is in production.
    let mail_store: Arc<dyn fauna_client_config::MailStore> =
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty());
    let mail = build_mail_settings_machine(
        user_client.clone(),
        ActorKeypair::from_secret(alice_secret),
        Arc::clone(&mail_store),
        &base,
        ledger.clone(),
        None,
    );
    // Enable mail — mints the MSEK and persists it into alice's `fauna.state.mail` plane entry (so
    // the toggle's writer derives the same recipient key that sealed the model).
    mail.dispatch(MailSettingsAction::EnableMail {
        display_name: "Default".into(),
        kind: CredentialKind::Plain,
        secret: SecretBytes::from(b"spam-baseline-read-pw-24ch!!".to_vec()),
    })
    .await
    .expect("enable mail");
    // Train one spam sample through the real sealed-write loop — this leaves a
    // client-sealed `spam_models` row (the worklist's `is_sealed_model_blob` gate),
    // which is the precondition for the toggle to attach an initial copy.
    mail.apply_spam_model_write(
        ModelWriteOp::Train {
            text: "buy cheap pills now cheap pills".into(),
            label: SpamLabel::Spam,
        },
        None,
    )
    .await
    .expect("train a client-sealed model");

    // ── The behavior under test: opt in via the production MailSpamMachine ─
    let spam = build_mail_spam_machine(
        user_client.clone(),
        ActorKeypair::from_secret(alice_secret),
        Arc::clone(&mail_store),
        &base,
        ledger.clone(),
    );
    spam.dispatch(MailSpamAction::SetContributeBaseline { contribute: true })
        .await
        .expect("real client opt-in");

    // Nest-side: the opt-in bit is set and a holder copy landed, sealed to the
    // holder and openable under the holder's OWN x25519 secret to the current
    // model (the copy the REAL machine produced, not a seal-helper seed).
    assert!(
        state
            .db
            .get_spam_preferences(&alice_id.0)
            .await
            .unwrap()
            .contribute_baseline,
        "opt-in set the contribute_baseline bit"
    );
    let copies = state
        .db
        .list_spam_model_holder_copies(&holder_x25519_public)
        .await
        .unwrap();
    assert_eq!(copies.len(), 1, "one holder copy attached after opt-in");
    assert_eq!(copies[0].0, alice_id.0, "the copy is alice's");
    {
        let blob =
            SpamModelCopyBlob::from_canonical_bytes(&copies[0].1).expect("decode holder copy");
        let plaintext = unseal_spam_model_copy(&blob, &holder_x25519_secret, None)
            .expect("holder opens the real client's copy under its own key");
        let model = SpamModel::from_bytes(&plaintext).expect("decode merged-in model");
        assert_eq!(
            model.spam_messages, 1,
            "the copy carries alice's one-spam-sample model"
        );
    }

    // ── Publish, driven with the in-process holder concurrently ───────────
    // `publish_spam_baseline` registers a run, pokes the holder, and blocks
    // awaiting its `submit_spam_baseline`; the holder pulls the worklist (which
    // serves alice's copy ONLY because her minted grant reaches this holder —
    // the untested link), aggregates it, and submits. Run both concurrently.
    let admin_client = connected_client(&base, ActorKeypair::from_secret(admin_secret)).await;
    let holder_client = connected_client(&base, ActorKeypair::from_secret(holder_ed_secret)).await;

    let publish_fut = async {
        admin_client
            .request::<PublishSpamBaselineRequest, PublishSpamBaselineReply>(
                "fauna.bridges.publish_spam_baseline",
                PublishSpamBaselineRequest {
                    extra: Default::default(),
                },
            )
            .await
    };
    let holder_fut = async {
        let run_id = wait_for_pending_run(&state).await;
        let worklist: SpamBaselineWorklistReply = holder_client
            .request(
                "fauna.capabilities.spam_baseline_worklist",
                SpamBaselineWorklistRequest {
                    run_id: ByteBuf::from(run_id.clone()),
                    extra: Default::default(),
                },
            )
            .await
            .expect("holder pulls the worklist");
        let (merged_model, contributors, unreadable, merged_contributors) =
            holder_aggregate(&worklist.copies, &holder_x25519_secret);
        let submit: SubmitSpamBaselineReply = holder_client
            .request(
                "fauna.capabilities.submit_spam_baseline",
                SubmitSpamBaselineRequest {
                    run_id: ByteBuf::from(run_id),
                    merged_model,
                    contributors,
                    unreadable,
                    merged_contributors,
                    extra: Default::default(),
                },
            )
            .await
            .expect("holder submits its merged half");
        assert!(submit.ok, "submit accepted against the pending run");
        contributors
    };
    let (publish_res, holder_contributors) = tokio::join!(publish_fut, holder_fut);
    let reply = publish_res.expect("publish");
    assert_eq!(
        holder_contributors, 1,
        "the holder merged exactly the real contributor's copy"
    );
    assert_eq!(
        reply.contributors, 1,
        "publish counts the real client's sealed contribution"
    );
    assert_eq!(reply.skipped_contributors, 0, "nothing skipped");
    // The *counting* (contributors == 1) — not the publish — is what proves the
    // real client's grant reaches the holder and its sealed copy aggregates:
    // `contributors` is only nonzero when the worklist served alice's copy (grant
    // reached) AND the holder's submitted merge folded (`bridge_imap_handlers.rs`
    // publish handler). A single contributor is below the k-anon floor
    // (`BASELINE_MIN_CONTRIBUTORS = 3`, `mail-spam.md` § Cold start Path 2), so
    // the baseline is deliberately withheld (reset to empty) — the floor + the
    // published-baseline/sample_count path with ≥3 contributors is covered by
    // `tests/e2e-unified/tests/test_spam_baseline_drain.py`.
    assert!(
        !reply.published,
        "one contributor is below the k-anon floor of 3 ⇒ baseline withheld"
    );
    assert_eq!(
        reply.sample_count, 0,
        "sub-floor baseline is withheld (reset to empty), so no samples are published"
    );

    // ── Opt out via the production machine drops the contribution ─────────
    spam.dispatch(MailSpamAction::SetContributeBaseline { contribute: false })
        .await
        .expect("real client opt-out");
    assert!(
        !state
            .db
            .get_spam_preferences(&alice_id.0)
            .await
            .unwrap()
            .contribute_baseline,
        "opt-out cleared the bit"
    );
    assert!(
        state
            .db
            .list_spam_model_holder_copies(&holder_x25519_public)
            .await
            .unwrap()
            .is_empty(),
        "opt-out (grant revoke) deleted the holder copy"
    );
    // Republish: alice is no longer an opted-in sealed candidate, so there is no
    // holder round-trip and she is not counted.
    let reply2 = admin_client
        .request::<PublishSpamBaselineRequest, PublishSpamBaselineReply>(
            "fauna.bridges.publish_spam_baseline",
            PublishSpamBaselineRequest {
                extra: Default::default(),
            },
        )
        .await
        .expect("republish after opt-out");
    assert_eq!(
        reply2.contributors, 0,
        "the revoked contributor no longer counts"
    );
}
