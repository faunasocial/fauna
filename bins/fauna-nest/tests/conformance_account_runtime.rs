//! The W3 (account-data-plane.md § Workstreams) lifecycle runtime end to end (`docs/goal/architecture/
//! account-data-plane.md` § The account store → *The client-side lifecycle*;
//! the read-path inversion's flow assertion, V1): two
//! [`AccountStoreRuntime`] instances converge over the real nest handlers and
//! the real scope-tagged `fauna.sync.changed` push — nothing between them
//! stubbed, and no direct plane calls in any test body (the handle is the
//! only client surface, exactly as an app consumes it). No nest in this file
//! serves a blob rail for account state: the runtime never asks for one.
//!
//! The flows this file drives:
//!
//! 1. **V1 — the pump converges two replicas through the runtime:** A's
//!    `put_preference` → the local plane put, then its publish step → the nest's state-put
//!    handler records the feed row and fires the scope-tagged nudge →
//!    the push frame reaches the account's subscribed connection → the
//!    runtime's own push arm (`AccountRuntimeParams::pushes`, mapped by
//!    `nudge_scope_for_push`) wakes B's pump → walk + merge → B's
//!    handle read returns A's value. The backstop ticker is disarmed and no
//!    reconnect arm exists, so the nudge is the *only* path B can converge
//!    by — a hung nudge chain fails the test rather than hiding behind a
//!    rescan.
//! 2. **V4 — the auto-in-set seen-set producer** (charter § The replica
//!    boundary, the T1 producer decomposition): a nest-sequenced post record
//!    enters the seen-set on the next pass with no render event, and a second
//!    device converges through the class-2 walk without echoing a row.
//! 3. **V6 — the T1 browse trigger, end to end** (same section, the producer
//!    decomposition's *other* half): an app reports one rendered body through
//!    `record_observation`, that ONE record's scope-feed coordinate enters the
//!    channel's seen-set entry itemized, and the sibling device converges on
//!    it. This is the flow no test could drive until an app could call the
//!    intake — the reporting half tui now builds.
//! 4. **V8 — the W5.1 engine-singleton election** (§ Multi-instance
//!    concurrency, T9): two runtimes on ONE store dir, exactly one pumping,
//!    the role transferring on the survivor's own backstop tick when the
//!    holder exits. (Numbered V8, not V6: the case landed 2026-08-14 spelled
//!    `V6`, colliding with the T1 case above, and the duplicate is corrected
//!    here rather than left for a cold reader to trip on.)
//! 5. **V9 — the W5.3 migration/adoption critical section** (same section):
//!    two runtimes assembling *concurrently* against one un-migrated store
//!    dir both come up, on a store neither corrupted. The exactly-once
//!    observable is `fauna-account-store`'s own tier_1 pin (it can count
//!    rebuilds; this level cannot); what this level adds is the production
//!    assembly path — `AccountStore::open`'s adoption included — under the
//!    same race.
//!
//! Tier: tier_3 (real nest handlers + real stores + real runtimes). Every
//! assertion is on latency-independent state (e2e convention 14): every
//! positive wait is a named-budget deadline poll — no sleeps, no wall-clock
//! asserts. (V3 and V16, which imported an old client's blob write through
//! the CAS-blob bridge, retired with the bridge at closure step (5) of
//! `config-dissolution.md` § The `__config` dissolution schedule.)

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use ed25519_dalek::SigningKey;
use fauna_account_store::types::WriterId;
use fauna_core::data::ModerationConfig;
use fauna_core::device_endpoints::{DeviceEndpoints, DeviceEndpointsEntry};
use fauna_core::encoding::{canonical_decode, canonical_encode};
use fauna_core::identity::ActorKeypair;
use fauna_core::seen_set::SeenScopeSet;
use fauna_credential_store::CredentialStore;
use fauna_nest::backup::service::BackupService;
use fauna_nest::{
    db::CacheDb, folder_handlers, routes::AppState, rpc_router::RpcRouter, sync_handlers,
};
use fauna_protocol::account_state::{ACCOUNT_STATE_SCOPE, ItemClass};
use fauna_protocol::merge_policy::{
    KIND_DEVICE_ENDPOINTS, KIND_GENERATION_MINT, KIND_MODERATION, KIND_SEEN_SET,
};
use fauna_protocol::scope::ContentScope;
use fauna_protocol::sync::{SyncChangesListReply, SyncChangesListRequest};
use fauna_protocol::{
    Frame, PushEvent, RpcError, RpcErrorClass, RpcRequester, decode_frame, encode_canonical,
};
use fauna_sync_engine::account_runtime::{
    AccountRuntimeParams, AccountStoreHandle, AccountStoreRuntime, CRED_NAMESPACE, EnrollmentPass,
    PeerLegBinding, PeerLegPass, PeerTransportFactory, RuntimePrincipal, StoreRoot,
    resolve_writer_key_serialized,
};
use fauna_sync_engine::device_endpoints_writer::{EndpointFacts, EndpointsPass};

// ── One account: a shared identity, per-device writers ──────────────────────

fn account() -> ActorKeypair {
    ActorKeypair::from_secret([0x33; 32])
}

/// The dispatch actor IS the account's actor id: the nest keys feed rows and push fan-out by the dispatch
/// actor, and the runtime keys its store placement by `actor_id_hex` — one
/// identity end to end, as in production.
fn actor_bytes() -> [u8; 32] {
    fauna_core::hex32::decode(&account().actor_id_hex()).expect("actor id is 32-byte hex")
}

// ── The transport: the real router, dispatched in-process ────────────────────

struct RouterRequester {
    router: RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
}

#[derive(Debug)]
struct Refused(RpcError);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {:?}", self.0.code, self.0.message)
    }
}

/// A handler refusal is a server rejection carrying its wire code.
impl RpcErrorClass for Refused {
    fn is_rejection(&self) -> bool {
        true
    }
    fn as_rpc_error(&self) -> Option<&RpcError> {
        Some(&self.0)
    }
}

impl RpcRequester for RouterRequester {
    type Error = Refused;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply, Refused>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        // The dispatch actor is seeded ONCE in `nest()`, not here: this
        // requester is dispatched concurrently from each runtime's store
        // thread and the test thread, and `seed_dispatch_actor`'s
        // check-then-insert is sequentially idempotent but not
        // concurrency-safe (two racing seeds → UNIQUE violation).
        let Some(meta) = self.router.kind_meta(kind) else {
            // Production shape: a nest that does not serve a kind refuses it
            // (the pump reads the rotation chain at every assembly, which most
            // rigs here do not register — the answer of a nest that has not registered it).
            return Err(Refused(RpcError::new(
                "kind_not_served",
                "test.router.kind_not_served",
            )));
        };
        let bytes = Bytes::from(encode_canonical(&payload).unwrap().to_vec());
        let reply = (meta.handler)(Arc::clone(&self.state), self.actor, bytes)
            .await
            .map_err(Refused)?;
        Ok(fauna_protocol::decode_strict(&reply).expect("reply decodes"))
    }
}

/// The keyed (outbox-drain) arm the runtime's bound now requires. This
/// requester dispatches handlers directly — there is no envelope and no
/// connection-layer idempotency cache in the path — so the key has nowhere to
/// go and is **deliberately ignored**: these tests exercise the pump's plane
/// legs, not drain dedup (which the outbox's own tier_1s cover against a
/// recording fake, and phase 3's conformance will cover against the durable
/// table).
impl fauna_protocol::KeyedRpcRequester for RouterRequester {
    async fn request_keyed<Req, Reply>(
        &self,
        kind: &'static str,
        _idempotency_key: [u8; 16],
        payload: Req,
    ) -> Result<Reply, Refused>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.request(kind, payload).await
    }
}

/// A real nest: router + `AppState` (the `ws` registry included — V1's push
/// leg runs through it). The tempdir guard rides along: the runtimes place
/// their stores + credential slots under it.
async fn nest() -> (Arc<RouterRequester>, Arc<AppState>, tempfile::TempDir) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let blob_dir = tmp.path().join("blob");
    std::fs::create_dir_all(&blob_dir).unwrap();
    let backup_svc = Arc::new(BackupService::new(db.clone(), None, false, blob_dir, None).unwrap());
    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        ..AppState::for_test(db)
    });
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    // `fauna.capabilities.reconcile` — every full pass enumerates it.
    fauna_nest::bridge_blob_handlers::register_capability_handlers(&mut b);
    // Seed the one dispatch actor up front — see the note in `request`.
    common::seed_dispatch_actor(&state.db, &actor_bytes()).await;
    (
        Arc::new(RouterRequester {
            router: b.build(),
            state: Arc::clone(&state),
            actor: actor_bytes(),
        }),
        state,
        tmp,
    )
}

/// The machine's named `sync_devices` row for the fixture device `device` —
/// the app's own id, which the enrollment targets unconditionally
/// (`sync-agent-credentials.md` § Credential model → the RULED 2026-09-28
/// block, decision 3). A distinct 64-hex id per device.
fn device_row(device: &str) -> String {
    format!("{:0<64}", hex::encode(device.as_bytes()))
}

/// Params for one device of this account: a distinct state + credential area
/// per device (two devices = two writers over the one nest; the credential
/// slot is the real T10 type on a file backend — never the OS keyring,
/// testing.md § point 10). The backstop is disarmed (an hour) and there is no
/// reconnect arm, so in V1 the nudge is the only wake source under test.
fn params(
    base: &Path,
    device: &str,
    rpc: &Arc<RouterRequester>,
) -> AccountRuntimeParams<Arc<RouterRequester>> {
    AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        store_root: StoreRoot::at(base.join(device).join("state")),
        actor_id_hex: account().actor_id_hex(),
        rpc: Arc::clone(rpc),
        process_rpc: None,
        principal: RuntimePrincipal::SeedHolding(account().into()),
        credentials: CredentialStore::with_file_backend(
            CRED_NAMESPACE,
            base.join(device).join("creds"),
        ),
        reconnects: None,
        pushes: None,
        backstop_interval: Duration::from_secs(3600),
        // Own-actor scopes only: this suite pins the nudge as the sole wake
        // source, and a membership source would add a second scope origin the
        // assertions do not model.
        memberships: None,
        // No escrow holder trusted: this suite drives `Gen0` preference kinds
        // only, which the R14 (account-data-plane.md § The ratified decisions) door admits without resolving a tip. The
        // production-consumer path — a trusted holder's receipt lifting the
        // door for a `GenerationTip` kind — is
        // `conformance_account_state_walk.rs`'s step-7 section.
        trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(Vec::new()),
        attested_predecessors: Default::default(),
        linked_nests: None,
        owed_nests: None,
        peer_transport: None,
        enrollment_target_device_id: device_row(device),
    }
}

fn moderation_bytes(words: &[&str]) -> Vec<u8> {
    canonical_encode(&ModerationConfig {
        muted_keywords: words.iter().map(|w| (*w).into()).collect(),
        ..Default::default()
    })
    .unwrap()
}

/// Positive-wait budget (convention 14: a named generous ceiling + deadline
/// poll — green runs pay only the actual latency, and no assertion depends on
/// load staying low).
const CONVERGENCE_BUDGET: Duration = Duration::from_secs(30);

async fn eventually<F, Fut>(what: &str, probe: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    eventually_or(what, probe, || async { String::new() }).await;
}

