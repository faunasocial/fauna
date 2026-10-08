//! **The nest custody door, end to end over the real nest** (tier_3; W8.6 (account-data-plane.md § Workstreams) —
//! `docs/goal/architecture/account-data-plane.md` § Replica posture → *The
//! custody grant + ceremony*, decision 6): a custodian holding NO account
//! credential on this nest handshakes with the witness + PoP, and pulls the
//! owner's sealed planes over the generalized feed — with T13's nest-side
//! revocation store enforced PER REQUEST.
//!
//! The flow, every link production code:
//!
//!   the owner (a registered user) seals state rows through the REAL writer
//!   door onto the nest feed → mints the custody capability row over the
//!   real verbs (record-then-deposit) → the custodian's
//!   `custody_nest_client` (bearer = `fauna.auth.custody_handshake`: PoP by
//!   the custodian key + the inline witness + the live row) pulls
//!   `fauna.sync.changes.list { of_owner }` into a keyless custodied store →
//!   `fauna.capabilities.revoke` deletes the row → the SAME live session's
//!   very next request refuses (the per-request row re-check — stricter than
//!   the ratified next-evaluation bound) and a fresh handshake refuses too.
//!
//! Refusal arms driven in-process through the real router: a PoP signed by
//! a key other than the claimed custodian (mutation-red for the key-binding
//! check), and the scope-admission bound live at the door (an explicit-list
//! grant serves exactly its named scopes — the same `AdmittedScopes`
//! vocabulary the peer seam evaluates, carve-out included).
//!
//! Every assertion is on latency-independent state (e2e convention 14).

mod common;
use common::connected_client;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use ed25519_dalek::{Signer, SigningKey};
use fauna_account_store::{sqlite::SqliteBackend, store::AccountStore, types::WriterId};
use fauna_client::NestClient;
use fauna_client_capabilities::custody_grants::{custody_event_scopes, custody_mint_blob};
use fauna_client_capabilities::grant_log::{self, PublishedGrants};
use fauna_client_capabilities::rpc::CapabilitiesClient;
use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
use fauna_core::custody_grant::{
    CUSTODY_GRANT_ID_LEN, CustodyGrant, CustodyScopeSet, sign_custody_grant,
};
use fauna_core::data::{ModerationConfig, Timestamp};
use fauna_core::identity::ActorKeypair;
use fauna_credential_store::CredentialStore;
use fauna_mls::wrapped_blob::GrantWindow;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::account_state::{ACCOUNT_STATE_SCOPE, ItemClass};
use fauna_protocol::merge_policy::{KIND_MODERATION, MODERATION_KEY};
use fauna_sync_engine::account_runtime::{
    AccountRuntimeParams, AccountStoreRuntime, CRED_NAMESPACE, RuntimePrincipal, StoreRoot,
};
use fauna_sync_engine::account_state_plane::{AccountStatePlane, ItemId};
use fauna_sync_engine::custody_leg::custody_pull;
use fauna_sync_engine::generation_tip::GenerationTrust;

const OWNER_SEED: [u8; 32] = [0x5A; 32];
/// The custodian's device-principal secret — its key is the witness's
/// `custodian_key` and the minted session's actor.
const CUSTODIAN_SECRET: [u8; 32] = [0xC5; 32];
/// The custodian's ACCOUNT identity, distinct from its device principal above:
/// a custodian is a real account with its own plane, and the pump case below
/// runs its runtime.
const CUSTODIAN_ACCOUNT_SEED: [u8; 32] = [0x7B; 32];
const GRANT_ID: [u8; CUSTODY_GRANT_ID_LEN] = [0x1D; CUSTODY_GRANT_ID_LEN];

fn owner() -> ActorKeypair {
    ActorKeypair::from_secret(OWNER_SEED)
}

fn custodian_account() -> ActorKeypair {
    ActorKeypair::from_secret(CUSTODIAN_ACCOUNT_SEED)
}

fn custodian_key() -> SigningKey {
    SigningKey::from_bytes(&CUSTODIAN_SECRET)
}

async fn start_door_nest() -> (String, Arc<AppState>, tempfile::TempDir) {
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
            fauna_nest::sync_handlers::register_sync_handlers(&mut b);
            fauna_nest::bridge_blob_handlers::register_bridge_blob_handlers(&mut b);
            fauna_nest::bridge_blob_handlers::register_capability_handlers(&mut b);
            // The bulk half's control plane — `fauna.segments.list` is where a
            // custodian turns the walk's coordinates into segments to fetch.
            fauna_nest::segments::register_segments_handlers(&mut b);
            // The conv arm's cases cross the production floor-roster report door
            // (`fauna.conversations.room.roster_report`).
            fauna_nest::conversations_handlers::register_conversations_handlers(&mut b);
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

/// Connect a custody-bearer client and wait for Connected — the custody
/// handshake mints on this connect, so a dead row fails HERE.
async fn connect_custody(nest: &Arc<NestClient>) -> Result<(), String> {
    nest.connect().await.map_err(|e| e.to_string())?;
    let mut state = nest.connection_state();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if *state.borrow() == fauna_client::types::ConnectionState::Connected {
                return;
            }
            if state.changed().await.is_err() {
                return;
            }
        }
    })
    .await
    .map_err(|_| "never reached Connected".to_string())?;
    Ok(())
}

fn witness_for(scopes: CustodyScopeSet) -> fauna_core::encoding::EmbedAsBytes {
    witness_for_id(scopes, GRANT_ID)
}

/// [`witness_for`] over an arbitrary grant id — the shape a custodian holding
/// several of one owner's rows presents, one witness per row.
fn witness_for_id(
    scopes: CustodyScopeSet,
    grant_id: [u8; CUSTODY_GRANT_ID_LEN],
) -> fauna_core::encoding::EmbedAsBytes {
    sign_custody_grant(
        &owner(),
        &CustodyGrant {
            grant_id: grant_id.to_vec(),
            owner: owner().actor_id(),
            custodian_key: custodian_key().verifying_key().to_bytes(),
            scopes,
            minted_at: Timestamp(1_000),
            expires_at: Timestamp(9_000_000_000_000_000), // far future (µs)
            removed_devices: Vec::new(),
        },
    )
    .expect("sign witness")
}

/// Mint the custody capability row over the real verbs — the W8.2 tier_3
/// recipe, verbatim: record-then-deposit through the real CAS.
async fn mint_custody_row(
    nest: &Arc<NestClient>,
    set: &CustodyScopeSet,
    grant_id: [u8; CUSTODY_GRANT_ID_LEN],
) {
    let now = Timestamp::now_secs().max(0) as u64;
    mint_custody_row_windowed(nest, set, grant_id, GrantWindow(now, now + 90 * 24 * 3600)).await;
}

/// [`mint_custody_row`] over an explicit window — the deposit path is
/// window-agnostic, so a post-dated row rides the same real verbs a live one
/// does (`record_mint` carries `window_start` as a first-class signed field).
async fn mint_custody_row_windowed(
    nest: &Arc<NestClient>,
    set: &CustodyScopeSet,
    grant_id: [u8; CUSTODY_GRANT_ID_LEN],
    window: GrantWindow,
) {
    let now = Timestamp::now_secs().max(0) as u64;
    let GrantWindow(window_start, window_end) = window;
    let custodian = custodian_key().verifying_key().to_bytes();
    let undeposited = custody_mint_blob(
        &owner().actor_id().0,
        &grant_id,
        &custodian,
        GrantWindow(window_start, window_end),
        set,
    )
    .expect("keyless custody blob");
    let mut log = fauna_core::succession_ledger::SuccessionLedger::empty(owner().actor_id());
    grant_log::record_mint(
        &mut log,
        owner().signing_key(),
        grant_id,
        custodian,
        custody_event_scopes(set),
        window_start,
        window_end,
        now,
    )
    .expect("record the Mint event");
    // The grant log is the succession ledger; the Mint is recorded before
    // the release, exactly as a minting machine records it. This test drives
    // the nest's custody door, not the grant-mint door, so the log stands in
    // for one the bound nest acknowledged (the door itself is pinned over a
    // real account runtime in `conformance_capability_reconcile_sweep_pass.rs`).
    let stored = log;
    let blob_bytes = undeposited
        .release(&PublishedGrants::from_published(
            &fauna_client_config::PublishedLedger::acknowledged_for_test(stored),
        ))
        .expect("released against the published log");
    let reply = CapabilitiesClient::new(Arc::clone(nest))
        .mint(blob_bytes)
        .await
        .expect("mint RPC");
    assert!(reply.ok, "the custody row deposited");
}

