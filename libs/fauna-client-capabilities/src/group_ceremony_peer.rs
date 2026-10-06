//! The ceremony state machine behind the peer-channel carriage's seam —
//! [`fauna_peer_share::CeremonyState`] implemented over
//! [`crate::group_ceremony`] + the receive-act **expectation** store
//! (`p2p.md` § Offline share initiation; row 62 slice 3, the glue).
//!
//! # The expectation is the first-contact admission (wormability rule 1)
//!
//! A co-present initiation is between actors who may not be P2P contacts
//! yet, so the recipient's own "receive a share" act — after the in-person
//! key compare — mints a short-lived, device-local [`CeremonyExpectation`]
//! naming exactly the scanned initiator. That expectation is what admits the
//! very first frame, and it is the ONLY thing that admits an offer for a
//! scope this side holds no record of. Expectations are in-memory on
//! purpose: they describe THIS device's live co-present session, never fleet
//! state, and a crash simply means re-scanning (co-present, cheap). Rule 6:
//! they expire ([`GROUP_CEREMONY_EXPECTATION_TTL_SECS`]) and are cancellable
//! ([`GroupCeremonyPeer::cancel_expectation`]).
//!
//! # The record resumes its own ceremony, and nothing else
//!
//! From the moment the offer is recorded, the durable ceremony record admits
//! its initiator, so a dropped connection redials without re-scanning — but
//! only for frames on that record's own scope, and only while the ceremony
//! is live: not declined, and either not yet delivered or delivered less
//! than [`GROUP_CEREMONY_DELIVER_RESEND_GRACE_SECS`] ago (the initiator
//! re-sends a deliver whose ack a dropped connection lost; the byte-identical
//! re-send is acked and records nothing new). The pre-decode gate can only
//! name the actor, so the per-scope half runs after decode in
//! [`CeremonyState::ingest_frame`]: a new-scope offer needs a live
//! expectation, and at most
//! [`GROUP_CEREMONY_MAX_PENDING_INVITATIONS_PER_INITIATOR`] un-consented
//! invitations per initiator are held at once. The listener carries only the
//! initiator's pushes (offer, deliver) — every leg is initiator-originated —
//! so an `initiated` record, naming an actor this account shared TO, admits
//! nobody here, and an accept frame pushed at the listener is refused.
//!
//! # Ownership
//!
//! The driver owns the record's persistence; this adapter shares the record
//! under one `Arc<Mutex<_>>` with the driver and signals every mutation
//! through `on_config_change`, so the driver persists exactly as it does for
//! its own acts (record-then-act — the record is durable state, the signal
//! is when to flush it). The clock is injected (`now`), never read from the
//! wall — e2e convention 14 applied at tier 1.

use std::sync::{Arc, Mutex};

use fauna_core::data::Timestamp;
use fauna_core::encoding::EmbedAsBytes;
use fauna_core::encoding::canonical_encode;
use fauna_core::group_ceremony::GroupShareConfig;
use fauna_core::group_ceremony::{
    GroupCeremonyMessage, InvitedGroupShare, decode_group_ceremony_message,
    encode_group_ceremony_message, verify_group_share_deliver, verify_group_share_offer,
};
use fauna_core::identity::ActorId;
use fauna_peer_share::{CeremonyRefusal, CeremonyState};

use crate::group_ceremony::{GroupCeremonyError, ingest_group_frame, mark_group_accept_posted};

/// How long a receive-act expectation admits its named initiator. A Rust
/// constant, never a knob: a co-present ceremony either happens within the
/// sitting that minted the expectation or is re-initiated by re-scanning.
pub const GROUP_CEREMONY_EXPECTATION_TTL_SECS: u64 = 15 * 60;

/// How long a recorded deliver keeps its ceremony live, so the initiator's
/// re-send of a deliver whose ack a dropped connection lost is still admitted
/// and acked. A Rust constant: it covers the initiator's whole delivery
/// budget (`group_ceremony_node::DELIVERY_BUDGET`) with room to spare, and a
/// ceremony delivered longer ago than this has nothing left to resume.
pub const GROUP_CEREMONY_DELIVER_RESEND_GRACE_SECS: u64 = GROUP_CEREMONY_EXPECTATION_TTL_SECS;

/// How many un-consented (neither accepted nor declined) invitations one
/// initiator may hold on this account at once; a further new-scope offer is
/// refused and records nothing. A Rust constant: a co-present sitting shares
/// one scope at a time, so this bounds the consent cards an expected actor
/// can flood onto every device while leaving room for a re-begun share.
pub const GROUP_CEREMONY_MAX_PENDING_INVITATIONS_PER_INITIATOR: usize = 4;

