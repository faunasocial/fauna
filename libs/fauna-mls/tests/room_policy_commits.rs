//! The room commit policy against real openMLS groups
//! (`docs/goal/behavior/conversation-rooms.md` § Roles and authorization →
//! *End-to-end rooms — enforced cryptographically*; module doc
//! `fauna_mls::room_policy`).
//!
//! Every verdict here is asserted on **at least two** members: the property
//! under test is not "a commit is refused" but "every honest member reaches
//! the same verdict on the same bytes" — the only thing that keeps a refused
//! commit from forking the group between the members who applied it and the
//! members who did not.
//!
//! Red-verified 2026-09-09 against a build with the room-policy verdict in
//! `MlsEngine::process_commit` disabled (`room_policy_in(…).filter(|_| false)`):
//! six of the eleven pins went red — every refusal pin (the plain member's
//! Remove merged everywhere, the uninvited Add seated its member, the
//! member's and the admin's over-reaching policy changes installed, the
//! owner became removable, a stranger recorded another's succession) — and
//! the five that only assert permitted paths stayed green, as they must.

use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_mls::engine::MlsEngine;
use fauna_mls::error::MlsError;
use fauna_mls::room_policy::{
    HistoryPolicy, JoinRule, RecordedSuccession, RoomOwnershipOffer, RoomPolicy,
    RoomPolicyExtension, RoomRole,
};
use fauna_mls::types::ChannelId;

fn engine(seed: u8) -> MlsEngine {
    MlsEngine::new_in_memory(ActorKeypair::from_secret([seed; 32])).unwrap()
}

fn keypair(seed: u8) -> ActorKeypair {
    ActorKeypair::from_secret([seed; 32])
}

/// alice owns, bob is an admin, carol a plain member — every member joined
/// from alice's Welcome and holds the policy in its own group context.
struct Room {
    alice: MlsEngine,
    bob: MlsEngine,
    carol: MlsEngine,
    channel: ChannelId,
    policy: RoomPolicyExtension,
}

fn room() -> Room {
    let alice = engine(1);
    let bob = engine(2);
    let carol = engine(3);
    let mut policy = RoomPolicy::initial(alice.identity_actor_id(), Some("the room".into()));
    policy.set_admins([bob.identity_actor_id()]);
    let signed = policy.sign(&keypair(1)).unwrap();
    let ext = RoomPolicyExtension::new(signed);
    let mut kps = bob.generate_key_packages(1).unwrap();
    kps.extend(carol.generate_key_packages(1).unwrap());
    let (channel, welcome) = alice.create_group_with_policy(&kps, &ext).unwrap();
    bob.join_from_welcome(welcome.clone()).unwrap();
    carol.join_from_welcome(welcome).unwrap();
    Room {
        alice,
        bob,
        carol,
        channel,
        policy: ext,
    }
}

fn policy_of(e: &MlsEngine, ch: &ChannelId) -> RoomPolicyExtension {
    e.room_policy(ch)
        .expect("the channel is held")
        .expect("the policy decodes")
}

/// Stage + merge a commit on its author, and hand the bytes back for the
/// other members to process.
fn commit_and_merge(author: &MlsEngine, ch: &ChannelId, bytes: Vec<u8>) -> Vec<u8> {
    author.merge_pending_commit(ch).unwrap();
    bytes
}

fn assert_refused(e: &MlsEngine, ch: &ChannelId, bytes: &[u8], who: &str) {
    let err = match e.process_commit(ch, bytes) {
        Ok(()) => panic!("{who} must refuse the commit"),
        Err(err) => err,
    };
    assert!(
        matches!(err, MlsError::PolicyRefusedCommit { .. }),
        "{who}: expected the typed policy refusal, got {err:?}"
    );
}