/// [`eventually`] with a failure diagnostic: on a blown budget the panic
/// carries `diagnose()`'s report, so a timeout names the state it died in
/// (convention 6 — failures must diagnose themselves) instead of only the
/// budget it exhausted.
async fn eventually_or<F, Fut, D, DFut>(what: &str, mut probe: F, diagnose: D)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
    D: FnOnce() -> DFut,
    DFut: Future<Output = String>,
{
    if tokio::time::timeout(CONVERGENCE_BUDGET, async {
        while !probe().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_err()
    {
        let report = diagnose().await;
        panic!("eventually({what}): not reached within budget. Diagnostic: {report}");
    }
}

/// The session's push stream, as `NestClient::subscribe_pushes` hands it to
/// the runtime: decode each frame off the account's real WS subscription and
/// broadcast the typed event. This is exactly what the client's push broker
/// does between the socket and every subscriber; what a runtime DOES with an
/// event — the push→nudge mapping (`nudge_scope_for_push`), the wake — is the
/// production pump's own arm (`AccountRuntimeParams::pushes`), not glue
/// reproduced here. Everything upstream — handler, fan-out, frame encoding —
/// is the production path too.
fn push_source(
    mut push_rx: tokio::sync::mpsc::Receiver<Bytes>,
) -> (
    tokio::sync::broadcast::Receiver<PushEvent>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = tokio::sync::broadcast::channel(64);
    let forwarder = tokio::spawn(async move {
        while let Some(bytes) = push_rx.recv().await {
            let Ok(Frame::Push(push)) = decode_frame(&bytes) else {
                continue;
            };
            if tx
                .send(PushEvent::from_push(&push.kind, push.payload))
                .is_err()
            {
                break; // the runtime is gone
            }
        }
    });
    (rx, forwarder)
}

// ── V1: put on A → real push nudge → B's pump → B's read ────────────────────

#[tokio::test]
async fn v1_two_runtimes_converge_through_the_scope_tagged_nudge() {
    let (rpc, state, tmp) = nest().await;
    let base: PathBuf = tmp.path().to_path_buf();

    // Subscribe the account's push connection BEFORE any write, exactly as an
    // app's WS session exists before its user acts.
    let (_conn, push_rx) = state.ws.subscribe(actor_bytes());

    let a = AccountStoreRuntime::start(params(&base, "a", &rpc))
        .await
        .expect("start A");
    // B's runtime owns the push arm: the session's push stream is a param,
    // and the pump maps each `fauna.sync.changed` to the scope it wakes.
    // Red-verified: with `pushes: None`, the convergence poll times out —
    // the green below rides the nudge chain, nothing else.
    let (pushes, forwarder) = push_source(push_rx);
    let mut b_params = params(&base, "b", &rpc);
    b_params.pushes = Some(pushes);
    let b = AccountStoreRuntime::start(b_params).await.expect("start B");

    // Causal barrier (convention 14): B's prologue has finished —
    // `reconcile_now` is pass-bound, served only once the prologue has ended,
    // and runs a pass of its own — and its walk saw an EMPTY feed. Whatever B
    // serves after this point cannot be prologue catch-up; with the backstop
    // disarmed and no reconnect arm, the nudge chain is the only path left.
    let barrier = b.reconcile_now().await.expect("B's prologue barrier");
    assert_eq!(
        barrier.walk.expect("barrier walk ran").applied,
        0,
        "the feed is empty before A writes"
    );

    // The app-side write, through the handle only.
    let value = moderation_bytes(&["lottery"]);
    a.put_preference(KIND_MODERATION, value.clone())
        .await
        .expect("put on A");

    // B converges. Backstop disarmed + no reconnect arm: the scope-tagged
    // nudge is the only path this value can travel.
    eventually_or(
        "B serves A's value through the nudge chain",
        || {
            let b = b.clone();
            let value = value.clone();
            async move {
                matches!(
                    b.get_preference(KIND_MODERATION).await,
                    Ok(Some(entry)) if entry.value == value
                )
            }
        },
        // The timeout's post-mortem, each clause chosen to split the chain:
        // an explicit pass that then serves the value ⇒ the data was on the
        // nest and B could import it, so the NUDGE leg (push → pump task →
        // nudge channel → walk) was dead; a pass with applied == 0 ⇒ A's
        // publish never reached the nest feed; pass errors ⇒ the walk itself
        // is failing repeatedly. A is read too: if A's own entry no longer
        // carries the value, the write was LOST (superseded), not merely
        // undelivered.
        || {
            let a = a.clone();
            let b = b.clone();
            let rpc = Arc::clone(&rpc);
            let base = base.clone();
            async move {
                let report = b.reconcile_now().await;
                let after = b.get_preference(KIND_MODERATION).await;
                let a_view = a.get_preference(KIND_MODERATION).await;
                let feed: Result<SyncChangesListReply, _> = rpc
                    .request(
                        "fauna.sync.changes.list",
                        SyncChangesListRequest {
                            since: 0,
                            item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
                            scope: Some(ACCOUNT_STATE_SCOPE.to_string()),
                            frontier: Some(Default::default()),
                            ..Default::default()
                        },
                    )
                    .await;
                let feed = feed.map(|r| {
                    r.changes
                        .iter()
                        .map(|c| format!("{c:?}"))
                        .collect::<Vec<_>>()
                });
                // The journal is the authority: dump both stores' state-scope
                // rows + writer identity straight off the DB files.
                let dump = |device: &str| -> String {
                    let db_path = base
                        .join(device)
                        .join("state")
                        .join(account().actor_id_hex())
                        .join("account-store")
                        .join("account-store.db");
                    let conn = match rusqlite::Connection::open(&db_path) {
                        Ok(c) => c,
                        Err(e) => return format!("open {db_path:?}: {e}"),
                    };
                    let writer: String = conn
                        .query_row(
                            "SELECT hex(value) FROM store_meta WHERE key='writer_id'",
                            [],
                            |r| r.get(0),
                        )
                        .unwrap_or_else(|e| format!("? ({e})"));
                    let mut journal = Vec::new();
                    if let Ok(mut stmt) = conn.prepare(
                        "SELECT hex(writer_id), writer_seq, op FROM journal \
                         WHERE scope='state' ORDER BY writer_id, writer_seq",
                    ) && let Ok(rows) = stmt.query_map([], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    }) {
                        for row in rows.flatten() {
                            journal.push(format!("{}#{} {}", &row.0[..8], row.1, row.2));
                        }
                    }
                    format!("writer={} journal(state)={journal:?}", &writer[..8])
                };
                let a_store = dump("a");
                let b_store = dump("b");
                format!(
                    "B's explicit pass: {report:?}; B reads after pass: {after:?}; \
                     A reads: {a_view:?}; nest feed(state): {feed:?}; \
                     A store: {a_store}; B store: {b_store}"
                )
            }
        },
    )
    .await;

    a.shutdown().await;
    b.shutdown().await;
    forwarder.abort();
}

// ── V4: a delivery enters the seen-set; a sibling converges with no echo ────

/// The auto-in-set seen-set producer end to end (charter § The replica
/// boundary — R1's creation/delivery classes; the T1 producer decomposition):
/// a post record the nest sequenced enters the account's seen-set on the next
/// pump pass — watermark = the accounted frontier, no render event anywhere —
/// and a second device converges through the ordinary class-2 walk **without
/// publishing a row of its own**: its raise is covered by the merged entry,
/// and a second live feed row is exactly the churn the covered-raise skip
/// exists to prevent (the nest collapses per `(item, writer)`, so an echoing
/// sibling would mint one).
#[tokio::test]
async fn v4_a_delivery_enters_the_seen_set_and_a_sibling_converges_without_echo() {
    let (rpc, state, tmp) = nest().await;

    // One real post record, written by the nest's own append path — the
    // account's own creation, in-set by R1 with no render involved.
    fauna_nest::segments::post::append_body(
        &state.post_segments,
        &state.db,
        &actor_bytes(),
        b"first post",
        1_715_000_000,
    )
    .await
    .expect("append post body");
    let post_scope = ContentScope::new("post", actor_bytes())
        .expect("scope")
        .to_string();

    // Device A: one explicit pass (`reconcile_now` is pass-bound, so it runs
    // after the start prologue and the sequence is deterministic).
    let a = AccountStoreRuntime::start(params(tmp.path(), "a", &rpc))
        .await
        .expect("start a");
    a.reconcile_now().await.expect("a pass");
    let entry = a
        .states_of_kind(KIND_SEEN_SET)
        .await
        .expect("states")
        .into_iter()
        .find(|e| e.key == post_scope)
        .expect("a seen-set entry for the post scope");
    let set: SeenScopeSet = canonical_decode(&entry.value).expect("decode seen-set");
    assert!(
        set.contains(&WriterId::NEST_SEQUENCER.0, 1),
        "the record's coordinate is in-set with no render event"
    );
    assert!(set.refs.is_empty(), "auto-in-set is watermark-only");

    // Device B converges through the class-2 walk (reconcile runs before the
    // producer step in one pass, so the merge lands first and B's own raise
    // finds itself covered)...
    let b = AccountStoreRuntime::start(params(tmp.path(), "b", &rpc))
        .await
        .expect("start b");
    b.reconcile_now().await.expect("b pass");
    let entry = b
        .states_of_kind(KIND_SEEN_SET)
        .await
        .expect("states")
        .into_iter()
        .find(|e| e.key == post_scope)
        .expect("b holds the merged seen-set entry");
    let set: SeenScopeSet = canonical_decode(&entry.value).expect("decode seen-set");
    assert!(set.contains(&WriterId::NEST_SEQUENCER.0, 1));

    // ...and publishes nothing of its own: the account-state feed holds ONE
    // live state-entry row — A's.
    let reply: SyncChangesListReply = rpc
        .request(
            "fauna.sync.changes.list",
            SyncChangesListRequest {
                since: 0,
                item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
                scope: Some(ACCOUNT_STATE_SCOPE.to_string()),
                frontier: Some(Default::default()),
                ..Default::default()
            },
        )
        .await
        .expect("list state rows");
    assert_eq!(
        reply.changes.len(),
        1,
        "one writer published the seen-set; a converged sibling stays silent"
    );

    a.shutdown().await;
    b.shutdown().await;
}

// ── A walk that takes a delegable row whole writes none ─────────────────────

/// The nest's live rows of the delegable scope, counted by origin writer.
async fn delegable_census(state: &AppState) -> std::collections::BTreeMap<Vec<u8>, usize> {
    let Some(folder) = state
        .db
        .find_state_scope(&actor_bytes(), ACCOUNT_STATE_SCOPE)
        .await
        .unwrap()
    else {
        return Default::default();
    };
    let read = state
        .db
        .get_account_state_changes(folder, 0, &std::collections::BTreeMap::new(), None, None)
        .await
        .unwrap();
    let mut by_writer = std::collections::BTreeMap::new();
    for row in read.rows {
        *by_writer
            .entry(row.origin_writer.expect("a device-writer row"))
            .or_insert(0) += 1;
    }
    by_writer
}

/// The rows each writer holds in `now` beyond `base` — the writers a step
/// made contributors, and how many rows each added.
fn census_growth(
    base: &std::collections::BTreeMap<Vec<u8>, usize>,
    now: &std::collections::BTreeMap<Vec<u8>, usize>,
) -> std::collections::BTreeMap<Vec<u8>, usize> {
    now.iter()
        .filter_map(|(w, n)| {
            let grew = n.saturating_sub(base.get(w).copied().unwrap_or(0));
            (grew > 0).then(|| (w.clone(), grew))
        })
        .collect()
}

/// **A device that only adopts its siblings' read markers holds no row of
/// the delegable scope** (`delegable-scope-reclamation.md` § Delegable-scope
/// reclamation, part (1): a walk that takes a row whole writes none — the
/// 2026-10-01 measurement (c) shape) — **and the scope settles at one row per
/// item** (part (2): each raise names the rows it covers). Three devices, five
/// channels: A raises every marker to 1, B raises them to 2, and C — which
/// raised nothing — converges on 2 everywhere while the scope holds B's five
/// rows alone. Only C's own raise to 3 makes it a contributor, and then the
/// scope holds C's five alone.
///
/// Red-verified: before the merge arms answered `Replace` for a join equal
/// to the served row, C re-authored every adopted marker (15 rows after B's
/// raise, five under C); before a put named the rows it covers, A's five
/// stale rows stayed beside B's (10 rows, then 15).
#[tokio::test]
async fn a_device_that_only_adopts_read_markers_writes_no_row_of_its_own() {
    let (rpc, state, tmp) = nest_with_escrow().await;
    let base: PathBuf = tmp.path().to_path_buf();
    let channels: Vec<String> = (1u8..=5).map(|i| hex::encode([i; 32])).collect();

    let mut devices = Vec::new();
    for name in ["a", "b", "c"] {
        devices.push(
            AccountStoreRuntime::start(params_trusting_deployment(&base, name, &rpc))
                .await
                .expect("start device"),
        );
    }
    let pass_all_twice = || {
        let devices = devices.clone();
        async move {
            for _ in 0..2 {
                for d in &devices {
                    d.reconcile_now().await.expect("pass");
                }
            }
        }
    };
    let converge_on = |through: u64| {
        let devices = devices.clone();
        let channels = channels.clone();
        async move {
            eventually("every device reads every channel's marker", || {
                let devices = devices.clone();
                let channels = channels.clone();
                async move {
                    for d in &devices {
                        d.reconcile_now().await.expect("pass");
                    }
                    for d in &devices {
                        let held: std::collections::BTreeMap<String, u64> = d
                            .read_markers()
                            .await
                            .expect("markers")
                            .into_iter()
                            .collect();
                        if channels.iter().any(|c| held.get(c) != Some(&through)) {
                            return false;
                        }
                    }
                    true
                }
            })
            .await;
        }
    };
    let (a, b, c) = (&devices[0], &devices[1], &devices[2]);

    pass_all_twice().await;
    let baseline = delegable_census(&state).await;

    // A raises every marker to 1; the others adopt.
    for ch in &channels {
        assert!(a.raise_read_marker(ch, 1).await.expect("A raises"));
    }
    converge_on(1).await;
    pass_all_twice().await;
    let after_a = census_growth(&baseline, &delegable_census(&state).await);
    assert_eq!(
        after_a.values().copied().collect::<Vec<_>>(),
        vec![5],
        "A alone wrote, one row per channel: {after_a:?}"
    );

    // B raises them to 2; A and C adopt B's rows and author none.
    for ch in &channels {
        assert!(b.raise_read_marker(ch, 2).await.expect("B raises"));
    }
    converge_on(2).await;
    pass_all_twice().await;
    let after_b = census_growth(&baseline, &delegable_census(&state).await);
    let live_b = delegable_census(&state).await;
    assert_eq!(
        (after_b.len(), after_b.values().sum::<usize>()),
        (1, 5),
        "B's five, which named A's five — C, which raised nothing, holds \
         none: {after_b:?}"
    );

    // C raises them to 3: now it is a contributor.
    for ch in &channels {
        assert!(c.raise_read_marker(ch, 3).await.expect("C raises"));
    }
    converge_on(3).await;
    pass_all_twice().await;
    let after_c = census_growth(&baseline, &delegable_census(&state).await);
    assert_eq!(
        (after_c.len(), after_c.values().sum::<usize>()),
        (1, 5),
        "one row per channel, the writer that just raised: {after_c:?}"
    );
    assert!(
        after_b.keys().all(|w| !after_c.contains_key(w)),
        "the contributor is the writer that just raised"
    );
    assert_eq!(
        delegable_census(&state).await.values().sum::<usize>(),
        live_b.values().sum::<usize>(),
        "the scope's live rows did not grow with C's raise"
    );

    for d in devices {
        d.shutdown().await;
    }
}

// ── One live row per item: the cover, the hand-over, the retire below ──────

/// A runtime's fleet id — its writer on the delegable scope.
async fn fleet_id(handle: &AccountStoreHandle) -> [u8; 32] {
    handle
        .principal_bundle_status()
        .await
        .expect("status")
        .device_authorization
        .expect("the prologue minted the grant")
        .device_key
}

/// Every channel's marker reads `through` on `d`, and the moderation record
/// reads `words`.
async fn reads_everything(
    d: &AccountStoreHandle,
    channels: &[String],
    through: u64,
    words: &[&str],
) -> bool {
    let held: std::collections::BTreeMap<String, u64> = d
        .read_markers()
        .await
        .expect("markers")
        .into_iter()
        .collect();
    let record = d
        .get_preference(KIND_MODERATION)
        .await
        .expect("record")
        .map(|e| e.value);
    channels.iter().all(|c| held.get(c) == Some(&through))
        && record.as_deref() == Some(moderation_bytes(words).as_slice())
}

/// **A removed device's rows leave the delegable scope, and no item is lost**
/// (`delegable-scope-reclamation.md` § Delegable-scope reclamation, parts
/// (3) and (4); the 2026-10-01 measurement (a)). A and B settle; B raises
/// five markers only it raises and writes the moderation record; both raise
/// five shared markers in turn. B is shut down and A removes it. A's passes
/// hand over the items only B's rows carried and retire every row of B's
/// below a cover: no row under B is left, and a fresh device reads every
/// marker and the record.
///
/// Red-verified: before the cover step, all of B's rows stayed live and A
/// wrote none of B's sole items.
#[tokio::test]
async fn a_removed_devices_rows_leave_the_scope_and_no_item_is_lost() {
    let (rpc, state, tmp) = nest_with_escrow().await;
    let base: PathBuf = tmp.path().to_path_buf();
    let only_b: Vec<String> = (1u8..=5).map(|i| hex::encode([i; 32])).collect();
    let shared: Vec<String> = (6u8..=10).map(|i| hex::encode([i; 32])).collect();
    // A is in every channel it hands a marker over for (part (7)).
    let channels: Vec<[u8; 32]> = (1u8..=10).map(|i| [i; 32]).collect();
    let a = AccountStoreRuntime::start(params_in_channels(&base, "a", &rpc, channels))
        .await
        .expect("start a");
    let b = AccountStoreRuntime::start(params_trusting_deployment(&base, "b", &rpc))
        .await
        .expect("start b");
    for _ in 0..2 {
        a.reconcile_now().await.expect("a passes");
        b.reconcile_now().await.expect("b passes");
    }
    for ch in &only_b {
        assert!(b.raise_read_marker(ch, 1).await.expect("B raises"));
    }
    b.put_preference(KIND_MODERATION, moderation_bytes(&["witness"]))
        .await
        .expect("B writes the record");
    for ch in &shared {
        assert!(a.raise_read_marker(ch, 1).await.expect("A raises"));
    }
    for _ in 0..2 {
        a.reconcile_now().await.expect("a passes");
        b.reconcile_now().await.expect("b passes");
    }
    for ch in &shared {
        assert!(b.raise_read_marker(ch, 2).await.expect("B raises"));
    }
    for _ in 0..2 {
        b.reconcile_now().await.expect("b passes");
        a.reconcile_now().await.expect("a passes");
    }
    let (a_id, b_id) = (fleet_id(&a).await, fleet_id(&b).await);
    let before = delegable_census(&state).await;
    assert!(
        before.get(b_id.as_slice()).is_some_and(|n| *n >= 11),
        "B carries its five, the shared five and the record: {before:?}"
    );
    b.shutdown().await;
    a.remove_fleet_member(b_id).await.expect("A removes B");

    let mut census = Default::default();
    for _ in 0..4 {
        a.reconcile_now().await.expect("a passes");
        census = delegable_census(&state).await;
        if !census.contains_key(b_id.as_slice()) {
            break;
        }
    }
    assert!(
        !census.contains_key(b_id.as_slice()),
        "no row under the removed writer: {census:?}"
    );
    assert_eq!(
        census.keys().cloned().collect::<Vec<_>>(),
        vec![a_id.to_vec()],
        "one writer left, so one row per item: {census:?}"
    );
    let fresh = AccountStoreRuntime::start(params_trusting_deployment(&base, "fresh", &rpc))
        .await
        .expect("start a fresh device");
    let (b_only, both) = (only_b.clone(), shared.clone());
    eventually("a fresh device reads every marker and the record", || {
        let fresh = fresh.clone();
        let (b_only, both) = (b_only.clone(), both.clone());
        async move {
            fresh.reconcile_now().await.expect("pass");
            reads_everything(&fresh, &b_only, 1, &["witness"]).await
                && reads_everything(&fresh, &both, 2, &["witness"]).await
        }
    })
    .await;

    a.shutdown().await;
    fresh.shutdown().await;
}

/// **A removed device's sole items are handed over at a full scope, the
/// count unchanged** (`delegable-scope-reclamation.md` § Delegable-scope
/// reclamation, part (3) and "A full scope is owed a put that needs no
/// room"). B raises five markers only it raises and writes the moderation
/// record; A removes it; the scope is then filled to its cap with rows no
/// device opens. A's hand-overs name B's rows, so the nest supersedes each
/// in the put's own transaction: every hand-over lands, no row under B is
/// left, the live count is the cap throughout, and A's six rows carry every
/// item B's alone did. The filler rows, which no device can open, are all
/// kept.
#[tokio::test]
async fn a_removed_devices_sole_items_are_handed_over_at_a_full_scope() {
    use fauna_protocol::account_state::MAX_STATE_ENTRIES_PER_SCOPE;
    let (rpc, state, tmp) = nest_with_escrow().await;
    let base: PathBuf = tmp.path().to_path_buf();
    let only_b: Vec<String> = (1u8..=5).map(|i| hex::encode([i; 32])).collect();
    // A is in every channel it hands a marker over for (part (7)).
    let channels: Vec<[u8; 32]> = (1u8..=5).map(|i| [i; 32]).collect();
    let a = AccountStoreRuntime::start(params_in_channels(&base, "a", &rpc, channels))
        .await
        .expect("start a");
    let b = AccountStoreRuntime::start(params_trusting_deployment(&base, "b", &rpc))
        .await
        .expect("start b");
    for _ in 0..2 {
        a.reconcile_now().await.expect("a passes");
        b.reconcile_now().await.expect("b passes");
    }
    for ch in &only_b {
        assert!(b.raise_read_marker(ch, 1).await.expect("B raises"));
    }
    b.put_preference(KIND_MODERATION, moderation_bytes(&["witness"]))
        .await
        .expect("B writes the record");
    for _ in 0..2 {
        b.reconcile_now().await.expect("b passes");
        a.reconcile_now().await.expect("a passes");
    }
    let (a_id, b_id) = (fleet_id(&a).await, fleet_id(&b).await);
    b.shutdown().await;
    a.remove_fleet_member(b_id).await.expect("A removes B");

    let folder = state
        .db
        .find_state_scope(&actor_bytes(), ACCOUNT_STATE_SCOPE)
        .await
        .unwrap()
        .expect("the scope exists");
    let live = delegable_census(&state).await.values().sum::<usize>() as i64;
    let filler = [0xEE; 32];
    for i in live..MAX_STATE_ENTRIES_PER_SCOPE {
        let mut item = [0u8; 32];
        item[..8].copy_from_slice(&i.to_be_bytes());
        state
            .db
            .record_account_state_entry(
                &actor_bytes(),
                folder,
                &item,
                &filler,
                i + 1,
                "state-put",
                b"a filler row no device opens",
                None,
                &[],
            )
            .await
            .unwrap()
            .expect("room under the cap");
    }
    let cap = MAX_STATE_ENTRIES_PER_SCOPE as usize;
    assert_eq!(delegable_census(&state).await.values().sum::<usize>(), cap);

    let mut census = Default::default();
    for _ in 0..4 {
        let report = a.reconcile_now().await.expect("a passes");
        census = delegable_census(&state).await;
        assert_eq!(
            census.values().sum::<usize>(),
            cap,
            "the live count is the cap after every pass: {:?}",
            report.errors
        );
        if !census.contains_key(b_id.as_slice()) {
            break;
        }
    }
    assert!(
        !census.contains_key(b_id.as_slice()),
        "every row under the removed writer was handed over: {census:?}"
    );
    assert_eq!(
        census.get(filler.as_slice()).copied(),
        Some(cap - 6),
        "no filler row, which no device opens, was taken: {census:?}"
    );
    // A wrote nothing of its own before the removal: its six rows are the
    // hand-overs of B's five markers and the record, each a member's row
    // carrying what B's alone did. (A fresh device's walk of the 4,096-row
    // scope is the uncapped proof's read, not repeated here.)
    assert_eq!(
        census.get(a_id.as_slice()).copied(),
        Some(6),
        "every item B alone carried is handed over: {census:?}"
    );
    assert!(reads_everything(&a, &only_b, 1, &["witness"]).await);
    a.shutdown().await;
}

/// **One machine, four lives, one row per item** (`delegable-scope-reclamation.md`
/// § Delegable-scope reclamation; the 2026-10-01 measurement (b): 11, 17, 23
/// and 29 rows). The same machine signs in four times — each life over an
/// empty store and slot, the same enrollment target — and each advances the
/// same five markers and the moderation record. After the last life's passes
/// the scope holds one row per item, every one under the last life's writer.
#[tokio::test]
async fn four_lives_of_one_machine_leave_one_row_per_item() {
    let (rpc, state, tmp) = nest_with_escrow().await;
    let base: PathBuf = tmp.path().to_path_buf();
    let channels: Vec<String> = (1u8..=5).map(|i| hex::encode([i; 32])).collect();
    let mut last = None;
    for life in 1u64..=4 {
        let device = format!("life{life}");
        let d = AccountStoreRuntime::start(AccountRuntimeParams {
            enrollment_target_device_id: device_row("life1"),
            ..params_trusting_deployment(&base, &device, &rpc)
        })
        .await
        .expect("this life signs in");
        d.settled().await;
        eventually("this life reads the last life's markers", || {
            let d = d.clone();
            let channels = channels.clone();
            async move {
                d.reconcile_now().await.expect("pass");
                let held: std::collections::BTreeMap<String, u64> = d
                    .read_markers()
                    .await
                    .expect("markers")
                    .into_iter()
                    .collect();
                channels
                    .iter()
                    .all(|c| held.get(c).copied().unwrap_or(0) == life - 1)
            }
        })
        .await;
        for ch in &channels {
            assert!(d.raise_read_marker(ch, life).await.expect("raise"));
        }
        d.put_preference(KIND_MODERATION, moderation_bytes(&[device.as_str()]))
            .await
            .expect("this life writes the record");
        eventually("this life's rows are published", || {
            let d = d.clone();
            async move {
                let report = d.reconcile_now().await.expect("pass");
                report.errors.is_empty() && report.fleet_published == Some(0)
            }
        })
        .await;
        if life < 4 {
            let retirement = d.shutdown_for_sign_out().await;
            assert!(
                matches!(
                    retirement,
                    fauna_sync_engine::account_runtime::EnrollmentRetirement::Retired { .. }
                ),
                "life {life}: the sign-out retires the principal: {retirement:?}"
            );
        } else {
            last = Some(d);
        }
    }
    let d = last.unwrap();
    let writer = fleet_id(&d).await;
    let mut census = Default::default();
    for _ in 0..4 {
        d.reconcile_now().await.expect("pass");
        census = delegable_census(&state).await;
        if census.keys().all(|w| w.as_slice() == writer.as_slice()) {
            break;
        }
    }
    assert_eq!(
        census,
        [(writer.to_vec(), 6)].into(),
        "five markers and the record, one row each, under the last life: {census:?}"
    );
    let fresh = AccountStoreRuntime::start(params_trusting_deployment(&base, "fresh", &rpc))
        .await
        .expect("start a fresh device");
    let all = channels.clone();
    eventually("a fresh device reads the last life's items", || {
        let fresh = fresh.clone();
        let all = all.clone();
        async move {
            fresh.reconcile_now().await.expect("pass");
            reads_everything(&fresh, &all, 4, &["life4"]).await
        }
    })
    .await;
    d.shutdown().await;
    fresh.shutdown().await;
}

// ── V6: a reported body enters the seen-set itemized; a sibling converges ────

/// The MLS channel this account is a member of. Opaque 32 bytes — the nest
/// keys on it and never interprets it (a conv scope id is the channel, never
/// an actor).
const V6_CHANNEL: [u8; 32] = [0x7C; 32];

/// The group creator who delivers the Welcome that admits this account.
fn v6_creator() -> [u8; 32] {
    ActorKeypair::from_secret([0xC5; 32]).actor_id().0
}

/// The same in-process nest as [`nest`], plus the conversations handlers, with
/// this account admitted to [`V6_CHANNEL`] through the REAL Welcome-delivery
/// kind (that is what writes the `actor_channels` roster row the content-feed
/// door's admission checks) and `bodies` appended through the production
/// `segments::conv::append` path.
///
/// Returns the record digests in append order — derived here exactly as
/// `fauna_conversations::plane` derives them client-side, which is the point:
/// an app that reports a body names the record by a digest it computed itself,
/// and it has to be the one the nest sequenced.
async fn nest_with_conv_membership(
    bodies: &[&[u8]],
) -> (
    Arc<RouterRequester>,
    Arc<AppState>,
    tempfile::TempDir,
    Vec<[u8; 32]>,
) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let blob_dir = tmp.path().join("blob");
    std::fs::create_dir_all(&blob_dir).unwrap();
    let backup_svc = Arc::new(BackupService::new(db.clone(), None, false, blob_dir, None).unwrap());
    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        ..AppState::for_test(db)
    });
    for actor in [actor_bytes(), v6_creator()] {
        state.db.create_user(&actor, "free", "test").await.unwrap();
    }
    common::seed_dispatch_actor(&state.db, &actor_bytes()).await;
    // An accepted contact lets the creator's DM Welcome flow the reach floor.
    state
        .db
        .upsert_contact(&actor_bytes(), &v6_creator(), "accepted")
        .await
        .unwrap();

    let requester = |actor: [u8; 32]| {
        let mut b = RpcRouter::builder();
        folder_handlers::register_folders_handlers(&mut b);
        sync_handlers::register_sync_handlers(&mut b);
        // `fauna.capabilities.reconcile` — every full pass enumerates it.
        fauna_nest::bridge_blob_handlers::register_capability_handlers(&mut b);
        fauna_nest::conversations_handlers::register_conversations_handlers(&mut b);
        RouterRequester {
            router: b.build(),
            state: Arc::clone(&state),
            actor,
        }
    };
    let _: fauna_protocol::conversations::WelcomeDeliverReply = requester(v6_creator())
        .request(
            "fauna.conversations.welcome.deliver",
            fauna_protocol::conversations::WelcomeDeliverRequest {
                recipient_actor_id: hex::encode(actor_bytes()),
                channel_id: hex::encode(V6_CHANNEL),
                welcome_bytes: vec![],
                kind: fauna_protocol::conversations::WelcomeKind::Dm,
                nest_url: None,
                extra: Default::default(),
            },
        )
        .await
        .expect("welcome delivery");
    assert!(
        state
            .db
            .is_actor_in_channel(&actor_bytes(), &V6_CHANNEL)
            .await
            .unwrap(),
        "fixture: the Welcome must have registered this account on the roster",
    );

    let mut digests = Vec::new();
    for (i, body) in bodies.iter().enumerate() {
        fauna_nest::segments::conv::append(
            &state.conv_segments,
            &state.db,
            &V6_CHANNEL,
            body,
            1_715_900_000_000 + i as i64,
        )
        .await
        .expect("append conv record");
        // The record's filing digest — the content hash of its envelope, minted
        // through the same shared fn the nest filed under (no seq: it left the
        // pre-image with the 2026-08-17 record-identity cutover).
        digests.push(
            fauna_mls::segments::derive_record_cid(body)
                .expect("derive conv record cid")
                .digest(),
        );
    }
    (Arc::new(requester(actor_bytes())), state, tmp, digests)
}

