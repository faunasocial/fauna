//! Row 66 — the group plane's store proven end to end: a ceremony's plane
//! rows and held-root row land in a store and **survive a restart**, the
//! joiner's reception keypair rests durably and its admission bundle still
//! opens after one, and the scope's roster + generation tip resolve from
//! persisted state alone (`docs/goal/architecture/account-data-plane.md`
//! § Implementation status today, the group-scope paragraph).
//!
//! Two real `fauna-account-store` replicas of **two different accounts**
//! (cross-user — the group plane's whole point), no nest and no transport
//! anywhere: the ceremony runs in memory exactly as the capstone test drives
//! it, and every durable claim is asserted against a store **reopened from
//! disk**. The walk test then proves the serve shape: one member's sealed
//! relay rows, served as a feed, land on another member's replica through the
//! machinery-root trial-open — and stay dark to a holder without the root
//! (the custodian posture).
//!
//! The ceremony record's durability is deliberately out of scope: it
//! rides the `fauna.state.group-share-ceremony` door, proven by that kind's own
//! conformance test (`peer_leg_convergence.rs`). The stores are
//! what row 66 built; the cfgs stay in memory here.

mod group_seats;
use group_seats::*;

use fauna_core::encoding::{EmbedAsBytes, canonical_decode, canonical_encode};
use fauna_core::group_ceremony::{GroupShareDeliver, verify_group_share_deliver};
use fauna_core::group_generation::{
    GroupHeldRootRecord, GroupReceptionKeyRecord, group_generation_key_commitment,
    resolve_admissible_group_tip,
};
use fauna_core::group_scope::{GroupAuthority, GroupBirthRecord, RosterView};
use fauna_mls::wrapped_blob::group_generation_wraps::open_group_admission_bundle;
use fauna_protocol::account_state::{ACCOUNT_STATE_FLEET_SCOPE, ItemClass};
use fauna_protocol::group_state::{
    GROUP_BIRTH_KEY, GROUP_NAME_KEY, KIND_GROUP_BIRTH, KIND_GROUP_GENERATION_MINT, KIND_GROUP_NAME,
    KIND_GROUP_ROSTER,
};
use fauna_protocol::merge_policy::LwwStamp;
use fauna_protocol::merge_policy::{
    KIND_DEVICE_SET, KIND_GROUP_MACHINERY_ROOT, KIND_GROUP_RECEPTION_KEY,
};
use fauna_protocol::scope::GroupScope;
use fauna_protocol::sync::{SyncChange, SyncChangesListReply, SyncChangesListRequest};
use fauna_protocol::{decode_strict, encode_canonical};
use fauna_sync_engine::account_state_plane::{AccountStatePlane, ItemId};
use fauna_sync_engine::group_set::{fold_group_files, fold_group_name, group_set_keys};
use fauna_sync_engine::group_state_plane::{GroupStatePlane, NoFeed};

/// Resolve the scope's roster + tip from a store's PERSISTED group rows alone
/// (plus the retained keys the reloaded admission bundle yields) — the
/// "scope is still there" assertion, shared by both seats.
fn resolve_from_store_rows(
    rows: &[fauna_account_store::types::StateEntry],
    scope_id: &[u8; 32],
    authority: &fauna_core::identity::ActorId,
    retained: &[([u8; 32], fauna_core::crypto::GenerationKey)],
) -> [u8; 32] {
    let roster: Vec<(&str, &[u8])> = rows
        .iter()
        .filter(|r| r.kind == KIND_GROUP_ROSTER)
        .map(|r| (r.key.as_str(), r.value.as_slice()))
        .collect();
    let authority = &GroupAuthority::build(scope_id, authority, &[], std::iter::empty());
    let view = RosterView::build(scope_id, authority, roster.iter().copied());
    let mints: Vec<(&str, &[u8])> = rows
        .iter()
        .filter(|r| r.kind == KIND_GROUP_GENERATION_MINT)
        .map(|r| (r.key.as_str(), r.value.as_slice()))
        .collect();
    let resolution = resolve_admissible_group_tip(
        &view,
        authority,
        mints.iter().copied(),
        std::iter::empty(),
        |id, core, _wraps| {
            retained.iter().any(|(rid, key)| {
                rid == id && group_generation_key_commitment(key) == core.key_commitment
            })
        },
    );
    resolution
        .tip
        .unwrap_or_else(|| {
            panic!(
                "persisted rows resolve no keyable tip (invalid: {:?})",
                resolution.invalid
            )
        })
        .generation_id
}

/// The account runtime's door-put verdict rests on this answer: a
/// tip-sealed origination runs the first-need mint (network) exactly while
/// no admissible tip resolves, and a `Gen0` kind never does — so a door put
/// served inside a pass once a tip resolves is store work only
/// (`AccountStatePlane::origination_mints`; `account-data-plane.md` § The
/// client-side lifecycle, the pump bullet → *Commands and passes*).
#[tokio::test]
async fn a_tip_sealed_origination_mints_only_while_no_tip_resolves() {
    /// A publishing plane's requester with nothing behind it: the question
    /// is answered from the store alone, so any request is the failure.
    #[derive(Debug)]
    struct NoNest(&'static str);
    impl std::fmt::Display for NoNest {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "no nest in this test: {}", self.0)
        }
    }
    impl fauna_protocol::RpcErrorClass for NoNest {
        fn is_rejection(&self) -> bool {
            false
        }
    }
    struct Offline;
    impl fauna_protocol::RpcRequester for Offline {
        type Error = NoNest;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, NoNest>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            Err(NoNest(kind))
        }
    }

    let seat = Seat::open(0x41, 0xC3, 0x09).await;
    let requester = Offline;
    let sk = seat.device();
    let trust = seat.trust();
    let fleet = AccountStatePlane::new(
        &seat.store,
        &requester,
        &seat.schedule,
        &sk,
        &trust,
        ACCOUNT_STATE_FLEET_SCOPE,
    )
    .unwrap();

    assert!(
        fleet
            .origination_mints(KIND_GROUP_RECEPTION_KEY)
            .await
            .unwrap(),
        "no tip yet: the door would mint"
    );
    assert!(
        !fleet.origination_mints(KIND_DEVICE_SET).await.unwrap(),
        "a Gen0 machinery kind never mints"
    );

    seat.establish_fleet_tip().await;
    for kind in [KIND_GROUP_RECEPTION_KEY, KIND_GROUP_MACHINERY_ROOT] {
        assert!(
            !fleet.origination_mints(kind).await.unwrap(),
            "{kind}: an acked tip resolves, so the door writes without minting"
        );
    }
}

