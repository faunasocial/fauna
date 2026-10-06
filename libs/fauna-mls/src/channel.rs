// Channel abstractions over MLS groups (DM, Group, DeviceSync).

use openmls::prelude::{KeyPackage, MlsMessageOut};

use fauna_core::identity::ActorId;

use crate::engine::MlsEngine;
use crate::error::{MlsError, Result};
use crate::types::ChannelId;

/// A direct-message channel between two actors.
///
/// `DmChannel` is a thin wrapper over [`MlsEngine`] that offers **no add path**:
/// its [`DmChannel::add_member`] always refuses.
///
/// ## That is not an enforced 2-member policy — nothing enforces the count
///
/// This type used to claim it "enforces the 2-member DM policy". It does not,
/// and the distinction matters to anyone building on the sentence. The refusal
/// is the *absence of a method on a wrapper*, not a rule in the group: one call
/// to [`MlsEngine::add_member`] with the same `channel_id` seats a third leaf,
/// and the receive side accepts the resulting commit — `process_commit` applies
/// no roster policy, and nothing anywhere marks a group as "DM-shaped" for a
/// policy to key on.
///
/// Nothing is broken by that today, because this wrapper has **no production
/// consumers** — all 7 apps go through `fauna-conversations`
/// (`ConversationsManager` / `FaunaMlsBackend`), which drives [`MlsEngine`]
/// directly. The 2-member shape of a real DM is a **product convention** upheld
/// by the apps (adding someone to a 1:1 forks a group thread instead), not a
/// protocol invariant. See `docs/goal/behavior/direct-messages.md` § Security
/// Properties, which owns that claim and now states it in those terms.
pub struct DmChannel {
    pub channel_id: ChannelId,
}

impl DmChannel {
    /// Create a DM channel between the engine's identity and a peer.
    ///
    /// The resulting MLS group starts with two members (self + peer); see the
    /// type doc for why "starts with" is the honest tense. The returned
    /// `MlsMessageOut` is the Welcome message the peer must process to join.
    pub fn create(
        engine: &MlsEngine,
        peer_key_package: &KeyPackage,
    ) -> Result<(Self, MlsMessageOut)> {
        let (channel_id, welcome) = engine.create_group(std::slice::from_ref(peer_key_package))?;
        Ok((Self { channel_id }, welcome))
    }

    /// Join a DM from a Welcome message.
    ///
    /// The joiner processes the Welcome and derives the same `ChannelId`.
    pub fn join(engine: &MlsEngine, welcome: MlsMessageOut) -> Result<Self> {
        let channel_id = engine.join_from_welcome(welcome)?;
        Ok(Self { channel_id })
    }

    /// This wrapper offers no add path — always returns a `PolicyViolation`.
    ///
    /// A refusal here does **not** keep a third leaf out of the group: see the
    /// type doc. Callers that need a real guarantee must not infer one from this.
    pub fn add_member(&self, _engine: &MlsEngine, _key_package: &KeyPackage) -> Result<()> {
        Err(MlsError::PolicyViolation(
            "DM channels do not allow adding members".into(),
        ))
    }
}

/// A group channel supporting N members with add/remove semantics.
///
/// `GroupChannel` is a thin wrapper over [`MlsEngine`] that delegates to
/// the engine's group management methods. Unlike [`DmChannel`], it allows
/// adding and removing members after creation.
pub struct GroupChannel {
    pub channel_id: ChannelId,
}

impl GroupChannel {
    /// Create a group channel with the given initial members.
    ///
    /// The creator (the engine's identity) is always included. Each member's
    /// key package is added to the group. Returns the channel and the Welcome
    /// message that all invited members must process to join.
    pub fn create(
        engine: &MlsEngine,
        member_key_packages: &[KeyPackage],
    ) -> Result<(Self, MlsMessageOut)> {
        let (channel_id, welcome) = engine.create_group(member_key_packages)?;
        Ok((Self { channel_id }, welcome))
    }

    /// Join a group from a Welcome message.
    ///
    /// The joiner processes the Welcome and derives the same `ChannelId`.
    pub fn join(engine: &MlsEngine, welcome: MlsMessageOut) -> Result<Self> {
        let channel_id = engine.join_from_welcome(welcome)?;
        Ok(Self { channel_id })
    }

    /// Add a new member to the group.
    ///
    /// Returns the serialized commit bytes (which existing members must
    /// process) and the Welcome message for the new member.
    pub fn add_member(
        &self,
        engine: &MlsEngine,
        key_package: &KeyPackage,
    ) -> Result<(Vec<u8>, MlsMessageOut)> {
        engine.add_member(&self.channel_id, key_package)
    }

