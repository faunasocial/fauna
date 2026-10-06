//! Integration round-trip for the generalized account-data feed's class-2 leg —
//! `fauna.account.state.put` and `fauna.sync.changes.list`'s `item_class` /
//! `frontier` arms (W2.3 (account-data-plane.md § Workstreams), `docs/goal/architecture/account-sync-plane.md`
//! § Feeds and cursors).
//!
//! The target flow this file drives end to end: a class-2 write kind verifies
//! coordinates → inserts the sealed entry + a `sync_changes` row (`item_class =
//! state-entry`, `path_hash` = the blinded item key, origin coords) →
//! `fauna.sync.changes.list` with an optional `frontier` pages it back with
//! per-row origin coordinates → **a bare `since` without `item_class` (file sync)
//! sees exactly the file rows**.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;
use common::dispatch;
use common::encode;

use std::collections::BTreeMap;
use std::sync::Arc;

use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    folder_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
    sync_handlers,
};
use fauna_protocol::{
    ByteBuf, RpcError,
    account_state::{
        ACCOUNT_STATE_FLEET_SCOPE, ACCOUNT_STATE_SCOPE, AccountStatePutReply,
        AccountStatePutRequest, ItemClass, KIND_STATE_PUT, MAX_REPLACED_ROWS_PER_PUT,
        MAX_STATE_ENTRY_BYTES, OP_STATE_PUT, OP_TOMBSTONE, ReplacedRow,
    },
    decode_strict as decode,
    folders::{FolderCreateReply, FolderCreateRequest},
    sync::{
        SyncChangeRecordReply, SyncChangeRecordRequest, SyncChangesListReply,
        SyncChangesListRequest, SyncRegisterRequest,
    },
};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    (b.build(), state)
}

fn id(b: u8) -> [u8; 32] {
    [b; 32]
}
fn id_hex(b: u8) -> String {
    hex::encode(id(b))
}

const ACTOR: u8 = 0xA1;
const OTHER_ACTOR: u8 = 0xA2;
const WRITER_A: u8 = 0x0A;
const WRITER_B: u8 = 0x0B;
const ITEM: u8 = 0x11;
const ITEM2: u8 = 0x22;

fn put_req(item: u8, writer: u8, writer_seq: i64, entry: &[u8]) -> AccountStatePutRequest {
    AccountStatePutRequest {
        scope: ACCOUNT_STATE_SCOPE.into(),
        writer_id: id_hex(writer),
        writer_seq,
        item_key: ByteBuf::from(id(item).to_vec()),
        op: OP_STATE_PUT.into(),
        entry: ByteBuf::from(entry.to_vec()),
        ..Default::default()
    }
}

async fn put(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: u8,
    req: &AccountStatePutRequest,
) -> Result<AccountStatePutReply, RpcError> {
    let bytes = dispatch(
        router,
        Arc::clone(state),
        id(actor),
        KIND_STATE_PUT,
        encode(req),
    )
    .await?;
    Ok(decode(&bytes).unwrap())
}

/// `fauna.sync.changes.list` routed at the class-2 feed.
async fn list_state(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: u8,
    since: i64,
    frontier: Option<BTreeMap<String, i64>>,
) -> SyncChangesListReply {
    let bytes = dispatch(
        router,
        Arc::clone(state),
        id(actor),
        "fauna.sync.changes.list",
        encode(&SyncChangesListRequest {
            item_class: Some(ItemClass::StateEntry.as_wire().into()),
            since,
            frontier,
            ..Default::default()
        }),
    )
    .await
    .expect("state feed list ok");
    decode(&bytes).unwrap()
}

/// The **whole target flow** in one test: write → feed → per-row origin
/// coordinates, and the sealed bytes survive the round trip untouched.
#[tokio::test]
async fn a_sealed_entry_lands_and_pages_back_with_its_origin_coordinates() {
    let (router, state) = router_and_state().await;

    let reply = put(
        &router,
        &state,
        ACTOR,
        &put_req(ITEM, WRITER_A, 1, b"sealed-v1"),
    )
    .await
    .expect("put ok");
    assert!(reply.seq > 0, "the reply carries the nest-log seq");

    let listed = list_state(&router, &state, ACTOR, 0, None).await;
    assert_eq!(listed.changes.len(), 1);
    let row = &listed.changes[0];
    assert_eq!(row.seq, reply.seq);
    assert_eq!(
        row.item_class.as_deref(),
        Some(ItemClass::StateEntry.as_wire())
    );
    assert_eq!(
        row.origin_writer.as_deref(),
        Some(id_hex(WRITER_A).as_str())
    );
    assert_eq!(row.origin_seq, Some(1));
    assert_eq!(
        row.path_hash,
        id_hex(ITEM),
        "the blinded item key rides the shipped path_hash slot"
    );
    assert_eq!(
        row.entry.as_ref().map(|e| e.to_vec()),
        Some(b"sealed-v1".to_vec()),
        "the sealed envelope is echoed byte-for-byte"
    );
    assert_eq!(row.change_type, OP_STATE_PUT);
}

