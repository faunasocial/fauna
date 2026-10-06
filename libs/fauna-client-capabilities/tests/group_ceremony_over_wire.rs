//! The in-memory capstone, re-run OVER THE WIRE: the whole two-party
//! offline-share ceremony — begin → offer → consent → accept (via poll) →
//! deliver → admit — with REAL state machines on both seats and every frame
//! crossing a real peer channel (`fauna_transport::testing::MemTransport`),
//! exactly the seam `p2p.md` § Offline share initiation names as slice 3's
//! transport half.
//!
//! What this adds over `fauna-peer-share`'s carriage test (scripted seam) and
//! the in-memory capstone (no transport): the glue
//! (`group_ceremony_peer::GroupCeremonyPeer`) really drives the state
//! machine from channel-proven identities, the receive-act expectation
//! really is what admits first contact, and the admission the joiner ends
//! with is the full resolver's — over bytes that crossed a wire.
//!
//! No wall-clock waits: the clock is injected, and the consent gap is driven
//! by the test (offer lands → poll answers pending → the recipient's driver
//! consents → the same poll answers the frame).

#![cfg(feature = "p2p-share")]

use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use fauna_client_capabilities::group_ceremony::{
    GroupIngestOutcome, admit_group_share, begin_group_share, build_group_accept,
    build_group_deliver, ingest_group_frame, mark_group_offer_posted,
};
use fauna_client_capabilities::group_ceremony_peer::GroupCeremonyPeer;
use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
use fauna_core::group_ceremony::GroupShareConfig;
use fauna_core::group_generation::GroupReceptionKeyRecord;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_peer_channel::{PeerChannel, PeerNode};
use fauna_peer_share::provenance::LocalShareChange;
use fauna_peer_share::server::{ShareServer, ShareServerConfig, ShareStore};
use fauna_peer_share::{
    CeremonyState, SetMembership, poll_ceremony_accept, send_ceremony_deliver, send_ceremony_offer,
};
use fauna_peer_sync::quota::QuotaConfig;
use fauna_transport::testing::{MemTransport, await_listening};
use fauna_transport::{EndpointKey, PathCandidates, PeerTransport};

fn alice() -> ActorKeypair {
    ActorKeypair::from_secret([21u8; 32])
}

fn bob() -> ActorKeypair {
    ActorKeypair::from_secret([31u8; 32])
}

fn mallory() -> ActorKeypair {
    ActorKeypair::from_secret([41u8; 32])
}

fn alices_device() -> SigningKey {
    SigningKey::from_bytes(&[0x41; 32])
}

fn alices_device_cert() -> Vec<u8> {
    let cert = DeviceAuthorization {
        actor_id: alice().actor_id(),
        device_key: alices_device().verifying_key().to_bytes(),
        capabilities: vec![Capability::RenewBearer],
        created_at: Timestamp(1_000),
        expires_at: None,
    };
    let (bytes, env) = sign_envelope(&alice(), &cert).expect("sign cert");
    canonical_encode(&EmbedAsBytes::from_signed(bytes, env))
        .expect("encode carriage")
        .to_vec()
}

/// A roster that vouches for nobody and a store that holds nothing — the
/// ceremony precedes any set membership.
struct NoSets;
impl SetMembership for NoSets {
    fn is_member(&self, _channel_id: &[u8; 32], _actor: &ActorId) -> bool {
        false
    }
}

#[derive(Default)]
struct EmptyStore;

