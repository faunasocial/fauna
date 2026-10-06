//! Integration round-trip for the `__index` rail — `fauna.index.{record,list}`
//! (`docs/goal/behavior/content-index.md` § Ingest triggers, v1).
//!
//! This is the plane a capability-position builder publishes sealed content-index
//! segments over: bytes to the blob store (HTTP, not this plane — WS-RPC frames
//! cap at 2 MiB), then the reference here, which appends the `sync_changes` row
//! that replicates the blob to the user's other locations.
//!
//! The rail exists *because* the generic `fauna.sync.changes.record` is closed to
//! reserved sets — see `conformance_folders.rs`
//! `changes_record_refuses_a_reserved_rail_so_no_client_writer_can_use_it` for
//! the refusal and the two reasons behind it.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` + a real
//! `DiskBlobStore` — no mocks).

mod common;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    backup::service::BackupService,
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::{CacheDb, bridge_service_users::BridgeRole},
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    RpcError,
    content_index::{
        BridgeListIndexBlobsRequest, KIND_BRIDGE_LIST, KIND_LIST, KIND_RECORD, ListIndexBlobsReply,
        ListIndexBlobsRequest, RecordIndexBlobReply, RecordIndexBlobRequest,
    },
    decode_strict as decode,
};

struct Fixture {
    router: RpcRouter,
    state: Arc<AppState>,
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, dir.path().to_path_buf(), None).unwrap(),
    );
    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        ..AppState::for_test(db)
    });
    let mut b = RpcRouter::builder();
    fauna_nest::content_index_handlers::register_content_index_handlers(&mut b);
    Fixture {
        router: b.build(),
        state,
        _dir: dir,
    }
}

async fn dispatch(
    f: &Fixture,
    actor: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    common::seed_dispatch_actor(&f.state.db, &actor).await;
    let meta = f.router.kind_meta(kind).expect("kind registered");
    (meta.handler)(Arc::clone(&f.state), actor, payload).await
}

/// Put sealed bytes in the blob store the way the HTTP blob route would, and
/// return the hex hash a builder would then record.
async fn upload(f: &Fixture, bytes: &[u8]) -> String {
    let digest: [u8; 32] = *blake3::hash(bytes).as_bytes();
    f.state
        .backup_service
        .as_ref()
        .unwrap()
        .local_blob_store()
        .put(
            &fauna_core::data::ContentHash::from_digest_raw(digest),
            bytes,
        )
        .await
        .unwrap();
    hex::encode(digest)
}

async fn record(
    f: &Fixture,
    actor: [u8; 32],
    path: &str,
    blob_hash: &str,
    size_bytes: i64,
) -> Result<RecordIndexBlobReply, RpcError> {
    let payload = encode(&RecordIndexBlobRequest {
        path: path.into(),
        blob_hash: blob_hash.into(),
        size_bytes,
        ..Default::default()
    });
    dispatch(f, actor, KIND_RECORD, payload)
        .await
        .map(|b| decode(&b).unwrap())
}

async fn list(f: &Fixture, actor: [u8; 32]) -> ListIndexBlobsReply {
    let payload = encode(&ListIndexBlobsRequest::default());
    decode(
        &dispatch(f, actor, KIND_LIST, payload)
            .await
            .expect("list ok"),
    )
    .unwrap()
}