/// The success criterion, verbatim: mint on one seat, admit on
/// another, restart, and the scope is still there with its roster and
/// generation tip resolvable — from disk, both seats.
#[tokio::test(flavor = "multi_thread")]
async fn the_ceremony_scope_survives_a_restart_on_both_seats() {
    let mut alice = Seat::open(0x21, 0xA1, 0x07).await;
    let mut bob = Seat::open(0x31, 0xB2, 0x08).await;
    alice.establish_fleet_tip().await;
    bob.establish_fleet_tip().await;

    let run = run_ceremony(&alice, &bob);
    let scope_str = GroupScope::new(run.scope_id).to_string();

    // The driver writes, both seats: custody rows through the fleet-scope
    // account plane, machinery rows through the group plane.
    alice
        .write_custody_rows(&run.alice_root_row, &run.alice_reception)
        .await;
    bob.write_custody_rows(&run.bob_root_row, &run.bob_reception)
        .await;

    let alice_root = run.alice_root_row.machinery_root().unwrap();
    {
        let requester = NoFeed;
        let sk = alice.device();
        let plane = GroupStatePlane::new(&alice.store, &requester, &alice_root, &sk, &run.scope_id)
            .unwrap();
        let report = plane
            .adopt_rows(&run.plane_rows)
            .await
            .expect("alice adopts");
        assert_eq!(report.adopted, 4, "birth + 2 roster + mint: {report:?}");
    }
    let bob_root = run.bob_root_row.machinery_root().unwrap();
    {
        let requester = NoFeed;
        let sk = bob.device();
        let plane =
            GroupStatePlane::new(&bob.store, &requester, &bob_root, &sk, &run.scope_id).unwrap();
        let report = plane.adopt_rows(&run.bob_rows).await.expect("bob adopts");
        assert_eq!(report.adopted, 4, "{report:?}");
    }

    // ── The restart ─────────────────────────────────────────────────────────
    alice.restart().await;
    bob.restart().await;

    // Both seats: the scope's four machinery rows are on disk.
    for (name, seat) in [("alice", &alice), ("bob", &bob)] {
        let rows = seat.store.group_scope_states(&scope_str).await.unwrap();
        assert_eq!(rows.len(), 4, "{name}: persisted group rows");
        assert!(
            rows.iter()
                .any(|r| r.kind == KIND_GROUP_BIRTH && r.key == GROUP_BIRTH_KEY),
            "{name}: birth row persisted"
        );
    }

    // Bob's custody rows rest in the reopened account store (the local
    // replica's plaintext half — the sealed halves ride the relay plane).
    let reception_key = run.bob_reception.logical_key().unwrap();
    let reception_entry = bob
        .store
        .state(KIND_GROUP_RECEPTION_KEY, &reception_key)
        .await
        .unwrap()
        .expect("reception keypair row persisted");
    let reloaded_reception: GroupReceptionKeyRecord =
        canonical_decode(&reception_entry.value).unwrap();
    let root_entry = bob
        .store
        .state(
            KIND_GROUP_MACHINERY_ROOT,
            &GroupHeldRootRecord::logical_key_for(&run.scope_id),
        )
        .await
        .unwrap()
        .expect("held root row persisted");
    let reloaded_root_row: GroupHeldRootRecord = canonical_decode(&root_entry.value).unwrap();

    // The reloaded root verifies against the PERSISTED birth record's
    // commitment — the id-bound-at-birth check, from disk.
    let birth_entry = bob
        .store
        .group_state(&scope_str, KIND_GROUP_BIRTH, GROUP_BIRTH_KEY)
        .await
        .unwrap()
        .expect("birth row");
    let birth: GroupBirthRecord = canonical_decode(&birth_entry.value).unwrap();
    assert_eq!(
        reloaded_root_row.machinery_root().unwrap().commitment(),
        birth.machinery_root_commit,
        "reloaded root commits to the persisted birth record"
    );

    // The admission bundle still opens after the restart, with the RELOADED
    // reception secret — and its retained keys make the persisted mint DAG
    // resolvable.
    let invited = run
        .bob_cfg
        .invited
        .iter()
        .find(|r| r.scope_id == run.scope_id)
        .expect("bob's ceremony record");
    let deliver_env: EmbedAsBytes = canonical_decode(&invited.deliver).unwrap();
    let deliver: GroupShareDeliver =
        verify_group_share_deliver(&deliver_env, &alice.actor.actor_id()).expect("deliver");
    let keypair = reloaded_reception.keypair().unwrap();
    let (opened_root, retained) = open_group_admission_bundle(
        &deliver.admission_wrap,
        &keypair.secret,
        &run.scope_id,
        &run.bob_entry_id,
        &birth,
    )
    .expect("admission bundle opens with the reloaded reception secret");
    assert_eq!(
        opened_root.commitment(),
        birth.machinery_root_commit,
        "the re-opened root is the scope's"
    );

    // The scope resolves from persisted rows alone, both seats.
    let bob_rows = bob.store.group_scope_states(&scope_str).await.unwrap();
    assert_eq!(
        resolve_from_store_rows(&bob_rows, &run.scope_id, &alice.actor.actor_id(), &retained),
        run.generation_id,
        "bob's persisted scope resolves the delivered tip"
    );
    let alice_rows = alice.store.group_scope_states(&scope_str).await.unwrap();
    assert_eq!(
        resolve_from_store_rows(
            &alice_rows,
            &run.scope_id,
            &alice.actor.actor_id(),
            &retained
        ),
        run.generation_id,
        "alice's persisted scope resolves the same tip"
    );
}

/// A canned group-scope feed serving one member's relay rows — the share
/// serve set's shape without its transport (that wiring is the `p2p-share`
/// workstream's). Honors the per-writer frontier so a caught-up walk
/// terminates on an empty page.
struct FakeGroupFeed {
    scope: String,
    rows: Vec<SyncChange>,
}

impl fauna_protocol::RpcRequester for FakeGroupFeed {
    type Error = anyhow::Error;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> anyhow::Result<Reply>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        anyhow::ensure!(
            kind == "fauna.sync.changes.list",
            "the canned feed serves the feed only, got {kind}"
        );
        let req: SyncChangesListRequest =
            decode_strict(&encode_canonical(&payload)?).expect("feed request shape");
        anyhow::ensure!(req.scope.as_deref() == Some(self.scope.as_str()));
        let frontier = req.frontier.unwrap_or_default();
        let reply = SyncChangesListReply {
            changes: self
                .rows
                .iter()
                .filter(|r| {
                    let writer = r.origin_writer.as_deref().unwrap_or_default();
                    r.origin_seq.unwrap_or(0) > frontier.get(writer).copied().unwrap_or(0)
                })
                .cloned()
                .collect(),
            ..Default::default()
        };
        Ok(decode_strict(&encode_canonical(&reply)?)?)
    }
}

/// One member's sealed relay rows as the feed a peer would serve.
async fn feed_of(seat: &Seat, scope_str: &str) -> FakeGroupFeed {
    let relay = seat
        .store
        .relay_rows(scope_str, ItemClass::StateEntry.as_wire(), &[], 100)
        .await
        .unwrap();
    let rows = relay
        .iter()
        .enumerate()
        .map(|(i, r)| SyncChange {
            seq: (i + 1) as i64,
            path_hash: hex::encode(r.item_key.as_slice()),
            change_type: r.op.clone(),
            origin_writer: Some(r.writer.to_hex()),
            origin_seq: Some(r.writer_seq as i64),
            entry: r.entry.clone().map(Into::into),
            ..Default::default()
        })
        .collect();
    FakeGroupFeed {
        scope: scope_str.to_string(),
        rows,
    }
}