#[test]
fn a_room_is_born_with_its_policy_in_every_members_context() {
    let r = room();
    for (name, e) in [("alice", &r.alice), ("bob", &r.bob), ("carol", &r.carol)] {
        let p = policy_of(e, &r.channel);
        assert_eq!(p, r.policy, "{name} holds the creator's policy verbatim");
        assert_eq!(p.role_of(&r.alice.identity_actor_id()), RoomRole::Owner);
        assert_eq!(p.role_of(&r.bob.identity_actor_id()), RoomRole::Admin);
        assert_eq!(p.role_of(&r.carol.identity_actor_id()), RoomRole::Member);
    }
}

#[test]
fn a_plain_members_remove_is_refused_by_every_other_member_and_the_replay_repeats_it() {
    let r = room();
    let bob_leaf = r
        .carol
        .find_leaf_by_identity(&r.channel, &r.bob.identity_actor_id())
        .unwrap();
    // Carol (a member) stages a Remove of bob — the commit a patched client
    // would broadcast.
    let hostile = r.carol.remove_member_staged(&r.channel, bob_leaf).unwrap();

    assert_refused(&r.alice, &r.channel, &hostile, "the owner");
    assert_refused(&r.bob, &r.channel, &hostile, "the admin being removed");
    // The replay (a relaunch's re-walk from seq 0) repeats the verdict from
    // the durable memo, before decryption.
    assert_refused(&r.alice, &r.channel, &hostile, "the owner, on replay");

    // The group did not advance: the owner's next commit, built at the
    // unadvanced epoch, merges for bob; carol must clear her refused pending
    // first, as the rail does on a rejected send.
    r.carol.clear_pending_commit(&r.channel).unwrap();
    let rotate = commit_and_merge(
        &r.alice,
        &r.channel,
        r.alice.self_update(&r.channel).unwrap(),
    );
    r.bob.process_commit(&r.channel, &rotate).unwrap();
    r.carol.process_commit(&r.channel, &rotate).unwrap();
}

#[test]
fn an_admin_removes_a_member_and_the_owner_is_never_removable() {
    let r = room();
    let carol_leaf = r
        .bob
        .find_leaf_by_identity(&r.channel, &r.carol.identity_actor_id())
        .unwrap();
    let remove = commit_and_merge(
        &r.bob,
        &r.channel,
        r.bob.remove_member_staged(&r.channel, carol_leaf).unwrap(),
    );
    r.alice
        .process_commit(&r.channel, &remove)
        .expect("the owner merges an admin's Remove of a member");
    assert_eq!(
        r.alice.group_members(&r.channel).len(),
        2,
        "carol is gone from the owner's roster"
    );

    // The admin cannot remove the owner.
    let alice_leaf = r
        .bob
        .find_leaf_by_identity(&r.channel, &r.alice.identity_actor_id())
        .unwrap();
    let hostile = r.bob.remove_member_staged(&r.channel, alice_leaf).unwrap();
    assert_refused(&r.alice, &r.channel, &hostile, "the owner");
}

#[test]
fn a_member_cannot_add_under_invite_but_may_under_member_invite() {
    let r = room();
    let dave = engine(4);
    let dave_kp = dave.generate_key_packages(1).unwrap().remove(0);

    // Under `invite`, carol's Add is refused by the owner and the admin.
    let (hostile, _welcome) = r.carol.add_member_staged(&r.channel, &dave_kp).unwrap();
    assert_refused(&r.alice, &r.channel, &hostile, "the owner");
    assert_refused(&r.bob, &r.channel, &hostile, "the admin");
    r.carol.clear_pending_commit(&r.channel).unwrap();

    // The owner switches the join rule: a policy change every member merges.
    let mut next = r.policy.clone();
    let mut p = next.signed.policy.clone();
    p.version += 1;
    p.join_rule = JoinRule::MemberInvite;
    next.signed = p.sign(&keypair(1)).unwrap();
    let change = commit_and_merge(
        &r.alice,
        &r.channel,
        r.alice.set_room_policy_staged(&r.channel, &next).unwrap(),
    );
    r.bob.process_commit(&r.channel, &change).unwrap();
    r.carol.process_commit(&r.channel, &change).unwrap();
    assert_eq!(policy_of(&r.carol, &r.channel), next);

    // Now carol's Add is permitted, and dave joins holding the same policy.
    let dave_kp = dave.generate_key_packages(1).unwrap().remove(0);
    let (add, welcome) = r.carol.add_member_staged(&r.channel, &dave_kp).unwrap();
    let add = commit_and_merge(&r.carol, &r.channel, add);
    r.alice.process_commit(&r.channel, &add).unwrap();
    r.bob.process_commit(&r.channel, &add).unwrap();
    dave.join_from_welcome(welcome).unwrap();
    assert_eq!(policy_of(&dave, &r.channel), next);
    assert_eq!(
        policy_of(&dave, &r.channel).role_of(&dave.identity_actor_id()),
        RoomRole::Member
    );
}