/// A fresh keyless custodied store for the pull side.
async fn custodied_store(dir: &std::path::Path) -> AccountStore<SqliteBackend> {
    AccountStore::open(
        SqliteBackend::open(dir).unwrap(),
        &owner().actor_id_hex(),
        WriterId(custodian_key().verifying_key().to_bytes()),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn a_credentialless_custodian_pulls_the_owners_planes_and_revoke_severs_live() {
    // Nest-side refusal warns are this test's only view of WHICH check
    // refused (convention 6; the custody-ceremony rig's precedent).
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    let (base, state, _tmp) = start_door_nest().await;

    // ── The owner: registered, sealing rows through the REAL writer door ──
    state
        .db
        .create_user_with_handle(&owner().actor_id().0, "free", "owner", None)
        .await
        .unwrap();
    let owner_nest = connected_client(&base, owner()).await;

    let store_dir = tempfile::tempdir().unwrap();
    let device_key = SigningKey::from_bytes(&[0x0A; 32]);
    let device_pub = device_key.verifying_key().to_bytes();
    let owner_store = AccountStore::open(
        SqliteBackend::open(store_dir.path()).unwrap(),
        &owner().actor_id_hex(),
        WriterId(device_pub),
    )
    .await
    .unwrap();
    let schedule = AccountStateKeySchedule::derive(&BackupKey::derive(owner().secret_bytes()));
    let trust = GenerationTrust {
        root: owner().actor_id(),
        prior: Vec::new(),
        trusted_holders: Default::default(),
    };
    let plane = AccountStatePlane::new(
        &owner_store,
        &owner_nest,
        &schedule,
        &device_key,
        &trust,
        ACCOUNT_STATE_SCOPE,
    )
    .unwrap();
    let sealed_value = fauna_core::encoding::canonical_encode(&ModerationConfig {
        muted_keywords: vec!["through-the-nest-door".into()],
        ..Default::default()
    })
    .unwrap()
    .to_vec();
    plane
        .put(
            &ItemId {
                kind: KIND_MODERATION.into(),
                key: MODERATION_KEY.into(),
            },
            sealed_value,
            Some(
                fauna_protocol::merge_policy::LwwStamp {
                    at_ms: 1_000,
                    writer: device_pub,
                }
                .encode()
                .unwrap(),
            ),
        )
        .await
        .expect("the owner's row seals and publishes to the nest feed");

    // ── The custody row (Account form) + the witness ──────────────────────
    mint_custody_row(&owner_nest, &CustodyScopeSet::Account, GRANT_ID).await;
    let witness = witness_for(CustodyScopeSet::Account);

    // ── The custodian: NO account credential on this nest — its whole auth
    // path is the custody handshake; its pull is the production
    // custody_pull over the production client.
    let custodian_nest = fauna_client::ws_custody_handshake_bearer::custody_nest_client(
        &base,
        owner().actor_id().0,
        custodian_key(),
        witness.clone(),
    );
    connect_custody(&custodian_nest)
        .await
        .expect("the custody handshake mints and the WS connects");
    let pull_dir = tempfile::tempdir().unwrap();
    let held = custodied_store(pull_dir.path()).await;
    let report = custody_pull(
        &held,
        &custodian_nest,
        ACCOUNT_STATE_SCOPE,
        Some(&owner().actor_id_hex()),
    )
    .await
    .expect("the custodian pulls the owner's sealed plane");
    assert!(report.recorded >= 1, "{report:?}");
    let rows = held
        .relay_rows(
            ACCOUNT_STATE_SCOPE,
            ItemClass::StateEntry.as_wire(),
            &[],
            u32::MAX,
        )
        .await
        .unwrap();
    assert!(!rows.is_empty(), "sealed rows held verbatim");
    assert_eq!(
        held.max_held_seq(ACCOUNT_STATE_SCOPE, &WriterId(device_pub))
            .await
            .unwrap(),
        None,
        "a custodian never journals"
    );

    // ── REVOKE: the row dies; the LIVE session refuses at its very next
    // request (per-request row re-check), and a fresh handshake refuses. ──
    let revoke = CapabilitiesClient::new(Arc::clone(&owner_nest))
        .revoke(GRANT_ID)
        .await
        .expect("revoke RPC");
    assert!(revoke.ok);
    // Wipe the cursor so the next pull genuinely re-asks the nest rather
    // than returning early on an empty page.
    let err = custody_pull(
        &held,
        &custodian_nest, // the SAME live session (bearer still cached + valid)
        ACCOUNT_STATE_SCOPE,
        Some(&owner().actor_id_hex()),
    )
    .await
    .expect_err("the live session refuses at the next request after revoke");
    assert!(
        err.to_string().contains("permission")
            || err.to_string().contains("denied")
            || err.to_string().contains("custody"),
        "the refusal is the custody gate, not transport noise: {err:#}"
    );
    // A fresh handshake refuses too — the mint's own live-row check.
    let fresh = fauna_client::ws_custody_handshake_bearer::custody_nest_client(
        &base,
        owner().actor_id().0,
        custodian_key(),
        witness,
    );
    connect_custody(&fresh)
        .await
        .expect_err("a fresh custody handshake refuses on the dead row");

    // ── The PoP key-binding, mutation-red in-process through the router ──
    // A request CLAIMING the witness's custodian key but SIGNED by another
    // key must refuse — remove the signature check and this arm greens.
    {
        let imposter = SigningKey::from_bytes(&[0xC6; 32]);
        let claimed = custodian_key().verifying_key().to_bytes();
        let ts = fauna_core::data::Timestamp::now_millis();
        let nonce = [0x77u8; 32];
        let nest_id = state.bound_identity();
        let msg = fauna_protocol::auth::custody_handshake_signed_message(
            &owner().actor_id().0,
            &claimed,
            ts,
            &nest_id,
            &nonce,
        );
        let req = fauna_protocol::auth::CustodyHandshakeRequest {
            owner_actor_id: owner().actor_id_hex(),
            custodian_key: fauna_core::hex32::encode(&claimed),
            timestamp: ts,
            signature: hex::encode(imposter.sign(&msg).to_bytes()),
            client_nonce: fauna_protocol::ByteBuf::from(nonce.to_vec()),
            witness: witness_for(CustodyScopeSet::Account),
            nest_id: fauna_core::hex32::encode(&nest_id),
            extra: Default::default(),
        };
        let meta = state
            .rpc_router
            .kind_meta("fauna.auth.custody_handshake")
            .expect("the kind is registered");
        let payload = Bytes::from(fauna_protocol::encode_canonical(&req).unwrap().to_vec());
        let err = (meta.handler)(Arc::clone(&state), [0u8; 32], payload)
            .await
            .expect_err("a PoP by the wrong key must refuse");
        assert!(
            err.code.contains("signature") || err.code.contains("auth"),
            "the refusal is the PoP check: {err:?}"
        );
    }

    // ── The scope bound, live at the door: an explicit-list grant serves
    // exactly its named scopes — the AdmittedScopes vocabulary at the nest.
    {
        let narrow_id = [0x2E; CUSTODY_GRANT_ID_LEN];
        let narrow_set = CustodyScopeSet::Scopes(vec!["state-fleet".into()]);
        mint_custody_row(&owner_nest, &narrow_set, narrow_id).await;
        let narrow_witness = sign_custody_grant(
            &owner(),
            &CustodyGrant {
                grant_id: narrow_id.to_vec(),
                owner: owner().actor_id(),
                custodian_key: custodian_key().verifying_key().to_bytes(),
                scopes: narrow_set,
                minted_at: Timestamp(1_000),
                expires_at: Timestamp(9_000_000_000_000_000),
                removed_devices: Vec::new(),
            },
        )
        .unwrap();
        let narrow = fauna_client::ws_custody_handshake_bearer::custody_nest_client(
            &base,
            owner().actor_id().0,
            custodian_key(),
            narrow_witness,
        );
        connect_custody(&narrow)
            .await
            .expect("the narrow grant's handshake mints");
        let narrow_dir = tempfile::tempdir().unwrap();
        let narrow_store = custodied_store(narrow_dir.path()).await;
        // The named scope serves (empty is fine — admission is the question)…
        custody_pull(
            &narrow_store,
            &narrow,
            "state-fleet",
            Some(&owner().actor_id_hex()),
        )
        .await
        .expect("the named scope serves under the explicit-list grant");
        // …and the UNNAMED state scope refuses — the row's own bound, live.
        custody_pull(
            &narrow_store,
            &narrow,
            ACCOUNT_STATE_SCOPE,
            Some(&owner().actor_id_hex()),
        )
        .await
        .expect_err("a scope outside the explicit list refuses at the door");
    }
}

/// **The record-cid arm of the custody door** — the walk half of the
/// asymmetry W8.6 left: the peer leg could pull an owner's CONTENT planes, the
/// nest leg served only the state-entry class.
///
/// Four properties, and three of them are refusals, because the whole risk here
/// is a door that answers too widely:
///
/// 1. an `Account`-form custodian walks the owner's own-actor content scope;
/// 2. ⚠ **the same custodian may NOT name a third party's scope** — the check
///    that `AdmittedScopes` structurally cannot make (it answers the scope half
///    only, and `content:mail:<stranger>` shape-checks and is not co-authored,
///    so without the account binding this door would serve a stranger's plane
///    to anyone holding any Account-form row);
/// 3. a `conv` scope under an `Account` row refuses **loudly** — never an empty
///    page, which a walking replica records as converged-empty forever;
/// 4. revoke severs the live session on the content arm too, not just the
///    state-entry arm — the per-request row re-check is shared.
#[tokio::test]
async fn the_custody_door_serves_the_owners_content_walk_and_refuses_every_wider_ask() {
    let (base, state, _tmp) = start_door_nest().await;
    let owner_id = owner().actor_id().0;
    state
        .db
        .create_user(&owner_id, "free", "owner")
        .await
        .unwrap();

    // A record on the owner's own-actor `mail` content scope, seeded through
    // the real mirror the feed derives from (fixture setup — the mail append
    // chain is proven by its own suite; the question here is the DOOR).
    let cid = fauna_cbor::Cid::of_dag_cbor(b"a sealed mail record");
    state
        .db
        .segment_records_insert_mail(
            &owner_id,
            1,
            &cid,
            "inbox",
            1_700_000_000,
            "example.test",
            "ham",
            false,
            1,
            None,
            0,
            1_700_000_000,
        )
        .await
        .expect("seed one mail record on the owner's scope");

    let owner_nest = connected_client(&base, owner()).await;
    mint_custody_row(&owner_nest, &CustodyScopeSet::Account, GRANT_ID).await;

    let custodian = fauna_client::ws_custody_handshake_bearer::custody_nest_client(
        &base,
        owner_id,
        custodian_key(),
        witness_for(CustodyScopeSet::Account),
    );
    connect_custody(&custodian)
        .await
        .expect("the custodian handshakes with no account credential");

    let owner_hex = owner().actor_id_hex();
    let mail_scope = format!("content:mail:{owner_hex}");

    // (1) The covered walk serves, and names the seeded record.
    let reply = content_walk(&custodian, &mail_scope, Some(&owner_hex))
        .await
        .expect("an Account-form custodian walks the owner's own-actor scope");
    assert_eq!(
        reply.changes.len(),
        1,
        "the owner's one mail record is on the custodian's walk"
    );
    assert_eq!(
        reply.changes[0].path_hash,
        hex::encode(cid.digest()),
        "the row carries the record CID's digest — the coordinate the walk pulls by"
    );

    // (2) ⚠ The account binding. A stranger's scope under the SAME row must
    //     refuse: the grant covers the owner's planes, not "any plane".
    let stranger = [0x77u8; 32];
    let stranger_scope = format!("content:mail:{}", hex::encode(stranger));
    let err = content_walk(&custodian, &stranger_scope, Some(&owner_hex))
        .await
        .expect_err("an Account-form row must not reach a third party's scope");
    assert_eq!(
        refusal_code(&err),
        "fauna.sync.permission_denied",
        "the refusal is authorization, not shape"
    );

    // (3) A conv scope under an Account row: refused loudly, never empty.
    let conv_scope = format!("content:conv:{}", hex::encode([0x99u8; 32]));
    let err = content_walk(&custodian, &conv_scope, Some(&owner_hex))
        .await
        .expect_err("a co-authored scope is outside the Account form");
    assert_eq!(
        refusal_code(&err),
        "fauna.sync.permission_denied",
        "a conv scope refuses, and does not answer an empty page: {err}"
    );

    // (4) Revoke severs THIS live session's next content request.
    CapabilitiesClient::new(Arc::clone(&owner_nest))
        .revoke(GRANT_ID)
        .await
        .expect("revoke the custody row");
    content_walk(&custodian, &mail_scope, Some(&owner_hex))
        .await
        .expect_err("the very next content request on the live session refuses");
}

/// **The bulk half of the same door.** The walk above yields
/// record-CID *coordinates* (`entry: None`); the bytes those coordinates name
/// live in the nest's segment store, whose two doors —
/// `fauna.segments.list` and `GET /api/v1/segments/{kind}/{actor}/{id}[/meta]`
/// — were owner-only, so a custodian could see what it held and never fetch it
/// (`account-data-plane.md` § Implementation status today, the W8-wide
/// residual's blob/block half).
///
/// `post` is the kind under test because it is the one this plane can both
/// serve and *adopt*: its records are filed under `Cid::of_dag_cbor(body)`,
/// the identity `fauna_account_store::segments::admit` re-hashes against
/// (`segments::list_handler`'s module doc). Every assertion is on
/// latency-independent state (e2e convention 14).
#[tokio::test]
async fn the_custody_door_serves_the_owners_record_bytes_and_refuses_every_wider_ask() {
    let (base, state, _tmp) = start_door_nest().await;
    let owner_id = owner().actor_id().0;
    state
        .db
        .create_user(&owner_id, "free", "owner")
        .await
        .unwrap();

    // One real post record through the nest's OWN append path — it writes the
    // segment file (the bytes) and the `segment_records` mirror (the walk's
    // coordinate) in one call, so the two halves cannot disagree in the fixture.
    let appended = fauna_nest::segments::post::append_body(
        &state.post_segments,
        &state.db,
        &owner_id,
        b"a post the custodian must be able to fetch, not merely name",
        1_715_000_000_000,
    )
    .await
    .expect("append one post body on the owner's scope");

    // ⚠ A stranger who really HAS segments of their own. Without this the
    // stranger arms below are vacuous: a stranger with no segment file on disk
    // is refused by `404 no such file` whether or not the authorization guard
    // exists, so the assertion would pass against a door that authorizes
    // nothing. The mutation round caught exactly that — neutering the byte
    // route's guard left the stranger arm green and reddened only the revoke
    // arm. With real bytes behind the stranger's actor, an unguarded door
    // hands them over and the arm reds for the reason it claims to test.
    let stranger = ActorKeypair::from_secret([0x77u8; 32]);
    let stranger_id = stranger.actor_id().0;
    let stranger_hex = stranger.actor_id_hex();
    state
        .db
        .create_user(&stranger_id, "free", "stranger")
        .await
        .unwrap();
    let stranger_seg = fauna_nest::segments::post::append_body(
        &state.post_segments,
        &state.db,
        &stranger_id,
        b"a third party's post, which no custody row over the owner may reach",
        1_715_000_000_000,
    )
    .await
    .expect("append one post body on the stranger's scope");

    let owner_nest = connected_client(&base, owner()).await;
    mint_custody_row(&owner_nest, &CustodyScopeSet::Account, GRANT_ID).await;

    let custodian = fauna_client::ws_custody_handshake_bearer::custody_nest_client(
        &base,
        owner_id,
        custodian_key(),
        witness_for(CustodyScopeSet::Account),
    );
    connect_custody(&custodian)
        .await
        .expect("the custodian handshakes with no account credential");

    let owner_hex = owner().actor_id_hex();

    // (1) The control plane: an Account-form custodian enumerates the owner's
    //     own-actor post segments — the same door the owner's own replica uses.
    let listed = segments_list(&custodian, "post", &owner_hex)
        .await
        .expect("an Account-form custodian lists the owner's post segments");
    assert!(
        listed
            .segments
            .iter()
            .any(|s| s.segment_id == appended.seg_id),
        "the segment the append landed in is on the custodian's list: {:?}",
        listed.segments
    );

    // (2) ⚠ The account binding — the half `AdmittedScopes` structurally
    //     cannot make. A stranger's actor under the SAME row must refuse: the
    //     grant covers the owner's planes, not "any plane".
    let err = segments_list(&custodian, "post", &stranger_hex)
        .await
        .expect_err("an Account-form row must not enumerate a third party's segments");
    assert_eq!(
        refusal_code(&err),
        "fauna.segments.not_owner",
        "the refusal is the owner-scoping one, indistinguishable from a stranger's"
    );

    // (3) The byte plane, both halves of the pair. `.meta` is not optional for
    //     an adopting replica — `record_order` lives nowhere else.
    let bytes = fauna_sync_engine::nest_client::SyncClient::new(
        Arc::clone(custodian.auth()),
        &custodian_key().verifying_key().to_bytes(),
    );
    let dat = bytes
        .get_segment_bytes("post", &owner_hex, appended.seg_id, 0, u64::MAX)
        .await
        .expect("the custodian fetches the .dat it was allowed to enumerate");
    assert!(!dat.is_empty(), "the framed segment carries bytes");
    bytes
        .get_segment_meta_bytes("post", &owner_hex, appended.seg_id, u64::MAX)
        .await
        .expect("and its .meta sidecar, without which adoption cannot rebuild the index");

    // (4) The byte plane refuses a third party for the same reason (2) does —
    //     a door that enumerated correctly and served anything would be worse
    //     than one that refused both. The stranger's segment id is named, so
    //     the bytes really are there to be wrongly served: this asserts
    //     authorization, not the absence of a file.
    bytes
        .get_segment_bytes("post", &stranger_hex, stranger_seg.seg_id, 0, u64::MAX)
        .await
        .expect_err("a stranger's segment bytes stay refused under an Account row");
    bytes
        .get_segment_meta_bytes("post", &stranger_hex, stranger_seg.seg_id, u64::MAX)
        .await
        .expect_err("and so does the sidecar half of the same pair");

    // (5) Revoke severs THIS live session on BOTH halves at their next request
    //     — the per-dispatch row re-check, not a next-evaluation bound.
    CapabilitiesClient::new(Arc::clone(&owner_nest))
        .revoke(GRANT_ID)
        .await
        .expect("revoke the custody row");
    segments_list(&custodian, "post", &owner_hex)
        .await
        .expect_err("the very next enumeration on the live session refuses");
    bytes
        .get_segment_bytes("post", &owner_hex, appended.seg_id, 0, u64::MAX)
        .await
        .expect_err("and the very next byte fetch refuses too");
}

/// **A placement journal is served to its owner alone — no grant reaches it.**
///
/// The journal joined the segment plane so a backup can carry it
/// (`segment-backup-protocol.md` § Client-device custodian (pull) → *Restore* →
/// *The placement journal rides the set*), and that made it reachable through
/// the same two doors a custodian's bulk half uses. It must not be: every
/// content kind on this plane rests sealed, and a journal rests as floor
/// plaintext — mailbox names and flags, readable by whoever fetches the file.
///
/// The holder here carries the WIDEST row that exists, an Account-form one,
/// which is what makes the assertion load-bearing: that row admits every
/// well-formed content scope it is asked about, and a journal tag is a
/// well-formed kind tag. Left to the grant's own vocabulary this door would
/// hand the journal over. Both refusals are the same answer a stranger gets.
///
/// The owner's own session is the positive half — without it a refusal proves
/// nothing, since a door that served the journal to nobody would pass too.
#[tokio::test]
async fn the_custody_door_refuses_the_owners_placement_journal_to_every_holder() {
    let (base, state, _tmp) = start_door_nest().await;
    let owner_id = owner().actor_id().0;
    let owner_hex = owner().actor_id_hex();
    state
        .db
        .create_user(&owner_id, "free", "owner")
        .await
        .unwrap();

    // A real journal segment, through the journal's own append path. A mailbox
    // name is exactly the kind of thing the refusal exists to keep private.
    let seg_id = state
        .mail_placement
        .append_event(
            &owner_id,
            &fauna_mail::segments::placement::MailPlacementRecord::Create {
                mailbox: "Letters from my lawyer".to_string(),
                uid_validity: 7,
                attrs: Vec::new(),
            },
        )
        .await
        .expect("journal one mailbox create on the owner's scope");

    let owner_nest = connected_client(&base, owner()).await;
    mint_custody_row(&owner_nest, &CustodyScopeSet::Account, GRANT_ID).await;
    let custodian = fauna_client::ws_custody_handshake_bearer::custody_nest_client(
        &base,
        owner_id,
        custodian_key(),
        witness_for(CustodyScopeSet::Account),
    );
    connect_custody(&custodian)
        .await
        .expect("the custodian handshakes with no account credential");

    // The owner's own session: listed, and both halves fetched.
    let listed = segments_list(&owner_nest, "mail-placement", &owner_hex)
        .await
        .expect("the owner lists their own placement journal");
    assert!(
        listed.segments.iter().any(|s| s.segment_id == seg_id),
        "the journal segment the append landed in is on the owner's list: {:?}",
        listed.segments
    );
    let owner_bytes =
        fauna_sync_engine::nest_client::SyncClient::new(Arc::clone(owner_nest.auth()), &owner_id);
    let dat = owner_bytes
        .get_segment_bytes("mail-placement", &owner_hex, seg_id, 0, u64::MAX)
        .await
        .expect("the owner fetches their own journal segment");
    assert!(
        dat.windows(7).any(|w| w == b"Letters"),
        "the journal rests as plaintext, which is the whole reason for the refusal below"
    );
    owner_bytes
        .get_segment_meta_bytes("mail-placement", &owner_hex, seg_id, u64::MAX)
        .await
        .expect("and its sidecar");

    // The widest holder there is: refused on the control plane…
    let err = segments_list(&custodian, "mail-placement", &owner_hex)
        .await
        .expect_err("no custody row enumerates a placement journal");
    assert_eq!(
        refusal_code(&err),
        "fauna.segments.not_owner",
        "the same answer an unrelated stranger gets"
    );
    // …and on the byte plane, both halves, with the segment id named so the
    // bytes really are there to be wrongly served.
    let holder_bytes = fauna_sync_engine::nest_client::SyncClient::new(
        Arc::clone(custodian.auth()),
        &custodian_key().verifying_key().to_bytes(),
    );
    holder_bytes
        .get_segment_bytes("mail-placement", &owner_hex, seg_id, 0, u64::MAX)
        .await
        .expect_err("no custody row fetches a placement journal's bytes");
    holder_bytes
        .get_segment_meta_bytes("mail-placement", &owner_hex, seg_id, u64::MAX)
        .await
        .expect_err("nor its sidecar");

    // The same row still reaches what it was minted for: the refusal is the
    // journal's, not a broken grant's.
    segments_list(&custodian, "mail", &owner_hex)
        .await
        .expect("the row still enumerates the owner's sealed mail");
}

/// One `fauna.segments.list` request over the real wire, addressed at an
/// explicit owner — the shape a custodian's bulk half issues.
async fn segments_list(
    rpc: &Arc<NestClient>,
    kind: &str,
    actor_hex: &str,
) -> Result<fauna_protocol::segments::SegmentsListReply, fauna_client::NestClientError> {
    use fauna_protocol::RpcRequester;
    rpc.request(
        "fauna.segments.list",
        fauna_protocol::segments::SegmentsListRequest {
            kind: kind.to_string(),
            actor_id: actor_hex.to_string(),
            extra: Default::default(),
        },
    )
    .await
}

/// **The custodian's nest leg, composed by the PUMP** (W8-wide residual item
/// 4 — charter § *Implementation status today*, the `**Recorded residuals
/// (W8-wide)**` paragraph): the two cases above drive `custody_nest_client` +
/// `custody_pull` by hand, which is exactly what the residual calls "v1 is
/// app-glue/tier_3-driven". Here the composition is production code: a
/// custodian's real [`AccountStoreRuntime`] converges on the owner's sealed
/// plane through its own pump.
///
/// Two staging choices carry the sentence, and both are load-bearing:
///
/// * `peer_transport: None` — there is no peer leg on this machine at all, so
///   the QUIC dial pass cannot be what converged it.
/// * the `custodies-held` row's `owner_devices` is **empty** — even with a
///   transport, the peer leg would have nothing to dial. `owner_nest_url` is
///   the row's only route to the owner's data, which is precisely the
///   always-on-anchor role the field's doc comment names.
///
/// The custodian's writer key IS the witness's `custodian_key`: the credential
/// slot is seeded before assembly, exactly as `resolve_writer_key_serialized`
/// would have minted it, so the runtime's device principal is the key the
/// owner signed the grant to.
///
/// Convention 14: the convergence criterion is a deadline poll over
/// `reconcile_now` — each iteration one full real pass — never a sleep.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_pump_pulls_a_custodied_owner_from_the_owners_nest_with_no_peer_leg() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    let (base, state, tmp) = start_door_nest().await;

    // ── The owner: registered, sealing one row through the REAL writer door ──
    state
        .db
        .create_user_with_handle(&owner().actor_id().0, "free", "owner", None)
        .await
        .unwrap();
    let owner_nest = connected_client(&base, owner()).await;

    let owner_store_dir = tempfile::tempdir().unwrap();
    let owner_device_key = SigningKey::from_bytes(&[0x0A; 32]);
    let owner_device_pub = owner_device_key.verifying_key().to_bytes();
    let owner_store = AccountStore::open(
        SqliteBackend::open(owner_store_dir.path()).unwrap(),
        &owner().actor_id_hex(),
        WriterId(owner_device_pub),
    )
    .await
    .unwrap();
    let schedule = AccountStateKeySchedule::derive(&BackupKey::derive(owner().secret_bytes()));
    let trust = GenerationTrust {
        root: owner().actor_id(),
        prior: Vec::new(),
        trusted_holders: Default::default(),
    };
    AccountStatePlane::new(
        &owner_store,
        &owner_nest,
        &schedule,
        &owner_device_key,
        &trust,
        ACCOUNT_STATE_SCOPE,
    )
    .unwrap()
    .put(
        &ItemId {
            kind: KIND_MODERATION.into(),
            key: MODERATION_KEY.into(),
        },
        fauna_core::encoding::canonical_encode(&ModerationConfig {
            muted_keywords: vec!["pulled-by-the-pump".into()],
            ..Default::default()
        })
        .unwrap()
        .to_vec(),
        Some(
            fauna_protocol::merge_policy::LwwStamp {
                at_ms: 1_000,
                writer: owner_device_pub,
            }
            .encode()
            .unwrap(),
        ),
    )
    .await
    .expect("the owner's row seals and publishes to the nest feed");

    // ── The custody row (Account form) + the witness ──────────────────────
    mint_custody_row(&owner_nest, &CustodyScopeSet::Account, GRANT_ID).await;
    let witness = witness_for(CustodyScopeSet::Account);

    // ── The custodian: a DIFFERENT account, with its own nest session for
    //    its own plane and no credential whatsoever for the owner's. ──────
    state
        .db
        .create_user_with_handle(&custodian_account().actor_id().0, "free", "custodian", None)
        .await
        .unwrap();
    let custodian_hex = custodian_account().actor_id_hex();
    let custodian_nest = connected_client(&base, custodian_account()).await;

    let state_base = tmp.path().join("custodian-state");
    let store_root = StoreRoot::at(state_base.clone());
    let creds_dir = tmp.path().join("custodian-creds");
    // The runtime's writer key = the grant's `custodian_key`. Seeding the slot
    // is what `resolve_writer_key_serialized` does on a fresh machine, with the
    // one difference that the bytes are the ones the owner already signed to.
    fauna_credential_store::cred_file_write(
        &creds_dir,
        CRED_NAMESPACE,
        &std::collections::BTreeMap::from([(custodian_hex.clone(), hex::encode(CUSTODIAN_SECRET))]),
    )
    .expect("seed the custodian's writer-key slot");
    let creds = CredentialStore::with_file_backend(CRED_NAMESPACE, creds_dir);

    // The `custodies-held` row the W8.4 ceremony would have delivered —
    // staged on the custodian's OWN plane before assembly, the
    // `custody_convergence.rs` recipe.
    {
        let own_dir = store_root.store_dir(&custodian_hex).expect("store dir");
        let own = AccountStore::open(
            SqliteBackend::open(&own_dir).unwrap(),
            &custodian_hex,
            WriterId(custodian_key().verifying_key().to_bytes()),
        )
        .await
        .unwrap();
        let kind = fauna_protocol::merge_policy::KIND_CUSTODIES_HELD;
        own.put_state(fauna_account_store::types::StateEntry {
            kind: kind.into(),
            key: fauna_core::custody_grant::custody_entry_key(&GRANT_ID),
            scope: fauna_protocol::merge_policy::home_scope_for_kind(kind)
                .unwrap()
                .into(),
            value: fauna_core::encoding::canonical_encode(
                &fauna_core::custodies_held::CustodyHeld {
                    grant_id: GRANT_ID.to_vec(),
                    owner: owner().actor_id().0,
                    witness: fauna_core::encoding::canonical_encode(&witness)
                        .unwrap()
                        .to_vec(),
                    // EMPTY on purpose — the peer leg has nothing to dial.
                    owner_devices: Vec::new(),
                    owner_nest_url: Some(base.clone()),
                    retained_bytes_cap: 8 * 1024 * 1024 * 1024,
                    ..Default::default()
                },
            )
            .unwrap()
            .to_vec(),
            merge_meta: None,
            entry_version: 0,
            tombstone: false,
        })
        .await
        .unwrap();
    }

    let handle = AccountStoreRuntime::start(AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        store_root: store_root.clone(),
        actor_id_hex: custodian_hex.clone(),
        rpc: Arc::clone(&custodian_nest),
        process_rpc: None,
        principal: RuntimePrincipal::SeedHolding(custodian_account().into()),
        credentials: creds,
        reconnects: None,
        pushes: None,
        backstop_interval: Duration::from_secs(3600), // disarmed: passes are explicit
        memberships: None,
        trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(Vec::new()),
        attested_predecessors: Default::default(),
        linked_nests: None,
        owed_nests: None,
        peer_transport: None, // ⚠ no peer leg on this machine at all
        // The machine's named row (the RULED 2026-09-28 block, decision 3).
        enrollment_target_device_id: "cd".repeat(32),
    })
    .await
    .expect("the custodian's runtime assembles");

    // ── Convergence: the custodied store holds the owner's sealed row ─────
    let custodied_dir = store_root
        .store_dir(&owner().actor_id_hex())
        .expect("custodied store dir");
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        let report = handle.reconcile_now().await.expect("pass");
        let last = format!("{report:?}");
        // The custodied store is opened by the pump itself; read it on its own
        // connection rather than holding one across passes.
        if custodied_dir.exists() {
            let held = AccountStore::open(
                SqliteBackend::open(&custodied_dir).unwrap(),
                &owner().actor_id_hex(),
                WriterId(custodian_key().verifying_key().to_bytes()),
            )
            .await
            .unwrap();
            let rows = held
                .relay_rows(
                    ACCOUNT_STATE_SCOPE,
                    ItemClass::StateEntry.as_wire(),
                    &[],
                    u32::MAX,
                )
                .await
                .unwrap();
            if !rows.is_empty() {
                assert_eq!(
                    held.max_held_seq(ACCOUNT_STATE_SCOPE, &WriterId(owner_device_pub))
                        .await
                        .unwrap(),
                    None,
                    "a custodian never journals — the rows are relay-plane only"
                );
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the pump never pulled the owner's plane from the owner's nest; \
             last report: {last}"
        );
        tokio::task::yield_now().await;
    }
}