/// The walk's first contact holds a served birth row to its scope. Bob holds
/// the root, so he can seal a birth row under the scope's coordinates — here
/// one naming HIMSELF as authority, which hashes to another scope's id. Carol
/// holds the root with no birth row yet (a sibling device fresh off the fleet)
/// and is served Bob's row first: it must be skipped, never landed, because
/// the birth kind is `Immutable` — landed, it would root the scope in Bob for
/// good and refuse Alice's real record after it. Served next, the real one
/// lands.
#[tokio::test(flavor = "multi_thread")]
async fn a_served_birth_row_that_is_another_scopes_never_lands_on_first_contact() {
    let alice = Seat::open(0x21, 0xA1, 0x07).await;
    let bob = Seat::open(0x31, 0xB2, 0x08).await;
    alice.establish_fleet_tip().await;
    bob.establish_fleet_tip().await;

    let run = run_ceremony(&alice, &bob);
    let scope_str = GroupScope::new(run.scope_id).to_string();
    let root = run.alice_root_row.machinery_root().unwrap();
    {
        let requester = NoFeed;
        let sk = alice.device();
        let plane =
            GroupStatePlane::new(&alice.store, &requester, &root, &sk, &run.scope_id).unwrap();
        plane
            .adopt_rows(&run.plane_rows)
            .await
            .expect("alice adopts");
    }

    // Bob's forgery, sealed under the real scope's coordinates onto his own
    // relay plane (below the plane's own write funnel, which refuses it).
    let real_birth: GroupBirthRecord = canonical_decode(
        &run.plane_rows
            .iter()
            .find(|r| r.kind == KIND_GROUP_BIRTH)
            .expect("the snapshot carries its birth row")
            .value,
    )
    .unwrap();
    let forged = GroupBirthRecord {
        authority_actor: bob.actor.actor_id(),
        ..real_birth.clone()
    };
    assert_ne!(
        fauna_core::group_scope::group_scope_id(&forged).unwrap(),
        run.scope_id
    );
    let bob_writer = bob.store.writer();
    let sealed = fauna_core::account_entry_crypto::seal_entry(
        &fauna_core::crypto::GroupMachinerySchedule::derive(&root).for_kind(KIND_GROUP_BIRTH),
        &fauna_core::account_entry_crypto::EntryCoordinates {
            writer_id: bob_writer.0,
            writer_seq: 1,
            scope: &scope_str,
        },
        &fauna_core::account_entry_crypto::EntryPlaintext {
            kind: KIND_GROUP_BIRTH.into(),
            key: GROUP_BIRTH_KEY.into(),
            merge_meta: None,
            value: canonical_encode(&forged).unwrap().into(),
            tombstone: false,
        },
        &bob.device(),
    )
    .unwrap();
    bob.store
        .record_relay_row(&fauna_account_store::types::RelayRow {
            scope: scope_str.clone(),
            item_class: ItemClass::StateEntry.as_wire().to_string(),
            writer: bob_writer,
            writer_seq: 1,
            item_key: sealed.item_key.to_vec(),
            op: fauna_protocol::account_state::OP_STATE_PUT.to_string(),
            entry: Some(sealed.envelope),
            feed_seq: None,
        })
        .await
        .unwrap();

    let carol = Seat::open(0x51, 0xD4, 0x0A).await;
    let sk = carol.device();
    let forged_feed = feed_of(&bob, &scope_str).await;
    let plane =
        GroupStatePlane::new(&carol.store, &forged_feed, &root, &sk, &run.scope_id).unwrap();
    let report = plane.walk().await.expect("carol walks bob's feed");
    assert_eq!(
        (report.rows, report.unmergeable, report.applied),
        (1, 1, 0),
        "{report:?}"
    );
    assert!(
        carol
            .store
            .group_state(&scope_str, KIND_GROUP_BIRTH, GROUP_BIRTH_KEY)
            .await
            .unwrap()
            .is_none(),
        "a birth row that is another scope's never lands"
    );

    let real_feed = feed_of(&alice, &scope_str).await;
    let plane = GroupStatePlane::new(&carol.store, &real_feed, &root, &sk, &run.scope_id).unwrap();
    let report = plane.walk().await.expect("carol walks alice's feed");
    assert_eq!(report.applied, 4, "{report:?}");
    let held = carol
        .store
        .group_state(&scope_str, KIND_GROUP_BIRTH, GROUP_BIRTH_KEY)
        .await
        .unwrap()
        .expect("the real birth row lands after the refused one");
    assert_eq!(held.value, canonical_encode(&real_birth).unwrap().to_vec());
}

/// The serve shape: a root-holding member walks another member's sealed rows
/// off a feed and lands all four through the machinery-root trial-open, with
/// the frontier accounting the walk; a holder WITHOUT the root — the
/// custodian posture — opens none of them.
#[tokio::test(flavor = "multi_thread")]
async fn sealed_rows_walk_member_to_member_and_stay_dark_without_the_root() {
    let alice = Seat::open(0x21, 0xA1, 0x07).await;
    let bob = Seat::open(0x31, 0xB2, 0x08).await;
    alice.establish_fleet_tip().await;
    bob.establish_fleet_tip().await;

    let run = run_ceremony(&alice, &bob);
    let scope_str = GroupScope::new(run.scope_id).to_string();
    let alice_root = run.alice_root_row.machinery_root().unwrap();

    // Alice's own plane writes — journal + sealed relay rows.
    {
        let requester = NoFeed;
        let sk = alice.device();
        let plane = GroupStatePlane::new(&alice.store, &requester, &alice_root, &sk, &run.scope_id)
            .unwrap();
        plane
            .adopt_rows(&run.plane_rows)
            .await
            .expect("alice adopts");
    }
    let feed = feed_of(&alice, &scope_str).await;
    assert_eq!(feed.rows.len(), 4, "alice serves four sealed rows");

    // Bob — holding the root from his admission — walks the feed: every row
    // opens, lands, and is accounted.
    let bob_root = run.bob_root_row.machinery_root().unwrap();
    {
        let sk = bob.device();
        let plane = GroupStatePlane::new(&bob.store, &feed, &bob_root, &sk, &run.scope_id).unwrap();
        let report = plane.walk().await.expect("bob walks");
        assert_eq!(report.applied, 4, "{report:?}");
        assert_eq!(report.unopened, 0, "{report:?}");
        let rows = bob.store.group_scope_states(&scope_str).await.unwrap();
        assert_eq!(rows.len(), 4);
        // A second walk serves nothing: the frontier accounted the first.
        let again = plane.walk().await.expect("bob re-walks");
        assert_eq!(again.rows, 0, "{again:?}");
    }

    // A holder without the scope's root — a custodian, or any stranger —
    // sees nothing past the coordinates: every row stays unopened.
    let mallory = Seat::open(0x41, 0xC3, 0x09).await;
    let wrong_root = fauna_core::crypto::GroupMachineryRoot::mint();
    let sk = mallory.device();
    let plane =
        GroupStatePlane::new(&mallory.store, &feed, &wrong_root, &sk, &run.scope_id).unwrap();
    let report = plane.reconcile().await.expect("mallory walks");
    assert_eq!(report.unopened, 4, "{report:?}");
    assert_eq!(
        report.applied + report.merged + report.kept,
        0,
        "{report:?}"
    );
    assert!(
        mallory
            .store
            .group_scope_states(&scope_str)
            .await
            .unwrap()
            .is_empty(),
        "nothing lands without the root"
    );
}