#[test]
fn a_policy_change_follows_the_signers_role() {
    let r = room();

    // A member's re-signed rename is refused by both others.
    let mut p = r.policy.signed.policy.clone();
    p.version += 1;
    p.name = Some("mine".into());
    let mut next = r.policy.clone();
    next.signed = p.sign(&keypair(3)).unwrap();
    let hostile = r.carol.set_room_policy_staged(&r.channel, &next).unwrap();
    assert_refused(&r.alice, &r.channel, &hostile, "the owner");
    assert_refused(&r.bob, &r.channel, &hostile, "the admin");
    r.carol.clear_pending_commit(&r.channel).unwrap();

    // The admin renames: permitted everywhere.
    let mut p = r.policy.signed.policy.clone();
    p.version += 1;
    p.name = Some("renamed by an admin".into());
    p.history_policy = HistoryPolicy::Full;
    let mut next = r.policy.clone();
    next.signed = p.sign(&keypair(2)).unwrap();
    let change = commit_and_merge(
        &r.bob,
        &r.channel,
        r.bob.set_room_policy_staged(&r.channel, &next).unwrap(),
    );
    r.alice.process_commit(&r.channel, &change).unwrap();
    r.carol.process_commit(&r.channel, &change).unwrap();
    assert_eq!(
        policy_of(&r.carol, &r.channel)
            .signed
            .policy
            .name
            .as_deref(),
        Some("renamed by an admin")
    );

    // The admin cannot widen the admin set.
    let mut p = next.signed.policy.clone();
    p.version += 1;
    p.set_admins([r.bob.identity_actor_id(), r.carol.identity_actor_id()]);
    let mut widened = next.clone();
    widened.signed = p.sign(&keypair(2)).unwrap();
    let hostile = r.bob.set_room_policy_staged(&r.channel, &widened).unwrap();
    assert_refused(&r.alice, &r.channel, &hostile, "the owner");
    assert_refused(&r.carol, &r.channel, &hostile, "the member");
}

#[test]
fn a_bare_self_update_takeover_merges_from_any_member() {
    let r = room();
    let takeover = commit_and_merge(
        &r.carol,
        &r.channel,
        r.carol.self_update(&r.channel).unwrap(),
    );
    r.alice.process_commit(&r.channel, &takeover).unwrap();
    r.bob.process_commit(&r.channel, &takeover).unwrap();
}

