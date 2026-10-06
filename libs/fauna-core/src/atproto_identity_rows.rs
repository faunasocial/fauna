//! The `fauna.state.atproto-identity` plane rows' key grammar
//! (`config-dissolution.md` § Phases and gates → *Bounded rows*; the kinds
//! table's `fauna.state.atproto-identity` row owns the ruling).
//!
//! **One row per entity, never one per account.** [`AtprotoIdentityConfig`]
//! is four lists that each grow with use — a rotation key per mint
//! (fresh-key-per-mint), a consent per retired DID, an intent per contest, a
//! DID per box claim — and every rotation key carries a P-256 scalar, so a
//! per-account row has no size bound. Each list element is its own row, keyed
//! by the element's identity (the pair [`AtprotoIdentityConfig::merge`]
//! deduplicates on):
//!
//! | key | value |
//! |---|---|
//! | `key/<pubkey_did_key>` | one [`AtprotoRotationKey`] |
//! | `consent/<did>` | one tombstone-consent DID (a text string) |
//! | `intent/<did>/<contested_op_cid>` | one [`AtprotoContestIntent`] |
//! | `named/<did>` | one nest-named DID (a text string) |
//!
//! A row's value must name its own key. [`AtprotoIdentityConfig`] is the READ
//! fold over the account's rows ([`AtprotoIdentityConfig::fold_row`]) and
//! [`AtprotoIdentityConfig::rows`] the split; the plane arm is the per-row
//! half of the composite merge ([`AtprotoIdentityRecord::merge`]). Nothing is
//! ever removed — the composite is a pure union — so the kind has no
//! tombstone.
//!
//! **Decode posture: an unknown field is refused, not stripped**
//! (`config-dissolution.md` P4 — the CrdtPerField posture). The value types
//! stay tolerant; the row decode instead requires the value to re-encode to its
//! own bytes, which a value carrying a field this build does not know (or any
//! non-canonical spelling) cannot.

use crate::data::{AtprotoContestIntent, AtprotoIdentityConfig, AtprotoRotationKey};
use crate::error::{Error, Result};

/// Key prefix of a rotation-key row: `key/<pubkey_did_key>`.
pub const ROTATION_KEY_PREFIX: &str = "key/";
/// Key prefix of a tombstone-consent row: `consent/<did>`.
pub const TOMBSTONE_CONSENT_PREFIX: &str = "consent/";
/// Key prefix of a contest-intent row: `intent/<did>/<contested_op_cid>`.
pub const CONTEST_INTENT_PREFIX: &str = "intent/";
/// Key prefix of a nest-named-DID row: `named/<did>`.
pub const NEST_NAMED_DID_PREFIX: &str = "named/";

/// One plane row's value: the list element its key names.
#[derive(Debug, Clone, PartialEq)]
pub enum AtprotoIdentityRecord {
    RotationKey(AtprotoRotationKey),
    TombstoneConsent(String),
    ContestIntent(AtprotoContestIntent),
    NestNamedDid(String),
}

/// A key segment: non-empty and free of the `/` separator, so one element
/// has exactly one key and every key parses one way. A DID, a `did:key` and
/// a CID all qualify.
fn segment(what: &str, s: &str) -> Result<()> {
    if s.is_empty() || s.contains('/') {
        return Err(Error::Encoding(format!(
            "atproto identity {what} cannot key a row: {s:?}"
        )));
    }
    Ok(())
}

impl AtprotoIdentityRecord {
    /// The row key this element lives at.
    ///
    /// # Errors
    /// A key segment that is empty or holds a `/`.
    pub fn plane_key(&self) -> Result<String> {
        Ok(match self {
            Self::RotationKey(k) => {
                segment("rotation key", &k.pubkey_did_key)?;
                format!("{ROTATION_KEY_PREFIX}{}", k.pubkey_did_key)
            }
            Self::TombstoneConsent(did) => {
                segment("tombstone consent", did)?;
                format!("{TOMBSTONE_CONSENT_PREFIX}{did}")
            }
            Self::ContestIntent(i) => {
                segment("contest intent DID", &i.did)?;
                segment("contest intent op CID", &i.contested_op_cid)?;
                format!("{CONTEST_INTENT_PREFIX}{}/{}", i.did, i.contested_op_cid)
            }
            Self::NestNamedDid(did) => {
                segment("nest-named DID", did)?;
                format!("{NEST_NAMED_DID_PREFIX}{did}")
            }
        })
    }

