//! **The custody-hosting registration doors, end to end over the real nest**
//! (tier_3; the custodian-nest runtime's stage (b) —
//! `docs/goal/architecture/account-data-plane.md` § Replica posture → *The
//! custody grant + ceremony*, the device-or-nest bullet, item 6): the HOST
//! user deposits the keyless custody-hosting row on its own nest over the
//! production `CustodyHostingClient`, rewrites it (the stop control and the
//! budget-adjust are the same verb), and reads it back — and the register
//! door refuses every row the pump could never use (a witness that does not
//! admit THIS nest, a wrong owner, a policy-violating URL, a grant-id that is
//! not the witness's own) rather than letting it rest and fail on the first
//! pull pass.
//!
//! Restart survival is proven at the store layer with a file-backed
//! `CacheDb` — the same table, the same SQL, no serving nest required.
//!
//! Every assertion is on latency-independent state (e2e convention 14).

mod common;
use common::connected_client;

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_client_capabilities::custody_hosting::{CustodyHostingClient, HostingDeposit};
use fauna_core::custody_grant::{
    CUSTODY_GRANT_ID_LEN, CustodyGrant, CustodyScopeSet, sign_custody_grant,
};
use fauna_core::data::Timestamp;
use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;

const OWNER_SEED: [u8; 32] = [0x6A; 32];
const HOST_SEED: [u8; 32] = [0x6B; 32];
const OTHER_HOST_SEED: [u8; 32] = [0x6C; 32];
const GRANT_ID: [u8; CUSTODY_GRANT_ID_LEN] = [0x2E; CUSTODY_GRANT_ID_LEN];

fn owner() -> ActorKeypair {
    ActorKeypair::from_secret(OWNER_SEED)
}

fn host() -> ActorKeypair {
    ActorKeypair::from_secret(HOST_SEED)
}

async fn start_hosting_nest() -> (String, Arc<AppState>, tempfile::TempDir) {
    start_hosting_nest_with_identity(None).await
}

/// `identity_domain` overrides what the nest believes its own identity domain is
/// — the deployment fact the nest-scoped dial policy reads through
/// `AppState::is_public_deployment`. `None` leaves the loopback
/// authority, i.e. a local deployment; `Some("nest.example.com")` makes the same
/// rig believe it is publicly reachable.
async fn start_hosting_nest_with_identity(
    identity_domain: Option<&str>,
) -> (String, Arc<AppState>, tempfile::TempDir) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let authority = format!("127.0.0.1:{}", addr.port());
    let handle_domain = identity_domain.map_or_else(|| authority.clone(), str::to_string);

    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, tmp.path().to_path_buf(), None).unwrap(),
    );

    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity::from_seed(&secret)),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::custody_hosting_handlers::register_custody_hosting_handlers(&mut b);
            b.build()
        }),
        auth: AuthState {
            token_store: Arc::new(TokenStore::new()),
            registration: RegistrationConfig {
                handle_domain: Some(handle_domain),
                ..Default::default()
            },
            ..Default::default()
        },
        enforce_tier_quotas: Arc::new(tokio::sync::RwLock::new(true)),
        backup_service: Some(backup_svc),
        // The admin remove door's store-teardown root.
        custody_hosting_root: Some(tmp.path().join("custody-hosting")),
        ..AppState::for_test(db)
    });

    let app = fauna_nest::build_router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{authority}"), state, tmp)
}

/// A witness the OWNER signs, naming `custodian` as the bound principal —
/// for the happy path that is the host NEST's identity key.
fn witness_naming(custodian: [u8; 32], expires_at: Timestamp) -> Vec<u8> {
    witness_for_grant(&GRANT_ID, custodian, expires_at)
}

/// [`witness_naming`] for an arbitrary ceremony id — the register door requires
/// the row key to be the witness's own grant, so a test depositing several rows
/// needs a matching witness per row.
fn witness_for_grant(grant_id: &[u8], custodian: [u8; 32], expires_at: Timestamp) -> Vec<u8> {
    let embed = sign_custody_grant(
        &owner(),
        &CustodyGrant {
            grant_id: grant_id.to_vec(),
            owner: owner().actor_id(),
            custodian_key: custodian,
            scopes: CustodyScopeSet::Account,
            minted_at: Timestamp(1_000),
            expires_at,
            removed_devices: Vec::new(),
        },
    )
    .expect("sign witness");
    fauna_core::encoding::canonical_encode(&embed).expect("encode witness")
}