/// [`params`], plus a membership source naming [`V6_CHANNEL`] — the production
/// shape (tui reads it off the live MLS session), so the runtime derives and
/// walks `content:conv:<channel>` on its own.
fn params_in_channel(
    base: &Path,
    device: &str,
    rpc: &Arc<RouterRequester>,
) -> AccountRuntimeParams<Arc<RouterRequester>> {
    AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        memberships: Some(Arc::new(|| Some(vec![V6_CHANNEL]))),
        ..params(base, device, rpc)
    }
}

/// **The T1 browse trigger end to end** (charter § The replica boundary → T1
/// and its producer decomposition): the flow that had no caller until an app
/// could report a rendered body.
///
/// Two records sit on the channel's feed and both are walked in, so the replica
/// could have credited either. The app reports **one** of them — the one it
/// painted — through the same `AccountStoreHandle::record_observation` tui's
/// shell calls, and the assertions are what separate T1 from a watermark:
///
/// - the reported record's coordinate is in-set;
/// - the *other* record, equally walked and equally present, is NOT — reading
///   one message in a channel earns no claim over the prefix below it;
/// - the entry carries no watermark at all.
///
/// Then the sibling device converges on it through the ordinary class-2 walk,
/// which is what makes the seen-set an account-level boundary rather than a
/// per-device one: read a message on your laptop, and your phone knows.
#[tokio::test]
async fn v6_a_reported_body_enters_the_seen_set_itemized_and_the_sibling_converges() {
    let (rpc, _state, tmp, digests) =
        nest_with_conv_membership(&[b"first message", b"second message"]).await;
    let conv_scope = ContentScope::new("conv", V6_CHANNEL)
        .expect("scope")
        .to_string();

    let a = AccountStoreRuntime::start(params_in_channel(tmp.path(), "a", &rpc))
        .await
        .expect("start a");
    // The walk that gives the reported record a coordinate to resolve to. An
    // observation ahead of it would be `Unresolved` by design (dropped, never
    // queued) — this test is about the resolved path.
    a.reconcile_now().await.expect("a pass");

    // The app's report: this body was handed to a visible view. Exactly the
    // call tui's shell makes for a `dm-message-text` the viewport painted.
    let reported = fauna_sync_engine::observation_intake::Observation::parse(
        &conv_scope,
        &hex::encode(digests[1]),
    )
    .expect("the app's plane ref parses");
    assert_eq!(
        a.record_observation(reported).await.expect("report"),
        fauna_sync_engine::observation_intake::ObservationOutcome::Recorded,
    );

    let entry = a
        .states_of_kind(KIND_SEEN_SET)
        .await
        .expect("states")
        .into_iter()
        .find(|e| e.key == conv_scope)
        .expect("a seen-set entry for the channel scope");
    let set: SeenScopeSet = canonical_decode(&entry.value).expect("decode seen-set");
    assert!(
        set.contains(&WriterId::NEST_SEQUENCER.0, 2),
        "the rendered record's own coordinate is in-set: {set:?}",
    );
    assert!(
        set.watermarks.is_empty(),
        "browse content earns no watermark — only the record whose body was \
         shown is in-set: {set:?}",
    );
    assert_eq!(
        set.refs.len(),
        1,
        "one body rendered, one ref — the unrendered record stays out: {set:?}",
    );

    // The sibling converges through the ordinary class-2 walk — once `a`'s
    // publish step, armed by the local write, has shipped the entry.
    a.settled().await;
    let b = AccountStoreRuntime::start(params_in_channel(tmp.path(), "b", &rpc))
        .await
        .expect("start b");
    b.reconcile_now().await.expect("b pass");
    let entry = b
        .states_of_kind(KIND_SEEN_SET)
        .await
        .expect("states")
        .into_iter()
        .find(|e| e.key == conv_scope)
        .expect("b holds the merged seen-set entry");
    let merged: SeenScopeSet = canonical_decode(&entry.value).expect("decode seen-set");
    assert_eq!(
        merged.refs, set.refs,
        "the observation A recorded is the one B converged on",
    );

    a.shutdown().await;
    b.shutdown().await;
}

/// **A left conversation's rows leave the nest, and a re-join writes them
/// back** (`delegable-scope-reclamation.md` § Delegable-scope reclamation,
/// parts (6) and (7)), on the real handlers. The device reads in the channel
/// (its read marker and its seen-set entry: two live rows), leaves it (the
/// membership source stops naming it: the departure pass concludes it and the
/// cover step retires both rows, so the nest's live count falls by two and
/// later passes send nothing), then re-joins: the entries it kept are written
/// back, and a fresh device reads the marker at its old position.
#[tokio::test]
async fn a_left_conversations_rows_leave_the_nest_and_a_re_join_writes_them_back() {
    let (rpc, state, tmp, digests) = nest_with_conv_membership(&[b"first message"]).await;
    let conv_scope = ContentScope::new("conv", V6_CHANNEL)
        .expect("scope")
        .to_string();
    let channel_hex = hex::encode(V6_CHANNEL);
    let joined: Arc<std::sync::Mutex<Vec<[u8; 32]>>> =
        Arc::new(std::sync::Mutex::new(vec![V6_CHANNEL]));
    let source = Arc::clone(&joined);
    let a = AccountStoreRuntime::start(AccountRuntimeParams {
        memberships: Some(Arc::new(move || Some(source.lock().unwrap().clone()))),
        ..params_in_channel(tmp.path(), "a", &rpc)
    })
    .await
    .expect("start a");
    a.reconcile_now().await.expect("a pass");
    let reported = fauna_sync_engine::observation_intake::Observation::parse(
        &conv_scope,
        &hex::encode(digests[0]),
    )
    .expect("the plane ref parses");
    assert_eq!(
        a.record_observation(reported).await.expect("report"),
        fauna_sync_engine::observation_intake::ObservationOutcome::Recorded,
    );
    assert!(a.raise_read_marker(&channel_hex, 1).await.expect("raise"));
    a.settled().await;
    a.reconcile_now().await.expect("a pass");
    let a_id = fleet_id(&a).await;
    let total =
        |census: &std::collections::BTreeMap<Vec<u8>, usize>| census.values().sum::<usize>();
    let in_channel = delegable_census(&state).await;
    assert_eq!(
        in_channel.get(a_id.as_slice()),
        Some(&2),
        "the marker and the seen-set entry: {in_channel:?}"
    );

    joined.lock().unwrap().clear();
    for _ in 0..3 {
        a.reconcile_now().await.expect("a pass");
    }
    let left = delegable_census(&state).await;
    assert_eq!(
        total(&left) + 2,
        total(&in_channel),
        "the nest's live count fell by the two rows: {in_channel:?} → {left:?}"
    );
    assert!(!left.contains_key(a_id.as_slice()), "{left:?}");
    assert!(
        a.read_markers()
            .await
            .expect("markers")
            .contains(&(channel_hex.clone(), 1)),
        "the device keeps the entry"
    );

    *joined.lock().unwrap() = vec![V6_CHANNEL];
    for _ in 0..2 {
        a.reconcile_now().await.expect("a pass");
    }
    let rejoined = delegable_census(&state).await;
    assert_eq!(
        rejoined.get(a_id.as_slice()),
        Some(&2),
        "both items written back: {rejoined:?}"
    );
    let fresh = AccountStoreRuntime::start(params_in_channel(tmp.path(), "fresh", &rpc))
        .await
        .expect("start fresh");
    fresh.reconcile_now().await.expect("fresh pass");
    assert!(
        fresh
            .read_markers()
            .await
            .expect("markers")
            .contains(&(channel_hex, 1)),
        "the re-join resumed at the old position"
    );
    a.shutdown().await;
    fresh.shutdown().await;
}

// ── V5: the device-endpoints writer — the fleet's first `GenerationTip` rows ─

/// The nest's deployment identity for the V5 leg — the escrow-receipt signer,
/// and (via [`params_trusting_deployment`]) the only holder this account
/// accepts. Production learns this key by TOFU pin, never from the nest's own
/// claim (`conformance_account_state_walk.rs` § step 7 is the plane-level
/// twin of this trust shape).
fn deployment_key() -> SigningKey {
    SigningKey::from_bytes(&[0x66; 32])
}

/// The same in-process nest as [`nest`], plus the generation-escrow doors and
/// the deployment identity that signs their receipts — what the writer door's
/// first-need mint (trigger (a)) deposits against.
async fn nest_with_escrow() -> (Arc<RouterRequester>, Arc<AppState>, tempfile::TempDir) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let blob_dir = tmp.path().join("blob");
    std::fs::create_dir_all(&blob_dir).unwrap();
    let backup_svc = Arc::new(BackupService::new(db.clone(), None, false, blob_dir, None).unwrap());
    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        nest_signing_key: Some(deployment_key()),
        ..AppState::for_test(db)
    });
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    // `fauna.capabilities.reconcile` — every full pass enumerates it.
    fauna_nest::bridge_blob_handlers::register_capability_handlers(&mut b);
    fauna_nest::generation_escrow_handlers::register_generation_escrow_handlers(&mut b);
    // V12's peer-leg bind gate fetches `fauna.nest.info` for the `peer-sync`
    // brake — the REAL handler, so the always-on advertisement
    // (`discovery_core::advertised_capabilities`) is what the gate sees.
    fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
    common::seed_dispatch_actor(&state.db, &actor_bytes()).await;
    (
        Arc::new(RouterRequester {
            router: b.build(),
            state: Arc::clone(&state),
            actor: actor_bytes(),
        }),
        state,
        tmp,
    )
}

/// [`params`] with the deployment identity as the trusted escrow holder — the
/// production trust shape (`AccountRuntimeParams::trusted_escrow_holders` is
/// the app's TOFU pin; tui supplies exactly this).
fn params_trusting_deployment(
    base: &Path,
    device: &str,
    rpc: &Arc<RouterRequester>,
) -> AccountRuntimeParams<Arc<RouterRequester>> {
    AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(vec![
            deployment_key().verifying_key().to_bytes(),
        ]),
        attested_predecessors: Default::default(),
        ..params(base, device, rpc)
    }
}

/// [`params_trusting_deployment`], plus a membership source naming
/// `channels` — the shape every host wires. A device hands over a read
/// marker only when its membership answer names the channel
/// (`delegable-scope-reclamation.md` § Delegable-scope reclamation, part
/// (7)).
fn params_in_channels(
    base: &Path,
    device: &str,
    rpc: &Arc<RouterRequester>,
    channels: Vec<[u8; 32]>,
) -> AccountRuntimeParams<Arc<RouterRequester>> {
    AccountRuntimeParams {
        memberships: Some(Arc::new(move || Some(channels.clone()))),
        ..params_trusting_deployment(base, device, rpc)
    }
}

/// The device-endpoints rows a runtime's store currently serves, decoded —
/// keyed by their logical key (the publishing device's writer id hex).
async fn endpoint_rows(handle: &AccountStoreHandle) -> Vec<(String, DeviceEndpoints)> {
    handle
        .states_of_kind(KIND_DEVICE_ENDPOINTS)
        .await
        .expect("states_of_kind")
        .into_iter()
        .filter(|e| !e.tombstone)
        .map(|e| {
            let v: DeviceEndpoints = canonical_decode(&e.value).expect("decode DeviceEndpoints");
            (e.key, v)
        })
        .collect()
}

