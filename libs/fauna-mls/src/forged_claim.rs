//! Attacker simulation: a commit record whose **sender data names another
//! member's leaf** (`docs/goal/behavior/devices.md` § Cross-device MLS
//! group-state sync, Rule 2 → *An own-leaf claim is the account's replica's to
//! answer*). Test-only (`cfg(test)` or the `test-helpers` feature); no shipped
//! build compiles it.
//!
//! A private message's sender data — the leaf index of whoever sent it — is
//! sealed under a key derived from the epoch's shared sender-data secret, which
//! every member of the epoch holds. So any member can seal sender data naming
//! any leaf. Everything here is written from the public MLS specification
//! (RFC 9420 §6.3 "Encoding and Decoding a Private Message", §6.3.2 "Sender
//! Data Encryption", §8 "ExpandWithLabel") and from the attacker's own
//! exported provider storage: an in-group attacker needs nothing else, and no
//! library internals.

use crate::engine::MlsEngine;

/// A commit record for the attacker's current epoch whose sender data names
/// **the other member's leaf** of a two-member group, over a garbage
/// ciphertext. `honest_commit` is a commit the attacker staged itself (for
/// example [`MlsEngine::self_update`]'s bytes): it supplies the framing, and
/// opening its sender data with the hand-derived key proves that key is the
/// real one and reads the attacker's own leaf index.
///
/// The victim's engine answers these bytes with `MlsError::OwnLeafCommit`.
pub fn forged_other_leaf_commit(attacker: &MlsEngine, honest_commit: &[u8]) -> Vec<u8> {
    let secret = sender_data_secret_of(attacker);
    let mut parts = PrivateMessageParts::parse(honest_commit);
    let (attacker_leaf, _) = parts.open_sender_data(&secret);
    assert!(attacker_leaf < 2, "a two-member group: leaves 0 and 1");
    parts.ciphertext = vec![0xAA; 64];
    parts.forge_sender_data(&secret, 1 - attacker_leaf);
    parts.serialize()
}

/// An MLS variable-size vector: a QUIC-style length prefix, then the bytes.
fn mls_vec(bytes: &[u8]) -> Vec<u8> {
    let n = bytes.len();
    let mut out = if n < 64 {
        vec![n as u8]
    } else {
        assert!(n < 16384, "probe vectors stay under the two-byte prefix");
        vec![0x40 | (n >> 8) as u8, n as u8]
    };
    out.extend_from_slice(bytes);
    out
}

/// Read one variable-size vector at `*at`, advancing it.
fn read_mls_vec(buf: &[u8], at: &mut usize) -> Vec<u8> {
    let first = buf[*at];
    let (len, used) = match first >> 6 {
        0 => (first as usize, 1),
        1 => ((((first & 0x3f) as usize) << 8) | buf[*at + 1] as usize, 2),
        2 => (
            (((first & 0x3f) as usize) << 24)
                | ((buf[*at + 1] as usize) << 16)
                | ((buf[*at + 2] as usize) << 8)
                | buf[*at + 3] as usize,
            4,
        ),
        _ => panic!("invalid vector length prefix"),
    };
    *at += used;
    let out = buf[*at..*at + len].to_vec();
    *at += len;
    out
}

/// RFC 9420 `ExpandWithLabel` over HKDF-SHA256.
fn mls_expand_with_label(secret: &[u8], label: &str, context: &[u8], len: usize) -> Vec<u8> {
    let mut info = (len as u16).to_be_bytes().to_vec();
    info.extend(mls_vec(format!("MLS 1.0 {label}").as_bytes()));
    info.extend(mls_vec(context));
    let mut out = vec![0u8; len];
    hkdf::Hkdf::<sha2::Sha256>::from_prk(secret)
        .expect("a 32-byte secret is a valid PRK")
        .expand(&info, &mut out)
        .expect("expand");
    out
}

/// The fields of a serialized `MLSMessage` carrying a `PrivateMessage`.
struct PrivateMessageParts {
    group_id: Vec<u8>,
    epoch: u64,
    content_type: u8,
    authenticated_data: Vec<u8>,
    encrypted_sender_data: Vec<u8>,
    ciphertext: Vec<u8>,
}

impl PrivateMessageParts {
    fn parse(bytes: &[u8]) -> Self {
        assert_eq!(&bytes[..4], &[0, 1, 0, 2], "mls10 + mls_private_message");
        let mut at = 4;
        let group_id = read_mls_vec(bytes, &mut at);
        let epoch = u64::from_be_bytes(bytes[at..at + 8].try_into().unwrap());
        at += 8;
        let content_type = bytes[at];
        at += 1;
        let authenticated_data = read_mls_vec(bytes, &mut at);
        let encrypted_sender_data = read_mls_vec(bytes, &mut at);
        let ciphertext = read_mls_vec(bytes, &mut at);
        assert_eq!(at, bytes.len(), "the whole message was read");
        Self {
            group_id,
            epoch,
            content_type,
            authenticated_data,
            encrypted_sender_data,
            ciphertext,
        }
    }

