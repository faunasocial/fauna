use bech32::{Bech32, Hrp};

const NPUB_HRP: Hrp = Hrp::parse_unchecked("npub");
const NSEC_HRP: Hrp = Hrp::parse_unchecked("nsec");
const NOTE_HRP: Hrp = Hrp::parse_unchecked("note");

/// Encode a 32-byte public key as an npub bech32 string.
pub fn encode_npub(pubkey: &[u8; 32]) -> String {
    bech32::encode::<Bech32>(NPUB_HRP, pubkey).expect("bech32 encoding should not fail")
}

/// Decode an npub bech32 string to a 32-byte public key.
pub fn decode_npub(s: &str) -> anyhow::Result<[u8; 32]> {
    let (hrp, data) = bech32::decode(s).map_err(|e| anyhow::anyhow!("invalid bech32: {e}"))?;
    if hrp != NPUB_HRP {
        anyhow::bail!("expected npub prefix, got {hrp}");
    }
    if data.len() != 32 {
        anyhow::bail!("expected 32 bytes, got {}", data.len());
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&data);
    Ok(arr)
}

/// Encode a 32-byte secret key as an nsec bech32 string.
pub fn encode_nsec(secret: &[u8; 32]) -> String {
    bech32::encode::<Bech32>(NSEC_HRP, secret).expect("bech32 encoding should not fail")
}

/// Decode an nsec bech32 string to a 32-byte secret key.
pub fn decode_nsec(s: &str) -> anyhow::Result<[u8; 32]> {
    let (hrp, data) = bech32::decode(s).map_err(|e| anyhow::anyhow!("invalid bech32: {e}"))?;
    if hrp != NSEC_HRP {
        anyhow::bail!("expected nsec prefix, got {hrp}");
    }
    if data.len() != 32 {
        anyhow::bail!("expected 32 bytes, got {}", data.len());
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&data);
    Ok(arr)
}

/// Encode a 32-byte event ID as a note bech32 string.
pub fn encode_note(id: &[u8; 32]) -> String {
    bech32::encode::<Bech32>(NOTE_HRP, id).expect("bech32 encoding should not fail")
}

/// Decode a note bech32 string to a 32-byte event ID.
pub fn decode_note(s: &str) -> anyhow::Result<[u8; 32]> {
    let (hrp, data) = bech32::decode(s).map_err(|e| anyhow::anyhow!("invalid bech32: {e}"))?;
    if hrp != NOTE_HRP {
        anyhow::bail!("expected note prefix, got {hrp}");
    }
    if data.len() != 32 {
        anyhow::bail!("expected 32 bytes, got {}", data.len());
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&data);
    Ok(arr)
}

const NEVENT_HRP: Hrp = Hrp::parse_unchecked("nevent");

/// NIP-19 TLV types this crate reads and writes on an `nevent`.
const TLV_SPECIAL: u8 = 0;
const TLV_AUTHOR: u8 = 2;

/// Encode an event id (+ optional author pubkey) as an `nevent` bech32 string
/// — the TLV form NIP-21 `nostr:` references use, which clients render as an
/// embedded note. No relay hint is written: the caller's relay set is the
/// user's, and a hint naming one would leak it into public content.
pub fn encode_nevent(id: &[u8; 32], author: Option<&[u8; 32]>) -> String {
    let mut data = Vec::with_capacity(68);
    data.extend_from_slice(&[TLV_SPECIAL, 32]);
    data.extend_from_slice(id);
    if let Some(author) = author {
        data.extend_from_slice(&[TLV_AUTHOR, 32]);
        data.extend_from_slice(author);
    }
    bech32::encode::<Bech32>(NEVENT_HRP, &data).expect("bech32 encoding should not fail")
}