/// Driven through the real wire: a file-sync reader's request shape — no
/// `item_class` — must see only file rows, even though the actor now has
/// class-2 rows. The folder-less branch is actor-keyed with no folder filter, so without the exclusion this is precisely where they
/// would leak.
#[tokio::test]
async fn a_bare_since_without_item_class_sees_only_file_rows() {
    let (router, state) = router_and_state().await;
    // The recording actor is a real keypair: a file-sync client's record is
    // signed by its identity key under the set's stored nonce, and an unsigned
    // one is refused `signature_required`.
    let kp = common::signing_actor(ACTOR);
    let actor = kp.actor_id().0;

    // An ordinary folder change, the way a shipped client records one.
    let _set = {
        let reply: FolderCreateReply = decode(
            &dispatch(
                &router,
                Arc::clone(&state),
                actor,
                "fauna.folders.create",
                encode(&FolderCreateRequest {
                    name: "docs".into(),
                    set_nonce: common::set_nonce_field(),
                    ..Default::default()
                }),
            )
            .await
            .expect("create ok"),
        )
        .unwrap();
        reply.id
    };
    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.register",
        encode(&SyncRegisterRequest {
            device_id: id_hex(0xD1),
            label: "test device".into(),
            capabilities: "read,write".into(),
            ..Default::default()
        }),
    )
    .await
    .expect("register ok");
    let recorded: SyncChangeRecordReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.changes.record",
            encode(&common::signed_record(
                SyncChangeRecordRequest {
                    folder: "docs".into(),
                    device_id: id_hex(0xD1),
                    path: "a.txt".into(),
                    manifest_hash: Some(id_hex(0x55)),
                    size_bytes: 12,
                    change_type: "create".into(),
                    path_sealed: Some(ByteBuf::from(b"sealed-label".to_vec())),
                    ..Default::default()
                },
                &kp,
            )),
        )
        .await
        .expect("record ok"),
    )
    .unwrap();

    // ...and a class-2 entry beside it, by the same actor.
    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        KIND_STATE_PUT,
        encode(&put_req(ITEM, WRITER_A, 1, b"sealed")),
    )
    .await
    .expect("put ok");

    // The folder's own feed: no item_class, no frontier.
    let bytes = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.changes.list",
        encode(&SyncChangesListRequest {
            folder: Some("docs".into()),
            since: 0,
            ..Default::default()
        }),
    )
    .await
    .expect("folder list ok");
    let bare: SyncChangesListReply = decode(&bytes).unwrap();

    assert_eq!(
        bare.changes.len(),
        1,
        "exactly the file row; the state entry is invisible to a client that did not ask"
    );
    assert_eq!(bare.changes[0].seq, recorded.seq);
    assert!(bare.changes[0].item_class.is_none());
    assert!(bare.changes[0].entry.is_none());
}

/// The frontier is a **vector**: satisfying one writer's slot hides that
/// writer's rows and nobody else's. `since` stays the nest-writer slot, which is
/// what "an omitted frontier is `{nest: since}`" means.
#[tokio::test]
async fn the_frontier_pages_each_writer_independently() {
    let (router, state) = router_and_state().await;

    put(&router, &state, ACTOR, &put_req(ITEM, WRITER_A, 1, b"a1"))
        .await
        .unwrap();
    put(&router, &state, ACTOR, &put_req(ITEM2, WRITER_A, 2, b"a2"))
        .await
        .unwrap();
    put(&router, &state, ACTOR, &put_req(ITEM, WRITER_B, 7, b"b7"))
        .await
        .unwrap();

    let mut frontier = BTreeMap::new();
    frontier.insert(id_hex(WRITER_A), 1i64);
    let listed = list_state(&router, &state, ACTOR, 0, Some(frontier.clone())).await;
    let bodies: Vec<Vec<u8>> = listed
        .changes
        .iter()
        .map(|c| c.entry.as_ref().unwrap().to_vec())
        .collect();
    assert_eq!(
        bodies,
        vec![b"a2".to_vec(), b"b7".to_vec()],
        "A's satisfied row drops out; B, absent from the map, stands at 0"
    );

    frontier.insert(id_hex(WRITER_A), 2);
    frontier.insert(id_hex(WRITER_B), 7);
    assert!(
        list_state(&router, &state, ACTOR, 0, Some(frontier))
            .await
            .changes
            .is_empty(),
        "a fully-caught-up frontier is owed nothing"
    );
}