/// One live receive-act expectation: exactly one actor, until `expires_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CeremonyExpectation {
    pub initiator: ActorId,
    pub expires_at: Timestamp,
}

/// The injected clock (seconds — [`Timestamp`]'s own resolution).
pub type NowFn = Arc<dyn Fn() -> Timestamp + Send + Sync>;

/// The persistence signal: fired after every config mutation this adapter
/// makes, so the driver flushes the shared config exactly as it does for its
/// own acts.
pub type OnConfigChange = Arc<dyn Fn() + Send + Sync>;

/// The reactive (serve-side) half of the ceremony on one device — hand it to
/// `fauna_peer_share::ShareServer::set_ceremony`.
pub struct GroupCeremonyPeer {
    own_actor: ActorId,
    config: Arc<Mutex<GroupShareConfig>>,
    expectations: Mutex<Vec<CeremonyExpectation>>,
    now: NowFn,
    on_config_change: OnConfigChange,
}

impl GroupCeremonyPeer {
    pub fn new(
        own_actor: ActorId,
        config: Arc<Mutex<GroupShareConfig>>,
        now: NowFn,
        on_config_change: OnConfigChange,
    ) -> Self {
        Self {
            own_actor,
            config,
            expectations: Mutex::new(Vec::new()),
            now,
            on_config_change,
        }
    }

    /// The receive act: after the in-person key compare, admit `initiator`'s
    /// ceremony frames for the TTL. Re-expecting the same actor refreshes
    /// the window.
    pub fn expect_share_from(&self, initiator: ActorId) {
        let expires_at = Timestamp(
            (self.now)()
                .0
                .saturating_add(GROUP_CEREMONY_EXPECTATION_TTL_SECS),
        );
        let mut expectations = self.expectations.lock().unwrap();
        expectations.retain(|e| e.initiator != initiator);
        expectations.push(CeremonyExpectation {
            initiator,
            expires_at,
        });
    }

    /// Withdraw the expectation for `initiator` (rule 6 — the user changes
    /// their mind before any offer arrives).
    pub fn cancel_expectation(&self, initiator: &ActorId) {
        self.expectations
            .lock()
            .unwrap()
            .retain(|e| e.initiator != *initiator);
    }

    fn expectation_admits(&self, peer: &ActorId) -> bool {
        let now = (self.now)();
        self.expectations
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.initiator == *peer && now.0 < e.expires_at.0)
    }

    fn record_admits(&self, peer: &ActorId) -> bool {
        let now = (self.now)();
        self.config
            .lock()
            .unwrap()
            .invited
            .iter()
            .any(|r| r.initiator == *peer && ceremony_is_live(r, now))
    }
}

/// A recorded invitation still has a ceremony to resume: not declined, and
/// either not yet delivered or delivered within the re-send grace.
/// `updated_at` is stamped when the deliver is recorded (the adoption markers
/// never touch it; a later cross-device merge only moves it forward).
fn ceremony_is_live(record: &InvitedGroupShare, now: Timestamp) -> bool {
    !record.declined
        && (record.deliver.is_empty()
            || now.0
                < record
                    .updated_at
                    .0
                    .saturating_add(GROUP_CEREMONY_DELIVER_RESEND_GRACE_SECS))
}