    fn serialize(&self) -> Vec<u8> {
        let mut out = vec![0, 1, 0, 2];
        out.extend(mls_vec(&self.group_id));
        out.extend(self.epoch.to_be_bytes());
        out.push(self.content_type);
        out.extend(mls_vec(&self.authenticated_data));
        out.extend(mls_vec(&self.encrypted_sender_data));
        out.extend(mls_vec(&self.ciphertext));
        out
    }

    /// The sender-data AEAD key, nonce and AAD for this message's
    /// ciphertext sample (RFC 9420 §6.3.2).
    fn sender_data_seal(&self, sender_data_secret: &[u8]) -> ([u8; 32], [u8; 12], Vec<u8>) {
        let sample = &self.ciphertext[..self.ciphertext.len().min(32)];
        let key = mls_expand_with_label(sender_data_secret, "key", sample, 32);
        let nonce = mls_expand_with_label(sender_data_secret, "nonce", sample, 12);
        let mut aad = mls_vec(&self.group_id);
        aad.extend(self.epoch.to_be_bytes());
        aad.push(self.content_type);
        (key.try_into().unwrap(), nonce.try_into().unwrap(), aad)
    }

    /// Open the sender data: `(leaf_index, generation)`.
    fn open_sender_data(&self, sender_data_secret: &[u8]) -> (u32, u32) {
        use chacha20poly1305::aead::{Aead, KeyInit, Payload};
        let (key, nonce, aad) = self.sender_data_seal(sender_data_secret);
        let plain = chacha20poly1305::ChaCha20Poly1305::new(&key.into())
            .decrypt(
                &nonce.into(),
                Payload {
                    msg: &self.encrypted_sender_data,
                    aad: &aad,
                },
            )
            .expect("the hand-derived sender-data key opens an honest record");
        (
            u32::from_be_bytes(plain[0..4].try_into().unwrap()),
            u32::from_be_bytes(plain[4..8].try_into().unwrap()),
        )
    }

    /// Seal sender data naming `leaf_index` over this message's ciphertext.
    fn forge_sender_data(&mut self, sender_data_secret: &[u8], leaf_index: u32) {
        use chacha20poly1305::aead::{Aead, KeyInit, Payload};
        let (key, nonce, aad) = self.sender_data_seal(sender_data_secret);
        let mut plain = leaf_index.to_be_bytes().to_vec();
        plain.extend(0u32.to_be_bytes()); // generation
        plain.extend([0u8; 4]); // reuse guard
        self.encrypted_sender_data = chacha20poly1305::ChaCha20Poly1305::new(&key.into())
            .encrypt(
                &nonce.into(),
                Payload {
                    msg: &plain,
                    aad: &aad,
                },
            )
            .expect("seal");
    }
}

/// The current epoch's sender-data secret, read out of a member's own
/// exported provider storage — what any member's device holds at rest.
fn sender_data_secret_of(engine: &MlsEngine) -> Vec<u8> {
    fn find<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
        match v {
            serde_json::Value::Object(map) => map
                .get(key)
                .or_else(|| map.values().find_map(|child| find(child, key))),
            serde_json::Value::Array(items) => items.iter().find_map(|c| find(c, key)),
            _ => None,
        }
    }
    fn bytes32(v: &serde_json::Value) -> Option<Vec<u8>> {
        match v {
            serde_json::Value::Array(items)
                if items.len() == 32 && items.iter().all(|n| n.is_u64()) =>
            {
                Some(items.iter().map(|n| n.as_u64().unwrap() as u8).collect())
            }
            serde_json::Value::Array(items) => items.iter().find_map(bytes32),
            serde_json::Value::Object(map) => map.values().find_map(bytes32),
            _ => None,
        }
    }
    let storage = engine.export_provider_storage();
    let (_, value) = storage
        .iter()
        .find(|(k, _)| k.starts_with(b"MessageSecrets"))
        .expect("the member's storage holds its message secrets");
    let value: serde_json::Value = serde_json::from_slice(value).expect("stored as JSON");
    let current = value
        .get("message_secrets")
        .expect("the current epoch's message secrets");
    let secret = find(current, "sender_data_secret").expect("a sender-data secret");
    bytes32(secret).expect("32 secret bytes")
}
