//! The per-group succession **sweep driver**, driven end to end
//! (`docs/goal/behavior/identity-succession.md` § Propagation → *MLS groups*).
//!
//! `fauna-mls`'s own `succession_ceremony.rs` pins the two commits in isolation.
//! What is proven here is the thing a *user* gets: after
//! [`sweep_groups`] runs over a succeeded identity, every one of that identity's
//! groups has actually been re-pointed — the successor reads, the thief does
//! not — with the commits carried over the real `fauna.conversations.channel.send`
//! wire shape rather than handed between engines in memory.
//!
//! Three engines, for the same reason the ceremony test uses three: a two-engine
//! test cannot tell "the group survived" from "the group became a DM". `bob` is
//! an unrelated member who must keep reading across both commits and is never
//! asked to re-invite anyone.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fauna_client_recovery::{
    GroupSweepState, RecoveryClient, SweepRetryOutcome, retry_group_sweep, sweep_groups,
};
use fauna_core::data::Timestamp;
use fauna_core::encoding::canonical_encode;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::recovery::{
    ChainHead, IdentitySuccession, RecoveryKey, RecoveryKeyRegistration, SignedIdentitySuccession,
};
use fauna_mls::channel::GroupChannel;
use fauna_mls::engine::MlsEngine;
use fauna_mls::types::{ChannelId, ChannelMessage, ChannelMessageBody, GroupMetaMessage};
use fauna_protocol::conversations::{ChannelEnvelope, ChannelSendReply, ChannelSendRequest};
use fauna_protocol::recovery as recovery_wire;
use fauna_protocol::{ByteBuf, RpcError, RpcErrorClass, RpcRequester};

// ── a nest that actually carries the channel ────────────────────────────────

#[derive(Debug)]
struct FakeError(RpcError);

impl core::fmt::Display for FakeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0.code)
    }
}

impl RpcErrorClass for FakeError {
    fn is_rejection(&self) -> bool {
        true
    }
    fn as_rpc_error(&self) -> Option<&RpcError> {
        Some(&self.0)
    }
}

/// A nest that does the one thing the sweep needs of it: accept a channel post
/// and deliver its commit to the other members.
///
/// Delivering for real is the point — a fake that only *counted* sends would
/// pass while every member's group state silently diverged, which is precisely
/// the failure the ceremony exists to prevent.
struct ChannelNest {
    /// Members who apply every commit posted to a channel they hold.
    members: Vec<Arc<MlsEngine>>,
    /// Channels this nest refuses to accept a post for — how a **claimed
    /// folder channel**'s owner-managed roster presents to the successor,
    /// which may only auto-register on an ordinary channel.
    refuse: Vec<ChannelId>,
    /// Posts accepted, per channel, in order — the raw envelope bytes, so a
    /// test can assert the [Commit, Application, Commit] shape of a sweep.
    sent: Mutex<HashMap<ChannelId, Vec<Vec<u8>>>>,
    /// What each member decrypted from delivered application messages, per
    /// channel, in order — captured at delivery because a sender-ratchet
    /// decrypt is one-shot.
    delivered: Mutex<HashMap<ChannelId, Vec<(fauna_core::identity::ActorId, ChannelMessage)>>>,
    /// Landed successions the recovery plane serves, per old identity: the
    /// seq-1 registration link and the landed statement, both as verbatim
    /// canonical bytes — what [`retry_group_sweep`]'s chain-verified
    /// re-acquisition walks. Absent = no succession landed for that identity.
    landed: HashMap<ActorId, (Vec<u8>, Vec<u8>)>,
}

impl ChannelNest {
    fn new(members: Vec<Arc<MlsEngine>>) -> Self {
        Self {
            members,
            refuse: Vec::new(),
            sent: Mutex::new(HashMap::new()),
            delivered: Mutex::new(HashMap::new()),
            landed: HashMap::new(),
        }
    }

    fn refusing(mut self, channel: ChannelId) -> Self {
        self.refuse.push(channel);
        self
    }

    /// Serve a landed succession for `old` — [`landed_succession`]'s output.
    fn with_landed_succession(
        mut self,
        old: ActorId,
        registration: Vec<u8>,
        statement: Vec<u8>,
    ) -> Self {
        self.landed.insert(old, (registration, statement));
        self
    }