// ── The group set's content plane (`p2p-shared-set-build.md` § Build design
// — the group set, decisions 2–5) ─────────────────────────────────────────

/// Decisions 2–5 end to end on two real replicas of two accounts, no nest:
/// a member's file row and the authority's file row and set name seal under
/// the ceremony's generation tip (the v2 envelope naming it), walk to the
/// other replica, open there with the key its own wrap yields, and fold under
/// the writer check; a row a member seals for a stranger who holds no wrap at
/// that generation opens but is refused and counted, never folded; both
/// replicas derive the same content key ring; and all of it holds after a
/// restart, from disk.
#[tokio::test(flavor = "multi_thread")]
async fn group_set_rows_seal_under_the_tip_walk_and_fold_across_a_restart() {
    let (mut alice, mut bob, run) = admitted_pair().await;
    let scope_str = GroupScope::new(run.scope_id).to_string();
    let alice_root = run.alice_root_row.machinery_root().unwrap();
    let bob_root = run.bob_root_row.machinery_root().unwrap();
    let carol = Seat::open(0x51, 0xD4, 0x0A).await;

    // Bob writes his own file and — sealing it himself, under the tip he
    // holds — a file authored by Carol, who is no member.
    {
        let sk = bob.device();
        let plane =
            GroupStatePlane::new(&bob.store, &NoFeed, &bob_root, &sk, &run.scope_id).unwrap();
        let (item, value, stamp) = file_row(
            &run,
            &bob,
            Some(entry_core_of(&run, &bob)),
            0x11,
            1_700_000_100_000,
        );
        plane
            .put(&item, value, Some(stamp))
            .await
            .expect("bob's file");
        let stranger = fauna_core::group_scope::RosterEntryCore {
            scope_id: run.scope_id,
            member_actor: carol.actor.actor_id(),
            admission_salt: [0x5C; 32],
        };
        let (item, value, stamp) = file_row(&run, &carol, Some(stranger), 0x33, 1_700_000_100_001);
        plane
            .put(&item, value, Some(stamp))
            .await
            .expect("the stranger's row seals");
    }
    // Alice — the authority — names the set and writes a file of her own.
    {
        let sk = alice.device();
        let plane =
            GroupStatePlane::new(&alice.store, &NoFeed, &alice_root, &sk, &run.scope_id).unwrap();
        let (item, value, stamp) = name_row(&run, &alice, "Holiday photos", 1_700_000_100_002);
        plane
            .put(&item, value, Some(stamp))
            .await
            .expect("the name");
        let (item, value, stamp) = file_row(&run, &alice, None, 0x22, 1_700_000_100_003);
        plane
            .put(&item, value, Some(stamp))
            .await
            .expect("alice's file");
    }

    // The content rows rest in the v2 envelope naming the ceremony's tip.
    let bob_feed = feed_of(&bob, &scope_str).await;
    let generation_sealed = bob_feed
        .rows
        .iter()
        .filter(|r| {
            r.entry
                .as_deref()
                .and_then(|e| fauna_core::account_entry_crypto::peek_generation_id(e))
                == Some(run.generation_id)
        })
        .count();
    assert_eq!(
        generation_sealed, 2,
        "both of bob's file rows are tip-sealed"
    );

    // Alice walks Bob's rows: the machinery is already hers, his file
    // lands, the stranger's is refused and counted.
    {
        let sk = alice.device();
        let plane =
            GroupStatePlane::new(&alice.store, &bob_feed, &alice_root, &sk, &run.scope_id).unwrap();
        let report = plane.walk().await.expect("alice walks bob");
        assert_eq!(
            (
                report.rows,
                report.kept,
                report.applied,
                report.refused,
                report.unopened
            ),
            (6, 4, 1, 1, 0),
            "{report:?}"
        );
    }
    // Bob walks Alice's: the name and her file open and land.
    {
        let alice_feed = feed_of(&alice, &scope_str).await;
        let sk = bob.device();
        let plane =
            GroupStatePlane::new(&bob.store, &alice_feed, &bob_root, &sk, &run.scope_id).unwrap();
        let report = plane.walk().await.expect("bob walks alice");
        assert_eq!(
            (report.applied, report.refused, report.unopened),
            (2, 0, 0),
            "{report:?}"
        );
    }

    alice.restart().await;
    bob.restart().await;

    // From disk, both seats: the same two honoured files and the same name.
    // Bob's own store still holds the stranger's row he sealed himself — the
    // walk never saw it — so his fold, the second line, refuses it there.
    let mut rings = Vec::new();
    for (name, seat, stored_refusals) in [("alice", &alice, 0), ("bob", &bob, 1)] {
        let rows = seat.store.group_scope_states(&scope_str).await.unwrap();
        let fold = fold_group_files(&run.scope_id, &rows).expect("the scope reads");
        let mut authors: Vec<_> = fold.files.values().map(|f| f.member).collect();
        authors.sort_by_key(|a| a.0);
        let mut want = vec![alice.actor.actor_id(), bob.actor.actor_id()];
        want.sort_by_key(|a| a.0);
        assert_eq!(authors, want, "{name}: the honoured files' authors");
        assert_eq!(fold.refused, stored_refusals, "{name}: the fold's refusals");
        assert!(
            !fold
                .files
                .contains_key(&fauna_core::group_file::group_file_logical_key(&[0x33; 32])),
            "{name}: the stranger's row never folds"
        );
        let named = fold_group_name(&run.scope_id, &rows).expect("the scope reads");
        assert_eq!(
            (named.name.as_ref().map(|n| n.name()), named.refused),
            (Some("Holiday photos"), 0),
            "{name}: the authority's name folds"
        );

        let ring = group_set_keys(&seat.store, &run.scope_id).await.unwrap();
        let (tip, key) = ring.tip().expect("a keyable tip");
        assert_eq!(tip, run.generation_id, "{name}: the ring's tip");
        rings.push(*key);
    }
    assert_eq!(
        rings[0], rings[1],
        "both members derive the one content key"
    );
}

