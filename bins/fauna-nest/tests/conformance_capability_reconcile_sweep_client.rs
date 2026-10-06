//! **The reconcile sweep, client-driven over the real wire** (tier_3) — the
//! owner half of `fauna.capabilities.reconcile` (`docs/goal/ui/nests.md`
//! § Trust facet — grants → *Reconcile*, ratified 2026-08-15).
//!
//! The sibling `conformance_capability_reconcile_client.rs` proves the nest's
//! *read*: that the kind is wired, gated to `User`, and answers ids-only.
//! This proves what the read exists **for** — the client half that makes the
//! answer mean something: at every trust-facet refresh, the owner's machine
//! enumerates the rows this nest holds for it and revokes each one its own
//! signed log does not hold live, narrowing the nest to the log.
//!
//! Production data flow asserted end-to-end on the real `fauna-nest` binary
//! (real `fauna.capabilities.{reconcile,revoke}` WS-RPC,
//! real signed `GrantEvent` log in the client-side ledger, real
//! SQLite `capability_grants`):
//!
//!   the owner's log records a `Mint` for grant A and a `Mint`+`Revoke` for
//!   grant C (recorded in the client-side ledger) → the nest's
//!   `capability_grants` holds A (recognized), B (an orphan the log never
//!   minted — the row stranded by a deposit whose `Mint` never became durable)
//!   and C (resurrected after its revoke) →
//!   `LinkedNestsMachine::hydrate()` → the machine calls
//!   `fauna.capabilities.reconcile`, judges the three ids against its own log,
//!   and fires `fauna.capabilities.revoke` for B and C → the nest's rows are
//!   exactly `[A]`, and the owner's grant log is **byte-identical** to what it
//!   was before the sweep.
//!
//! What only a real-wire test can catch, over the in-process
//! `fauna-client-pair` `FakeNest` unit tests (`refresh_revokes_the_orphan_row_
//! and_leaves_the_live_one`, `a_row_resurrected_after_a_revoke_is_swept_again`):
//! that `reconcile`'s reply actually decodes into the seam's `[u8; 16]` ids
//! (a `ByteBuf`/array-shape mismatch passes every fake), that the revoke the
//! sweep fires is admitted by the real `bridge_method_allowlist` for the same
//! `User` class that just enumerated, and that the sweep's ledger read
//! (the `fauna.state.succession-ledger` grant events in production) is the
//! machine's own seam rather than a nest handler. A break
//! anywhere in that chain fails here and passes the fakes.
//!
//! ⚠ **The seeded rows stand in for prior real deposits, deliberately.** The
//! whole subject is rows whose *client* record is missing or contradicted — a
//! state a well-behaved mint chain cannot produce on demand (that is the point
//! of the sweep). Seeding `capability_grants` directly is the only way to
//! arrange it; the mint chain itself is proven by
//! `conformance_capability_trust_client.rs` (E2E testing-rules fixture-setup
//! carve-out).
//!
//! Harness mirrors `conformance_capability_trust_client.rs` (socket/serve +
//! `AppState` skeleton + `connected_client` + a `BackupService`).

mod common;
use common::connected_client;

use std::sync::Arc;

use fauna_client_capabilities::grant_log;
use fauna_client_config::test_helpers::FakeSuccessionLedgerStore;
use fauna_client_pair::build_linked_nests_machine_with_trust;
use fauna_core::grant_event::{GrantEvent, GrantEventScope};
use fauna_core::identity::ActorKeypair;
use fauna_core::succession_ledger::SuccessionLedger;
use fauna_mls::wrapped_blob::generate_x25519_keypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;

/// The owner identity, from a fixed secret.
const OWNER_SEED: [u8; 32] = [31u8; 32];
/// The content-processor holder's bridge identity. The holder process never
/// runs — this is the owner half — but it must be really *enrolled*, because
/// the trust facet's lenses render only grants to discovered holders. Without
/// the enrollment the Now/History assertions below would pass vacuously
/// against an empty facet.
const HOLDER_SEED: [u8; 32] = [32u8; 32];

/// `A` — minted and live in the owner's log: the nest's row is recognized and
/// must survive the sweep.
const RECOGNIZED: [u8; 16] = [0xA1u8; 16];
/// `B` — a row the log has no event for at all: an orphan, swept.
const ORPHAN: [u8; 16] = [0xB2u8; 16];
/// `C` — minted then revoked in the log, yet present on the nest: a row
/// resurrected after revocation, swept again.
const RESURRECTED: [u8; 16] = [0xC3u8; 16];

