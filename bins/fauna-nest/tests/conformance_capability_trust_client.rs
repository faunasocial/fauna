//! **Owner-side nest-trust facet** (tier_3) — real-wire, *client-driven*. Proves
//! the Nests-page trust facet (`docs/goal/ui/nests.md` § Trust facet) end-to-end
//! through the production client stack: the shared trust-enabled
//! `fauna_client_pair::LinkedNestsMachine`'s `Mint`/`Revoke`/`SetLens` actions,
//! driven over an authenticated `NestClient`, deposit + delete a real
//! `capability_grants` row on the nest AND keep the client-authoritative signed
//! grant log (the `fauna.state.succession-ledger` grant events, rendered as the Now/History lens) in
//! lock-step.
//!
//! Production data flow asserted end-to-end on the real `fauna-nest` binary
//! (real `fauna.capabilities.{mint,revoke}` + `fauna.bridges.{list_service_users,
//! fetch_bridge_pubkey}` WS-RPC, real HPKE grant seal,
//! real SQLite `capability_grants`):
//!
//!   owner (admin) enables mail (MSEK seeded into the client-side mail store) →
//!   `Mint{content.read{mail}}` → the machine discovers the enrolled MDA holder
//!   (`content_processor_holders`), derives the mail scope key from the MSEK,
//!   seals a `GrantBlob` to the holder's x25519, deposits it
//!   (`fauna.capabilities.mint`), and appends a signed `Mint` event to the log
//!   (persisted to the client-side grant ledger) → the owner's **Now** lens shows
//!   one Active grant to the holder, and the nest's `capability_grants` holds the
//!   row (the holder's self-scoped fetch would return it). `Revoke` deletes the
//!   `(owner, grant_id)` row (the holder's next fetch goes dark — spec § 2.5 /
//!   `wrapped_blob.rs` fetch omits revoked) and appends a signed `Revoke` event →
//!   the **Now** lens drops the grant (back to `nest-trust-empty`) while the
//!   **History** lens keeps the full `[Revoke, Mint]` timeline (Now = projection,
//!   History = the append-only log — `nests.md:79`).
//!
//! What this catches that nothing else does:
//! - Over the crate's `FakeNest` unit tests (`fauna-client-pair/src/lib.rs`
//!   `mint_deposits_..._shows_active_grant`, `revoke_drops_from_now_keeps_history`):
//!   the real client→seam→handler→DB chain — that `content_processor_holders`
//!   actually resolves the enrolled roster, that `mint_grant`'s HPKE seal + the
//!   nest handler write the real `capability_grants` row, and that the signed
//!   grant log is recorded in the client-side ledger. A break anywhere in
//!   that chain fails here but passes the in-process fakes (the exact gap the
//!   sibling `conformance_cross_nest_pairing_client.rs` closes for pairing).
//! - Over the holder-side tier_3 `tests/e2e-unified/tests/test_capability_rescore_drain.py`
//!   (which already proves the MDA *fetches + opens + re-scores* under a grant and
//!   goes dark on revoke, via a raw-RPC seeded mint): this is the **owner** half —
//!   the client-authoritative `grant_log` Now/History audit view the owner's Nests
//!   page renders, which `fetch` (holder-pubkey-scoped, `nests.md:75`) can never
//!   serve. Together they prove both ends of one grant's lifecycle.
//! - Over the rendering-only `tests/e2e-unified/tests/test_nest_trust.py` (the
//!   *empty* facet + lens toggle on a fresh nest): this drives the facet
//!   **populated** by a real machine mint.
//!
//! **Per-user since 2026-07-17**: holder discovery rides
//! `fauna.bridges.list_service_users`, now `User | Admin` with a class-scoped
//! reply (`bridge_method_allowlist.rs`) — the admin-driven test below predates
//! the relax and keeps the full-roster admin path covered, and
//! `a_plain_user_discovers_holders_and_mints_then_revokes` pins the User-gated
//! path (it went RED against the old Admin-only gate — the mint found no
//! holders). The **MSEK seed** is a fixture precondition (the E2E
//! testing-rules fixture-setup carve-out (b) — "Enable mail" is a real prior UI
//! step, arranging the world, not the mint action under test); it is seeded into
//! the same client-side mail store the machine reads. No v1 UI mints — the mint action is
//! the client-side operation the settings page performs (deferred onboarding
//! shortcut, `nests.md` § Not in v1); driving it here is the tier_3 proof that the
//! machine's mint chain is sound.
//!
//! Harness mirrors `conformance_cross_nest_pairing_client.rs` (socket/serve +
//! `AppState` skeleton + `connected_client`).

