//! Shared fixture for the `fauna-peer-share` channel integration tests.
//! `share_leg_over_the_channel.rs` and `group_ceremony_over_the_channel.rs`
//! each hand-rolled this exact `Serving` struct + its `dial` method before
//! this lift (round 72 of the shared-Rust lift sweep) — each file's own
//! `serving()` constructor genuinely differs (roster/store/claimed-sets vs an
//! optional ceremony seam) and stays local.

use std::sync::Arc;

use fauna_peer_channel::{PeerChannel, PeerNode};
use fauna_transport::testing::MemTransport;
use fauna_transport::{EndpointKey, PathCandidates, PeerTransport};

/// One serving node bound to the in-memory transport. The listener is brought
/// up with `PeerNode::start_with` directly, because the crate deliberately
/// ships no bind door (the rule-7 capability brake lands with the nest legs —
/// `fauna_peer_share::server`'s module doc); a test binding its own listener
/// is not that door.
pub struct Serving {
    pub key: [u8; 32],
    pub transport: Arc<MemTransport>,
    pub _node: PeerNode,
}

impl Serving {
    pub async fn dial(&self, other: &Serving) -> Arc<PeerChannel> {
        let conn = self
            .transport
            .dial(
                EndpointKey::from_bytes(other.key),
                PathCandidates::default(),
            )
            .await
            .expect("dial over the in-memory network");
        Arc::new(PeerChannel::open(conn).await.expect("channel"))
    }
}