// ── The serve-order watermark ────────────────────────────────────────────────
//
// Owner: `docs/goal/architecture/account-sync-plane.md` § Feeds and cursors →
// *Compaction is a serve-order watermark*. The request's `held_through_seq`
// is the third gate; the reply's `complete_through_seq` is both the watermark's
// only source and the signal that this nest honours it.

/// [`list_state`] with the watermark field — the request shape a requester
/// that has banked an echo sends.
async fn list_state_held(
    router: &RpcRouter,
    state: &Arc<AppState>,
    frontier: Option<BTreeMap<String, i64>>,
    held_through_seq: Option<i64>,
) -> SyncChangesListReply {
    let bytes = dispatch(
        router,
        Arc::clone(state),
        id(ACTOR),
        "fauna.sync.changes.list",
        encode(&SyncChangesListRequest {
            item_class: Some(ItemClass::StateEntry.as_wire().into()),
            frontier,
            held_through_seq,
            ..Default::default()
        }),
    )
    .await
    .expect("state feed list ok");
    decode(&bytes).unwrap()
}

fn bodies(reply: &SyncChangesListReply) -> Vec<Vec<u8>> {
    reply
        .changes
        .iter()
        .map(|c| c.entry.as_ref().unwrap().to_vec())
        .collect()
}

/// The third gate, through the wire: an UNNAMED writer is served from above
/// the watermark instead of from 0, a NAMED writer keeps its own slot whatever
/// the watermark says, and a request with no watermark is served exactly as
/// before — the new nest facing an old requester.
#[tokio::test]
async fn the_watermark_serves_unnamed_writers_above_it_and_named_ones_by_their_slot() {
    let (router, state) = router_and_state().await;

    put(&router, &state, ACTOR, &put_req(ITEM, WRITER_A, 1, b"a1"))
        .await
        .unwrap();
    let a2 = put(&router, &state, ACTOR, &put_req(ITEM2, WRITER_A, 2, b"a2"))
        .await
        .unwrap()
        .seq;
    let b7 = put(&router, &state, ACTOR, &put_req(ITEM, WRITER_B, 7, b"b7"))
        .await
        .unwrap()
        .seq;

    assert_eq!(
        bodies(&list_state_held(&router, &state, None, Some(a2)).await),
        vec![b"b7".to_vec()],
        "both writers unnamed: A's rows sit at or below the watermark, B's above it"
    );

    let named_a = BTreeMap::from([(id_hex(WRITER_A), 1i64)]);
    assert_eq!(
        bodies(&list_state_held(&router, &state, Some(named_a), Some(b7)).await),
        vec![b"a2".to_vec()],
        "A is named, so its slot — not the watermark — gates it; B is unnamed and \
         wholly at or below the watermark"
    );

    assert_eq!(
        bodies(&list_state_held(&router, &state, None, None).await),
        vec![b"a1".to_vec(), b"a2".to_vec(), b"b7".to_vec()],
        "no watermark: every unnamed writer stands at 0, byte-for-byte the two-gate serve"
    );
}

