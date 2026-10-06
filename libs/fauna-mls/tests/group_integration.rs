//! Full group lifecycle integration test — pure Rust, no HTTP.
//!
//! Exercises: group creation, member addition via Welcome + commit,
//! multi-party encrypted messaging, member removal, and forward secrecy
//! (removed members cannot decrypt new messages).

use fauna_core::data::Timestamp;
use fauna_core::identity::ActorKeypair;
use fauna_mls::channel::GroupChannel;
use fauna_mls::engine::MlsEngine;
use fauna_mls::types::{ChannelMessage, ChannelMessageBody};
use tempfile::NamedTempFile;

fn make_engine() -> (MlsEngine, NamedTempFile) {
    let tmp = NamedTempFile::new().unwrap();
    let identity = ActorKeypair::generate();
    let engine = MlsEngine::new(identity, tmp.path()).unwrap();
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

#[test]
fn group_add_remove_lifecycle() {
    // ------------------------------------------------------------------
    // 1. Alice and Bob create a group
    // ------------------------------------------------------------------
    let (alice, _tmp_a) = make_engine();
    let (bob, _tmp_b) = make_engine();
    let (charlie, _tmp_c) = make_engine();

    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (alice_group, welcome) = GroupChannel::create(&alice, &bob_kps).unwrap();
    let bob_group = GroupChannel::join(&bob, welcome).unwrap();

    assert_eq!(alice_group.channel_id, bob_group.channel_id);
    let channel_id = alice_group.channel_id;

    // ------------------------------------------------------------------
    // 2. Alice adds Charlie (Bob processes commit, Charlie joins via Welcome)
    // ------------------------------------------------------------------
    let charlie_kps = charlie.generate_key_packages(1).unwrap();
    let (add_commit, charlie_welcome) = alice_group.add_member(&alice, &charlie_kps[0]).unwrap();

    // Bob must process the commit to advance his group state.
    bob.process_commit(&channel_id, &add_commit).unwrap();

    // Charlie joins via the Welcome message.
    let charlie_group = GroupChannel::join(&charlie, charlie_welcome).unwrap();
    assert_eq!(charlie_group.channel_id, channel_id);

    // ------------------------------------------------------------------
    // 3. Alice sends message — all three can decrypt
    // ------------------------------------------------------------------
    let msg1 = text_msg(&alice, 1, "hello from alice to the group");
    let ciphertext1 = alice.encrypt(&channel_id, &msg1).unwrap();

    let bob_dec = bob.decrypt(&channel_id, &ciphertext1).unwrap();
    assert_text(&bob_dec, "hello from alice to the group");
    assert_eq!(bob_dec.sender, alice.identity_actor_id());

    let charlie_dec = charlie.decrypt(&channel_id, &ciphertext1).unwrap();
    assert_text(&charlie_dec, "hello from alice to the group");
    assert_eq!(charlie_dec.sender, alice.identity_actor_id());

    // ------------------------------------------------------------------
    // 4. Alice removes Bob (Charlie processes commit)
    // ------------------------------------------------------------------
    // Bob's leaf index is 1 (Alice=0, Bob=1, Charlie=2).
    let remove_commit = alice_group.remove_member(&alice, 1).unwrap();

    // Charlie processes the remove commit so his group state advances.
    charlie.process_commit(&channel_id, &remove_commit).unwrap();

    // ------------------------------------------------------------------
    // 5. Alice sends another message — only Charlie can decrypt, Bob cannot
    // ------------------------------------------------------------------
    let msg2 = text_msg(&alice, 2, "post-removal secret");
    let ciphertext2 = alice.encrypt(&channel_id, &msg2).unwrap();

    // Charlie can still decrypt.
    let charlie_dec2 = charlie.decrypt(&channel_id, &ciphertext2).unwrap();
    assert_text(&charlie_dec2, "post-removal secret");
    assert_eq!(charlie_dec2.sender, alice.identity_actor_id());

    // Bob cannot decrypt — his group state is stale after removal.
    let bob_result = bob.decrypt(&channel_id, &ciphertext2);
    assert!(
        bob_result.is_err(),
        "Bob should not be able to decrypt after removal, but got: {bob_result:?}"
    );
}

#[test]
fn test_find_leaf_by_identity() {
    let alice = ActorKeypair::generate();
    let bob = ActorKeypair::generate();
    let bob_actor_id = bob.actor_id();
    let bob_engine = MlsEngine::new_in_memory(bob).unwrap();
    let kp_bytes = bob_engine.generate_key_packages_bytes(1).unwrap().remove(0);

    let engine = MlsEngine::new_in_memory(alice).unwrap();
    let channel_id = engine.create_solo_group().unwrap();

    // Bob not in group yet
    assert!(
        engine
            .find_leaf_by_identity(&channel_id, &bob_actor_id)
            .is_none()
    );

    // Add Bob using the bytes API
    engine
        .add_member_from_bytes(&channel_id, &kp_bytes)
        .unwrap();

    // Now Bob should be found
    let leaf = engine.find_leaf_by_identity(&channel_id, &bob_actor_id);
    assert!(leaf.is_some());
}

#[test]
fn test_export_subscription_secret() {
    let alice = ActorKeypair::generate();
    let engine = MlsEngine::new_in_memory(alice).unwrap();
    let channel_id = engine.create_solo_group().unwrap();

    let (epoch, secret) = engine.export_subscription_secret(&channel_id).unwrap();
    assert_eq!(epoch, 0); // initial epoch
    assert_eq!(secret.len(), 32);

    // Exporting again should give the same result (deterministic)
    let (epoch2, secret2) = engine.export_subscription_secret(&channel_id).unwrap();
    assert_eq!(epoch, epoch2);
    assert_eq!(secret, secret2);
}
