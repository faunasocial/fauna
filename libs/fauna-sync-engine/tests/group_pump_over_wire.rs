//! The group pump leg over a REAL peer channel — two member seats pull one
//! group scope from each other through the share plane's own per-scope pull
//! (`fauna_sync_engine::group_pump::pull_group_scope`), with no nest anywhere
//! (`docs/goal/behavior/p2p-shared-set-build.md` § Build design — the group
//! set, decisions 6 and 8).
//!
//! Two real `fauna-account-store` replicas of two different accounts go
//! through the offline share ceremony (the shared `group_seats` fixtures),
//! the deliver carrying the initiator's endpoints cell — decision 8's
//! bootstrap row, which both sides adopt rather than refuse. Each seat then
//! binds the production `CeremonyNode` on an in-memory transport
//! (`fauna_transport::testing::MemTransport`) with its group feed and
//! witness door wired. The recipient's dial target is the bootstrap row; the
//! initiator's first dial of the recipient is the compare code's candidates,
//! kept in its ceremony record and read back through the production reader,
//! after which the recipient's own cell has reached it and the scope's cells
//! alone name both members — the candidates no longer dialed.

#![cfg(all(feature = "p2p-share", feature = "account-runtime"))]

mod group_seats;
use group_seats::wire::{SeatStore, bind_seat, bind_seat_without_feed, serve_facts};
use group_seats::*;

use fauna_account_plane::group_pump::pump_scope;
use fauna_account_plane::group_share_rows::read_group_shares;
use fauna_core::device_endpoints::DeviceEndpointsEntry;
use fauna_core::group_ceremony::GroupShareConfig;
use fauna_core::group_endpoints::{GroupEndpointsCore, GroupEndpointsRecord};
use fauna_core::group_generation::GroupHeldRootRecord;
use fauna_protocol::group_state::{KIND_GROUP_ENDPOINTS, KIND_GROUP_ROSTER};
use fauna_protocol::merge_policy::{KIND_GROUP_SHARE_CEREMONY, LwwStamp};
use fauna_protocol::scope::GroupScope;
use fauna_sync_engine::account_state_plane::ItemId;
use fauna_sync_engine::group_pump::{group_dial_targets, pull_group_scope};
use fauna_sync_engine::group_set::{fold_group_files, fold_group_name};
use fauna_sync_engine::group_state_plane::{GroupStatePlane, NoFeed};
use fauna_sync_engine::share_glue::GroupFeedWiring;
use std::sync::Arc;

async fn put(
    seat: &Seat,
    root_row: &GroupHeldRootRecord,
    run: &CeremonyRun,
    item: &ItemId,
    value: Vec<u8>,
    stamp: Option<Vec<u8>>,
) {
    let root = root_row.machinery_root().unwrap();
    let sk = seat.device();
    GroupStatePlane::new(&seat.store, &NoFeed, &root, &sk, &run.scope_id)
        .unwrap()
        .put(item, value, stamp)
        .await
        .expect("the plane write");
}

fn entry_for(seat: &Seat, lan: &str) -> DeviceEndpointsEntry {
    DeviceEndpointsEntry {
        node_id: seat.actor.actor_id().0,
        lan_addrs: vec![lan.into()],
        ..Default::default()
    }
}

/// `seat`'s own endpoints cell as its device writes it — the production
/// builder, signed under the seat's carried cert.
fn own_cell(
    run: &CeremonyRun,
    seat: &Seat,
    entry: &DeviceEndpointsEntry,
    at_ms: i64,
) -> (ItemId, Vec<u8>, Vec<u8>) {
    fauna_account_plane::group_pump::own_endpoints_cell(
        &run.scope_id,
        &seat.device(),
        seat.device_cert(),
        entry,
        at_ms,
    )
    .unwrap()
}