    /// The canonical value bytes.
    ///
    /// # Errors
    /// Canonical-encoding failure.
    pub fn encode(&self) -> Result<Vec<u8>> {
        match self {
            Self::RotationKey(k) => crate::encoding::canonical_encode(k),
            Self::TombstoneConsent(did) | Self::NestNamedDid(did) => {
                crate::encoding::canonical_encode(did)
            }
            Self::ContestIntent(i) => crate::encoding::canonical_encode(i),
        }
    }

    /// The per-row join — the halves [`AtprotoIdentityConfig::merge`] runs:
    /// [`AtprotoRotationKey::merge`] and [`AtprotoContestIntent::merge`]; a
    /// DID row is its own join. Two rotation keys under one `pubkey_did_key`
    /// but different scalars are refused: honest devices cannot produce them
    /// (the public key is derived from the scalar), and keeping either would
    /// be a pick between a real key and a forged one.
    ///
    /// # Errors
    /// The two sides are different elements, or one pubkey with two scalars.
    pub fn merge(&self, other: &Self) -> Result<Self> {
        let differ = || Error::Encoding("atproto identity rows name different elements".into());
        match (self, other) {
            (Self::RotationKey(a), Self::RotationKey(b))
                if a.pubkey_did_key == b.pubkey_did_key =>
            {
                if a.secret_scalar != b.secret_scalar {
                    return Err(Error::Encoding(format!(
                        "two rotation keys under {:?} hold different scalars",
                        a.pubkey_did_key
                    )));
                }
                Ok(Self::RotationKey(a.merge(b)))
            }
            (Self::ContestIntent(a), Self::ContestIntent(b))
                if a.did == b.did && a.contested_op_cid == b.contested_op_cid =>
            {
                Ok(Self::ContestIntent(a.merge(b)))
            }
            (Self::TombstoneConsent(a), Self::TombstoneConsent(b))
            | (Self::NestNamedDid(a), Self::NestNamedDid(b))
                if a == b =>
            {
                Ok(self.clone())
            }
            _ => Err(differ()),
        }
    }
}

/// Decode `value` as `T`, refusing bytes that do not re-encode to themselves
/// — the row decode's `deny_unknown_fields` (the module doc).
fn decode_exact<T>(value: &[u8]) -> Result<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let decoded: T = crate::encoding::canonical_decode(value)?;
    if crate::encoding::canonical_encode(&decoded)? != value {
        return Err(Error::Encoding(
            "atproto identity row carries a field this build does not know".into(),
        ));
    }
    Ok(decoded)
}

/// Decode one `fauna.state.atproto-identity` row: the key's prefix picks the
/// element type, the value must decode as it exactly (the module doc's
/// posture) and name the same element as the key — an element is never
/// silently filed under another's key.
///
/// # Errors
/// A key outside the grammar, an undecodable or inexact value, or a
/// key/value mismatch.
pub fn decode_atproto_identity_row(key: &str, value: &[u8]) -> Result<AtprotoIdentityRecord> {
    let record = if key.starts_with(ROTATION_KEY_PREFIX) {
        AtprotoIdentityRecord::RotationKey(decode_exact(value)?)
    } else if key.starts_with(TOMBSTONE_CONSENT_PREFIX) {
        AtprotoIdentityRecord::TombstoneConsent(decode_exact(value)?)
    } else if key.starts_with(CONTEST_INTENT_PREFIX) {
        AtprotoIdentityRecord::ContestIntent(decode_exact(value)?)
    } else if key.starts_with(NEST_NAMED_DID_PREFIX) {
        AtprotoIdentityRecord::NestNamedDid(decode_exact(value)?)
    } else {
        return Err(Error::Encoding(format!(
            "not an atproto identity key: {key:?}"
        )));
    };
    let named = record.plane_key()?;
    if named != key {
        return Err(Error::Encoding(format!(
            "atproto identity row at {key:?} holds the element for {named:?}"
        )));
    }
    Ok(record)
}

impl AtprotoIdentityConfig {
    /// Every element as its plane row, `(key, record)`: rotation keys,
    /// consents, intents, then named DIDs, each in the composite's order.
    ///
    /// # Errors
    /// An element whose key segment is empty or holds a `/`.
    pub fn rows(&self) -> Result<Vec<(String, AtprotoIdentityRecord)>> {
        self.rotation_keys
            .iter()
            .cloned()
            .map(AtprotoIdentityRecord::RotationKey)
            .chain(
                self.tombstone_consents
                    .iter()
                    .cloned()
                    .map(AtprotoIdentityRecord::TombstoneConsent),
            )
            .chain(
                self.contest_intents
                    .iter()
                    .cloned()
                    .map(AtprotoIdentityRecord::ContestIntent),
            )
            .chain(
                self.nest_named_dids
                    .iter()
                    .cloned()
                    .map(AtprotoIdentityRecord::NestNamedDid),
            )
            .map(|r| Ok((r.plane_key()?, r)))
            .collect()
    }