const FAR_FUTURE: Timestamp = Timestamp(9_000_000_000_000_000);

fn valid_deposit(nest_key: [u8; 32]) -> HostingDeposit {
    HostingDeposit {
        grant_id: GRANT_ID.to_vec(),
        owner: owner().actor_id().0,
        witness: witness_naming(nest_key, FAR_FUTURE),
        owner_nest_url: "http://127.0.0.1:9".into(),
        owner_devices: Vec::new(),
        retained_bytes_cap: 4096,
        stopped: false,
    }
}

#[tokio::test]
async fn a_host_deposits_rewrites_and_reads_back_its_hosting_row() {
    let (base, state, _tmp) = start_hosting_nest().await;
    let nest_key = state.nest_identity.public_key_bytes();
    state
        .db
        .create_user_with_handle(&host().actor_id().0, "free", "host", None)
        .await
        .unwrap();
    let client = CustodyHostingClient::new(connected_client(&base, host()).await);

    let deposit = valid_deposit(nest_key);
    let reply = client.register(&deposit).await.expect("register RPC");
    assert!(reply.ok, "the hosting row deposited");

    let rows = client.list().await.expect("list RPC").rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].grant_id.as_slice(), GRANT_ID.as_slice());
    assert_eq!(rows[0].owner_actor_id, owner().actor_id_hex());
    assert_eq!(rows[0].owner_nest_url, "http://127.0.0.1:9");
    assert_eq!(rows[0].retained_bytes_cap, 4096);
    assert!(!rows[0].stopped);
    assert_eq!(rows[0].held_bytes, 0, "no pull pass yet");
    assert_eq!(rows[0].last_receipt_at, 0, "no receipt yet");

    // The stop control and the budget-adjust are the SAME verb: a rewrite
    // replaces in place, never duplicates.
    let rewrite = HostingDeposit {
        retained_bytes_cap: 1024,
        stopped: true,
        ..deposit
    };
    assert!(client.register(&rewrite).await.expect("rewrite RPC").ok);
    let rows = client.list().await.expect("list RPC").rows;
    assert_eq!(rows.len(), 1, "rewrite replaced in place");
    assert!(rows[0].stopped);
    assert_eq!(rows[0].retained_bytes_cap, 1024);
}

#[tokio::test]
async fn the_register_door_refuses_every_row_the_pump_could_never_use() {
    let (base, state, _tmp) = start_hosting_nest().await;
    let nest_key = state.nest_identity.public_key_bytes();
    state
        .db
        .create_user_with_handle(&host().actor_id().0, "free", "host", None)
        .await
        .unwrap();
    let client = CustodyHostingClient::new(connected_client(&base, host()).await);

    // (a) A witness binding a DIFFERENT principal — a device key, another
    // nest — could never pass the owner-side custody handshake when THIS
    // nest dials, so the row is junk at deposit time.
    let stranger_device = SigningKey::from_bytes(&[0xD7; 32])
        .verifying_key()
        .to_bytes();
    let mut d = valid_deposit(nest_key);
    d.witness = witness_naming(stranger_device, FAR_FUTURE);
    assert!(
        client.register(&d).await.is_err(),
        "a witness naming a foreign principal is refused"
    );

    // (b) The request's owner must be the witness's own signer-account.
    let mut d = valid_deposit(nest_key);
    d.owner = ActorKeypair::from_secret(OTHER_HOST_SEED).actor_id().0;
    assert!(
        client.register(&d).await.is_err(),
        "an owner mismatch with the witness is refused"
    );

    // (c) The counterparty dial policy holds at ingest: plaintext http is
    // loopback-only, and origin-only means no path.
    for bad_url in ["http://203.0.113.7:443", "https://owner.example/path"] {
        let mut d = valid_deposit(nest_key);
        d.owner_nest_url = bad_url.into();
        assert!(
            client.register(&d).await.is_err(),
            "policy-violating URL {bad_url} is refused"
        );
    }

    // (d) The row key must be the witness's own ceremony.
    let mut d = valid_deposit(nest_key);
    d.grant_id = vec![0x99; CUSTODY_GRANT_ID_LEN];
    assert!(
        client.register(&d).await.is_err(),
        "a grant_id that is not the witness's own is refused"
    );

    // (e) An expired witness is a ceremony the pump could never redeem.
    let mut d = valid_deposit(nest_key);
    d.witness = witness_naming(nest_key, Timestamp(1_000));
    assert!(
        client.register(&d).await.is_err(),
        "an expired witness is refused"
    );

    // None of the refusals rested a row.
    assert!(
        client.list().await.expect("list RPC").rows.is_empty(),
        "every refusal left the registry empty"
    );
}