/// The echo on a page that left nothing behind is the scope's log tip — on a
/// full page, on a page every row of which was gated, and on a page the
/// watermark alone emptied. That last shape is how a converged walk banks the
/// whole log. A scope this nest has never been written to echoes nothing:
/// there is no log to be complete through.
#[tokio::test]
async fn the_echo_is_the_scope_tip_on_a_page_that_left_nothing_behind() {
    let (router, state) = router_and_state().await;

    put(&router, &state, ACTOR, &put_req(ITEM, WRITER_A, 1, b"a1"))
        .await
        .unwrap();
    let tip = put(&router, &state, ACTOR, &put_req(ITEM, WRITER_B, 7, b"b7"))
        .await
        .unwrap()
        .seq;

    let full = list_state_held(&router, &state, None, None).await;
    assert_eq!(full.changes.len(), 2);
    assert_eq!(full.complete_through_seq, Some(tip), "a full page");

    let caught_up = BTreeMap::from([(id_hex(WRITER_A), 1i64), (id_hex(WRITER_B), 7)]);
    let gated = list_state_held(&router, &state, Some(caught_up), None).await;
    assert!(gated.changes.is_empty());
    assert_eq!(
        gated.complete_through_seq,
        Some(tip),
        "an empty page whose rows were all gated by slot still carries the tip"
    );

    let held = list_state_held(&router, &state, None, Some(tip)).await;
    assert!(held.changes.is_empty());
    assert_eq!(
        held.complete_through_seq,
        Some(tip),
        "and so does one the watermark emptied"
    );

    let never_written = decode::<SyncChangesListReply>(
        &dispatch(
            &router,
            Arc::clone(&state),
            id(OTHER_ACTOR),
            "fauna.sync.changes.list",
            encode(&SyncChangesListRequest {
                item_class: Some(ItemClass::StateEntry.as_wire().into()),
                ..Default::default()
            }),
        )
        .await
        .expect("state feed list ok"),
    )
    .unwrap();
    assert!(never_written.changes.is_empty());
    assert_eq!(never_written.complete_through_seq, None);
}

/// The echo on a frame-truncated page is the last row the page actually
/// carries — never the tip the nest computed before cutting it — and a
/// requester that sends it back as its watermark, naming nobody, is served
/// exactly the rows the cut left behind.
#[tokio::test]
async fn the_echo_is_the_last_served_seq_on_a_frame_truncated_page() {
    let (router, state) = router_and_state().await;

    // Forty maximum-size entries overflow one WS frame's page budget.
    let mut seqs = Vec::new();
    for i in 0..40u8 {
        let entry = vec![i; MAX_STATE_ENTRY_BYTES];
        let reply = put(
            &router,
            &state,
            ACTOR,
            &put_req(0x40 + i, WRITER_A, i64::from(i) + 1, &entry),
        )
        .await
        .expect("put ok");
        seqs.push(reply.seq);
    }
    let tip = *seqs.last().unwrap();

    let first = list_state_held(&router, &state, None, None).await;
    assert!(
        !first.changes.is_empty() && first.changes.len() < seqs.len(),
        "fixture: the page budget must cut this page ({} of {} rows)",
        first.changes.len(),
        seqs.len()
    );
    let cut = first.changes.last().unwrap().seq;
    assert_eq!(
        first.complete_through_seq,
        Some(cut),
        "complete through the last row the page carries"
    );
    assert_ne!(cut, tip);

    let rest = list_state_held(&router, &state, None, Some(cut)).await;
    let served: Vec<i64> = first
        .changes
        .iter()
        .chain(rest.changes.iter())
        .map(|c| c.seq)
        .collect();
    assert_eq!(
        served, seqs,
        "the watermark resumes the walk exactly where the cut left it"
    );
    assert_eq!(rest.complete_through_seq, Some(tip));
}

/// Multi-master, through the wire: two writers' entries for one item both stay
/// on the feed. Collapsing across writers would hand the reader-side merge seam
/// a single survivor and silently make the plane last-writer-wins.
#[tokio::test]
async fn two_writers_entries_for_one_item_both_stay_on_the_feed() {
    let (router, state) = router_and_state().await;

    put(
        &router,
        &state,
        ACTOR,
        &put_req(ITEM, WRITER_A, 1, b"from-a"),
    )
    .await
    .unwrap();
    put(
        &router,
        &state,
        ACTOR,
        &put_req(ITEM, WRITER_B, 1, b"from-b"),
    )
    .await
    .unwrap();
    // ...while a writer's own newer entry does replace its predecessor.
    put(
        &router,
        &state,
        ACTOR,
        &put_req(ITEM, WRITER_A, 2, b"from-a-2"),
    )
    .await
    .unwrap();

    let listed = list_state(&router, &state, ACTOR, 0, None).await;
    let mut bodies: Vec<Vec<u8>> = listed
        .changes
        .iter()
        .map(|c| c.entry.as_ref().unwrap().to_vec())
        .collect();
    bodies.sort();
    assert_eq!(bodies, vec![b"from-a-2".to_vec(), b"from-b".to_vec()]);
}

