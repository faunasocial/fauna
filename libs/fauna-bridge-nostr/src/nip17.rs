//! NIP-17 private direct messages using gift-wrapped events.
//!
//! Three layers of wrapping:
//! 1. **Rumor** (kind 14) — unsigned event containing DM content + `p` tag
//! 2. **Seal** (kind 13) — rumor JSON encrypted with NIP-44, signed by sender
//! 3. **Gift Wrap** (kind 1059) — seal JSON encrypted with NIP-44 using a
//!    random throwaway key, `p` tag for recipient

use anyhow::{Context, Result, bail};

use crate::nip44::{nip44_decrypt, nip44_encrypt};
use crate::signing::Keypair;
use crate::types::{Event, Tag, UnsignedEvent, kind};

/// A decrypted gift-wrapped direct message.
#[derive(Debug, Clone, PartialEq)]
pub struct GiftWrappedDm {
    /// Hex-encoded x-only public key of the sender.
    pub sender_pubkey: String,
    /// Hex-encoded x-only public key of the recipient.
    pub recipient_pubkey: String,
    /// Plaintext message content.
    pub content: String,
    /// Unix timestamp from the rumor (the real timestamp).
    pub created_at: u64,
}

/// Unwrap a NIP-17 gift-wrapped DM event.
///
/// Decrypts the three layers (gift wrap -> seal -> rumor) and extracts
/// the original DM content.
pub fn unwrap_gift_wrap(our_secret: &[u8; 32], event: &Event) -> Result<GiftWrappedDm> {
    // Step 1: Verify outer event is kind 1059
    if event.kind != kind::GIFT_WRAP {
        bail!(
            "expected gift wrap event (kind {}), got kind {}",
            kind::GIFT_WRAP,
            event.kind
        );
    }

    // Step 2: Decrypt the gift wrap content using our secret + throwaway pubkey
    let throwaway_pubkey =
        hex_to_32bytes(&event.pubkey).context("invalid throwaway pubkey in gift wrap")?;
    let seal_json = nip44_decrypt(our_secret, &throwaway_pubkey, &event.content)
        .context("failed to decrypt gift wrap layer")?;

    // Step 3: Parse the seal (kind 13)
    let seal: Event =
        serde_json::from_str(&seal_json).context("failed to parse seal event JSON")?;
    if seal.kind != kind::SEAL {
        bail!(
            "expected seal event (kind {}), got kind {}",
            kind::SEAL,
            seal.kind
        );
    }

    // Step 4: Decrypt the seal content using our secret + sender's real pubkey
    let sender_pubkey_bytes =
        hex_to_32bytes(&seal.pubkey).context("invalid sender pubkey in seal")?;
    let rumor_json = nip44_decrypt(our_secret, &sender_pubkey_bytes, &seal.content)
        .context("failed to decrypt seal layer")?;

    // Step 5: Parse the rumor (kind 14)
    let rumor: serde_json::Value =
        serde_json::from_str(&rumor_json).context("failed to parse rumor JSON")?;

    let rumor_kind = rumor.get("kind").and_then(|k| k.as_u64()).unwrap_or(0);
    if rumor_kind != kind::PRIVATE_DM {
        bail!(
            "expected private DM rumor (kind {}), got kind {}",
            kind::PRIVATE_DM,
            rumor_kind
        );
    }

    // Step 6: Extract fields from the rumor
    let content = rumor
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    let created_at = rumor
        .get("created_at")
        .and_then(|t| t.as_u64())
        .unwrap_or(0);

    // Find the recipient pubkey from the rumor's `p` tag.
    //
    // **Absent is REFUSED, not defaulted.** A NIP-17 private DM rumor names its
    // recipient — a kind-14 without a `p` tag is malformed, and there is no
    // honest value to stand in for one. This used to `.unwrap_or_default()` to
    // `""`, which travelled all the way to the nest's inbound gate as a peer
    // identity: a guardian-blocked sender who simply omitted the tag had their
    // DM stored keyed on the empty string, because no verdict is ever recorded
    // against `""`.
    // The caller's own discriminator is fixed too, but refusing here closes the
    // whole class one layer down: no consumer of this type ever sees a
    // recipient the rumor did not actually name.
    //
    // ⚠ This is deliberately NOT a *validity* check on the value — a `p` tag
    // that is present but oddly spelled is still the sender's choice, and
    // canonicalizing or trusting it is the caller's business. Absence is the
    // only thing decided here, because absence has no caller-side reading at
    // all.
    let recipient_pubkey = rumor
        .get("tags")
        .and_then(|t| t.as_array())
        .and_then(|tags| {
            tags.iter().find_map(|tag| {
                let arr = tag.as_array()?;
                let name = arr.first()?.as_str()?;
                if name == "p" {
                    arr.get(1)?.as_str().map(|s| s.to_string())
                } else {
                    None
                }
            })
        })
        .context("NIP-17 rumor has no `p` tag naming its recipient")?;

    Ok(GiftWrappedDm {
        sender_pubkey: seal.pubkey,
        recipient_pubkey,
        content,
        created_at,
    })
}