/// The whole builder flush, end to end: a sealed segment and the manifest that
/// references it are published in that order, and a second device enumerating
/// the rail sees both — which is the replication `content-index.md` § Where the
/// index is built requires of `__index`.
#[tokio::test]
async fn publish_a_segment_then_its_manifest_and_list_them_back() {
    let f = fixture().await;
    let actor = [0x11_u8; 32];

    let seg_bytes = b"sealed-mail-segment-1".repeat(8);
    let man_bytes = b"sealed-mailcal-manifest".repeat(4);
    let seg_path = fauna_index::segment_path(fauna_index::ContentKind::Mail, 1);
    let man_path = fauna_index::mailcal_manifest_path();

    let seg_hash = upload(&f, &seg_bytes).await;
    let seg_seq = record(&f, actor, &seg_path, &seg_hash, seg_bytes.len() as i64)
        .await
        .expect("record the segment")
        .seq;

    let man_hash = upload(&f, &man_bytes).await;
    let man_seq = record(&f, actor, &man_path, &man_hash, man_bytes.len() as i64)
        .await
        .expect("record the manifest")
        .seq;

    // Publish order is load-bearing (the manifest must never precede the
    // segment it references), and the journal preserves it.
    assert!(
        man_seq > seg_seq,
        "the manifest's journal row must follow the segment's"
    );

    let entries = list(&f, actor).await.entries;
    assert_eq!(entries.len(), 2, "both published paths are live");

    let seg = entries
        .iter()
        .find(|e| e.path == seg_path)
        .expect("segment");
    assert_eq!(seg.blob_hash, seg_hash);
    assert_eq!(seg.size_bytes, seg_bytes.len() as i64);

    let man = entries
        .iter()
        .find(|e| e.path == man_path)
        .expect("manifest");
    assert_eq!(man.blob_hash, man_hash);

    // The bytes are fetchable by the hash the list handed back — the read half
    // the replica refresh performs over the blob route.
    let fetched = f
        .state
        .backup_service
        .as_ref()
        .unwrap()
        .local_blob_store()
        .get(&fauna_core::data::ContentHash::from_digest_raw(
            fauna_core::hex32::decode(&seg.blob_hash).unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(fetched.as_deref(), Some(&seg_bytes[..]));
}

/// Recording a reference the nest cannot resolve is refused with a typed,
/// retryable code — never accepted. A journal row pointing at a missing blob is
/// exactly the corruption the builder's publish order exists to prevent, and a
/// client that recorded first would produce it on every crash.
#[tokio::test]
async fn record_refuses_a_blob_the_nest_does_not_hold() {
    let f = fixture().await;
    let actor = [0x12_u8; 32];
    let path = fauna_index::segment_path(fauna_index::ContentKind::Mail, 1);

    let err = record(&f, actor, &path, &"ab".repeat(32), 4096)
        .await
        .expect_err("recording un-uploaded bytes must fail");
    assert_eq!(err.code, "fauna.index.bytes_not_held");

    assert!(
        list(&f, actor).await.entries.is_empty(),
        "the refused record must leave no live row"
    );
}

/// The nest re-derives the `__index` path shape rather than trusting the
/// writer's string, so a malformed or hostile client cannot stuff unaddressable
/// rows into a rail no user-facing surface lists.
#[tokio::test]
async fn record_refuses_a_path_that_is_not_an_index_virtual_path() {
    let f = fixture().await;
    let actor = [0x13_u8; 32];
    let hash = upload(&f, b"sealed").await;

    for bad in [
        "notes/secret.txt",                  // not the rail at all
        "__index/mail/seg-1.idx",            // sequence not 8 digits
        "__index/nonsense/seg-00000001.idx", // unknown kind
        "__index/manifest.txt",              // not a manifest name
        "__config/user.faunaconfig",         // a reserved name outside this rail
    ] {
        let err = record(&f, actor, bad, &hash, 6)
            .await
            .unwrap_err_or_else_msg(bad);
        // The shared `malformed` helper's code, same as the `__drafts` twin's
        // path validation — a decode/shape rejection, not a rail-specific one.
        assert_eq!(err.code, "fauna.protocol.malformed", "path {bad:?}");
    }

    assert!(
        list(&f, actor).await.entries.is_empty(),
        "no malformed path may become a live row"
    );
}

/// The manifest is rewritten wholesale on every flush, so the same path is
/// recorded repeatedly. The rail must converge on the LATEST blob and keep one
/// entry — not accumulate a row per flush that a reader would have to reconcile.
#[tokio::test]
async fn re_recording_a_path_converges_on_the_latest_blob() {
    let f = fixture().await;
    let actor = [0x14_u8; 32];
    let path = fauna_index::mailcal_manifest_path();

    let first = upload(&f, b"manifest-generation-1").await;
    record(&f, actor, &path, &first, 21).await.expect("first");
    let second = upload(&f, b"manifest-generation-2").await;
    record(&f, actor, &path, &second, 21).await.expect("second");

    let entries = list(&f, actor).await.entries;
    assert_eq!(
        entries.len(),
        1,
        "one live entry per path, not one per flush"
    );
    assert_eq!(
        entries[0].blob_hash, second,
        "the live entry is the latest generation"
    );
}

/// A fresh actor's rail is empty rather than an error — the first-run state the
/// builder starts from with a fresh manifest, and the reason no provisioning
/// step precedes a first publish.
#[tokio::test]
async fn list_is_empty_before_anything_is_published() {
    let f = fixture().await;
    assert!(list(&f, [0x15_u8; 32]).await.entries.is_empty());
}

/// Both kinds are self-scoped: the owning actor comes from the authenticated
/// connection, so one user's index is invisible to another on the same nest.
#[tokio::test]
async fn one_actors_index_is_invisible_to_another() {
    let f = fixture().await;
    let alice = [0x16_u8; 32];
    let bob = [0x17_u8; 32];
    let path = fauna_index::segment_path(fauna_index::ContentKind::Mail, 1);

    let hash = upload(&f, b"alices-sealed-segment").await;
    record(&f, alice, &path, &hash, 21).await.expect("alice");

    assert_eq!(list(&f, alice).await.entries.len(), 1);
    assert!(
        list(&f, bob).await.entries.is_empty(),
        "bob must not see alice's index"
    );
}

/// The **user plane** stays self-scoped and closed to every bridge class. This is
/// what makes the MDA's on-behalf-of write unrepresentable on this pair of kinds:
/// they carry no `actor_id` at all, so admitting a bridge here could only ever
/// write the *bridge's own* rail. The MDA's reach is the bridge-plane twins below.
#[tokio::test]
async fn allowlist_user_admin_permitted_bridges_denied() {
    for kind in [KIND_RECORD, KIND_LIST] {
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(is_permitted(class, kind), "{kind} permitted for {class:?}");
        }
        for class in [
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
            CallerClass::ContentProcessor,
            CallerClass::BridgeAtprotoPds,
        ] {
            assert!(!is_permitted(class, kind), "{kind} denied for {class:?}");
        }
    }
}

// ── The bridge plane — the MDA's index-builder reach (rollout slice S5) ──
//
// `content-index.md` § Where the index is built ratifies the MDA bridge, during
// an active MUA-AUTH session, as an index *builder* — holding only the
// MSEK-derived mail/calendar index-segment key, so it builds and queries only
// the mail and calendar slices (§ Architectural rules #3; rule #7's
// blast-radius invariant in `key-material-hierarchy.md` § Path B-sibling-4).
//
// Two properties these tests pin, and neither is optional:
//
//  * The MDA publishes to the **target user's** rail, named explicitly — the
//    `fetch_mls_snapshot_blob` shape every MDA-on-behalf kind uses.
//  * Its reach is **mail/calendar only**, enforced on the PATH. The seal carries
//    no class discriminator in its bytes (both classes use the same `FXSG`
//    magic), so the nest cannot infer the class from the blob.

/// Every master-class path, as a builder would spell it. A `BridgeMda` must be
/// refused on all of them — that refusal IS the rule-#7 blast-radius bound at
/// the rail layer.
fn master_class_paths() -> Vec<String> {
    let mut v = vec![fauna_index::manifest_path()];
    for kind in [
        fauna_index::ContentKind::Conversation,
        fauna_index::ContentKind::Post,
        fauna_index::ContentKind::File,
        fauna_index::ContentKind::Contact,
        fauna_index::ContentKind::Draft,
        fauna_index::ContentKind::Media,
    ] {
        v.push(fauna_index::segment_path(kind, 1));
    }
    v
}

fn mailcal_paths() -> Vec<String> {
    vec![
        fauna_index::mailcal_manifest_path(),
        fauna_index::segment_path(fauna_index::ContentKind::Mail, 1),
        fauna_index::segment_path(fauna_index::ContentKind::Calendar, 2),
    ]
}

async fn approve_mda(f: &Fixture, pk: &[u8; 32]) {
    f.state
        .db
        .create_pending_bridge_service_user(pk, BridgeRole::Mda, "test-mda")
        .await
        .expect("create pending bridge");
    f.state
        .db
        .upsert_bridge_x25519(pk, &[0x99u8; 32])
        .await
        .expect("upsert x25519");
    f.state
        .db
        .approve_bridge_service_user(pk, None)
        .await
        .expect("approve bridge");
}

async fn bridge_list(
    f: &Fixture,
    bridge: [u8; 32],
    target: [u8; 32],
) -> Result<ListIndexBlobsReply, RpcError> {
    let payload = encode(&BridgeListIndexBlobsRequest {
        actor_id: target.to_vec().into(),
        ..Default::default()
    });
    dispatch(f, bridge, KIND_BRIDGE_LIST, payload)
        .await
        .map(|b| decode(&b).unwrap())
}

/// The write kind is GONE, not merely denied: `fauna.bridges.index_record`
/// was retired with the MDA's build half (`content-index.md` § Where the index
/// is built — the 2026-08-10 carrier ruling). No handler is registered for it,
/// so a caller of any class hits the dispatcher's unknown-kind refusal before
/// any decode — which is why this pin asserts on the router's own metadata
/// table, the same authority the dispatcher consults. Re-registering the kind
/// is what turns this red; the read kind beside it proves the fixture's router
/// is the real, fully-registered one and not an empty stub.
#[tokio::test]
async fn the_bridge_record_kind_is_retired() {
    let f = fixture().await;
    assert!(
        f.router.kind_meta("fauna.bridges.index_record").is_none(),
        "the retired write kind must have no registered handler"
    );
    // Beside-control: the same router serves the surviving read kind, so the
    // assertion above cannot be satisfied by an unregistered fixture.
    assert!(
        f.router.kind_meta(KIND_BRIDGE_LIST).is_some(),
        "the read kind must still be registered — the retirement is write-only"
    );
}

/// Least privilege on the read side. The MDA has no key for master-class
/// segments and no reason to enumerate them; leaving them out also keeps the
/// user's per-kind segment counts away from a MUA-credential-reachable position.
#[tokio::test]
async fn an_mdas_list_shows_only_the_mailcal_class() {
    let f = fixture().await;
    let bridge = [0x27_u8; 32];
    let user = [0x28_u8; 32];
    approve_mda(&f, &bridge).await;
    let hash = upload(&f, b"sealed").await;

    // The user's own client publishes BOTH classes, as it does in production —
    // it holds both keys.
    for path in mailcal_paths().into_iter().chain(master_class_paths()) {
        record(&f, user, &path, &hash, 6)
            .await
            .expect("user writes");
    }
    assert_eq!(
        list(&f, user).await.entries.len(),
        10,
        "the user sees their whole rail"
    );

    let seen: Vec<String> = bridge_list(&f, bridge, user)
        .await
        .expect("bridge list")
        .entries
        .into_iter()
        .map(|e| e.path)
        .collect();
    let mut expected = mailcal_paths();
    expected.sort();
    let mut seen_sorted = seen.clone();
    seen_sorted.sort();
    assert_eq!(
        seen_sorted, expected,
        "the MDA enumerates exactly the mail/calendar class"
    );
}

/// The bridge plane is bridge-only in both directions: a `User` cannot reach it
/// (it would be an actor-spoofing door), and no other bridge role can either —
/// the MDA is the one ratified builder position.
#[tokio::test]
async fn the_bridge_plane_is_reachable_only_by_an_mda() {
    let kind = KIND_BRIDGE_LIST;
    assert!(
        is_permitted(CallerClass::BridgeMda, kind),
        "{kind} permitted for BridgeMda"
    );
    for class in [
        CallerClass::User,
        CallerClass::Admin,
        CallerClass::BridgeMta,
        CallerClass::ContentProcessor,
        CallerClass::BridgeAtprotoPds,
    ] {
        assert!(!is_permitted(class, kind), "{kind} denied for {class:?}");
    }
    // The retired write kind must keep NO allowlist row for any class — a row
    // for an unregistered kind is an authorization held open for a kind that
    // may return under the same name (the stale-allowlist-entry class the
    // 113th pass's ratchet pins on the pre-identity list).
    for class in [
        CallerClass::BridgeMda,
        CallerClass::User,
        CallerClass::Admin,
        CallerClass::BridgeMta,
        CallerClass::ContentProcessor,
        CallerClass::BridgeAtprotoPds,
    ] {
        assert!(
            !is_permitted(class, "fauna.bridges.index_record"),
            "the retired write kind must have no allowlist row ({class:?})"
        );
    }
}

/// Small ergonomic shim so the malformed-path loop reports WHICH path passed
/// when it should not have.
trait UnwrapErrOrElseMsg {
    fn unwrap_err_or_else_msg(self, what: &str) -> RpcError;
}
impl UnwrapErrOrElseMsg for Result<RecordIndexBlobReply, RpcError> {
    fn unwrap_err_or_else_msg(self, what: &str) -> RpcError {
        match self {
            Ok(_) => panic!("path {what:?} was accepted but must be refused"),
            Err(e) => e,
        }
    }
}

// ── The rail's resting bound (2026-09-20) ──────────
//
// `record_index_blob_change` appends through the UNMETERED
// `record_sync_change`, which a reserved rail may do only because it is
// "bounded by construction" — the ruling at `db/drafts.rs:90-126`. That ruling names TWO axes, and the second
// is the one every rail keeps re-opening: the count of *retained versions* of
// each path. Nothing in production marks a reserved rail's rows superseded, so
// without a collapse every historical blob stays GC-pinned permanently
// (`sync_storage.rs::sync_change_manifest_hashes` pins every live row). The
// `__index` rail repeated it on a third rail: the two manifests are rewritten
// on every flush, so an honest builder pinned one manifest blob per flush
// forever.
//
// The collapse is safe here for the same reason it is for `__drafts`:
// `list_index_blobs` is a per-path head reader (`MAX(seq)` +
// `superseded_at IS NULL`), so superseding the predecessors changes no answer,
// and in-flight readers keep the `superseded_before_millis` buffer.
//
// NOT in scope, and deliberately: reclaiming a *tombstoned segment's* blob.
// `content-index.md` § Compaction rules the compactor tombstone-only and defers
// physical reclamation to distributed GC; the nest is also forbidden from
// reading or sweeping `__index` at all (§ Don't do these), so it cannot know
// which segment a fold retired. The segment-path axis is a named open bound in
// that doc, not a thing this rail closes on its own.

/// A zero-grace blob GC sweep — the admin `fauna.admin.gc` door's exact call.
async fn zero_grace_gc(state: &Arc<AppState>) -> fauna_nest::backup::gc::GcResult {
    let backup_svc = state.backup_service.as_ref().expect("backup service");
    let blob_store = backup_svc.local_blob_store();
    fauna_nest::backup::gc::garbage_collect(
        &state.db,
        &blob_store,
        fauna_nest::backup::gc::PostBodySource {
            segments: &state.post_segments,
        },
        0,
        backup_svc.encryption_key(),
        false,
    )
    .await
    .expect("gc sweep")
}

async fn blob_is_stored(f: &Fixture, hex_hash: &str) -> bool {
    let digest: [u8; 32] = fauna_core::hex32::decode(hex_hash).unwrap();
    f.state
        .backup_service
        .as_ref()
        .unwrap()
        .local_blob_store()
        .exists(&fauna_core::data::ContentHash::from_digest_raw(digest))
        .await
        .unwrap()
}

/// **A superseded manifest generation stops pinning its blob.** The original
/// probe ran exactly this and the superseded version *survived*: an honest builder — one flush per debounce, for the life
/// of the account — pinned unbounded bytes through an unmetered recorder, which
/// is precisely the retained-version axis closed for `__drafts` and
/// re-opened here.
///
/// `re_recording_a_path_converges_on_the_latest_blob` above already pins the
/// *listing* side. This is the byte side, which the listing cannot see.
#[tokio::test]
async fn a_superseded_manifest_generation_stops_pinning_its_blob() {
    let f = fixture().await;
    let actor = [0x40_u8; 32];
    let path = fauna_index::mailcal_manifest_path();

    let first = upload(&f, b"manifest-generation-1").await;
    record(&f, actor, &path, &first, 21).await.expect("first");
    let second = upload(&f, b"manifest-generation-2").await;
    record(&f, actor, &path, &second, 21).await.expect("second");

    assert!(blob_is_stored(&f, &first).await, "both blobs are on disk");
    assert!(blob_is_stored(&f, &second).await);

    zero_grace_gc(&f.state).await;

    assert!(
        blob_is_stored(&f, &second).await,
        "the LIVE manifest generation must survive — collapsing history must \
         never touch the head row"
    );
    assert!(
        !blob_is_stored(&f, &first).await,
        "a superseded manifest generation is still GC-pinned — the rail appends \
         through the unmetered recorder without collapsing its history, so every \
         manifest version an honest builder ever wrote stays pinned \
         forever. \
         `record_index_blob_change` must call `collapse_reserved_rail_history`, \
         as its `__drafts` twin does."
    );
}

/// The collapse is per-path, so a *segment* path — written once and never
/// rewritten — keeps its blob, and one path's collapse never reaches another's.
/// The bound this closes is the version count, never the live set.
#[tokio::test]
async fn collapsing_one_paths_history_leaves_every_other_path_live() {
    let f = fixture().await;
    let actor = [0x41_u8; 32];
    let seg_path = fauna_index::segment_path(fauna_index::ContentKind::Mail, 1);
    let man_path = fauna_index::mailcal_manifest_path();

    let seg = upload(&f, b"sealed-mail-segment-1").await;
    record(&f, actor, &seg_path, &seg, 21).await.expect("seg");
    let man1 = upload(&f, b"manifest-generation-1").await;
    record(&f, actor, &man_path, &man1, 21).await.expect("m1");
    let man2 = upload(&f, b"manifest-generation-2").await;
    record(&f, actor, &man_path, &man2, 21).await.expect("m2");

    zero_grace_gc(&f.state).await;

    assert!(
        blob_is_stored(&f, &seg).await,
        "the segment is live in its own right — a manifest's collapse must not \
         reach it"
    );
    assert!(
        blob_is_stored(&f, &man2).await,
        "the live manifest survives"
    );
    assert!(!blob_is_stored(&f, &man1).await, "its predecessor does not");

    let entries = list(&f, actor).await.entries;
    assert_eq!(entries.len(), 2, "both live paths still list");
}

// ── `size_bytes` is verified, not believed ──
//
// The handler checked `size_bytes > 0` and nothing more, then handed the claim
// straight to the rail. Since the blob PUT now records metadata (and
// `put_blob_metadata` is `INSERT OR IGNORE`), a lie can no longer reach
// `blob_metadata` — but it still lands in `sync_changes.size_bytes`, which is
// what `fauna.index.list` reports and what the shared fold planner budgets on.
// The nest holds the bytes already, so the true length is free to check.

/// A declared size that disagrees with the held blob is refused. The nest
/// cannot read a sealed segment, but it can always measure one.
#[tokio::test]
async fn a_size_that_disagrees_with_the_held_blob_is_refused() {
    let f = fixture().await;
    let actor = [0x42_u8; 32];
    let path = fauna_index::segment_path(fauna_index::ContentKind::Mail, 1);
    let hash = upload(&f, b"sealed-mail-segment-1").await;

    let err = record(&f, actor, &path, &hash, 21 * 4096).await.expect_err(
        "a declared size that is not the held blob's length must be refused — \
             it is what `fauna.index.list` reports and what the fold planner \
             budgets on",
    );
    // The same refusal code the rail's other shape checks use — a bad size is
    // a malformed request, not a new error class.
    assert_eq!(err.code, "fauna.protocol.malformed", "{err:?}");

    assert!(
        list(&f, actor).await.entries.is_empty(),
        "a refused record writes no row"
    );
}

/// The honest builder's own call — the sealed byte length it uploaded — still
/// passes. The check is an equality against what the nest holds, not a ceiling.
#[tokio::test]
async fn the_true_sealed_length_is_accepted() {
    let f = fixture().await;
    let actor = [0x43_u8; 32];
    let path = fauna_index::segment_path(fauna_index::ContentKind::Mail, 1);
    let bytes = b"sealed-mail-segment-of-some-particular-length";
    let hash = upload(&f, bytes).await;

    record(&f, actor, &path, &hash, bytes.len() as i64)
        .await
        .expect("the true length is what a builder declares");

    let entries = list(&f, actor).await.entries;
    assert_eq!(entries[0].size_bytes, bytes.len() as i64);
}

// ── Paging — the `fauna.index.list` half ────────────
//
// This rail's segment-path count is deliberately unbounded (see the rail-bound
// bullet in `content-index.md` § Where the index is built), so the one read
// that enumerates it must not depend on the whole set fitting one frame. Past
// the 2 MiB cap the unpaged reply was *undeliverable*: a large enough index
// could never be refreshed again, by its own owner, with no way back — a client
// -reachable dead end, which is the "no client-causable unrecoverable state"
// shape rather than mere inefficiency.
//
// The page is cut at the ratified frame BYTE budget, not at a row count, which
// is what makes this additive in both directions: a cursor-less first page (what the rail
// publisher and the Go bridge send) gets a reply at least as complete as any the frame could ever have
// carried. These tests drive an explicit small `limit` so the walk is
// observable without publishing eight thousand segments.

/// One raw page, the way a paging client asks for it.
async fn list_page(
    f: &Fixture,
    actor: [u8; 32],
    cursor: Option<&str>,
    limit: i64,
) -> ListIndexBlobsReply {
    let payload = encode(&ListIndexBlobsRequest {
        cursor: cursor.map(str::to_string),
        limit,
        ..Default::default()
    });
    decode(
        &dispatch(f, actor, KIND_LIST, payload)
            .await
            .expect("list ok"),
    )
    .unwrap()
}

/// Walk to the cursor's ABSENCE — never to the first empty page — and report
/// how many pages it took, which is what proves the walk actually paged.
async fn walk_user_plane(f: &Fixture, actor: [u8; 32], limit: i64) -> (Vec<String>, usize) {
    let mut cursor: Option<String> = None;
    let mut paths = Vec::new();
    let mut pages = 0usize;
    loop {
        let page = list_page(f, actor, cursor.as_deref(), limit).await;
        pages += 1;
        assert!(pages < 100, "walk did not terminate");
        paths.extend(page.entries.into_iter().map(|e| e.path));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return (paths, pages),
        }
    }
}

