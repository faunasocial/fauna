//! MLS offline compose — the plaintext intent a send made with no nest
//! becomes (`docs/goal/behavior/devices.md` § Offline compose).
//!
//! The composer arm is [`crate::manager::ConversationsManager`]'s send path:
//! with the app's outbox lent ([`ConversationsManager::set_offline_queue`])
//! and the app's connection word offline (the offline gate's own
//! `is_online`), a send on a thread bound to an MLS group this device holds
//! ([`crate::backend::RailBackend::queueable_channel`]) appends an
//! [`MlsSendIntent`] through the outbox's MLS door instead of failing. Never
//! pre-built ciphertext: sealing at compose time would consume a ratchet
//! generation and bind an epoch the send may never happen in.
//!
//! The drain is [`crate::session::ConversationsSession`]'s
//! `MlsIntentSender`: per intent, catch the channel's log up, then seal at the
//! then-current epoch and append under the intent id
//! ([`crate::backend::RailBackend::send_queued`]). The order and the park
//! policy are the outbox's (`AccountStoreHandle::drain_mls_outbox`); only a
//! process hosting the conversations engine runs it.
//!
//! [`ConversationsManager::set_offline_queue`]: crate::manager::ConversationsManager::set_offline_queue

use serde::{Deserialize, Serialize};

use crate::compose::AttachmentDraft;
use crate::message::MessageId;

/// One queued MLS send: the application payload, exactly as the send path
/// takes it — never an MLS message. Its intent id (the idempotency key) and
/// its per-channel compose sequence are the outbox row's own; it rests
/// plaintext in the device's replica and never syncs (`devices.md` § Offline
/// compose → *The intent is the application payload*).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlsSendIntent {
    /// The channel (hex) the intent drains on — also its FIFO scope's name.
    pub channel_id: String,
    /// The composed body, as `create_message` would seal it.
    pub body: String,
    /// The message this one replies to, when it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<MessageId>,
    /// References into locally staged attachment bytes (by `blob_hash`),
    /// resolved and sealed under the send epoch at drain time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentDraft>,
    /// When the user composed it (ms since the epoch) — the queued copy's
    /// place in the thread until the nest assigns the real one.
    pub composed_at_ms: i64,
}

/// The FIFO scope a channel's intents drain in.
pub fn intent_scope(channel_id_hex: &str) -> String {
    format!("conv:{channel_id_hex}")
}

/// The message id the queued own copy carries until its drain replaces it
/// with the nest-assigned one.
pub fn queued_message_id(intent_id: &[u8; 16]) -> MessageId {
    MessageId(format!("queued:{}", hex::encode(intent_id)))
}

/// The intent id (hex) a queued own copy's message id carries — the inverse
/// of [`queued_message_id`]; `None` for any other message.
pub fn queued_intent_hex(message_id: &MessageId) -> Option<&str> {
    message_id.0.strip_prefix("queued:")
}

/// The intent id a queued own copy's message id carries, as the outbox's key.
pub fn queued_intent_id(message_id: &MessageId) -> Option<[u8; 16]> {
    fauna_core::offline_intent::parse_intent_id(queued_intent_hex(message_id)?)
}

impl MlsSendIntent {
    /// Canonical DAG-CBOR — the bytes the outbox keeps.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        fauna_cbor::encode_canonical(self).map_err(|e| e.to_string())
    }

    /// Read back what [`Self::encode`] wrote.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        fauna_cbor::decode_strict(bytes).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_intent_round_trips_through_its_canonical_bytes() {
        let intent = MlsSendIntent {
            channel_id: "ab".repeat(16),
            body: "hello **there**".into(),
            reply_to: Some(MessageId("conv:x:3".into())),
            attachments: vec![AttachmentDraft {
                blob_hash: "cd".repeat(32),
                filename: "a.png".into(),
                mime_type: "image/png".into(),
                size_bytes: 4,
                is_image: true,
            }],
            composed_at_ms: 1_700_000_000_000,
        };
        let back = MlsSendIntent::decode(&intent.encode().unwrap()).unwrap();
        assert_eq!(back.channel_id, intent.channel_id);
        assert_eq!(back.body, intent.body);
        assert_eq!(back.reply_to, intent.reply_to);
        assert_eq!(back.attachments.len(), 1);
        assert_eq!(back.composed_at_ms, intent.composed_at_ms);
        assert_eq!(intent_scope("ab"), "conv:ab");
        assert_eq!(
            queued_message_id(&[1; 16]).0,
            format!("queued:{}", "01".repeat(16))
        );
    }
}
