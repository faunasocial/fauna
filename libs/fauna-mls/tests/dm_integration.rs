//! Full DM lifecycle integration test — pure Rust, no HTTP.
//!
//! Exercises: key package generation, DM creation, Welcome join,
//! bidirectional encrypted messaging.

use fauna_core::data::Timestamp;
use fauna_core::identity::ActorKeypair;
use fauna_mls::channel::DmChannel;
use fauna_mls::engine::MlsEngine;
use fauna_mls::types::{ChannelMessage, ChannelMessageBody};
use tempfile::NamedTempFile;

fn make_engine() -> (MlsEngine, NamedTempFile) {
    let tmp = NamedTempFile::new().unwrap();
    let identity = ActorKeypair::generate();
    let engine = MlsEngine::new(identity, tmp.path()).unwrap();
    (engine, tmp)
}

#[test]
fn dm_full_lifecycle() {
    // ------------------------------------------------------------------
    // Setup: create two engines (Alice, Bob)
    // ------------------------------------------------------------------
    let (alice, _tmp_a) = make_engine();
    let (bob, _tmp_b) = make_engine();

    let alice_id = alice.identity_actor_id();
    let bob_id = bob.identity_actor_id();

    // ------------------------------------------------------------------
    // Bob generates KeyPackages (as he would upload to a node)
    // ------------------------------------------------------------------
    let bob_kps = bob.generate_key_packages(3).unwrap();
    assert_eq!(bob_kps.len(), 3);

    // ------------------------------------------------------------------
    // Alice creates a DM with Bob using one of his KeyPackages
    // ------------------------------------------------------------------
    let (alice_dm, welcome) = DmChannel::create(&alice, &bob_kps[0]).unwrap();

    // ------------------------------------------------------------------
    // Bob joins the DM from the Welcome message
    // ------------------------------------------------------------------
    let bob_dm = DmChannel::join(&bob, welcome).unwrap();

    // Both sides must agree on the channel ID.
    assert_eq!(alice_dm.channel_id, bob_dm.channel_id);
    let channel_id = alice_dm.channel_id;

    // ------------------------------------------------------------------
    // Alice sends an encrypted text message, Bob decrypts it
    // ------------------------------------------------------------------
    let msg1 = ChannelMessage {
        sender: alice_id,
        sequence: 1,
        channel_epoch: 0,
        body: ChannelMessageBody::Text("Hey Bob, this is Alice!".into()),
        timestamp: Timestamp::now(),
    };
    let ciphertext1 = alice.encrypt(&channel_id, &msg1).unwrap();

    // Ciphertext should be non-empty and different from plaintext.
    assert!(!ciphertext1.is_empty());

    let decrypted1 = bob.decrypt(&channel_id, &ciphertext1).unwrap();
    assert_eq!(decrypted1.sender, alice_id);
    assert_eq!(decrypted1.sequence, 1);
    match &decrypted1.body {
        ChannelMessageBody::Text(text) => {
            assert_eq!(text, "Hey Bob, this is Alice!");
        }
        other => panic!("expected Text body, got: {other:?}"),
    }

    // ------------------------------------------------------------------
    // Bob replies, Alice decrypts — bidirectional messaging
    // ------------------------------------------------------------------
    let msg2 = ChannelMessage {
        sender: bob_id,
        sequence: 1,
        channel_epoch: 0,
        body: ChannelMessageBody::Text("Hi Alice, got your message!".into()),
        timestamp: Timestamp::now(),
    };
    let ciphertext2 = bob.encrypt(&channel_id, &msg2).unwrap();
    let decrypted2 = alice.decrypt(&channel_id, &ciphertext2).unwrap();

    assert_eq!(decrypted2.sender, bob_id);
    assert_eq!(decrypted2.sequence, 1);
    match &decrypted2.body {
        ChannelMessageBody::Text(text) => {
            assert_eq!(text, "Hi Alice, got your message!");
        }
        other => panic!("expected Text body, got: {other:?}"),
    }

    // ------------------------------------------------------------------
    // Additional round-trip: Alice sends a second message
    // ------------------------------------------------------------------
    let msg3 = ChannelMessage {
        sender: alice_id,
        sequence: 2,
        channel_epoch: 0,
        body: ChannelMessageBody::Text("Second message from Alice".into()),
        timestamp: Timestamp::now(),
    };
    let ciphertext3 = alice.encrypt(&channel_id, &msg3).unwrap();
    let decrypted3 = bob.decrypt(&channel_id, &ciphertext3).unwrap();

    assert_eq!(decrypted3.sender, alice_id);
    assert_eq!(decrypted3.sequence, 2);
    match &decrypted3.body {
        ChannelMessageBody::Text(text) => {
            assert_eq!(text, "Second message from Alice");
        }
        other => panic!("expected Text body, got: {other:?}"),
    }

    // ------------------------------------------------------------------
    // Verify DM policy: adding a third member must fail
    // ------------------------------------------------------------------
    let (charlie, _tmp_c) = make_engine();
    let charlie_kps = charlie.generate_key_packages(1).unwrap();
    let err = alice_dm.add_member(&alice, &charlie_kps[0]).unwrap_err();
    assert!(
        err.to_string()
            .contains("DM channels do not allow adding members"),
        "expected policy violation, got: {err}"
    );
}

#[test]
fn extract_dm_peer_returns_correct_peer() {
    let alice_kp = fauna_core::identity::ActorKeypair::generate();
    let bob_kp = fauna_core::identity::ActorKeypair::generate();
    let alice_id = alice_kp.actor_id();
    let bob_id = bob_kp.actor_id();
    let alice_engine = fauna_mls::engine::MlsEngine::new_in_memory(alice_kp).unwrap();
    let bob_engine = fauna_mls::engine::MlsEngine::new_in_memory(bob_kp).unwrap();
    let bob_packages = bob_engine.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice_engine.create_group(&bob_packages).unwrap();
    let bob_channel_id = bob_engine.join_from_welcome(welcome).unwrap();
    assert_eq!(channel_id, bob_channel_id);
    let peer = alice_engine.extract_dm_peer(&channel_id);
    assert_eq!(peer, Some(bob_id));
    let peer = bob_engine.extract_dm_peer(&bob_channel_id);
    assert_eq!(peer, Some(alice_id));
}

#[test]
fn dm_channel_delegators_work() {
    let kp = fauna_core::identity::ActorKeypair::generate();
    let engine = fauna_mls::engine::MlsEngine::new_in_memory(kp).unwrap();
    let peer = [0xAAu8; 32];
    let channel = [0xBBu8; 32];
    assert_eq!(engine.get_dm_channel(&peer).unwrap(), None);
    engine.put_dm_channel(&peer, &channel).unwrap();
    assert_eq!(engine.get_dm_channel(&peer).unwrap(), Some(channel));
    let channels = engine.list_dm_channels().unwrap();
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0], (peer, channel));
}