mod common;
use common::connected_client;

use std::sync::Arc;

use fauna_client_config::test_helpers::FakeMailStore;
use fauna_client_config::test_helpers::FakeSuccessionLedgerStore;
use fauna_client_pair::{
    LinkedNestsAction, TrustEventKind, TrustGrantDuration, TrustLens, TrustLiveness, TrustScope,
    build_linked_nests_machine_with_trust,
};
use fauna_core::data::MailConfig;
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

/// The owner (admin) identity, from a fixed secret.
const OWNER_SEED: [u8; 32] = [11u8; 32];
/// The content-processor holder's bridge identity — distinct from the owner (an
/// actor enrolled as a service-user resolves to the bridge caller-class, not
/// Admin/User, so it must not be the minting owner).
const HOLDER_SEED: [u8; 32] = [22u8; 32];
/// The owner's mail MSEK — `content.read{mail}` derives its per-scope key from
/// account's mail custody (`fauna.state.mail`), so mail must be "enabled"
/// before the mint.
const OWNER_MSEK: [u8; 32] = [0x5Au8; 32];

/// The mail `content.read` scope tuple v1 mints (spec `content.read{mail}`).
fn mail_read_scope() -> TrustScope {
    TrustScope {
        class: "content.read".into(),
        kind: Some("mail".into()),
        tier: None,
    }
}

/// Current epoch seconds — the `now` the nest's holder-scoped fetch filters
/// non-expired grants against.
fn now_epoch() -> i64 {
    fauna_core::data::Timestamp::now_secs()
}