/// Wrap a DM as a NIP-17 gift-wrapped event.
///
/// Builds three layers:
/// 1. Rumor (kind 14) — unsigned, contains the plaintext
/// 2. Seal (kind 13) — encrypts rumor with NIP-44, signed by sender
/// 3. Gift Wrap (kind 1059) — encrypts seal with NIP-44 using throwaway key
pub fn wrap_dm(
    sender_keypair: &Keypair,
    recipient_pubkey: &[u8; 32],
    content: &str,
) -> Result<Event> {
    // The honest spelling: the rumor names the same recipient the outer wrap
    // does, lowercase hex. Every production sender does this.
    let recipient_hex = hex::encode(recipient_pubkey);
    wrap_dm_inner(
        sender_keypair,
        recipient_pubkey,
        content,
        Some(recipient_hex),
        None,
    )
}

/// [`wrap_dm`], but the caller chooses what the **rumor**'s `p` tag says —
/// including saying nothing at all (`None`).
///
/// **This exists to forge exactly what a hostile sender controls.** The rumor
/// is plaintext inside the seal and is signed by nobody, so a real sender can
/// put any string there, or omit the tag; `wrap_dm` cannot express that, which
/// is precisely why the inbound gate's use of that field went untested and
/// shipped a bypass. A test that must
/// prove the gate does not trust it needs to be able to lie here.
///
/// Behind `test-helpers`, so it is compiled out of every release artifact
/// (e2e conventions point 15). Never call it from production code: the outer
/// wrap still addresses `recipient_pubkey` honestly, so a mismatched rumor is
/// a malformed DM by construction.
#[cfg(any(test, feature = "test-helpers"))]
pub fn wrap_dm_with_rumor_recipient(
    sender_keypair: &Keypair,
    recipient_pubkey: &[u8; 32],
    content: &str,
    rumor_recipient: Option<&str>,
) -> Result<Event> {
    wrap_dm_inner(
        sender_keypair,
        recipient_pubkey,
        content,
        rumor_recipient.map(|s| s.to_string()),
        None,
    )
}

/// [`wrap_dm`], but the caller chooses the **rumor**'s `created_at` — the
/// field an inbound gift wrap's honest `p`-tagged recipient (`wrap_dm`) still
/// leaves to `randomize_timestamp`.
///
/// **This exists to forge exactly what a hostile sender controls.** The
/// rumor's `created_at` is plaintext inside the seal, signed by nobody, so a
/// real sender can set it to anything — `0`, `i64::MAX` as a `u64`, or a value
/// past `u64::MAX >> 1` that would wrap negative on a naive `as i64` cast.
/// `wrap_dm` cannot express any of that (it always randomizes the outer AND
/// inner timestamps per NIP-59); a test proving the ingest path does not
/// trust this field needs to be able to lie here.
///
/// Behind `test-helpers`, so it is compiled out of every release artifact
/// (e2e conventions point 15). Never call it from production code.
#[cfg(any(test, feature = "test-helpers"))]
pub fn wrap_dm_with_rumor_created_at(
    sender_keypair: &Keypair,
    recipient_pubkey: &[u8; 32],
    content: &str,
    rumor_created_at: u64,
) -> Result<Event> {
    let recipient_hex = hex::encode(recipient_pubkey);
    wrap_dm_inner(
        sender_keypair,
        recipient_pubkey,
        content,
        Some(recipient_hex),
        Some(rumor_created_at),
    )
}