/// **The store-capacity probe now runs as a standing test.** The probe
/// deposited 256 rows, each with `retained_bytes_cap: u64::MAX`, and the store
/// accepted every one: the door is `User` class, its only gate is a witness the
/// caller mints entirely themselves, and neither the row count nor the budget
/// had a ceiling. So an ordinary account holder could make the nest dial an
/// address of their choosing every 15 minutes, without bound, and hold whatever
/// it served.
///
/// Both bounds are hard-coded constants (bucket 1 — no human would choose
/// either), and they behave differently on purpose: rows **refuse** (a caller
/// error worth surfacing) and bytes **clamp** (a legitimate host is never locked
/// out by a number their own app suggested).
#[tokio::test]
async fn the_register_door_bounds_a_hostile_accounts_deposits() {
    let (base, state, _tmp) = start_hosting_nest().await;
    let nest_key = state.nest_identity.public_key_bytes();
    state
        .db
        .create_user_with_handle(&host().actor_id().0, "free", "host", None)
        .await
        .unwrap();
    let client = CustodyHostingClient::new(connected_client(&base, host()).await);

    let cap = fauna_core::custody_ceremony::MAX_CUSTODY_HOSTING_ROWS_PER_HOST;
    let grant_of = |i: usize| vec![i as u8; CUSTODY_GRANT_ID_LEN];
    let hostile = |i: usize| HostingDeposit {
        grant_id: grant_of(i),
        witness: witness_for_grant(&grant_of(i), nest_key, FAR_FUTURE),
        // The attacker's own number — the probe's exact value.
        retained_bytes_cap: u64::MAX,
        ..valid_deposit(nest_key)
    };

    // Up to the cap the rows land, and every budget is clamped to the ceiling
    // rather than honoured as given.
    for i in 0..cap {
        assert!(
            client.register(&hostile(i)).await.expect("register RPC").ok,
            "row {i} is within the cap and must land"
        );
    }
    let rows = client.list().await.expect("list RPC").rows;
    assert_eq!(rows.len(), cap, "exactly the cap rests");
    for row in &rows {
        assert_eq!(
            row.retained_bytes_cap,
            fauna_core::custody_ceremony::MAX_RETAINED_BYTES_CAP,
            "u64::MAX was clamped to the ceiling, never written through"
        );
    }

    // Past the cap a NEW row is refused, and the refusal rests nothing.
    assert!(
        client.register(&hostile(cap)).await.is_err(),
        "the {}th row is refused — the cap is the bound, not a suggestion",
        cap + 1
    );
    assert_eq!(
        client.list().await.expect("list RPC").rows.len(),
        cap,
        "the refused deposit rested no row"
    );

    // ...but a REWRITE of a row the host already holds is still admitted at the
    // cap. Otherwise a host sitting at the cap could never stop one of its own
    // rows, and the cap itself would become unrecoverable state.
    let stop = HostingDeposit {
        stopped: true,
        ..hostile(0)
    };
    assert!(
        client.register(&stop).await.expect("rewrite RPC").ok,
        "a host at the cap can still stop a row it already holds"
    );
    let rows = client.list().await.expect("list RPC").rows;
    assert_eq!(rows.len(), cap, "the rewrite replaced in place");
    assert!(
        rows.iter()
            .any(|r| r.grant_id.as_slice() == grant_of(0).as_slice() && r.stopped),
        "the stop took effect on the row it named"
    );
}