/// One `record-cid` feed request over the real wire — the request shape
/// `ContentScopePlane` builds, issued directly so the test can assert on the
/// refusals as well as the served page.
/// The wire code behind a refusal, for assertions that are about *which*
/// refusal fired.
///
/// Asserting on `err.to_string()` is what broke all of this file's refusal
/// checks at once: since 2026-08-25 `NestClientError::Display`
/// renders the LOCALIZED sentence, which deliberately carries no wire
/// vocabulary — `version-compatibility.md` § Dimension 4, and the ratified
/// "no diagnostic ever reaches user-facing text" contract. A localized string
/// is also i18n-mutable, so pinning one pins a moving target. The code is the
/// stable, contractual half of the wire error, and is what the nest's own unit
/// tests assert on (`segments/list_handler.rs:316`).
fn refusal_code(err: &fauna_client::NestClientError) -> &str {
    match err {
        fauna_client::NestClientError::Rpc(e) => e.code.as_str(),
        other => panic!("expected a wire-level refusal, got a transport fault: {other:?}"),
    }
}

async fn content_walk(
    rpc: &Arc<NestClient>,
    scope: &str,
    of_owner: Option<&str>,
) -> Result<fauna_protocol::sync::SyncChangesListReply, fauna_client::NestClientError> {
    use fauna_protocol::RpcRequester;
    rpc.request(
        "fauna.sync.changes.list",
        fauna_protocol::sync::SyncChangesListRequest {
            since: 0,
            item_class: Some(ItemClass::RecordCid.as_wire().to_string()),
            scope: Some(scope.to_string()),
            frontier: None,
            of_owner: of_owner.map(str::to_string),
            ..Default::default()
        },
    )
    .await
}