/// The listener's per-scope admission, after decode and before the state
/// machine records anything: the pre-decode gate ([`CeremonyState::admits`])
/// can only name the actor, so this is where a record-admitted actor is held
/// to its own recorded ceremony. `ingest_group_frame`'s other callers (the
/// initiator ingesting its own polled accept) never pass through here.
fn listener_admits_frame(
    cfg: &GroupShareConfig,
    own_actor: &ActorId,
    sender: &ActorId,
    message: &GroupCeremonyMessage,
    expected: bool,
    now: Timestamp,
) -> Result<(), CeremonyRefusal> {
    let refuse = |why: &str| Err(CeremonyRefusal::Refused(why.into()));
    match message {
        GroupCeremonyMessage::Offer(envelope) => {
            let offer = verify_group_share_offer(envelope, sender, own_actor)
                .map_err(|e| CeremonyRefusal::Refused(e.to_string()))?;
            // The verified scope id is content-derived from a birth record
            // naming the sender as its authority, so a record for this scope
            // is the sender's own ceremony.
            let invited = &cfg.invited;
            match invited.iter().find(|r| r.scope_id == offer.scope_id) {
                Some(r) if !ceremony_is_live(r, now) => {
                    refuse("this scope's ceremony is no longer live")
                }
                Some(_) => Ok(()),
                None if !expected => refuse("a new share needs this side's receive act"),
                None => {
                    let pending = invited
                        .iter()
                        .filter(|r| r.initiator == *sender && !r.declined && r.accept.is_empty())
                        .count();
                    if pending >= GROUP_CEREMONY_MAX_PENDING_INVITATIONS_PER_INITIATOR {
                        refuse("too many unanswered invitations from this initiator")
                    } else {
                        Ok(())
                    }
                }
            }
        }
        GroupCeremonyMessage::Deliver(envelope) => {
            let deliver = verify_group_share_deliver(envelope, sender)
                .map_err(|e| CeremonyRefusal::Refused(e.to_string()))?;
            let Some(r) = cfg
                .invited
                .iter()
                .find(|r| r.scope_id == deliver.scope_id && r.initiator == *sender)
            else {
                return refuse("deliver names a scope this side was never offered by this sender");
            };
            if !ceremony_is_live(r, now) {
                return refuse("this scope's ceremony is no longer live");
            }
            if !r.deliver.is_empty() {
                // A lost-ack re-send is byte-identical; anything else is a
                // second deliver for a scope that already holds one.
                let bytes = canonical_encode(envelope)
                    .map_err(|e| CeremonyRefusal::Refused(e.to_string()))?;
                if bytes[..] != r.deliver[..] {
                    return refuse("this scope already holds a different deliver");
                }
            }
            Ok(())
        }
        // Every leg is initiator-originated: the initiator ingests the accept
        // from its own poll, so nothing ever pushes one at a listener.
        GroupCeremonyMessage::Accept(_) => refuse("the listener carries no accept frames"),
    }
}

fn refused(e: GroupCeremonyError) -> CeremonyRefusal {
    CeremonyRefusal::Refused(e.to_string())
}

#[async_trait::async_trait]
impl CeremonyState for GroupCeremonyPeer {
    fn admits(&self, peer: &ActorId) -> bool {
        self.expectation_admits(peer) || self.record_admits(peer)
    }

    async fn ingest_frame(&self, sender: &ActorId, frame: &[u8]) -> Result<(), CeremonyRefusal> {
        let now = (self.now)();
        let message = decode_group_ceremony_message(frame)
            .map_err(|e| CeremonyRefusal::Refused(format!("group ceremony frame refused: {e}")))?;
        let expected = self.expectation_admits(sender);
        let outcome = {
            let mut cfg = self.config.lock().unwrap();
            if let Err(refusal) =
                listener_admits_frame(&cfg, &self.own_actor, sender, &message, expected, now)
            {
                tracing::debug!(
                    record = ?Arc::as_ptr(&self.config),
                    "[offline-share] ceremony frame refused: {refusal}"
                );
                return Err(refusal);
            }
            ingest_group_frame(&mut cfg, &self.own_actor, sender, frame, now)
        };
        // Both arms name the record by address. A two-process ceremony that
        // stalls is diagnosable only if the recipient's log says which record
        // took each frame — `await_delivery` names the one it polls — and
        // nothing else on the serve path logs at all.
        match outcome {
            Ok(outcome) => tracing::debug!(
                record = ?Arc::as_ptr(&self.config),
                ?outcome,
                "[offline-share] ceremony frame recorded"
            ),
            Err(e) => {
                tracing::debug!(
                    record = ?Arc::as_ptr(&self.config),
                    "[offline-share] ceremony frame refused: {e}"
                );
                return Err(refused(e));
            }
        }
        (self.on_config_change)();
        Ok(())
    }