/// **The door's learn-early refusal.**
/// Held custody bytes are a DERIVED per-host figure
/// (`SUM(custody_hosting.held_bytes)`, metered by the pump every pass) bounded
/// by the host's tier `max_storage_bytes` — deliberately never charged into
/// `users.storage_bytes_used`, where the sync plane would start refusing the
/// host's OWN file writes (the cross-plane wedge the re-ruling withdrew). A
/// host already at its bound is told so at the door rather than depositing a
/// row the pump would immediately squeeze to nothing; a REWRITE stays admitted
/// for the same reason it is exempt from the row cap — stop and re-budget are
/// this verb, and a host over bound must be able to shrink.
#[tokio::test]
async fn the_register_door_refuses_a_new_row_for_a_host_over_its_tier_bound() {
    let (base, state, _tmp) = start_hosting_nest().await;
    let nest_key = state.nest_identity.public_key_bytes();
    state
        .db
        .create_user_with_handle(&host().actor_id().0, "free", "host", None)
        .await
        .unwrap();

    // Squeeze the host's tier down to a bound one metered row can exceed.
    // (`enforce_tier_quotas` is already on — this rig's default.)
    let mut tier = state
        .db
        .get_tier("free")
        .await
        .unwrap()
        .expect("the migrations seed the free tier");
    tier.max_storage_bytes = 1_000;
    assert!(state.db.update_tier(&tier).await.unwrap());

    let client = CustodyHostingClient::new(connected_client(&base, host()).await);

    // Under the bound the deposit lands...
    assert!(
        client
            .register(&valid_deposit(nest_key))
            .await
            .expect("register RPC")
            .ok
    );

    // ...then the pump's metering carries it over the tier bound.
    state
        .db
        .update_custody_hosting_metering(&host().actor_id().0, &GRANT_ID, 5_000)
        .await
        .unwrap();

    // A NEW row is now refused, and the refusal names the bound so the host
    // learns at the door, not from a row that silently holds nothing.
    let second_grant = vec![0x3F; CUSTODY_GRANT_ID_LEN];
    let over = HostingDeposit {
        grant_id: second_grant.clone(),
        witness: witness_for_grant(&second_grant, nest_key, FAR_FUTURE),
        ..valid_deposit(nest_key)
    };
    let err = client.register(&over).await.unwrap_err();
    let rpc = fauna_protocol::RpcErrorClass::as_rpc_error(&err)
        .expect("a wire-level refusal, not a transport fault");
    // The naming rides in the wire error's `details` — Display drops details
    // by contract (`NestClientError`'s `display_never_includes_details`), and
    // the app-rendered verdict is the shared client-side rule instead.
    let details = format!("{:?}", rpc.details);
    assert!(
        details.contains("storage bound"),
        "the refusal must name the bound, got: {details}"
    );
    assert_eq!(
        client.list().await.expect("list RPC").rows.len(),
        1,
        "the refused deposit rested no row"
    );

    // A REWRITE of the row the host already holds is still admitted — an
    // over-bound host must be able to stop or shrink its way back under.
    let stop = HostingDeposit {
        retained_bytes_cap: 1024,
        stopped: true,
        ..valid_deposit(nest_key)
    };
    assert!(
        client.register(&stop).await.expect("rewrite RPC").ok,
        "a host over its bound can still stop a row it already holds"
    );
    let rows = client.list().await.expect("list RPC").rows;
    assert_eq!(rows.len(), 1, "the rewrite replaced in place");
    assert!(rows[0].stopped, "the stop took effect");
    assert_eq!(rows[0].retained_bytes_cap, 1024);
}