    /// Remove a member from the group by their leaf index.
    ///
    /// Returns the serialized commit bytes that remaining members must
    /// process to advance their group state.
    pub fn remove_member(&self, engine: &MlsEngine, leaf_index: u32) -> Result<Vec<u8>> {
        engine.remove_member(&self.channel_id, leaf_index)
    }
}

/// A device-sync channel for syncing state across a single actor's devices.
///
/// `DeviceSyncChannel` creates an MLS group whose membership is restricted to
/// key packages belonging to the same actor. A deterministic channel ID is
/// derived from the actor's identity so that any device can independently
/// compute the expected routing key without prior coordination.
///
/// The struct stores both the deterministic routing-level `channel_id` and the
/// MLS-derived `mls_channel_id` needed for encrypt/decrypt operations.
pub struct DeviceSyncChannel {
    /// Deterministic routing ID derived from the actor identity.
    pub channel_id: ChannelId,
    /// The MLS group-derived channel ID used for encrypt/decrypt operations.
    pub mls_channel_id: ChannelId,
}

impl DeviceSyncChannel {
    /// Derive the expected channel ID for a given actor (deterministic).
    ///
    /// Computed as `blake3::hash(actor_id.0 || b"fauna.devices.v1")`,
    /// matching protocol spec section 10.5.3. Note that the underlying MLS group has
    /// its own random group_id; this deterministic ID is used for routing and
    /// lookup only.
    pub fn expected_channel_id(actor_id: &ActorId) -> ChannelId {
        let mut input = Vec::with_capacity(32 + b"fauna.devices.v1".len());
        input.extend_from_slice(&actor_id.0);
        input.extend_from_slice(b"fauna.devices.v1");
        let hash = blake3::hash(&input);
        ChannelId(*hash.as_bytes())
    }

    /// Create a device sync channel, adding peer device(s).
    ///
    /// The creator's engine identity and the provided device key packages must
    /// all belong to the same actor. Returns the channel and the Welcome
    /// message that the peer devices must process to join.
    pub fn create(
        engine: &MlsEngine,
        actor_id: &ActorId,
        device_key_packages: &[KeyPackage],
    ) -> Result<(Self, MlsMessageOut)> {
        let (mls_channel_id, welcome) = engine.create_group(device_key_packages)?;
        let channel_id = Self::expected_channel_id(actor_id);
        Ok((
            Self {
                channel_id,
                mls_channel_id,
            },
            welcome,
        ))
    }

    /// Join a device sync channel from a Welcome message.
    ///
    /// The joiner processes the Welcome and derives the MLS channel ID
    /// internally. The returned `DeviceSyncChannel` uses the deterministic
    /// routing ID for the given actor.
    pub fn join(engine: &MlsEngine, actor_id: &ActorId, welcome: MlsMessageOut) -> Result<Self> {
        let mls_channel_id = engine.join_from_welcome(welcome)?;
        let channel_id = Self::expected_channel_id(actor_id);
        Ok(Self {
            channel_id,
            mls_channel_id,
        })
    }
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use crate::engine::MlsEngine;
    use fauna_core::identity::ActorKeypair;
    use tempfile::NamedTempFile;

    fn make_engine() -> (MlsEngine, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let identity = ActorKeypair::generate();
        let engine = MlsEngine::new(identity, tmp.path()).unwrap();
        (engine, tmp)
    }

    #[test]
    fn dm_channel_create_and_join() {
        let (alice, _tmp_a) = make_engine();
        let (bob, _tmp_b) = make_engine();

        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (alice_dm, welcome) = DmChannel::create(&alice, &bob_kps[0]).unwrap();
        let bob_dm = DmChannel::join(&bob, welcome).unwrap();

        assert_eq!(alice_dm.channel_id, bob_dm.channel_id);
    }

    #[test]
    fn dm_channel_rejects_third_member() {
        let (alice, _tmp_a) = make_engine();
        let (bob, _tmp_b) = make_engine();
        let (charlie, _tmp_c) = make_engine();

        // Alice creates DM with Bob.
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (alice_dm, _welcome) = DmChannel::create(&alice, &bob_kps[0]).unwrap();

        // Attempt to add Charlie — must fail with PolicyViolation.
        let charlie_kps = charlie.generate_key_packages(1).unwrap();
        let err = alice_dm.add_member(&alice, &charlie_kps[0]).unwrap_err();

        match err {
            MlsError::PolicyViolation(msg) => {
                assert!(msg.contains("DM channels do not allow adding members"));
            }
            other => panic!("expected PolicyViolation, got: {other}"),
        }
    }