/// **The pump publishes this device's registration-latch row as its
/// device-endpoints row statement** (`account-data-taxonomy.md` § The
/// generation machinery → *Fleet-scope reclamation*, clause (4), *The removal
/// target*, arm (b): the member stating the row is the target, whatever the
/// nest claims). That arm rests on one argument — the pump passes the latch
/// row into `ensure_published` — and every other pin either feeds the
/// statement by hand or never reads `enrolled_row`. This runs the real pump
/// over the v5 shape (a nest with a deployment escrow key, a runtime that
/// trusts it) and reads the entry the writer published. The tier_3 journey
/// `test_device_member_removal.py` witnesses the same feed end to end; this is
/// the tier_1 witness.
///
/// Red-verified: pass `None` at `account_driver/pass.rs`'s
/// `ensure_published` call and this alone reddens.
#[tokio::test]
async fn the_pump_publishes_the_latch_row_as_the_device_endpoints_statement() {
    let (rpc, _state, tmp) = nest_with_escrow().await;
    let a = AccountStoreRuntime::start(params_trusting_deployment(tmp.path(), "a", &rpc))
        .await
        .expect("start A");
    let statement = |a: AccountStoreHandle| async move {
        let rows = a
            .states_of_kind(KIND_DEVICE_ENDPOINTS)
            .await
            .expect("states_of_kind");
        rows.into_iter().find(|e| !e.tombstone).map(|e| {
            canonical_decode::<DeviceEndpointsEntry>(&e.value)
                .expect("decode DeviceEndpointsEntry")
                .enrolled_row
        })
    };
    eventually_or(
        "A publishes an entry stating its latch row",
        || {
            let a = a.clone();
            async move {
                a.reconcile_now().await.expect("A pass");
                statement(a).await == Some(Some(device_row("a")))
            }
        },
        || {
            let a = a.clone();
            async move { format!("statement={:?}", statement(a).await) }
        },
    )
    .await;
    a.shutdown().await;
}

/// **End to end** (charter § The peer
/// leg → *Discovery* (T5) + § The generation machinery, trigger (a)): with a
/// trusted escrow holder pinned and the real escrow doors registered, each
/// runtime's pump publishes this device's `fauna.state.device-endpoints` entry
/// through the fleet plane's writer door — the account's FIRST production
/// `GenerationTip` origination, so the door mints — and the sibling walks
/// `state-fleet` and OPENS the row under that generation.
///
/// The enrollment race is the deliberately hard half: A starts and mints
/// **alone**, so generation 1's member set is {A} and B — enrolled later —
/// can never key it. Convergence therefore requires two ST-007-shaped moves
/// this test refuses to fake: B heal-mints a successor tip covering both
/// devices ("no candidate for this observer" IS first-need), and A's writer
/// **re-seals its unchanged row under the superseding tip** on a later pass
/// (the retained-window re-seal, scoped to this kind). The asserted state is
/// latency-independent (convention 14): both replicas hold BOTH device rows,
/// each decoding to its own key's node id.
#[tokio::test]
async fn v5_the_device_endpoints_writer_mints_and_the_sibling_opens_the_row() {
    let (rpc, _state, tmp) = nest_with_escrow().await;
    let base: PathBuf = tmp.path().to_path_buf();

    // A alone first — the writer's first pass door-mints generation 1 with a
    // member set of {A}, which is what makes the re-seal discipline
    // observable below rather than skipped.
    let a = AccountStoreRuntime::start(params_trusting_deployment(&base, "a", &rpc))
        .await
        .expect("start A");
    eventually("A publishes its own device-endpoints row", || {
        let a = a.clone();
        async move {
            a.reconcile_now().await.expect("A pass");
            let rows = endpoint_rows(&a).await;
            rows.len() == 1 && hex::encode(rows[0].1.node_id) == rows[0].0
        }
    })
    .await;

    // The mint is real merged state, not a side effect the test imagines: the
    // fleet walk serves the mint row back through the ordinary read path.
    let mints = a
        .states_of_kind(KIND_GENERATION_MINT)
        .await
        .expect("mint rows");
    assert!(
        !mints.is_empty(),
        "the first origination minted through the door (trigger (a))"
    );

    // B joins the fleet. Its own writer publishes B's row (heal-minting a tip
    // that covers both devices), and A's writer re-seals its unchanged row
    // under the superseding tip — after which each side opens the other's.
    let b = AccountStoreRuntime::start(params_trusting_deployment(&base, "b", &rpc))
        .await
        .expect("start B");
    eventually("both replicas open both device rows", || {
        let a = a.clone();
        let b = b.clone();
        async move {
            a.reconcile_now().await.expect("A pass");
            b.reconcile_now().await.expect("B pass");
            let (ra, rb) = (endpoint_rows(&a).await, endpoint_rows(&b).await);
            let complete = |rows: &[(String, DeviceEndpoints)]| {
                rows.len() == 2 && rows.iter().all(|(key, v)| hex::encode(v.node_id) == *key)
            };
            let same_keys = {
                let keys = |rows: &[(String, DeviceEndpoints)]| {
                    let mut k: Vec<&String> = rows.iter().map(|(k, _)| k).collect();
                    k.sort();
                    k.into_iter().cloned().collect::<Vec<_>>()
                };
                keys(&ra) == keys(&rb)
            };
            complete(&ra) && complete(&rb) && same_keys
        }
    })
    .await;

    // The facts door: the assembler feeds transport truth (here the relay
    // URL — `NestInfoReply.iroh_relay_url`'s shape); the writer republishes
    // and the sibling reads the updated candidates.
    a.set_endpoint_facts(EndpointFacts {
        relay_url: Some("https://relay.example/".into()),
        ..Default::default()
    })
    .await
    .expect("set facts on A");
    eventually("B reads A's relay URL", || {
        let a = a.clone();
        let b = b.clone();
        async move {
            a.reconcile_now().await.expect("A pass");
            b.reconcile_now().await.expect("B pass");
            endpoint_rows(&b)
                .await
                .iter()
                .any(|(_, v)| v.relay_url.as_deref() == Some("https://relay.example/"))
        }
    })
    .await;

    // Quiescence: with facts unchanged and the tip settled, a further pass on
    // each side holds the row rather than churning the feed.
    for (name, h) in [("A", &a), ("B", &b)] {
        let report = h.reconcile_now().await.expect("settled pass");
        assert_eq!(
            report.device_endpoints,
            Some(EndpointsPass::Current),
            "{name}'s settled pass republishes nothing"
        );
    }

    a.shutdown().await;
    b.shutdown().await;
}

/// [`nest_with_escrow`] with **no deployment identity** — the escrow doors are
/// registered but the nest cannot sign a receipt, which is the handler's own
/// documented `holder_unavailable` case ("a nest still booting without its
/// deployment key cannot act as an escrow holder"). The shape that makes a
/// mint's deposit fail against a door that is genuinely there.
async fn nest_with_escrow_lacking_deployment_identity()
-> (Arc<RouterRequester>, Arc<AppState>, tempfile::TempDir) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let blob_dir = tmp.path().join("blob");
    std::fs::create_dir_all(&blob_dir).unwrap();
    let backup_svc = Arc::new(BackupService::new(db.clone(), None, false, blob_dir, None).unwrap());
    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        nest_signing_key: None,
        ..AppState::for_test(db)
    });
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    // `fauna.capabilities.reconcile` — every full pass enumerates it.
    fauna_nest::bridge_blob_handlers::register_capability_handlers(&mut b);
    fauna_nest::generation_escrow_handlers::register_generation_escrow_handlers(&mut b);
    fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
    common::seed_dispatch_actor(&state.db, &actor_bytes()).await;
    (
        Arc::new(RouterRequester {
            router: b.build(),
            state: Arc::clone(&state),
            actor: actor_bytes(),
        }),
        state,
        tmp,
    )
}

/// **A `GenerationTip` origination that cannot mint must report the half that
/// actually failed, not only the standing refusal** (charter § The generation
/// machinery: *"fleet-only sealing stays refused **and says why**"*).
///
/// The standing text — *"no candidate generation tip resolves for this device
/// … 0 row(s) flagged invalid and 0 unreachable"* — is true of a nest that
/// cannot sign a receipt, a machine with no escrow target, and a data path
/// that never connected alike, so on its own it names nothing. What separates
/// them is the mint failure attached as its **cause**, and only the full chain
/// carries it: `resolve_or_mint` wraps the mint error with the standing
/// refusal as *context*, so a consumer printing `{e}` sees the standing text
/// and a consumer printing `{e:#}` sees both.
///
/// This is not a hypothetical regression. The share journey's linux red sat behind exactly this for four sessions: the
/// sink logged the outermost context only, and the deposit failure underneath
/// it — the whole diagnosis — was invisible until `{e:#}` landed on
/// 2026-08-24. Nothing pinned the chain, so nothing would catch it being
/// re-hidden. This does.
///
/// Convention 14: one explicit pass, no waiting — the refusal is synchronous
/// with the origination by construction (the door refuses before any row is
/// staged), which is the same property that makes a refused write leave no
/// durable local row.
#[tokio::test]
async fn v15_a_mint_that_cannot_deposit_refuses_with_the_deposit_failure_attached() {
    let (rpc, _state, tmp) = nest_with_escrow_lacking_deployment_identity().await;
    let base: PathBuf = tmp.path().to_path_buf();

    let a = AccountStoreRuntime::start(params_trusting_deployment(&base, "a", &rpc))
        .await
        .expect("start A");

    // The device-endpoints writer's own pass is the account's first
    // `GenerationTip` origination, so its step error is the refusal.
    let report = a.reconcile_now().await.expect("pass");
    let refusal = report
        .errors
        .iter()
        .find(|e| e.starts_with("device-endpoints:"))
        .unwrap_or_else(|| {
            panic!(
                "the first `GenerationTip` origination must refuse against a holder that \
                 cannot sign: {report:?}"
            )
        });

    assert!(
        refusal.contains("no candidate generation tip resolves"),
        "the standing R14 refusal is the outermost context: {refusal}"
    );
    assert!(
        refusal.contains("escrow deposit failed"),
        "…and the half that ACTUALLY failed must ride the same chain, or every \
         unmintable cause reads identically. Got: {refusal}"
    );

    // No mint row was left behind: deposit-first means a failed sequence
    // stages nothing, so the next pass retries cleanly rather than resolving
    // a tip whose escrow never landed.
    assert!(
        a.states_of_kind(KIND_GENERATION_MINT)
            .await
            .expect("mint rows")
            .is_empty(),
        "a refused deposit publishes no mint row (escrow-before-first-seal)"
    );

    a.shutdown().await;
}

// ── V17: a sign-out then a sign-in — the prologue opens what it just keyed ───

/// The senior ATProto rotation key V17 writes — key material that rests
/// nowhere but its `fauna.state.atproto-identity` row.
fn senior_rotation_key() -> fauna_core::data::AtprotoRotationKey {
    fauna_core::data::AtprotoRotationKey {
        secret_scalar: fauna_core::secret::SecretArray32::new([0x5e; 32]),
        pubkey_did_key: "did:key:zDnaeSeniorRotationKey".into(),
        created_at: 1_700_000_000,
        published_for_dids: vec!["did:plc:ewvi7nxzyoun6zhxrhs64oiz".into()],
    }
}

/// **A same-device sign-out followed by a sign-in reads the rotation ring back
/// at the readiness edge** (`account-data-taxonomy.md` § The generation
/// machinery → *Escrow recovery*, item (3): the pass that keys a generation
/// is the pass that reads it).
///
/// The single-device account mints its senior rotation key (a `GenerationTip`
/// row sealed under generation 1) and signs out: the machine's store and its
/// credential slot are erased, so the next sign-in is a fresh replica under a
/// fresh writer key, with no sibling to top it up. Its prologue walks the
/// fleet scope — generation 1's rows unopenable — and only then keys
/// generation 1 from its escrow wrap. `AccountStoreHandle::settled` is the
/// readiness edge every consumer of the ring waits on (the critical-alert
/// sweep's feeder #1, the ATProto settings machine), so the ring must be
/// readable when it resolves: a pass that keyed the generation but left its
/// rows for the next pass reports "no key held" to a consumer that asked
/// exactly once.
///
/// `conformance_account_state_walk.rs`'s sign-in-recovery pin proves the rows
/// open *eventually* (its hand-driven passes each end in a reconcile); this
/// one pins WHEN, through the production pump.
///
/// Convention 14: `settled` is a causal barrier, not a wait — nothing is
/// polled after it.
///
/// Red-verified: without the re-presentation in `pump`, the ring reads empty
/// after `settled`.
#[tokio::test]
async fn v17_a_sign_in_after_a_sign_out_reads_the_rotation_ring_once_its_prologue_settles() {
    let (rpc, _state, tmp) = nest_with_escrow().await;
    let base: PathBuf = tmp.path().to_path_buf();
    let ring = fauna_core::data::AtprotoIdentityConfig {
        rotation_keys: vec![senior_rotation_key()],
        ..Default::default()
    };

    let a = AccountStoreRuntime::start(params_trusting_deployment(&base, "before", &rpc))
        .await
        .expect("the first sign-in assembles");
    a.settled().await;
    let joined = a
        .merge_atproto_identity(ring.clone())
        .await
        .expect("the mint path writes the rotation key");
    assert_eq!(joined.rotation_keys, ring.rotation_keys);
    // Everything is on the nest before the sign-out: a pass with nothing left
    // to publish and no step error.
    eventually("the ring row is published", || {
        let a = a.clone();
        async move {
            let report = a.reconcile_now().await.expect("A pass");
            report.errors.is_empty() && report.fleet_published == Some(0)
        }
    })
    .await;
    let retirement = a.shutdown_for_sign_out().await;
    assert!(
        matches!(
            retirement,
            fauna_sync_engine::account_runtime::EnrollmentRetirement::Retired { .. }
        ),
        "the sign-out retires the machine's principal: {retirement:?}"
    );

    // The sign-out's erase took the store and the slot: the same machine (its
    // named row) signs in again over an empty state dir and an empty slot.
    let again = AccountStoreRuntime::start(AccountRuntimeParams {
        enrollment_target_device_id: device_row("before"),
        ..params_trusting_deployment(&base, "after", &rpc)
    })
    .await
    .expect("the second sign-in assembles");
    again.settled().await;

    let held = again
        .atproto_identity()
        .await
        .expect("the ring reads through the runtime's door");
    assert_eq!(
        held.rotation_keys, ring.rotation_keys,
        "the prologue keyed generation 1 from escrow, so the ring it seals is readable the \
         moment the prologue settles — not one pass later"
    );

    again.shutdown().await;
}

// ── V8: the W5.1 engine-singleton election — two runtimes, ONE store dir ────

/// The W5.1 two-runtimes-one-store-dir proof (charter § Multi-instance
/// concurrency, T9): two [`AccountStoreRuntime`]s share one store dir — two
/// processes of one machine in production, run here as two runtimes because
/// the election lock lives on the open file description, so same-process
/// acquires contend exactly like processes. The first to assemble holds the
/// engine-singleton role; the second comes up as a plain reader/writer whose
/// `put_preference` still lands and publishes; and when the holder exits, the
/// survivor picks the role up on its own backstop tick — kernel arbitration,
/// no probe window, no lease, no protocol.
///
/// Convention 14 throughout (the lesson): the role is asserted as an
/// in-band report fact (`skipped_non_holder`), never pass attribution; the
/// takeover assert is a deadline poll on merged state only a pump pass can
/// produce (a row another device published after the holder's exit, which
/// reaches this store only by a walk), and the survivor's ticker is the only
/// wake source left for it: no `reconcile_now` after the exit, no push
/// pump, no reconnect arm.
#[tokio::test]
async fn v8_two_same_store_runtimes_elect_one_engine_singleton() {
    let (rpc, _state, tmp) = nest().await;
    let base: PathBuf = tmp.path().to_path_buf();

    // ONE device area — same state dir, same credential slot, same writer
    // key: the multi-instance shape, not V1's two-device shape.
    let holder = AccountStoreRuntime::start(params(&base, "shared", &rpc))
        .await
        .expect("start the first runtime");
    // The second runtime's backstop is armed — the one cadence in this test:
    // pre-takeover it is the re-election re-try, post-takeover the pump. The
    // holder's stays disarmed, so no pass in this test is anonymous.
    let survivor = {
        let mut p = params(&base, "shared", &rpc);
        p.backstop_interval = Duration::from_millis(50);
        AccountStoreRuntime::start(p)
            .await
            .expect("start the second runtime")
    };

    // Roles, as in-band causal facts — both elections are settled (each
    // start's readiness barrier covers its election). The survivor's answer
    // also pins reconcile_now's opportunistic re-try never stealing from a
    // live holder.
    let report = holder.reconcile_now().await.expect("holder pass");
    assert!(!report.skipped_non_holder, "the first runtime pumps");
    let report = survivor
        .reconcile_now()
        .await
        .expect("survivor role answer");
    assert!(
        report.skipped_non_holder,
        "the second same-store runtime is a plain reader/writer"
    );

    // The plain writer's write: durable in the shared store — both handles
    // read the row — and its publish step (the non-holder runs it too: its
    // own rows are its own to publish) sends it to the nest, with no pump
    // pass on the writing side.
    let value = moderation_bytes(&["lottery"]);
    survivor
        .put_preference(KIND_MODERATION, value.clone())
        .await
        .expect("put on the non-holder");
    for (name, h) in [("the holder", &holder), ("the non-holder", &survivor)] {
        let entry = h
            .get_preference(KIND_MODERATION)
            .await
            .expect("get")
            .expect("entry present");
        assert_eq!(entry.value, value, "{name} reads the shared store's row");
    }
    // Behind the non-holder's pass barrier: its publish step has run, so the
    // row rests on the nest — a second DEVICE of the account, with a store of
    // its own, reads it by its walk.
    survivor.settled().await;
    let other = AccountStoreRuntime::start(params(&base, "other", &rpc))
        .await
        .expect("start a second device");
    other
        .reconcile_now()
        .await
        .expect("the second device's pass");
    assert_eq!(
        other
            .get_preference(KIND_MODERATION)
            .await
            .expect("get on the second device")
            .map(|entry| entry.value),
        Some(value.clone()),
        "the non-holder's publish step reached the nest"
    );

    // The holder exits; the role is free. The second device publishes a newer
    // row AFTER the exit, so only a walk on the survivor can bring it into
    // the shared store: serving the new value proves acquisition on the
    // backstop tick plus the acquired catch-up pass — not a read of anything
    // this test wrote to that store directly.
    holder.shutdown().await;
    other
        .put_preference(KIND_MODERATION, moderation_bytes(&["prize"]))
        .await
        .expect("the second device's write after the holder exits");
    other.settled().await;
    eventually(
        "the survivor takes the role on its backstop tick and walks the newer row in",
        || {
            let survivor = survivor.clone();
            async move {
                matches!(
                    survivor.get_preference(KIND_MODERATION).await,
                    Ok(Some(entry)) if entry.value == moderation_bytes(&["prize"])
                )
            }
        },
    )
    .await;

    survivor.shutdown().await;
    other.shutdown().await;
}