/// **The dial half.** The plaintext-loopback carve-out was an
/// acceptance about a *device's* own loopback; the nest leg inherited
/// it verbatim, so an account holder could aim a hosting row at the nest's own
/// loopback ports. On a public deployment the carve-out is withdrawn — and only
/// there, which is what keeps every local/tier_3 rig (this file's own default)
/// working with no runtime knob.
#[tokio::test]
async fn a_public_nest_refuses_a_loopback_hosting_url() {
    let (base, state, _tmp) = start_hosting_nest_with_identity(Some("nest.example.com")).await;
    let nest_key = state.nest_identity.public_key_bytes();
    assert!(
        state.is_public_deployment(),
        "the rig must believe it is publicly reachable, or this proves nothing"
    );
    state
        .db
        .create_user_with_handle(&host().actor_id().0, "free", "host", None)
        .await
        .unwrap();
    let client = CustodyHostingClient::new(connected_client(&base, host()).await);

    // `valid_deposit`'s URL is `http://127.0.0.1:9` — admitted on a local nest
    // by the same rig, refused here.
    assert!(
        client.register(&valid_deposit(nest_key)).await.is_err(),
        "a public nest must not be made to knock at its own loopback"
    );
    assert!(
        client.list().await.expect("list RPC").rows.is_empty(),
        "the refusal rested no row"
    );

    // A TLS origin is unaffected — the withdrawal is the plaintext clause only.
    let tls = HostingDeposit {
        owner_nest_url: "https://owner.example".into(),
        ..valid_deposit(nest_key)
    };
    assert!(
        client.register(&tls).await.expect("register RPC").ok,
        "https anywhere is one ruling and must not have forked"
    );
}

#[tokio::test]
async fn hosting_rows_are_host_scoped() {
    let (base, state, _tmp) = start_hosting_nest().await;
    let nest_key = state.nest_identity.public_key_bytes();
    state
        .db
        .create_user_with_handle(&host().actor_id().0, "free", "host", None)
        .await
        .unwrap();
    let other = ActorKeypair::from_secret(OTHER_HOST_SEED);
    state
        .db
        .create_user_with_handle(&other.actor_id().0, "free", "otherhost", None)
        .await
        .unwrap();

    let host_a = CustodyHostingClient::new(connected_client(&base, host()).await);
    let host_b = CustodyHostingClient::new(connected_client(&base, other).await);

    assert!(
        host_a
            .register(&valid_deposit(nest_key))
            .await
            .expect("register")
            .ok
    );
    assert_eq!(host_a.list().await.expect("list").rows.len(), 1);
    assert!(
        host_b.list().await.expect("list").rows.is_empty(),
        "a caller only ever sees its own rows"
    );
}

/// Restart survival at the store layer: the row rests in the file-backed DB
/// and a fresh open (the boot path's `CacheDb::open`) reads it back intact.
#[tokio::test]
async fn a_hosting_row_survives_a_nest_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("nest.db");
    let host_id = host().actor_id().0;
    let owner_id = owner().actor_id().0;

    {
        let db = CacheDb::open(&path).unwrap();
        db.put_custody_hosting(
            &host_id,
            &GRANT_ID,
            &owner_id,
            b"witness-bytes",
            "http://127.0.0.1:9",
            b"",
            4096,
            false,
        )
        .await
        .unwrap();
        db.update_custody_hosting_metering(&host_id, &GRANT_ID, 555)
            .await
            .unwrap();
        db.record_custody_hosting_receipt(&host_id, &GRANT_ID, 666, true)
            .await
            .unwrap();
    }

    let db = CacheDb::open(&path).unwrap();
    let rows = db.list_custody_hosting(&host_id).await.unwrap();
    assert_eq!(rows.len(), 1, "the row survived the restart");
    assert_eq!(rows[0].owner_actor_id, owner_id.to_vec());
    assert_eq!(rows[0].held_bytes, 555, "the pump's metering survived too");
    assert_eq!(rows[0].last_receipt_at, 666);
    assert!(rows[0].last_receipt_degraded, "receipt state survived too");
}

// ── The admin surface ─────────────────────────────────

const ADMIN_SEED: [u8; 32] = [0x6D; 32];

