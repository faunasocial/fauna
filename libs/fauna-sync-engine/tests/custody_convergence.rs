//! **W8.5 (account-data-plane.md § Workstreams)'s success core** (`account-data-plane.md` § Replica posture → the
//! custody build;):
//! owner device A seals rows → custodian C — a DIFFERENT account's runtime,
//! keyless for the owner — pulls them from A under the REAL custody witness
//! → owner device B pulls them from C → B's plane converges, all three
//! runtimes driven entirely through their own pumps with **no nest
//! anywhere**.
//!
//! What each leg proves, production code end to end:
//!
//! - C's pump opens the custodied store from its `custodies-held` row,
//!   dials A's fleet candidates, admits with the custody grant, and
//!   `custody_pull`s the sealed rows verbatim (no schedule, no merge — the
//!   structural assertions below pin the keylessness).
//! - C's node SERVES the custodied account: B — holding only a
//!   `custodian-endpoints` row, deliberately **no** sibling row for A —
//!   dials C, admits with its own `DeviceAuthorization` (C answers with THE
//!   CUSTODY GRANT, the per-account reply witness), and its ordinary
//!   pull-only walk opens the owner-sealed rows C never could.
//! - Revoking the grant owner-side (the W8.2 verb order's end state,
//!   written into the account store the runtime's snapshot refresh
//!   reads) severs C at A's next admission evaluation —
//!   the live half of the tier_1 predicate pins.
//! - The shared-audience carve-out holds ON THE WIRE: a conv-scope
//!   changes.list under an `Account`-form custody verdict is refused while
//!   the state scope serves.
//!
//! Latency-independent throughout (convention 14): explicit passes, named
//! generous budgets, the listener barrier is `fauna_transport::testing`'s.

#![cfg(feature = "account-runtime")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fauna_account_store::sqlite::SqliteBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::{StateEntry, WriterId};
use fauna_core::custodian_endpoints::CustodianEndpoints;
use fauna_core::custodies_held::CustodyHeld;
use fauna_core::custody_ceremony::DEFAULT_RETAINED_BYTES_CAP;
use fauna_core::custody_grant::{
    CUSTODY_GRANT_ID_LEN, CustodyGrant, CustodyScopeSet, custody_entry_key, sign_custody_grant,
};
use fauna_core::data::{ModerationConfig, Timestamp};
use fauna_core::device_endpoints::DeviceEndpoints;
use fauna_core::encoding::{canonical_decode, canonical_encode};
use fauna_core::identity::ActorKeypair;
use fauna_credential_store::CredentialStore;
use fauna_protocol::account_state::{ACCOUNT_STATE_SCOPE, ItemClass};
use fauna_protocol::discovery::NestInfoReply;
use fauna_protocol::merge_policy::{
    KIND_CUSTODIAN_ENDPOINTS, KIND_CUSTODIES_HELD, KIND_MODERATION, home_scope_for_kind,
};
use fauna_protocol::sync::{SyncChangesListReply, SyncChangesListRequest};
use fauna_protocol::{RpcErrorClass, RpcRequester, decode_strict, encode_canonical};
use fauna_sync_engine::account_runtime::{
    AccountRuntimeParams, AccountStoreHandle, AccountStoreRuntime, CRED_NAMESPACE,
    RuntimePrincipal, StoreRoot, resolve_writer_key_serialized,
};
use fauna_transport::PeerTransport as _;
use fauna_transport::testing::{Listeners, MemTransport, await_listening, listeners};

mod common;
use common::mem_factory;

fn owner() -> ActorKeypair {
    ActorKeypair::from_secret([0x5A; 32])
}

fn custodian() -> ActorKeypair {
    ActorKeypair::from_secret([0xC0; 32])
}

const GRANT_ID: [u8; CUSTODY_GRANT_ID_LEN] = [0x1D; CUSTODY_GRANT_ID_LEN];

/// A transport fault the pump absorbs — nothing reached a nest, because
/// there is none.
#[derive(Debug)]
struct NoNest(&'static str);

impl std::fmt::Display for NoNest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no nest in this test: {}", self.0)
    }
}

impl RpcErrorClass for NoNest {
    fn is_rejection(&self) -> bool {
        false
    }
}

#[derive(Clone)]
struct NestInfoOnly;

impl RpcRequester for NestInfoOnly {
    type Error = NoNest;

    async fn request<Req, Reply>(&self, kind: &'static str, _payload: Req) -> Result<Reply, NoNest>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        if kind == "fauna.nest.info" {
            let reply = NestInfoReply {
                capabilities: vec!["peer-sync".to_string()],
                ..Default::default()
            };
            return Ok(decode_strict(&encode_canonical(&reply).expect("encode"))
                .expect("node-info reply decodes"));
        }
        Err(NoNest(kind))
    }
}