// ── V9: the W5.3 migration/adoption section — two COLD assemblies at once ───

/// The W5.3 critical section through the production assembly path (charter
/// § Multi-instance concurrency: "store-level advisory locks for the two
/// genuinely exclusive critical sections — schema migration/adoption, and the
/// engine-singleton role").
///
/// V8 starts its two runtimes one after the other, so it never exercises the
/// *open* path concurrently. Here both assemble at the same moment against a
/// store dir that **does not exist yet**, which is the shape a machine with
/// two co-located apps hits on the very first launch after sign-in: both run
/// dir creation, the WAL conversion, the schema migrations, and
/// `AccountStore::open`'s adoption of the format pair and the replica
/// identity, all at once.
///
/// Two failures live here, both fixed in W5.3 and neither visible to a
/// sequential test:
///
/// - `PRAGMA journal_mode=WAL` does not consult the busy handler, so a
///   racing opener used to fail outright with `database is locked` (measured
///   in `fauna-account-store`'s own tier_1 pin before the lock moved in
///   front of the connection).
/// - The format pair was written as two transactions, and `AccountStore::
///   open` *refuses to guess* at half a pair — so the loser of the race read
///   a perfectly good store as corrupt and bailed.
///
/// Two more showed only under contention (many copies of this test at once
/// on one core), each a two-step READ with the sibling's write landing
/// between the steps — the adoption runs after the migration section is
/// released, so nothing serializes it:
///
/// - The open read the format pair as two reads, so a pair that was never
///   written half was still read half ("half a version pair (format_version
///   None, min_reader_format_version Some(1))"). Pinned in the store crate:
///   `sqlite::tests::the_open_reads_the_format_pair_as_of_one_instant`.
/// - The lost-slot heal of the assembly that LOADED its sibling's freshly
///   minted writer key read the store's stamp and then the slot's
///   unstamped-mint marker, while the sibling stamped the store and spent the
///   marker in that order: no stamp, no marker, so it minted a second key and
///   was refused by the stamp it had just missed ("lost-slot heal … the store
///   is already stamped"). Pinned in the sync-engine crate:
///   `principal_succession::tests::a_sibling_stamping_a_cold_store_under_the_heal_is_adopted_not_reminted`.
///
/// The exactly-once claim is **not** asserted here: counting rebuilds is
/// possible only inside the store crate, and its tier_1 pin
/// (`sqlite::tests::concurrent_cold_opens_migrate_the_store_exactly_once`)
/// owns it. What this level adds is that the real runtime assembly survives
/// the race — and that the store it leaves behind is openable, which is the
/// end-to-end statement that the version pair landed whole.
///
/// Convention 14: no wall-clock anywhere. The two assemblies are `spawn`ed
/// on a multi-thread runtime and joined; every assertion is on state after
/// both joins.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v9_two_runtimes_assemble_concurrently_on_one_cold_store_dir() {
    let (rpc, _state, tmp) = nest().await;
    let base: PathBuf = tmp.path().to_path_buf();

    let first = tokio::spawn({
        let p = params(&base, "shared", &rpc);
        async move { AccountStoreRuntime::start(p).await }
    });
    let second = tokio::spawn({
        let p = params(&base, "shared", &rpc);
        async move { AccountStoreRuntime::start(p).await }
    });
    // Either assembly can be the race's loser, so both carry the same reading.
    const COLD_RACE: &str = "a failure here is two cold opens racing — the cause chain \
         says which: the WAL conversion lost the race (`database is locked`); the open read \
         the format pair across a sibling's stamp (`half a version pair`); or the lost-slot \
         heal missed the stamp its sibling landed and minted over it (`lost-slot heal … \
         already stamped`)";
    let first = first
        .await
        .expect("no panic in the first assembly")
        .unwrap_or_else(|e| panic!("the first cold assembly comes up — {COLD_RACE}: {e:#}"));
    let second = second
        .await
        .expect("no panic in the second assembly")
        .unwrap_or_else(|e| panic!("the second cold assembly comes up — {COLD_RACE}: {e:#}"));

    // W5.4a+b — the rest of the T10 bundle resolves inside the same critical
    // section as the writer key, so the concurrent cold assembly must neither
    // deadlock on the doubled section entry nor leave the two processes
    // disagreeing about the slot: both see the backup key persisted (the
    // seed-holding side's carriage for W5.5's seedless agent), ONE enrollment
    // grant (the W5.4b ceremony's mint half runs at assembly, inside the
    // section — the race's loser must LOAD the winner's grant, never mint a
    // second), and no retained generation keys yet.
    let first_bundle = first
        .principal_bundle_status()
        .await
        .expect("bundle status through the first runtime");
    let second_bundle = second
        .principal_bundle_status()
        .await
        .expect("bundle status through the second runtime");
    for (who, bundle) in [("first", &first_bundle), ("second", &second_bundle)] {
        assert!(
            bundle.backup_key_persisted,
            "{who}: the backup key rides the slot after a cold assembly"
        );
        assert_eq!(
            bundle.retained_generations, 0,
            "{who}: nothing retained yet"
        );
    }
    let first_auth = first_bundle
        .device_authorization
        .expect("the ceremony's mint half ran at the first assembly");
    let second_auth = second_bundle
        .device_authorization
        .expect("the ceremony's mint half is visible to the second assembly");
    assert_eq!(
        first_auth, second_auth,
        "both processes hold ONE machine grant — a disagreement means the \
         migration section failed to serialize the mint and each assembly \
         minted its own"
    );

    // The ceremony's nest legs ran through the real handlers (whichever
    // runtime holds the engine role registered during its prologue;
    // `reconcile_now` is pass-bound and runs after it, so no wait is needed):
    // the machine appears
    // ONCE in the devices list, under its named row, and the grant is
    // findable by the exact lookup `fauna.auth.device_handshake` performs.
    let holder_report = {
        let f = first.reconcile_now().await.expect("first reconcile");
        if f.skipped_non_holder {
            second.reconcile_now().await.expect("second reconcile")
        } else {
            f
        }
    };
    assert!(
        matches!(
            holder_report.enrollment,
            Some(
                fauna_sync_engine::account_runtime::EnrollmentPass::Current
                    | fauna_sync_engine::account_runtime::EnrollmentPass::Registered
            )
        ),
        "the holder's pass registered (or found registered) the enrollment: \
         {holder_report:?}"
    );
    let devices: fauna_protocol::sync::SyncDevicesListReply = rpc
        .request(
            "fauna.sync.devices.list",
            fauna_protocol::sync::SyncDevicesListRequest {
                extra: Default::default(),
            },
        )
        .await
        .expect("devices list");
    assert_eq!(
        devices.devices.len(),
        1,
        "one machine, one sync_devices row: {:?}",
        devices.devices
    );
    assert_eq!(
        devices.devices[0].device_id,
        device_row("shared"),
        "the machine row is its named row — the app's own id (the RULED \
         2026-09-28 block); the grant on it is over the writer key (T10)"
    );
    let grant_wire = _state
        .db
        .get_sync_device_grant(&actor_bytes(), &first_auth.device_key)
        .await
        .expect("grant lookup")
        .expect("the device_handshake lookup finds the registered grant");
    assert!(!grant_wire.is_empty());

    // The store both of them adopted is shared and usable: one writes, the
    // other reads the same row (no pump involved — the store IS the state).
    let value = moderation_bytes(&["cold-race"]);
    first
        .put_preference(KIND_MODERATION, value.clone())
        .await
        .expect("put through the first runtime");
    let entry = second
        .get_preference(KIND_MODERATION)
        .await
        .expect("get through the second runtime")
        .expect("entry present");
    assert_eq!(entry.value, value, "both runtimes share one store");

    first.shutdown().await;
    second.shutdown().await;

    // End-to-end proof that the format pair landed WHOLE: `AccountStore::
    // open` bails on half a pair, so a later cold assembly succeeding is the
    // store telling us the adoption transaction was atomic.
    let after = AccountStoreRuntime::start(params(&base, "shared", &rpc))
        .await
        .expect("a later assembly re-opens the store the race left behind");
    // …and W5.4b's "no re-enrollment": the later assembly LOADS the machine's
    // grant (never re-mints — same wire, so same created_at), and its pass
    // answers `Current` off the content-addressed latch without spending the
    // registration RPCs again. The devices list still shows one machine.
    let after_auth = after
        .principal_bundle_status()
        .await
        .expect("bundle status after re-assembly")
        .device_authorization
        .expect("the grant survives in the slot");
    assert_eq!(after_auth, first_auth, "loaded, not re-minted");
    let report = after.reconcile_now().await.expect("after reconcile");
    assert_eq!(
        report.enrollment,
        Some(fauna_sync_engine::account_runtime::EnrollmentPass::Current),
        "an already-registered machine spends no registration RPC"
    );
    let devices: fauna_protocol::sync::SyncDevicesListReply = rpc
        .request(
            "fauna.sync.devices.list",
            fauna_protocol::sync::SyncDevicesListRequest {
                extra: Default::default(),
            },
        )
        .await
        .expect("devices list after re-assembly");
    assert_eq!(devices.devices.len(), 1, "still one machine row");
    after.shutdown().await;
}

// ── V7: the W5.2 data_version floor — a sibling's commit is noticeable ──────

/// The W5.2 notification floor (charter § Multi-instance concurrency, T9
/// *poll-with-poke*): a second same-store process's write is (a) immediately
/// visible to the first process's read path — the shared store IS the state,
/// no restart, no pass — and (b) noticeable through the one cheap poll the
/// UI's refresh cadence runs: `data_version` moves on a sibling's commit and
/// never on the reader's own activity, its pump included.
///
/// Deterministic end to end (convention 14): every write here is
/// durable-before-return on its own connection, so each PRAGMA read is a
/// causal fact — no polls, no budgets.
#[tokio::test]
async fn v7_a_siblings_write_moves_data_version_and_is_served_without_restart() {
    let (rpc, _state, tmp) = nest().await;
    let base: PathBuf = tmp.path().to_path_buf();

    let holder = AccountStoreRuntime::start(params(&base, "shared", &rpc))
        .await
        .expect("start the holder");
    let sibling = AccountStoreRuntime::start(params(&base, "shared", &rpc))
        .await
        .expect("start the sibling");

    // Baseline AFTER both assemblies: the sibling's bootstrap writes are
    // durable before its start returns, so nothing else moves the counter
    // until this test acts (both backstops are disarmed).
    let v0 = holder
        .data_version()
        .await
        .expect("read")
        .expect("sqlite has a counter");

    // (a) + (b): the sibling — a plain reader/writer — commits; the holder's
    // counter moves and its read path serves the value, with no pass run.
    let value = moderation_bytes(&["lottery"]);
    sibling
        .put_preference(KIND_MODERATION, value.clone())
        .await
        .expect("put on the sibling");
    // The sibling's publish step follows its write and advances its own
    // frontier slot — one more commit on the sibling's connection — so the
    // reading below is taken behind the sibling's pass barrier: a causal
    // fact, not a race with that step.
    sibling.settled().await;
    let v1 = holder.data_version().await.expect("read").unwrap();
    assert_ne!(v0, v1, "a sibling's commit moves the holder's data_version");
    let entry = holder
        .get_preference(KIND_MODERATION)
        .await
        .expect("get")
        .expect("entry present");
    assert_eq!(
        entry.value, value,
        "the sibling's write is served without restart and without a pass"
    );

    // The holder's own activity — a full pump pass and its own put — must not
    // move its own counter: a runtime that notified itself would turn the
    // floor into a self-refresh storm.
    let report = holder.reconcile_now().await.expect("holder pass");
    assert!(
        !report.skipped_non_holder,
        "the first runtime holds the role"
    );
    holder
        .put_preference(KIND_MODERATION, moderation_bytes(&["lottery", "prize"]))
        .await
        .expect("put on the holder");
    let v2 = holder.data_version().await.expect("read").unwrap();
    assert_eq!(
        v1, v2,
        "own writes and own passes never move the runtime's own counter"
    );
    // …while the floor is symmetric — every instance can poll it: seed the
    // SIBLING's connection-local reading, commit on the holder, and the
    // sibling's counter moves.
    let vs = sibling.data_version().await.expect("read").unwrap();
    holder
        .put_preference(
            KIND_MODERATION,
            moderation_bytes(&["lottery", "prize", "win"]),
        )
        .await
        .expect("second put on the holder");
    let vs2 = sibling.data_version().await.expect("read").unwrap();
    assert_ne!(vs, vs2, "the holder's commit moves the sibling's counter");

    holder.shutdown().await;
    sibling.shutdown().await;
}

// ── The own-pump source: the change generation ──────────────────────────────
//
// `account-runtime.md` § Multi-instance concurrency → *A runtime's own
// pump is a source of the notice too*: a run of this runtime's own pump that
// changed an entry a read can answer moves its change generation, once, at
// the run's end; a quiet run, a gesture's own write and a non-holder move
// nothing. V7 above pins the other source, the `data_version` floor.

/// Wait for `handle`'s change generation to move past `after` — the own-pump
/// notice a store-backed surface re-reads on.
async fn generation_moves_past(handle: &AccountStoreHandle, after: u64, what: &str) -> u64 {
    tokio::time::timeout(CONVERGENCE_BUDGET, handle.changed_after(after))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{what}: the change generation stayed at {} (baseline {after})",
                handle.change_generation()
            )
        })
}

/// Pin (a): another device's row, applied by this holder's walk — the nudge's
/// walk, the run a `fauna.sync.changed` push wakes — moves the generation,
/// and the open page's re-read then answers the new value.
#[tokio::test]
async fn own_pump_a_siblings_row_applied_by_this_holders_walk_moves_the_generation() {
    let (rpc, _state, tmp) = nest().await;
    let base: PathBuf = tmp.path().to_path_buf();
    let a = AccountStoreRuntime::start(params(&base, "a", &rpc))
        .await
        .expect("start A");
    let b = AccountStoreRuntime::start(params(&base, "b", &rpc))
        .await
        .expect("start B");
    // A's prologue is over (`reconcile_now` is served behind it) and the
    // feed held nothing of B's yet.
    a.reconcile_now().await.expect("A's barrier");
    let g0 = a.change_generation();

    let value = moderation_bytes(&["lottery"]);
    b.put_preference(KIND_MODERATION, value.clone())
        .await
        .expect("put on B");
    b.settled().await; // B's publish step has run: the row is on the feed.

    a.nudge_scope(ACCOUNT_STATE_SCOPE);
    generation_moves_past(&a, g0, "A's walk applied B's row").await;
    assert_eq!(
        a.get_preference(KIND_MODERATION)
            .await
            .expect("read")
            .map(|e| e.value),
        Some(value),
        "the re-read the notice drives answers B's value"
    );

    a.shutdown().await;
    b.shutdown().await;
}

/// The predecessor identity of pin (b): a seed other than [`account`]'s.
const PREDECESSOR_SEED: [u8; 32] = [0x55; 32];

fn predecessor_backup_key() -> fauna_core::crypto::BackupKey {
    fauna_core::crypto::BackupKey::derive(
        ActorKeypair::from_secret(PREDECESSOR_SEED).secret_bytes(),
    )
}

/// Pin (b): a successor's walk carrying its predecessor's delegable rows moves
/// the generation — the case where a successor opens Muted words in its first
/// seconds and must see the inherited words without a re-visit. Setup after
/// `conformance_succession_carry.rs::a_successors_fresh_replica_reads_the_e1_value_its_predecessor_wrote`:
/// the predecessor writes the way a preference page does (the plane's local
/// put, then the publish), and the successor's runtime is handed the
/// predecessor's seed as an attested predecessor.
#[tokio::test]
async fn own_pump_a_successors_carry_of_its_predecessors_rows_moves_the_generation() {
    use fauna_account_store::{sqlite::SqliteBackend, store::AccountStore};
    use fauna_core::crypto::AccountStateKeySchedule;
    use fauna_protocol::merge_policy::PREFERENCE_KEY;
    use fauna_sync_engine::account_state_plane::AccountStatePlane;
    use fauna_sync_engine::attested_predecessors::AttestedPredecessors;
    use fauna_sync_engine::generation_tip::GenerationTrust;
    use fauna_sync_engine::preference_put::put_preference_local;

    let (rpc, _state, tmp) = nest().await;
    let base: PathBuf = tmp.path().to_path_buf();
    let predecessor = ActorKeypair::from_secret(PREDECESSOR_SEED);
    let mut s_params = params(&base, "successor", &rpc);
    s_params.attested_predecessors = AttestedPredecessors::from_backup_keys([(
        predecessor.actor_id(),
        &predecessor_backup_key(),
    )]);
    let s = AccountStoreRuntime::start(s_params)
        .await
        .expect("start the successor");
    s.reconcile_now().await.expect("the successor's barrier");
    let g0 = s.change_generation();

    // The predecessor's device, on the one nest actor that stands for the
    // rows the ceremony re-pointed at the successor.
    let value = moderation_bytes(&["witness"]);
    {
        let keys = AccountStateKeySchedule::derive(&predecessor_backup_key());
        let device = SigningKey::from_bytes(&[0x5A; 32]);
        let store = AccountStore::open(
            SqliteBackend::open_in_memory().unwrap(),
            &account().actor_id_hex(),
            WriterId(device.verifying_key().to_bytes()),
        )
        .await
        .unwrap();
        let trust = GenerationTrust {
            root: predecessor.actor_id(),
            prior: Vec::new(),
            trusted_holders: Default::default(),
        };
        let plane =
            AccountStatePlane::new(&store, &rpc, &keys, &device, &trust, ACCOUNT_STATE_SCOPE)
                .unwrap();
        put_preference_local(&store, &plane, KIND_MODERATION, value.clone())
            .await
            .expect("the predecessor's local put");
        assert_eq!(plane.publish_pending().await.expect("publish"), 1);
        assert!(
            store
                .state(KIND_MODERATION, PREFERENCE_KEY)
                .await
                .unwrap()
                .is_some()
        );
    }

    let report = s.reconcile_now().await.expect("the successor's pass");
    assert_eq!(
        report.walk.as_ref().map(|w| w.inherited),
        Some(1),
        "the successor's walk carried the predecessor's row: {report:?}"
    );
    assert!(
        s.change_generation() > g0,
        "the carry moved the generation (still {g0})"
    );
    assert_eq!(
        s.get_preference(KIND_MODERATION)
            .await
            .expect("read")
            .map(|e| e.value),
        Some(value)
    );

    s.shutdown().await;
}