/// **PROBE-372-A — the custody window's START bound binds at the mint door and
/// NOWHERE else, so a not-yet-valid custody row authorizes pulls today.**
///
/// `GrantWindow` is `[epoch_start, epoch_end]` and both bounds are
/// first-class: `grant_log::record_mint` signs `window_start` into the owner's
/// own grant log, and `custody_auth_core` refuses a row whose window has not
/// opened (`auth_core.rs`, `blob.window.0 > now_secs`). But that is the only
/// site in the nest that ever reads `window.0` — the storage table has no
/// `epoch_start` column at all (`migrations.rs`, `capability_grants`), so
/// `fetch_capability_grants_for_holder` filters on `epoch_end >= now` alone
/// and every downstream consumer inherits a half-checked window:
/// `custody_admits_scope` (both feed arms) and `is_live_custody_holder` (the
/// `CallerClass::Custodian` derivation).
///
/// The consequence is not theoretical, and it needs no forged material: a
/// custodian legitimately holding TWO of one owner's rows — one live and
/// narrow, one post-dated and wide — mints its bearer on the live row and then
/// pulls the scope only the post-dated row admits. The window a user set to
/// start next month is open now.
///
/// The arms below are exactly that, and the delta between them is ONLY the
/// window: the positive control (an `Account`-form row on a live window mints
/// and serves this same scope) is
/// `a_credentialless_custodian_pulls_the_owners_planes_and_revoke_severs_live`
/// above, and the narrow row's refusal of `ACCOUNT_STATE_SCOPE` is pinned by
/// that test's explicit-list arm.
#[tokio::test]
async fn a_post_dated_custody_row_must_not_admit_before_its_window_opens() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    let (base, state, _tmp) = start_door_nest().await;
    state
        .db
        .create_user_with_handle(&owner().actor_id().0, "free", "owner", None)
        .await
        .unwrap();
    let owner_nest = connected_client(&base, owner()).await;

    // ── Row A: LIVE, explicit-list, names only "state-fleet". Its whole job
    // is to be a legitimate reason for this custodian to hold a session. ──
    const LIVE_ID: [u8; CUSTODY_GRANT_ID_LEN] = [0x7A; CUSTODY_GRANT_ID_LEN];
    let live_set = CustodyScopeSet::Scopes(vec!["state-fleet".into()]);
    mint_custody_row(&owner_nest, &live_set, LIVE_ID).await;

    // ── Row B: the SAME custodian, Account form (so it admits the account
    // -state scope), on a window that opens in 30 days. ──────────────────
    const FUTURE_ID: [u8; CUSTODY_GRANT_ID_LEN] = [0x7B; CUSTODY_GRANT_ID_LEN];
    let now = Timestamp::now_secs().max(0) as u64;
    mint_custody_row_windowed(
        &owner_nest,
        &CustodyScopeSet::Account,
        FUTURE_ID,
        GrantWindow(now + 30 * 24 * 3600, now + 90 * 24 * 3600),
    )
    .await;

    // ── Control: the mint door DOES enforce the start bound — a handshake
    // presenting row B alone refuses, which is what makes the bound real
    // vocabulary rather than an unused field. ────────────────────────────
    let future_only = fauna_client::ws_custody_handshake_bearer::custody_nest_client(
        &base,
        owner().actor_id().0,
        custodian_key(),
        witness_for_id(CustodyScopeSet::Account, FUTURE_ID),
    );
    connect_custody(&future_only)
        .await
        .expect_err("the mint door refuses a custody row whose window has not opened");

    // ── The gap: mint on row A, then ask for the scope only row B admits. ──
    let session = fauna_client::ws_custody_handshake_bearer::custody_nest_client(
        &base,
        owner().actor_id().0,
        custodian_key(),
        witness_for_id(live_set, LIVE_ID),
    );
    connect_custody(&session)
        .await
        .expect("the live narrow row's handshake mints");
    let pull_dir = tempfile::tempdir().unwrap();
    let held = custodied_store(pull_dir.path()).await;
    custody_pull(
        &held,
        &session,
        ACCOUNT_STATE_SCOPE,
        Some(&owner().actor_id_hex()),
    )
    .await
    .expect_err(
        "a custody row whose window opens in 30 days must not admit a scope today — \
         the start bound is checked at the mint door and nowhere else",
    );
}