impl fauna_protocol::KeyedRpcRequester for NestInfoOnly {
    async fn request_keyed<Req, Reply>(
        &self,
        kind: &'static str,
        _idempotency_key: [u8; 16],
        payload: Req,
    ) -> Result<Reply, NoNest>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.request(kind, payload).await
    }
}

fn creds_for(base: &Path, device: &str) -> CredentialStore {
    CredentialStore::with_file_backend(CRED_NAMESPACE, base.join(device).join("creds"))
}

fn store_root_for(base: &Path, device: &str) -> StoreRoot {
    StoreRoot::at(base.join(device).join("state"))
}

fn params(
    base: &Path,
    device: &str,
    account: &ActorKeypair,
    listeners: &Listeners,
) -> AccountRuntimeParams<NestInfoOnly> {
    AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        store_root: store_root_for(base, device),
        actor_id_hex: account.actor_id_hex(),
        rpc: NestInfoOnly,
        process_rpc: None,
        principal: RuntimePrincipal::SeedHolding(
            ActorKeypair::from_secret(*account.secret_bytes()).into(),
        ),
        credentials: creds_for(base, device),
        reconnects: None,
        pushes: None,
        backstop_interval: Duration::from_secs(3600),
        memberships: None,
        trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(Vec::new()),
        attested_predecessors: Default::default(),
        linked_nests: None,
        owed_nests: None,
        peer_transport: Some(mem_factory(listeners)),
        // The machine's named row — never read here (no nest answers enrollment).
        enrollment_target_device_id: "ab".repeat(32),
    }
}

/// Pre-mint a device's writer key under its ACCOUNT and stage rows into its
/// store — before its runtime starts, so staging never races an assembly.
async fn premint_and_stage(
    base: &Path,
    device: &str,
    account: &ActorKeypair,
    rows: &[StateEntry],
) -> [u8; 32] {
    let store_root = store_root_for(base, device);
    let creds = creds_for(base, device);
    let writer_key = resolve_writer_key_serialized(&store_root, &account.actor_id_hex(), &creds)
        .expect("pre-mint writer key");
    let writer_pub = writer_key.verifying_key().to_bytes();
    if !rows.is_empty() {
        let dir = store_root
            .store_dir(&account.actor_id_hex())
            .expect("store dir");
        let store = AccountStore::open(
            SqliteBackend::open(&dir).unwrap(),
            &account.actor_id_hex(),
            WriterId(writer_pub),
        )
        .await
        .unwrap();
        for row in rows {
            store.put_state(row.clone()).await.unwrap();
        }
    }
    writer_pub
}

fn state_row(kind: &str, key: String, value: Vec<u8>) -> StateEntry {
    StateEntry {
        kind: kind.into(),
        key,
        scope: home_scope_for_kind(kind).unwrap().into(),
        value,
        merge_meta: None,
        entry_version: 0,
        tombstone: false,
    }
}

fn endpoints_of(node_id: [u8; 32]) -> DeviceEndpoints {
    DeviceEndpoints {
        node_id,
        lan_addrs: Vec::new(),
        public_addrs: Vec::new(),
        relay_url: None,
    }
}

/// The owner's grant log — the custody grant's events as
/// `fauna.state.succession-ledger` rows, what the runtime's revocation-snapshot
/// refresh reads — staged into `device`'s store before its runtime starts
/// ([`premint_and_stage`]'s discipline: staging never races an assembly).
async fn stage_owner_grant_log(base: &Path, device: &str, revoke: bool) {
    use fauna_client_capabilities::custody_grants::custody_event_scopes;
    use fauna_client_capabilities::grant_log::{record_mint, record_revoke};

    let kp = owner();
    let mut log = fauna_core::succession_ledger::SuccessionLedger::empty(kp.actor_id());
    let custodian_key = [0u8; 32]; // the holder slot; the snapshot keys on the grant id
    record_mint(
        &mut log,
        kp.signing_key(),
        GRANT_ID,
        custodian_key,
        custody_event_scopes(&CustodyScopeSet::Account),
        1_000,
        9_000_000_000,
        1_000,
    )
    .expect("record mint");
    if revoke {
        record_revoke(&mut log, kp.signing_key(), GRANT_ID, custodian_key, 2_000)
            .expect("record revoke");
    }
    let rows: Vec<StateEntry> = fauna_core::succession_ledger::SuccessionLedger {
        grant_events: log.grant_events,
        ..fauna_core::succession_ledger::SuccessionLedger::empty(kp.actor_id())
    }
    .rows()
    .unwrap()
    .into_iter()
    .filter(|(key, _)| key != fauna_core::succession_ledger::CHAIN_KEY)
    .map(|(key, record)| {
        state_row(
            fauna_protocol::merge_policy::KIND_SUCCESSION_LEDGER,
            key,
            record.encode().unwrap(),
        )
    })
    .collect();
    premint_and_stage(base, device, &kp, &rows).await;
}