/// Pin (c): a quiet run moves nothing — a full pass with nothing new, and a
/// walk that meets only this replica's own row coming back (`self_echo`) and
/// a row whose value lost to the one held (`kept`). (A nudge's walk is the
/// same walk; it has no barrier to assert behind — the serve loop answers a
/// command before a pending nudge — so pin (a) covers that run positively.)
/// Runs are frequent and a
/// consumer's re-drive is a whole reload, so bookkeeping must stay silent.
#[tokio::test]
async fn own_pump_a_quiet_run_moves_nothing() {
    let (rpc, _state, tmp) = nest().await;
    let base: PathBuf = tmp.path().to_path_buf();
    let a = AccountStoreRuntime::start(params(&base, "a", &rpc))
        .await
        .expect("start A");
    let b = AccountStoreRuntime::start(params(&base, "b", &rpc))
        .await
        .expect("start B");
    // Quiesce the two devices' fleet rows first (each one's device rows reach
    // the other — a real change, which would move A's generation in the pass
    // under test): two exchanges, so each has published and walked the
    // other's.
    for _ in 0..2 {
        b.reconcile_now().await.expect("B's pass");
        a.reconcile_now().await.expect("A's pass");
    }

    // B's value first, A's after it: latest wins, so B's row loses on A.
    b.put_preference(KIND_MODERATION, moderation_bytes(&["older"]))
        .await
        .expect("put on B");
    b.settled().await;
    a.put_preference(KIND_MODERATION, moderation_bytes(&["newer"]))
        .await
        .expect("put on A");
    a.settled().await;

    let g0 = a.change_generation();
    let report = a.reconcile_now().await.expect("A's pass");
    let walk = report.walk.clone().expect("the walk ran");
    assert!(
        walk.kept >= 1 && walk.self_echo >= 1 && walk.applied == 0 && walk.merged == 0,
        "the pass met a losing row and its own row, and changed no entry: {walk:?}"
    );
    let fleet = report.fleet_walk.clone().expect("the fleet walk ran");
    assert!(
        fleet.applied == 0 && fleet.merged == 0,
        "the fleet scope was quiesced before the pass: {fleet:?}"
    );
    assert_eq!(
        a.change_generation(),
        g0,
        "a run that changed only bookkeeping moves nothing: {report:?}"
    );
    let report = a.reconcile_now().await.expect("A's second pass");
    assert_eq!(
        a.change_generation(),
        g0,
        "a pass with nothing new moves nothing: {report:?}"
    );
    assert_eq!(
        a.get_preference(KIND_MODERATION)
            .await
            .expect("read")
            .map(|e| e.value),
        Some(moderation_bytes(&["newer"]))
    );

    a.shutdown().await;
    b.shutdown().await;
}

/// The watch's counted level (`StoreChangeLevel` — web's face of the notice):
/// a changed run moves the count past what a waiter last saw, a dropped level
/// ends its relay while the runtime lives, and the level ends with the
/// runtime, so a waiter always settles.
#[tokio::test]
async fn own_pump_the_counted_level_moves_on_a_changed_run_and_ends_with_its_owner() {
    use fauna_client_account_runtime::store_change::StoreChangeLevel;

    let (rpc, _state, tmp) = nest().await;
    let base: PathBuf = tmp.path().to_path_buf();
    let a = AccountStoreRuntime::start(params(&base, "a", &rpc))
        .await
        .expect("start A");
    let b = AccountStoreRuntime::start(params(&base, "b", &rpc))
        .await
        .expect("start B");
    a.reconcile_now().await.expect("A's barrier");

    let (level, relay) = StoreChangeLevel::new(a.clone()).await;
    tokio::spawn(relay);
    let (dropped, dropped_relay) = StoreChangeLevel::new(a.clone()).await;
    let dropped_relay = tokio::spawn(dropped_relay);

    b.put_preference(KIND_MODERATION, moderation_bytes(&["lottery"]))
        .await
        .expect("put on B");
    b.settled().await;
    a.nudge_scope(ACCOUNT_STATE_SCOPE);
    let mut seen = tokio::time::timeout(CONVERGENCE_BUDGET, level.changed_after(0))
        .await
        .expect("A's walk applied B's row and the level never moved")
        .expect("the level is live: its owner and the runtime both are");
    assert_ne!(seen, 0, "the count moved past what the waiter had seen");

    drop(dropped);
    tokio::time::timeout(CONVERGENCE_BUDGET, dropped_relay)
        .await
        .expect("a dropped level's relay kept running beside a live runtime")
        .expect("the relay");

    a.shutdown().await;
    tokio::time::timeout(CONVERGENCE_BUDGET, async {
        while let Some(count) = level.changed_after(seen).await {
            seen = count;
        }
    })
    .await
    .expect("the level outlived its runtime: a waiter would hang");

    b.shutdown().await;
}

/// Pin (d): this runtime's own `put_preference`, and the publish step it
/// wakes, move nothing — the surface that made the gesture repaints from the
/// gesture's own answer. Also when the write is served inside a pass.
#[tokio::test]
async fn own_pump_a_gestures_own_write_and_its_publish_move_nothing() {
    let (rpc, _state, tmp) = nest().await;
    let base: PathBuf = tmp.path().to_path_buf();
    let a = AccountStoreRuntime::start(params(&base, "a", &rpc))
        .await
        .expect("start A");
    a.reconcile_now().await.expect("A's barrier");
    let g0 = a.change_generation();

    a.put_preference(KIND_MODERATION, moderation_bytes(&["mine"]))
        .await
        .expect("put");
    a.settled().await; // the publish step the put armed has run
    assert_eq!(
        a.change_generation(),
        g0,
        "a gesture and its publish step move nothing"
    );

    // The same write, raced against a pass: served inside it or beside it,
    // it is the gesture's, never the run's.
    let (report, put) = tokio::join!(
        a.reconcile_now(),
        a.put_preference(KIND_MODERATION, moderation_bytes(&["mine", "again"]))
    );
    report.expect("pass");
    put.expect("put");
    a.settled().await;
    a.reconcile_now()
        .await
        .expect("a pass that meets the row's echo");
    assert_eq!(
        a.change_generation(),
        g0,
        "a gesture served beside or inside a pass moves nothing"
    );

    a.shutdown().await;
}

/// Pin (e): a non-holder's generation never moves — it runs no pass, so its
/// notice is the `data_version` floor (V7) — while the holder's own walk of
/// the same row moves the holder's. The non-holder's own gesture and the
/// publish step it runs for it move nothing either.
#[tokio::test]
async fn own_pump_a_non_holders_generation_never_moves() {
    let (rpc, _state, tmp) = nest().await;
    let base: PathBuf = tmp.path().to_path_buf();
    let holder = AccountStoreRuntime::start(params(&base, "shared", &rpc))
        .await
        .expect("start the holder");
    let sibling = AccountStoreRuntime::start(params(&base, "shared", &rpc))
        .await
        .expect("start the sibling");
    let other = AccountStoreRuntime::start(params(&base, "other", &rpc))
        .await
        .expect("start the other device");
    holder.reconcile_now().await.expect("the holder's barrier");
    assert!(holder.is_engine_holder() && !sibling.is_engine_holder());
    let s0 = sibling.change_generation();
    let g0 = holder.change_generation();

    other
        .put_preference(KIND_MODERATION, moderation_bytes(&["elsewhere"]))
        .await
        .expect("put on the other device");
    other.settled().await;
    let report = holder.reconcile_now().await.expect("the holder's pass");
    assert_eq!(
        report.walk.as_ref().map(|w| w.applied),
        Some(1),
        "{report:?}"
    );
    assert!(
        holder.change_generation() > g0,
        "the holder's walk moves its own"
    );

    sibling
        .put_preference(KIND_MODERATION, moderation_bytes(&["here"]))
        .await
        .expect("put on the sibling");
    sibling.settled().await;
    let report = sibling.reconcile_now().await.expect("the sibling's call");
    assert!(report.skipped_non_holder, "the sibling runs no pass");
    assert_eq!(
        sibling.change_generation(),
        s0,
        "a non-holder's generation never moves"
    );

    holder.shutdown().await;
    sibling.shutdown().await;
    other.shutdown().await;
}

// ── V10: W5.4b's bearer half — the runtime's nest leg IS the store principal ─

/// A real **listening** nest for V10: the in-process `RouterRequester` cannot
/// carry the bearer path (the bearer is minted on an anonymous WS connection
/// and presented in the authenticated connect's subprotocol), so this is the
/// real axum router serving on a loopback port — the
/// `agent_process_tier3::start_test_nest` idiom, plus the auth + session
/// handler sets and a `BackupService`.
async fn start_listening_nest() -> (String, Arc<AppState>, tempfile::TempDir) {
    start_listening_nest_with(Default::default()).await
}

/// [`start_listening_nest`] with a chosen failed-credential throttle — a
/// one-refusal bucket turns "the nest refused this key" into a readable fact.
async fn start_listening_nest_with(
    failed_credential_throttle: fauna_nest::failed_credential_throttle::FailedCredentialThrottle,
) -> (String, Arc<AppState>, tempfile::TempDir) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    db.create_user(&actor_bytes(), "free", "test")
        .await
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let blob_dir = tmp.path().join("blob");
    std::fs::create_dir_all(&blob_dir).unwrap();
    let backup_svc = Arc::new(BackupService::new(db.clone(), None, false, blob_dir, None).unwrap());
    let mut b = RpcRouter::builder();
    fauna_nest::auth_handlers::register_auth_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    // `fauna.capabilities.reconcile` — every full pass enumerates it.
    fauna_nest::bridge_blob_handlers::register_capability_handlers(&mut b);
    folder_handlers::register_folders_handlers(&mut b);
    fauna_nest::session_handlers::register_sessions_handlers(&mut b);
    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        rpc_router: Arc::new(b.build()),
        failed_credential_throttle: Arc::new(failed_credential_throttle),
        ..AppState::for_test(db)
    });
    let app = fauna_nest::build_router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), state, tmp)
}

/// **The per-process bearer, end to end over a real listening nest** (charter
/// § The store device principal — "each process mints its own" bearer over
/// `fauna.auth.device_handshake`; `sync-agent.md` § Credential model, the W5
/// convergence bullet). Both requesters are real `NestClient`s: the app
/// session (identity handshake) and the principal's
/// (`device_principal_nest_client` — bearer-only auth signing with the writer
/// key, which `resolve_writer_key_serialized` minted under the migration
/// section before the runtime existed, exactly as an app does).
///
/// The circle this proves: the ceremony registers the grant over the APP
/// session → the device client's `fauna.auth.device_handshake` mint verifies
/// against that grant → the runtime's data legs run over the
/// principal-authenticated connection to a fully clean pass — and the
/// sessions list then shows both per-process sessions while the devices list
/// shows the one machine.
///
/// **A first sign-in, in production's shape** — the device client's connect
/// retry spawned after the runtime starts, gated on its grant-registered
/// signal (`fauna_client_account_runtime::resolve_and_start`;
/// `transport-connection.md` § The dial budget): the principal mints exactly
/// once, after the ceremony put its grant on the nest, and is never refused
/// `not_registered`. A one-refusal failed-credential bucket makes the
/// "never" readable: a single refused handshake for the writer key fills it.
///
/// Convention 14: the connect comes up asynchronously once the gate opens,
/// so the pass criterion is a deadline poll over `reconcile_now` — each
/// iteration a full real pass, yield between, no sleeps; a green run exits
/// on its first clean pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v10_the_runtime_authenticates_as_the_store_principal() {
    let (url, state, tmp) = start_listening_nest_with(
        fauna_nest::failed_credential_throttle::FailedCredentialThrottle::with_config(
            fauna_nest::bridge_rate_limit::LimiterConfig {
                window: Duration::from_secs(3600),
                max_events: 1,
            },
        ),
    )
    .await;
    let base: PathBuf = tmp.path().to_path_buf();

    let app_client = fauna_client::client::NestClient::new(url.clone(), account());
    app_client.connect().await.expect("app session connects");

    let state_base = base.join("v10").join("state");
    let store_root = StoreRoot::at(state_base.clone());
    let creds = CredentialStore::with_file_backend(CRED_NAMESPACE, base.join("v10").join("creds"));
    let writer_key = resolve_writer_key_serialized(&store_root, &account().actor_id_hex(), &creds)
        .expect("pre-assembly writer key");
    let writer_pub = writer_key.verifying_key().to_bytes();
    let device_client = fauna_client::ws_device_handshake_bearer::device_principal_nest_client(
        &url,
        actor_bytes(),
        writer_key,
        None,
    );

    let handle = AccountStoreRuntime::start(AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        store_root,
        actor_id_hex: account().actor_id_hex(),
        rpc: Arc::clone(&app_client),
        process_rpc: Some(Arc::clone(&device_client)),
        principal: RuntimePrincipal::SeedHolding(account().into()),
        credentials: creds,
        reconnects: Some(device_client.subscribe_reconnects()),
        pushes: None,
        backstop_interval: Duration::from_secs(3600),
        memberships: None,
        trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(Vec::new()),
        attested_predecessors: Default::default(),
        linked_nests: None,
        owed_nests: None,
        peer_transport: None,
        enrollment_target_device_id: device_row("machine"),
    })
    .await
    .expect("runtime assembles");
    fauna_client::ws_device_handshake_bearer::spawn_connect_retry(
        Arc::clone(&device_client),
        fauna_client::ws_device_handshake_bearer::ConnectGate::AfterGrant(
            handle.subscribe_grant_registered(),
        ),
    );

    // A durable local write whose publish leg must eventually travel the
    // principal's connection.
    handle
        .put_preference(KIND_MODERATION, moderation_bytes(&["principal"]))
        .await
        .expect("put through the runtime");

    // Deadline poll for a FULLY CLEAN pass: enrollment settled and no step
    // error — which requires every data leg (publish + both walks + bridge)
    // to have succeeded over the principal-authenticated connection.
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        let report = handle.reconcile_now().await.expect("pass");
        let enrolled = matches!(
            report.enrollment,
            Some(EnrollmentPass::Current | EnrollmentPass::Registered)
        );
        if enrolled && report.errors.is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no clean principal-authenticated pass within budget; last report: {report:?}"
        );
        tokio::task::yield_now().await;
    }

    // Per-process honesty: the app session AND the principal's session are
    // both real sessions on the nest — exactly two, since the gated principal
    // mints once.
    // …and not one handshake was refused on the way: a single refusal for
    // the writer key would have filled its one-refusal bucket.
    let surface = fauna_nest::failed_credential_throttle::Surface::DeviceHandshake;
    for source in [Some(std::net::Ipv4Addr::LOCALHOST.into()), None] {
        assert!(
            !state
                .failed_credential_throttle
                .is_exhausted(surface, source, &writer_pub),
            "the principal's handshake was refused (`not_registered`) before its grant \
             landed — the connect gate did not hold (source {source:?})"
        );
    }

    let sessions: fauna_protocol::sessions::SessionsListReply = app_client
        .request(
            "fauna.sessions.list",
            fauna_protocol::sessions::SessionsListRequest {
                extra: Default::default(),
            },
        )
        .await
        .expect("sessions list");
    assert!(
        sessions.sessions.len() == 2,
        "expected the app session AND the principal's per-process session: {:?}",
        sessions.sessions
    );

    // …while the machine appears exactly once in the devices list.
    let devices: fauna_protocol::sync::SyncDevicesListReply = app_client
        .request(
            "fauna.sync.devices.list",
            fauna_protocol::sync::SyncDevicesListRequest {
                extra: Default::default(),
            },
        )
        .await
        .expect("devices list");
    assert_eq!(
        devices.devices.len(),
        1,
        "one machine row: {:?}",
        devices.devices
    );

    handle.shutdown().await;
}

// ── V14: the first-need mint over the store principal's OWN connection ───────