/// A tombstone is an ordinary sealed entry; only its cleartext op distinguishes
/// it, and that is the only thing a key-less position may read.
#[tokio::test]
async fn a_tombstone_is_an_ordinary_sealed_entry_with_its_own_op() {
    let (router, state) = router_and_state().await;

    put(
        &router,
        &state,
        ACTOR,
        &put_req(ITEM, WRITER_A, 1, b"value"),
    )
    .await
    .unwrap();
    put(
        &router,
        &state,
        ACTOR,
        &AccountStatePutRequest {
            op: OP_TOMBSTONE.into(),
            ..put_req(ITEM, WRITER_A, 2, b"sealed-tombstone")
        },
    )
    .await
    .unwrap();

    let listed = list_state(&router, &state, ACTOR, 0, None).await;
    assert_eq!(listed.changes.len(), 1);
    assert_eq!(listed.changes[0].change_type, OP_TOMBSTONE);
    assert_eq!(
        listed.changes[0].entry.as_ref().map(|e| e.to_vec()),
        Some(b"sealed-tombstone".to_vec()),
        "the deletion carries a sealed payload like any other entry"
    );
}

/// Each refusal gets its own code, because the client's remedy differs: a CAS
/// loser re-reads and retries, a stale replay is dropped, a full scope is
/// surfaced.
#[tokio::test]
async fn the_three_refusals_carry_distinguishable_codes() {
    let (router, state) = router_and_state().await;

    put(&router, &state, ACTOR, &put_req(ITEM, WRITER_A, 5, b"v5"))
        .await
        .unwrap();

    let stale = put(
        &router,
        &state,
        ACTOR,
        &put_req(ITEM, WRITER_A, 5, b"replay"),
    )
    .await
    .expect_err("a replayed writer_seq is refused");
    assert_eq!(
        stale.code,
        fauna_protocol::RpcError::CODE_ACCOUNT_STATE_STALE_WRITER_SEQ,
        "the code the client classifies on (`is_account_state_stale_writer_seq`) is the \
         one the handler emits"
    );

    let cas = put(
        &router,
        &state,
        ACTOR,
        &AccountStatePutRequest {
            cas_base: Some(99),
            ..put_req(ITEM, WRITER_A, 6, b"v6")
        },
    )
    .await
    .expect_err("a mismatched CAS base is refused");
    assert_eq!(cas.code, "fauna.account.state.cas_mismatch");

    // The head is untouched by either refusal.
    let listed = list_state(&router, &state, ACTOR, 0, None).await;
    assert_eq!(
        listed.changes[0].entry.as_ref().map(|e| e.to_vec()),
        Some(b"v5".to_vec())
    );
}

/// A writer's `(scope, writer_seq)` coordinate is never recorded twice — on a
/// different item, and after the first row was collapsed by a later put — and
/// the refusal rides the same `stale_writer_seq` code as a non-advancing head,
/// since the client's remedy is the same (its journal is burnt;
/// `account-replica-posture.md` § The store device principal, refinement 11).
/// Every replica keys row uniqueness on this coordinate, so a second row at it
/// would be accepted here and equivocate at each of them.
#[tokio::test]
async fn a_coordinate_this_writer_already_used_is_refused_on_any_item() {
    let (router, state) = router_and_state().await;

    put(&router, &state, ACTOR, &put_req(ITEM, WRITER_A, 5, b"v5"))
        .await
        .unwrap();
    // Collapse the seq-5 row under a later put on the same item: the
    // coordinate memory must outlive the row's liveness.
    put(&router, &state, ACTOR, &put_req(ITEM, WRITER_A, 7, b"v7"))
        .await
        .unwrap();

    let reused = put(
        &router,
        &state,
        ACTOR,
        &put_req(ITEM2, WRITER_A, 5, b"a-fresh-journal-reissuing-seq-5"),
    )
    .await
    .expect_err("the collapsed coordinate is refused on another item");
    assert_eq!(
        reused.code,
        fauna_protocol::RpcError::CODE_ACCOUNT_STATE_STALE_WRITER_SEQ,
        "the same code as a non-advancing head: the client's remedy is the same"
    );

    // Another writer's seq 5 is another coordinate entirely, and the same
    // writer's next unused seq on that item lands.
    put(&router, &state, ACTOR, &put_req(ITEM2, WRITER_B, 5, b"b5"))
        .await
        .unwrap();
    put(&router, &state, ACTOR, &put_req(ITEM2, WRITER_A, 8, b"a8"))
        .await
        .unwrap();
    let listed = list_state(&router, &state, ACTOR, 0, None).await;
    let mut coordinates: Vec<(String, i64)> = listed
        .changes
        .iter()
        .map(|c| (c.origin_writer.clone().unwrap(), c.origin_seq.unwrap()))
        .collect();
    coordinates.sort();
    assert_eq!(
        coordinates,
        vec![
            (id_hex(WRITER_A), 7),
            (id_hex(WRITER_A), 8),
            (id_hex(WRITER_B), 5),
        ],
        "one live row per (item, writer); the refused reuse left nothing behind"
    );
}

