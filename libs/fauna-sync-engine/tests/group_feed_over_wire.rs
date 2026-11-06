//! The group plane's feed over a REAL peer channel — the share serve set
//! answers a group scope's `fauna.sync.changes.list` and the group walk pages
//! it, member to member, with no nest anywhere
//! (`docs/goal/behavior/p2p-shared-set-build.md` § Build design — the group
//! set, decision 6).
//!
//! Two real `fauna-account-store` replicas of two different accounts go
//! through the offline share ceremony (the shared `group_seats` fixtures);
//! each then serves its store through a `ShareServer` bound on an in-memory
//! transport (`fauna_transport::testing::MemTransport`), its group feed a
//! serve handle over its own store and its witness door the production
//! evaluator (`LiveGroupRoster`) read from the same store. The other member
//! dials, presents its carried roster entry, and runs `GroupStatePlane::walk`
//! with the channel as the plane's `rpc` — both directions. The refusals
//! around it are asserted over the same wire: the account's own planes never
//! cross the share channel, and an entry the authority removes is refused at
//! the next request on the connection that is already open.

#![cfg(feature = "p2p-share")]

mod group_seats;
use group_seats::*;

use std::sync::Arc;

use fauna_account_store::sqlite::SqliteBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::WriterId;
use fauna_core::encoding::{canonical_decode, canonical_encode};
use fauna_core::group_generation::GroupHeldRootRecord;
use fauna_core::group_scope::{GroupRosterRecord, sign_roster_removal};
use fauna_core::identity::ActorId;
use fauna_peer_channel::{PeerChannel, PeerNode};
use fauna_peer_share::SetMembership;
use fauna_peer_share::admit_group_over;
use fauna_peer_share::provenance::LocalShareChange;
use fauna_peer_share::server::{ShareServer, ShareServerConfig, ShareStore};
use fauna_peer_sync::PeerRequester;
use fauna_peer_sync::quota::QuotaConfig;
use fauna_peer_sync::store_serve::ServeStoreHandle;
use fauna_protocol::RpcRequester;
use fauna_protocol::account_state::{ACCOUNT_STATE_SCOPE, ItemClass};
use fauna_protocol::group_state::KIND_GROUP_ROSTER;
use fauna_protocol::scope::GroupScope;
use fauna_protocol::sync::{SyncChangesListReply, SyncChangesListRequest};
use fauna_sync_engine::account_state_plane::ItemId;
use fauna_sync_engine::group_roster_door::LiveGroupRoster;
use fauna_sync_engine::group_scope_view::GroupRosterSnapshot;
use fauna_sync_engine::group_set::{fold_group_files, fold_group_name};
use fauna_sync_engine::group_state_plane::{GroupStatePlane, NoFeed};
use fauna_transport::testing::{Listeners, MemTransport, await_listening};
use fauna_transport::{EndpointKey, PathCandidates, PeerTransport};

/// No folder set on either side: the group family is the only one in play.
struct NoSets;
impl SetMembership for NoSets {
    fn is_member(&self, _channel_id: &[u8; 32], _actor: &ActorId) -> bool {
        false
    }
}

struct NoBytes;

#[async_trait::async_trait]
impl ShareStore for NoBytes {
    async fn changes_since(
        &self,
        _set: &[u8; 32],
        _since: i64,
        _max_rows: u32,
    ) -> anyhow::Result<Vec<LocalShareChange>> {
        Ok(Vec::new())
    }
    async fn manifest_bytes(
        &self,
        _set: &[u8; 32],
        _manifest_hash: &[u8; 32],
    ) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    async fn chunk_body(
        &self,
        _set: &[u8; 32],
        _store_key: &[u8; 32],
    ) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(None)
    }
}