    /// Fold one plane row into the record — the read side of the per-element
    /// rows, through the composite merge so the fold is order-independent.
    ///
    /// # Errors
    /// [`decode_atproto_identity_row`]'s refusals.
    pub fn fold_row(&mut self, key: &str, value: &[u8]) -> Result<()> {
        let mut one = Self::default();
        match decode_atproto_identity_row(key, value)? {
            AtprotoIdentityRecord::RotationKey(k) => one.rotation_keys.push(k),
            AtprotoIdentityRecord::TombstoneConsent(d) => one.tombstone_consents.push(d),
            AtprotoIdentityRecord::ContestIntent(i) => one.contest_intents.push(i),
            AtprotoIdentityRecord::NestNamedDid(d) => one.nest_named_dids.push(d),
        }
        *self = self.merge(&one);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::SecretArray32;

    fn key(seed: u8, created: u64, dids: &[&str]) -> AtprotoRotationKey {
        AtprotoRotationKey {
            secret_scalar: SecretArray32::new([seed; 32]),
            pubkey_did_key: format!("did:key:zDn{seed:02x}"),
            created_at: created,
            published_for_dids: dids.iter().map(|d| (*d).to_string()).collect(),
        }
    }

    fn intent(did: &str, cid: &str, at: u64) -> AtprotoContestIntent {
        AtprotoContestIntent {
            did: did.into(),
            contested_op_cid: cid.into(),
            requested_at: at,
        }
    }

    fn sample() -> AtprotoIdentityConfig {
        AtprotoIdentityConfig {
            rotation_keys: vec![key(1, 10, &["did:plc:aaa"]), key(2, 20, &[])],
            tombstone_consents: vec!["did:plc:aaa".into()],
            contest_intents: vec![intent("did:plc:bbb", "bafyop1", 30)],
            nest_named_dids: vec!["did:plc:aaa".into(), "did:plc:bbb".into()],
        }
    }

    /// Split and fold are inverses, each row decodes back to its element
    /// under its own key, and the keys are the documented grammar.
    #[test]
    fn rows_round_trip_through_the_fold() {
        let cfg = sample();
        let rows = cfg.rows().unwrap();
        let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "key/did:key:zDn01",
                "key/did:key:zDn02",
                "consent/did:plc:aaa",
                "intent/did:plc:bbb/bafyop1",
                "named/did:plc:aaa",
                "named/did:plc:bbb",
            ]
        );
        let mut folded = AtprotoIdentityConfig::default();
        for (k, r) in rows.iter().rev() {
            let v = r.encode().unwrap();
            assert_eq!(&decode_atproto_identity_row(k, &v).unwrap(), r);
            folded.fold_row(k, &v).unwrap();
        }
        assert_eq!(folded, cfg);
    }

    /// A value filed under another element's key, a key outside the
    /// grammar, and a segment the grammar cannot spell are all refused.
    #[test]
    fn misfiled_or_unspellable_rows_are_refused() {
        let v = AtprotoIdentityRecord::NestNamedDid("did:plc:aaa".into())
            .encode()
            .unwrap();
        assert!(decode_atproto_identity_row("named/did:plc:zzz", &v).is_err());
        assert!(decode_atproto_identity_row("consent/did:plc:aaa", &v).is_ok());
        assert!(decode_atproto_identity_row("self", &v).is_err());
        let k = AtprotoIdentityRecord::RotationKey(key(1, 10, &[]))
            .encode()
            .unwrap();
        assert!(decode_atproto_identity_row("key/did:key:zDn02", &k).is_err());
        assert!(decode_atproto_identity_row("intent/did:key:zDn01", &k).is_err());
        let slashed = AtprotoIdentityConfig {
            tombstone_consents: vec!["did:web:x/y".into()],
            ..Default::default()
        };
        assert!(slashed.rows().is_err());
        let empty = AtprotoIdentityConfig {
            contest_intents: vec![intent("did:plc:bbb", "", 1)],
            ..Default::default()
        };
        assert!(empty.rows().is_err());
    }

    /// One pubkey with two scalars is a forged pair, never a pick.
    #[test]
    fn a_rotation_key_pair_with_two_scalars_is_refused() {
        let a = key(1, 10, &[]);
        let mut b = a.clone();
        b.secret_scalar = SecretArray32::new([9; 32]);
        assert!(
            AtprotoIdentityRecord::RotationKey(a)
                .merge(&AtprotoIdentityRecord::RotationKey(b))
                .is_err()
        );
    }
}