/// Birth-time refusal (property 3 of the module doc): a key package that does
/// not advertise the extension cannot seat in a policy-bearing group, and the
/// refusal is final — no policy-less fallback exists for it (the older-app
/// fallback was a compat remnant, removed 2026-09-25). The policy-less group
/// the plain `create_group` mints is a live shape of its own (a folder-share
/// group), pinned in the second half: open commit processing, and no policy
/// acquirable in place.
#[test]
fn an_unadvertised_key_package_is_refused_by_name_and_a_policy_less_group_stays_open() {
    let alice = engine(1);
    let bob = engine(2);
    let carol = engine(3);
    let blind_bob = bob.generate_policy_blind_key_package_for_test().unwrap();
    assert!(!MlsEngine::key_package_supports_room_policy(&blind_bob));
    assert!(MlsEngine::key_package_supports_room_policy(
        &carol.generate_key_packages(1).unwrap()[0]
    ));

    let policy = RoomPolicyExtension::new(
        RoomPolicy::initial(alice.identity_actor_id(), None)
            .sign(&keypair(1))
            .unwrap(),
    );
    let carol_kp = carol.generate_key_packages(1).unwrap().remove(0);
    let err = alice
        .create_group_with_policy(&[blind_bob, carol_kp.clone()], &policy)
        .expect_err("an unadvertised package cannot be seated in a policy room");
    assert!(
        matches!(err, MlsError::PolicyViolation(_)),
        "refused by name, not as an openMLS capabilities error: {err:?}"
    );

    // A policy-less group (the folder-share shape) is minted through the
    // plain `create_group` from ordinary packages; every member joins and it
    // keeps open commit processing.
    let bob_kp = bob.generate_key_packages(1).unwrap().remove(0);
    let (channel, welcome) = alice.create_group(&[bob_kp, carol_kp]).unwrap();
    bob.join_from_welcome(welcome.clone()).unwrap();
    carol.join_from_welcome(welcome).unwrap();
    assert!(
        alice.room_policy(&channel).is_none(),
        "a policy-less group carries no policy"
    );
    let bob_leaf = carol
        .find_leaf_by_identity(&channel, &bob.identity_actor_id())
        .unwrap();
    let remove = commit_and_merge(
        &carol,
        &channel,
        carol.remove_member_staged(&channel, bob_leaf).unwrap(),
    );
    alice
        .process_commit(&channel, &remove)
        .expect("no policy → any member's proposal commit merges (unchanged)");

    // A policy-less group cannot acquire a policy in place.
    assert!(matches!(
        alice.set_room_policy_staged(&channel, &policy),
        Err(MlsError::PolicyViolation(_))
    ));
}

#[test]
fn a_policy_room_refuses_to_seat_an_unadvertised_leaf_for_every_member_alike() {
    let r = room();
    let dave = engine(4);
    let blind_dave = dave.generate_policy_blind_key_package_for_test().unwrap();
    // The invite-time guard is the engine's own, enforced **before**
    // openMLS's `required_capabilities` check ever runs. No commit is ever produced, so no member can diverge on it.
    let err = r
        .alice
        .add_member_staged(&r.channel, &blind_dave)
        .expect_err("an unadvertised leaf cannot be seated in a policy room");
    assert!(
        matches!(err, MlsError::PolicyViolation(_)),
        "refused by name at the author, not as an openMLS capabilities error: {err:?}"
    );
    let blind_bytes = dave
        .generate_policy_blind_key_package_bytes_for_test()
        .unwrap();
    let err = r
        .bob
        .add_member_staged_from_bytes(&r.channel, &blind_bytes)
        .expect_err("the from-bytes twin refuses the same leaf");
    assert!(matches!(err, MlsError::PolicyViolation(_)), "{err:?}");
}

/// `required_capabilities` is the guard behind property 3 of the module doc
/// (an unadvertised leaf is unseatable), and it is a field of the agreed group
/// context. Two guards stand in front of it.
/// The first is openMLS's own: a GroupContextExtensions proposal whose
/// context carries an extension type its `required_capabilities` does not
/// name is invalid (RFC 9420 § 12.1.7), refused at the author here — a
/// plain member's and the owner's alike, the requirement being nobody's to
/// waive — and validated identically by every member on receipt. The second
/// is the room verdict's, for a client patched past the first: a proposed
/// context that keeps the policy but drops the requirement is as invalid to
/// `staged_commit_facts` as one that drops the policy (pinned on the
/// predicate in `engine.rs`'s unit tests).
#[test]
fn a_group_context_that_drops_the_policys_required_capabilities_cannot_be_authored() {
    let r = room();
    for (who, e) in [("a member", &r.carol), ("the owner", &r.alice)] {
        let err = e
            .set_room_policy_staged_without_requirement_for_test(&r.channel, &r.policy)
            .expect_err("openMLS refuses to author a context whose extension is unrequired");
        assert!(
            matches!(err, MlsError::OpenMls(_)),
            "{who}: refused by the library's own validation, not by a fauna check: {err:?}"
        );
        // Nothing was staged, so the room is exactly where it was.
        assert_eq!(
            policy_of(e, &r.channel),
            r.policy,
            "{who}: the room is untouched"
        );
    }
}