/// One seat's serving side: the share server bound on the in-memory
/// transport, its group feed a second connection to the seat's own store dir
/// (the store's multi-connection posture — the peer leg's serve shape), and
/// its witness door the production evaluator, refreshed from the store.
struct Serving {
    roster: Arc<LiveGroupRoster>,
    _node: PeerNode,
}

impl Serving {
    async fn start(seat: &Seat, listeners: &Listeners) -> Serving {
        let actor_hex = hex::encode(seat.actor.actor_id().0);
        let serve_store = AccountStore::open(
            SqliteBackend::open(seat.dir.path()).unwrap(),
            &actor_hex,
            WriterId(seat.device().verifying_key().to_bytes()),
        )
        .await
        .unwrap();
        let server = Arc::new(ShareServer::new(
            ShareServerConfig {
                display_name: String::new(),
                own_actor: seat.actor.actor_id().0,
                quotas: QuotaConfig::default(),
                now: Arc::new(|| now().0),
            },
            Arc::new(NoSets),
            Arc::new(NoBytes),
        ));
        let roster = Arc::new(LiveGroupRoster::default());
        server.set_group_roster(Arc::clone(&roster) as _);
        server.set_group_feed(ServeStoreHandle::spawn(serve_store));
        let serving = Serving {
            roster,
            _node: PeerNode::start_with(
                Arc::new(MemTransport {
                    me: EndpointKey::from_bytes(seat.actor.actor_id().0),
                    listeners: Arc::clone(listeners),
                }),
                server.handler_factory(),
            )
            .await,
        };
        await_listening(listeners, &seat.actor.actor_id().0).await;
        serving.refresh(seat).await;
        serving
    }

    /// The pump's per-pass evaluator refresh, from the seat's own store.
    async fn refresh(&self, seat: &Seat) {
        self.roster
            .replace(GroupRosterSnapshot::load_held(&seat.store).await.unwrap());
    }
}

async fn dial(listeners: &Listeners, from: &Seat, to: &Seat) -> Arc<PeerChannel> {
    let conn = MemTransport {
        me: EndpointKey::from_bytes(from.actor.actor_id().0),
        listeners: Arc::clone(listeners),
    }
    .dial(
        EndpointKey::from_bytes(to.actor.actor_id().0),
        PathCandidates::default(),
    )
    .await
    .expect("dial");
    Arc::new(PeerChannel::open(conn).await.expect("channel"))
}

/// `seat`'s own `Enrolled` roster record in the ceremony's snapshot — the
/// carried witness it presents.
fn witness_of(run: &CeremonyRun, seat: &Seat) -> Vec<u8> {
    run.plane_rows
        .iter()
        .filter(|r| r.kind == KIND_GROUP_ROSTER)
        .find(|r| {
            matches!(
                canonical_decode::<GroupRosterRecord>(&r.value),
                Ok(GroupRosterRecord::Enrolled { ref core, .. })
                    if core.member_actor == seat.actor.actor_id()
            )
        })
        .expect("the seat's roster entry")
        .value
        .clone()
}

/// Write one value through `seat`'s own group plane (no feed — local only),
/// under the machinery root its own held-root row carries.
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

