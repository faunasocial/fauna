//! The ceremony carriage driven **over a real peer channel** — the transport
//! half of the offline share initiation (`p2p.md` § Offline share initiation;
//! row 62 slice 3's last piece), against a scripted [`CeremonyState`] so the
//! carriage's own obligations are pinned without the state machine in the
//! frame (the glue's end-to-end capstone lives with the state machine).
//!
//! What it pins:
//!
//! 1. **Rule 1 at the carriage boundary** — an actor the seam does not admit
//!    is refused [`ERR_CEREMONY_NOT_EXPECTED`] and the seam records **no
//!    ingest call**: the refusal really is before any frame reached state.
//! 2. **The three legs in order** — offer push → ack (the seam saw the
//!    channel-proven sender + the verbatim frame), accept poll `None` while
//!    consent is pending then the frame once given, deliver push → ack.
//! 3. **A declined invitation is terminal** — the poll answers
//!    [`ERR_CEREMONY_REFUSED`], never `None`-forever.
//! 4. **No seam wired → fail closed** — the kinds answer
//!    [`ERR_CEREMONY_NOT_EXPECTED`], not `unknown_kind`, and never a hang.

use std::sync::{Arc, Mutex};

use fauna_core::identity::ActorId;
use fauna_peer_channel::{PeerChannel, PeerNode};
use fauna_peer_share::provenance::LocalShareChange;
use fauna_peer_share::server::{ShareServer, ShareServerConfig, ShareStore};
use fauna_peer_share::{
    CeremonyRefusal, CeremonyState, SetMembership, poll_ceremony_accept, send_ceremony_deliver,
    send_ceremony_offer,
};
use fauna_peer_sync::quota::QuotaConfig;
use fauna_protocol::peer_share::{ERR_CEREMONY_NOT_EXPECTED, ERR_CEREMONY_REFUSED};
use fauna_transport::testing::{Listeners, MemTransport, await_listening};
use fauna_transport::{EndpointKey, PathCandidates, PeerTransport};

mod common;
use common::Serving;

/// The two ceremony parties + a stranger no expectation names.
const INITIATOR: [u8; 32] = [0xA1; 32];
const RECIPIENT: [u8; 32] = [0xB2; 32];
const STRANGER: [u8; 32] = [0xCC; 32];

const SCOPE: [u8; 32] = [0x5C; 32];

// ── Fixtures ─────────────────────────────────────────────────────────────────

/// A roster that vouches for nobody — the ceremony precedes set membership.
struct NoSets;
impl SetMembership for NoSets {
    fn is_member(&self, _channel_id: &[u8; 32], _actor: &ActorId) -> bool {
        false
    }
}

/// A store that holds nothing.
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

/// A scripted ceremony seam: admits exactly `admitted`, records every ingest,
/// answers the accept poll from a settable slot.
#[derive(Default)]
struct Scripted {
    admitted: Vec<ActorId>,
    ingested: Mutex<Vec<(ActorId, Vec<u8>)>>,
    accept: Mutex<Option<Vec<u8>>>,
    declined: bool,
}

#[async_trait::async_trait]
impl CeremonyState for Scripted {
    fn admits(&self, peer: &ActorId) -> bool {
        self.admitted.contains(peer)
    }
    async fn ingest_frame(&self, sender: &ActorId, frame: &[u8]) -> Result<(), CeremonyRefusal> {
        self.ingested
            .lock()
            .unwrap()
            .push((*sender, frame.to_vec()));
        Ok(())
    }
    async fn accept_frame(
        &self,
        _initiator: &ActorId,
        _scope_id: &[u8; 32],
    ) -> Result<Option<Vec<u8>>, CeremonyRefusal> {
        if self.declined {
            return Err(CeremonyRefusal::Refused("declined".into()));
        }
        Ok(self.accept.lock().unwrap().clone())
    }
}

/// One serving node, optionally with a ceremony seam wired. As in the share
/// leg's channel test, `PeerNode::start_with` is the test's own listener —
/// not the production bind door the rule-7 brake governs.
async fn serving(
    key: [u8; 32],
    listeners: &Listeners,
    ceremony: Option<Arc<dyn CeremonyState>>,
) -> Serving {
    let server = Arc::new(ShareServer::new(
        ShareServerConfig {
            display_name: format!("node-{:02x}", key[0]),
            own_actor: key,
            quotas: QuotaConfig::default(),
            now: Arc::new(|| 1_000),
        },
        Arc::new(NoSets),
        Arc::new(EmptyStore),
    ));
    if let Some(state) = ceremony {
        server.set_ceremony(state);
    }
    let transport = Arc::new(MemTransport {
        me: EndpointKey::from_bytes(key),
        listeners: Arc::clone(listeners),
    });
    let node = PeerNode::start_with(transport.clone(), server.handler_factory()).await;
    await_listening(listeners, &key).await;
    Serving {
        key,
        transport,
        _node: node,
    }
}