/// **The recoverability half.** The register door is
/// User-class, so any account holder can plant hosting rows; without an admin
/// surface the only remedy for a disk filled that way was `sqlite3` on
/// `nest.db` plus `rm -rf` — off-box-only-fixable state, a bug by the
/// client-state-recoverability rule. This proves an admin can SEE the whole
/// registry (nest-wide, host-attributed — the per-caller list deliberately is
/// not) and DROP a row with its custodied store, while a plain user is
/// refused both doors.
#[tokio::test]
async fn an_admin_lists_nest_wide_and_removes_a_row_with_its_store() {
    use fauna_protocol::RpcRequester;
    use fauna_protocol::custody::{
        ADMIN_HOSTING_LIST_KIND, ADMIN_HOSTING_REMOVE_KIND, AdminHostingListReply,
        AdminHostingListRequest, AdminHostingRemoveReply, AdminHostingRemoveRequest,
    };

    let (base, state, tmp) = start_hosting_nest().await;
    let nest_key = state.nest_identity.public_key_bytes();

    // Two hosts, one row each.
    let host_a = host();
    let host_b = ActorKeypair::from_secret(OTHER_HOST_SEED);
    for (kp, handle) in [(&host_a, "host"), (&host_b, "otherhost")] {
        state
            .db
            .create_user_with_handle(&kp.actor_id().0, "free", handle, None)
            .await
            .unwrap();
    }
    let client_a = CustodyHostingClient::new(connected_client(&base, host()).await);
    let client_b = CustodyHostingClient::new(
        connected_client(&base, ActorKeypair::from_secret(OTHER_HOST_SEED)).await,
    );
    assert!(
        client_a
            .register(&valid_deposit(nest_key))
            .await
            .unwrap()
            .ok
    );
    let b_grant = vec![0x44; CUSTODY_GRANT_ID_LEN];
    let b_deposit = HostingDeposit {
        grant_id: b_grant.clone(),
        witness: witness_for_grant(&b_grant, nest_key, FAR_FUTURE),
        ..valid_deposit(nest_key)
    };
    assert!(client_b.register(&b_deposit).await.unwrap().ok);

    // The admin: a user with the extra role.
    let admin = ActorKeypair::from_secret(ADMIN_SEED);
    state
        .db
        .create_user_with_handle(&admin.actor_id().0, "free", "admin", None)
        .await
        .unwrap();
    state
        .db
        .add_admin_actor(&admin.actor_id().0[..])
        .await
        .unwrap();
    let admin_conn = connected_client(&base, admin).await;

    // Nest-wide, host-attributed list.
    let listed: AdminHostingListReply = admin_conn
        .request(ADMIN_HOSTING_LIST_KIND, AdminHostingListRequest::default())
        .await
        .expect("admin list");
    assert_eq!(
        listed.rows.len(),
        2,
        "the ADMIN list sees every host's rows"
    );
    let hosts: Vec<&str> = listed
        .rows
        .iter()
        .map(|r| r.host_actor_id.as_str())
        .collect();
    assert!(hosts.contains(&host_a.actor_id_hex().as_str()));
    assert!(hosts.contains(&host_b.actor_id_hex().as_str()));

    // A plain user is refused BOTH admin doors.
    let user_conn = connected_client(&base, host()).await;
    assert!(
        user_conn
            .request::<_, AdminHostingListReply>(
                ADMIN_HOSTING_LIST_KIND,
                AdminHostingListRequest::default()
            )
            .await
            .is_err(),
        "a plain user must not reach the nest-wide list"
    );
    assert!(
        user_conn
            .request::<_, AdminHostingRemoveReply>(
                ADMIN_HOSTING_REMOVE_KIND,
                AdminHostingRemoveRequest {
                    host_actor_id: host_b.actor_id_hex(),
                    grant_id: serde_bytes::ByteBuf::from(b_grant.clone()),
                    extra: Default::default(),
                }
            )
            .await
            .is_err(),
        "a plain user must not drop another host's rows"
    );

    // Give host A's row a custodied store on disk, as the pump would.
    let store_dir = tmp
        .path()
        .join("custody-hosting")
        .join(host_a.actor_id_hex())
        .join(owner().actor_id_hex());
    std::fs::create_dir_all(&store_dir).unwrap();
    std::fs::write(store_dir.join("held.bytes"), b"custodied content").unwrap();

    // The admin drops host A's row: row gone, store gone, host B untouched.
    let removed: AdminHostingRemoveReply = admin_conn
        .request(
            ADMIN_HOSTING_REMOVE_KIND,
            AdminHostingRemoveRequest {
                host_actor_id: host_a.actor_id_hex(),
                grant_id: serde_bytes::ByteBuf::from(GRANT_ID.to_vec()),
                extra: Default::default(),
            },
        )
        .await
        .expect("admin remove");
    assert!(removed.removed, "the row existed and was dropped");
    assert!(
        removed.store_dropped,
        "the pair's last row takes the store with it"
    );
    assert!(
        !store_dir.exists(),
        "the custodied bytes are gone from disk"
    );

    let listed: AdminHostingListReply = admin_conn
        .request(ADMIN_HOSTING_LIST_KIND, AdminHostingListRequest::default())
        .await
        .expect("admin list after remove");
    assert_eq!(listed.rows.len(), 1, "host B's row stands");
    assert_eq!(listed.rows[0].host_actor_id, host_b.actor_id_hex());

    // Removing an already-gone row is an honest no-op, never an error.
    let removed: AdminHostingRemoveReply = admin_conn
        .request(
            ADMIN_HOSTING_REMOVE_KIND,
            AdminHostingRemoveRequest {
                host_actor_id: host_a.actor_id_hex(),
                grant_id: serde_bytes::ByteBuf::from(GRANT_ID.to_vec()),
                extra: Default::default(),
            },
        )
        .await
        .expect("admin re-remove");
    assert!(!removed.removed);
    assert!(!removed.store_dropped);
}