/// Decision 2's "written by the authority", end to end on two replicas: a
/// member holding the tip key seals a later-stamped rename, signed by his
/// own device under his own cert. The authority's replica refuses it at the
/// walk, before the merge, and keeps the authority's name. The member's own
/// replica stored the rename itself, so his fold — the second line — refuses
/// it there. Then the authority revokes the device that signed the name,
/// and the authority's fold retires the name with that device's other
/// signatures.
#[tokio::test(flavor = "multi_thread")]
async fn only_a_current_authority_device_names_the_set() {
    let (alice, bob, run) = admitted_pair().await;
    let scope_str = GroupScope::new(run.scope_id).to_string();
    let alice_root = run.alice_root_row.machinery_root().unwrap();
    let bob_root = run.bob_root_row.machinery_root().unwrap();

    // Alice — the authority — names the set; Bob renames it later.
    {
        let sk = alice.device();
        let plane =
            GroupStatePlane::new(&alice.store, &NoFeed, &alice_root, &sk, &run.scope_id).unwrap();
        let (item, value, stamp) = name_row(&run, &alice, "Holiday photos", 1_700_000_100_000);
        plane
            .put(&item, value, Some(stamp))
            .await
            .expect("alice's name");
    }
    {
        let sk = bob.device();
        let plane =
            GroupStatePlane::new(&bob.store, &NoFeed, &bob_root, &sk, &run.scope_id).unwrap();
        let (item, value, stamp) = name_row(&run, &bob, "Bob's now", 1_700_000_200_000);
        plane
            .put(&item, value, Some(stamp))
            .await
            .expect("a member's rename seals under the tip");
    }

    // The walk: Alice refuses Bob's rename before the merge.
    {
        let bob_feed = feed_of(&bob, &scope_str).await;
        let sk = alice.device();
        let plane =
            GroupStatePlane::new(&alice.store, &bob_feed, &alice_root, &sk, &run.scope_id).unwrap();
        let report = plane.walk().await.expect("alice walks bob");
        assert_eq!(
            (report.applied, report.refused, report.unopened),
            (0, 1, 0),
            "the member's rename is refused at the walk: {report:?}"
        );
    }
    let alice_rows = alice.store.group_scope_states(&scope_str).await.unwrap();
    let alice_name = fold_group_name(&run.scope_id, &alice_rows).expect("the scope reads");
    assert_eq!(
        (
            alice_name.name.as_ref().map(|n| n.name()),
            alice_name.refused
        ),
        (Some("Holiday photos"), 0),
        "the authority's name stands at the authority's replica"
    );
    assert_eq!(
        alice_name.name.as_ref().map(|n| n.device),
        Some(alice.device().verifying_key().to_bytes()),
        "honoured as the authority device's"
    );

    // The fold: Bob's own replica holds the rename he sealed; it never folds.
    let bob_rows = bob.store.group_scope_states(&scope_str).await.unwrap();
    let bob_name = fold_group_name(&run.scope_id, &bob_rows).expect("the scope reads");
    assert_eq!(
        (bob_name.name.as_ref().map(|n| n.name()), bob_name.refused),
        (None, 1),
        "the member's stored rename is refused in the fold"
    );

    // The revocation: Alice's actor revokes the device that signed the name.
    {
        let (key, record) = fauna_core::group_scope::sign_authority_revocation(
            alice.actor.signing_key(),
            Vec::new(),
            run.scope_id,
            alice.device().verifying_key().to_bytes(),
            1_700_000_300_000,
        );
        let sk = alice.device();
        let plane =
            GroupStatePlane::new(&alice.store, &NoFeed, &alice_root, &sk, &run.scope_id).unwrap();
        plane
            .put(
                &ItemId {
                    kind: fauna_protocol::group_state::KIND_GROUP_AUTHORITY_REVOCATION.into(),
                    key,
                },
                canonical_encode(&record).unwrap().to_vec(),
                None,
            )
            .await
            .expect("the revocation");
    }
    // The revocation is a row the writer check reads, so the plane re-folds
    // the name's cell over its writers' latest rows (`group-set.md`
    // decision 5, rule (ii)): neither the revoked device's name nor Bob's
    // rename is honoured, and the cell folds as no row — nothing left for
    // the fold's second line to refuse.
    let alice_rows = alice.store.group_scope_states(&scope_str).await.unwrap();
    let retired = fold_group_name(&run.scope_id, &alice_rows).expect("the scope reads");
    assert_eq!(
        (retired.name.as_ref().map(|n| n.name()), retired.refused),
        (None, 0),
        "a name signed by a since-revoked authority device is retired"
    );
}

/// A revoked authority device's far-future-stamped name never holds the cell
/// against a current authority device's rename. The
/// authority's second, stolen device names the set stamped `i64::MAX / 2`
/// and Bob lands it; the authority revokes that device and renames from its
/// own device at an ordinary instant. Bob's fold shows the rename, where
/// latest-wins alone would keep the revoked row and fold no name at all.
/// Both rows reach Bob under one relay writer (Alice's replica), so the
/// re-fold's one-row-per-writer pick already supersedes the stolen row here;
/// the writer check in the re-fold is pinned by the cross-writer form below.
#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_authority_devices_future_stamped_name_never_holds_the_cell() {
    let (alice, bob, run) = admitted_pair().await;
    let scope_str = GroupScope::new(run.scope_id).to_string();
    let alice_root = run.alice_root_row.machinery_root().unwrap();
    let bob_root = run.bob_root_row.machinery_root().unwrap();
    // A second authority device of Alice's, later stolen.
    let stolen = ed25519_dalek::SigningKey::from_bytes(&[0x7A; 32]);
    let alice_plane_write = |item: ItemId, value: Vec<u8>, stamp: Option<Vec<u8>>| {
        let (alice, run, alice_root) = (&alice, &run, &alice_root);
        async move {
            let sk = alice.device();
            GroupStatePlane::new(&alice.store, &NoFeed, alice_root, &sk, &run.scope_id)
                .unwrap()
                .put(&item, value, stamp)
                .await
                .expect("alice's write");
        }
    };
    let bob_walks_alice = || {
        let (alice, bob, run, scope_str, bob_root) = (&alice, &bob, &run, &scope_str, &bob_root);
        async move {
            let feed = feed_of(alice, scope_str).await;
            let sk = bob.device();
            GroupStatePlane::new(&bob.store, &feed, bob_root, &sk, &run.scope_id)
                .unwrap()
                .walk()
                .await
                .expect("bob walks alice")
        }
    };

    // The compromised second device names the set far in the future.
    let (item, value, stamp) = name_row_by(&run, &alice, &stolen, "Hijacked", i64::MAX / 2);
    alice_plane_write(item, value, Some(stamp)).await;
    let report = bob_walks_alice().await;
    assert_eq!(report.applied, 1, "bob lands the name: {report:?}");

    // The authority renames from its own device at an ordinary instant, and
    // revokes the stolen one. The revocation reaches Bob first — carried by
    // another replica; it verifies under the authority actor's signature
    // whoever carries it, so it is planted here as Bob's own write.
    let (item, value, stamp) = name_row(&run, &alice, "Holiday photos", 1_700_000_400_000);
    alice_plane_write(item, value, Some(stamp)).await;
    let (key, record) = fauna_core::group_scope::sign_authority_revocation(
        alice.actor.signing_key(),
        Vec::new(),
        run.scope_id,
        stolen.verifying_key().to_bytes(),
        1_700_000_300_000,
    );
    {
        let sk = bob.device();
        GroupStatePlane::new(&bob.store, &NoFeed, &bob_root, &sk, &run.scope_id)
            .unwrap()
            .put(
                &ItemId {
                    kind: fauna_protocol::group_state::KIND_GROUP_AUTHORITY_REVOCATION.into(),
                    key,
                },
                canonical_encode(&record).unwrap().to_vec(),
                None,
            )
            .await
            .expect("the revocation lands at bob");
    }
    let report = bob_walks_alice().await;
    assert_eq!(report.unopened, 0, "the rename opens at bob: {report:?}");
    assert_eq!(
        report.refused, 0,
        "the rename passes the writer check: {report:?}"
    );

    let bob_rows = bob.store.group_scope_states(&scope_str).await.unwrap();
    let named = fold_group_name(&run.scope_id, &bob_rows).expect("the scope reads");
    assert_eq!(
        (named.name.as_ref().map(|n| n.name()), named.refused),
        (Some("Holiday photos"), 0),
        "the current authority device's rename takes the cell from the revoked one's"
    );
    assert_eq!(
        named.name.as_ref().map(|n| n.device),
        Some(alice.device().verifying_key().to_bytes()),
        "honoured as the current authority device's"
    );
}