/// The ownership transfer ceremony on real groups (`conversation-rooms.md`
/// § Roles and authorization → *Ownership transfer*): a hand-over signed by
/// the incoming owner alone is refused by every member, one countersigned by
/// an admin likewise; the owner's offer round-trips the channel bytes and
/// verifies for the named member alone; the named member's commit with both
/// signatures lands everywhere, after which the room's authorization root has
/// moved — the new owner removes the old one (a member now) and the old
/// owner's own removal attempt of the new owner is refused.
#[test]
fn an_ownership_transfer_lands_with_both_signatures_and_moves_the_root() {
    let r = room();
    let alice_id = r.alice.identity_actor_id();
    let bob_id = r.bob.identity_actor_id();
    let carol_id = r.carol.identity_actor_id();

    let mut policy = r.policy.signed.policy.clone();
    policy.version += 1;
    policy.owner = carol_id;
    let admins = policy.admins.clone();
    policy.set_admins(admins);

    // Carol signs the hand-over to herself with no countersignature: refused.
    let bare = RoomPolicyExtension::new(r.carol.sign_room_policy(&policy).unwrap());
    let hostile = r.carol.set_room_policy_staged(&r.channel, &bare).unwrap();
    assert_refused(&r.alice, &r.channel, &hostile, "the owner");
    assert_refused(&r.bob, &r.channel, &hostile, "the admin");
    r.carol.clear_pending_commit(&r.channel).unwrap();

    // Countersigned by the admin rather than the owner: refused.
    let mut by_admin = r.carol.sign_room_policy(&policy).unwrap();
    by_admin.countersignature = Some(
        r.bob
            .countersign_ownership_transfer(&r.channel, &policy)
            .unwrap(),
    );
    let hostile = r
        .carol
        .set_room_policy_staged(&r.channel, &RoomPolicyExtension::new(by_admin))
        .unwrap();
    assert_refused(&r.alice, &r.channel, &hostile, "the owner");
    assert_refused(&r.bob, &r.channel, &hostile, "the admin");
    r.carol.clear_pending_commit(&r.channel).unwrap();

    // The owner's offer, as it rides the channel.
    let offer = RoomOwnershipOffer {
        policy: policy.clone(),
        countersignature: r
            .alice
            .countersign_ownership_transfer(&r.channel, &policy)
            .unwrap(),
    };
    let offer = RoomOwnershipOffer::from_bytes(&offer.to_bytes().unwrap()).unwrap();
    offer
        .verify_against(&policy_of(&r.carol, &r.channel), &r.channel.0, &carol_id)
        .expect("addressed to carol");
    assert!(
        offer
            .verify_against(&policy_of(&r.bob, &r.channel), &r.channel.0, &bob_id)
            .is_err(),
        "not bob's to complete"
    );

    // Carol completes it: signed by her, carrying alice's countersignature.
    let mut signed = r.carol.sign_room_policy(&offer.policy).unwrap();
    signed.countersignature = Some(offer.countersignature.clone());
    let next = RoomPolicyExtension {
        signed,
        successions: r.policy.successions.clone(),
    };
    let change = commit_and_merge(
        &r.carol,
        &r.channel,
        r.carol.set_room_policy_staged(&r.channel, &next).unwrap(),
    );
    r.alice.process_commit(&r.channel, &change).unwrap();
    r.bob.process_commit(&r.channel, &change).unwrap();
    for (who, e) in [("alice", &r.alice), ("bob", &r.bob), ("carol", &r.carol)] {
        let p = policy_of(e, &r.channel);
        assert_eq!(
            p.role_of(&carol_id),
            RoomRole::Owner,
            "{who}: carol owns the room"
        );
        assert_eq!(
            p.role_of(&alice_id),
            RoomRole::Member,
            "{who}: alice is a member"
        );
        assert_eq!(
            p.role_of(&bob_id),
            RoomRole::Admin,
            "{who}: bob stays an admin"
        );
    }

    // The root moved: alice cannot remove the new owner …
    let carol_leaf = r
        .alice
        .find_leaf_by_identity(&r.channel, &carol_id)
        .unwrap();
    let hostile = r
        .alice
        .remove_member_staged(&r.channel, carol_leaf)
        .unwrap();
    assert_refused(&r.bob, &r.channel, &hostile, "the admin");
    assert_refused(&r.carol, &r.channel, &hostile, "the new owner");
    r.alice.clear_pending_commit(&r.channel).unwrap();
    // … and carol removes alice, the owner of yesterday, like any member.
    let alice_leaf = r
        .carol
        .find_leaf_by_identity(&r.channel, &alice_id)
        .unwrap();
    let remove = commit_and_merge(
        &r.carol,
        &r.channel,
        r.carol
            .remove_member_staged(&r.channel, alice_leaf)
            .unwrap(),
    );
    r.bob.process_commit(&r.channel, &remove).unwrap();
    assert!(!r.bob.group_members(&r.channel).contains(&alice_id));
}