    fn sent_count(&self, channel: &ChannelId) -> usize {
        self.sent
            .lock()
            .unwrap()
            .get(channel)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    fn total_sent(&self) -> usize {
        self.sent.lock().unwrap().values().map(|v| v.len()).sum()
    }

    /// The decoded envelope sequence a channel saw.
    fn sent_envelopes(&self, channel: &ChannelId) -> Vec<ChannelEnvelope> {
        self.sent
            .lock()
            .unwrap()
            .get(channel)
            .map(|v| {
                v.iter()
                    .map(|bytes| ChannelEnvelope::from_bytes(bytes).unwrap())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// What `member` decrypted on `channel`, in delivery order.
    fn delivered_to(
        &self,
        channel: &ChannelId,
        member: fauna_core::identity::ActorId,
    ) -> Vec<ChannelMessage> {
        self.delivered
            .lock()
            .unwrap()
            .get(channel)
            .map(|v| {
                v.iter()
                    .filter(|(m, _)| *m == member)
                    .map(|(_, cm)| cm.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn accept(&self, req: &ChannelSendRequest) -> Result<(), FakeError> {
        let channel = ChannelId::from_hex(&req.channel_id).expect("channel id is 32-byte hex");
        if self.refuse.contains(&channel) {
            // The shape a roster refusal takes on the wire.
            return Err(FakeError(RpcError::new(
                "fauna.conversations.forbidden",
                "sender is not on this channel's roster",
            )));
        }

        match ChannelEnvelope::from_bytes(&req.envelope).expect("a decodable envelope") {
            ChannelEnvelope::Commit(commit) => {
                for member in &self.members {
                    if member.has_group(&channel) {
                        member
                            .process_commit(&channel, &commit)
                            .expect("a member must be able to apply a swept commit");
                    }
                }
            }
            // A community room's generation-sealed envelope never reaches this
            // fake: the sweep drives MLS group channels, and a room whose log
            // seals under a generation key has no MLS group to sweep.
            ChannelEnvelope::RoomSealed { .. } | ChannelEnvelope::RoomFloorDelete(_) => {}
            // The in-group succession statement. Deliver it exactly as members
            // would receive it — each opens their own copy off their own ratchet
            // view — and record what they saw, since a sender-ratchet decrypt is
            // one-shot and the test cannot re-open it later.
            ChannelEnvelope::Application(ct) => {
                for member in &self.members {
                    if member.has_group(&channel) {
                        let cm = member
                            .decrypt(&channel, &ct)
                            .expect("a member must be able to open a swept statement");
                        self.delivered
                            .lock()
                            .unwrap()
                            .entry(channel)
                            .or_default()
                            .push((member.identity_actor_id(), cm));
                    }
                }
            }
        }

        self.sent
            .lock()
            .unwrap()
            .entry(channel)
            .or_default()
            .push(req.envelope.clone());
        Ok(())
    }
}

impl RpcRequester for ChannelNest {
    type Error = FakeError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let bytes = fauna_protocol::encode_canonical(&payload).unwrap();
        let reply = match kind {
            "fauna.conversations.channel.send" => {
                let req: ChannelSendRequest = fauna_protocol::decode_strict(&bytes).unwrap();
                self.accept(&req)?;
                fauna_protocol::encode_canonical(&ChannelSendReply {
                    seq: 1,
                    extra: Default::default(),
                })
                .unwrap()
            }
            // The retry's chain-verified statement re-acquisition — two
            // read-only recovery kinds, served from the pre-landed material.
            "fauna.recovery.succession.lookup" => {
                let req: recovery_wire::SuccessionLookupRequest =
                    fauna_protocol::decode_strict(&bytes).unwrap();
                let actor = ActorId(req.actor_id.as_ref().try_into().unwrap());
                fauna_protocol::encode_canonical(&recovery_wire::SuccessionLookupReply {
                    statements: self
                        .landed
                        .get(&actor)
                        .map(|(_, statement)| vec![ByteBuf::from(statement.clone())])
                        .unwrap_or_default(),
                    ..Default::default()
                })
                .unwrap()
            }
            "fauna.recovery.registration.chain" => {
                let req: recovery_wire::RegistrationChainRequest =
                    fauna_protocol::decode_strict(&bytes).unwrap();
                let actor = ActorId(req.actor_id.as_ref().try_into().unwrap());
                fauna_protocol::encode_canonical(&recovery_wire::RegistrationChainReply {
                    registrations: self
                        .landed
                        .get(&actor)
                        .map(|(registration, _)| vec![ByteBuf::from(registration.clone())])
                        .unwrap_or_default(),
                    ..Default::default()
                })
                .unwrap()
            }
            other => panic!(
                "the sweep uploads over channel.send, and its retry adds only the two \
                 read-only recovery kinds — {other} would mean the transport ruling \
                 was quietly reopened"
            ),
        };
        Ok(fauna_protocol::decode_strict(&reply).unwrap())
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn engine() -> Arc<MlsEngine> {
    Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap())
}

/// An engine whose identity secret the test also holds — the succeeded and
/// successor roles, which must sign the statement fixture. Deterministic and
/// distinct per `seed`.
fn keyed_engine(seed: u8) -> (Arc<MlsEngine>, ActorKeypair) {
    let engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret([seed; 32])).unwrap());
    (engine, ActorKeypair::from_secret([seed; 32]))
}

/// A genuine succession statement for the pair, plus the [`ChainHead`] a
/// consumer that saw the registration would hold — what a member verifies the
/// received statement against.
fn signed_statement(
    old: &ActorKeypair,
    successor: &ActorKeypair,
) -> (SignedIdentitySuccession, ChainHead) {
    let recovery = RecoveryKey::generate();
    let statement = IdentitySuccession {
        old_actor_id: old.actor_id(),
        new_actor_id: successor.actor_id(),
        recovery_pubkey: recovery.public(),
        seq: 2,
        created_at: Timestamp::now(),
    };
    let signed = statement
        .sign(&recovery, successor.signing_key(), Some(old.signing_key()))
        .expect("the fixture statement signs");
    (signed, ChainHead::new(recovery.public(), 1))
}

/// A landed succession exactly as the recovery plane serves it to the retry's
/// chain-verified walk: the old identity's seq-1 RecoveryKey registration and
/// the statement that key authorized, both as verbatim canonical bytes.
fn landed_succession(old: &ActorKeypair, successor: &ActorKeypair) -> (Vec<u8>, Vec<u8>) {
    let recovery = RecoveryKey::generate();
    let registration = RecoveryKeyRegistration {
        actor_id: old.actor_id(),
        recovery_pubkey: recovery.public(),
        seq: 1,
        created_at: Timestamp::now(),
    }
    .sign(old.signing_key(), &recovery, None)
    .expect("the fixture registration signs");
    let statement = IdentitySuccession {
        old_actor_id: old.actor_id(),
        new_actor_id: successor.actor_id(),
        recovery_pubkey: recovery.public(),
        seq: 2,
        created_at: Timestamp::now(),
    }
    .sign(&recovery, successor.signing_key(), Some(old.signing_key()))
    .expect("the fixture statement signs");
    (
        canonical_encode(&registration).expect("registration encodes"),
        canonical_encode(&statement).expect("statement encodes"),
    )
}

fn text_msg(from: &MlsEngine, seq: u64, text: &str) -> ChannelMessage {
    ChannelMessage {
        sender: from.identity_actor_id(),
        sequence: seq,
        channel_epoch: 0,
        body: ChannelMessageBody::Text(text.into()),
        timestamp: Timestamp::now(),
    }
}

/// A group `owner` and `bob` share.
fn shared_group(owner: &MlsEngine, bob: &MlsEngine) -> ChannelId {
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (owner_group, welcome) = GroupChannel::create(owner, &bob_kps).unwrap();
    GroupChannel::join(bob, welcome).unwrap();
    owner_group.channel_id
}

// ── the sweep stamps its own join ─────────────────────

/// The successor's fresh seat carries the predecessor's folder-owner marker:
/// re-pointed to the successor where the predecessor OWNED the channel (so the
/// successor admits its own roster commits and anchors the declassification
/// verdict), copied as is where the predecessor merely joined someone else's
/// folder channel, and absent on a chat group, which stays on open commit
/// processing.
#[tokio::test]
async fn a_sweep_stamps_the_successor_as_folder_owner_where_the_predecessor_owned_the_channel() {
    let (old, old_kp) = keyed_engine(1);
    let bob = engine();
    let (successor, successor_kp) = keyed_engine(2);
    let old_actor = old.identity_actor_id();
    let bobs_actor = bob.identity_actor_id();
    let (statement, _head) = signed_statement(&old_kp, &successor_kp);

    let owned = shared_group(&old, &bob);
    let joined = shared_group(&old, &bob);
    let chat = shared_group(&old, &bob);
    // The folder mint's stamp on a set the old identity owned, and the
    // folder-Welcome join's stamp on one bob owns that it joined.
    old.mark_folder_channel_owner(&owned, &old_actor);
    old.mark_folder_channel_owner(&joined, &bobs_actor);

    let client = RecoveryClient::new(ChannelNest::new(vec![Arc::clone(&bob)]));
    let report = sweep_groups(&client, &old, &successor, &statement).await;
    assert!(
        report
            .outcomes
            .iter()
            .all(|o| o.state == GroupSweepState::Swept),
        "{report:?}"
    );

    assert_eq!(
        successor.folder_channel_owner(&owned),
        Some(successor.identity_actor_id()),
        "the successor now owns what the predecessor owned"
    );
    assert_eq!(
        successor.folder_channel_owner(&joined),
        Some(bobs_actor),
        "a channel the predecessor merely joined keeps its owner"
    );
    assert_eq!(
        successor.folder_channel_owner(&chat),
        None,
        "a chat group carries no marker"
    );
}

/// The resumed path stamps too: an interrupted run that got as far as the
/// successor's join and died before anything else left the successor's seat
/// unstamped, and the resume — which redoes the statement before its remove —
/// redoes the stamp with it.
#[tokio::test]
async fn a_resumed_sweep_stamps_the_marker_the_interrupted_run_never_reached() {
    let (old, old_kp) = keyed_engine(1);
    let bob = engine();
    let (successor, successor_kp) = keyed_engine(2);
    let old_actor = old.identity_actor_id();
    let (statement, _head) = signed_statement(&old_kp, &successor_kp);
    let owned = shared_group(&old, &bob);
    old.mark_folder_channel_owner(&owned, &old_actor);

    // The interrupted run: add-successor landed everywhere and the successor
    // joined, and the process died there.
    let key_package = successor.generate_key_packages(1).unwrap().remove(0);
    let add = fauna_mls::succession::commit_add_successor(&old, &owned, &key_package).unwrap();
    bob.process_commit(&owned, &add.commit_bytes).unwrap();
    successor.join_from_welcome(add.welcome).unwrap();
    assert_eq!(successor.folder_channel_owner(&owned), None);

    let client = RecoveryClient::new(ChannelNest::new(vec![Arc::clone(&bob)]));
    let report = sweep_groups(&client, &old, &successor, &statement).await;
    assert_eq!(
        report.outcomes[0].state,
        GroupSweepState::ResumedRemoveOnly,
        "{report:?}"
    );
    assert_eq!(
        successor.folder_channel_owner(&owned),
        Some(successor.identity_actor_id())
    );
}

// ── the property the sweep exists for ───────────────────────────────────────

/// The whole point: after the sweep, a real user's group is re-pointed — and the
/// thief holding the old leaf can no longer read it.
#[tokio::test]
async fn a_sweep_repoints_every_group_and_ratchets_the_thief_out() {
    let (old, old_kp) = keyed_engine(1);
    let bob = engine();
    let (successor, successor_kp) = keyed_engine(2);
    let old_actor = old.identity_actor_id();
    let (statement, _head) = signed_statement(&old_kp, &successor_kp);

    let one = shared_group(&old, &bob);
    let two = shared_group(&old, &bob);

    // Baseline — which is also exactly what the thief can do until the sweep runs.
    let sealed = bob.encrypt(&one, &text_msg(&bob, 1, "before")).unwrap();
    assert!(
        old.decrypt(&one, &sealed).is_ok(),
        "the old leaf reads the group before the sweep — the reach being closed"
    );

    let nest = ChannelNest::new(vec![Arc::clone(&bob)]);
    let client = RecoveryClient::new(nest);
    let report = sweep_groups(&client, &old, &successor, &statement).await;

    assert_eq!(
        report.outcomes.len(),
        2,
        "both groups were swept: {report:?}"
    );
    assert!(
        report
            .outcomes
            .iter()
            .all(|o| o.state == GroupSweepState::Swept),
        "every group ran both halves: {report:?}"
    );
    assert!(
        report.old_leaf_removed_everywhere(),
        "no group owes the ceremony: {report:?}"
    );

    // Three posts per group — add-successor, the in-group statement, remove-old.
    assert_eq!(client.transport().sent_count(&one), 3);
    assert_eq!(client.transport().sent_count(&two), 3);

    for channel in [one, two] {
        // Continuity: bob was never asked to re-invite anyone, and still reads.
        let after = bob.encrypt(&channel, &text_msg(&bob, 2, "after")).unwrap();
        let seen = successor.decrypt(&channel, &after).unwrap();
        assert!(
            matches!(&seen.body, ChannelMessageBody::Text(t) if t == "after"),
            "the successor must read post-sweep traffic"
        );

        // The assertion the ceremony exists for.
        assert!(
            old.decrypt(&channel, &after).is_err(),
            "the removed old leaf — the thief's copy — must not read post-sweep traffic"
        );

        let members = bob.group_members(&channel);
        assert!(
            members.contains(&successor.identity_actor_id()),
            "the successor must be a member of {channel}"
        );
        assert!(
            !members.contains(&old_actor),
            "the succeeded identity must be gone from {channel}'s roster"
        );
    }
}

/// A sweep is re-runnable. The second pass must recognise finished work from
/// membership alone and post nothing — an unconditional re-run would grow a
/// duplicate leaf per pass.
#[tokio::test]
async fn a_completed_sweep_is_idempotent() {
    let (old, old_kp) = keyed_engine(1);
    let bob = engine();
    let (successor, successor_kp) = keyed_engine(2);
    let (statement, _head) = signed_statement(&old_kp, &successor_kp);
    let channel = shared_group(&old, &bob);

    let client = RecoveryClient::new(ChannelNest::new(vec![Arc::clone(&bob)]));
    sweep_groups(&client, &old, &successor, &statement).await;
    let after_first = client.transport().total_sent();

    let report = sweep_groups(&client, &old, &successor, &statement).await;

    assert_eq!(
        report.outcomes[0].state,
        GroupSweepState::AlreadySwept,
        "a finished group is recognised, not re-run: {report:?}"
    );
    assert_eq!(
        client.transport().total_sent(),
        after_first,
        "a re-run must post nothing"
    );
    assert_eq!(
        bob.group_members(&channel).len(),
        2,
        "still exactly bob + the successor — no duplicate leaf"
    );
}

/// One unreachable group must not deny the others their re-key. This is how a
/// **claimed folder channel** presents: the successor may not auto-register on
/// its owner-managed roster, so the post is refused.
#[tokio::test]
async fn a_refused_channel_does_not_deny_the_other_groups() {
    let (old, old_kp) = keyed_engine(1);
    let bob = engine();
    let (successor, successor_kp) = keyed_engine(2);
    let (statement, _head) = signed_statement(&old_kp, &successor_kp);

    let reachable = shared_group(&old, &bob);
    let claimed = shared_group(&old, &bob);

    let nest = ChannelNest::new(vec![Arc::clone(&bob)]).refusing(claimed);
    let client = RecoveryClient::new(nest);
    let report = sweep_groups(&client, &old, &successor, &statement).await;

    let verdict = |c: ChannelId| {
        report
            .outcomes
            .iter()
            .find(|o| o.channel_id == c)
            .map(|o| o.state.clone())
            .unwrap_or_else(|| panic!("no outcome recorded for {c}"))
    };

    assert_eq!(
        verdict(reachable),
        GroupSweepState::Swept,
        "the reachable group is swept regardless of its neighbour: {report:?}"
    );
    assert!(
        matches!(verdict(claimed), GroupSweepState::Failed(_)),
        "the refused channel is reported, not silently skipped: {report:?}"
    );

    // And the residual is surfaced rather than rounded down to "done".
    assert!(
        !report.old_leaf_removed_everywhere(),
        "an unreachable group leaves a residual"
    );
    assert_eq!(report.groups_old_leaf_removed(), 1);
    assert_eq!(report.groups_owing_ceremony().len(), 1);

    // The reachable group is genuinely closed, not merely reported so.
    let after = bob
        .encrypt(&reachable, &text_msg(&bob, 2, "after"))
        .unwrap();
    assert!(successor.decrypt(&reachable, &after).is_ok());
    assert!(
        old.decrypt(&reachable, &after).is_err(),
        "the thief is out of the group that could be swept"
    );
}

/// An identity with no groups sweeps to an empty, complete report — the honest
/// answer, and one a UI can render without a special case.
#[tokio::test]
async fn an_identity_with_no_groups_sweeps_clean() {
    let (old, old_kp) = keyed_engine(1);
    let (successor, successor_kp) = keyed_engine(2);
    let (statement, _head) = signed_statement(&old_kp, &successor_kp);

    let client = RecoveryClient::new(ChannelNest::new(vec![]));
    let report = sweep_groups(&client, &old, &successor, &statement).await;

    assert!(report.outcomes.is_empty());
    assert!(
        report.old_leaf_removed_everywhere(),
        "nothing owed is complete, not failed"
    );
    assert!(!report.has_unattested_members());
    assert_eq!(client.transport().total_sent(), 0);
}

// ── what the ceremony does NOT settle ────────────────────────────────

/// The finding this pin exists for: the thief
/// held the **seed**, therefore the old leaf's authority to author `add_member`,
/// so they may have seated a leaf under a second identity they control. That
/// leaf survives the ceremony — and the honest requirement is that it be
/// *reported*, never silently rounded into "the thief is out".
///
/// The control the finding asks to keep green sits in the same test: the old
/// leaf really is out. That half of the claim always held; the broad one did not
/// follow from it.
#[tokio::test]
async fn a_planted_leaf_survives_the_ceremony_and_is_reported_as_unattested() {
    let (old, old_kp) = keyed_engine(1);
    let bob = engine();
    let (successor, successor_kp) = keyed_engine(2);
    let (statement, _head) = signed_statement(&old_kp, &successor_kp);
    // The second identity the thief controls, seated with the stolen leaf's
    // authority before the succession — a window the thief owned by definition.
    let mallory = engine();

    let channel = shared_group(&old, &bob);
    let mallory_kp = mallory.generate_key_packages(1).unwrap().pop().unwrap();
    let (plant, welcome) = old.add_member(&channel, &mallory_kp).unwrap();
    bob.process_commit(&channel, &plant).unwrap();
    mallory.join_from_welcome(welcome).unwrap();

    let nest = ChannelNest::new(vec![Arc::clone(&bob), Arc::clone(&mallory)]);
    let client = RecoveryClient::new(nest);
    let report = sweep_groups(&client, &old, &successor, &statement).await;

    // The ceremony itself ran: the succeeded credential is gone.
    assert!(
        report.old_leaf_removed_everywhere(),
        "the ceremony ran: {report:?}"
    );

    // CONTROL — the narrow claim, which always held.
    let after = bob.encrypt(&channel, &text_msg(&bob, 2, "after")).unwrap();
    assert!(
        successor.decrypt(&channel, &after).is_ok(),
        "the successor reads post-sweep traffic"
    );
    assert!(
        old.decrypt(&channel, &after).is_err(),
        "the removed old leaf must not read post-sweep traffic"
    );

    // THE FINDING — the planted leaf is untouched by the ceremony and still
    // reads. That is not a bug to fix here (the client cannot tell it from an
    // honestly-added member); it is a fact the report must not hide.
    assert!(
        mallory.decrypt(&channel, &after).is_ok(),
        "the planted leaf still reads — which is exactly why it must be reported"
    );
    assert!(
        report
            .unattested_members()
            .contains(&mallory.identity_actor_id()),
        "the planted leaf must be surfaced as unattested, never rounded into \
         'the thief is out': {report:?}"
    );
    assert!(report.has_unattested_members());

    // Honest members land in the same list — the client cannot distinguish them,
    // and pretending otherwise is the overclaim in miniature.
    assert!(
        report
            .unattested_members()
            .contains(&bob.identity_actor_id()),
        "bob is unattested too — the sweep reports, it does not adjudicate"
    );
    // The two the ceremony DID act on are never listed: the successor it added,
    // and the old leaf it evicted.
    assert!(
        !report
            .unattested_members()
            .contains(&successor.identity_actor_id())
    );
    assert!(
        !report
            .unattested_members()
            .contains(&old.identity_actor_id())
    );
}

/// The **fallback** is what makes a
/// failed group report anyone at all, and it was unpinned.
///
/// The roster is read off whichever engine still holds the group: the
/// successor's once it has joined, else the old one. That `else` is the whole
/// answer for a group the successor never reached — and a group the successor
/// never reached is exactly where a planted leaf goes unseen, so it is the group
/// whose roster matters most. Since the aftermath writes this roster down,
/// `SweepReport::unattested_members` is the permanent basis of the review
/// surfaces (§ Propagation → *Removing a flagged member*, rule (1)).
///
/// What makes it worth a test of its own is how quietly the tempting cleanup
/// fails: "read the successor's engine, it is the current identity" looks
/// strictly more correct, and `MlsEngine::group_members` answers `Vec::new()`
/// for a group it does not hold — so the planted leaf is not mis-reported, it is
/// reported as *nobody*, and the sweep looks clean.
#[tokio::test]
async fn a_failed_group_reports_the_roster_the_old_engine_still_holds() {
    let (old, old_kp) = keyed_engine(1);
    let bob = engine();
    let (successor, successor_kp) = keyed_engine(2);
    let (statement, _head) = signed_statement(&old_kp, &successor_kp);
    // The second identity the thief controls, seated with the stolen leaf's
    // authority before the succession.
    let mallory = engine();

    let unreachable = shared_group(&old, &bob);
    let mallory_kp = mallory.generate_key_packages(1).unwrap().pop().unwrap();
    let (plant, welcome) = old.add_member(&unreachable, &mallory_kp).unwrap();
    bob.process_commit(&unreachable, &plant).unwrap();
    mallory.join_from_welcome(welcome).unwrap();

    // The post is refused, so the successor never joins — how a claimed folder
    // channel's owner-managed roster presents to a stranger.
    let nest = ChannelNest::new(vec![Arc::clone(&bob), Arc::clone(&mallory)]).refusing(unreachable);
    let client = RecoveryClient::new(nest);
    let report = sweep_groups(&client, &old, &successor, &statement).await;

    // ── the fixture is on the side of the gate this test is about ───────────
    // Without these, a fixture that drifted into "the successor did join" would
    // pass while asserting nothing about the fallback.
    assert!(
        matches!(&report.outcomes[0].state, GroupSweepState::Failed(_)),
        "the refused group must fail — otherwise this is not the fallback's case: {report:?}"
    );
    assert!(
        !successor.has_group(&unreachable),
        "the successor must NOT hold the refused group — that absence is the \
         branch under test"
    );
    assert!(
        old.has_group(&unreachable),
        "the old engine still holds the roster, which is why an answer exists"
    );
    assert!(
        successor.group_members(&unreachable).is_empty(),
        "and this is what the successor's engine would answer — the empty the \
         severed fallback would report as the user's roster"
    );

    // ── the roster reported is the old engine's, not an empty ───────────────
    let reported = report.unattested_members();
    assert!(
        reported.contains(&mallory.identity_actor_id()),
        "the planted leaf is in the group the ceremony could not reach and must \
         be reported: {report:?}"
    );
    assert!(
        reported.contains(&bob.identity_actor_id()),
        "the honest member is reported too — the sweep reports, it does not \
         adjudicate: {report:?}"
    );

    // Asserted as the EQUALITY rather than a hand-listed roster: the property is
    // "the report reads the old engine", and a hand-listed expectation stays
    // green while both sides drift together.
    let mut expected: Vec<_> = old
        .group_members(&unreachable)
        .into_iter()
        .filter(|m| *m != old.identity_actor_id() && *m != successor.identity_actor_id())
        .collect();
    expected.sort_unstable_by_key(|a| a.0);
    assert_eq!(
        reported, expected,
        "the failed group's roster IS the old engine's, minus the pair the \
         ceremony acts on: {report:?}"
    );
}

/// The sweep evicts the succeeded credential with **one** commit, and every leaf
/// bearing it — see `a_credential_cannot_be_seated_twice` in `fauna-mls` for why
/// "every" is one today, and why `commit_remove_old` declines to depend on that.
#[tokio::test]
async fn the_succeeded_credential_is_evicted_in_a_single_commit() {
    let (old, old_kp) = keyed_engine(1);
    let bob = engine();
    let (successor, successor_kp) = keyed_engine(2);
    let (statement, _head) = signed_statement(&old_kp, &successor_kp);
    let old_actor = old.identity_actor_id();

    let channel = shared_group(&old, &bob);

    let client = RecoveryClient::new(ChannelNest::new(vec![Arc::clone(&bob)]));
    let report = sweep_groups(&client, &old, &successor, &statement).await;

    assert!(
        report.old_leaf_removed_everywhere(),
        "the ceremony ran: {report:?}"
    );
    assert!(
        bob.find_leaves_by_identity(&channel, &old_actor).is_empty(),
        "no leaf bearing the succeeded credential may survive: {:?}",
        bob.group_members(&channel)
    );
    let commits = client
        .transport()
        .sent_envelopes(&channel)
        .into_iter()
        .filter(|e| matches!(e, ChannelEnvelope::Commit(_)))
        .count();
    assert_eq!(
        commits, 2,
        "add-successor plus ONE remove commit — an eviction must not cost the \
         other members an epoch per leaf"
    );
}

// ── the in-group statement (§ Propagation: continuity, not a stranger join) ──

/// The statement rides **between** the two commits, and what a member decrypts
/// is the genuine, verifiable `SignedIdentitySuccession` — the § The succession
/// statement verification rule passes against the chain head the member knows,
/// and the pair it names is exactly the pair the ceremony moved.
#[tokio::test]
async fn the_statement_rides_between_the_commits_and_verifies() {
    let (old, old_kp) = keyed_engine(1);
    let bob = engine();
    let (successor, successor_kp) = keyed_engine(2);
    let (statement, head) = signed_statement(&old_kp, &successor_kp);

    let channel = shared_group(&old, &bob);

    let client = RecoveryClient::new(ChannelNest::new(vec![Arc::clone(&bob)]));
    let report = sweep_groups(&client, &old, &successor, &statement).await;
    assert!(report.old_leaf_removed_everywhere(), "{report:?}");

    // The wire order members experience: add, statement, remove.
    let shapes: Vec<&'static str> = client
        .transport()
        .sent_envelopes(&channel)
        .iter()
        .map(|e| match e {
            ChannelEnvelope::Commit(_) => "commit",
            ChannelEnvelope::Application(_) => "statement",
            ChannelEnvelope::RoomSealed { .. } => "room-sealed",
            ChannelEnvelope::RoomFloorDelete(_) => "room-floor-delete",
        })
        .collect();
    assert_eq!(
        shapes,
        vec!["commit", "statement", "commit"],
        "the statement rides between add-successor and remove-old"
    );

    // What bob actually received, decrypted off his own ratchet view.
    let received = client
        .transport()
        .delivered_to(&channel, bob.identity_actor_id());
    assert_eq!(received.len(), 1, "exactly one statement per group");
    let ChannelMessageBody::GroupMeta(GroupMetaMessage::Succession(bytes)) = &received[0].body
    else {
        panic!("the statement must ride as GroupMeta::Succession: {received:?}");
    };

    // The carried bytes are the verbatim canonical statement …
    let decoded: SignedIdentitySuccession =
        fauna_protocol::decode_strict(bytes).expect("a member decodes the carried statement bytes");
    assert_eq!(decoded, statement, "verbatim — never re-encoded");

    // … they verify under the rule every consumer runs …
    decoded
        .verify(&head)
        .expect("the statement verifies against the head the member knows");

    // … and the verified pair is the pair the ceremony moved.
    assert_eq!(decoded.statement.old_actor_id, old.identity_actor_id());
    assert_eq!(
        decoded.statement.new_actor_id,
        successor.identity_actor_id()
    );
}

/// The resume path re-posts the statement before its remove: membership facts
/// cannot say whether the interrupted run got that far, and a duplicate is
/// harmless where a silent gap is not.
#[tokio::test]
async fn a_resumed_sweep_still_offers_the_statement() {
    let (old, old_kp) = keyed_engine(1);
    let bob = engine();
    let (successor, successor_kp) = keyed_engine(2);
    let (statement, _head) = signed_statement(&old_kp, &successor_kp);

    let channel = shared_group(&old, &bob);

    // An interrupted earlier run: the add was authored, published and joined,
    // and nothing after it happened — the crash window right before the
    // statement post.
    let kp = successor.generate_key_packages(1).unwrap().pop().unwrap();
    let add = fauna_mls::succession::commit_add_successor(&old, &channel, &kp).unwrap();
    bob.process_commit(&channel, &add.commit_bytes).unwrap();
    successor.join_from_welcome(add.welcome).unwrap();

    let client = RecoveryClient::new(ChannelNest::new(vec![Arc::clone(&bob)]));
    let report = sweep_groups(&client, &old, &successor, &statement).await;

    assert_eq!(
        report.outcomes[0].state,
        GroupSweepState::ResumedRemoveOnly,
        "{report:?}"
    );
    let shapes: Vec<&'static str> = client
        .transport()
        .sent_envelopes(&channel)
        .iter()
        .map(|e| match e {
            ChannelEnvelope::Commit(_) => "commit",
            ChannelEnvelope::Application(_) => "statement",
            ChannelEnvelope::RoomSealed { .. } => "room-sealed",
            ChannelEnvelope::RoomFloorDelete(_) => "room-floor-delete",
        })
        .collect();
    assert_eq!(
        shapes,
        vec!["statement", "commit"],
        "the resume still offers the statement, before its remove"
    );
    assert_eq!(
        client
            .transport()
            .delivered_to(&channel, bob.identity_actor_id())
            .len(),
        1,
        "bob received the statement despite the interrupted first run"
    );
}

/// A statement naming any pair other than the two engines is refused before
/// anything is published — it would be a *true, verifiable* statement about the
/// wrong identities, and members would re-point the wrong person.
#[tokio::test]
async fn a_mismatched_statement_is_refused_before_anything_is_published() {
    let (old, old_kp) = keyed_engine(1);
    let bob = engine();
    let (successor, _successor_kp) = keyed_engine(2);
    // Signed for a DIFFERENT successor than the engine pair being swept.
    let other = ActorKeypair::from_secret([3u8; 32]);
    let (statement, _head) = signed_statement(&old_kp, &other);

    let channel = shared_group(&old, &bob);

    let client = RecoveryClient::new(ChannelNest::new(vec![Arc::clone(&bob)]));
    let report = sweep_groups(&client, &old, &successor, &statement).await;

    assert!(
        matches!(&report.outcomes[0].state, GroupSweepState::Failed(r) if r.contains("refusing")),
        "the mismatch is a loud per-group failure: {report:?}"
    );
    assert_eq!(
        client.transport().total_sent(),
        0,
        "nothing may be published under a mismatched statement"
    );
    assert!(
        bob.group_members(&channel)
            .contains(&old.identity_actor_id()),
        "the ceremony did not run"
    );

    // a refusing sweep still reports its rosters.
    //
    // This is the ONLY pin either whole-sweep disaster arm can have: its sibling
    // ("the succession statement does not encode") is unconstructible, since
    // `SignedIdentitySuccession` holds nothing dag-cbor can refuse. Which is why
    // both arms return through one `failed_sweep_report` — this assertion covers
    // the sibling exactly as long as that stays true.
    assert_eq!(
        report.unattested_members(),
        vec![bob.identity_actor_id()],
        "a sweep that refuses before publishing must still tell the user who is \
         in their groups: an empty roster here reaches the aftermath's raise as \
         `NothingToRaise`, i.e. the disaster arm reporting that there is nobody \
         to review: {report:?}"
    );
}

// ── the retry: finishing a ceremony whose sweep never ran ───────────────────

/// **The pin the retry exists for**: a `NoEngine` succession
/// — the ceremony landed but conversations were not up, so the sweep never ran
/// and the group still holds the old leaf — followed by [`retry_group_sweep`]
/// evicts the old leaf. The retry re-acquires the landed statement through the
/// chain-verified walk (no handoff survives the account switch) and runs the
/// same sweep over the same engine pair; a second retry recognises finished
/// work from membership alone and posts nothing.
#[tokio::test]
async fn a_no_engine_succession_is_finished_by_the_retry() {
    let (old, old_kp) = keyed_engine(1);
    let bob = engine();
    let (successor, successor_kp) = keyed_engine(2);
    let old_actor = old.identity_actor_id();
    let channel = shared_group(&old, &bob);

    // The succession LANDED, but no ceremony-time sweep ever ran — the
    // `NoEngine` premise. Until the retry, the old leaf still reads.
    let (registration, statement) = landed_succession(&old_kp, &successor_kp);
    let client = RecoveryClient::new(
        ChannelNest::new(vec![Arc::clone(&bob)]).with_landed_succession(
            old_kp.actor_id(),
            registration,
            statement,
        ),
    );
    let sealed = bob.encrypt(&channel, &text_msg(&bob, 1, "before")).unwrap();
    assert!(
        old.decrypt(&channel, &sealed).is_ok(),
        "the old leaf reads the group before the retry — the reach being closed"
    );

    let outcome = retry_group_sweep(&client, &old, &successor, &successor_kp)
        .await
        .expect("the retry reaches the chain");
    let SweepRetryOutcome::Swept(report) = outcome else {
        panic!("a landed succession must be swept, got {outcome:?}");
    };
    assert!(
        report.old_leaf_removed_everywhere(),
        "the retry finishes the eviction: {report:?}"
    );

    // The same observables as the ceremony-time sweep: continuity for bob and
    // the successor, and the old leaf — the thief's copy — locked out.
    let after = bob.encrypt(&channel, &text_msg(&bob, 2, "after")).unwrap();
    let seen = successor.decrypt(&channel, &after).unwrap();
    assert!(
        matches!(&seen.body, ChannelMessageBody::Text(t) if t == "after"),
        "the successor must read post-retry traffic"
    );
    assert!(
        old.decrypt(&channel, &after).is_err(),
        "the removed old leaf must not read post-retry traffic"
    );
    let members = bob.group_members(&channel);
    assert!(members.contains(&successor.identity_actor_id()));
    assert!(!members.contains(&old_actor));

    // Re-runnable: finished work is recognised, nothing is posted twice.
    let sent = client.transport().total_sent();
    let second = retry_group_sweep(&client, &old, &successor, &successor_kp)
        .await
        .expect("the retry re-runs");
    let SweepRetryOutcome::Swept(second_report) = second else {
        panic!("a re-run still reports, got {second:?}");
    };
    assert_eq!(
        second_report.outcomes[0].state,
        GroupSweepState::AlreadySwept,
        "a finished group is recognised, not re-run: {second_report:?}"
    );
    assert_eq!(
        client.transport().total_sent(),
        sent,
        "a retry over finished work must post nothing"
    );
}

/// No landed succession → nothing to re-run, and nothing may be posted: the
/// retry answers `NotLanded` rather than sweeping on a statement nobody
/// authorized.
#[tokio::test]
async fn a_retry_with_no_landed_succession_posts_nothing() {
    let (old, _old_kp) = keyed_engine(1);
    let bob = engine();
    let (successor, successor_kp) = keyed_engine(2);
    shared_group(&old, &bob);

    let client = RecoveryClient::new(ChannelNest::new(vec![Arc::clone(&bob)]));
    let outcome = retry_group_sweep(&client, &old, &successor, &successor_kp)
        .await
        .expect("the retry reaches the chain");
    assert!(
        matches!(outcome, SweepRetryOutcome::NotLanded),
        "no landed succession means nothing to re-run, got {outcome:?}"
    );
    assert_eq!(
        client.transport().total_sent(),
        0,
        "nothing may be posted without a landed statement"
    );
}

/// The account was re-pointed to a DIFFERENT successor: the retry refuses —
/// a sweep under the held key would post a statement every member verifies and
/// refuses — and names the successor the chain actually authorizes.
#[tokio::test]
async fn a_retry_under_a_superseded_key_refuses_and_names_the_real_successor() {
    let (old, old_kp) = keyed_engine(1);
    let bob = engine();
    let winner_kp = ActorKeypair::from_secret([3u8; 32]);
    let (loser_engine, loser_kp) = keyed_engine(2);
    shared_group(&old, &bob);

    let (registration, statement) = landed_succession(&old_kp, &winner_kp);
    let client = RecoveryClient::new(
        ChannelNest::new(vec![Arc::clone(&bob)]).with_landed_succession(
            old_kp.actor_id(),
            registration,
            statement,
        ),
    );

    let outcome = retry_group_sweep(&client, &old, &loser_engine, &loser_kp)
        .await
        .expect("the retry reaches the chain");
    let SweepRetryOutcome::LandedForAnother { new_actor_id } = outcome else {
        panic!("a superseded key must be refused, got {outcome:?}");
    };
    assert_eq!(
        new_actor_id,
        winner_kp.actor_id(),
        "the refusal names the successor the chain authorizes"
    );
    assert_eq!(
        client.transport().total_sent(),
        0,
        "a refused retry must post nothing"
    );
}