/// **The nest leg's SEGMENT composition** (`account-data-plane.md`
/// § Implementation status today → *Built — the custodian's nest leg in the
/// pump*, residual (2)): the pass above proves the pump pulls the owner's
/// *coordinates* with no peer leg. This proves it pulls the **bytes those
/// coordinates name** — the difference the charter draws between "real
/// redundancy" and "a restore-from-custody".
///
/// The two halves it composes were both landed 2026-08-16 and never joined:
/// the nest leg (W8.6's pump half) walks content scopes for record-CID rows
/// and stops there, while the bulk plane opened
/// `fauna.segments.list` and the segment byte pair to `CallerClass::Custodian`
/// — and had, until this test, **zero production callers** addressing another
/// actor. So the assertion below is on the composition, not on either door.
///
/// Staging is the pump case's, with one addition: a real `post` body appended
/// through the nest's own writer path, so the segment file and the
/// `segment_records` mirror cannot disagree. `post` is the kind under test
/// because it is the one this plane can both serve and *adopt* — its records
/// are filed under `Cid::of_dag_cbor(body)`, the identity
/// `fauna_account_store::segments::admit` re-hashes every block against,
/// whereas mail files records under a sequenced record id
/// (`segments::list_handler`'s module doc owns that distinction).
///
/// `peer_transport: None` and an empty `owner_devices` again: no peer leg
/// exists and none could dial, so `pull_missing_blocks` cannot be what moved
/// the bytes. Convention 14: convergence is a deadline poll over
/// `reconcile_now`, never a sleep.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_pump_pulls_the_owners_record_bytes_from_the_owners_nest_with_no_peer_leg() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    let (base, state, tmp) = start_door_nest().await;

    state
        .db
        .create_user_with_handle(&owner().actor_id().0, "free", "owner", None)
        .await
        .unwrap();
    let owner_nest = connected_client(&base, owner()).await;
    let owner_hex = owner().actor_id_hex();

    // The bytes under test: one real post body on the owner's own-actor post
    // scope, written by the nest's production append path.
    const POST_BODY: &[u8] =
        b"the body a nest-anchored custodian must actually HOLD, not merely name";
    let appended = fauna_nest::segments::post::append_body(
        &state.post_segments,
        &state.db,
        &owner().actor_id().0,
        POST_BODY,
        1_715_000_000_000,
    )
    .await
    .expect("append one post body on the owner's scope");

    // ── The custody row (Account form) + the witness ──────────────────────
    mint_custody_row(&owner_nest, &CustodyScopeSet::Account, GRANT_ID).await;
    let witness = witness_for(CustodyScopeSet::Account);

    // ── The custodian: a different account, no credential for the owner ───
    state
        .db
        .create_user_with_handle(&custodian_account().actor_id().0, "free", "custodian", None)
        .await
        .unwrap();
    let custodian_hex = custodian_account().actor_id_hex();
    let custodian_nest = connected_client(&base, custodian_account()).await;

    let state_base = tmp.path().join("custodian-state");
    let store_root = StoreRoot::at(state_base.clone());
    let creds_dir = tmp.path().join("custodian-creds");
    fauna_credential_store::cred_file_write(
        &creds_dir,
        CRED_NAMESPACE,
        &std::collections::BTreeMap::from([(custodian_hex.clone(), hex::encode(CUSTODIAN_SECRET))]),
    )
    .expect("seed the custodian's writer-key slot");
    let creds = CredentialStore::with_file_backend(CRED_NAMESPACE, creds_dir);

    {
        let own_dir = store_root.store_dir(&custodian_hex).expect("store dir");
        let own = AccountStore::open(
            SqliteBackend::open(&own_dir).unwrap(),
            &custodian_hex,
            WriterId(custodian_key().verifying_key().to_bytes()),
        )
        .await
        .unwrap();
        let kind = fauna_protocol::merge_policy::KIND_CUSTODIES_HELD;
        own.put_state(fauna_account_store::types::StateEntry {
            kind: kind.into(),
            key: fauna_core::custody_grant::custody_entry_key(&GRANT_ID),
            scope: fauna_protocol::merge_policy::home_scope_for_kind(kind)
                .unwrap()
                .into(),
            value: fauna_core::encoding::canonical_encode(
                &fauna_core::custodies_held::CustodyHeld {
                    grant_id: GRANT_ID.to_vec(),
                    owner: owner().actor_id().0,
                    witness: fauna_core::encoding::canonical_encode(&witness)
                        .unwrap()
                        .to_vec(),
                    // EMPTY on purpose — the peer leg has nothing to dial.
                    owner_devices: Vec::new(),
                    owner_nest_url: Some(base.clone()),
                    retained_bytes_cap: 8 * 1024 * 1024 * 1024,
                    ..Default::default()
                },
            )
            .unwrap()
            .to_vec(),
            merge_meta: None,
            entry_version: 0,
            tombstone: false,
        })
        .await
        .unwrap();
    }

    let handle = AccountStoreRuntime::start(AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        store_root: store_root.clone(),
        actor_id_hex: custodian_hex.clone(),
        rpc: Arc::clone(&custodian_nest),
        process_rpc: None,
        principal: RuntimePrincipal::SeedHolding(custodian_account().into()),
        credentials: creds,
        reconnects: None,
        pushes: None,
        backstop_interval: Duration::from_secs(3600), // disarmed: passes are explicit
        memberships: None,
        trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(Vec::new()),
        attested_predecessors: Default::default(),
        linked_nests: None,
        owed_nests: None,
        peer_transport: None, // ⚠ no peer leg on this machine at all
        // The machine's named row (the RULED 2026-09-28 block, decision 3).
        enrollment_target_device_id: "cd".repeat(32),
    })
    .await
    .expect("the custodian's runtime assembles");

    let post_scope = fauna_protocol::scope::ContentScope::new("post", owner().actor_id().0)
        .expect("the owner's own-actor post scope")
        .to_string();
    let custodied_dir = store_root
        .store_dir(&owner_hex)
        .expect("custodied store dir");

    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    loop {
        let report = handle.reconcile_now().await.expect("pass");
        let last = format!("{report:?}");
        if custodied_dir.exists() {
            let held = AccountStore::open(
                SqliteBackend::open(&custodied_dir).unwrap(),
                &owner_hex,
                WriterId(custodian_key().verifying_key().to_bytes()),
            )
            .await
            .unwrap();
            // The composition's own witness: the segment file is ADOPTED, and
            // the record's bytes are readable out of it. Asserting only on the
            // index row would pass on the coordinate walk alone — which is
            // exactly the state this row exists to leave behind.
            if let Some(bytes) = held.block(&appended.record_cid).await.unwrap() {
                assert_eq!(
                    bytes, POST_BODY,
                    "the custodian holds the owner's record bytes verbatim"
                );
                let adopted = held.adopted_segments(&post_scope).await.unwrap();
                assert!(
                    adopted.iter().any(|k| k.segment_id == appended.seg_id
                        && k.kind == "post"
                        && k.scope == post_scope),
                    "the bytes arrived by SEGMENT ADOPTION on the owner's post scope, \
                     not by some other route: {adopted:?}"
                );
                assert_eq!(
                    held.max_held_seq(
                        &post_scope,
                        &WriterId(custodian_key().verifying_key().to_bytes())
                    )
                    .await
                    .unwrap(),
                    None,
                    "a custodian never journals — adoption is bulk transport of truth the \
                     origin already journaled, so re-announcing it on this replica's own \
                     log would forge authorship of records it merely mirrors"
                );
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the pump never pulled the owner's record BYTES from the owner's nest \
             (coordinates alone are not a restore-from-custody); last report: {last}"
        );
        tokio::task::yield_now().await;
    }
}