fn label_write_scope() -> GrantEventScope {
    GrantEventScope {
        class: "label.write".into(),
        kind: None,
        tier: None,
    }
}

/// Spin a real in-process nest serving what the sweep drives: auth bootstrap,
/// node-info discovery (the home row's identity), `fauna.pair.list` (the
/// machine lists pairings before building the home row), and the capability
/// kinds (`reconcile` + `revoke`); the signed grant log the sweep judges
/// against is client-side.
async fn start_sweep_nest() -> (String, Arc<AppState>, tempfile::TempDir) {
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

/// Enroll one approved MDA service-user with an x25519 seal target and return
/// that pubkey — the holder the seeded grants name, and the one the trust
/// facet must discover for its lenses to render anything at all.
async fn enroll_holder(state: &AppState) -> [u8; 32] {
    let holder_ed = ActorKeypair::from_secret(HOLDER_SEED).actor_id().0;
    let (_sk, holder_x25519_pk) = generate_x25519_keypair();
    state
        .db
        .create_pending_bridge_service_user(&holder_ed, BridgeRole::Mda, "mda-1")
        .await
        .unwrap();
    state
        .db
        .upsert_bridge_x25519(&holder_ed, &holder_x25519_pk)
        .await
        .unwrap();
    state
        .db
        .approve_bridge_service_user(&holder_ed, None)
        .await
        .unwrap();
    holder_x25519_pk
}

/// The owner's grant rows on the nest, sorted — the sweep's effect, read from
/// the real table rather than from anything the client reported.
async fn rows_on_nest(state: &AppState, owner_id: &[u8]) -> Vec<Vec<u8>> {
    let mut ids = state
        .db
        .fetch_capability_grant_ids_for_owner(owner_id)
        .await
        .expect("read the owner's rows");
    ids.sort();
    ids
}

#[tokio::test]
async fn one_refresh_revokes_the_orphan_and_the_resurrection_and_leaves_the_log_alone() {
    let (base, state, _tmp) = start_sweep_nest().await;

    let owner = ActorKeypair::from_secret(OWNER_SEED);
    let owner_id = owner.actor_id().0;
    state
        .db
        .create_user(&owner_id, "free", "owner")
        .await
        .unwrap();
    let holder_pubkey = enroll_holder(&state).await;

    let now = fauna_core::data::Timestamp::now_secs() as u64;

    // ── The owner's signed log — the succession ledger's grant events, here
    //    the client-side ledger double the machine reads: A minted and live;
    //    C minted then revoked. B is deliberately absent from it.
    let ledger = Arc::new(FakeSuccessionLedgerStore::empty(owner.actor_id()));
    let log_before: Vec<GrantEvent> = {
        let mut cfg = SuccessionLedger::empty(owner.actor_id());
        grant_log::record_mint(
            &mut cfg,
            owner.signing_key(),
            RECOGNIZED,
            holder_pubkey,
            vec![label_write_scope()],
            now,
            now + 86_400,
            now,
        )
        .expect("record Mint for A");
        grant_log::record_mint(
            &mut cfg,
            owner.signing_key(),
            RESURRECTED,
            holder_pubkey,
            vec![label_write_scope()],
            now,
            now + 86_400,
            now,
        )
        .expect("record Mint for C");
        grant_log::record_revoke(
            &mut cfg,
            owner.signing_key(),
            RESURRECTED,
            holder_pubkey,
            now + 1,
        )
        .expect("record Revoke for C");
        ledger.replace(cfg);
        ledger.current().grant_events
    };
    assert_eq!(log_before.len(), 3, "two mints and one revoke are recorded");

    // ── The nest's rows: all three, including the two the log disagrees with.
    for id in [RECOGNIZED, ORPHAN, RESURRECTED] {
        state
            .db
            .put_capability_grant(
                &owner_id,
                &id,
                &holder_pubkey,
                (now + 86_400) as i64,
                b"blob",
            )
            .await
            .unwrap();
    }
    assert_eq!(
        rows_on_nest(&state, &owner_id).await.len(),
        3,
        "precondition: the nest holds all three rows"
    );

    // ── ONE refresh of the production machine. No sweep action exists to
    //    dispatch — the sweep is automatic and silent by ratified constraint,
    //    so hydrating the Nests page is the whole trigger.
    let nest = connected_client(&base, ActorKeypair::from_secret(OWNER_SEED)).await;
    let machine = build_linked_nests_machine_with_trust(
        Arc::clone(&nest),
        ActorKeypair::from_secret(OWNER_SEED),
        ledger.clone(),
        // The backup destination list is not under test: an empty one.
        Arc::new(fauna_client_config::test_helpers::FakeBackupStateStore::empty()),
        // No account runtime: the nest alone is under test.
        Arc::new(fauna_client_pair::NoAccountRuntime),
        fauna_client_subscriptions::period_keys::MemoryPeriodKeyStore::new().shared(),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
    );
    machine.hydrate().await.expect("hydrate the Nests page");

    // ── The nest is narrowed to the log: only the recognized row survives.
    assert_eq!(
        rows_on_nest(&state, &owner_id).await,
        vec![RECOGNIZED.to_vec()],
        "the orphan (no event) and the resurrection (revoked in the log) are \
         both revoked on the nest; the recognized grant is untouched"
    );

    // ── …and nothing the nest said entered client state. A `Revoke` event for
    //    an id the log never minted would be nest-influenced content in the
    //    signed log — the precise channel the audit rule exists to close.
    let log_after = ledger.current().grant_events;
    assert_eq!(
        log_after, log_before,
        "the sweep appends NO GrantEvent — the log is byte-identical across it"
    );

    // ── The lenses are unchanged too: the Now lens shows the one live grant it
    //    showed before, and neither swept id appears anywhere on the page.
    let home = machine.snapshot().home.expect("home row after the sweep");
    let now_ids: Vec<Vec<u8>> = home
        .trust_grants
        .iter()
        .map(|g| g.grant_id.clone())
        .collect();
    assert_eq!(
        now_ids,
        vec![RECOGNIZED.to_vec()],
        "the Now lens is the log's projection — the sweep does not feed it"
    );
    assert!(
        !home
            .trust_history
            .iter()
            .any(|e| e.grant_id == ORPHAN.to_vec()),
        "an id the nest reported and the log never knew must not reach History"
    );
}

/// Idempotence: a second refresh with nothing left to disagree about fires no
/// revoke and changes nothing. The sweep runs at **every** refresh, so a sweep
/// that churned on a converged state would revoke-storm an honest nest once per
/// page load.
#[tokio::test]
async fn a_second_refresh_on_a_converged_pair_is_a_no_op() {
    let (base, state, _tmp) = start_sweep_nest().await;

    let owner = ActorKeypair::from_secret(OWNER_SEED);
    let owner_id = owner.actor_id().0;
    state
        .db
        .create_user(&owner_id, "free", "owner")
        .await
        .unwrap();
    let holder_pubkey = enroll_holder(&state).await;
    let now = fauna_core::data::Timestamp::now_secs() as u64;

    let ledger = Arc::new(FakeSuccessionLedgerStore::empty(owner.actor_id()));
    ledger.mutate(|cfg| {
        grant_log::record_mint(
            cfg,
            owner.signing_key(),
            RECOGNIZED,
            holder_pubkey,
            vec![label_write_scope()],
            now,
            now + 86_400,
            now,
        )
        .expect("record Mint for A");
    });
    state
        .db
        .put_capability_grant(
            &owner_id,
            &RECOGNIZED,
            &holder_pubkey,
            (now + 86_400) as i64,
            b"blob",
        )
        .await
        .unwrap();

    let nest = connected_client(&base, ActorKeypair::from_secret(OWNER_SEED)).await;
    let machine = build_linked_nests_machine_with_trust(
        Arc::clone(&nest),
        ActorKeypair::from_secret(OWNER_SEED),
        ledger,
        // The backup destination list is not under test: an empty one.
        Arc::new(fauna_client_config::test_helpers::FakeBackupStateStore::empty()),
        // No account runtime: the nest alone is under test.
        Arc::new(fauna_client_pair::NoAccountRuntime),
        fauna_client_subscriptions::period_keys::MemoryPeriodKeyStore::new().shared(),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
    );

    machine.hydrate().await.expect("first refresh");
    machine.hydrate().await.expect("second refresh");

    assert_eq!(
        rows_on_nest(&state, &owner_id).await,
        vec![RECOGNIZED.to_vec()],
        "a converged log/nest pair survives repeated sweeps untouched"
    );
}