/// The one gift-wrap construction. `rumor_recipient` is the rumor's `p` tag
/// value, or `None` to omit the tag entirely. `rumor_created_at_override`
/// forges the rumor's `created_at`, or `None` for the honest NIP-59
/// randomized default.
fn wrap_dm_inner(
    sender_keypair: &Keypair,
    recipient_pubkey: &[u8; 32],
    content: &str,
    rumor_recipient: Option<String>,
    rumor_created_at_override: Option<u64>,
) -> Result<Event> {
    let recipient_hex = hex::encode(recipient_pubkey);
    let now = fauna_core::data::Timestamp::now_secs() as u64;

    // Step 1: Build the rumor (kind 14) — unsigned
    // Randomize timestamp within ±48h to obfuscate timing, unless the caller
    // (a test) forged an exact value to drive.
    let rumor_created_at = match rumor_created_at_override {
        Some(t) => t,
        None => randomize_timestamp(now)?,
    };

    let rumor_tags: Vec<Vec<String>> = match &rumor_recipient {
        Some(p) => vec![vec!["p".to_string(), p.clone()]],
        None => vec![],
    };
    let rumor = serde_json::json!({
        "pubkey": sender_keypair.public_key_hex(),
        "created_at": rumor_created_at,
        "kind": kind::PRIVATE_DM,
        "tags": rumor_tags,
        "content": content,
    });
    let rumor_json = serde_json::to_string(&rumor)?;

    // Step 2: Create the seal (kind 13)
    // Encrypt the rumor JSON with NIP-44 using sender's secret + recipient's pubkey
    let encrypted_rumor = nip44_encrypt(
        &sender_keypair.secret_bytes(),
        recipient_pubkey,
        &rumor_json,
    )
    .context("failed to encrypt rumor for seal")?;

    // Sign the seal with a randomized timestamp too
    let seal_created_at = randomize_timestamp(now)?;
    let seal_unsigned = UnsignedEvent {
        pubkey: sender_keypair.public_key_bytes(),
        created_at: seal_created_at,
        kind: kind::SEAL,
        tags: vec![],
        content: encrypted_rumor,
    };
    let seal = sender_keypair.sign_event(seal_unsigned);
    let seal_json = serde_json::to_string(&seal)?;

    // Step 3: Create the gift wrap (kind 1059) with throwaway key
    let throwaway = Keypair::generate();
    let encrypted_seal = nip44_encrypt(&throwaway.secret_bytes(), recipient_pubkey, &seal_json)
        .context("failed to encrypt seal for gift wrap")?;

    let wrap_created_at = randomize_timestamp(now)?;
    let wrap_unsigned = UnsignedEvent {
        pubkey: throwaway.public_key_bytes(),
        created_at: wrap_created_at,
        kind: kind::GIFT_WRAP,
        tags: vec![Tag::new(vec!["p".into(), recipient_hex])],
        content: encrypted_seal,
    };
    let gift_wrap = throwaway.sign_event(wrap_unsigned);

    Ok(gift_wrap)
}