#[tokio::test]
async fn malformed_coordinates_are_refused_before_anything_is_written() {
    let (router, state) = router_and_state().await;

    let cases: Vec<(&str, AccountStatePutRequest)> = vec![
        (
            "an unknown scope",
            AccountStatePutRequest {
                scope: "not-a-scope".into(),
                ..put_req(ITEM, WRITER_A, 1, b"x")
            },
        ),
        (
            "an unknown op",
            AccountStatePutRequest {
                op: "create".into(),
                ..put_req(ITEM, WRITER_A, 1, b"x")
            },
        ),
        (
            "a non-hex writer id",
            AccountStatePutRequest {
                writer_id: "zz".into(),
                ..put_req(ITEM, WRITER_A, 1, b"x")
            },
        ),
        (
            "a short item key",
            AccountStatePutRequest {
                item_key: ByteBuf::from(vec![0u8; 8]),
                ..put_req(ITEM, WRITER_A, 1, b"x")
            },
        ),
        ("an empty entry", put_req(ITEM, WRITER_A, 1, b"")),
        (
            "an oversized entry",
            put_req(ITEM, WRITER_A, 1, &vec![0u8; MAX_STATE_ENTRY_BYTES + 1]),
        ),
    ];

    for (what, req) in cases {
        let err = put(&router, &state, ACTOR, &req)
            .await
            .expect_err(&format!("{what} must be refused"));
        assert_eq!(
            err.code, "fauna.account.state.invalid_request",
            "the refusal for {what} is namespaced to its own kind"
        );
    }

    assert!(
        list_state(&router, &state, ACTOR, 0, None)
            .await
            .changes
            .is_empty(),
        "no refused request wrote a row"
    );
}

/// The scope is derived from the connection actor and carries no actor field,
/// so one account's entries are unreachable from another's connection.
#[tokio::test]
async fn one_actors_scope_is_invisible_to_another() {
    let (router, state) = router_and_state().await;

    put(&router, &state, ACTOR, &put_req(ITEM, WRITER_A, 1, b"mine"))
        .await
        .unwrap();

    assert!(
        list_state(&router, &state, OTHER_ACTOR, 0, None)
            .await
            .changes
            .is_empty(),
        "another actor's scope is empty, not shared"
    );
}