/// **A paged walk returns the complete rail, in order, with nothing repeated
/// and nothing skipped.** The property the whole design rests on: the cursor is
/// the path itself, which is already a total order (`path_hash` is
/// `hash(path)`, so one path is one group), so no two pages can overlap and no
/// row can fall between them.
#[tokio::test]
async fn a_paged_walk_returns_the_whole_rail_exactly_once() {
    let f = fixture().await;
    let actor = [0x50_u8; 32];
    let hash = upload(&f, b"sealed").await;
    let mut expected: Vec<String> = mailcal_paths()
        .into_iter()
        .chain(master_class_paths())
        .collect();
    for path in &expected {
        record(&f, actor, path, &hash, 6).await.expect("record");
    }
    expected.sort();

    let (walked, pages) = walk_user_plane(&f, actor, 4).await;
    assert!(
        pages > 1,
        "a limit of 4 over {} paths must take more than one page, or this pins \
         nothing",
        expected.len()
    );
    assert_eq!(
        walked, expected,
        "the walk must yield every live path exactly once, in path order"
    );
}

/// **A limit-less first page — no cursor, no limit — gets everything that fits
/// the byte budget, in one reply with no cursor.** Not an older-client arm: the
/// shared rail publisher and the Go bridge both send no limit today and rely on
/// the byte-budgeted page, so `limit` stays optional (`version-compatibility.md`
/// § Dimension 2, the fourth ratified exception — recorded as *stays*).
#[tokio::test]
async fn an_unpaged_request_still_returns_the_whole_rail_in_one_reply() {
    let f = fixture().await;
    let actor = [0x51_u8; 32];
    let hash = upload(&f, b"sealed").await;
    let expected: Vec<String> = mailcal_paths()
        .into_iter()
        .chain(master_class_paths())
        .collect();
    for path in &expected {
        record(&f, actor, path, &hash, 6).await.expect("record");
    }

    // `ListIndexBlobsRequest::default()` — no cursor, no row bound.
    let reply = list(&f, actor).await;
    assert_eq!(reply.entries.len(), expected.len(), "complete in one reply");
    assert!(
        reply.next_cursor.is_none(),
        "a complete reply mints no cursor — that absence is what ends a walk"
    );
}

