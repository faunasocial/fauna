//! The two-seat fixtures' wire half: a seat's production share seat on the
//! in-memory transport, and the seat's store as the group leg reaches it —
//! shared by the group leg's tier_1 files (`group_pump_over_wire.rs`,
//! `group_set_over_wire.rs`).

use std::sync::{Arc, Mutex};

use fauna_account_plane::group_pump::{ChannelRequester, GroupFeedChannel};
use fauna_account_store::types::{StateEntry, WriterId};
use fauna_client_capabilities::group_ceremony_node::{CeremonyBindVerdict, CeremonyNode};
use fauna_core::data::Timestamp;
use fauna_core::group_ceremony::GroupShareConfig;
use fauna_core::group_generation::GroupHeldRootRecord;
use fauna_core::identity::ActorId;
use fauna_peer_share::SetMembership;
use fauna_protocol::scope::GroupScope;
use fauna_sync_engine::account_runtime::ServeStoreFacts;
use fauna_sync_engine::account_state_plane::WalkReport;
use fauna_sync_engine::group_pump::GroupWalkStore;
use fauna_sync_engine::group_roster_door::LiveGroupRoster;
use fauna_sync_engine::group_scope_view::GroupRosterSnapshot;
use fauna_sync_engine::group_set::{GroupContentKeys, GroupFileDraft};
use fauna_sync_engine::group_state_plane::GroupStatePlane;
use fauna_sync_engine::peer_leg::open_serve_store;
use fauna_transport::EndpointKey;
use fauna_transport::testing::{Listeners, MemTransport, await_listening};

use super::Seat;

/// No folder set on either side: the group family is the only one in play.
pub struct NoSets;
impl SetMembership for NoSets {
    fn is_member(&self, _channel_id: &[u8; 32], _actor: &ActorId) -> bool {
        false
    }
}

/// One seat's production share seat on the in-memory transport: its group
/// feed a second connection to its own store dir (the production opener,
/// `peer_leg::open_serve_store`), its witness door the production evaluator
/// read from the same store.
pub async fn bind_seat(seat: &Seat, listeners: &Listeners) -> CeremonyNode {
    let node = bind_seat_without_feed(seat, listeners).await;
    node.set_group_feed(open_serve_store(serve_facts(seat)).await.unwrap());
    node
}

/// What the account runtime seats on its handle for `seat`'s store
/// (`AccountStoreHandle::serve_store_facts`) — the test store is unsealed.
pub fn serve_facts(seat: &Seat) -> ServeStoreFacts {
    ServeStoreFacts {
        store_dir: seat.dir.path().to_path_buf(),
        seal_key: None,
        actor_id_hex: hex::encode(seat.actor.actor_id().0),
        writer: WriterId(seat.device().verifying_key().to_bytes()),
    }
}

/// [`bind_seat`] before the feed is wired: what every production seat was
/// until the share plane's driver wired one — it answers a member's group
/// pull `unsupported`.
pub async fn bind_seat_without_feed(seat: &Seat, listeners: &Listeners) -> CeremonyNode {
    let node = CeremonyNode::bind_with_share_plane(
        CeremonyBindVerdict::Bind,
        Arc::new(MemTransport {
            me: EndpointKey::from_bytes(seat.actor.actor_id().0),
            listeners: Arc::clone(listeners),
        }),
        seat.actor.actor_id(),
        String::new(),
        Arc::new(Mutex::new(GroupShareConfig::default())),
        Arc::new(|| Timestamp(1_700_000_000)),
        Arc::new(|| {}),
        Arc::new(NoSets),
    )
    .await
    .expect("the seat binds");
    let roster = Arc::new(LiveGroupRoster::default());
    roster.replace(GroupRosterSnapshot::load_held(&seat.store).await.unwrap());
    node.set_group_roster(roster);
    await_listening(listeners, &seat.actor.actor_id().0).await;
    node
}

/// The seat's store as the group leg reaches it — the account runtime's
/// doors (`walk_group_scope`, `group_scope_states`, `group_set_keys`,
/// `put_group_file`) over the test's store, signing as the seat's device
/// with its own certificate.
pub struct SeatStore<'a> {
    pub seat: &'a Seat,
    pub root_row: &'a GroupHeldRootRecord,
}

impl GroupWalkStore for SeatStore<'_> {
    async fn walk_group_scope(
        &self,
        scope_id: [u8; 32],
        channel: Arc<dyn GroupFeedChannel>,
    ) -> anyhow::Result<WalkReport> {
        let root = self.root_row.machinery_root().unwrap();
        let sk = self.seat.device();
        let rpc = ChannelRequester(channel);
        GroupStatePlane::new(&self.seat.store, &rpc, &root, &sk, &scope_id)?
            .walk()
            .await
    }

    async fn group_scope_states(&self, scope_id: [u8; 32]) -> anyhow::Result<Vec<StateEntry>> {
        self.seat
            .store
            .group_scope_states(&GroupScope::new(scope_id).to_string())
            .await
    }

    async fn group_set_keys(&self, scope_id: [u8; 32]) -> anyhow::Result<GroupContentKeys> {
        fauna_sync_engine::group_set::group_set_keys(&self.seat.store, &scope_id).await
    }

    async fn put_group_file(
        &self,
        scope_id: [u8; 32],
        draft: GroupFileDraft,
    ) -> anyhow::Result<u64> {
        let root = self.root_row.machinery_root().unwrap();
        fauna_sync_engine::group_set::put_group_file(
            &self.seat.store,
            &root,
            &self.seat.device(),
            &scope_id,
            self.seat.device_cert(),
            &draft,
        )
        .await
    }
}