/// The same theft with the stolen device's row carried by a replica of its
/// own: Bob's replica relays the stolen
/// device's `i64::MAX / 2` name under Bob's writer while the authority's
/// rename arrives under Alice's, so the re-fold holds one live row per writer
/// and only its writer check keeps the revoked device's row from winning on
/// stamp. Dropping that check folds the cell to the stolen row, which the
/// display fold then refuses — no name at all.
#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_authority_devices_name_from_another_replica_never_holds_the_cell() {
    let (alice, bob, run) = admitted_pair().await;
    let scope_str = GroupScope::new(run.scope_id).to_string();
    let alice_root = run.alice_root_row.machinery_root().unwrap();
    let bob_root = run.bob_root_row.machinery_root().unwrap();
    let stolen = ed25519_dalek::SigningKey::from_bytes(&[0x7A; 32]);
    let bob_puts = |item: ItemId, value: Vec<u8>, stamp: Option<Vec<u8>>| {
        let (bob, run, bob_root) = (&bob, &run, &bob_root);
        async move {
            let sk = bob.device();
            GroupStatePlane::new(&bob.store, &NoFeed, bob_root, &sk, &run.scope_id)
                .unwrap()
                .put(&item, value, stamp)
                .await
                .expect("bob carries the row");
        }
    };

    // The stolen authority device names the set far in the future; the row
    // reaches Bob through a replica other than Alice's — here, his own.
    let (item, value, stamp) = name_row_by(&run, &alice, &stolen, "Hijacked", i64::MAX / 2);
    bob_puts(item, value, Some(stamp)).await;
    let bob_rows = bob.store.group_scope_states(&scope_str).await.unwrap();
    let named = fold_group_name(&run.scope_id, &bob_rows).expect("the scope reads");
    assert_eq!(
        named.name.as_ref().map(|n| n.name()),
        Some("Hijacked"),
        "before the revocation the second authority device's name stands"
    );

    // The authority renames from its own device and revokes the stolen one.
    {
        let sk = alice.device();
        let (item, value, stamp) = name_row(&run, &alice, "Holiday photos", 1_700_000_400_000);
        GroupStatePlane::new(&alice.store, &NoFeed, &alice_root, &sk, &run.scope_id)
            .unwrap()
            .put(&item, value, Some(stamp))
            .await
            .expect("alice's rename");
    }
    let (key, record) = fauna_core::group_scope::sign_authority_revocation(
        alice.actor.signing_key(),
        Vec::new(),
        run.scope_id,
        stolen.verifying_key().to_bytes(),
        1_700_000_300_000,
    );
    bob_puts(
        ItemId {
            kind: fauna_protocol::group_state::KIND_GROUP_AUTHORITY_REVOCATION.into(),
            key,
        },
        canonical_encode(&record).unwrap().to_vec(),
        None,
    )
    .await;
    let report = {
        let feed = feed_of(&alice, &scope_str).await;
        let sk = bob.device();
        GroupStatePlane::new(&bob.store, &feed, &bob_root, &sk, &run.scope_id)
            .unwrap()
            .walk()
            .await
            .expect("bob walks alice")
    };
    assert_eq!(
        (report.unopened, report.refused),
        (0, 0),
        "the rename opens and passes the writer check: {report:?}"
    );

    let bob_rows = bob.store.group_scope_states(&scope_str).await.unwrap();
    let named = fold_group_name(&run.scope_id, &bob_rows).expect("the scope reads");
    assert_eq!(
        (named.name.as_ref().map(|n| n.name()), named.refused),
        (Some("Holiday photos"), 0),
        "the current authority device's rename takes the cell from the revoked device's row \
         relayed under another writer"
    );
}