    #[test]
    fn group_channel_multi_member() {
        use crate::types::{ChannelMessage, ChannelMessageBody};
        use fauna_core::data::Timestamp;

        let (alice, _tmp_a) = make_engine();
        let (bob, _tmp_b) = make_engine();
        let (charlie, _tmp_c) = make_engine();

        // Alice creates a group with Bob.
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (alice_group, welcome) = GroupChannel::create(&alice, &bob_kps).unwrap();
        let bob_group = GroupChannel::join(&bob, welcome).unwrap();
        assert_eq!(alice_group.channel_id, bob_group.channel_id);
        let channel_id = alice_group.channel_id;

        // Alice adds Charlie.
        let charlie_kps = charlie.generate_key_packages(1).unwrap();
        let (commit_bytes, charlie_welcome) =
            alice_group.add_member(&alice, &charlie_kps[0]).unwrap();

        // Bob processes the add commit so his group state advances.
        bob.process_commit(&channel_id, &commit_bytes).unwrap();

        // Charlie joins via the Welcome.
        let charlie_group = GroupChannel::join(&charlie, charlie_welcome).unwrap();
        assert_eq!(charlie_group.channel_id, channel_id);

        // Alice sends a message — both Bob and Charlie should decrypt it.
        let msg = ChannelMessage {
            sender: alice.identity_actor_id(),
            sequence: 1,
            channel_epoch: 0,
            body: ChannelMessageBody::Text("hello group".into()),
            timestamp: Timestamp::now(),
        };
        let ciphertext = alice.encrypt(&channel_id, &msg).unwrap();

        let bob_decrypted = bob.decrypt(&channel_id, &ciphertext).unwrap();
        match &bob_decrypted.body {
            ChannelMessageBody::Text(text) => assert_eq!(text, "hello group"),
            other => panic!("expected Text body, got: {other:?}"),
        }

        let charlie_decrypted = charlie.decrypt(&channel_id, &ciphertext).unwrap();
        match &charlie_decrypted.body {
            ChannelMessageBody::Text(text) => assert_eq!(text, "hello group"),
            other => panic!("expected Text body, got: {other:?}"),
        }
    }

    #[test]
    fn device_sync_channel_lifecycle() {
        use crate::types::{ChannelMessage, ChannelMessageBody, DeviceSyncMessage};
        use fauna_core::data::{ContentHash, Timestamp};

        // Two "devices" for the same actor — in practice each device has its
        // own Ed25519 keypair (MLS requires distinct signature keys per leaf).
        // The ActorId used for routing is from device 1's keypair.
        let kp1 = ActorKeypair::generate();
        let kp2 = ActorKeypair::generate();
        let actor_id = kp1.actor_id();

        let tmp_d1 = NamedTempFile::new().unwrap();
        let device1 = MlsEngine::new(kp1, tmp_d1.path()).unwrap();

        let tmp_d2 = NamedTempFile::new().unwrap();
        let device2 = MlsEngine::new(kp2, tmp_d2.path()).unwrap();

        // Device 2 generates a key package for Device 1 to use in the invite.
        let d2_kps = device2.generate_key_packages(1).unwrap();

        // Device 1 creates the sync channel.
        let (d1_sync, welcome) = DeviceSyncChannel::create(&device1, &actor_id, &d2_kps).unwrap();

        // Both devices should agree on the deterministic channel ID.
        let expected_id = DeviceSyncChannel::expected_channel_id(&actor_id);
        assert_eq!(d1_sync.channel_id, expected_id);

        // Device 2 joins via Welcome.
        let d2_sync = DeviceSyncChannel::join(&device2, &actor_id, welcome).unwrap();
        assert_eq!(d2_sync.channel_id, expected_id);
        // The MLS-level channel ID should also match between devices.
        assert_eq!(d1_sync.mls_channel_id, d2_sync.mls_channel_id);

        // Device 1 sends a DeviceSyncMessage::BlobAdded inside a ChannelMessage.
        let blob_hash = ContentHash::of_raw(b"test blob data");
        let msg = ChannelMessage {
            sender: actor_id,
            sequence: 1,
            channel_epoch: 0,
            body: ChannelMessageBody::DeviceSync(DeviceSyncMessage::BlobAdded {
                blob_hash,
                path: "/photos/sunset.jpg".into(),
                size_bytes: 2_048_000,
                media_type: "image/jpeg".into(),
                created_at: Timestamp::now(),
            }),
            timestamp: Timestamp::now(),
        };

        let ciphertext = device1.encrypt(&d1_sync.mls_channel_id, &msg).unwrap();

        // Device 2 decrypts and verifies.
        let decrypted = device2
            .decrypt(&d2_sync.mls_channel_id, &ciphertext)
            .unwrap();
        match &decrypted.body {
            ChannelMessageBody::DeviceSync(DeviceSyncMessage::BlobAdded { path, .. }) => {
                assert_eq!(path, "/photos/sunset.jpg");
            }
            other => panic!("expected DeviceSync(BlobAdded), got: {other:?}"),
        }
    }
}