/// One device's own `fauna.state.custodian-endpoints` row (read-only).
async fn custodian_row_of(
    base: &Path,
    device: &str,
    writer: [u8; 32],
) -> Option<CustodianEndpoints> {
    let dir = store_root_for(base, device)
        .store_dir(&owner().actor_id_hex())
        .ok()?;
    let store = AccountStore::open(
        SqliteBackend::open(&dir).ok()?,
        &owner().actor_id_hex(),
        WriterId(writer),
    )
    .await
    .ok()?;
    let entry = store
        .states_of_kind(KIND_CUSTODIAN_ENDPOINTS)
        .await
        .ok()?
        .into_iter()
        .next()?;
    canonical_decode(&entry.value).ok()
}

/// Sealed state-scope rows in C's custodied relay plane — the cumulative
/// pull outcome (opened read-only under the store's own writer identity).
async fn custodied_relay_rows(base: &Path) -> usize {
    let Ok(dir) = store_root_for(base, "c").store_dir(&owner().actor_id_hex()) else {
        return 0;
    };
    let Ok(backend) = SqliteBackend::open(&dir) else {
        return 0;
    };
    let Ok(store) = AccountStore::open(
        backend,
        &owner().actor_id_hex(),
        WriterId(custodian_device_pub(base)),
    )
    .await
    else {
        return 0;
    };
    store
        .relay_rows(
            ACCOUNT_STATE_SCOPE,
            ItemClass::StateEntry.as_wire(),
            &[],
            u32::MAX,
        )
        .await
        .map(|r| r.len())
        .unwrap_or(0)
}

/// Payload bytes held in C's custodied store, by the same meter the budget
/// pass uses. `None` = no custodied store yet. The cumulative witness that the
/// pump ran T15's budget: nothing else in the runtime clears a relay payload.
async fn custodied_payload_bytes(base: &Path) -> Option<u64> {
    let dir = store_root_for(base, "c")
        .store_dir(&owner().actor_id_hex())
        .ok()?;
    let store = AccountStore::open(
        SqliteBackend::open(&dir).ok()?,
        &owner().actor_id_hex(),
        WriterId(custodian_device_pub(base)),
    )
    .await
    .ok()?;
    store
        .custody_meter(&[fauna_protocol::account_state::OP_TOMBSTONE])
        .await
        .ok()
        .map(|m| m.held_bytes())
}

/// C's device principal — the custodied store's writer identity tag.
fn custodian_device_pub(base: &Path) -> [u8; 32] {
    resolve_writer_key_serialized(
        &store_root_for(base, "c"),
        &custodian().actor_id_hex(),
        &creds_for(base, "c"),
    )
    .expect("C writer key")
    .verifying_key()
    .to_bytes()
}

const CONVERGENCE_BUDGET: Duration = Duration::from_secs(60);