/// **Row 61 — mail adoptability, end to end** (`message-segment-store.md`
/// § Record identity per kind): the record-identity cutover makes mail the
/// second adoptable kind — `segments::mail::append_record` files under the
/// content hash of the stored envelope bytes, the exact identity
/// `fauna_account_store::segments::admit` re-hashes every block against — and
/// this proves the full composition on the kind that motivated the ruling: a
/// sealed mail record appended through the nest's production path is ADOPTED
/// by a nest-anchored custodian (bytes held, filing identity re-derivable
/// from the held bytes) with no peer leg and no owner device.
///
/// Staging is the post case's, with two mail-specific steps: the record goes
/// through `segments::mail::append_record` (the S6.12b sealed-carrier gate),
/// and the open segment is FINALIZED before the pull — adoption enumerates
/// finalized segments (`admit` refuses an index-less active CARv2 by design).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_pump_adopts_the_owners_mail_segments_end_to_end() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    let (base, state, tmp) = start_door_nest().await;

    state
        .db
        .create_user_with_handle(&owner().actor_id().0, "free", "owner", None)
        .await
        .unwrap();
    let owner_nest = connected_client(&base, owner()).await;
    let owner_hex = owner().actor_id_hex();

    // The bytes under test: one sealed mail record on the owner's mail scope,
    // written by the nest's production append path. Opaque at-rest carrier —
    // the custodian must hold it without being able to open it.
    let appended = fauna_nest::segments::mail::append_record(
        &state.mail_segments,
        &state.db,
        &owner().actor_id().0,
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            b"the sealed mail a custodian must actually HOLD, not merely name".to_vec(),
        ),
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            b"sealed-index-hint".to_vec(),
        ),
        fauna_mail::segments::MailFloorMetadata {
            format_version: fauna_mail::segments::MAIL_FLOOR_FORMAT_VERSION,
            received_at: 1_715_000_000_000,
            timestamp: 1_715_000_000,
            sender_domain: "example.com".to_string(),
            spam_disposition: "accept".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("append one sealed mail record on the owner's scope");
    assert!(appended.inserted, "first append inserts");
    state
        .mail_segments
        .finalize_open(&owner().actor_id().0)
        .await
        .expect("finalize the open mail segment so the pair is adoptable");

    // ── The custody row (Account form) + the witness ──────────────────────
    mint_custody_row(&owner_nest, &CustodyScopeSet::Account, GRANT_ID).await;
    let witness = witness_for(CustodyScopeSet::Account);

    // ── The custodian: a different account, no credential for the owner ───
    state
        .db
        .create_user_with_handle(&custodian_account().actor_id().0, "free", "custodian", None)
        .await
        .unwrap();
    let custodian_hex = custodian_account().actor_id_hex();
    let custodian_nest = connected_client(&base, custodian_account()).await;

    let state_base = tmp.path().join("custodian-state");
    let store_root = StoreRoot::at(state_base.clone());
    let creds_dir = tmp.path().join("custodian-creds");
    fauna_credential_store::cred_file_write(
        &creds_dir,
        CRED_NAMESPACE,
        &std::collections::BTreeMap::from([(custodian_hex.clone(), hex::encode(CUSTODIAN_SECRET))]),
    )
    .expect("seed the custodian's writer-key slot");
    let creds = CredentialStore::with_file_backend(CRED_NAMESPACE, creds_dir);

    {
        let own_dir = store_root.store_dir(&custodian_hex).expect("store dir");
        let own = AccountStore::open(
            SqliteBackend::open(&own_dir).unwrap(),
            &custodian_hex,
            WriterId(custodian_key().verifying_key().to_bytes()),
        )
        .await
        .unwrap();
        let kind = fauna_protocol::merge_policy::KIND_CUSTODIES_HELD;
        own.put_state(fauna_account_store::types::StateEntry {
            kind: kind.into(),
            key: fauna_core::custody_grant::custody_entry_key(&GRANT_ID),
            scope: fauna_protocol::merge_policy::home_scope_for_kind(kind)
                .unwrap()
                .into(),
            value: fauna_core::encoding::canonical_encode(
                &fauna_core::custodies_held::CustodyHeld {
                    grant_id: GRANT_ID.to_vec(),
                    owner: owner().actor_id().0,
                    witness: fauna_core::encoding::canonical_encode(&witness)
                        .unwrap()
                        .to_vec(),
                    // EMPTY on purpose — the peer leg has nothing to dial.
                    owner_devices: Vec::new(),
                    owner_nest_url: Some(base.clone()),
                    retained_bytes_cap: 8 * 1024 * 1024 * 1024,
                    ..Default::default()
                },
            )
            .unwrap()
            .to_vec(),
            merge_meta: None,
            entry_version: 0,
            tombstone: false,
        })
        .await
        .unwrap();
    }

    let handle = AccountStoreRuntime::start(AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        store_root: store_root.clone(),
        actor_id_hex: custodian_hex.clone(),
        rpc: Arc::clone(&custodian_nest),
        process_rpc: None,
        principal: RuntimePrincipal::SeedHolding(custodian_account().into()),
        credentials: creds,
        reconnects: None,
        pushes: None,
        backstop_interval: Duration::from_secs(3600), // disarmed: passes are explicit
        memberships: None,
        trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(Vec::new()),
        attested_predecessors: Default::default(),
        linked_nests: None,
        owed_nests: None,
        peer_transport: None, // ⚠ no peer leg on this machine at all
        // The machine's named row (the RULED 2026-09-28 block, decision 3).
        enrollment_target_device_id: "cd".repeat(32),
    })
    .await
    .expect("the custodian's runtime assembles");

    let mail_scope = fauna_protocol::scope::ContentScope::new("mail", owner().actor_id().0)
        .expect("the owner's own-actor mail scope")
        .to_string();
    let custodied_dir = store_root
        .store_dir(&owner_hex)
        .expect("custodied store dir");

    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    loop {
        let report = handle.reconcile_now().await.expect("pass");
        let last = format!("{report:?}");
        if custodied_dir.exists() {
            let held = AccountStore::open(
                SqliteBackend::open(&custodied_dir).unwrap(),
                &owner_hex,
                WriterId(custodian_key().verifying_key().to_bytes()),
            )
            .await
            .unwrap();
            if let Some(bytes) = held.block(&appended.cid).await.unwrap() {
                // The cutover's whole point, asserted on the HELD bytes: the
                // filing identity re-derives from the bytes themselves — the
                // property `admit`'s anti-poisoning re-hash checked on the way
                // in, and the reason mail is adoptable at all.
                assert_eq!(
                    fauna_cbor::Cid::of_dag_cbor(&bytes),
                    appended.cid,
                    "the held block re-hashes to its filing identity"
                );
                let env = fauna_mail::segments::MailRecordEnvelope::decode(&bytes)
                    .expect("the held block is the verbatim stored mail envelope");
                assert_eq!(
                    env.encrypted_body,
                    b"the sealed mail a custodian must actually HOLD, not merely name".to_vec(),
                    "the sealed body survives adoption verbatim (and stays sealed)"
                );
                let adopted = held.adopted_segments(&mail_scope).await.unwrap();
                assert!(
                    adopted.iter().any(|k| k.segment_id == appended.seg_id
                        && k.kind == "mail"
                        && k.scope == mail_scope),
                    "the bytes arrived by SEGMENT ADOPTION on the owner's mail scope, \
                     not by some other route: {adopted:?}"
                );
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the pump never adopted the owner's MAIL segments from the owner's nest \
             (mail is the bulk of what a custodian should hold); \
             last report: {last}"
        );
        tokio::task::yield_now().await;
    }
}