/// An admin may not record a succession **for the owner**: with the vouch arm asking only who vouches, an admin could
/// append `owner → <an identity it holds>`, resolve to the owner role through
/// the chain, remove the real owner as "its own predecessor", and re-sign the
/// policy onto itself — every honest member computing `Permit` all the way.
/// Pinned on real groups: the plant is refused by the owner and by a plain
/// member, so nothing merges anywhere; and the owner's own vouch for an
/// admin's succession — the outranking case — still lands everywhere.
///
/// Red-verified 2026-09-14, three separate builds:
/// forcing `outranks` in `judge_extension_change` to always return `true`,
/// and separately forcing it to always return `false`, each redden exactly
/// this test and `nobody_records_a_succession_for_someone_else_but_an_owner_may_vouch`
/// — no other pin in this file — proving the check is load-bearing in both
/// directions (a stranger it should refuse, and a legitimate outranking
/// vouch it should permit). Forcing `self_record` (the old leaf's own vouch
/// for itself) to always be `false` instead reddens a disjoint pair,
/// `a_succession_is_recorded_by_the_old_leaf_and_its_two_commits_are_admitted`
/// and `an_owners_successor_holds_the_owner_role_and_governs_the_room`, while
/// this test and the one above stay green — the self-record bypass and the
/// `outranks` check are independently load-bearing.
#[test]
fn an_admin_cannot_plant_a_successor_for_the_owner() {
    let r = room();
    let alice_id = r.alice.identity_actor_id();
    let planted = r
        .policy
        .with_succession(RecordedSuccession {
            old: alice_id,
            new: ActorId([21u8; 32]),
        })
        .unwrap();
    let hostile = r.bob.set_room_policy_staged(&r.channel, &planted).unwrap();
    assert_refused(&r.alice, &r.channel, &hostile, "the owner");
    assert_refused(&r.carol, &r.channel, &hostile, "the member");
    r.bob.clear_pending_commit(&r.channel).unwrap();
    for (who, e) in [("the owner", &r.alice), ("the member", &r.carol)] {
        assert_eq!(
            policy_of(e, &r.channel).role_of(&alice_id),
            RoomRole::Owner,
            "{who} still holds alice as the owner"
        );
    }

    // The owner vouches for the admin's succession: permitted everywhere.
    let bob_id = r.bob.identity_actor_id();
    let bob2_id = ActorId([22u8; 32]);
    let vouched = r
        .policy
        .with_succession(RecordedSuccession {
            old: bob_id,
            new: bob2_id,
        })
        .unwrap();
    let record = commit_and_merge(
        &r.alice,
        &r.channel,
        r.alice
            .set_room_policy_staged(&r.channel, &vouched)
            .unwrap(),
    );
    r.bob.process_commit(&r.channel, &record).unwrap();
    r.carol.process_commit(&r.channel, &record).unwrap();
    assert_eq!(
        policy_of(&r.carol, &r.channel).role_of(&bob2_id),
        RoomRole::Admin,
        "the vouched-for successor inherits the admin role"
    );
}