    async fn accept_frame(
        &self,
        initiator: &ActorId,
        scope_id: &[u8; 32],
    ) -> Result<Option<Vec<u8>>, CeremonyRefusal> {
        let frame = {
            let mut cfg = self.config.lock().unwrap();
            let Some(record) = cfg.invited.iter().find(|r| r.scope_id == *scope_id) else {
                return Err(CeremonyRefusal::Refused(
                    "no invitation for this scope".into(),
                ));
            };
            if record.initiator != *initiator {
                // The poller is not the ceremony's initiator — a live,
                // admitted actor still cannot read another ceremony's frames.
                return Err(CeremonyRefusal::Refused(
                    "this scope's ceremony belongs to a different initiator".into(),
                ));
            }
            if record.declined {
                return Err(CeremonyRefusal::Refused(
                    "the invitation was declined".into(),
                ));
            }
            if record.accept.is_empty() {
                return Ok(None); // consent pending — poll again
            }
            let envelope: EmbedAsBytes = fauna_core::encoding::canonical_decode(&record.accept)
                .map_err(|e| {
                    CeremonyRefusal::Refused(format!("recorded accept does not decode: {e}"))
                })?;
            let frame = encode_group_ceremony_message(&GroupCeremonyMessage::Accept(envelope))
                .map_err(|e| CeremonyRefusal::Refused(e.to_string()))?;
            mark_group_accept_posted(&mut cfg, scope_id);
            frame
        };
        (self.on_config_change)();
        Ok(Some(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::identity::ActorKeypair;

    fn alice() -> ActorKeypair {
        ActorKeypair::from_secret([21u8; 32])
    }

    fn bob() -> ActorKeypair {
        ActorKeypair::from_secret([31u8; 32])
    }

    fn peer_at(now: Arc<Mutex<u64>>) -> GroupCeremonyPeer {
        let cfg = Arc::new(Mutex::new(GroupShareConfig::default()));
        GroupCeremonyPeer::new(
            bob().actor_id(),
            cfg,
            Arc::new(move || Timestamp(*now.lock().unwrap())),
            Arc::new(|| {}),
        )
    }

    #[test]
    fn an_expectation_admits_exactly_its_actor_until_the_ttl() {
        let clock = Arc::new(Mutex::new(1_000u64));
        let peer = peer_at(Arc::clone(&clock));
        assert!(!peer.admits(&alice().actor_id()));

        peer.expect_share_from(alice().actor_id());
        assert!(peer.admits(&alice().actor_id()));
        // Exactly the named actor — nobody else.
        assert!(!peer.admits(&ActorKeypair::from_secret([41u8; 32]).actor_id()));

        // The TTL edge: one second before expiry admits, at expiry refuses.
        *clock.lock().unwrap() = 1_000 + GROUP_CEREMONY_EXPECTATION_TTL_SECS - 1;
        assert!(peer.admits(&alice().actor_id()));
        *clock.lock().unwrap() = 1_000 + GROUP_CEREMONY_EXPECTATION_TTL_SECS;
        assert!(!peer.admits(&alice().actor_id()));
    }

    #[test]
    fn cancelling_the_expectation_revokes_admission() {
        let peer = peer_at(Arc::new(Mutex::new(1_000)));
        peer.expect_share_from(alice().actor_id());
        assert!(peer.admits(&alice().actor_id()));
        peer.cancel_expectation(&alice().actor_id());
        assert!(!peer.admits(&alice().actor_id()));
    }

    /// After the offer is recorded, the durable record admits the initiator
    /// even with the expectation expired — a dropped connection redials
    /// without re-scanning — and declining withdraws that admission.
    #[test]
    fn a_recorded_invitation_outlives_the_expectation_until_declined() {
        let clock = Arc::new(Mutex::new(1_000u64));
        let peer = peer_at(Arc::clone(&clock));
        {
            let mut cfg = peer.config.lock().unwrap();
            cfg.invited
                .push(fauna_core::group_ceremony::InvitedGroupShare {
                    scope_id: [0x5C; 32],
                    initiator: alice().actor_id(),
                    offer: vec![1],
                    ..Default::default()
                });
        }
        assert!(peer.admits(&alice().actor_id()));

        peer.config.lock().unwrap().invited[0].declined = true;
        assert!(!peer.admits(&alice().actor_id()));
    }

    fn carol() -> ActorKeypair {
        ActorKeypair::from_secret([51u8; 32])
    }

    fn alices_device() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[0x41; 32])
    }

    fn alices_device_cert() -> Vec<u8> {
        use fauna_core::data::{Capability, DeviceAuthorization};
        use fauna_core::encoding::{EmbedAsBytes, sign_envelope};
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

    /// Alice begins a fresh share to Bob in her own store: a NEW scope and
    /// its offer frame.
    fn alice_offers(alice_cfg: &mut GroupShareConfig, now: u64) -> ([u8; 32], Vec<u8>) {
        let begun = crate::group_ceremony::begin_group_share(
            alice_cfg,
            &alice(),
            bob().actor_id(),
            Timestamp(now),
        )
        .expect("begin");
        (begun.scope_id, begun.frame)
    }

    fn invited_count(peer: &GroupCeremonyPeer) -> usize {
        peer.config.lock().unwrap().invited.len()
    }

    ///  A recorded invitation resumes ITS OWN ceremony — its
    /// offer is still taken after the expectation lapsed — but no longer
    /// admits a new scope from the same actor, which records nothing.
    #[tokio::test]
    async fn a_record_admitted_actor_cannot_open_a_new_scope_once_the_expectation_lapsed() {
        let clock = Arc::new(Mutex::new(1_000u64));
        let peer = peer_at(Arc::clone(&clock));
        let mut alice_cfg = GroupShareConfig::default();
        peer.expect_share_from(alice().actor_id());
        let (_, first) = alice_offers(&mut alice_cfg, 1_000);
        peer.ingest_frame(&alice().actor_id(), &first)
            .await
            .expect("the expectation admits the first offer");

        *clock.lock().unwrap() = 1_000 + 30 * 24 * 3600;
        assert!(
            peer.admits(&alice().actor_id()),
            "the recorded ceremony still admits its initiator's redial"
        );
        peer.ingest_frame(&alice().actor_id(), &first)
            .await
            .expect("the recorded scope's own offer is taken again, idempotently");

        let (_, second) = alice_offers(&mut alice_cfg, 1_000 + 30 * 24 * 3600);
        let refused = peer
            .ingest_frame(&alice().actor_id(), &second)
            .await
            .expect_err("a new scope needs a live receive act");
        assert!(
            matches!(refused, CeremonyRefusal::Refused(_)),
            "{refused:?}"
        );
        assert_eq!(invited_count(&peer), 1, "the refused offer records nothing");
    }

    ///  An actor this account only ever shared TO is not
    /// admitted to this account's listener — every leg is
    /// initiator-originated, so nothing of theirs ever arrives here.
    #[test]
    fn a_recipient_only_actor_is_not_admitted() {
        let peer = peer_at(Arc::new(Mutex::new(1_000)));
        crate::group_ceremony::begin_group_share(
            &mut peer.config.lock().unwrap(),
            &bob(),
            carol().actor_id(),
            Timestamp(1_000),
        )
        .expect("bob begins a share to carol");
        assert!(!peer.admits(&carol().actor_id()));
    }

    /// Even an admitted actor cannot push an accept at the listener: the
    /// initiator ingests the accept from its own poll, never from here.
    #[tokio::test]
    async fn the_listener_refuses_an_accept_frame() {
        let peer = peer_at(Arc::new(Mutex::new(1_000)));
        let now = Timestamp(1_000);
        let begun = crate::group_ceremony::begin_group_share(
            &mut peer.config.lock().unwrap(),
            &bob(),
            carol().actor_id(),
            now,
        )
        .expect("bob begins a share to carol");
        let mut carol_cfg = GroupShareConfig::default();
        ingest_group_frame(
            &mut carol_cfg,
            &carol().actor_id(),
            &bob().actor_id(),
            &begun.frame,
            now,
        )
        .expect("carol records the offer");
        let reception = fauna_core::group_generation::GroupReceptionKeyRecord::mint(1_000_000);
        let accept = crate::group_ceremony::build_group_accept(
            &mut carol_cfg,
            &carol(),
            &begun.scope_id,
            &reception,
            now,
        )
        .expect("carol accepts");

        peer.expect_share_from(carol().actor_id());
        peer.ingest_frame(&carol().actor_id(), &accept)
            .await
            .expect_err("the listener carries no accept frames");
        assert!(
            peer.config.lock().unwrap().initiated[0].accept.is_empty(),
            "nothing recorded"
        );
    }

    /// A live expectation admits new scopes, but only up to the cap of
    /// un-consented invitations per initiator.
    #[tokio::test]
    async fn an_expected_actor_holds_at_most_the_cap_of_unanswered_invitations() {
        let peer = peer_at(Arc::new(Mutex::new(1_000)));
        let mut alice_cfg = GroupShareConfig::default();
        peer.expect_share_from(alice().actor_id());
        for _ in 0..GROUP_CEREMONY_MAX_PENDING_INVITATIONS_PER_INITIATOR {
            let (_, frame) = alice_offers(&mut alice_cfg, 1_000);
            peer.ingest_frame(&alice().actor_id(), &frame)
                .await
                .expect("under the cap");
        }
        let (_, over) = alice_offers(&mut alice_cfg, 1_000);
        peer.ingest_frame(&alice().actor_id(), &over)
            .await
            .expect_err("over the cap");
        assert_eq!(
            invited_count(&peer),
            GROUP_CEREMONY_MAX_PENDING_INVITATIONS_PER_INITIATOR
        );
    }

    /// The deliver whose ack a dropped connection lost is re-sent verbatim
    /// (`GroupShareInitiator::drive`): the delivered record still admits it
    /// and acks it, recording nothing new — until the re-send grace ends.
    #[tokio::test]
    async fn a_lost_ack_deliver_resend_is_admitted_and_records_nothing_new() {
        let clock = Arc::new(Mutex::new(1_000u64));
        let peer = peer_at(Arc::clone(&clock));
        let now = Timestamp(1_000);
        let mut alice_cfg = GroupShareConfig::default();
        peer.expect_share_from(alice().actor_id());
        let (scope, offer) = alice_offers(&mut alice_cfg, 1_000);
        peer.ingest_frame(&alice().actor_id(), &offer)
            .await
            .expect("offer");
        let bob_reception = fauna_core::group_generation::GroupReceptionKeyRecord::mint(1_000_000);
        let accept = crate::group_ceremony::build_group_accept(
            &mut peer.config.lock().unwrap(),
            &bob(),
            &scope,
            &bob_reception,
            now,
        )
        .expect("bob accepts");
        ingest_group_frame(
            &mut alice_cfg,
            &alice().actor_id(),
            &bob().actor_id(),
            &accept,
            now,
        )
        .expect("alice records the accept");
        let alice_reception =
            fauna_core::group_generation::GroupReceptionKeyRecord::mint(1_000_000);
        let delivered = crate::group_ceremony::build_group_deliver(
            &mut alice_cfg,
            &alice(),
            &alices_device(),
            alices_device_cert(),
            &alice_reception,
            &scope,
            &bob().actor_id(),
            now,
        )
        .expect("alice builds the deliver");
        peer.ingest_frame(&alice().actor_id(), &delivered.frame)
            .await
            .expect("the deliver lands");
        let recorded = peer.config.lock().unwrap().invited.clone();

        // The redial pause later, with the expectation long gone.
        peer.cancel_expectation(&alice().actor_id());
        *clock.lock().unwrap() = 1_000 + 5;
        assert!(peer.admits(&alice().actor_id()));
        peer.ingest_frame(&alice().actor_id(), &delivered.frame)
            .await
            .expect("the byte-identical re-send is acked");
        assert_eq!(
            peer.config.lock().unwrap().invited,
            recorded,
            "the re-send records nothing new"
        );

        // Only the byte-identical re-send rides the grace: a deliver that
        // differs from the recorded one is a second deliver, refused.
        peer.config.lock().unwrap().invited[0].deliver = vec![0xFF];
        peer.ingest_frame(&alice().actor_id(), &delivered.frame)
            .await
            .expect_err("a deliver differing from the recorded one is refused");
        peer.config.lock().unwrap().invited = recorded;

        *clock.lock().unwrap() = 1_000 + GROUP_CEREMONY_DELIVER_RESEND_GRACE_SECS;
        assert!(
            !peer.admits(&alice().actor_id()),
            "a ceremony delivered past the grace has nothing left to resume"
        );
        peer.ingest_frame(&alice().actor_id(), &delivered.frame)
            .await
            .expect_err("past the grace even the identical re-send is refused");
    }

    /// A declined ceremony is over: even a live expectation for the same
    /// initiator does not revive its scope.
    #[tokio::test]
    async fn a_declined_scope_is_not_offered_again_under_a_live_expectation() {
        let peer = peer_at(Arc::new(Mutex::new(1_000)));
        let mut alice_cfg = GroupShareConfig::default();
        peer.expect_share_from(alice().actor_id());
        let (scope, offer) = alice_offers(&mut alice_cfg, 1_000);
        peer.ingest_frame(&alice().actor_id(), &offer)
            .await
            .expect("offer");
        crate::group_ceremony::decline_group_offer(
            &mut peer.config.lock().unwrap(),
            &scope,
            Timestamp(1_000),
        );
        let recorded = peer.config.lock().unwrap().invited.clone();

        peer.ingest_frame(&alice().actor_id(), &offer)
            .await
            .expect_err("the declined scope's offer is refused");
        assert_eq!(peer.config.lock().unwrap().invited, recorded);
    }
}
