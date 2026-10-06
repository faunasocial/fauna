//! The offline share-initiation ceremony's peer-channel carriage — the
//! transport half of `fauna_core::group_ceremony` (`p2p.md` § Offline share
//! initiation, contract point 1; row 62 slice 3's last piece).
//!
//! # Every leg is initiator-originated
//!
//! The accept side of a [`fauna_peer_channel::PeerNode`] serves and never
//! originates, so the carriage keeps the whole ceremony on ONE dialed
//! channel, driven by the initiator: push the offer
//! ([`send_ceremony_offer`]), poll for the accept while the recipient's user
//! decides ([`poll_ceremony_accept`] — `None` is "consent pending", a
//! declined invitation is a terminal [`ERR_CEREMONY_REFUSED`]), then push the
//! deliver ([`send_ceremony_deliver`]). The recipient's side is purely
//! reactive state behind the [`CeremonyState`] seam.
//!
//! # Admission before parsing (wormability rule 1)
//!
//! A co-present initiation is between actors who may not be P2P contacts
//! yet, so *something* must admit the ceremony kinds before any roster
//! exists. That something is [`CeremonyState::admits`]: an actor-level gate
//! the serve side consults **before decoding any payload**, satisfied by a
//! live receive-act **expectation** (the recipient's own "receive a share"
//! act names exactly the scanned initiator — an edge both endpoints chose)
//! or an in-flight ceremony record naming the actor as its initiator. The
//! gate can only name the actor; holding a record-admitted actor to its own
//! recorded scope (and a new scope to a live expectation) is the state's
//! post-decode check inside [`CeremonyState::ingest_frame`]. This is the
//! § Inbound authorization carve-out (a stronger per-kind admission of the
//! kind's own) applied to the ceremony family; with no seam wired the
//! handlers fail closed ([`ERR_CEREMONY_NOT_EXPECTED`]).
//!
//! # The frames are opaque here
//!
//! A carriage payload is the verbatim `GroupCeremonyMessage` encoding; the
//! signatures inside cover exactly those bytes, and every verification
//! (sender binding, offer/accept/deliver chain, order) is the state
//! machine's (`fauna_client_capabilities::group_ceremony` over
//! `fauna_core::group_ceremony`) — this module never re-encodes or
//! interprets a frame, mirroring the custody carriage idiom.

use std::sync::Arc;

use anyhow::{Context, Result};
use fauna_core::identity::ActorId;
use fauna_peer_channel::PeerChannel;
use fauna_peer_sync::client::PeerRequester;
use fauna_protocol::RpcRequester;
use fauna_protocol::peer_share::{
    KIND_PEER_SHARE_CEREMONY_ACCEPT_POLL, KIND_PEER_SHARE_CEREMONY_DELIVER,
    KIND_PEER_SHARE_CEREMONY_OFFER, PeerShareCeremonyAcceptPollReply,
    PeerShareCeremonyAcceptPollRequest, PeerShareCeremonyFrameAck, PeerShareCeremonyFrameRequest,
};
use serde_bytes::ByteBuf;

/// Why the ceremony state refused a step — mapped onto the wire's two
/// `fauna.peer.share.ceremony.*` error codes by the serve handlers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CeremonyRefusal {
    /// No live expectation or in-flight ceremony record admits the actor
    /// (→ [`fauna_protocol::peer_share::ERR_CEREMONY_NOT_EXPECTED`]).
    #[error("ceremony not expected: {0}")]
    NotExpected(String),
    /// The step was refused on its content or order — a frame failing
    /// verification, a step out of order, a declined invitation
    /// (→ [`fauna_protocol::peer_share::ERR_CEREMONY_REFUSED`]).
    #[error("ceremony step refused: {0}")]
    Refused(String),
}

/// The recipient side's ceremony state, as one seam — implemented over the
/// ceremony state machine + the receive-act expectation store by the client
/// glue, never here (this crate carries no account state).
///
/// [`Self::admits`] is deliberately synchronous and I/O-free: it runs on the
/// serve path **before any payload parsing** (rule 1), so it must answer from
/// in-memory state alone — the e2e state-provider corollary applied to the
/// peer serve path.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait CeremonyState: fauna_core::MaybeSendSync {
    /// Actor-level pre-parse admission: does a live expectation or an
    /// in-flight ceremony record name this channel-proven actor?
    fn admits(&self, peer: &ActorId) -> bool;

    /// Verify + record one pushed frame (offer or deliver) from the
    /// channel-proven `sender`. Record-then-act: recording is the whole
    /// outcome; owed actions re-derive from state. The per-scope admission
    /// runs here, after decode: a frame is taken only for a live ceremony
    /// the sender initiated, or — for an offer on a scope with no record — on
    /// a live receive-act expectation.
    async fn ingest_frame(&self, sender: &ActorId, frame: &[u8]) -> Result<(), CeremonyRefusal>;

    /// The owed accept frame for the ceremony `initiator` offered `scope_id`,
    /// once this side's user has consented; `Ok(None)` while consent is
    /// pending. A declined invitation is `Err(Refused)` — terminal, so the
    /// initiator stops polling.
    async fn accept_frame(
        &self,
        initiator: &ActorId,
        scope_id: &[u8; 32],
    ) -> Result<Option<Vec<u8>>, CeremonyRefusal>;
}

/// Push the offer frame over the dialed channel. The reply is the recorded
/// ack; any refusal surfaces as the request's error.
pub async fn send_ceremony_offer(channel: Arc<PeerChannel>, frame: Vec<u8>) -> Result<()> {
    let requester = PeerRequester::new(channel);
    let _: PeerShareCeremonyFrameAck = requester
        .request(
            KIND_PEER_SHARE_CEREMONY_OFFER,
            PeerShareCeremonyFrameRequest {
                frame: ByteBuf::from(frame),
                extra: Default::default(),
            },
        )
        .await
        .context("ceremony offer push")?;
    Ok(())
}

/// One poll for the owed accept frame — `None` while the recipient's user is
/// still deciding. Pacing is the caller's (a driver polls under its own named
/// budget; this helper is single-shot so the carriage owns no timing).
pub async fn poll_ceremony_accept(
    channel: Arc<PeerChannel>,
    scope_id: &[u8; 32],
) -> Result<Option<Vec<u8>>> {
    let requester = PeerRequester::new(channel);
    let reply: PeerShareCeremonyAcceptPollReply = requester
        .request(
            KIND_PEER_SHARE_CEREMONY_ACCEPT_POLL,
            PeerShareCeremonyAcceptPollRequest {
                scope_id: ByteBuf::from(scope_id.to_vec()),
                extra: Default::default(),
            },
        )
        .await
        .context("ceremony accept poll")?;
    Ok(reply.frame.map(ByteBuf::into_vec))
}

/// Push the deliver frame over the dialed channel.
pub async fn send_ceremony_deliver(channel: Arc<PeerChannel>, frame: Vec<u8>) -> Result<()> {
    let requester = PeerRequester::new(channel);
    let _: PeerShareCeremonyFrameAck = requester
        .request(
            KIND_PEER_SHARE_CEREMONY_DELIVER,
            PeerShareCeremonyFrameRequest {
                frame: ByteBuf::from(frame),
                extra: Default::default(),
            },
        )
        .await
        .context("ceremony deliver push")?;
    Ok(())
}