/// The succession pair under a policy: the old leaf records its succession
/// first, then add-successor by the old leaf and remove-old by the new leaf
/// are admitted although both are the shapes a plain member is refused —
/// and the successor inherits its predecessor's role.
#[test]
fn a_succession_is_recorded_by_the_old_leaf_and_its_two_commits_are_admitted() {
    let r = room();
    let carol2 = engine(13);
    let carol_id = r.carol.identity_actor_id();
    let carol2_id = carol2.identity_actor_id();

    // 1. Carol records her succession — a group-context change by a plain
    //    member, permitted because it names herself as the succeeded one.
    let recorded = r
        .policy
        .with_succession(RecordedSuccession {
            old: carol_id,
            new: carol2_id,
        })
        .unwrap();
    let record = commit_and_merge(
        &r.carol,
        &r.channel,
        r.carol
            .set_room_policy_staged(&r.channel, &recorded)
            .unwrap(),
    );
    r.alice.process_commit(&r.channel, &record).unwrap();
    r.bob.process_commit(&r.channel, &record).unwrap();

    // 2. add-successor, authored by the old leaf, under `invite`.
    let kp = carol2.generate_key_packages(1).unwrap().remove(0);
    let (add, welcome) = r.carol.add_member_staged(&r.channel, &kp).unwrap();
    let add = commit_and_merge(&r.carol, &r.channel, add);
    r.alice.process_commit(&r.channel, &add).unwrap();
    r.bob.process_commit(&r.channel, &add).unwrap();
    carol2.join_from_welcome(welcome).unwrap();
    // The Welcome carried the record, so the newcomer holds the chain too.
    assert_eq!(policy_of(&carol2, &r.channel), recorded);

    // 3. remove-old, authored by the new leaf.
    let old_leaf = carol2.find_leaf_by_identity(&r.channel, &carol_id).unwrap();
    let remove = commit_and_merge(
        &carol2,
        &r.channel,
        carol2.remove_member_staged(&r.channel, old_leaf).unwrap(),
    );
    r.alice.process_commit(&r.channel, &remove).unwrap();
    r.bob.process_commit(&r.channel, &remove).unwrap();
    assert!(!r.alice.group_members(&r.channel).contains(&carol_id));

    // The successor is a plain member like her predecessor: a further Remove
    // by her is refused.
    let bob_leaf = carol2
        .find_leaf_by_identity(&r.channel, &r.bob.identity_actor_id())
        .unwrap();
    let hostile = carol2.remove_member_staged(&r.channel, bob_leaf).unwrap();
    assert_refused(&r.alice, &r.channel, &hostile, "the owner");
}