/// **Both halves of the reserved-scope rule** (`account-sync-plane.md` § Feeds
/// and cursors → *Feed row + wire evolution* — the explicit-`item_class`
/// request shape supersedes the reserved-set refusal, and *only* it does).
///
/// ⚠ The refusal half had **no test anywhere** before this one — the contract
/// asked to "update the conformance test" and there was none to update, so a
/// refactor merging the two paths would have gone unnoticed.
#[tokio::test]
async fn a_reserved_scope_is_reachable_by_item_class_routing_and_by_nothing_else() {
    let (router, state) = router_and_state().await;

    // (a) The explicitly-routed shape reaches the reserved `__state` scope.
    put(
        &router,
        &state,
        ACTOR,
        &put_req(ITEM, WRITER_A, 1, b"sealed"),
    )
    .await
    .expect("the class-2 kind writes the reserved scope");
    assert_eq!(
        list_state(&router, &state, ACTOR, 0, None)
            .await
            .changes
            .len(),
        1
    );

    // (b) The legacy shape still cannot, on any reserved name — the refusal
    // stands exactly as before. Driven through the real kind so a later
    // refactor that routed `changes.record` at the generalized writer would
    // redden here.
    dispatch(
        &router,
        Arc::clone(&state),
        id(ACTOR),
        "fauna.folders.create",
        encode(&FolderCreateRequest {
            name: "__state".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("folders.create rejects a reserved name up front");

    // The device must exist first, or the refusal comes from
    // `device_unregistered` and this test would pass with the reserved-name
    // guard deleted — it has to fail for the reason it names.
    dispatch(
        &router,
        Arc::clone(&state),
        id(ACTOR),
        "fauna.sync.register",
        encode(&SyncRegisterRequest {
            device_id: id_hex(0xD1),
            label: "test device".into(),
            capabilities: "read,write".into(),
            ..Default::default()
        }),
    )
    .await
    .expect("register ok");

    let err = dispatch(
        &router,
        Arc::clone(&state),
        id(ACTOR),
        "fauna.sync.changes.record",
        encode(&SyncChangeRecordRequest {
            folder: "__state".into(),
            device_id: id_hex(0xD1),
            path: "a.txt".into(),
            manifest_hash: Some(id_hex(0x55)),
            size_bytes: 1,
            change_type: "create".into(),
            path_sealed: Some(ByteBuf::from(b"sealed-label".to_vec())),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a legacy-shaped record into a reserved set is refused");
    assert_eq!(err.code, "fauna.sync.invalid_request");
    let detail = format!("{:?}", err.details);
    assert!(
        detail.contains("reserved"),
        "refused BY THE RESERVED GUARD, not incidentally: {detail}"
    );

    // ...and the scope's class-2 rows are untouched by the refused write.
    assert_eq!(
        list_state(&router, &state, ACTOR, 0, None)
            .await
            .changes
            .len(),
        1
    );
}

/// The subtler half of "opt-in by request shape": naming the reserved scope as a
/// **`folder`** — the shipped request shape — must not serve class-2 rows
/// either.
///
/// ⚠ This is a real hole that was open until W2.3 closed it:
/// `resolve_readable_folder` does not refuse reserved names (it resolves
/// `(name, actor_id)` for whatever the owner passes), so the owner of a `__state`
/// scope could reach the folder branch, which had no class filter. The other
/// rails never exposed it because their rows are ordinary file rows; a new item
/// class is what makes the difference load-bearing.
#[tokio::test]
async fn naming_the_reserved_scope_as_a_folder_serves_no_class_2_rows() {
    let (router, state) = router_and_state().await;

    put(
        &router,
        &state,
        ACTOR,
        &put_req(ITEM, WRITER_A, 1, b"sealed"),
    )
    .await
    .unwrap();

    let bytes = dispatch(
        &router,
        Arc::clone(&state),
        id(ACTOR),
        "fauna.sync.changes.list",
        encode(&SyncChangesListRequest {
            folder: Some("__state".into()),
            since: 0,
            ..Default::default()
        }),
    )
    .await;

    // Either refused outright or served empty — never served the entries.
    if let Ok(bytes) = bytes {
        let reply: SyncChangesListReply = decode(&bytes).unwrap();
        assert!(
            reply.changes.is_empty(),
            "the item-class-less folder shape must not serve class-2 rows"
        );
    }

    // Control: the explicitly-routed shape still serves it, so the exclusion is
    // scoped to the request shape and did not simply hide the data.
    assert_eq!(
        list_state(&router, &state, ACTOR, 0, None)
            .await
            .changes
            .len(),
        1
    );
}

/// Asking for a class this feed does not serve is a refusal, not an empty page
/// — "no rows" would read to a caller as "your scope is empty".
#[tokio::test]
async fn an_unserved_item_class_is_refused_rather_than_answered_empty() {
    let (router, state) = router_and_state().await;

    for class in [
        ItemClass::RecordCid.as_wire(),
        ItemClass::ChunkManifest.as_wire(),
        "a-class-from-the-future",
    ] {
        let err = dispatch(
            &router,
            Arc::clone(&state),
            id(ACTOR),
            "fauna.sync.changes.list",
            encode(&SyncChangesListRequest {
                item_class: Some(class.into()),
                ..Default::default()
            }),
        )
        .await
        .expect_err("an unserved item_class is refused");
        assert_eq!(err.code, "fauna.sync.invalid_request", "for {class}");
    }
}

/// The plane's caller class, pinned beside the surface it guards: a bridge actor
/// has no account replica and must not reach it.
#[test]
fn the_write_kind_is_user_class() {
    assert!(is_permitted(CallerClass::User, KIND_STATE_PUT));
    assert!(is_permitted(CallerClass::Admin, KIND_STATE_PUT));
    assert!(!is_permitted(CallerClass::BridgeMta, KIND_STATE_PUT));
    assert!(!is_permitted(CallerClass::BridgeMda, KIND_STATE_PUT));
    assert!(!is_permitted(CallerClass::ContentProcessor, KIND_STATE_PUT));
}

// ── Delegable-scope reclamation, part (2): a put names the rows it covers
// (`delegable-scope-reclamation.md` § Delegable-scope reclamation) ──────────

fn naming(item: u8, writer: u8, writer_seq: i64) -> ReplacedRow {
    ReplacedRow {
        item_key: ByteBuf::from(id(item).to_vec()),
        writer_id: id_hex(writer),
        writer_seq,
        ..Default::default()
    }
}

fn served_entries(listed: &SyncChangesListReply) -> Vec<Option<Vec<u8>>> {
    listed
        .changes
        .iter()
        .map(|c| c.entry.as_ref().map(|e| e.to_vec()))
        .collect()
}

/// `replaced` rides the reply exactly when the request named a row — its
/// presence is how the device learns the nest read the list — and counts
/// the rows that were live at the named coordinates.
#[tokio::test]
async fn the_reply_counts_replaced_rows_only_when_the_put_named_any() {
    let (router, state) = router_and_state().await;

    let plain = put(&router, &state, ACTOR, &put_req(ITEM, WRITER_A, 1, b"a1"))
        .await
        .unwrap();
    assert_eq!(plain.replaced, None, "a put that names no row counts none");

    let covering = put(
        &router,
        &state,
        ACTOR,
        &AccountStatePutRequest {
            replaces: vec![naming(ITEM, WRITER_A, 1)],
            ..put_req(ITEM, WRITER_B, 1, b"b1")
        },
    )
    .await
    .unwrap();
    assert_eq!(covering.replaced, Some(1));
    assert_eq!(
        served_entries(&list_state(&router, &state, ACTOR, 0, None).await),
        vec![Some(b"b1".to_vec())],
        "the replaced row is no longer served"
    );

    // A row no longer live at those coordinates is skipped, never an error,
    // and the reply still says the list was read.
    let skipped = put(
        &router,
        &state,
        ACTOR,
        &AccountStatePutRequest {
            replaces: vec![naming(ITEM, WRITER_A, 1)],
            ..put_req(ITEM, WRITER_B, 2, b"b2")
        },
    )
    .await
    .unwrap();
    assert_eq!(skipped.replaced, Some(0));
}

/// The fleet scope takes no `replaces` (its retires carry orderings the
/// retention gate enforces); a list over the bound, and a malformed named
/// row, are refused like a malformed put — and none writes or supersedes
/// anything.
#[tokio::test]
async fn a_replaces_list_is_refused_on_the_fleet_scope_over_its_bound_and_malformed() {
    let (router, state) = router_and_state().await;
    put(&router, &state, ACTOR, &put_req(ITEM, WRITER_A, 1, b"a1"))
        .await
        .unwrap();

    let cases: Vec<(&str, AccountStatePutRequest)> = vec![
        (
            "a list on the fleet scope",
            AccountStatePutRequest {
                scope: ACCOUNT_STATE_FLEET_SCOPE.into(),
                replaces: vec![naming(ITEM, WRITER_A, 1)],
                ..put_req(ITEM, WRITER_B, 1, b"b1")
            },
        ),
        (
            "a list over the bound",
            AccountStatePutRequest {
                replaces: (0..=MAX_REPLACED_ROWS_PER_PUT as i64)
                    .map(|s| naming(ITEM, WRITER_A, s))
                    .collect(),
                ..put_req(ITEM, WRITER_B, 1, b"b1")
            },
        ),
        (
            "a named row with a non-hex writer id",
            AccountStatePutRequest {
                replaces: vec![ReplacedRow {
                    writer_id: "zz".into(),
                    ..naming(ITEM, WRITER_A, 1)
                }],
                ..put_req(ITEM, WRITER_B, 1, b"b1")
            },
        ),
        (
            "a named row with a short item key",
            AccountStatePutRequest {
                replaces: vec![ReplacedRow {
                    item_key: ByteBuf::from(vec![0u8; 8]),
                    ..naming(ITEM, WRITER_A, 1)
                }],
                ..put_req(ITEM, WRITER_B, 1, b"b1")
            },
        ),
        (
            "a named row at a negative seq",
            AccountStatePutRequest {
                replaces: vec![naming(ITEM, WRITER_A, -1)],
                ..put_req(ITEM, WRITER_B, 1, b"b1")
            },
        ),
    ];
    for (what, req) in cases {
        let err = put(&router, &state, ACTOR, &req)
            .await
            .expect_err(&format!("{what} must be refused"));
        assert_eq!(
            err.code, "fauna.account.state.invalid_request",
            "the refusal for {what}"
        );
    }

    assert_eq!(
        served_entries(&list_state(&router, &state, ACTOR, 0, None).await),
        vec![Some(b"a1".to_vec())],
        "no refused request wrote or superseded a row"
    );
}