/// A cell `seat`'s device signs — honestly carried, but with `entry` and
/// the merge stamp of the forger's choosing: the finding's two forgeries.
fn forged_cell(
    run: &CeremonyRun,
    seat: &Seat,
    entry: DeviceEndpointsEntry,
    key: &str,
    stamp: LwwStamp,
) -> (ItemId, Vec<u8>, Vec<u8>) {
    let record = GroupEndpointsRecord::sign(
        GroupEndpointsCore {
            scope_id: run.scope_id,
            entry,
            stamped_at_ms: stamp.at_ms,
        },
        &seat.device(),
        seat.device_cert(),
    )
    .unwrap();
    (
        ItemId {
            kind: KIND_GROUP_ENDPOINTS.into(),
            key: key.into(),
        },
        fauna_core::encoding::canonical_encode(&record)
            .unwrap()
            .to_vec(),
        stamp.encode().unwrap(),
    )
}

/// Enrol a third member, `carol`, in the scope on `seat`'s replica: her
/// roster row, authority-signed by Alice's device as the ceremony signs
/// them, so a reader there holds her as a live member with a witness.
async fn enrol_third_member(
    run: &CeremonyRun,
    alice: &Seat,
    seat: &Seat,
    carol: &fauna_core::identity::ActorKeypair,
) {
    let reception = fauna_core::group_generation::GroupReceptionKeyRecord::mint(now_ms());
    let alice_device = alice.device();
    let (entry_id, record) = fauna_core::group_scope::sign_roster_enrollment(
        &alice_device,
        fauna_core::group_scope::RosterEntryCore {
            scope_id: run.scope_id,
            member_actor: carol.actor_id(),
            admission_salt: [0x3c; 32],
        },
        reception.reception_pubkey().unwrap(),
        alice.device_cert(),
        now_ms(),
    )
    .unwrap();
    let item = ItemId {
        kind: KIND_GROUP_ROSTER.into(),
        key: fauna_core::group_scope::roster_cell_key(
            &entry_id,
            &alice_device.verifying_key().to_bytes(),
        ),
    };
    let value = fauna_core::encoding::canonical_encode(&record)
        .unwrap()
        .to_vec();
    let root_row = if std::ptr::eq(seat, alice) {
        &run.alice_root_row
    } else {
        &run.bob_root_row
    };
    put(seat, root_row, run, &item, value, None).await;
}

/// Write `cfg`'s ceremony records to `seat`'s fleet plane, one row each, as
/// the seat's ceremony persistence does.
async fn persist_ceremony(seat: &Seat, cfg: &GroupShareConfig) {
    for (key, record) in cfg.rows() {
        let item = ItemId {
            kind: KIND_GROUP_SHARE_CEREMONY.into(),
            key,
        };
        seat.fleet_put(&item, record.encode().unwrap()).await;
    }
}

/// `seat`'s ceremony fold, read through the production reader.
async fn ceremonies(seat: &Seat) -> GroupShareConfig {
    read_group_shares(&seat.store).await.unwrap()
}