/// Decode a hex string into a 32-byte array.
fn hex_to_32bytes(hex_str: &str) -> Result<[u8; 32]> {
    let bytes = hex::decode(hex_str).context("invalid hex")?;
    if bytes.len() != 32 {
        bail!("expected 32 bytes, got {}", bytes.len());
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

/// Randomize a Unix timestamp by ±48 hours (±172800 seconds).
fn randomize_timestamp(base: u64) -> Result<u64> {
    let mut buf = [0u8; 4];
    getrandom::fill(&mut buf).context("generate random for timestamp")?;
    let random_u32 = u32::from_le_bytes(buf);
    // Map to range [0, 2 * 172800) then subtract 172800 for ±48h
    let offset = (random_u32 as u64 % (2 * 172800)) as i64 - 172800;
    Ok((base as i64 + offset) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signing::Keypair;

    #[test]
    fn wrap_and_unwrap_roundtrip() {
        let sender = Keypair::generate();
        let recipient = Keypair::generate();
        let content = "Hello, this is a private DM!";

        let gift_wrap = wrap_dm(&sender, &recipient.public_key_bytes(), content).unwrap();
        let dm = unwrap_gift_wrap(&recipient.secret_bytes(), &gift_wrap).unwrap();

        assert_eq!(dm.content, content);
        assert_eq!(dm.sender_pubkey, sender.public_key_hex());
        assert_eq!(dm.recipient_pubkey, recipient.public_key_hex());
    }

    #[test]
    fn unwrap_wrong_key_fails() {
        let sender = Keypair::generate();
        let recipient = Keypair::generate();
        let third_party = Keypair::generate();

        let gift_wrap = wrap_dm(&sender, &recipient.public_key_bytes(), "secret message").unwrap();

        // A third party should not be able to unwrap
        let result = unwrap_gift_wrap(&third_party.secret_bytes(), &gift_wrap);
        assert!(
            result.is_err(),
            "third party should not be able to unwrap DM"
        );
    }

    #[test]
    fn gift_wrap_has_correct_structure() {
        let sender = Keypair::generate();
        let recipient = Keypair::generate();

        let gift_wrap = wrap_dm(&sender, &recipient.public_key_bytes(), "test structure").unwrap();

        // Kind must be 1059
        assert_eq!(gift_wrap.kind, kind::GIFT_WRAP);

        // Must have a `p` tag for the recipient
        let p_tag = gift_wrap
            .tags
            .iter()
            .find(|t| t.name() == Some("p"))
            .expect("gift wrap must have a p tag");
        assert_eq!(p_tag.value(), Some(recipient.public_key_hex().as_str()));

        // The pubkey should NOT be the sender's (it's the throwaway key)
        assert_ne!(gift_wrap.pubkey, sender.public_key_hex());
    }

    #[test]
    fn seal_is_kind_13() {
        let sender = Keypair::generate();
        let recipient = Keypair::generate();

        let gift_wrap = wrap_dm(&sender, &recipient.public_key_bytes(), "test seal kind").unwrap();

        // Decrypt the outer layer to get the seal
        let throwaway_pubkey = hex_to_32bytes(&gift_wrap.pubkey).unwrap();
        let seal_json = nip44_decrypt(
            &recipient.secret_bytes(),
            &throwaway_pubkey,
            &gift_wrap.content,
        )
        .unwrap();
        let seal: Event = serde_json::from_str(&seal_json).unwrap();

        assert_eq!(seal.kind, kind::SEAL);
        // Seal's pubkey should be the sender's real pubkey
        assert_eq!(seal.pubkey, sender.public_key_hex());
        // Seal should have no tags (per NIP-17)
        assert!(seal.tags.is_empty());
    }

    #[test]
    fn rumor_content_matches() {
        let sender = Keypair::generate();
        let recipient = Keypair::generate();
        let original_content = "The quick brown fox jumps over the lazy dog";

        let gift_wrap = wrap_dm(&sender, &recipient.public_key_bytes(), original_content).unwrap();

        // Fully unwrap and check content
        let dm = unwrap_gift_wrap(&recipient.secret_bytes(), &gift_wrap).unwrap();
        assert_eq!(dm.content, original_content);

        // Also verify by manually decrypting down to the rumor
        let throwaway_pubkey = hex_to_32bytes(&gift_wrap.pubkey).unwrap();
        let seal_json = nip44_decrypt(
            &recipient.secret_bytes(),
            &throwaway_pubkey,
            &gift_wrap.content,
        )
        .unwrap();
        let seal: Event = serde_json::from_str(&seal_json).unwrap();

        let sender_pubkey = hex_to_32bytes(&seal.pubkey).unwrap();
        let rumor_json =
            nip44_decrypt(&recipient.secret_bytes(), &sender_pubkey, &seal.content).unwrap();
        let rumor: serde_json::Value = serde_json::from_str(&rumor_json).unwrap();

        assert_eq!(rumor["kind"], kind::PRIVATE_DM);
        assert_eq!(rumor["content"].as_str().unwrap(), original_content);
    }
}