/// Decode an `nevent` bech32 string to its event id and optional author.
/// Unknown TLV types (relay hints, kind) are skipped, per NIP-19.
pub fn decode_nevent(s: &str) -> anyhow::Result<([u8; 32], Option<[u8; 32]>)> {
    let (hrp, data) = bech32::decode(s).map_err(|e| anyhow::anyhow!("invalid bech32: {e}"))?;
    if hrp != NEVENT_HRP {
        anyhow::bail!("expected nevent prefix, got {hrp}");
    }
    let (mut id, mut author) = (None, None);
    let mut rest = data.as_slice();
    while let [t, len, tail @ ..] = rest {
        let len = *len as usize;
        if tail.len() < len {
            anyhow::bail!("truncated nevent TLV");
        }
        let (value, next) = tail.split_at(len);
        match (*t, <[u8; 32]>::try_from(value)) {
            (TLV_SPECIAL, Ok(v)) => id = Some(v),
            (TLV_AUTHOR, Ok(v)) => author = Some(v),
            (TLV_SPECIAL | TLV_AUTHOR, Err(_)) => anyhow::bail!("nevent TLV {t} is not 32 bytes"),
            _ => {}
        }
        rest = next;
    }
    if !rest.is_empty() {
        anyhow::bail!("trailing byte in nevent TLV");
    }
    Ok((
        id.ok_or_else(|| anyhow::anyhow!("nevent has no event id"))?,
        author,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npub_roundtrip() {
        let pubkey = [42u8; 32];
        let encoded = encode_npub(&pubkey);
        assert!(encoded.starts_with("npub1"));
        let decoded = decode_npub(&encoded).unwrap();
        assert_eq!(pubkey, decoded);
    }

    #[test]
    fn nsec_roundtrip() {
        let secret = [7u8; 32];
        let encoded = encode_nsec(&secret);
        assert!(encoded.starts_with("nsec1"));
        let decoded = decode_nsec(&encoded).unwrap();
        assert_eq!(secret, decoded);
    }

    #[test]
    fn note_roundtrip() {
        let id = [99u8; 32];
        let encoded = encode_note(&id);
        assert!(encoded.starts_with("note1"));
        let decoded = decode_note(&encoded).unwrap();
        assert_eq!(id, decoded);
    }

    /// The NIP-21 `nostr:nevent1…` reference a derived quote appends
    /// (`nostr.md` § Replying to and quoting a nostr note) — TLV type 0 the
    /// event id, type 2 the author; decodes back to both.
    #[test]
    fn nevent_roundtrip_carries_id_and_author() {
        let id = [0xabu8; 32];
        let author = [0x11u8; 32];
        let encoded = encode_nevent(&id, Some(&author));
        assert!(encoded.starts_with("nevent1"), "{encoded}");
        assert_eq!(decode_nevent(&encoded).unwrap(), (id, Some(author)));

        let bare = encode_nevent(&id, None);
        assert_eq!(decode_nevent(&bare).unwrap(), (id, None));
        assert!(decode_nevent(&encode_note(&id)).is_err());
    }

    /// NIP-19's worked TLV layout, byte for byte: `00 20 <id>` then
    /// `02 20 <author>` — so a stock client's parser reads the same fields.
    #[test]
    fn nevent_tlv_layout_is_the_nip19_one() {
        let id = [1u8; 32];
        let author = [2u8; 32];
        let (_, data) = bech32::decode(&encode_nevent(&id, Some(&author))).unwrap();
        let mut want = vec![0u8, 32];
        want.extend_from_slice(&id);
        want.extend_from_slice(&[2, 32]);
        want.extend_from_slice(&author);
        assert_eq!(data, want);
    }

    #[test]
    fn wrong_prefix_errors() {
        let pubkey = [1u8; 32];
        let npub = encode_npub(&pubkey);
        assert!(decode_nsec(&npub).is_err());
        assert!(decode_note(&npub).is_err());
    }

    #[test]
    fn invalid_bech32_errors() {
        assert!(decode_npub("not-a-bech32-string").is_err());
        assert!(decode_nsec("").is_err());
        assert!(decode_note("note1invalid").is_err());
    }

    #[test]
    fn signing_keypair_npub_roundtrip() {
        use crate::signing::Keypair;
        let kp = Keypair::generate();
        let npub = encode_npub(&kp.public_key_bytes());
        let decoded = decode_npub(&npub).unwrap();
        assert_eq!(kp.public_key_bytes(), decoded);
    }
}