/// [`start_listening_nest`] plus what [`nest_with_escrow`] adds — the
/// generation-escrow doors and the deployment identity that signs their
/// receipts. Neither existing rig covers this intersection: `nest_with_escrow`
/// mints over an in-process `RouterRequester` that is connected by
/// construction, and `start_listening_nest` carries a real bearer connection
/// but pins no escrow holder, so no mint has ever crossed one.
async fn start_listening_nest_with_escrow() -> (String, Arc<AppState>, tempfile::TempDir) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    db.create_user(&actor_bytes(), "free", "test")
        .await
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let blob_dir = tmp.path().join("blob");
    std::fs::create_dir_all(&blob_dir).unwrap();
    let backup_svc = Arc::new(BackupService::new(db.clone(), None, false, blob_dir, None).unwrap());
    let mut b = RpcRouter::builder();
    fauna_nest::auth_handlers::register_auth_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    // `fauna.capabilities.reconcile` — every full pass enumerates it.
    fauna_nest::bridge_blob_handlers::register_capability_handlers(&mut b);
    folder_handlers::register_folders_handlers(&mut b);
    fauna_nest::session_handlers::register_sessions_handlers(&mut b);
    fauna_nest::generation_escrow_handlers::register_generation_escrow_handlers(&mut b);
    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        nest_signing_key: Some(deployment_key()),
        rpc_router: Arc::new(b.build()),
        ..AppState::for_test(db)
    });
    let app = fauna_nest::build_router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), state, tmp)
}

/// **A first-need mint must complete over the connection the data path
/// actually runs on** (charter § The generation machinery, trigger (a) +
/// § The store device principal): with `process_rpc` wired, every plane write
/// — the escrow deposit included — rides the *store principal's* client, not
/// the app session. That client's `connect` comes up asynchronously
/// (`spawn_connect_retry`, ungated here, so it races the enrollment ceremony;
/// V10 holds production's gated shape), and the deposit is the one **synchronous**
/// nest call on the whole plane path: every other leg is local-first and
/// retries on a later pass, so a principal connection that never comes up is
/// invisible everywhere except here, where it fails the origination outright.
///
/// V5 proves the mint over an in-process requester that is connected by
/// construction; V10 proves the principal's connection carrying ordinary
/// data legs with no escrow holder pinned. This is the intersection, and it
/// is the exact configuration the share journey's linux seats run: the sink's
/// `fauna.state.share-endpoints` write is `GenerationTip`-sealed, so it
/// door-mints, and the measured red was `escrow deposit failed: rpc
/// disconnected (was_in_flight=false)` — the deposit's requester never
/// connected — against a refusal reporting **zero** mint rows in existence.
///
/// Convention 14: the pass criterion is a deadline poll over `reconcile_now`,
/// each iteration a full real pass — no sleeps, and a green run exits on its
/// first pass that carries a mint row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v14_the_first_need_mint_crosses_the_store_principals_own_connection() {
    let (url, _state, tmp) = start_listening_nest_with_escrow().await;
    let base: PathBuf = tmp.path().to_path_buf();

    let app_client = fauna_client::client::NestClient::new(url.clone(), account());
    app_client.connect().await.expect("app session connects");

    let state_base = base.join("v14").join("state");
    let store_root = StoreRoot::at(state_base.clone());
    let creds = CredentialStore::with_file_backend(CRED_NAMESPACE, base.join("v14").join("creds"));
    let writer_key = resolve_writer_key_serialized(&store_root, &account().actor_id_hex(), &creds)
        .expect("pre-assembly writer key");
    let device_client = fauna_client::ws_device_handshake_bearer::device_principal_nest_client(
        &url,
        actor_bytes(),
        writer_key,
        None,
    );
    fauna_client::ws_device_handshake_bearer::spawn_connect_retry(
        Arc::clone(&device_client),
        fauna_client::ws_device_handshake_bearer::ConnectGate::Open,
    );

    let handle = AccountStoreRuntime::start(AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        store_root,
        actor_id_hex: account().actor_id_hex(),
        rpc: Arc::clone(&app_client),
        process_rpc: Some(Arc::clone(&device_client)),
        principal: RuntimePrincipal::SeedHolding(account().into()),
        credentials: creds,
        reconnects: Some(device_client.subscribe_reconnects()),
        pushes: None,
        backstop_interval: Duration::from_secs(3600),
        memberships: None,
        // The pinned deployment identity — what makes the door mint rather
        // than refuse for want of a trusted receipt signer.
        trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(vec![
            deployment_key().verifying_key().to_bytes(),
        ]),
        attested_predecessors: Default::default(),
        linked_nests: None,
        owed_nests: None,
        peer_transport: None,
        enrollment_target_device_id: device_row("machine"),
    })
    .await
    .expect("runtime assembles");

    // The device-endpoints writer's own pass is the account's first
    // production `GenerationTip` origination, so a completed mint row is the
    // observable — served back through the ordinary read path, never a side
    // effect the test imagines.
    eventually_or(
        "the first-need mint's rows land through the principal's connection",
        || {
            let handle = handle.clone();
            async move {
                let report = handle.reconcile_now().await.expect("pass");
                let _ = report;
                !handle
                    .states_of_kind(KIND_GENERATION_MINT)
                    .await
                    .expect("mint rows")
                    .is_empty()
            }
        },
        || {
            let handle = handle.clone();
            async move {
                let report = handle.reconcile_now().await;
                format!(
                    "no mint row: the deposit rides `process_rpc`, so a principal connection \
                     that never came up fails every `GenerationTip` origination while every \
                     other leg still looks healthy. Last pass: {report:?}"
                )
            }
        },
    )
    .await;

    handle.shutdown().await;
}

// ── V13: row 50 — principal succession after a device delete ─────────────────

/// **Delete-then-sign-in revives the machine as a NEW fleet device with its
/// un-pushed tail preserved**, over the REAL doors end to end
/// (`account-data-plane.md` § The store device principal → *Principal
/// succession after a device delete*): the real `fauna.sync.devices.delete`
/// handler tombstones the writer key, the real register handler answers the
/// typed `fauna.sync.device_grant_revoked` over a real WS client (the code
/// surviving `NestClientError`'s classification is exactly what the
/// FakeNest-level twin cannot prove), the probe's rotation mints the
/// successor, and the revived runtime re-authors and publishes the tail the
/// dead writer never pushed — while the dead key stays dead.
///
/// The un-pushed row is staged at the store surface between the sessions —
/// byte-for-byte the state a publish-failed crash leaves. The production put
/// path for that staging is proven in
/// `fauna_sync_engine::account_runtime::tests::
/// a_deleted_machine_revives_as_a_successor_with_its_unpushed_tail`, whose
/// publish leg one-shot-fails through `put_preference` itself.
///
/// Convention 14: sequential by construction — session 1 fully shut down
/// before the staging and the delete; the revival's convergence is V10's
/// deadline poll.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v13_a_deleted_machine_revives_as_a_successor_through_the_real_doors() {
    let (url, _state, tmp) = start_listening_nest().await;
    let base: PathBuf = tmp.path().to_path_buf();
    let app_client = fauna_client::client::NestClient::new(url.clone(), account());
    app_client.connect().await.expect("app session connects");

    let state_base = base.join("v13").join("state");
    let creds_dir = base.join("v13").join("creds");
    let params = || AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        store_root: StoreRoot::at(state_base.clone()),
        actor_id_hex: account().actor_id_hex(),
        rpc: Arc::clone(&app_client),
        process_rpc: None,
        principal: RuntimePrincipal::SeedHolding(account().into()),
        credentials: CredentialStore::with_file_backend(CRED_NAMESPACE, creds_dir.clone()),
        reconnects: None,
        pushes: None,
        backstop_interval: Duration::from_secs(3600),
        memberships: None,
        trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(Vec::new()),
        attested_predecessors: Default::default(),
        linked_nests: None,
        owed_nests: None,
        peer_transport: None,
        enrollment_target_device_id: device_row("machine"),
    };
    let converge = |handle: AccountStoreHandle| async move {
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        loop {
            let report = handle.reconcile_now().await.expect("pass");
            let enrolled = matches!(
                report.enrollment,
                Some(EnrollmentPass::Current | EnrollmentPass::Registered)
            );
            if enrolled && report.errors.is_empty() {
                return handle;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no clean pass within budget; last report: {report:?}"
            );
            tokio::task::yield_now().await;
        }
    };

    // ── Session 1: enroll the machine.
    let handle = AccountStoreRuntime::start(params()).await.expect("enroll");
    let handle = converge(handle).await;
    let writer_hex = || {
        let creds = CredentialStore::with_file_backend(CRED_NAMESPACE, creds_dir.clone());
        fauna_core::hex32::encode(
            &fauna_sync_engine::principal_bundle::load_writer_key(
                &creds,
                &account().actor_id_hex(),
            )
            .expect("the slot holds the enrolled writer")
            .verifying_key()
            .to_bytes(),
        )
    };
    let old_writer = writer_hex();
    let old_id = {
        let devices: fauna_protocol::sync::SyncDevicesListReply = app_client
            .request(
                "fauna.sync.devices.list",
                fauna_protocol::sync::SyncDevicesListRequest {
                    extra: Default::default(),
                },
            )
            .await
            .expect("devices list");
        assert_eq!(devices.devices.len(), 1, "{:?}", devices.devices);
        devices.devices[0].device_id.clone()
    };
    handle.shutdown().await;

    // ── The un-pushed tail: a row the (about-to-die) writer journaled but
    // never pushed, staged on the closed store (see the doc header).
    let tail_value = moderation_bytes(&["the-tail-that-must-survive"]);
    {
        let creds = CredentialStore::with_file_backend(CRED_NAMESPACE, creds_dir.clone());
        let writer =
            fauna_sync_engine::principal_bundle::load_writer_key(&creds, &account().actor_id_hex())
                .expect("the slot holds the enrolled writer");
        let store_dir = StoreRoot::at(state_base.clone())
            .store_dir(&account().actor_id_hex())
            .expect("store dir");
        let backend =
            fauna_account_store::sqlite::SqliteBackend::open(&store_dir).expect("backend");
        let store = fauna_account_store::store::AccountStore::open(
            backend,
            &account().actor_id_hex(),
            WriterId(writer.verifying_key().to_bytes()),
        )
        .await
        .expect("store opens under the enrolled writer");
        let stamp = fauna_protocol::merge_policy::LwwStamp {
            at_ms: 1,
            writer: writer.verifying_key().to_bytes(),
        };
        store
            .put_state(fauna_account_store::types::StateEntry {
                kind: KIND_MODERATION.to_string(),
                key: fauna_protocol::merge_policy::PREFERENCE_KEY.to_string(),
                scope: ACCOUNT_STATE_SCOPE.to_string(),
                value: tail_value.clone(),
                merge_meta: Some(stamp.encode().expect("stamp encodes")),
                entry_version: 0,
                tombstone: false,
            })
            .await
            .expect("the un-pushed row lands locally");
    }

    // ── The user's delete, through the real handler over the real wire.
    let del = fauna_client_sync::SyncClient::new(Arc::clone(&app_client))
        .devices_delete(old_id.clone())
        .await
        .expect("devices.delete");
    assert!(del.deleted, "the delete must report deleted");

    // ── Revival: a fresh seed-holding sign-in on the same machine state.
    let handle = AccountStoreRuntime::start(params()).await.expect("revival");
    let handle = converge(handle).await;

    // The machine is a NEW device under a successor principal: one row — its
    // named row, re-created by the successor's enrollment alone (the probe is
    // grant-first precisely so the dead key never re-creates it).
    let devices: fauna_protocol::sync::SyncDevicesListReply = app_client
        .request(
            "fauna.sync.devices.list",
            fauna_protocol::sync::SyncDevicesListRequest {
                extra: Default::default(),
            },
        )
        .await
        .expect("devices list");
    assert_eq!(
        devices.devices.len(),
        1,
        "one machine row after revival: {:?}",
        devices.devices
    );
    assert_eq!(
        devices.devices[0].device_id, old_id,
        "the machine is still known by its named row"
    );
    let new_writer = writer_hex();
    assert_ne!(
        new_writer, old_writer,
        "revival must mint a SUCCESSOR, never resurrect the dead key"
    );

    // The no-data-loss proof: the staged un-pushed value survived the
    // succession and reads back through the revived runtime…
    let entry = handle
        .get_preference(KIND_MODERATION)
        .await
        .expect("get")
        .expect("the tail survived");
    assert_eq!(entry.value, tail_value);
    // …and reached the nest re-authored under the successor: the state feed
    // holds a row whose AUTHORING writer is the successor (never the dead
    // one — the dead writer's tail was local-only by construction).
    let listed: SyncChangesListReply = app_client
        .request(
            "fauna.sync.changes.list",
            SyncChangesListRequest {
                item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
                scope: Some(ACCOUNT_STATE_SCOPE.to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("state feed list");
    let feed_writers: Vec<_> = listed
        .changes
        .iter()
        .filter_map(|c| c.origin_writer.clone())
        .collect();
    assert!(
        feed_writers.contains(&new_writer),
        "the nest must hold the re-authored row under the successor writer; \
         feed writers: {feed_writers:?}"
    );
    assert!(
        !feed_writers.contains(&old_writer),
        "the dead writer's rows were never pushed and must not appear; \
         feed writers: {feed_writers:?}"
    );

    handle.shutdown().await;
}

// ── V11: W5.5 — the SEEDLESS host mounts a store a signed-in app enrolled ────

/// **The app-dead sync agent's assembly** (`account-data-plane.md` § The store
/// device principal, R2 — "replica freshness while no app runs arrives at W5
/// when the agent mounts the store as the always-on host";
/// `apps/sync-agent.md` § Credential model — the agent is a bearer-only +
/// `BackupKey` host and the identity seed is deliberately never in its
/// process).
///
/// The claim under test is that **the seed is not needed to host an already
/// enrolled account**: everything the assembly wants either lives in the shared
/// credential slot a signed-in app populated (the writer key, and the
/// `BackupKey` W5.4a persists *for this consumer*) or is a seed-only leg that
/// is skipped rather than faked (grant mint, fleet bootstrap).
///
/// Both directions are asserted, because a host that could only read would be
/// useless as the always-on engine singleton, while one that could only write
/// would be corrupting the account: the seedless runtime reads the row the app
/// wrote, writes its own, and a seed-holding device opens the result.
///
/// Convention 14: no wall-clock. The app's runtime is fully shut down before
/// the seedless one starts — a sequential handoff, not a race — and every
/// assertion is on state after an awaited `reconcile_now`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v11_a_seedless_runtime_hosts_a_store_a_signed_in_app_enrolled() {
    let (rpc, _state, tmp) = nest().await;
    let base: PathBuf = tmp.path().to_path_buf();

    // ── The signed-in app: the only process here that ever holds the seed.
    // Its assembly mints the writer key, persists the backup key and runs the
    // enrollment ceremony's mint half.
    let from_app = moderation_bytes(&["from-the-app"]);
    {
        let app = AccountStoreRuntime::start(params(&base, "machine", &rpc))
            .await
            .expect("the app's seed-holding assembly");
        app.put_preference(KIND_MODERATION, from_app.clone())
            .await
            .expect("the app writes a row");
        app.reconcile_now().await.expect("the app's pass");
        app.shutdown().await;
    }

    // ── The agent: same machine, same store dir, same credential slot, and no
    // identity seed anywhere in the params.
    let mut seedless = params(&base, "machine", &rpc);
    seedless.principal = RuntimePrincipal::Seedless;
    let agent = AccountStoreRuntime::start(seedless)
        .await
        .expect("the seedless assembly mounts the enrolled store");

    // It inherited the machine's principal rather than establishing its own —
    // T11's "one principal per machine, not one per process", which is what
    // keeps the devices list at one row.
    let bundle = agent
        .principal_bundle_status()
        .await
        .expect("bundle status through the seedless runtime");
    assert!(
        bundle.backup_key_persisted,
        "the seedless host reads the app's persisted backup key — without it \
         there is nothing to unseal account state with"
    );
    assert!(
        bundle.device_authorization.is_some(),
        "the seedless host serves under the grant the app's ceremony minted; \
         minting its own would need the seed and would split one machine into \
         two devices"
    );

    // Read: it opens what the app sealed.
    let seen = agent
        .get_preference(KIND_MODERATION)
        .await
        .expect("get through the seedless host")
        .expect("the app's row is present");
    assert_eq!(
        seen.value, from_app,
        "the seedless host opens the app's sealed row — a mismatch means it \
         resolved a different BackupKey than the account's"
    );

    // Write: its own row lands, and the pass publishes it.
    let from_agent = moderation_bytes(&["from-the-agent"]);
    agent
        .put_preference(KIND_MODERATION, from_agent.clone())
        .await
        .expect("the seedless host writes");
    let report = agent
        .reconcile_now()
        .await
        .expect("the seedless host's pass");
    assert!(
        report.errors.is_empty(),
        "a clean pass with no seed in the process; errors: {:?}",
        report.errors
    );

    // …and a seed-holding device — which every app is — opens what the
    // seedless host wrote. This is the direction that would silently corrupt
    // the account if the host had sealed under the wrong key.
    let app = AccountStoreRuntime::start(params(&base, "second-machine", &rpc))
        .await
        .expect("a seed-holding app on another machine");
    app.reconcile_now().await.expect("its pass");
    assert_eq!(
        app.get_preference(KIND_MODERATION)
            .await
            .expect("get on the seed-holding device")
            .map(|entry| entry.value),
        Some(from_agent),
        "the seedless host's write is readable by a seed-holding app"
    );

    app.shutdown().await;
    agent.shutdown().await;
}

/// The refusal that makes the above safe: on a machine where **no signed-in app
/// has ever enrolled**, the slot carries no writer key — and a seedless host
/// must fail to assemble rather than invent one.
///
/// This is a client-state-recoverability concern (`nest/common.md`
/// § Client-state recoverability), not an ergonomic one: a host that minted a
/// fresh writer identity would leave an orphaned key behind that no nest has
/// ever granted — the exact divergence finding closed by making
/// the seedless leg load-only and check the writer key FIRST, ahead of the
/// backup-key resolution below it (`AccountStoreRuntime::start`,
/// `libs/fauna-sync-engine/src/account_runtime.rs`'s own
/// `a_seedless_assembly_over_an_empty_slot_mints_nothing` pins the same
/// ordering at the unit level). So an empty slot's refusal names the missing
/// writer key, not the backup key it never gets far enough to check.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v11b_a_seedless_runtime_refuses_a_store_no_app_has_enrolled() {
    let (rpc, _state, tmp) = nest().await;
    let base: PathBuf = tmp.path().to_path_buf();

    let mut seedless = params(&base, "never-signed-in", &rpc);
    seedless.principal = RuntimePrincipal::Seedless;
    let outcome = AccountStoreRuntime::start(seedless).await;

    let err = outcome.expect_err("a seedless assembly on an unenrolled machine must refuse");
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("writer key"),
        "the refusal must name the missing carriage, since the recovery is \
         'sign in on this machine once'; got: {rendered}"
    );
}

