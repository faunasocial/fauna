//! The per-group identity-succession ceremony
//! (`docs/goal/behavior/identity-succession.md` § Propagation → *MLS groups*).
//!
//! The one property that matters, and the reason this ceremony is in the
//! **urgent** phase: after `remove-old` the thief who holds the old seed — and
//! therefore the old leaf's MLS state — can no longer read what the group says
//! next. Everything else here is scaffolding around that assertion.
//!
//! Three engines, because a two-engine test cannot tell "the group survived"
//! from "the group became a DM": `owner` (the succeeded identity, whose leaf the
//! thief also holds), `bob` (an unrelated third member who must see continuity
//! and never be disturbed), and `successor` (the new identity).

use fauna_core::data::Timestamp;
use fauna_core::identity::ActorKeypair;
use fauna_mls::channel::GroupChannel;
use fauna_mls::engine::MlsEngine;
use fauna_mls::succession::{commit_add_successor, commit_remove_old};
use fauna_mls::types::{ChannelMessage, ChannelMessageBody};
use tempfile::NamedTempFile;

fn make_engine() -> (MlsEngine, NamedTempFile) {
    let tmp = NamedTempFile::new().unwrap();
    let engine = MlsEngine::new(ActorKeypair::generate(), tmp.path()).unwrap();
    (engine, tmp)
}

fn text_msg(engine: &MlsEngine, seq: u64, text: &str) -> ChannelMessage {
    ChannelMessage {
        sender: engine.identity_actor_id(),
        sequence: seq,
        channel_epoch: 0,
        body: ChannelMessageBody::Text(text.into()),
        timestamp: Timestamp::now(),
    }
}

fn assert_text(msg: &ChannelMessage, expected: &str) {
    match &msg.body {
        ChannelMessageBody::Text(text) => assert_eq!(text, expected),
        other => panic!("expected Text body, got: {other:?}"),
    }
}

/// The whole ceremony, end to end, with the thief's reach asserted at each step.
#[test]
fn succession_adds_the_successor_then_ratchets_the_old_leaf_out() {
    let (owner, _tmp_o) = make_engine();
    let (bob, _tmp_b) = make_engine();
    let (successor, _tmp_s) = make_engine();
    let old_actor = owner.identity_actor_id();

    // ---- a group the owner and bob share -------------------------------
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (owner_group, welcome) = GroupChannel::create(&owner, &bob_kps).unwrap();
    let bob_group = GroupChannel::join(&bob, welcome).unwrap();
    let channel = owner_group.channel_id;

    // Baseline: the owner's leaf can read the group. This is also precisely
    // what the thief can do before the ceremony, which is why it must stop.
    let sealed = bob.encrypt(&bob_group.channel_id, &text_msg(&bob, 1, "before"));
    let sealed = sealed.unwrap();
    assert_text(&owner.decrypt(&channel, &sealed).unwrap(), "before");

    // ---- step 1: the OLD leaf commits add-successor ---------------------
    // Authored by the old leaf constructively one last time — the successor is
    // not yet a member and cannot author anything in this group.
    let successor_kp = successor.generate_key_packages(1).unwrap();
    let add = commit_add_successor(&owner, &channel, &successor_kp[0]).unwrap();

    bob.process_commit(&channel, &add.commit_bytes).unwrap();
    let successor_group = GroupChannel::join(&successor, add.welcome).unwrap();
    assert_eq!(
        successor_group.channel_id, channel,
        "the successor must land in the same group, not a new one"
    );

    // Continuity: bob and the successor now share an epoch, and bob was never
    // asked to re-invite anyone.
    let sealed = bob.encrypt(&channel, &text_msg(&bob, 2, "mid")).unwrap();
    assert_text(&successor.decrypt(&channel, &sealed).unwrap(), "mid");

    // ---- step 2: the NEW leaf commits remove-old ------------------------
    // MLS forbids committing one's own removal, which is why this half is
    // authored by the successor rather than by the old leaf.
    let remove = commit_remove_old(&successor, &channel, &old_actor).unwrap();
    bob.process_commit(&channel, &remove).unwrap();

    // ---- the assertion the ceremony exists for --------------------------
    let after = bob.encrypt(&channel, &text_msg(&bob, 3, "after")).unwrap();
    assert_text(&successor.decrypt(&channel, &after).unwrap(), "after");
    assert!(
        owner.decrypt(&channel, &after).is_err(),
        "the removed old leaf — the thief's copy — must not read post-removal traffic"
    );

    // And the roster is the successor's, not the old identity's.
    let members = bob.group_members(&channel);
    assert!(
        members.contains(&successor.identity_actor_id()),
        "successor must be a member"
    );
    assert!(
        !members.contains(&old_actor),
        "the succeeded identity must be gone from the roster"
    );
}