#[async_trait::async_trait]
impl ShareStore for EmptyStore {
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

/// Bob's seat: the shared config, the glue wired into a serving node.
struct BobSeat {
    config: Arc<Mutex<GroupShareConfig>>,
    peer: Arc<GroupCeremonyPeer>,
    saves: Arc<Mutex<usize>>,
    _node: PeerNode,
}

async fn bob_seat(listeners: &fauna_transport::testing::Listeners) -> BobSeat {
    let config = Arc::new(Mutex::new(GroupShareConfig::default()));
    let saves = Arc::new(Mutex::new(0usize));
    let saves_signal = Arc::clone(&saves);
    let peer = Arc::new(GroupCeremonyPeer::new(
        bob().actor_id(),
        Arc::clone(&config),
        Arc::new(|| Timestamp(1_700_000_000)),
        Arc::new(move || *saves_signal.lock().unwrap() += 1),
    ));
    let server = Arc::new(ShareServer::new(
        ShareServerConfig {
            display_name: "bob".into(),
            own_actor: bob().actor_id().0,
            quotas: QuotaConfig::default(),
            now: Arc::new(|| 1_700_000_000),
        },
        Arc::new(NoSets),
        Arc::new(EmptyStore),
    ));
    server.set_ceremony(Arc::clone(&peer) as Arc<dyn CeremonyState>);
    let transport = Arc::new(MemTransport {
        me: EndpointKey::from_bytes(bob().actor_id().0),
        listeners: Arc::clone(listeners),
    });
    let node = PeerNode::start_with(transport, server.handler_factory()).await;
    await_listening(listeners, &bob().actor_id().0).await;
    BobSeat {
        config,
        peer,
        saves,
        _node: node,
    }
}

async fn dial_bob(
    listeners: &fauna_transport::testing::Listeners,
    me: [u8; 32],
) -> Arc<PeerChannel> {
    let transport = Arc::new(MemTransport {
        me: EndpointKey::from_bytes(me),
        listeners: Arc::clone(listeners),
    });
    let conn = transport
        .dial(
            EndpointKey::from_bytes(bob().actor_id().0),
            PathCandidates::default(),
        )
        .await
        .expect("dial");
    Arc::new(PeerChannel::open(conn).await.expect("channel"))
}

/// Seal-length check of every `fauna.state.group-share-ceremony` row
/// `record` would write — the length the writer door measures against
/// `MAX_STATE_ENTRY_BYTES`, generation-sealed as the kind is.
fn assert_every_ceremony_row_fits(side: &str, record: &GroupShareConfig) {
    use fauna_core::account_entry_crypto::{EntryPlaintext, sealed_envelope_len};
    use fauna_protocol::account_state::MAX_STATE_ENTRY_BYTES;
    use fauna_protocol::merge_policy::KIND_GROUP_SHARE_CEREMONY;
    let rows = record.rows();
    assert!(!rows.is_empty(), "{side} recorded the ceremony");
    let measured: Vec<(String, usize)> = rows
        .into_iter()
        .map(|(key, row)| {
            let plaintext = EntryPlaintext {
                kind: KIND_GROUP_SHARE_CEREMONY.to_string(),
                key: key.clone(),
                merge_meta: None,
                value: row.encode().expect("encode").into(),
                tombstone: false,
            };
            (key, sealed_envelope_len(&plaintext, true).expect("measure"))
        })
        .collect();
    eprintln!("size pin: {side}'s rows seal to {measured:?}");
    for (key, sealed) in measured {
        assert!(
            sealed <= MAX_STATE_ENTRY_BYTES / 2,
            "{side}'s row {key} seals to {sealed} B, over half the {MAX_STATE_ENTRY_BYTES} B cap"
        );
    }
}

/// The whole ceremony over the wire, plus the refusal legs around it.
#[tokio::test]
async fn the_full_offline_ceremony_admits_bob_over_a_real_channel() {
    let now = Timestamp(1_700_000_000);
    let listeners = fauna_transport::testing::listeners();
    let seat = bob_seat(&listeners).await;

    // ── First contact is gated: before Bob's receive act, Alice is refused.
    let channel = dial_bob(&listeners, alice().actor_id().0).await;
    let mut alice_cfg = GroupShareConfig::default();
    let begun = begin_group_share(&mut alice_cfg, &alice(), bob().actor_id(), now).expect("begin");
    let scope = begun.scope_id;
    send_ceremony_offer(Arc::clone(&channel), begun.frame.clone())
        .await
        .expect_err("no receive act yet — the offer must be refused");

    // ── Bob's receive act (the in-person compare) mints the expectation.
    seat.peer.expect_share_from(alice().actor_id());

    // ── Offer: crosses, and Bob's REAL state machine records it.
    send_ceremony_offer(Arc::clone(&channel), begun.frame.clone())
        .await
        .expect("the expectation admits Alice");
    mark_group_offer_posted(&mut alice_cfg, &scope, &bob().actor_id());
    {
        let cfg = seat.config.lock().unwrap();
        assert_eq!(cfg.invited.len(), 1);
        assert_eq!(cfg.invited[0].scope_id, scope);
        assert_eq!(cfg.invited[0].initiator, alice().actor_id());
    }
    assert!(
        *seat.saves.lock().unwrap() >= 1,
        "the ingest signalled the driver to persist"
    );

    // ── Consent gap: the poll answers pending until Bob's driver consents.
    assert_eq!(
        poll_ceremony_accept(Arc::clone(&channel), &scope)
            .await
            .expect("pending is not an error"),
        None
    );
    let bob_reception = GroupReceptionKeyRecord::mint(1_700_000_000_000);
    {
        let mut cfg = seat.config.lock().unwrap();
        build_group_accept(&mut cfg, &bob(), &scope, &bob_reception, now).expect("bob consents");
    }

    // ── Accept: crosses as the poll's reply; Alice's machine records it.
    let accept_frame = poll_ceremony_accept(Arc::clone(&channel), &scope)
        .await
        .expect("the accept is owed now")
        .expect("the frame is there");
    let sender = ActorId(*channel.peer_identity().as_bytes());
    let outcome = ingest_group_frame(
        &mut alice_cfg,
        &alice().actor_id(),
        &sender,
        &accept_frame,
        now,
    )
    .expect("alice ingests the accept");
    assert_eq!(
        outcome,
        GroupIngestOutcome::AcceptRecorded {
            scope_id: scope,
            recipient: bob().actor_id(),
        }
    );

    // ── Deliver: minted against the accepted reception key, crosses, and
    // Bob's machine records it.
    let alice_reception = GroupReceptionKeyRecord::mint(1_700_000_000_000);
    let built = build_group_deliver(
        &mut alice_cfg,
        &alice(),
        &alices_device(),
        alices_device_cert(),
        &alice_reception,
        &scope,
        &bob().actor_id(),
        now,
    )
    .expect("deliver builds");
    send_ceremony_deliver(Arc::clone(&channel), built.frame.clone())
        .await
        .expect("the deliver crosses");

    // ── The size pin (`config-dissolution.md` § Phases and gates → *Bounded
    // rows*): every ceremony row, on both sides, at its fullest (the deliver
    // recorded), seals under HALF the per-entry cap. A plane-arm join test
    // never sees the cap; only the writer door and this pin do.
    {
        let bobs = seat.config.lock().unwrap().clone();
        assert_every_ceremony_row_fits("alice", &alice_cfg);
        assert_every_ceremony_row_fits("bob", &bobs);
    }

    // ── Admit: Bob runs the FULL verification chain over bytes that crossed
    // the wire — re-derived scope id, in-door root commitment, authority
    // chain, resolver with keyability.
    let admitted = {
        let cfg = seat.config.lock().unwrap();
        admit_group_share(&cfg, &bob(), &bob_reception, &scope, now).expect("bob admits")
    };
    assert_eq!(admitted.held_root_row.scope_id, scope);
    assert_eq!(admitted.rows, built.plane_rows);
    assert_eq!(admitted.generation_id, built.generation_id);

    // ── The expectation was never a blanket door: Mallory is still refused,
    // mid-ceremony state and all.
    let mallory_channel = dial_bob(&listeners, mallory().actor_id().0).await;
    send_ceremony_offer(mallory_channel, b"anything".to_vec())
        .await
        .expect_err("no expectation or record names Mallory");
}

/// A dropped connection redials without re-scanning: after the offer is
/// recorded, the durable record admits Alice even with the expectation gone.
#[tokio::test]
async fn a_redial_after_the_offer_needs_no_second_receive_act() {
    let now = Timestamp(1_700_000_000);
    let listeners = fauna_transport::testing::listeners();
    let seat = bob_seat(&listeners).await;
    seat.peer.expect_share_from(alice().actor_id());

    let channel = dial_bob(&listeners, alice().actor_id().0).await;
    let mut alice_cfg = GroupShareConfig::default();
    let begun = begin_group_share(&mut alice_cfg, &alice(), bob().actor_id(), now).expect("begin");
    send_ceremony_offer(Arc::clone(&channel), begun.frame.clone())
        .await
        .expect("admitted by the expectation");
    drop(channel);

    // The receive act is withdrawn (or expired) — the record now carries it.
    seat.peer.cancel_expectation(&alice().actor_id());
    let redial = dial_bob(&listeners, alice().actor_id().0).await;
    assert_eq!(
        poll_ceremony_accept(redial, &begun.scope_id)
            .await
            .expect("the invited record admits the redial"),
        None
    );
}