/// **Row 69 — calendar + card on the serve plane, end to end**
/// (`message-segment-store.md` § *Which kinds the two planes serve*): the
/// joint cutover leg made both kinds adoptable, but adoption also needs a
/// source willing to enumerate and serve — the `segment_manager_for_kind`
/// gate, which answered `mail | post` only, making the widened pin inert.
/// This proves the wiring: a sealed calendar event and a sealed vCard,
/// appended through each kind's production path, are ADOPTED by a
/// nest-anchored custodian with no peer leg and no owner device — the exact
/// arm row 61 could not write because this gate made it impossible.
///
/// Staging is the mail case's, per kind: production append (the S6.12
/// sealed-carrier gate), then FINALIZE (adoption enumerates finalized
/// segments only).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_pump_adopts_the_owners_calendar_and_card_segments_end_to_end() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    let (base, state, tmp) = start_door_nest().await;

    state
        .db
        .create_user_with_handle(&owner().actor_id().0, "free", "owner", None)
        .await
        .unwrap();
    let owner_nest = connected_client(&base, owner()).await;
    let owner_hex = owner().actor_id_hex();

    // One sealed record per kind, through each kind's production append path.
    let cal_appended = fauna_nest::segments::cal::append_record(
        &state.cal_segments,
        &state.db,
        &owner().actor_id().0,
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            b"the sealed EVENT a custodian must hold without opening".to_vec(),
        ),
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            b"sealed-cal-hint".to_vec(),
        ),
        &fauna_calendar::segments::CalFloorMetadata {
            created_at: 1_715_000_000,
            ..Default::default()
        },
    )
    .await
    .expect("append one sealed calendar record on the owner's scope");
    state
        .cal_segments
        .finalize_open(&owner().actor_id().0)
        .await
        .expect("finalize the open calendar segment so the pair is adoptable");

    let card_appended = fauna_nest::segments::card::append_record(
        &state.card_segments,
        &state.db,
        &owner().actor_id().0,
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            b"the sealed VCARD a custodian must hold without opening".to_vec(),
        ),
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            b"sealed-card-hint".to_vec(),
        ),
        &fauna_contacts::segments::CardFloorMetadata {
            created_at: 1_715_000_000,
            ..Default::default()
        },
    )
    .await
    .expect("append one sealed card record on the owner's scope");
    state
        .card_segments
        .finalize_open(&owner().actor_id().0)
        .await
        .expect("finalize the open card segment so the pair is adoptable");

    // ── The custody row (Account form) + the witness ──────────────────────
    mint_custody_row(&owner_nest, &CustodyScopeSet::Account, GRANT_ID).await;
    let witness = witness_for(CustodyScopeSet::Account);

    // ── The custodian: a different account, no credential for the owner ───
    state
        .db
        .create_user_with_handle(&custodian_account().actor_id().0, "free", "custodian", None)
        .await
        .unwrap();
    let custodian_hex = custodian_account().actor_id_hex();
    let custodian_nest = connected_client(&base, custodian_account()).await;

    let state_base = tmp.path().join("custodian-state");
    let store_root = StoreRoot::at(state_base.clone());
    let creds_dir = tmp.path().join("custodian-creds");
    fauna_credential_store::cred_file_write(
        &creds_dir,
        CRED_NAMESPACE,
        &std::collections::BTreeMap::from([(custodian_hex.clone(), hex::encode(CUSTODIAN_SECRET))]),
    )
    .expect("seed the custodian's writer-key slot");
    let creds = CredentialStore::with_file_backend(CRED_NAMESPACE, creds_dir);

    {
        let own_dir = store_root.store_dir(&custodian_hex).expect("store dir");
        let own = AccountStore::open(
            SqliteBackend::open(&own_dir).unwrap(),
            &custodian_hex,
            WriterId(custodian_key().verifying_key().to_bytes()),
        )
        .await
        .unwrap();
        let kind = fauna_protocol::merge_policy::KIND_CUSTODIES_HELD;
        own.put_state(fauna_account_store::types::StateEntry {
            kind: kind.into(),
            key: fauna_core::custody_grant::custody_entry_key(&GRANT_ID),
            scope: fauna_protocol::merge_policy::home_scope_for_kind(kind)
                .unwrap()
                .into(),
            value: fauna_core::encoding::canonical_encode(
                &fauna_core::custodies_held::CustodyHeld {
                    grant_id: GRANT_ID.to_vec(),
                    owner: owner().actor_id().0,
                    witness: fauna_core::encoding::canonical_encode(&witness)
                        .unwrap()
                        .to_vec(),
                    // EMPTY on purpose — the peer leg has nothing to dial.
                    owner_devices: Vec::new(),
                    owner_nest_url: Some(base.clone()),
                    retained_bytes_cap: 8 * 1024 * 1024 * 1024,
                    ..Default::default()
                },
            )
            .unwrap()
            .to_vec(),
            merge_meta: None,
            entry_version: 0,
            tombstone: false,
        })
        .await
        .unwrap();
    }

    let handle = AccountStoreRuntime::start(AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        store_root: store_root.clone(),
        actor_id_hex: custodian_hex.clone(),
        rpc: Arc::clone(&custodian_nest),
        process_rpc: None,
        principal: RuntimePrincipal::SeedHolding(custodian_account().into()),
        credentials: creds,
        reconnects: None,
        pushes: None,
        backstop_interval: Duration::from_secs(3600), // disarmed: passes are explicit
        memberships: None,
        trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(Vec::new()),
        attested_predecessors: Default::default(),
        linked_nests: None,
        owed_nests: None,
        peer_transport: None, // ⚠ no peer leg on this machine at all
        // The machine's named row (the RULED 2026-09-28 block, decision 3).
        enrollment_target_device_id: "cd".repeat(32),
    })
    .await
    .expect("the custodian's runtime assembles");

    let cal_scope = fauna_protocol::scope::ContentScope::new("calendar", owner().actor_id().0)
        .expect("the owner's own-actor calendar scope")
        .to_string();
    let card_scope = fauna_protocol::scope::ContentScope::new("card", owner().actor_id().0)
        .expect("the owner's own-actor card scope")
        .to_string();
    let custodied_dir = store_root
        .store_dir(&owner_hex)
        .expect("custodied store dir");

    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    loop {
        let report = handle.reconcile_now().await.expect("pass");
        let last = format!("{report:?}");
        if custodied_dir.exists() {
            let held = AccountStore::open(
                SqliteBackend::open(&custodied_dir).unwrap(),
                &owner_hex,
                WriterId(custodian_key().verifying_key().to_bytes()),
            )
            .await
            .unwrap();
            let cal_bytes = held.block(&cal_appended.record_cid).await.unwrap();
            let card_bytes = held.block(&card_appended.record_cid).await.unwrap();
            if let (Some(cal_bytes), Some(card_bytes)) = (cal_bytes, card_bytes) {
                // The cutover's property, asserted on the HELD bytes for both
                // kinds: the filing identity re-derives from the bytes
                // themselves — what `admit`'s anti-poisoning re-hash checked
                // on the way in, and the reason either kind is adoptable.
                assert_eq!(
                    fauna_cbor::Cid::of_dag_cbor(&cal_bytes),
                    cal_appended.record_cid,
                    "the held calendar block re-hashes to its filing identity"
                );
                assert_eq!(
                    fauna_cbor::Cid::of_dag_cbor(&card_bytes),
                    card_appended.record_cid,
                    "the held card block re-hashes to its filing identity"
                );
                let cal_adopted = held.adopted_segments(&cal_scope).await.unwrap();
                assert!(
                    cal_adopted
                        .iter()
                        .any(|k| k.segment_id == cal_appended.seg_id
                            && k.kind == "calendar"
                            && k.scope == cal_scope),
                    "the calendar bytes arrived by SEGMENT ADOPTION on the owner's \
                     calendar scope: {cal_adopted:?}"
                );
                let card_adopted = held.adopted_segments(&card_scope).await.unwrap();
                assert!(
                    card_adopted
                        .iter()
                        .any(|k| k.segment_id == card_appended.seg_id
                            && k.kind == "card"
                            && k.scope == card_scope),
                    "the card bytes arrived by SEGMENT ADOPTION on the owner's \
                     card scope: {card_adopted:?}"
                );
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the pump never adopted the owner's CALENDAR + CARD segments from the \
             owner's nest (the serve-plane gate was the last \
             missing piece); last report: {last}"
        );
        tokio::task::yield_now().await;
    }
}