/// [`eventually`] for a probe that also yields the value it waited for.
async fn eventually_some<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    match tokio::time::timeout(CONVERGENCE_BUDGET, async {
        loop {
            if let Some(v) = probe().await {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    {
        Ok(v) => v,
        Err(_) => panic!("eventually_some({what}): not reached within budget"),
    }
}

async fn eventually<F, Fut>(what: &str, mut probe: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    if tokio::time::timeout(CONVERGENCE_BUDGET, async {
        while !probe().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_err()
    {
        panic!("eventually({what}): not reached within budget");
    }
}

async fn moderation_of(handle: &AccountStoreHandle) -> Option<Vec<String>> {
    handle
        .get_preference(KIND_MODERATION)
        .await
        .ok()
        .flatten()
        .and_then(|row| canonical_decode::<ModerationConfig>(&row.value).ok())
        .map(|m| {
            m.muted_keywords
                .into_iter()
                .map(|k| k.keyword)
                .collect::<Vec<_>>()
        })
}

/// The whole W8.5 success sentence, one flow — see the module docs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_row_converges_to_a_second_device_through_a_keyless_custodian() {
    let tmp = tempfile::tempdir().unwrap();
    let base: PathBuf = tmp.path().to_path_buf();
    let net = listeners();

    // ── Identities + the grant ────────────────────────────────────────────
    let a_pub = premint_and_stage(&base, "a", &owner(), &[]).await;
    // C's serving device key must be known before the witness can name it.
    let c_pub = {
        let store_root = store_root_for(&base, "c");
        let creds = creds_for(&base, "c");
        resolve_writer_key_serialized(&store_root, &custodian().actor_id_hex(), &creds)
            .expect("C writer key")
            .verifying_key()
            .to_bytes()
    };
    let witness = sign_custody_grant(
        &owner(),
        &CustodyGrant {
            grant_id: GRANT_ID.to_vec(),
            owner: owner().actor_id(),
            custodian_key: c_pub,
            scopes: CustodyScopeSet::Account,
            minted_at: Timestamp(1_000),
            expires_at: Timestamp(9_000_000_000_000_000), // far future (µs)
            removed_devices: Vec::new(),
        },
    )
    .expect("sign witness");
    let witness_bytes = canonical_encode(&witness).unwrap().to_vec();

    // ── Staging: each store holds exactly what production would have ─────
    // C: the custodies-held row the W8.4 ceremony delivered (witness + the
    // owner fleet's candidates — device A).
    premint_and_stage(
        &base,
        "c",
        &custodian(),
        &[state_row(
            KIND_CUSTODIES_HELD,
            custody_entry_key(&GRANT_ID),
            canonical_encode(&CustodyHeld {
                grant_id: GRANT_ID.to_vec(),
                owner: owner().actor_id().0,
                witness: witness_bytes,
                owner_devices: vec![endpoints_of(a_pub)],
                owner_nest_url: None,
                // Ample on purpose. This field became LOAD-BEARING in W8.7:
                // the budget pass reads it every dial pass, so the old
                // placeholder (42 bytes) now evicts every sealed row's
                // payload before B can pull it — this test's convergence
                // sentence is about the custody path, not the budget. The
                // budget's own proof is the sibling test below.
                retained_bytes_cap: DEFAULT_RETAINED_BYTES_CAP,
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        )],
    )
    .await;
    // B: ONLY the custodian-endpoints row — its single route to A's data is
    // through C. (No device-endpoints row for A, on purpose.) Its candidates
    // are the ceremony-time value, deliberately EMPTY here: the row is stale
    // exactly as a real one goes stale, and T13 step 4's re-exchange is what
    // heals it (asserted after B converges).
    let b_pub = premint_and_stage(
        &base,
        "b",
        &owner(),
        &[state_row(
            KIND_CUSTODIAN_ENDPOINTS,
            custody_entry_key(&GRANT_ID),
            canonical_encode(&CustodianEndpoints {
                grant_id: GRANT_ID.to_vec(),
                endpoints: endpoints_of(c_pub),
                // Struct-update, not an exhaustive literal: the next field this
                // wire type grows must not break this fixture, and two branches
                // growing it independently should merge cleanly.
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        )],
    )
    .await;
    // A + B's grant logs: the grant's Mint event (the revocation snapshot's
    // green half). C has no owner log — it is not the owner's fleet.
    stage_owner_grant_log(&base, "a", false).await;
    stage_owner_grant_log(&base, "b", false).await;

    // ── A: seal the row the whole chain must carry ────────────────────────
    let a = AccountStoreRuntime::start(params(&base, "a", &owner(), &net))
        .await
        .expect("start A");
    let value = canonical_encode(&ModerationConfig {
        muted_keywords: vec!["through-the-custodian".into()],
        ..Default::default()
    })
    .unwrap()
    .to_vec();
    let _ = a.put_preference(KIND_MODERATION, value).await; // nest leg dead — locally durable
    await_listening(&net, &a_pub).await;
    // ── C: the custodian pulls A's sealed rows under the REAL witness ────
    let c = AccountStoreRuntime::start(params(&base, "c", &custodian(), &net))
        .await
        .expect("start C");
    // The pull may complete on C's very first (startup) pass, so the probe
    // asserts the CUMULATIVE outcome — sealed rows in the custodied relay
    // plane — never a per-pass delta.
    eventually("C pulls the owner's sealed rows from A", || {
        let (a, c) = (a.clone(), c.clone());
        let base = base.clone();
        async move {
            let _ = a.reconcile_now().await; // A refreshes its revocation snapshot
            let report = c.reconcile_now().await.expect("C pass");
            let admitted = report
                .custody_dial
                .map(|d| d.admitted >= 1)
                .unwrap_or(false);
            admitted && custodied_relay_rows(&base).await > 0
        }
    })
    .await;
    await_listening(&net, &c_pub).await;

    // The custodied store is KEYLESS by construction: sealed relay rows
    // only — nothing journaled, nothing merged, no projection.
    {
        let dir = store_root_for(&base, "c")
            .store_dir(&owner().actor_id_hex())
            .expect("custodied store dir");
        let probe = AccountStore::open(
            SqliteBackend::open(&dir).unwrap(),
            &owner().actor_id_hex(),
            WriterId(c_pub), // the store's own writer identity; read-only here
        )
        .await
        .unwrap();
        let rows = probe
            .relay_rows(
                ACCOUNT_STATE_SCOPE,
                ItemClass::StateEntry.as_wire(),
                &[],
                u32::MAX,
            )
            .await
            .unwrap();
        assert!(!rows.is_empty(), "C holds A's sealed rows verbatim");
        assert_eq!(
            probe
                .max_held_seq(ACCOUNT_STATE_SCOPE, &WriterId(a_pub))
                .await
                .unwrap(),
            None,
            "a custodian never journals"
        );
        assert!(
            probe
                .states_of_kind(KIND_MODERATION)
                .await
                .unwrap()
                .is_empty(),
            "a custodian never merges a projection"
        );
    }

    // ── B: the second owner device converges THROUGH C ───────────────────
    let b = AccountStoreRuntime::start(params(&base, "b", &owner(), &net))
        .await
        .expect("start B");
    eventually("B converges A's row through the custodian", || {
        let (b, c) = (b.clone(), c.clone());
        async move {
            let _ = c.reconcile_now().await; // C refreshes its serve registry
            let _ = b.reconcile_now().await;
            moderation_of(&b).await.as_deref() == Some(&["through-the-custodian".to_string()][..])
        }
    })
    .await;

    // ── T13 step 4: the re-exchange, over the production stack ───────────
    // B staged C's candidates as the ceremony left them (EMPTY — a row that
    // has gone stale), and B can never read C's fleet-only
    // `device-endpoints` kind, C being a different account. So the admit
    // exchange is B's only possible source for where C is now, and after a
    // real session B must hold C's live bound address, keyed by the identity
    // the CHANNEL proved rather than anything C asserted about itself.
    //
    // What this asserts is the observation, not the sealed row: the write-back
    // rides the fleet plane's `GenerationTip` door, and B — nestless here —
    // resolves no tip, so its put stays correctly *pending* rather than
    // sealing (the same reason B publishes no `device-endpoints` row of its
    // own in this fixture). The write-back rule itself is pinned at tier_1 in
    // `custody_rows::tests`.
    let observed = eventually_some(
        "B observes C's live candidates over the admit exchange",
        || {
            let (b, c) = (b.clone(), c.clone());
            async move {
                let _ = c.reconcile_now().await;
                let report = b.reconcile_now().await.ok()?;
                report.peer_dial?.observed.get(&c_pub).cloned()
            }
        },
    )
    .await;
    assert_eq!(
        observed.lan_addrs,
        vec!["203.0.113.9:4711".to_string()],
        "B learned exactly what C's transport bound — carried on the admit \
         reply, since no plane could ever have told B"
    );
    assert_eq!(
        observed.node_id, c_pub,
        "the value is bound to the channel-proven key, not to what C named"
    );
    // And the row it will be folded into is still the ceremony's: a refresh
    // moves WHERE a custodian is, never WHO it is.
    let row = custodian_row_of(&base, "b", b_pub)
        .await
        .expect("B holds the custodian row");
    assert_eq!(row.grant_id, GRANT_ID.to_vec());
    assert_eq!(row.endpoints.node_id, c_pub);

    // ── The carve-out, live on the wire ───────────────────────────────────
    // A probe custodian (its own key + witness) admits at A with the
    // Account-form grant: the state scope serves, a conv scope refuses.
    {
        use fauna_peer_sync::PeerRequester;

        let probe_key = ed25519_dalek::SigningKey::from_bytes(&[0xF7; 32]);
        let probe_pub = probe_key.verifying_key().to_bytes();
        let probe_witness = sign_custody_grant(
            &owner(),
            &CustodyGrant {
                grant_id: [0x2E; CUSTODY_GRANT_ID_LEN].to_vec(),
                owner: owner().actor_id(),
                custodian_key: probe_pub,
                scopes: CustodyScopeSet::Account,
                minted_at: Timestamp(1_000),
                expires_at: Timestamp(9_000_000_000_000_000),
                removed_devices: Vec::new(),
            },
        )
        .unwrap();
        let transport = MemTransport {
            me: fauna_transport::EndpointKey::from_bytes(probe_pub),
            listeners: Listeners::clone(&net),
        };
        let conn = transport
            .dial(
                fauna_transport::EndpointKey::from_bytes(a_pub),
                fauna_transport::PathCandidates {
                    lan_endpoints: Vec::new(),
                    wan_endpoint: None,
                    relay_available: false,
                },
            )
            .await
            .expect("probe dial");
        let channel = Arc::new(fauna_peer_channel::PeerChannel::open(conn).await.unwrap());
        fauna_peer_sync::admit_over_as(
            &channel,
            fauna_protocol::peer_sync::WITNESS_CUSTODY_GRANT,
            probe_witness,
            &owner().actor_id().0,
            2_000,
            fauna_peer_sync::AdmissionViews::default(),
            None,
        )
        .await
        .expect("the probe witness admits (unrevoked id)");
        let list = |scope: String| {
            let requester = PeerRequester::new(Arc::clone(&channel));
            async move {
                requester
                    .request::<_, SyncChangesListReply>(
                        "fauna.sync.changes.list",
                        SyncChangesListRequest {
                            since: 0,
                            item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
                            scope: Some(scope),
                            frontier: Some(BTreeMap::new()),
                            ..Default::default()
                        },
                    )
                    .await
            }
        };
        assert!(
            list(ACCOUNT_STATE_SCOPE.to_string()).await.is_ok(),
            "the state scope serves under the Account-form custody verdict"
        );
        let conv = format!("content:conv:{}", "2b".repeat(32));
        assert!(
            list(conv).await.is_err(),
            "a co-authored scope is refused under the Account form — the carve-out, live"
        );
    }

    // ── Revocation severs at the next admission evaluation ───────────────
    // The owner's Revoke reaches A's store while A is down (staging never
    // races a running assembly); A's first pass after the restart derives it.
    a.shutdown().await;
    stage_owner_grant_log(&base, "a", true).await;
    let a = AccountStoreRuntime::start(params(&base, "a", &owner(), &net))
        .await
        .expect("restart A");
    await_listening(&net, &a_pub).await;
    eventually(
        "the revoked grant is refused at A's next evaluation",
        || {
            let (a, c) = (a.clone(), c.clone());
            async move {
                let _ = a.reconcile_now().await; // snapshot refresh picks up the revoke
                let report = c.reconcile_now().await.expect("C pass");
                report
                    .custody_dial
                    .map(|d| d.admitted == 0 && d.failed >= 1)
                    .unwrap_or(false)
            }
        },
    )
    .await;

    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
}

/// **The held grant's exclusion list, both directions, through the pumps**
/// (`account-replica-posture.md` § The custody grant + ceremony, the witness
/// bullet): C holds a grant whose owner-signed `removed_devices` names owner
/// device A. C cannot read the owner's sealed device-set rows, so that list is
/// its whole exclusion set for the account — and it must refuse A both ways:
/// C's custody dial never walks A (the dialer's view), and A dialing C as its
/// custodian is refused at C's admit door (the serve view).
///
/// Owner device B — same account, same fixture, NOT listed — is the in-test
/// control: every asserted pass admits B in the same direction, so a refusal
/// of A is the list's and never a cold snapshot's or a dead path's. Asserted
/// over consecutive settled passes, never a first failure (a startup pass
/// refuses custody at A until A's revocation snapshot derives).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removed_owner_device_the_held_grant_lists_is_refused_by_the_custodian_both_ways() {
    let tmp = tempfile::tempdir().unwrap();
    let base: PathBuf = tmp.path().to_path_buf();
    let net = listeners();

    let c_pub = custodian_device_pub(&base);
    // Both owner devices hold C's custodian-endpoints row, so each one's own
    // peer dial targets C.
    let endpoints_row = || {
        state_row(
            KIND_CUSTODIAN_ENDPOINTS,
            custody_entry_key(&GRANT_ID),
            canonical_encode(&CustodianEndpoints {
                grant_id: GRANT_ID.to_vec(),
                endpoints: endpoints_of(c_pub),
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        )
    };
    let a_pub = premint_and_stage(&base, "a", &owner(), &[endpoints_row()]).await;
    let b_pub = premint_and_stage(&base, "b", &owner(), &[endpoints_row()]).await;
    // The grant was minted after the owner fleet removed A.
    let witness = sign_custody_grant(
        &owner(),
        &CustodyGrant {
            grant_id: GRANT_ID.to_vec(),
            owner: owner().actor_id(),
            custodian_key: c_pub,
            scopes: CustodyScopeSet::Account,
            minted_at: Timestamp(1_000),
            expires_at: Timestamp(9_000_000_000_000_000),
            removed_devices: vec![a_pub],
        },
    )
    .expect("sign witness");
    premint_and_stage(
        &base,
        "c",
        &custodian(),
        &[state_row(
            KIND_CUSTODIES_HELD,
            custody_entry_key(&GRANT_ID),
            canonical_encode(&CustodyHeld {
                grant_id: GRANT_ID.to_vec(),
                owner: owner().actor_id().0,
                witness: canonical_encode(&witness).unwrap().to_vec(),
                owner_devices: vec![endpoints_of(a_pub), endpoints_of(b_pub)],
                owner_nest_url: None,
                retained_bytes_cap: DEFAULT_RETAINED_BYTES_CAP,
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        )],
    )
    .await;
    stage_owner_grant_log(&base, "a", false).await;
    stage_owner_grant_log(&base, "b", false).await;

    let a = AccountStoreRuntime::start(params(&base, "a", &owner(), &net))
        .await
        .expect("start A");
    let b = AccountStoreRuntime::start(params(&base, "b", &owner(), &net))
        .await
        .expect("start B");
    await_listening(&net, &a_pub).await;
    await_listening(&net, &b_pub).await;
    let c = AccountStoreRuntime::start(params(&base, "c", &custodian(), &net))
        .await
        .expect("start C");
    await_listening(&net, &c_pub).await;

    // Settle: C's custody dial reaches the control device B.
    eventually("C's custody dial admits the unlisted device", || {
        let (a, b, c) = (a.clone(), b.clone(), c.clone());
        async move {
            let _ = a.reconcile_now().await;
            let _ = b.reconcile_now().await;
            let report = c.reconcile_now().await.expect("C pass");
            report
                .custody_dial
                .map(|d| d.admitted >= 1)
                .unwrap_or(false)
        }
    })
    .await;

    for pass in 0..3 {
        // C → A: every settled pass admits B and refuses A.
        let _ = a.reconcile_now().await;
        let _ = b.reconcile_now().await;
        let dial = c
            .reconcile_now()
            .await
            .expect("C pass")
            .custody_dial
            .expect("C's custody dial ran");
        assert_eq!(
            (dial.admitted, dial.failed),
            (1, 1),
            "pass {pass}: C's dial admits B and refuses the listed A: {dial:?}"
        );

        // A → C: C's admit door refuses A while it admits B.
        let a_dial = a
            .reconcile_now()
            .await
            .expect("A pass")
            .peer_dial
            .expect("A's peer dial ran");
        let b_dial = b
            .reconcile_now()
            .await
            .expect("B pass")
            .peer_dial
            .expect("B's peer dial ran");
        assert_eq!(
            b_dial.admitted, b_dial.targets,
            "pass {pass}: the unlisted device admits at C: {b_dial:?}"
        );
        assert!(
            a_dial.targets >= 1 && a_dial.admitted == 0,
            "pass {pass}: C's admit door refuses the listed device: {a_dial:?}"
        );
    }

    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
}

/// **W8.7's success core** (`account-data-plane.md` § Replica posture →
/// *Custody policy* (T15)): the accepted byte budget is enforced BY THE PUMP,
/// on bytes a real custody pull put there, and enforcing it costs payload
/// only.
///
/// The tier_1 tests in `custody_leg.rs` prove `meter_and_evict`'s arithmetic
/// and its store effects. What only this tier can prove is the wiring: that
/// the dial pass actually calls it with the row's own cap, so a budget nobody
/// invoked cannot pass for a budget enforced. (It caught its own regression
/// on arrival — the fixture above carried a 42-byte placeholder cap from when
/// the field had no consumer, and the convergence test went red the moment
/// eviction became real.)
///
/// Two runtimes, no B and no nest: A seals, C pulls under the real witness
/// with a cap far under what it pulled, and the assertions are T15's floor —
/// coordinates survive, tombstones survive, and the shrinkage is reported.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_custodians_pump_enforces_the_accepted_byte_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let base: PathBuf = tmp.path().to_path_buf();
    let net = listeners();

    let a_pub = premint_and_stage(&base, "a", &owner(), &[]).await;
    let c_pub = custodian_device_pub(&base);
    let witness = sign_custody_grant(
        &owner(),
        &CustodyGrant {
            grant_id: GRANT_ID.to_vec(),
            owner: owner().actor_id(),
            custodian_key: c_pub,
            scopes: CustodyScopeSet::Account,
            minted_at: Timestamp(1_000),
            expires_at: Timestamp(9_000_000_000_000_000),
            removed_devices: Vec::new(),
        },
    )
    .expect("sign witness");

    // A one-byte cap: whatever A seals, C is over budget the instant it lands.
    // Deliberately not 0 — that spelling means "no cap recorded" and falls back
    // to the ceremony default (`custody_leg::meter_and_evict`).
    const TINY_CAP: u64 = 1;
    premint_and_stage(
        &base,
        "c",
        &custodian(),
        &[state_row(
            KIND_CUSTODIES_HELD,
            custody_entry_key(&GRANT_ID),
            canonical_encode(&CustodyHeld {
                grant_id: GRANT_ID.to_vec(),
                owner: owner().actor_id().0,
                witness: canonical_encode(&witness).unwrap().to_vec(),
                owner_devices: vec![endpoints_of(a_pub)],
                owner_nest_url: None,
                retained_bytes_cap: TINY_CAP,
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        )],
    )
    .await;
    stage_owner_grant_log(&base, "a", false).await;

    let a = AccountStoreRuntime::start(params(&base, "a", &owner(), &net))
        .await
        .expect("start A");
    let value = canonical_encode(&ModerationConfig {
        muted_keywords: vec!["over-the-custodians-budget".into()],
        ..Default::default()
    })
    .unwrap()
    .to_vec();
    let _ = a.put_preference(KIND_MODERATION, value).await;
    await_listening(&net, &a_pub).await;

    let c = AccountStoreRuntime::start(params(&base, "c", &custodian(), &net))
        .await
        .expect("start C");
    // The pass reports its own eviction, which is the wiring assertion: these
    // slots are only ever written by the dial pass calling the budget.
    // Staged so a failure names its own leg (convention 6): first that the
    // custody path ran at all, then that the budget acted on what it pulled.
    //
    // Both probes assert the CUMULATIVE outcome, never a per-pass delta — the
    // trap the sibling test's comment names, and one this test fell into on
    // first writing: the pull AND its budget pass can both complete on C's
    // startup pass, so `evicted_rows` is back to 0 by the time a probe looks,
    // with nothing left to evict on any later pass. The durable witness is the
    // store: nothing else in the runtime NULLs a relay row's payload, so
    // payload-less-rows-with-coordinates is the pump having run the budget.
    eventually("C's pump admits at A and pulls under the witness", || {
        let (a, c) = (a.clone(), c.clone());
        let base = base.clone();
        async move {
            let _ = a.reconcile_now().await;
            let report = c.reconcile_now().await.expect("C pass");
            report
                .custody_dial
                .map(|d| d.admitted >= 1)
                .unwrap_or(false)
                && custodied_relay_rows(&base).await > 0
        }
    })
    .await;
    eventually("and the budget has evicted what it pulled", || {
        let (a, c) = (a.clone(), c.clone());
        let base = base.clone();
        async move {
            let _ = a.reconcile_now().await;
            let _ = c.reconcile_now().await.expect("C pass");
            custodied_payload_bytes(&base).await == Some(0)
        }
    })
    .await;

    // ── T15's floor, on the real custodied store ─────────────────────────
    let dir = store_root_for(&base, "c")
        .store_dir(&owner().actor_id_hex())
        .expect("custodied store dir");
    let probe = AccountStore::open(
        SqliteBackend::open(&dir).unwrap(),
        &owner().actor_id_hex(),
        WriterId(c_pub),
    )
    .await
    .unwrap();
    let rows = probe
        .relay_rows(
            ACCOUNT_STATE_SCOPE,
            ItemClass::StateEntry.as_wire(),
            &[],
            u32::MAX,
        )
        .await
        .unwrap();
    assert!(
        !rows.is_empty(),
        "eviction is dehydration, not deletion — the coordinate rows survive"
    );
    for r in &rows {
        assert!(
            !r.item_key.is_empty() && !r.op.is_empty(),
            "every surviving row keeps the coordinate floor a peer's frontier accounts"
        );
    }
    assert!(
        rows.iter().any(|r| r.entry.is_none()),
        "and the payload is what went"
    );

    // The meter agrees, and so does the receipt the owner would read: this
    // custodian is honestly degraded, not quietly thin.
    let meter = probe
        .custody_meter(&[fauna_protocol::account_state::OP_TOMBSTONE])
        .await
        .unwrap();
    assert!(
        meter.held_bytes() <= meter.floor_bytes(),
        "nothing evictable is left above the floor: held {} vs floor {}",
        meter.held_bytes(),
        meter.floor_bytes()
    );

    a.shutdown().await;
    c.shutdown().await;
}