/// Spin a real in-process nest serving the full surface the trust machine drives:
/// auth bootstrap + node-info discovery + the capability kinds
/// (`fauna.capabilities.{mint,fetch,renew,revoke}`) + the bridge service-user
/// roster (`list_service_users` + `fetch_bridge_pubkey`) + a
/// `BackupService` over a tempdir.
/// Returns the `http://` base, the state, and the tempdir guard (so the blob dir
/// outlives the server).
async fn start_trust_nest() -> (String, Arc<AppState>, tempfile::TempDir) {
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
            // `LinkedNestsMachine::refresh` lists pairings (`fauna.pair.list`)
            // before building the home trust row.
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

#[tokio::test]
async fn mint_then_revoke_over_real_nest_updates_owner_grant_log_and_holder_darkens() {
    let (base, state, _tmp) = start_trust_nest().await;

    // ── Owner: a registered ADMIN. Holder discovery (`list_service_users`) is
    // Admin-gated; Admin ⊇ User covers the User-gated mint/revoke, so one admin
    // owner drives the whole flow (`nests.md:143` — the facet is admin-scoped in
    // v1). The grant's owner is this actor (the mint handler enforces
    // `blob.owner == caller`).
    let owner = ActorKeypair::from_secret(OWNER_SEED);
    let owner_id = owner.actor_id().0;
    state
        .db
        .create_user(&owner_id, "free", "owner")
        .await
        .unwrap();
    state.db.add_admin_actor(&owner_id).await.unwrap();

    // ── Enroll one content-processor holder: a bridge MDA service-user with an
    // x25519 seal target. The client's holder filter is `role == "mda" &&
    // has_x25519` (`CONTENT_PROCESSOR_ROLES`), and `fetch_bridge_pubkey` only
    // resolves mta/mda — so the discoverable holder is enrolled as `Mda`
    // (spec § 2.4: `holder_pubkey = bridge_service_users.x25519_pubkey`). The MDA
    // process itself never runs here — this test is the owner half; the holder's
    // fetch/open/drain is the sibling `test_capability_rescore_drain.py`.
    let holder_ed = ActorKeypair::from_secret(HOLDER_SEED).actor_id().0;
    let (_holder_x25519_sk, holder_x25519_pk) = generate_x25519_keypair();
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
        .approve_bridge_service_user(&holder_ed, Some(&owner_id))
        .await
        .unwrap();

    // ── Enable mail (seed the MSEK) into the client-side mail store the
    // trust machine loads. `content.read{mail}` derives its scope
    // key from the account's MSEK (the mail custody); without it the mint reports
    // "mail not enabled". Fixture precondition (carve-out (b)), not the action under test.
    let mail_store = Arc::new(FakeMailStore::with(&MailConfig {
        msek: Some(OWNER_MSEK.into()),
        ..MailConfig::default()
    }));

    // ── Build the production trust machine over a real authenticated NestClient
    // and hydrate the Nests page. The home nest row appears with an EMPTY trust
    // facet — a holder is enrolled but nothing is granted yet (`nest-trust-empty`).
    let nest = connected_client(&base, ActorKeypair::from_secret(OWNER_SEED)).await;
    let machine = build_linked_nests_machine_with_trust(
        Arc::clone(&nest),
        ActorKeypair::from_secret(OWNER_SEED),
        // The client-side grant log (the account store's ledger in
        // production) — the machine records its events here.
        Arc::new(FakeSuccessionLedgerStore::empty(
            ActorKeypair::from_secret(OWNER_SEED).actor_id(),
        )),
        // The backup destination list is not under test: an empty one.
        Arc::new(fauna_client_config::test_helpers::FakeBackupStateStore::empty()),
        // No account runtime: the nest alone is under test.
        Arc::new(fauna_client_pair::NoAccountRuntime),
        fauna_client_subscriptions::period_keys::MemoryPeriodKeyStore::new().shared(),
        mail_store,
    );
    machine.hydrate().await.expect("hydrate the Nests page");

    let home = machine
        .snapshot()
        .home
        .expect("the connected/home nest row is present after hydrate");
    let home_nest_id = home.nest_id.clone();
    assert!(home.is_home, "the connected nest renders as the home row");
    assert!(
        home.trust_grants.is_empty(),
        "no grant minted yet ⇒ the Now lens is empty (nest-trust-empty)"
    );
    assert!(
        home.trust_history.is_empty(),
        "no events yet ⇒ the History timeline is empty"
    );

    // ── MINT content.read{mail}. The machine discovers the holder, derives the
    // mail scope key from the MSEK, seals a GrantBlob to the holder's x25519,
    // deposits it (`fauna.capabilities.mint`), and appends a signed Mint event to
    // the client-authoritative grant log (persisted through the config CAS).
    machine
        .dispatch(LinkedNestsAction::Mint {
            nest_id: home_nest_id.clone(),
            holder_bridge_id: "mda-1".into(),
            scope: vec![mail_read_scope()],
            // The ~90-day grant these tests assert on; an un-blessed nest's
            // default is the few-hour one-off.
            duration: Some(TrustGrantDuration::Standard),
        })
        .await
        .expect("mint content.read{mail} to the enrolled holder");

    // Owner-side grant log — the **Now** lens shows exactly one Active grant to
    // the holder, carrying the mail scope. This is client-authoritative log state
    // (`nests.md:75`), never a nest read.
    let home = machine.snapshot().home.expect("home row after mint");
    assert_eq!(
        home.trust_grants.len(),
        1,
        "the Now lens shows the one minted grant"
    );
    let grant = home.trust_grants[0].clone();
    assert_eq!(
        grant.holder,
        holder_x25519_pk.to_vec(),
        "the grant is sealed to the enrolled holder's x25519 pubkey"
    );
    assert_eq!(
        grant.scope,
        vec![mail_read_scope()],
        "the grant carries the content.read{{mail}} scope the user chose"
    );
    assert_eq!(
        grant.liveness,
        TrustLiveness::Active,
        "a fresh ~90d master-key grant is Active (not expiring/expired)"
    );
    let grant_id = grant.grant_id.clone();

    // Nest side — the real deposit landed. The `(owner, grant_id)` row exists and
    // the holder's self-scoped fetch view returns it. This client→seam→handler→DB
    // chain is exactly what the FakeNest unit tests can't exercise.
    let now = now_epoch();
    assert!(
        state
            .db
            .get_capability_grant(&owner_id, &grant_id)
            .await
            .unwrap()
            .is_some(),
        "fauna.capabilities.mint deposited the grant on the real nest"
    );
    assert_eq!(
        state
            .db
            .fetch_capability_grants_for_holder(&holder_x25519_pk, now)
            .await
            .unwrap()
            .len(),
        1,
        "the holder's self-scoped fetch would return exactly this grant"
    );

    // ── Flip the lens to History (SetLens — local UI state, no nest round-trip)
    // and confirm the append-only signed timeline behind the Now projection.
    machine
        .dispatch(LinkedNestsAction::SetLens {
            nest_id: home_nest_id.clone(),
            lens: TrustLens::History,
        })
        .await
        .expect("SetLens is a local flip");
    let home = machine.snapshot().home.expect("home row after SetLens");
    assert_eq!(
        home.lens,
        TrustLens::History,
        "SetLens flips the row's lens locally"
    );
    assert_eq!(
        home.trust_history.len(),
        1,
        "one Mint event recorded so far"
    );
    assert_eq!(home.trust_history[0].kind, TrustEventKind::Mint);
    assert_eq!(home.trust_history[0].grant_id, grant_id);

    // ── REVOKE. The machine deletes the nest grant (`fauna.capabilities.revoke`)
    // and appends a signed Revoke event.
    machine
        .dispatch(LinkedNestsAction::Revoke {
            grant_id: grant_id.clone(),
        })
        .await
        .expect("revoke the grant");

    // Nest side — the `(owner, grant_id)` row is GONE, so the holder's next fetch
    // goes dark (honest-box revocation — the fetch handler omits revoked grants,
    // spec § 2.5).
    assert!(
        state
            .db
            .get_capability_grant(&owner_id, &grant_id)
            .await
            .unwrap()
            .is_none(),
        "revoke deleted the (owner, grant_id) row"
    );
    assert!(
        state
            .db
            .fetch_capability_grants_for_holder(&holder_x25519_pk, now)
            .await
            .unwrap()
            .is_empty(),
        "the holder's self-scoped fetch now returns nothing (dark)"
    );

    // Owner-side grant log — the **Now** lens drops the grant (back to
    // nest-trust-empty), but the **History** lens keeps the full timeline
    // `[Revoke, Mint]` most-recent-first: Now is a projection, History is the
    // append-only log (`nests.md:79`).
    let home = machine.snapshot().home.expect("home row after revoke");
    assert!(
        home.trust_grants.is_empty(),
        "a revoked grant leaves the Now lens empty (nest-trust-empty)"
    );
    assert_eq!(
        home.trust_history.len(),
        2,
        "History retains both the mint and the revoke"
    );
    assert_eq!(
        home.trust_history[0].kind,
        TrustEventKind::Revoke,
        "most-recent-first: the revoke leads the timeline"
    );
    assert_eq!(home.trust_history[1].kind, TrustEventKind::Mint);
    assert!(
        home.trust_history.iter().all(|e| e.grant_id == grant_id),
        "both events name the same grant"
    );
}

/// A PLAIN (non-admin) user drives the same discovery → mint → revoke flow.
/// This is the User-gated path the trust-holder-discovery track opened
/// (2026-07-17): `fauna.bridges.list_service_users` is User-callable with a
/// class-scoped reply (approved + x25519-attested + cp-family + off-box), so
/// the trust facet is no longer admin-scoped — closing the `nests.md`
/// "Known gap — non-admin holder discovery". Before the relax this test's
/// hydrate found no holders and the mint failed; it is the red-first pin for
/// the gate.
#[tokio::test]
async fn a_plain_user_discovers_holders_and_mints_then_revokes() {
    const USER_SEED: [u8; 32] = [33u8; 32];
    const USER_MSEK: [u8; 32] = [0x6Bu8; 32];

    let (base, state, _tmp) = start_trust_nest().await;

    // A registered, NON-admin owner.
    let owner = ActorKeypair::from_secret(USER_SEED);
    let owner_id = owner.actor_id().0;
    state
        .db
        .create_user(&owner_id, "free", "plainuser")
        .await
        .unwrap();

    // One enrolled off-box MDA holder with an attested x25519 (as above).
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

    // Enable mail (MSEK) through the real config CAS — fixture precondition.
    let mail_store = Arc::new(FakeMailStore::with(&MailConfig {
        msek: Some(USER_MSEK.into()),
        ..MailConfig::default()
    }));

    let nest = connected_client(&base, ActorKeypair::from_secret(USER_SEED)).await;
    let machine = build_linked_nests_machine_with_trust(
        Arc::clone(&nest),
        ActorKeypair::from_secret(USER_SEED),
        Arc::new(FakeSuccessionLedgerStore::empty(
            ActorKeypair::from_secret(USER_SEED).actor_id(),
        )),
        // The backup destination list is not under test: an empty one.
        Arc::new(fauna_client_config::test_helpers::FakeBackupStateStore::empty()),
        // No account runtime: the nest alone is under test.
        Arc::new(fauna_client_pair::NoAccountRuntime),
        fauna_client_subscriptions::period_keys::MemoryPeriodKeyStore::new().shared(),
        mail_store,
    );
    machine.hydrate().await.expect("hydrate as a plain user");

    let home = machine
        .snapshot()
        .home
        .expect("home row present for a plain user");
    let home_nest_id = home.nest_id.clone();
    assert!(
        home.trust_grants.is_empty(),
        "nothing granted yet (nest-trust-empty)"
    );

    // MINT as the plain user — discovery ran over the User-gated roster view.
    machine
        .dispatch(LinkedNestsAction::Mint {
            nest_id: home_nest_id,
            holder_bridge_id: "mda-1".into(),
            scope: vec![mail_read_scope()],
            // The ~90-day grant these tests assert on; an un-blessed nest's
            // default is the few-hour one-off.
            duration: Some(TrustGrantDuration::Standard),
        })
        .await
        .expect("a plain user mints content.read{mail} to the discovered holder");

    let home = machine.snapshot().home.expect("home row after mint");
    assert_eq!(home.trust_grants.len(), 1, "Now lens shows the grant");
    assert_eq!(home.trust_grants[0].holder, holder_x25519_pk.to_vec());
    let grant_id = home.trust_grants[0].grant_id.clone();

    let now = now_epoch();
    assert!(
        state
            .db
            .get_capability_grant(&owner_id, &grant_id)
            .await
            .unwrap()
            .is_some(),
        "the plain user's mint deposited the grant"
    );
    assert_eq!(
        state
            .db
            .fetch_capability_grants_for_holder(&holder_x25519_pk, now)
            .await
            .unwrap()
            .len(),
        1,
        "the holder's self-scoped fetch would return it"
    );

    // REVOKE — the holder goes dark.
    machine
        .dispatch(LinkedNestsAction::Revoke {
            grant_id: grant_id.clone(),
        })
        .await
        .expect("a plain user revokes their own grant");
    assert!(
        state
            .db
            .fetch_capability_grants_for_holder(&holder_x25519_pk, now)
            .await
            .unwrap()
            .is_empty(),
        "revoked ⇒ the holder's fetch is dark"
    );
}

/// The CUSTODY kind over the real capability doors (W8.2 (account-data-plane.md § Workstreams) — T13's lifecycle
/// bullet: "each verb also drives a keyless nest-side capability row").
/// Production data flow asserted end-to-end: the owner records the custody
/// `Mint` event into the client-side grant ledger → the keyless blob
/// releases only against the ledger state actually stored
/// (record-then-deposit, the `UndepositedGrant` door) → the real
/// `fauna.capabilities.mint` handler writes the `(owner, grant_id)` row —
/// holder = the custodian's Ed25519 device key, never a bridge enrollee,
/// `wrapped_keys` empty — with ZERO nest-side changes → `fauna.capabilities.
/// revoke` deletes the row, which is exactly the store the nest's custody
/// admission (the W8.6 door) will refuse from. What this catches that the
/// in-crate unit tests cannot: that the nest's mint handler genuinely accepts
/// a custody-class keyless blob whose holder is not X25519 material (the
/// handler's own guards — caller==owner, 32-byte holder, size, quota — all
/// hold), and that the whole verb chain runs over the real WS wire.
#[tokio::test]
async fn a_custody_grant_row_mints_keyless_and_revoke_deletes_it() {
    use fauna_client_capabilities::custody_grants::{custody_event_scopes, custody_mint_blob};
    use fauna_client_capabilities::grant_log::{self, PublishedGrants};
    use fauna_client_capabilities::rpc::CapabilitiesClient;
    use fauna_core::custody_grant::CustodyScopeSet;
    use fauna_mls::wrapped_blob::{GrantBlob, GrantWindow, ScopeTuple};

    const CUSTODY_OWNER_SEED: [u8; 32] = [44u8; 32];
    /// The custodian's device-principal Ed25519 key (= its peer-plane NodeId)
    /// — deliberately never enrolled anywhere on this nest.
    const CUSTODIAN_KEY: [u8; 32] = [0xC5u8; 32];

    let (base, state, _tmp) = start_trust_nest().await;
    let owner = ActorKeypair::from_secret(CUSTODY_OWNER_SEED);
    let owner_id = owner.actor_id().0;
    // A plain registered user — the custody verbs are User-class.
    state
        .db
        .create_user(&owner_id, "free", "custodyowner")
        .await
        .unwrap();

    let nest = connected_client(&base, ActorKeypair::from_secret(CUSTODY_OWNER_SEED)).await;
    let set = CustodyScopeSet::Account;
    let grant_id = [0x1Du8; 16];
    let now = now_epoch().max(0) as u64;
    let window_end = now + 90 * 24 * 3600;

    // Build the keyless blob FIRST (it cannot be deposited yet — the type
    // holds it), record the Mint event on the grant log (the succession
    // ledger), and release against what the log holds.
    let undeposited = custody_mint_blob(
        &owner_id,
        &grant_id,
        &CUSTODIAN_KEY,
        GrantWindow(now, window_end),
        &set,
    )
    .expect("keyless custody blob");
    let mut log = SuccessionLedger::empty(owner.actor_id());
    grant_log::record_mint(
        &mut log,
        owner.signing_key(),
        grant_id,
        CUSTODIAN_KEY,
        custody_event_scopes(&set),
        now,
        window_end,
        now,
    )
    .expect("record the Mint event");
    // The log stands in for one the bound nest acknowledged: this test drives
    // the custody row's mint and revoke, and the grant-mint door itself is
    // pinned over a real account runtime in
    // `conformance_capability_reconcile_sweep_pass.rs`.
    let stored = log.clone();
    let blob_bytes = undeposited
        .release(&PublishedGrants::from_published(
            &fauna_client_config::PublishedLedger::acknowledged_for_test(stored),
        ))
        .expect("released against the published log");

    // Deposit over the real wire — the unmodified mint handler accepts the
    // custody-class keyless blob.
    let capabilities = CapabilitiesClient::new(Arc::clone(&nest));
    let reply = capabilities.mint(blob_bytes).await.expect("mint RPC");
    assert!(reply.ok, "the real mint handler accepted the custody row");

    // The nest row exists, keyless, holder = the custodian device key.
    let row_blob = state
        .db
        .get_capability_grant(&owner_id, &grant_id)
        .await
        .unwrap()
        .expect("the (owner, grant_id) custody row landed");
    let blob = GrantBlob::from_canonical_bytes(&row_blob).expect("row blob decodes");
    assert!(blob.wrapped_keys.is_empty(), "custody is keyless always");
    assert_eq!(blob.holder, CUSTODIAN_KEY);
    assert!(
        blob.scope
            .iter()
            .all(|t| t.class == ScopeTuple::CLASS_CUSTODY),
        "the row declares custody scope tuples: {:?}",
        blob.scope
    );

    // REVOKE — nest-first (the narrowing verb), then the log records it. The
    // deleted row is the nest's custody revocation store answering "revoked"
    // at its next admission evaluation (T13's two-stores rule; the W8.6 door
    // consumes exactly this absence).
    let revoke = capabilities.revoke(grant_id).await.expect("revoke RPC");
    assert!(revoke.ok);
    let _ = grant_log::record_revoke(&mut log, owner.signing_key(), grant_id, CUSTODIAN_KEY, now);
    assert!(
        state
            .db
            .get_capability_grant(&owner_id, &grant_id)
            .await
            .unwrap()
            .is_none(),
        "revoke deleted the custody row — the nest-side revocation store severs"
    );
}