/// **Row 70 — conv on the serve plane, under the member-mint rule**
/// (`message-segment-store.md` § *Which kinds the two planes serve*, ruled
/// 2026-08-18; the authorization rule: `account-data-plane.md` § The custody
/// grant + ceremony → *Shared-audience carve-out*): conv's segment scope is a
/// **channel**, so the serve request's actor field carries the scope id (the
/// channel hex), never an owner, and admission is the explicit-list row —
/// `content:conv:<channel_hex>` named at mint — whose owner is a **current
/// member** of the channel, re-derived per request.
///
/// Four assertions carry the ruling, in door order:
///
/// 1. **A send-minted routing row does NOT admit** — the pin: the
///    granting member holds an `actor_channels` row (exactly what
///    `channel.send`'s auto-register writes) and a live explicit-list custody
///    row, and the door still refuses, because the membership authority is the
///    floor roster and no such row exists yet.
/// 2. **The authoritative roster admits** — after the roster is seated through
///    the production report door (`fauna.conversations.room.roster_report`),
///    the direct door serves the channel plane (`fauna.segments.list { kind:
///    "conv", actor_id: <channel_hex> }` answers the segment).
/// 3. **The pump adopts** the sealed conv record appended through the
///    production conv append path — the same nest-anchored staging as the
///    mail/cal/card arms (no peer leg, no owner device), with the held block
///    re-hashing to its filing identity.
/// 4. **Leaving the channel severs serving** — after the remaining member's
///    next roster report no longer names the granting member (the production
///    departure path of an end-to-end room), the very next enumeration refuses
///    (`not_owner`), exactly as a revoke would: membership is re-derived on
///    every request, never cached in a session.
///    Reverting the membership check in `custody_admission` is what turns
///    arms 1 and 4 red (the member-mint rule's mutation witnesses).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_pump_adopts_a_channels_conv_segments_under_the_member_mint_rule() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    let (base, state, tmp) = start_door_nest().await;

    state
        .db
        .create_user_with_handle(&owner().actor_id().0, "free", "owner", None)
        .await
        .unwrap();
    let owner_nest = connected_client(&base, owner()).await;
    let owner_hex = owner().actor_id_hex();

    // The channel. The granting member gets ONLY the send-minted ROUTING row
    // for now — exactly what `channel.send`'s auto-register writes — which
    // must NOT admit (a row any sender can mint proves
    // knowledge of the channel id, not membership). The authoritative
    // floor-roster seat is added later, between assertions 1 and 2.
    let channel_id: [u8; 32] = [0xCE; 32];
    let channel_hex = hex::encode(channel_id);
    state
        .db
        .register_actor_channel(&owner().actor_id().0, &channel_id)
        .await
        .expect("mint the send-shaped routing-roster row");

    // One sealed conv record through the production append path (the same
    // encode mint both wire sides use — its CID is the filing identity the
    // custodian's admit re-hashes).
    let body = b"the sealed MLS envelope a custodian must hold without opening".to_vec();
    let (record_cid, _env_bytes) = fauna_mls::segments::encode_record(
        &fauna_mls::segments::ConvRecordEnvelope::new(body.clone()),
    )
    .expect("encode the conv record envelope");
    let appended = fauna_nest::segments::conv::append(
        &state.conv_segments,
        &state.db,
        &channel_id,
        &body,
        1_715_000_000_000,
    )
    .await
    .expect("append one sealed conv record on the channel's scope");
    state
        .conv_segments
        .finalize_open(&channel_id)
        .await
        .expect("finalize the open conv segment so the pair is adoptable");

    // ── The custody row: the EXPLICIT-LIST form naming exactly this channel —
    // the only door for a co-authored scope (the Account form refuses conv
    // loudly; that refusal is pinned by the content-walk case above). ────────
    let conv_scope = fauna_protocol::scope::ContentScope::new("conv", channel_id)
        .expect("the channel's conv content scope")
        .to_string();
    let named = CustodyScopeSet::Scopes(vec![conv_scope.clone()]);
    mint_custody_row(&owner_nest, &named, GRANT_ID).await;
    let witness = witness_for(named.clone());

    // (1) The pin: live explicit-list row + send-minted routing row,
    // and the door still refuses — the routing roster is not a membership
    // authority. The custody HANDSHAKE itself succeeds (witness + row + PoP;
    // the member-mint verdict is per-request at the door, never at mint).
    let custody_session = fauna_client::ws_custody_handshake_bearer::custody_nest_client(
        &base,
        owner().actor_id().0,
        custodian_key(),
        witness_for(named.clone()),
    );
    connect_custody(&custody_session)
        .await
        .expect("the custodian handshakes with no account credential");
    let err = segments_list(&custody_session, "conv", &channel_hex)
        .await
        .expect_err(
            "a channel.send-minted actor_channels row must NOT admit the conv \
             plane — membership authority is the FLOOR ROSTER",
        );
    assert_eq!(
        refusal_code(&err),
        "fauna.segments.not_owner",
        "the refusal is the door's uniform not_owner"
    );

    // (2) The authoritative roster admits: the roster is seated through the
    // PRODUCTION report door — `fauna.conversations.room.roster_report`, the
    // kind a member device calls after every membership commit it authors
    // (`conversation-rooms.md` § The floor roster → *End-to-end rooms*) — and
    // the very next enumeration serves. This is a **live room**, not a
    // Mechanism-B fixture: re-homing this case onto the room family is the
    // first code step of § The group plane's fate.
    //
    // A non-owner seat on purpose, twice over: it proves ANY current member
    // mints admission (the member-mint rule reads membership, not rank), and
    // it lets case (4) below drive a real membership departure.
    let channel_owner = ActorKeypair::generate();
    let report_roster = |members: Vec<(&'static str, [u8; 32])>| {
        fauna_protocol::conversations::RoomRosterReportRequest {
            room_id: channel_hex.clone(),
            members: members
                .into_iter()
                .map(
                    |(role, actor)| fauna_protocol::conversations::RoomRosterEntryWire {
                        actor: hex::encode(actor),
                        role: Some(role.into()),
                        extra: Default::default(),
                    },
                )
                .collect(),
            policy_version: Some(1),
            // Unordered, as a current client reports a departure: the departure
            // case below must keep landing through the report path's
            // `commit_seq: None` arm.
            commit_seq: None,
            extra: Default::default(),
        }
    };
    {
        use fauna_protocol::RpcRequester;
        let _: fauna_protocol::conversations::RoomRosterReportReply = owner_nest
            .request(
                "fauna.conversations.room.roster_report",
                report_roster(vec![
                    ("owner", channel_owner.actor_id().0),
                    ("member", owner().actor_id().0),
                ]),
            )
            .await
            .expect("the granting member reports the room's roster to its home nest");
    }
    let listed = segments_list(&custody_session, "conv", &channel_hex)
        .await
        .expect("a member-minted explicit-list row serves the channel's conv plane");
    assert!(
        listed
            .segments
            .iter()
            .any(|s| s.segment_id == appended.seg_id),
        "the door enumerates the appended conv segment: {:?}",
        listed.segments
    );

    // ── The custodian: a different account, no credential for the owner ───
    state
        .db
        .create_user_with_handle(&custodian_account().actor_id().0, "free", "custodian", None)
        .await
        .unwrap();
    let custodian_hex = custodian_account().actor_id_hex();
    let custodian_nest = connected_client(&base, custodian_account()).await;

    let state_base = tmp.path().join("custodian-state");
    let store_root = StoreRoot::at(state_base.clone());
    let creds_dir = tmp.path().join("custodian-creds");
    fauna_credential_store::cred_file_write(
        &creds_dir,
        CRED_NAMESPACE,
        &std::collections::BTreeMap::from([(custodian_hex.clone(), hex::encode(CUSTODIAN_SECRET))]),
    )
    .expect("seed the custodian's writer-key slot");
    let creds = CredentialStore::with_file_backend(CRED_NAMESPACE, creds_dir);

    {
        let own_dir = store_root.store_dir(&custodian_hex).expect("store dir");
        let own = AccountStore::open(
            SqliteBackend::open(&own_dir).unwrap(),
            &custodian_hex,
            WriterId(custodian_key().verifying_key().to_bytes()),
        )
        .await
        .unwrap();
        let kind = fauna_protocol::merge_policy::KIND_CUSTODIES_HELD;
        own.put_state(fauna_account_store::types::StateEntry {
            kind: kind.into(),
            key: fauna_core::custody_grant::custody_entry_key(&GRANT_ID),
            scope: fauna_protocol::merge_policy::home_scope_for_kind(kind)
                .unwrap()
                .into(),
            value: fauna_core::encoding::canonical_encode(
                &fauna_core::custodies_held::CustodyHeld {
                    grant_id: GRANT_ID.to_vec(),
                    owner: owner().actor_id().0,
                    witness: fauna_core::encoding::canonical_encode(&witness)
                        .unwrap()
                        .to_vec(),
                    // EMPTY on purpose — the peer leg has nothing to dial.
                    owner_devices: Vec::new(),
                    owner_nest_url: Some(base.clone()),
                    retained_bytes_cap: 8 * 1024 * 1024 * 1024,
                    ..Default::default()
                },
            )
            .unwrap()
            .to_vec(),
            merge_meta: None,
            entry_version: 0,
            tombstone: false,
        })
        .await
        .unwrap();
    }

    let handle = AccountStoreRuntime::start(AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        store_root: store_root.clone(),
        actor_id_hex: custodian_hex.clone(),
        rpc: Arc::clone(&custodian_nest),
        process_rpc: None,
        principal: RuntimePrincipal::SeedHolding(custodian_account().into()),
        credentials: creds,
        reconnects: None,
        pushes: None,
        backstop_interval: Duration::from_secs(3600), // disarmed: passes are explicit
        memberships: None,
        trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(Vec::new()),
        attested_predecessors: Default::default(),
        linked_nests: None,
        owed_nests: None,
        peer_transport: None, // ⚠ no peer leg on this machine at all
        // The machine's named row (the RULED 2026-09-28 block, decision 3).
        enrollment_target_device_id: "cd".repeat(32),
    })
    .await
    .expect("the custodian's runtime assembles");

    let custodied_dir = store_root
        .store_dir(&owner_hex)
        .expect("custodied store dir");

    // (3) The pump adopts the channel's conv segment end to end.
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    loop {
        let report = handle.reconcile_now().await.expect("pass");
        let last = format!("{report:?}");
        if custodied_dir.exists() {
            let held = AccountStore::open(
                SqliteBackend::open(&custodied_dir).unwrap(),
                &owner_hex,
                WriterId(custodian_key().verifying_key().to_bytes()),
            )
            .await
            .unwrap();
            if let Some(held_bytes) = held.block(&record_cid).await.unwrap() {
                // The cutover's property on the HELD bytes: the filing identity
                // re-derives from the bytes themselves — what `admit`'s
                // anti-poisoning re-hash checked on the way in.
                assert_eq!(
                    fauna_cbor::Cid::of_dag_cbor(&held_bytes),
                    record_cid,
                    "the held conv block re-hashes to its filing identity"
                );
                let adopted = held.adopted_segments(&conv_scope).await.unwrap();
                assert!(
                    adopted.iter().any(|k| k.segment_id == appended.seg_id
                        && k.kind == "conv"
                        && k.scope == conv_scope),
                    "the conv bytes arrived by SEGMENT ADOPTION on the channel's \
                     conv scope: {adopted:?}"
                );
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the pump never adopted the channel's CONV segments from the owner's \
             nest (conv was admissible and unoffered); \
             last report: {last}"
        );
        tokio::task::yield_now().await;
    }

    // (4) Leaving the room severs serving at the very next request —
    // membership is re-derived per request, exactly as the live-row re-check
    // re-derives the grant. The leave crosses the PRODUCTION door of the room
    // family: in an end-to-end room a departure is an MLS commit the nest
    // cannot see, so what reaches the nest is the committing device's next
    // roster report, which simply no longer names the leaver
    // (before a per-channel severance existed at all,
    // the only one was the account-deletion cascade). The routing row
    // deliberately stays, and must not resurrect admission.
    {
        use fauna_protocol::RpcRequester;
        let _: fauna_protocol::conversations::RoomRosterReportReply = owner_nest
            .request(
                "fauna.conversations.room.roster_report",
                report_roster(vec![("owner", channel_owner.actor_id().0)]),
            )
            .await
            .expect("the remaining member reports the post-departure roster");
    }
    assert!(
        !state
            .db
            .is_room_member(&channel_id, &owner().actor_id().0)
            .await
            .expect("floor roster read"),
        "the report dropped exactly the leaver from the live floor roster"
    );
    let err = segments_list(&custody_session, "conv", &channel_hex)
        .await
        .expect_err(
            "the granting member left the channel, so the member-mint rule must \
             refuse the very next enumeration",
        );
    assert_eq!(
        refusal_code(&err),
        "fauna.segments.not_owner",
        "the refusal is the door's uniform not_owner — indistinguishable from a \
         stranger's"
    );
}