/// The wire code, fished out of the anyhow chain (`ChannelError::Rpc`'s
/// display carries it).
fn chain_names(err: &anyhow::Error, code: &str) -> bool {
    err.chain().any(|c| c.to_string().contains(code))
}

// ── 1. Rule 1 at the carriage boundary ───────────────────────────────────────

#[tokio::test]
async fn an_unexpected_actor_is_refused_before_any_frame_reaches_state() {
    let listeners = fauna_transport::testing::listeners();
    let seam = Arc::new(Scripted {
        admitted: vec![ActorId(INITIATOR)],
        ..Default::default()
    });
    let recipient = serving(RECIPIENT, &listeners, Some(seam.clone())).await;
    let stranger = serving(STRANGER, &listeners, None).await;

    let channel = stranger.dial(&recipient).await;
    let err = send_ceremony_offer(Arc::clone(&channel), b"not-even-a-frame".to_vec())
        .await
        .expect_err("no expectation names the stranger");
    assert!(chain_names(&err, ERR_CEREMONY_NOT_EXPECTED), "{err:#}");
    assert!(
        seam.ingested.lock().unwrap().is_empty(),
        "the refusal must land BEFORE the seam sees any frame (rule 1)"
    );

    // The poll and the deliver refuse identically — the gate is per kind
    // family, not per leg.
    let err = poll_ceremony_accept(Arc::clone(&channel), &SCOPE)
        .await
        .expect_err("the poll is gated too");
    assert!(chain_names(&err, ERR_CEREMONY_NOT_EXPECTED), "{err:#}");
    let err = send_ceremony_deliver(channel, b"frame".to_vec())
        .await
        .expect_err("the deliver is gated too");
    assert!(chain_names(&err, ERR_CEREMONY_NOT_EXPECTED), "{err:#}");
}

// ── 2. The three legs in order ───────────────────────────────────────────────

#[tokio::test]
async fn the_three_ceremony_legs_cross_the_channel() {
    let listeners = fauna_transport::testing::listeners();
    let seam = Arc::new(Scripted {
        admitted: vec![ActorId(INITIATOR)],
        ..Default::default()
    });
    let recipient = serving(RECIPIENT, &listeners, Some(seam.clone())).await;
    let initiator = serving(INITIATOR, &listeners, None).await;

    let channel = initiator.dial(&recipient).await;

    // Offer: the seam sees the channel-proven sender and the verbatim frame.
    send_ceremony_offer(Arc::clone(&channel), b"the offer frame".to_vec())
        .await
        .expect("the expectation admits the initiator");
    {
        let seen = seam.ingested.lock().unwrap();
        assert_eq!(
            seen.as_slice(),
            &[(ActorId(INITIATOR), b"the offer frame".to_vec())]
        );
    }

    // Consent pending: the poll answers None, and polling again is fine.
    assert_eq!(
        poll_ceremony_accept(Arc::clone(&channel), &SCOPE)
            .await
            .expect("a pending poll is not an error"),
        None
    );

    // Consent given: the owed accept frame crosses.
    *seam.accept.lock().unwrap() = Some(b"the accept frame".to_vec());
    assert_eq!(
        poll_ceremony_accept(Arc::clone(&channel), &SCOPE)
            .await
            .expect("the accept is owed now"),
        Some(b"the accept frame".to_vec())
    );

    // Deliver: pushed and recorded like the offer.
    send_ceremony_deliver(channel, b"the deliver frame".to_vec())
        .await
        .expect("the deliver crosses");
    let seen = seam.ingested.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1], (ActorId(INITIATOR), b"the deliver frame".to_vec()));
}

// ── 3. A declined invitation is terminal ─────────────────────────────────────

#[tokio::test]
async fn a_declined_invitation_answers_refused_not_pending() {
    let listeners = fauna_transport::testing::listeners();
    let seam = Arc::new(Scripted {
        admitted: vec![ActorId(INITIATOR)],
        declined: true,
        ..Default::default()
    });
    let recipient = serving(RECIPIENT, &listeners, Some(seam)).await;
    let initiator = serving(INITIATOR, &listeners, None).await;

    let channel = initiator.dial(&recipient).await;
    let err = poll_ceremony_accept(channel, &SCOPE)
        .await
        .expect_err("declined is terminal, never pending");
    assert!(chain_names(&err, ERR_CEREMONY_REFUSED), "{err:#}");
}

// ── 4. No seam wired → fail closed ───────────────────────────────────────────