/// The store-teardown gate: the custodied store dir is keyed `(host, owner)`
/// and shared by every grant of the pair, so it falls only with the pair's
/// LAST row — dropping one of two grants must leave the other's bytes.
#[tokio::test]
async fn the_store_falls_only_with_the_pairs_last_row() {
    use fauna_protocol::RpcRequester;
    use fauna_protocol::custody::{
        ADMIN_HOSTING_REMOVE_KIND, AdminHostingRemoveReply, AdminHostingRemoveRequest,
    };

    let (base, state, tmp) = start_hosting_nest().await;
    let nest_key = state.nest_identity.public_key_bytes();
    state
        .db
        .create_user_with_handle(&host().actor_id().0, "free", "host", None)
        .await
        .unwrap();
    let client = CustodyHostingClient::new(connected_client(&base, host()).await);

    // Two grants, same host, same owner.
    let second_grant = vec![0x45; CUSTODY_GRANT_ID_LEN];
    assert!(client.register(&valid_deposit(nest_key)).await.unwrap().ok);
    assert!(
        client
            .register(&HostingDeposit {
                grant_id: second_grant.clone(),
                witness: witness_for_grant(&second_grant, nest_key, FAR_FUTURE),
                ..valid_deposit(nest_key)
            })
            .await
            .unwrap()
            .ok
    );

    let admin = ActorKeypair::from_secret(ADMIN_SEED);
    state
        .db
        .create_user_with_handle(&admin.actor_id().0, "free", "admin", None)
        .await
        .unwrap();
    state
        .db
        .add_admin_actor(&admin.actor_id().0[..])
        .await
        .unwrap();
    let admin_conn = connected_client(&base, admin).await;

    let store_dir = tmp
        .path()
        .join("custody-hosting")
        .join(host().actor_id_hex())
        .join(owner().actor_id_hex());
    std::fs::create_dir_all(&store_dir).unwrap();
    std::fs::write(store_dir.join("held.bytes"), b"custodied content").unwrap();

    let remove = |grant: Vec<u8>| {
        let conn = Arc::clone(&admin_conn);
        async move {
            conn.request::<_, AdminHostingRemoveReply>(
                ADMIN_HOSTING_REMOVE_KIND,
                AdminHostingRemoveRequest {
                    host_actor_id: host().actor_id_hex(),
                    grant_id: serde_bytes::ByteBuf::from(grant),
                    extra: Default::default(),
                },
            )
            .await
            .expect("admin remove")
        }
    };

    let first = remove(GRANT_ID.to_vec()).await;
    assert!(first.removed);
    assert!(
        !first.store_dropped,
        "the second grant still reads this store"
    );
    assert!(
        store_dir.exists(),
        "the shared store survived the first drop"
    );

    let second = remove(second_grant).await;
    assert!(second.removed);
    assert!(second.store_dropped, "the pair's LAST row takes the store");
    assert!(!store_dir.exists());
}