/// `remove_old` must refuse to remove the caller's own leaf. MLS rejects a
/// self-removal commit anyway, but the ceremony should name the mistake rather
/// than surface an opaque OpenMLS error — this is the guardrail against calling
/// the two halves in the wrong order (old leaf trying to run step 2).
#[test]
fn remove_old_refuses_to_remove_the_caller() {
    let (owner, _tmp_o) = make_engine();
    let (bob, _tmp_b) = make_engine();

    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (owner_group, _welcome) = GroupChannel::create(&owner, &bob_kps).unwrap();

    let err = commit_remove_old(&owner, &owner_group.channel_id, &owner.identity_actor_id())
        .expect_err("removing your own leaf is the wrong half of the ceremony");
    let msg = err.to_string();
    assert!(
        msg.contains("own leaf") || msg.contains("self"),
        "error should name the self-removal, got: {msg}"
    );
}

/// A member the group does not hold cannot be removed — the honest answer when
/// a per-group sweep is handed a group the succeeded identity never joined.
#[test]
fn remove_old_refuses_a_non_member() {
    let (owner, _tmp_o) = make_engine();
    let (bob, _tmp_b) = make_engine();
    let stranger = ActorKeypair::generate().actor_id();

    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (owner_group, _welcome) = GroupChannel::create(&owner, &bob_kps).unwrap();

    let err = commit_remove_old(&owner, &owner_group.channel_id, &stranger)
        .expect_err("a non-member has no leaf to remove");
    assert!(
        err.to_string().contains("not a member"),
        "error should name the missing membership, got: {err}"
    );
}

/// **The invariant `commit_remove_old` deliberately does not rely on.**
///
/// A seed thief can mint KeyPackages for the victim's own identity, so "seat the
/// stolen credential at a second leaf, and survive an eviction that removes only
/// the first" is the obvious next move. It does not work, and this pins why: a fauna
/// credential *is* its leaf signature key (`validate_leaf_binding`, enforced on
/// admission and on commit ingest alike), and MLS refuses to seat a signature
/// key already in the tree. A credential is therefore single-seated by
/// construction.
///
/// `commit_remove_old` still evicts *every* matching leaf rather than the first.
/// The guarantee above is real but lives two layers from the ceremony, and if it
/// ever weakened the failure would be silent and maximal — an identity reported
/// as evicted that still reads the group. This test is what would go red first.
#[test]
fn a_credential_cannot_be_seated_twice() {
    // The stolen seed itself: both engines below are built from it.
    let seed = [7u8; 32];
    let tmp = NamedTempFile::new().unwrap();
    let owner = MlsEngine::new(ActorKeypair::from_secret(seed), tmp.path()).unwrap();
    let (bob, _tmp_b) = make_engine();

    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (owner_group, welcome) = GroupChannel::create(&owner, &bob_kps).unwrap();
    GroupChannel::join(&bob, welcome).unwrap();
    let channel = owner_group.channel_id;

    // A second engine under the SAME identity — exactly what holding the seed
    // buys — offering a structurally valid KeyPackage for its own credential.
    let twin = MlsEngine::new_in_memory(ActorKeypair::from_secret(seed)).unwrap();
    let twin_kp = twin.generate_key_packages(1).unwrap().pop().unwrap();

    let err = owner
        .add_member(&channel, &twin_kp)
        .expect_err("MLS must refuse a duplicate signature key in the tree");
    assert!(
        err.to_string().contains("DuplicateSignatureKey"),
        "the refusal must be the duplicate-key rule, not an unrelated failure: {err}"
    );

    // And the group is untouched: still exactly owner + bob, one leaf each.
    assert_eq!(
        owner
            .find_leaves_by_identity(&channel, &owner.identity_actor_id())
            .len(),
        1,
        "the succeeded credential stays single-seated"
    );
    assert_eq!(owner.group_members(&channel).len(), 2);
}