#[tokio::test]
async fn with_no_ceremony_state_wired_the_kinds_fail_closed() {
    let listeners = fauna_transport::testing::listeners();
    let recipient = serving(RECIPIENT, &listeners, None).await;
    let initiator = serving(INITIATOR, &listeners, None).await;

    let channel = initiator.dial(&recipient).await;
    let err = send_ceremony_offer(channel, b"frame".to_vec())
        .await
        .expect_err("no ceremony state → refused, not served");
    // Fail closed with the ceremony's own refusal — NOT `unknown_kind`: the
    // kinds are allowlisted (the hardening pins tie the table to the
    // allowlist), the state is what's absent.
    assert!(chain_names(&err, ERR_CEREMONY_NOT_EXPECTED), "{err:#}");
}

// ── 5. The ceremony's continuation: the certificate admits transfer ─────────
//
// After admit, a member holds its `Enrolled` roster entry — the group
// membership witness. This leg pins the CARRIAGE of that certificate over
// the wire: `admit_group_over` presents it on the admit exchange and the
// responder's verdict opens the group scope (the fourth witness kind,
// end-to-end over a real channel).

mod certificate_admits_transfer {
    use super::*;
    use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
    use fauna_core::group_scope::{GroupBirthRecord, RosterEntryCore, group_scope_id};
    use fauna_core::identity::ActorKeypair;
    use fauna_peer_share::{GroupRosterState, admit_group_over};

    fn authority() -> ActorKeypair {
        ActorKeypair::from_secret([0x51; 32])
    }

    fn member() -> ActorKeypair {
        ActorKeypair::from_secret([0x52; 32])
    }

    fn birth() -> GroupBirthRecord {
        GroupBirthRecord {
            authority_actor: authority().actor_id(),
            salt: [0xB1; 32],
            machinery_root_commit: fauna_core::crypto::GroupMachineryRoot::from_bytes([0xD7; 32])
                .commitment(),
            created_at_ms: 1_700_000_000_000,
        }
    }

    /// The member's own signed `Enrolled` entry, exactly as a deliver's
    /// machinery snapshot carries it.
    fn own_entry() -> Vec<u8> {
        let device = ed25519_dalek::SigningKey::from_bytes(&[0x41; 32]);
        let cert = DeviceAuthorization {
            actor_id: authority().actor_id(),
            device_key: device.verifying_key().to_bytes(),
            capabilities: vec![Capability::RenewBearer],
            created_at: Timestamp(1_000),
            expires_at: None,
        };
        let (bytes, env) = sign_envelope(&authority(), &cert).expect("sign cert");
        let core = RosterEntryCore {
            scope_id: group_scope_id(&birth()).unwrap(),
            member_actor: member().actor_id(),
            admission_salt: [0x01; 32],
        };
        let (_, record) = fauna_core::group_scope::sign_roster_enrollment(
            &device,
            core,
            vec![0xE0; 8],
            canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).unwrap(),
            2_000,
        )
        .unwrap();
        canonical_encode(&record).unwrap()
    }

    /// The serving side's own group state (it holds the scope's birth).
    struct Holds;
    impl GroupRosterState for Holds {
        fn authority(
            &self,
            scope_id: &[u8; 32],
        ) -> Option<fauna_core::group_scope::GroupAuthority> {
            Some(fauna_core::group_scope::GroupAuthority::build(
                scope_id,
                &authority().actor_id(),
                &[],
                std::iter::empty(),
            ))
        }
        fn is_entry_removed(&self, _scope_id: &[u8; 32], _entry_id: &[u8; 32]) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn the_enrolled_entry_admits_the_member_over_the_wire() {
        let listeners = fauna_transport::testing::listeners();
        let scope = group_scope_id(&birth()).unwrap();

        // The responder (another member's device, here the authority's) holds
        // the group and serves; wire its group state alongside everything else.
        let responder_key = authority().actor_id().0;
        let server = Arc::new(ShareServer::new(
            ShareServerConfig {
                display_name: "responder".into(),
                own_actor: responder_key,
                quotas: QuotaConfig::default(),
                now: Arc::new(|| 1_000),
            },
            Arc::new(NoSets),
            Arc::new(EmptyStore),
        ));
        server.set_group_roster(Arc::new(Holds));
        let transport = Arc::new(MemTransport {
            me: EndpointKey::from_bytes(responder_key),
            listeners: Arc::clone(&listeners),
        });
        let _node = PeerNode::start_with(transport, server.handler_factory()).await;
        await_listening(&listeners, &responder_key).await;

        // The member dials and presents its certificate.
        let member_transport = Arc::new(MemTransport {
            me: EndpointKey::from_bytes(member().actor_id().0),
            listeners: Arc::clone(&listeners),
        });
        let conn = member_transport
            .dial(
                EndpointKey::from_bytes(responder_key),
                PathCandidates::default(),
            )
            .await
            .expect("dial");
        let channel = Arc::new(PeerChannel::open(conn).await.expect("channel"));
        let admitted = admit_group_over(channel, &[(scope, own_entry())])
            .await
            .expect("the certificate admits");
        assert_eq!(admitted, vec![scope]);
    }
}