/// The reclaim door, end to end: the HOST removes its own row over the
/// production client — stop pauses, THIS frees — and the custodied store
/// falls only with the `(host, owner)` pair's last row, exactly the admin
/// door's teardown (one shared implementation).
#[tokio::test]
async fn a_host_removes_its_own_row_and_the_pairs_last_row_takes_the_store() {
    let (base, state, tmp) = start_hosting_nest().await;
    let nest_key = state.nest_identity.public_key_bytes();
    state
        .db
        .create_user_with_handle(&host().actor_id().0, "free", "host", None)
        .await
        .unwrap();
    let client = CustodyHostingClient::new(connected_client(&base, host()).await);

    // Two grants, same host, same owner — the pair shares one store dir.
    let second_grant = vec![0x46; CUSTODY_GRANT_ID_LEN];
    assert!(client.register(&valid_deposit(nest_key)).await.unwrap().ok);
    assert!(
        client
            .register(&HostingDeposit {
                grant_id: second_grant.clone(),
                witness: witness_for_grant(&second_grant, nest_key, FAR_FUTURE),
                ..valid_deposit(nest_key)
            })
            .await
            .unwrap()
            .ok
    );
    let store_dir = tmp
        .path()
        .join("custody-hosting")
        .join(host().actor_id_hex())
        .join(owner().actor_id_hex());
    std::fs::create_dir_all(&store_dir).unwrap();
    std::fs::write(store_dir.join("held.bytes"), b"custodied content").unwrap();

    let first = client.remove(&GRANT_ID).await.expect("host removes");
    assert!(first.removed, "the host's own row was dropped");
    assert!(
        !first.store_dropped,
        "the second grant still reads this store"
    );
    assert!(
        store_dir.exists(),
        "the shared store survived the first drop"
    );

    let second = client.remove(&second_grant).await.expect("host removes");
    assert!(second.removed);
    assert!(second.store_dropped, "the pair's LAST row takes the store");
    assert!(!store_dir.exists(), "the custodied bytes are reclaimed");

    // Removing an already-gone row is an honest no-op, never an error.
    let again = client.remove(&GRANT_ID).await.expect("re-remove");
    assert!(!again.removed);
    assert!(!again.store_dropped);

    // The registry read-back agrees: nothing left.
    assert!(client.list().await.unwrap().rows.is_empty());
}

/// The remove door is host-scoped like its siblings: a grant id the caller
/// does not host answers `removed: false` and touches nothing — cross-host
/// deletion is unrepresentable because the row key is derived from the
/// authenticated connection, never a wire parameter.
#[tokio::test]
async fn the_remove_door_cannot_reach_another_hosts_row() {
    let (base, state, _tmp) = start_hosting_nest().await;
    let nest_key = state.nest_identity.public_key_bytes();
    for (kp, handle) in [
        (host(), "host"),
        (ActorKeypair::from_secret(OTHER_HOST_SEED), "otherhost"),
    ] {
        state
            .db
            .create_user_with_handle(&kp.actor_id().0, "free", handle, None)
            .await
            .unwrap();
    }
    // Host A registers; host B tries to remove A's grant.
    let client_a = CustodyHostingClient::new(connected_client(&base, host()).await);
    assert!(
        client_a
            .register(&valid_deposit(nest_key))
            .await
            .unwrap()
            .ok
    );
    let client_b = CustodyHostingClient::new(
        connected_client(&base, ActorKeypair::from_secret(OTHER_HOST_SEED)).await,
    );
    let attempt = client_b.remove(&GRANT_ID).await.expect("door answers");
    assert!(
        !attempt.removed,
        "another host's grant id must not match this caller's rows"
    );
    // A's row stands, listed under A.
    let rows = client_a.list().await.unwrap().rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].grant_id.as_slice(), GRANT_ID.as_slice());
}