// ── V12: W5.7 — the peer-leg assembly seam ───────────────────────────────────

/// A [`RouterRequester`] whose `fauna.nest.info` answer is switchable — the
/// three brake-evidence worlds V12b walks: the REAL handler (always-on
/// `peer-sync` advertisement), a nest whose reply omits the capability (the
/// fleet brake pulled), and an unreachable nest (offline start; the cached
/// advertisement decides). Every other kind passes through untouched.
#[derive(Clone)]
struct NestInfoOverride {
    inner: Arc<RouterRequester>,
    mode: Arc<std::sync::Mutex<InfoMode>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InfoMode {
    Real,
    EmptyCaps,
    Unreachable,
}

impl NestInfoOverride {
    fn new(inner: Arc<RouterRequester>, mode: InfoMode) -> Self {
        NestInfoOverride {
            inner,
            mode: Arc::new(std::sync::Mutex::new(mode)),
        }
    }
    fn set_mode(&self, mode: InfoMode) {
        *self.mode.lock().unwrap() = mode;
    }
}

impl RpcRequester for NestInfoOverride {
    type Error = Refused;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply, Refused>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        if kind == "fauna.nest.info" {
            match *self.mode.lock().unwrap() {
                InfoMode::Real => {}
                InfoMode::EmptyCaps => {
                    let reply = fauna_protocol::discovery::NestInfoReply::default();
                    return Ok(
                        fauna_protocol::decode_strict(&encode_canonical(&reply).unwrap()).unwrap(),
                    );
                }
                InfoMode::Unreachable => {
                    return Err(Refused(fauna_protocol::RpcError::new(
                        "test.nest_unreachable",
                        "error.test.nest_unreachable",
                    )));
                }
            }
        }
        self.inner.request(kind, payload).await
    }
}

impl fauna_protocol::KeyedRpcRequester for NestInfoOverride {
    async fn request_keyed<Req, Reply>(
        &self,
        kind: &'static str,
        _idempotency_key: [u8; 16],
        payload: Req,
    ) -> Result<Reply, Refused>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.request(kind, payload).await
    }
}

/// [`params_trusting_deployment`] over the switchable requester, with a peer
/// transport factory installed — the V12 world.
fn v12_params(
    base: &Path,
    device: &str,
    rpc: NestInfoOverride,
    factory: PeerTransportFactory,
) -> AccountRuntimeParams<NestInfoOverride> {
    AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        store_root: StoreRoot::at(base.join(device).join("state")),
        actor_id_hex: account().actor_id_hex(),
        rpc,
        process_rpc: None,
        principal: RuntimePrincipal::SeedHolding(account().into()),
        credentials: CredentialStore::with_file_backend(
            CRED_NAMESPACE,
            base.join(device).join("creds"),
        ),
        reconnects: None,
        pushes: None,
        backstop_interval: Duration::from_secs(3600),
        memberships: None,
        trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(vec![
            deployment_key().verifying_key().to_bytes(),
        ]),
        attested_predecessors: Default::default(),
        linked_nests: None,
        owed_nests: None,
        peer_transport: Some(factory),
        enrollment_target_device_id: device_row(device),
    }
}

/// The in-memory seam-double factory: reports one deterministic non-loopback
/// bound address (so the facts assertion never depends on the test machine's
/// interfaces) and counts invocations (the one-live-endpoint-per-machine
/// refinement's observable: the elected holder constructs at most once).
fn mem_factory(
    listeners: &fauna_transport::testing::Listeners,
    invocations: &Arc<std::sync::atomic::AtomicUsize>,
) -> PeerTransportFactory {
    let listeners = fauna_transport::testing::Listeners::clone(listeners);
    let invocations = Arc::clone(invocations);
    Arc::new(
        move |inputs: fauna_sync_engine::account_runtime::PeerLegFactoryInputs| {
            let listeners = fauna_transport::testing::Listeners::clone(&listeners);
            invocations.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                Ok(PeerLegBinding {
                    transport: Arc::new(fauna_transport::testing::MemTransport {
                        me: fauna_transport::EndpointKey::from_bytes(
                            inputs.writer_key.verifying_key().to_bytes(),
                        ),
                        listeners,
                    }),
                    bound_addrs: vec!["203.0.113.7:4711".parse().unwrap()],
                    file_sync: None,
                })
            })
        },
    )
}

/// A root-signed admission witness over `device_key` — what a sibling device
/// presents when dialing in (`sign_root` is the account for the admitted case,
/// a stranger for the refused one).
fn dial_witness(
    sign_root: &fauna_core::identity::ActorKeypair,
    device_key: [u8; 32],
) -> fauna_core::encoding::EmbedAsBytes {
    let cert = fauna_core::data::DeviceAuthorization {
        actor_id: sign_root.actor_id(),
        device_key,
        capabilities: vec![fauna_core::data::Capability::RenewBearer],
        created_at: fauna_core::data::Timestamp(1_000),
        expires_at: None,
    };
    let (bytes, env) = fauna_core::encoding::sign_envelope(sign_root, &cert).expect("sign witness");
    fauna_core::encoding::EmbedAsBytes::from_signed(bytes, env)
}

fn epoch_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs_or_zero() as u64
}

/// **W5.7, the positive half**: with a transport
/// factory installed, an enrolled principal in the slot, and the REAL nest
/// advertising the always-on `peer-sync` brake token, the elected runtime
/// **binds the peer listener itself** — and the bind's observed transport
/// facts join the device-endpoints floor row on the same pass (dial candidates
/// beside the `node_id`), which is the slice's own success sentence.
///
/// The dial side then proves the listener is *admitting* and *refusing*: a
/// sibling device with a root-signed witness admits and walks A's relay plane
/// pull-only (reading the preference A published), while a witness signed by
/// a stranger root is refused at the admission exchange.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v12_the_elected_runtime_binds_the_peer_listener_and_a_sibling_walks_it() {
    let (rpc, _state, tmp) = nest_with_escrow().await;
    let base: PathBuf = tmp.path().to_path_buf();
    let listeners = fauna_transport::testing::listeners();
    let invocations = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let a = AccountStoreRuntime::start(v12_params(
        &base,
        "a",
        NestInfoOverride::new(Arc::clone(&rpc), InfoMode::Real),
        mem_factory(&listeners, &invocations),
    ))
    .await
    .expect("start A");
    a.put_preference(KIND_MODERATION, moderation_bytes(&["over-the-peer-leg"]))
        .await
        .expect("A writes a preference");

    // The bind and the facts publish ride ordinary passes: the report is the
    // causal observable, the published floor row the durable one.
    eventually(
        "the holder binds and its dial candidates join the floor row",
        || {
            let a = a.clone();
            async move {
                let report = a.reconcile_now().await.expect("A pass");
                let bound = matches!(
                    report.peer_leg,
                    Some(PeerLegPass::Bound | PeerLegPass::AlreadyBound)
                );
                let rows = endpoint_rows(&a).await;
                bound
                    && rows.iter().any(|(_, v)| {
                        v.lan_addrs == vec!["203.0.113.7:4711".to_string()] && v.relay_url.is_none()
                    })
            }
        },
    )
    .await;
    assert_eq!(
        invocations.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the factory ran exactly once — AlreadyBound thereafter (one live \
         endpoint per machine)"
    );

    // The listener's transport identity is the machine's device principal
    // (R5): the registered key IS the enrollment grant's device key.
    let server_key = a
        .principal_bundle_status()
        .await
        .expect("bundle status")
        .device_authorization
        .expect("enrolled")
        .device_key;
    fauna_transport::testing::await_listening(&listeners, &server_key).await;

    // Admitting: a sibling device (root-signed witness) dials in, admits
    // mutually, and walks A's relay plane pull-only into its own store.
    let dialer_sk = SigningKey::from_bytes(&[0x77; 32]);
    let dialer_pub = dialer_sk.verifying_key().to_bytes();
    use fauna_transport::PeerTransport as _;
    let dial_transport = fauna_transport::testing::MemTransport {
        me: fauna_transport::EndpointKey::from_bytes(dialer_pub),
        listeners: fauna_transport::testing::Listeners::clone(&listeners),
    };
    let conn = dial_transport
        .dial(
            fauna_transport::EndpointKey::from_bytes(server_key),
            fauna_transport::PathCandidates::default(),
        )
        .await
        .expect("dial the runtime's listener");
    let channel = Arc::new(
        fauna_peer_channel::PeerChannel::open(conn)
            .await
            .expect("channel"),
    );
    fauna_peer_sync::admit_over(
        &channel,
        dial_witness(&account(), dialer_pub),
        &actor_bytes(),
        epoch_secs(),
    )
    .await
    .expect("a root-signed sibling witness admits");

    let scratch = tempfile::tempdir().unwrap();
    let dialer_store = fauna_account_store::store::AccountStore::open(
        fauna_account_store::sqlite::SqliteBackend::open(scratch.path()).unwrap(),
        &account().actor_id_hex(),
        WriterId(dialer_pub),
    )
    .await
    .unwrap();
    let requester = fauna_peer_sync::PeerRequester::new(Arc::clone(&channel));
    let schedule = fauna_core::crypto::AccountStateKeySchedule::derive(
        &fauna_core::crypto::BackupKey::derive(account().secret_bytes()),
    );
    let trust = fauna_sync_engine::generation_tip::GenerationTrust {
        root: account().actor_id(),
        prior: Vec::new(),
        trusted_holders: vec![deployment_key().verifying_key().to_bytes()].into(),
    };
    let plane = fauna_sync_engine::account_state_plane::AccountStatePlane::new_pull_only(
        &dialer_store,
        &requester,
        &schedule,
        &dialer_sk,
        &trust,
        ACCOUNT_STATE_SCOPE,
    )
    .unwrap();
    plane
        .walk()
        .await
        .expect("walk the runtime-hosted listener");
    let got = dialer_store
        .state(
            KIND_MODERATION,
            fauna_protocol::merge_policy::PREFERENCE_KEY,
        )
        .await
        .unwrap()
        .expect("A's preference row arrived over the peer leg");
    let decoded: ModerationConfig = canonical_decode(&got.value).unwrap();
    assert_eq!(
        decoded
            .muted_keywords
            .iter()
            .map(|k| k.keyword.clone())
            .collect::<Vec<_>>(),
        vec!["over-the-peer-leg".to_string()]
    );

    // Refusing: a witness signed by a STRANGER root is refused at the
    // admission exchange — possession of a well-formed envelope conveys
    // nothing (the admission seam's proven-key rule).
    let stranger = fauna_core::identity::ActorKeypair::from_secret([0x55; 32]);
    let conn = dial_transport
        .dial(
            fauna_transport::EndpointKey::from_bytes(server_key),
            fauna_transport::PathCandidates::default(),
        )
        .await
        .expect("dial again");
    let channel = fauna_peer_channel::PeerChannel::open(conn)
        .await
        .expect("channel");
    fauna_peer_sync::admit_over(
        &channel,
        dial_witness(&stranger, dialer_pub),
        &actor_bytes(),
        epoch_secs(),
    )
    .await
    .expect_err("a stranger-signed witness must be refused");

    a.shutdown().await;
}

/// **W5.7, the brake gates** (wormability rule 7 — the `peer-sync` token is
/// the fleet brake, refused client-side at the one bind door — plus the
/// offline posture the outage matrix demands): a nest that does not advertise
/// the token keeps the leg down with the factory never invoked; the
/// advertisement, once seen, is cached in the store's meta table so a start
/// with the nest unreachable still binds from the last-known brake state; and
/// a store that has NEVER seen an advertisement refuses by default.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v12b_the_brake_gates_the_bind_and_the_cached_advertisement_covers_offline_starts() {
    let (rpc, _state, tmp) = nest_with_escrow().await;
    let base: PathBuf = tmp.path().to_path_buf();
    let listeners = fauna_transport::testing::listeners();
    let invocations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let rpc_override = NestInfoOverride::new(Arc::clone(&rpc), InfoMode::EmptyCaps);

    // Brake on: the reply carries no `peer-sync`, so the pass reports BrakeOn,
    // no listener registers, and the factory never runs (the gate order is
    // part of the contract — a braked leg constructs no endpoint).
    let b = AccountStoreRuntime::start(v12_params(
        &base,
        "b",
        rpc_override.clone(),
        mem_factory(&listeners, &invocations),
    ))
    .await
    .expect("start B");
    let report = b.reconcile_now().await.expect("braked pass");
    assert_eq!(report.peer_leg, Some(PeerLegPass::BrakeOn));
    assert_eq!(invocations.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(
        listeners.lock().unwrap().is_empty(),
        "no listener may exist while the brake is on"
    );

    // The real advertisement lifts it — and is cached in the store's meta
    // table as it binds.
    rpc_override.set_mode(InfoMode::Real);
    let report = b.reconcile_now().await.expect("unbraked pass");
    assert_eq!(report.peer_leg, Some(PeerLegPass::Bound));
    b.shutdown().await;

    // Offline start: the nest is unreachable, but the SAME store dir carries
    // the cached advertisement — the leg comes up (the outage matrix's "same
    // LAN, WAN down" is the peer leg's whole point).
    rpc_override.set_mode(InfoMode::Unreachable);
    let b2 = AccountStoreRuntime::start(v12_params(
        &base,
        "b",
        rpc_override.clone(),
        mem_factory(&listeners, &invocations),
    ))
    .await
    .expect("restart B offline");
    let report = b2.reconcile_now().await.expect("offline pass");
    // `AlreadyBound` is the ordinary answer here: the restart's own PROLOGUE
    // pass (b2 is the holder from birth — b released the lock) already bound
    // from the cache before this explicit pass ran. The invocation counter is
    // what proves a real second bind happened offline.
    assert!(
        matches!(
            report.peer_leg,
            Some(PeerLegPass::Bound | PeerLegPass::AlreadyBound)
        ),
        "the cached advertisement covers an offline start: {:?} (errors: {:?})",
        report.peer_leg,
        report.errors
    );
    assert_eq!(
        invocations.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "the offline restart constructed its own endpoint from the cache"
    );
    b2.shutdown().await;

    // No evidence at all: a fresh store that has never seen an advertisement
    // refuses by default — the door errs toward the brake, never optimism.
    let c = AccountStoreRuntime::start(v12_params(
        &base,
        "c",
        rpc_override.clone(),
        mem_factory(&listeners, &invocations),
    ))
    .await
    .expect("start C offline");
    let report = c.reconcile_now().await.expect("evidence-less pass");
    assert_eq!(report.peer_leg, Some(PeerLegPass::NoBrakeEvidence));
    c.shutdown().await;
}

// ── A row refused for room is parked (`account-replica-posture.md` § The
// store device principal, refinement 11 → *A row refused for room is
// parked*) — the measured flow on the real handlers ─────────────────────────

/// The scope is filled to its cap directly through the nest's own door under a
/// second writer id (a fill through a device took 100 s on a debug build). A
/// device's new item is then refused for room and parked, and the device's
/// rewrite of an item the nest already holds a row of lands in the same pass:
/// a fresh device reads it. Before the build the rewrite stayed on the device
/// behind the refused marker, pass after pass.
#[tokio::test]
async fn a_full_delegable_scope_parks_a_new_item_and_lands_the_rest() {
    use fauna_protocol::account_state::MAX_STATE_ENTRIES_PER_SCOPE;
    let (rpc, state, tmp) = nest().await;
    let base = tmp.path();
    let a = AccountStoreRuntime::start(params(base, "a", &rpc))
        .await
        .expect("start A");
    a.put_preference(KIND_MODERATION, moderation_bytes(&["one"]))
        .await
        .expect("put");
    a.settled().await;
    a.reconcile_now().await.expect("pass");

    let folder = state
        .db
        .find_state_scope(&actor_bytes(), ACCOUNT_STATE_SCOPE)
        .await
        .unwrap()
        .expect("A's put created the scope");
    let live = state
        .db
        .get_account_state_changes(folder, 0, &std::collections::BTreeMap::new(), None, None)
        .await
        .unwrap()
        .rows
        .len() as i64;
    let filler = [0xEE; 32];
    for i in live..MAX_STATE_ENTRIES_PER_SCOPE {
        let mut item = [0u8; 32];
        item[..8].copy_from_slice(&i.to_be_bytes());
        state
            .db
            .record_account_state_entry(
                &actor_bytes(),
                folder,
                &item,
                &filler,
                i + 1,
                "state-put",
                b"a filler row no device opens",
                None,
                &[],
            )
            .await
            .unwrap()
            .expect("room under the cap");
    }

    let channel = "4e".repeat(32);
    assert!(a.raise_read_marker(&channel, 1).await.expect("raise"));
    a.put_preference(KIND_MODERATION, moderation_bytes(&["two"]))
        .await
        .expect("the rewrite is durable locally");
    a.settled().await;
    for pass in 0..2 {
        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(
            report.parked,
            Some(1),
            "pass {pass}: the new marker is parked: {:?}",
            report.errors
        );
        assert!(
            !report.errors.iter().any(|e| e.contains("publish_pending")),
            "pass {pass}: a parked row is no publish failure: {:?}",
            report.errors
        );
    }

    let fresh = AccountStoreRuntime::start(params(base, "fresh", &rpc))
        .await
        .expect("start the fresh device");
    fresh.reconcile_now().await.expect("pass");
    assert_eq!(
        fresh
            .get_preference(KIND_MODERATION)
            .await
            .expect("get")
            .map(|e| e.value),
        Some(moderation_bytes(&["two"])),
        "the rewrite reached the nest past the parked row"
    );
    assert!(
        !fresh
            .read_markers()
            .await
            .expect("markers")
            .iter()
            .any(|(c, _)| *c == channel),
        "the parked marker has no room"
    );
    fresh.shutdown().await;
    a.shutdown().await;
}