/// The fail-safe gap at a replica that holds the scope's machinery root but
/// no generation key (a custodian's posture): it walks a member's feed and
/// lands the machinery, but every content row stays unopened, and its own
/// content write refuses and lands nothing — never a machinery-root fallback.
#[tokio::test(flavor = "multi_thread")]
async fn a_root_holder_without_a_generation_key_neither_opens_nor_writes_content() {
    let (alice, _bob, run) = admitted_pair().await;
    let scope_str = GroupScope::new(run.scope_id).to_string();
    let root = run.alice_root_row.machinery_root().unwrap();
    {
        let sk = alice.device();
        let plane = GroupStatePlane::new(&alice.store, &NoFeed, &root, &sk, &run.scope_id).unwrap();
        let (item, value, stamp) = file_row(&run, &alice, None, 0x22, 1_700_000_100_003);
        plane
            .put(&item, value, Some(stamp))
            .await
            .expect("alice's file");
    }

    let custodian = Seat::open(0x61, 0xE5, 0x0B).await;
    let feed = feed_of(&alice, &scope_str).await;
    let sk = custodian.device();
    let plane = GroupStatePlane::new(&custodian.store, &feed, &root, &sk, &run.scope_id).unwrap();
    let report = plane.walk().await.expect("the custodian walks");
    assert_eq!(
        (report.applied, report.unopened),
        (4, 1),
        "machinery lands, the content row stays dark: {report:?}"
    );
    let err = plane
        .put(
            &ItemId {
                kind: KIND_GROUP_NAME.into(),
                key: GROUP_NAME_KEY.into(),
            },
            b"mine now".to_vec(),
            Some(
                LwwStamp {
                    at_ms: 1,
                    writer: sk.verifying_key().to_bytes(),
                }
                .encode()
                .unwrap(),
            ),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no admissible"), "{err}");
    assert!(
        custodian
            .store
            .group_state(&scope_str, KIND_GROUP_NAME, GROUP_NAME_KEY)
            .await
            .unwrap()
            .is_none(),
        "the refused write landed nothing"
    );
}

/// A removed member's post-removal rows, merged by one stale replica, never
/// hold a cell anywhere the removal has merged — `group-set.md` decision 5, the
/// 2026-11-02 ruling: rule (i), a removed member is honoured as of its
/// removal; rule (ii), a writer-checked cell folds over each writer's latest
/// relay row, honoured rows only.
///
/// Alice is the authority and Bob a member. Carol is a second replica that
/// holds the generation Bob still holds and goes stale: it has walked the
/// files but not the removal. (The two-seat ceremony admits one member, so
/// the stale co-member is a sibling replica of the authority's actor — the
/// reader's mechanism is the same whoever's replica is stale — and no
/// severance mint follows the removal: the bound reads the roster's signed
/// removal instant, never a mint.)
///
/// Bob writes two honest files and Alice one. Alice removes Bob. Bob, still
/// holding the generation, signs three rows naming it, stamped a year after
/// the removal: a put on Alice's path, a put on a fresh path, and a put on
/// his own second path. Carol walks Bob and merges all three (stale, by
/// design), then walks Alice and learns the removal; Alice walks Carol. At
/// both: Alice's path folds as Alice's row, the fresh path as no row, Bob's
/// second path as no row (his refused row superseded his own earlier one —
/// the ruling's residual (3)), and his first path keeps his pre-removal row.
#[tokio::test(flavor = "multi_thread")]
async fn a_removed_members_post_removal_rows_never_hold_a_cell_through_a_stale_peer() {
    const REMOVED_AT: i64 = 1_700_000_200_000;
    const PLANTED_AT: i64 = REMOVED_AT + 365 * 24 * 3_600_000;
    const BOB_PATH: u8 = 0x11;
    const BOB_SECOND_PATH: u8 = 0x13;
    const ALICE_PATH: u8 = 0x22;
    const FRESH_PATH: u8 = 0x44;

    let (alice, bob, run) = admitted_pair().await;
    let alice_root = run.alice_root_row.machinery_root().unwrap();
    let bob_root = run.bob_root_row.machinery_root().unwrap();
    // Carol: a sibling replica of the authority's actor, admitted from the
    // same snapshot and custody rows.
    let carol = Seat::open(0x21, 0xA3, 0x0C).await;
    carol.establish_fleet_tip().await;
    carol
        .write_custody_rows(&run.alice_root_row, &run.alice_reception)
        .await;
    {
        let sk = carol.device();
        let plane =
            GroupStatePlane::new(&carol.store, &NoFeed, &alice_root, &sk, &run.scope_id).unwrap();
        let report = plane.adopt_rows(&run.plane_rows).await.expect("adopt");
        assert_eq!(report.refused, 0, "{report:?}");
    }
    let bob_entry = || Some(entry_core_of(&run, &bob));
    let cell = |path: u8| fauna_core::group_file::group_file_logical_key(&[path; 32]);

    // Honest writes, everywhere.
    put_as(
        &bob,
        &bob_root,
        &run.scope_id,
        file_row(&run, &bob, bob_entry(), BOB_PATH, 1_700_000_100_000),
    )
    .await;
    put_as(
        &bob,
        &bob_root,
        &run.scope_id,
        file_row(&run, &bob, bob_entry(), BOB_SECOND_PATH, 1_700_000_100_001),
    )
    .await;
    put_as(
        &alice,
        &alice_root,
        &run.scope_id,
        file_row(&run, &alice, None, ALICE_PATH, 1_700_000_100_002),
    )
    .await;
    walk_as(&alice, &alice_root, &run.scope_id, &bob).await;
    walk_as(&carol, &alice_root, &run.scope_id, &bob).await;
    walk_as(&carol, &alice_root, &run.scope_id, &alice).await;

    // Alice removes Bob. Carol does not see it yet.
    let (removal_cell, removal) = fauna_core::group_scope::sign_roster_removal(
        &alice.device(),
        run.bob_entry_id,
        alice.device_cert(),
        REMOVED_AT,
    );
    {
        let sk = alice.device();
        GroupStatePlane::new(&alice.store, &NoFeed, &alice_root, &sk, &run.scope_id)
            .unwrap()
            .put(
                &ItemId {
                    kind: KIND_GROUP_ROSTER.into(),
                    key: removal_cell,
                },
                canonical_encode(&removal).unwrap().to_vec(),
                None,
            )
            .await
            .expect("the removal");
    }

    // Bob, removed but still holding the generation, plants a year ahead.
    for path in [ALICE_PATH, FRESH_PATH, BOB_SECOND_PATH] {
        put_as(
            &bob,
            &bob_root,
            &run.scope_id,
            file_row(&run, &bob, bob_entry(), path, PLANTED_AT),
        )
        .await;
    }
    walk_as(&carol, &alice_root, &run.scope_id, &bob).await;
    assert_eq!(
        honoured_files(&carol, &run.scope_id)
            .await
            .get(&cell(ALICE_PATH)),
        Some(&(bob.actor.actor_id(), PLANTED_AT)),
        "the stale replica merges the planted row, by design"
    );

    // Carol learns the removal; Alice walks Carol.
    walk_as(&carol, &alice_root, &run.scope_id, &alice).await;
    let report = walk_as(&alice, &alice_root, &run.scope_id, &carol).await;
    assert_eq!(
        report.refused, 3,
        "every planted row is refused at the authority: {report:?}"
    );

    let want: std::collections::BTreeMap<_, _> = [
        (cell(BOB_PATH), (bob.actor.actor_id(), 1_700_000_100_000)),
        (
            cell(ALICE_PATH),
            (alice.actor.actor_id(), 1_700_000_100_002),
        ),
    ]
    .into_iter()
    .collect();
    for (name, seat) in [("alice", &alice), ("carol", &carol)] {
        assert_eq!(
            honoured_files(seat, &run.scope_id).await,
            want,
            "{name}: Alice's path is Alice's, Bob's pre-removal file stands, the fresh path \
             and Bob's superseded path fold as no row"
        );
    }
}

/// Rule (i) holds, and rule (ii)'s fold reads ACROSS generations, when a
/// severance mint follows the removal (`group-set.md` decision 5, the
/// 2026-11-02 ruling). After Alice
/// removes Bob she mints G1 wrapped to herself alone and writes under it, so
/// one path's rows sit at two item keys: her own under G0 and G1, Bob's
/// plant under G0. Bob, who has seen neither the removal nor the mint, plants
/// a year ahead under G0 on Alice's re-written path, on a path only G1 holds,
/// and on a fresh path; Carol (stale) merges them, then learns the removal
/// and the mint; Alice walks Carol. At both: each of Alice's paths folds as
/// her G1 row, the fresh path as no row, Bob's pre-removal file stands.
#[tokio::test(flavor = "multi_thread")]
async fn a_removed_members_g0_rows_never_outrank_a_g1_write_through_a_stale_peer() {
    const REMOVED_AT: i64 = 1_700_000_200_000;
    const MINTED_AT: i64 = REMOVED_AT + 1;
    const PLANTED_AT: i64 = REMOVED_AT + 365 * 24 * 3_600_000;
    const BOB_PATH: u8 = 0x11;
    const ALICE_PATH: u8 = 0x22;
    const ALICE_G1_PATH: u8 = 0x33;
    const FRESH_PATH: u8 = 0x44;
    const ALICE_G0_PATH: u8 = 0x55;

    let (alice, bob, run) = admitted_pair().await;
    let alice_root = run.alice_root_row.machinery_root().unwrap();
    let bob_root = run.bob_root_row.machinery_root().unwrap();
    let carol = Seat::open(0x21, 0xA3, 0x0C).await;
    carol.establish_fleet_tip().await;
    carol
        .write_custody_rows(&run.alice_root_row, &run.alice_reception)
        .await;
    {
        let sk = carol.device();
        let plane =
            GroupStatePlane::new(&carol.store, &NoFeed, &alice_root, &sk, &run.scope_id).unwrap();
        let report = plane.adopt_rows(&run.plane_rows).await.expect("adopt");
        assert_eq!(report.refused, 0, "{report:?}");
    }
    let bob_entry = || Some(entry_core_of(&run, &bob));
    let cell = |path: u8| fauna_core::group_file::group_file_logical_key(&[path; 32]);
    let alice_plane_put = |item: ItemId, value: Vec<u8>| {
        let alice = &alice;
        let alice_root = &alice_root;
        let scope_id = run.scope_id;
        async move {
            let sk = alice.device();
            GroupStatePlane::new(&alice.store, &NoFeed, alice_root, &sk, &scope_id)
                .unwrap()
                .put(&item, value, None)
                .await
                .expect("the authority's write");
        }
    };

    // Honest G0 writes, everywhere.
    put_as(
        &bob,
        &bob_root,
        &run.scope_id,
        file_row(&run, &bob, bob_entry(), BOB_PATH, 1_700_000_100_000),
    )
    .await;
    put_as(
        &alice,
        &alice_root,
        &run.scope_id,
        file_row(&run, &alice, None, ALICE_PATH, 1_700_000_100_002),
    )
    .await;
    put_as(
        &alice,
        &alice_root,
        &run.scope_id,
        file_row(&run, &alice, None, ALICE_G0_PATH, 1_700_000_100_003),
    )
    .await;
    walk_as(&alice, &alice_root, &run.scope_id, &bob).await;
    walk_as(&carol, &alice_root, &run.scope_id, &bob).await;
    walk_as(&carol, &alice_root, &run.scope_id, &alice).await;

    // Alice removes Bob, then mints G1 to herself alone.
    let (removal_cell, removal) = fauna_core::group_scope::sign_roster_removal(
        &alice.device(),
        run.bob_entry_id,
        alice.device_cert(),
        REMOVED_AT,
    );
    alice_plane_put(
        ItemId {
            kind: KIND_GROUP_ROSTER.into(),
            key: removal_cell,
        },
        canonical_encode(&removal).unwrap().to_vec(),
    )
    .await;
    let alice_core = entry_core_of(&run, &alice);
    let g1 = fauna_mls::wrapped_blob::group_generation_wraps::build_group_mint(
        &[fauna_core::group_scope::RosterMember {
            entry_id: fauna_core::group_scope::roster_entry_id(&alice_core).unwrap(),
            member_actor: alice.actor.actor_id(),
            reception_pubkey: run.alice_reception.reception_pubkey().unwrap(),
            enrolled_at_ms: now_ms(),
        }],
        vec![run.generation_id],
        Vec::new(),
        &alice.device(),
        alice.device_cert(),
        MINTED_AT,
    )
    .expect("the severance mint");
    alice_plane_put(
        ItemId {
            kind: KIND_GROUP_GENERATION_MINT.into(),
            key: fauna_core::hex32::encode(&g1.generation_id),
        },
        canonical_encode(&g1.record).unwrap().to_vec(),
    )
    .await;
    for (path, at) in [
        (ALICE_PATH, MINTED_AT + 10),
        (ALICE_G1_PATH, MINTED_AT + 11),
    ] {
        put_as(
            &alice,
            &alice_root,
            &run.scope_id,
            file_row_in(g1.generation_id, &run, &alice, None, path, at),
        )
        .await;
    }
    let want: std::collections::BTreeMap<_, _> = [
        (cell(BOB_PATH), (bob.actor.actor_id(), 1_700_000_100_000)),
        (cell(ALICE_PATH), (alice.actor.actor_id(), MINTED_AT + 10)),
        (
            cell(ALICE_G1_PATH),
            (alice.actor.actor_id(), MINTED_AT + 11),
        ),
        (
            cell(ALICE_G0_PATH),
            (alice.actor.actor_id(), 1_700_000_100_003),
        ),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        honoured_files(&alice, &run.scope_id).await,
        want,
        "the authority's G1 writes hold before any plant"
    );

    // Bob, who saw neither the removal nor the mint, plants a year ahead
    // under G0 — his tip.
    for path in [ALICE_PATH, ALICE_G1_PATH, FRESH_PATH, ALICE_G0_PATH] {
        put_as(
            &bob,
            &bob_root,
            &run.scope_id,
            file_row(&run, &bob, bob_entry(), path, PLANTED_AT),
        )
        .await;
    }
    walk_as(&carol, &alice_root, &run.scope_id, &bob).await;
    assert_eq!(
        honoured_files(&carol, &run.scope_id)
            .await
            .get(&cell(ALICE_PATH)),
        Some(&(bob.actor.actor_id(), PLANTED_AT)),
        "the stale replica merges the planted row, by design"
    );

    // Carol learns the removal, the mint and the G1 rows; Alice walks Carol.
    walk_as(&carol, &alice_root, &run.scope_id, &alice).await;
    let report = walk_as(&alice, &alice_root, &run.scope_id, &carol).await;
    assert_eq!(
        report.refused, 4,
        "every planted row is refused at the authority: {report:?}"
    );
    for (name, seat) in [("alice", &alice), ("carol", &carol)] {
        assert_eq!(
            honoured_files(seat, &run.scope_id).await,
            want,
            "{name}: Alice's re-written paths fold as her G1 rows, the path she wrote only \
             under G0 as that row (gathered at G0's item key once the tip is G1), the fresh \
             path as no row, Bob's pre-removal file stands"
        );
    }
}

/// One own write on `seat`'s plane.
async fn put_as(
    seat: &Seat,
    root: &fauna_core::crypto::GroupMachineryRoot,
    scope_id: &[u8; 32],
    (item, value, stamp): (ItemId, Vec<u8>, Vec<u8>),
) {
    let sk = seat.device();
    GroupStatePlane::new(&seat.store, &NoFeed, root, &sk, scope_id)
        .unwrap()
        .put(&item, value, Some(stamp))
        .await
        .expect("the write");
}

/// `reader` walks `served`'s relay rows.
async fn walk_as(
    reader: &Seat,
    root: &fauna_core::crypto::GroupMachineryRoot,
    scope_id: &[u8; 32],
    served: &Seat,
) -> fauna_sync_engine::account_state_plane::WalkReport {
    let feed = feed_of(served, &GroupScope::new(*scope_id).to_string()).await;
    let sk = reader.device();
    GroupStatePlane::new(&reader.store, &feed, root, &sk, scope_id)
        .unwrap()
        .walk()
        .await
        .expect("the walk")
}

/// A seat's honoured files: cell key → (author, stamp).
async fn honoured_files(
    seat: &Seat,
    scope_id: &[u8; 32],
) -> std::collections::BTreeMap<String, (fauna_core::identity::ActorId, i64)> {
    let rows = seat
        .store
        .group_scope_states(&GroupScope::new(*scope_id).to_string())
        .await
        .unwrap();
    fold_group_files(scope_id, &rows)
        .expect("the scope reads")
        .files
        .into_iter()
        .map(|(key, f)| (key, (f.member, f.record.core.stamped_at_ms)))
        .collect()
}