/// **The bridge plane's walk survives a page that serves NOTHING.** Its
/// master-class filter runs after the nest reads a page, so with a small limit
/// some windows are entirely master-class: the page comes back empty *while
/// mail/calendar entries still remain behind it*. A walk that stopped on the
/// first empty page would silently truncate the MDA's view of the user's mail
/// index — and a cursor minted from the last SERVED row instead of the last
/// READ row would re-read that window forever.
#[tokio::test]
async fn the_bridge_walk_passes_through_an_empty_page() {
    let f = fixture().await;
    let bridge = [0x52_u8; 32];
    let user = [0x53_u8; 32];
    approve_mda(&f, &bridge).await;
    let hash = upload(&f, b"sealed").await;
    for path in mailcal_paths().into_iter().chain(master_class_paths()) {
        record(&f, user, &path, &hash, 6).await.expect("record");
    }

    let mut cursor: Option<String> = None;
    let mut seen: Vec<String> = Vec::new();
    let mut saw_empty_page_with_more = false;
    let mut pages = 0usize;
    loop {
        let payload = encode(&BridgeListIndexBlobsRequest {
            actor_id: user.to_vec().into(),
            cursor: cursor.clone(),
            limit: 2,
            ..Default::default()
        });
        let reply: ListIndexBlobsReply = decode(
            &dispatch(&f, bridge, KIND_BRIDGE_LIST, payload)
                .await
                .expect("bridge list ok"),
        )
        .unwrap();
        pages += 1;
        assert!(pages < 100, "walk did not terminate");
        if reply.entries.is_empty() && reply.next_cursor.is_some() {
            saw_empty_page_with_more = true;
        }
        seen.extend(reply.entries.into_iter().map(|e| e.path));
        match reply.next_cursor {
            Some(next) => {
                assert_ne!(
                    Some(&next),
                    cursor.as_ref(),
                    "a cursor that does not advance would spin the walk forever"
                );
                cursor = Some(next);
            }
            None => break,
        }
    }

    assert!(
        saw_empty_page_with_more,
        "this fixture is meant to produce a page whose whole window is \
         master-class — without one the test does not exercise what it claims"
    );
    let mut expected = mailcal_paths();
    expected.sort();
    seen.sort();
    assert_eq!(
        seen, expected,
        "the paged bridge walk must yield exactly the mail/calendar class — no \
         master-class leak, and nothing dropped behind an empty page"
    );
}

/// A cursor the nest never minted is refused loudly rather than silently
/// restarting the walk from the top — a silent restart is an infinite walk for
/// a client that trusts the cursor.
#[tokio::test]
async fn a_cursor_past_the_end_drains_rather_than_restarting() {
    let f = fixture().await;
    let actor = [0x54_u8; 32];
    let hash = upload(&f, b"sealed").await;
    for path in mailcal_paths() {
        record(&f, actor, &path, &hash, 6).await.expect("record");
    }

    // Lexically past every `__index/...` path.
    let reply = list_page(&f, actor, Some("__index/zzzzzzzz"), 0).await;
    assert!(reply.entries.is_empty(), "nothing lives past the last path");
    assert!(
        reply.next_cursor.is_none(),
        "a drained read mints no cursor — it must not loop back to the start"
    );
}
