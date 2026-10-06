//! MLS encryption manager for the Linux desktop app.
//!
//! Wraps `fauna_mls::engine::MlsEngine` in a thread-safe `Arc` and provides
//! high-level methods for key management, DM channel creation, and
//! message encrypt/decrypt.

use std::path::Path;
use std::sync::Arc;

use fauna_core::data::Timestamp;
use fauna_core::identity::ActorKeypair;
use fauna_mls::engine::MlsEngine;
use fauna_mls::types::{ChannelId, ChannelMessage, ChannelMessageBody};

/// Thread-safe MLS engine wrapper.
///
/// `MlsEngine` is itself `Send + Sync` with interior mutability, so the engine
/// is held behind a plain `Arc` (no redundant outer `Mutex`). [`Self::engine`]
/// hands the same shared instance to the conversations `FaunaMlsBackend`
/// (`crate::conversations::conv_backend`), so legacy MLS DM code and the unified
/// conversations rail drive **one** engine over **one** `mls_state.db` — never
/// two engines racing on the same SQLite file.
pub struct MlsManager {
    engine: Arc<MlsEngine>,
}

impl MlsManager {
    /// Create a new MLS manager with persistent SQLite storage.
    ///
    /// `identity` is the user's Ed25519 keypair (same as auth keypair).
    /// `db_path` is where MLS state is persisted (e.g. `~/.config/fauna/mls_state.db`).
    ///
    /// Returns the typed [`fauna_mls::error::MlsError`] (not `anyhow`, unlike
    /// this module's other methods): the caller (`app.rs`'s `AuthSuccess` arm)
    /// must distinguish [`fauna_mls::error::MlsError::ServedElsewhere`] — the
    /// honest conversations-engine-role refusal, `account-data-plane.md`
    /// § Multi-instance concurrency — from every other engine-init failure, to
    /// surface it on `error-message` rather than log it as a generic error.
    pub fn new(
        identity: ActorKeypair,
        db_path: &Path,
    ) -> Result<Arc<Self>, fauna_mls::error::MlsError> {
        // Ensure parent directory exists.
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| fauna_mls::error::MlsError::Storage(e.to_string()))?;
        }
        let engine = MlsEngine::new(identity, db_path)?;
        Ok(Arc::new(Self {
            engine: Arc::new(engine),
        }))
    }

    /// The shared MLS engine. Handed to the conversations `FaunaMlsBackend` so
    /// the unified conversation rail and legacy MLS DM code share one engine /
    /// one `mls_state.db`.
    pub fn engine(&self) -> Arc<MlsEngine> {
        Arc::clone(&self.engine)
    }

    // `generate_key_packages` (a raw engine mint the linux settings/auto-replenish
    // surfaces used with `client.publish_key_packages_real`) was removed: it
    // minted fresh private init keys on the shared engine WITHOUT ticking the
    // replica autosave, so a later provider swap wiped them and stranded peers.
    // Key-package replenish now flows through the durable
    // `conversations::conv_backend::replenish_key_packages` →
    // `ConversationsManager::ensure_keypackages` surface (notify→autosave), like
    // login and web (`devices.md` § Cross-device MLS group-state sync).

    /// Create a DM channel with a peer using their key package bytes.
    ///
    /// Returns `(channel_id_hex, welcome_bytes)`.
    /// The caller must send `welcome_bytes` to the peer via `POST /api/v1/welcome/{peer}`.
    pub fn create_dm_channel(
        &self,
        peer_key_package_bytes: &[u8],
    ) -> anyhow::Result<(String, Vec<u8>)> {
        let engine = &self.engine;

        // Deserialize + validate the key package using the engine's crypto provider.
        let kp = engine
            .validate_key_package(peer_key_package_bytes)
            .map_err(|e| anyhow::anyhow!("validate key package: {e}"))?;

        let (channel_id, welcome) = engine
            .create_group(&[kp])
            .map_err(|e| anyhow::anyhow!("create group: {e}"))?;

        let welcome_bytes = welcome
            .to_bytes()
            .map_err(|e| anyhow::anyhow!("serialize welcome: {e}"))?;

        Ok((fauna_core::hex32::encode(&channel_id.0), welcome_bytes))
    }

    /// Join a DM channel from a received Welcome message.
    ///
    /// Returns the `channel_id_hex`.
    pub fn process_welcome(&self, welcome_bytes: &[u8]) -> anyhow::Result<String> {
        let engine = &self.engine;
        let channel_id = engine
            .join_from_welcome_bytes(welcome_bytes)
            .map_err(|e| anyhow::anyhow!("process welcome: {e}"))?;
        Ok(fauna_core::hex32::encode(&channel_id.0))
    }

    /// Encrypt a text message for a channel.
    ///
    /// Returns the ciphertext bytes to post to `POST /api/v1/channel/{channel_id}`.
    pub fn encrypt_text(&self, channel_id_hex: &str, text: &str) -> anyhow::Result<Vec<u8>> {
        let channel_id = parse_channel_id(channel_id_hex)?;
        let engine = &self.engine;

        let msg = ChannelMessage {
            sender: engine.identity_actor_id(),
            sequence: 0,
            channel_epoch: engine.current_epoch(&channel_id).unwrap_or(0),
            body: ChannelMessageBody::Text(text.to_string()),
            timestamp: Timestamp::now(),
        };

        let ciphertext = engine
            .encrypt(&channel_id, &msg)
            .map_err(|e| anyhow::anyhow!("encrypt: {e}"))?;
        Ok(ciphertext)
    }

    /// Decrypt a ciphertext received from a channel.
    ///
    /// Returns `(sender_actor_hex, body_text, timestamp_micros)`.
    pub fn decrypt_message(
        &self,
        channel_id_hex: &str,
        ciphertext: &[u8],
    ) -> anyhow::Result<(String, String, u64)> {
        let channel_id = parse_channel_id(channel_id_hex)?;
        let engine = &self.engine;

        let msg = engine
            .decrypt(&channel_id, ciphertext)
            .map_err(|e| anyhow::anyhow!("decrypt: {e}"))?;

        let sender_hex = fauna_core::hex32::encode(&msg.sender.0);
        let body_text = match &msg.body {
            ChannelMessageBody::Text(t) => t.clone(),
            other => format!(
                "[unsupported message type: {:?}]",
                std::mem::discriminant(other)
            ),
        };

        Ok((sender_hex, body_text, msg.timestamp.0))
    }

    /// Process an MLS commit message (membership change).
    pub fn process_commit(&self, channel_id_hex: &str, commit_bytes: &[u8]) -> anyhow::Result<()> {
        let channel_id = parse_channel_id(channel_id_hex)?;
        let engine = &self.engine;
        engine
            .process_commit(&channel_id, commit_bytes)
            .map_err(|e| anyhow::anyhow!("process commit: {e}"))?;
        Ok(())
    }

    /// Check if a DM channel exists for a peer.
    pub fn get_dm_channel(&self, peer_actor_hex: &str) -> anyhow::Result<Option<String>> {
        let peer_bytes = fauna_core::hex32::decode(peer_actor_hex)?;
        let engine = &self.engine;
        match engine.get_dm_channel(&peer_bytes) {
            Ok(Some(ch)) => Ok(Some(fauna_core::hex32::encode(&ch))),
            Ok(None) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("get dm channel: {e}")),
        }
    }

    /// Store DM channel mapping.
    pub fn put_dm_channel(&self, peer_actor_hex: &str, channel_id_hex: &str) -> anyhow::Result<()> {
        let peer_bytes = fauna_core::hex32::decode(peer_actor_hex)?;
        let ch_bytes = fauna_core::hex32::decode(channel_id_hex)?;
        let engine = &self.engine;
        engine
            .put_dm_channel(&peer_bytes, &ch_bytes)
            .map_err(|e| anyhow::anyhow!("put dm channel: {e}"))?;
        Ok(())
    }

    // `create_group_channel` / `add_group_member` (the legacy pre-manager group
    // surface) were removed: caller-less, and `add_group_member`'s optimistic
    // merge-before-send violated `devices.md` § Cross-device MLS group-state
    // sync Rule 1. Group membership flows through the shared
    // `ConversationsManager` (`conv_backend.rs`), whose staged/gated paths obey
    // the rule.

    /// Store a group_id -> channel_id mapping for MLS group channels.
    pub fn put_group_channel(&self, group_id: &str, channel_id_hex: &str) -> anyhow::Result<()> {
        let ch_bytes = fauna_core::hex32::decode(channel_id_hex)?;
        let engine = &self.engine;
        engine
            .put_group_channel(group_id, &ch_bytes)
            .map_err(|e| anyhow::anyhow!("put group channel: {e}"))?;
        Ok(())
    }

    /// Get the MLS channel_id for a group, if one exists.
    pub fn get_group_channel(&self, group_id: &str) -> anyhow::Result<Option<String>> {
        let engine = &self.engine;
        match engine.get_group_channel(group_id) {
            Ok(Some(ch)) => Ok(Some(fauna_core::hex32::encode(&ch))),
            Ok(None) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("get group channel: {e}")),
        }
    }

    /// Get the local actor's hex ID.
    pub fn actor_id_hex(&self) -> String {
        let engine = &self.engine;
        fauna_core::hex32::encode(&engine.identity_actor_id().0)
    }

    /// Check if a channel exists locally.
    pub fn has_channel(&self, channel_id_hex: &str) -> bool {
        if let Ok(cid) = parse_channel_id(channel_id_hex) {
            let engine = &self.engine;
            engine.has_group(&cid)
        } else {
            false
        }
    }
}

fn parse_channel_id(hex_str: &str) -> anyhow::Result<ChannelId> {
    ChannelId::from_hex(hex_str).map_err(Into::into)
}