#[tokio::test(flavor = "multi_thread")]
async fn the_group_walk_converges_both_ways_over_the_share_channel() {
    let (alice, bob, run) = admitted_pair().await;
    let scope_str = GroupScope::new(run.scope_id).to_string();
    let listeners = fauna_transport::testing::listeners();

    // Alice — the authority — names the set and writes a file; Bob writes one.
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

    let alice_serving = Serving::start(&alice, &listeners).await;
    let _bob_serving = Serving::start(&bob, &listeners).await;

    // ── Bob pulls from Alice: admitted on his carried entry, then the walk
    // pages Alice's feed through the channel.
    let to_alice = dial(&listeners, &bob, &alice).await;
    let admitted = admit_group_over(
        Arc::clone(&to_alice),
        &[(run.scope_id, witness_of(&run, &bob))],
    )
    .await
    .expect("alice admits bob's entry");
    assert_eq!(admitted, vec![run.scope_id]);
    let bob_root = run.bob_root_row.machinery_root().unwrap();
    let bob_sk = bob.device();
    let to_alice_rpc = PeerRequester::new(Arc::clone(&to_alice));
    let report = GroupStatePlane::new(&bob.store, &to_alice_rpc, &bob_root, &bob_sk, &run.scope_id)
        .unwrap()
        .walk()
        .await
        .expect("bob walks alice's feed");
    assert_eq!(
        (report.applied, report.refused, report.unopened),
        (2, 0, 0),
        "the name and alice's file: {report:?}"
    );

    // ── Alice pulls from Bob, the other way round on its own connection.
    let to_bob = dial(&listeners, &alice, &bob).await;
    admit_group_over(
        Arc::clone(&to_bob),
        &[(run.scope_id, witness_of(&run, &alice))],
    )
    .await
    .expect("bob admits alice's entry");
    let alice_root = run.alice_root_row.machinery_root().unwrap();
    let alice_sk = alice.device();
    let to_bob_rpc = PeerRequester::new(Arc::clone(&to_bob));
    let report = GroupStatePlane::new(
        &alice.store,
        &to_bob_rpc,
        &alice_root,
        &alice_sk,
        &run.scope_id,
    )
    .unwrap()
    .walk()
    .await
    .expect("alice walks bob's feed");
    assert_eq!(
        (report.applied, report.refused, report.unopened),
        (1, 0, 0),
        "bob's file: {report:?}"
    );

    // Both replicas now fold the same two files and hold the same name.
    for (who, seat) in [("alice", &alice), ("bob", &bob)] {
        let rows = seat.store.group_scope_states(&scope_str).await.unwrap();
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

    // ── The account's own planes never cross the share channel: asked on an
    // admitted connection, the feed refuses loudly (never an empty page).
    for scope in [None, Some(ACCOUNT_STATE_SCOPE.to_string())] {
        let refused = to_alice_rpc
            .request::<_, SyncChangesListReply>(
                "fauna.sync.changes.list",
                SyncChangesListRequest {
                    item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
                    scope: scope.clone(),
                    frontier: Some(Default::default()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("an account scope is refused on the share channel");
        assert!(
            format!("{refused:#}").contains("fauna.peer.share.unsupported"),
            "{scope:?}: {refused:#}"
        );
    }

    // ── The authority removes Bob. Once the removal reaches Alice's evaluator
    // (the pump's per-pass refresh), Bob's NEXT request on the connection that
    // is already open is refused — the verdict cached at admit is not enough.
    let (cell, removed) = sign_roster_removal(
        &alice.device(),
        run.bob_entry_id,
        alice.device_cert(),
        1_700_000_200_000,
    );
    put(
        &alice,
        &run.alice_root_row,
        &run,
        &ItemId {
            kind: KIND_GROUP_ROSTER.into(),
            key: cell,
        },
        canonical_encode(&removed).unwrap().to_vec(),
        None,
    )
    .await;
    alice_serving.refresh(&alice).await;
    GroupStatePlane::new(&bob.store, &to_alice_rpc, &bob_root, &bob_sk, &run.scope_id)
        .unwrap()
        .reconcile()
        .await
        .expect_err("the removed entry's walk is refused at its next request");
    // The walk's own error names the feed only; the refusal's code, read off
    // the same open connection, is the admission one.
    let severed = to_alice_rpc
        .request::<_, SyncChangesListReply>(
            "fauna.sync.changes.list",
            SyncChangesListRequest {
                item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
                scope: Some(scope_str.clone()),
                frontier: Some(Default::default()),
                ..Default::default()
            },
        )
        .await
        .expect_err("the removed entry is refused");
    assert!(
        format!("{severed:#}").contains("fauna.peer.share.not_admitted"),
        "{severed:#}"
    );
}