#[test]
fn an_owners_successor_holds_the_owner_role_and_governs_the_room() {
    let r = room();
    let alice2 = engine(11);
    let alice_id = r.alice.identity_actor_id();
    let alice2_id = alice2.identity_actor_id();

    let recorded = r
        .policy
        .with_succession(RecordedSuccession {
            old: alice_id,
            new: alice2_id,
        })
        .unwrap();
    let record = commit_and_merge(
        &r.alice,
        &r.channel,
        r.alice
            .set_room_policy_staged(&r.channel, &recorded)
            .unwrap(),
    );
    r.bob.process_commit(&r.channel, &record).unwrap();
    r.carol.process_commit(&r.channel, &record).unwrap();

    let kp = alice2.generate_key_packages(1).unwrap().remove(0);
    let (add, welcome) = r.alice.add_member_staged(&r.channel, &kp).unwrap();
    let add = commit_and_merge(&r.alice, &r.channel, add);
    r.bob.process_commit(&r.channel, &add).unwrap();
    r.carol.process_commit(&r.channel, &add).unwrap();
    alice2.join_from_welcome(welcome).unwrap();

    // The old owner, resolved away from the role, is removable by the
    // successor — and by nobody else.
    let old_leaf = alice2.find_leaf_by_identity(&r.channel, &alice_id).unwrap();
    let remove = commit_and_merge(
        &alice2,
        &r.channel,
        alice2.remove_member_staged(&r.channel, old_leaf).unwrap(),
    );
    r.bob.process_commit(&r.channel, &remove).unwrap();
    r.carol.process_commit(&r.channel, &remove).unwrap();

    // The successor governs: it appoints carol an admin (an owner-only act)
    // and re-points the owner field onto itself.
    assert_eq!(
        policy_of(&r.bob, &r.channel).role_of(&alice2_id),
        RoomRole::Owner
    );
    let mut p = recorded.signed.policy.clone();
    p.version += 1;
    p.owner = alice2_id;
    p.set_admins([r.bob.identity_actor_id(), r.carol.identity_actor_id()]);
    let mut next = recorded.clone();
    next.signed = p.sign(&keypair(11)).unwrap();
    let change = commit_and_merge(
        &alice2,
        &r.channel,
        alice2.set_room_policy_staged(&r.channel, &next).unwrap(),
    );
    r.bob.process_commit(&r.channel, &change).unwrap();
    r.carol.process_commit(&r.channel, &change).unwrap();
    let p = policy_of(&r.carol, &r.channel);
    assert_eq!(p.role_of(&r.carol.identity_actor_id()), RoomRole::Admin);
    assert_eq!(p.effective_owner(), alice2_id);

    // And a late joiner learns the whole story from its Welcome alone.
    let erin = engine(5);
    let kp = erin.generate_key_packages(1).unwrap().remove(0);
    let (add, welcome) = r.carol.add_member_staged(&r.channel, &kp).unwrap();
    let add = commit_and_merge(&r.carol, &r.channel, add);
    alice2.process_commit(&r.channel, &add).unwrap();
    r.bob.process_commit(&r.channel, &add).unwrap();
    erin.join_from_welcome(welcome).unwrap();
    assert_eq!(
        policy_of(&erin, &r.channel).role_of(&alice2_id),
        RoomRole::Owner
    );
}

#[test]
fn nobody_records_a_succession_for_someone_else_but_an_owner_may_vouch() {
    let r = room();
    let carol_id = r.carol.identity_actor_id();
    let carol2_id = ActorId([13u8; 32]);
    let recorded = r
        .policy
        .with_succession(RecordedSuccession {
            old: carol_id,
            new: carol2_id,
        })
        .unwrap();

    // Bob (an admin) may vouch — the member-side re-add remedy.
    let record = commit_and_merge(
        &r.bob,
        &r.channel,
        r.bob.set_room_policy_staged(&r.channel, &recorded).unwrap(),
    );
    r.alice.process_commit(&r.channel, &record).unwrap();
    r.carol.process_commit(&r.channel, &record).unwrap();

    // A member cannot record another member's succession.
    let r2 = room();
    let bob_id = r2.bob.identity_actor_id();
    let recorded = r2
        .policy
        .with_succession(RecordedSuccession {
            old: bob_id,
            new: ActorId([12u8; 32]),
        })
        .unwrap();
    let hostile = r2
        .carol
        .set_room_policy_staged(&r2.channel, &recorded)
        .unwrap();
    assert_refused(&r2.alice, &r2.channel, &hostile, "the owner");
    assert_refused(&r2.bob, &r2.channel, &hostile, "the admin");
}