async fn scope_rows(seat: &Seat, run: &CeremonyRun) -> Vec<fauna_account_store::types::StateEntry> {
    seat.store
        .group_scope_states(&GroupScope::new(run.scope_id).to_string())
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pump_converges_two_members_both_ways_from_the_bootstrap_row() {
    // The deliver carries Alice's endpoints cell; the fixture asserts every
    // snapshot row — that stamped row included — adopts on both sides.
    let (alice, bob, run) = admitted_pair_with(Some(DeviceEndpointsEntry::default())).await;
    let alice_device_hex = fauna_core::hex32::encode(&alice.device().verifying_key().to_bytes());
    let bob_rows = scope_rows(&bob, &run).await;
    let bootstrap = bob_rows
        .iter()
        .find(|r| r.kind == KIND_GROUP_ENDPOINTS)
        .expect("the bootstrap row landed on the recipient");
    assert_eq!(
        bootstrap.key, alice_device_hex,
        "keyed by the stamping device"
    );

    // Content on both sides: Alice names the set and writes a file; Bob
    // writes a file and his own endpoints cell, as his seat does at bind.
    let (item, value, stamp) = name_row(&run, &alice, "Holiday photos", 1_700_000_100_000);
    put(&alice, &run.alice_root_row, &run, &item, value, Some(stamp)).await;
    let (item, value, stamp) = file_row(&run, &alice, None, 0x22, 1_700_000_100_001);
    put(&alice, &run.alice_root_row, &run, &item, value, Some(stamp)).await;
    let (item, value, stamp) = file_row(
        &run,
        &bob,
        Some(entry_core_of(&run, &bob)),
        0x11,
        1_700_000_100_002,
    );
    put(&bob, &run.bob_root_row, &run, &item, value, Some(stamp)).await;
    let bob_entry = entry_for(&bob, "10.0.0.2:7000");
    let bob_device = bob.device().verifying_key().to_bytes();
    let (item, value, stamp) = own_cell(&run, &bob, &bob_entry, 1_700_000_100_003);
    put(&bob, &run.bob_root_row, &run, &item, value, Some(stamp)).await;
    assert!(
        !fauna_account_plane::group_pump::own_endpoints_due(
            &scope_rows(&bob, &run).await,
            &fauna_core::hex32::encode(&bob_device),
            &bob_entry
        ),
        "an unchanged cell is not rewritten"
    );

    let listeners = fauna_transport::testing::listeners();
    let alice_node = bind_seat(&alice, &listeners).await;
    let bob_node = bind_seat(&bob, &listeners).await;

    // ── Bob pulls from Alice: his only dial target is the bootstrap row.
    let bob_scope = pump_scope(
        run.scope_id,
        &scope_rows(&bob, &run).await,
        &bob.actor.actor_id(),
        &ceremonies(&bob).await,
    )
    .expect("bob is a live member");
    let targets = group_dial_targets(&bob_scope, &[], None);
    assert_eq!(
        targets.iter().map(|t| t.node_id).collect::<Vec<_>>(),
        vec![alice.actor.actor_id().0],
        "the bootstrap row names the initiator"
    );
    let outcomes = pull_group_scope(
        &bob_node,
        &SeatStore {
            seat: &bob,
            root_row: &run.bob_root_row,
        },
        run.scope_id,
        &bob_scope.witness,
        &targets,
        None,
    )
    .await;
    assert_eq!(outcomes.len(), 1);
    let report = outcomes[0]
        .result
        .as_ref()
        .expect("bob pulls alice's scope");
    assert_eq!(report.refused, 0, "{report:?}");

    // ── Alice pulls from Bob. Before any pull from him she holds no cell of
    // his: her first dial target is the compare code's candidates, kept in
    // her ceremony record and read back through the production reader.
    persist_ceremony(&alice, &run.alice_cfg).await;
    assert!(
        !scope_rows(&alice, &run)
            .await
            .iter()
            .any(|r| r.kind == KIND_GROUP_ENDPOINTS && r.key != alice_device_hex),
        "no cell of bob's has reached alice yet"
    );
    let alice_scope = pump_scope(
        run.scope_id,
        &scope_rows(&alice, &run).await,
        &alice.actor.actor_id(),
        &ceremonies(&alice).await,
    )
    .expect("alice is a live member");
    assert_eq!(
        alice_scope.peers,
        vec![DeviceEndpointsEntry {
            node_id: bob.actor.actor_id().0,
            lan_addrs: vec![BOB_CODE_LAN.into()],
            ..Default::default()
        }],
        "the recorded compare-code candidates name bob"
    );
    let targets = group_dial_targets(&alice_scope, &[], None);
    let outcomes = pull_group_scope(
        &alice_node,
        &SeatStore {
            seat: &alice,
            root_row: &run.alice_root_row,
        },
        run.scope_id,
        &alice_scope.witness,
        &targets,
        None,
    )
    .await;
    assert_eq!(outcomes.len(), 1);
    let report = outcomes[0]
        .result
        .as_ref()
        .expect("alice pulls bob's scope");
    assert_eq!(report.refused, 0, "{report:?}");

    // After that pull Bob's own cell has reached Alice: the scope's cells
    // alone now name each member to the other, and the record's candidates
    // are no longer dialed although the record still holds them.
    let alice_scope = pump_scope(
        run.scope_id,
        &scope_rows(&alice, &run).await,
        &alice.actor.actor_id(),
        &ceremonies(&alice).await,
    )
    .unwrap();
    assert_eq!(
        alice_scope.peers,
        vec![bob_entry],
        "bob's own cell reached alice and replaced the compare-code candidates"
    );

    // Both replicas fold the same two files and hold the same name.
    for (who, seat) in [("alice", &alice), ("bob", &bob)] {
        let rows = scope_rows(seat, &run).await;
        let fold = fold_group_files(&run.scope_id, &rows).expect("the scope reads");
        let mut authors: Vec<_> = fold.files.values().map(|f| f.member).collect();
        authors.sort_by_key(|a| a.0);
        let mut want = vec![alice.actor.actor_id(), bob.actor.actor_id()];
        want.sort_by_key(|a| a.0);
        assert_eq!(authors, want, "{who}: both files fold");
        let named = fold_group_name(&run.scope_id, &rows).expect("the scope reads");
        assert_eq!(
            named.name.as_ref().map(|n| n.name()),
            Some("Holiday photos"),
            "{who}: the authority's name folds"
        );
    }
}

/// A member cell naming a stranger — an actor with no live entry in the
/// scope — is never a dial target: one member cannot steer the others' dials.
#[tokio::test(flavor = "multi_thread")]
async fn a_cell_naming_a_non_member_is_not_a_dial_target() {
    let (alice, bob, run) = admitted_pair().await;
    let stranger = fauna_core::identity::ActorKeypair::from_secret([0x77; 32]);
    let entry = DeviceEndpointsEntry {
        node_id: stranger.actor_id().0,
        ..Default::default()
    };
    let bob_device = bob.device().verifying_key().to_bytes();
    let (item, value, stamp) = forged_cell(
        &run,
        &bob,
        entry,
        &fauna_core::hex32::encode(&bob_device),
        LwwStamp {
            at_ms: 1_700_000_100_000,
            writer: bob_device,
        },
    );
    put(&bob, &run.bob_root_row, &run, &item, value, Some(stamp)).await;
    // Alice reads the rows as they would arrive (the same opened rows), with
    // a ceremony record naming the stranger as its recipient: its candidates
    // are not a target either, since the stranger holds no live entry.
    let rows = scope_rows(&bob, &run).await;
    let mut cfg = GroupShareConfig::default();
    cfg.initiated
        .push(fauna_core::group_ceremony::InitiatedGroupShare {
            scope_id: run.scope_id,
            recipient: stranger.actor_id(),
            recipient_lan_addrs: vec!["10.0.0.9:7000".into()],
            ..Default::default()
        });
    let scope = pump_scope(run.scope_id, &rows, &alice.actor.actor_id(), &cfg).unwrap();
    assert!(scope.peers.is_empty(), "{:?}", scope.peers);
}

/// The finding's defect: a member's cell names that member and nobody else. Bob (A)
/// signs a cell under his own key naming Alice's (B's) node with an address
/// of his choosing; Carol (C), a third live member reading the same rows,
/// dials only Alice's own cell — the bootstrap row — never the address Bob
/// chose for her.
#[tokio::test(flavor = "multi_thread")]
async fn a_members_cell_naming_a_co_member_is_not_a_dial_target() {
    let (alice, bob, run) = admitted_pair_with(Some(DeviceEndpointsEntry {
        lan_addrs: vec!["10.0.0.1:7000".into()],
        ..Default::default()
    }))
    .await;
    let carol = fauna_core::identity::ActorKeypair::from_secret([0x3c; 32]);
    enrol_third_member(&run, &alice, &bob, &carol).await;
    let bob_device = bob.device().verifying_key().to_bytes();
    let (item, value, stamp) = forged_cell(
        &run,
        &bob,
        DeviceEndpointsEntry {
            node_id: alice.actor.actor_id().0,
            lan_addrs: vec!["10.66.66.66:7000".into()],
            ..Default::default()
        },
        &fauna_core::hex32::encode(&bob_device),
        LwwStamp {
            at_ms: 1_700_000_100_000,
            writer: bob_device,
        },
    );
    put(&bob, &run.bob_root_row, &run, &item, value, Some(stamp)).await;

    let rows = scope_rows(&bob, &run).await;
    let scope = pump_scope(
        run.scope_id,
        &rows,
        &carol.actor_id(),
        &GroupShareConfig::default(),
    )
    .expect("carol is a live member");
    assert_eq!(
        scope.peers,
        vec![DeviceEndpointsEntry {
            node_id: alice.actor.actor_id().0,
            lan_addrs: vec!["10.0.0.1:7000".into()],
            ..Default::default()
        }],
        "only alice's own cell is a dial target for alice"
    );
}

/// The finding's wider forgery: a member's row under a co-member's key with
/// the co-member's device as its stamp writer and a far-future instant. The
/// co-member's walk refuses it before the merge, so its own cell is never
/// displaced on any replica it reaches.
#[tokio::test(flavor = "multi_thread")]
async fn a_members_far_future_row_at_a_co_members_key_does_not_displace_its_cell() {
    let (alice, bob, run) = admitted_pair_with(Some(DeviceEndpointsEntry {
        lan_addrs: vec!["10.0.0.1:7000".into()],
        ..Default::default()
    }))
    .await;
    let alice_device = alice.device().verifying_key().to_bytes();
    let alice_key = fauna_core::hex32::encode(&alice_device);
    let (item, value, stamp) = forged_cell(
        &run,
        &bob,
        DeviceEndpointsEntry {
            node_id: alice.actor.actor_id().0,
            lan_addrs: vec!["10.66.66.66:7000".into()],
            ..Default::default()
        },
        &alice_key,
        LwwStamp {
            at_ms: 4_000_000_000_000,
            writer: alice_device,
        },
    );
    // Bob's own plane takes his write (own writes are not checked here);
    // what matters is that nobody else's does.
    put(&bob, &run.bob_root_row, &run, &item, value, Some(stamp)).await;

    let listeners = fauna_transport::testing::listeners();
    let _bob_node = bind_seat(&bob, &listeners).await;
    let alice_node = bind_seat(&alice, &listeners).await;
    persist_ceremony(&alice, &run.alice_cfg).await;
    let alice_scope = pump_scope(
        run.scope_id,
        &scope_rows(&alice, &run).await,
        &alice.actor.actor_id(),
        &ceremonies(&alice).await,
    )
    .expect("alice is a live member");
    let targets = group_dial_targets(&alice_scope, &[], None);
    let outcomes = pull_group_scope(
        &alice_node,
        &SeatStore {
            seat: &alice,
            root_row: &run.alice_root_row,
        },
        run.scope_id,
        &alice_scope.witness,
        &targets,
        None,
    )
    .await;
    assert_eq!(outcomes.len(), 1);
    let report = outcomes[0]
        .result
        .as_ref()
        .expect("alice pulls bob's scope");
    assert_eq!(report.refused, 1, "the forged row is refused: {report:?}");

    let rows = scope_rows(&alice, &run).await;
    let own = rows
        .iter()
        .find(|r| r.kind == KIND_GROUP_ENDPOINTS && r.key == alice_key && !r.tombstone)
        .expect("alice's own cell");
    let record: GroupEndpointsRecord = fauna_core::encoding::canonical_decode(&own.value).unwrap();
    assert_eq!(
        record.core.entry.lan_addrs,
        vec!["10.0.0.1:7000".to_string()],
        "alice's own cell was not displaced by bob's far-future row"
    );
    assert_eq!(
        LwwStamp::decode(own.merge_meta.as_deref().unwrap())
            .unwrap()
            .at_ms,
        record.core.stamped_at_ms,
        "and still merges under its own signed instant"
    );
}

/// The stamp arm on its own: a member re-publishes a co-member's GENUINE
/// cell — its key, carriage, signature and node id all honest — under a
/// far-future merge stamp naming the co-member's device. Every arm of the
/// record verifies; only the stamp check (`GroupWriterCheck::
/// verify_endpoints`, the entry merges under its author's signed instant)
/// refuses it. Admitted, the replayed cell would outrank every later write
/// of the co-member's own on each replica it reached, pinning a stale
/// address.
#[tokio::test(flavor = "multi_thread")]
async fn a_co_members_genuine_cell_restamped_far_future_is_refused() {
    let (alice, bob, run) = admitted_pair_with(Some(DeviceEndpointsEntry {
        lan_addrs: vec!["10.0.0.1:7000".into()],
        ..Default::default()
    }))
    .await;
    let alice_device = alice.device().verifying_key().to_bytes();
    let alice_key = fauna_core::hex32::encode(&alice_device);
    let genuine = scope_rows(&bob, &run)
        .await
        .into_iter()
        .find(|r| r.kind == KIND_GROUP_ENDPOINTS && r.key == alice_key && !r.tombstone)
        .expect("bob holds alice's own cell");
    let signed: GroupEndpointsRecord =
        fauna_core::encoding::canonical_decode(&genuine.value).unwrap();
    // Bob's own plane takes the replay (own writes are not checked here).
    put(
        &bob,
        &run.bob_root_row,
        &run,
        &ItemId {
            kind: KIND_GROUP_ENDPOINTS.into(),
            key: alice_key.clone(),
        },
        genuine.value.clone(),
        Some(
            LwwStamp {
                at_ms: 4_000_000_000_000,
                writer: alice_device,
            }
            .encode()
            .unwrap(),
        ),
    )
    .await;

    let listeners = fauna_transport::testing::listeners();
    let _bob_node = bind_seat(&bob, &listeners).await;
    let alice_node = bind_seat(&alice, &listeners).await;
    persist_ceremony(&alice, &run.alice_cfg).await;
    let alice_scope = pump_scope(
        run.scope_id,
        &scope_rows(&alice, &run).await,
        &alice.actor.actor_id(),
        &ceremonies(&alice).await,
    )
    .expect("alice is a live member");
    let targets = group_dial_targets(&alice_scope, &[], None);
    let outcomes = pull_group_scope(
        &alice_node,
        &SeatStore {
            seat: &alice,
            root_row: &run.alice_root_row,
        },
        run.scope_id,
        &alice_scope.witness,
        &targets,
        None,
    )
    .await;
    assert_eq!(outcomes.len(), 1);
    let report = outcomes[0]
        .result
        .as_ref()
        .expect("alice pulls bob's scope");
    assert_eq!(
        report.refused, 1,
        "the re-stamped row is refused: {report:?}"
    );

    let own = scope_rows(&alice, &run)
        .await
        .into_iter()
        .find(|r| r.kind == KIND_GROUP_ENDPOINTS && r.key == alice_key && !r.tombstone)
        .expect("alice's own cell");
    assert_eq!(
        LwwStamp::decode(own.merge_meta.as_deref().unwrap())
            .unwrap()
            .at_ms,
        signed.core.stamped_at_ms,
        "alice's cell still merges under its own signed instant"
    );
}

/// The production seat's feed wiring (`share_glue::run`'s step,
/// `group-set.md` § decision 6): a seat bound without a feed answers every
/// member's group pull `fauna.peer.share.unsupported` — what every production
/// seat did before the driver wired one; [`GroupFeedWiring`] opens the feed
/// from the facts the runtime seats on its handle, after which the same seat
/// answers the member's `changes.list` and the walk lands the rows. Wired
/// once per seat and writer: a second pass opens nothing, a fresh seat (a
/// re-bind) is wired again.
#[tokio::test(flavor = "multi_thread")]
async fn a_production_seat_answers_a_members_group_pull_once_the_driver_wires_its_feed() {
    let (alice, bob, run) = admitted_pair_with(Some(DeviceEndpointsEntry::default())).await;
    let (item, value, stamp) = file_row(&run, &alice, None, 0x22, 1_700_000_100_001);
    put(&alice, &run.alice_root_row, &run, &item, value, Some(stamp)).await;

    let listeners = fauna_transport::testing::listeners();
    let alice_seat = Arc::new(bind_seat_without_feed(&alice, &listeners).await);
    let bob_node = bind_seat(&bob, &listeners).await;
    let bob_scope = pump_scope(
        run.scope_id,
        &scope_rows(&bob, &run).await,
        &bob.actor.actor_id(),
        &ceremonies(&bob).await,
    )
    .expect("bob is a live member");
    let targets = group_dial_targets(&bob_scope, &[], None);
    let bob_store = SeatStore {
        seat: &bob,
        root_row: &run.bob_root_row,
    };
    let pull = || {
        pull_group_scope(
            &bob_node,
            &bob_store,
            run.scope_id,
            &bob_scope.witness,
            &targets,
            None,
        )
    };

    // ── No feed: the seat refuses the pull outright.
    let outcomes = pull().await;
    assert_eq!(outcomes.len(), 1);
    let refused = outcomes[0]
        .result
        .as_ref()
        .expect_err("a seat with no feed answers no group pull");
    assert!(
        format!("{refused:#}").contains("fauna.peer.share.unsupported"),
        "{refused:#}"
    );

    // ── The driver's step wires it, once per seat and writer.
    let mut wiring = GroupFeedWiring::default();
    assert!(
        wiring
            .ensure(&alice_seat, &alice_seat, Some(serve_facts(&alice)), |_| {})
            .await,
        "the first pass wires the feed"
    );
    assert!(
        !wiring
            .ensure(&alice_seat, &alice_seat, Some(serve_facts(&alice)), |_| {
                panic!("nothing is opened, so nothing is registered")
            })
            .await,
        "the same seat under the same writer is not re-opened"
    );
    assert!(
        !wiring.ensure(&alice_seat, &alice_seat, None, |_| {}).await,
        "no seated facts wire nothing"
    );

    // ── The same seat now answers Bob's pull, and the walk lands Alice's file.
    let outcomes = pull().await;
    let report = outcomes[0]
        .result
        .as_ref()
        .expect("bob pulls alice's scope through the wired feed");
    assert_eq!(report.refused, 0, "{report:?}");
    let rows = scope_rows(&bob, &run).await;
    let fold = fold_group_files(&run.scope_id, &rows).expect("the scope reads");
    assert!(
        fold.files
            .values()
            .any(|f| f.member == alice.actor.actor_id()),
        "alice's file folds on bob's replica"
    );

    // ── A fresh seat (the driver's re-bind) is not the one wired: wired again.
    drop(alice_seat);
    let rebound = Arc::new(bind_seat_without_feed(&alice, &listeners).await);
    assert!(
        wiring
            .ensure(&rebound, &rebound, Some(serve_facts(&alice)), |_| {})
            .await,
        "a re-bound seat gets its own feed"
    );
}

/// The feed's second store connection is closed by the stop's release while
/// the seat that holds it lives on (`account-scoping.md` § Erasure follows
/// scope, *The stop's receipt covers the runtime's SECOND connections too*):
/// the share plane's loop, the panel and the session slot all hold the seat
/// past the runtime's shutdown, so a close that waited for the seat to drop
/// would let a sign-out's erase meet the store open — which windows refuses
/// (`os error 32`). Red without the release: the seat's node keeps the serve
/// handle, and the close never resolves.
#[tokio::test(flavor = "multi_thread")]
async fn the_stop_releases_the_group_feed_and_sees_its_connection_closed_while_the_seat_lives() {
    let (alice, _bob, _run) = admitted_pair_with(Some(DeviceEndpointsEntry::default())).await;
    let listeners = fauna_transport::testing::listeners();
    let alice_seat = Arc::new(bind_seat_without_feed(&alice, &listeners).await);

    let registered = std::sync::Mutex::new(Vec::new());
    let mut wiring = GroupFeedWiring::default();
    assert!(
        wiring
            .ensure(&alice_seat, &alice_seat, Some(serve_facts(&alice)), |c| {
                registered.lock().unwrap().push(c)
            })
            .await,
        "the feed is wired"
    );
    let mut registered = registered.into_inner().unwrap();
    assert_eq!(registered.len(), 1, "the wired feed is registered once");
    let mut feed = registered.pop().unwrap();
    assert!(!feed.is_closed(), "a wired feed holds its connection open");

    // What the shared stop does after the runtime's shutdown
    // (`fauna_client_account_runtime::await_second_connections`).
    feed.release();
    tokio::time::timeout(std::time::Duration::from_secs(3), feed.close())
        .await
        .expect("the released feed's serving connection closes while the seat still lives");
    assert!(feed.is_closed());
    // The seat outlived the close — the point of the release.
    drop(alice_seat);
}
