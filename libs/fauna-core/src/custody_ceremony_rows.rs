//! The `fauna.state.custody-ceremony` plane rows' key grammar
//! (`config-dissolution.md` § Phases and gates → *Bounded rows*; the kinds
//! table's `fauna.state.custody-ceremony` row owns the ruling).
//!
//! **One row per ceremony side-record, never one per account.** A
//! [`CustodyConfig`] grows by one record per custody the account granted or
//! was offered, and every record holds its signed envelopes verbatim (offer,
//! accept, deliver with its witness, the latest receipt), so a per-account
//! row has no size bound — the group-share ceremony's measured 70 KB record
//! is the precedent. Each record is its own row, keyed by its side and its
//! grant id (the pair [`CustodyConfig::merge`] deduplicates on):
//!
//! | key | value |
//! |---|---|
//! | `granted/<grant id hex32>` | one [`GrantedCustody`] (this account is the owner) |
//! | `held/<grant id hex32>` | one [`HeldCustody`] (this account is the host) |
//!
//! The grant id is [`crate::custody_grant::CUSTODY_GRANT_ID_LEN`] bytes, its
//! key segment the lowercase hex the registry rows use
//! ([`crate::custody_grant::custody_entry_key`]) — one spelling. A row's
//! value must name its own key. [`CustodyConfig`] is the READ fold over the
//! account's rows ([`CustodyConfig::fold_row`]) and [`CustodyConfig::rows`]
//! the split; the plane arm is the per-record half of the composite merge
//! ([`CustodyRecord::merge`]). A record is never removed — a declined offer
//! or a reclaimed custody stays as a monotone mark, which is what keeps an
//! idempotent re-ingest from resurrecting it — so the kind has no tombstone,
//! and a spent, unanswered offer's row is not reclaimed either (ruled
//! 2026-10-08, `account-replica-posture.md` § Replica posture → *The custody
//! grant + ceremony*, step 1: the policy refuses tombstones and a published
//! row is permanent per `(item_key, writer)`; the bounded capture rate
//! stands). Each row stays bounded because each side keeps ONE latest
//! receipt, never a history.
//!
//! **Decode posture: an unknown field is refused, not stripped**
//! (`config-dissolution.md` P4 — the CrdtPerField posture). The value types
//! stay tolerant; the row decode instead requires the value to re-encode to
//! its own bytes, which a value carrying a field this build does not know (or
//! any non-canonical spelling) cannot.

use crate::custody_ceremony::{CustodyConfig, GrantedCustody, HeldCustody};
use crate::custody_grant::{CUSTODY_GRANT_ID_LEN, custody_entry_key};
use crate::error::{Error, Result};

/// Key prefix of an owner-side record: `granted/<grant id hex32>`.
pub const GRANTED_KEY_PREFIX: &str = "granted/";
/// Key prefix of a host-side record: `held/<grant id hex32>`.
pub const HELD_KEY_PREFIX: &str = "held/";

/// A parsed `fauna.state.custody-ceremony` row key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustodyRowKey {
    /// `granted/<grant id>` — holds one [`GrantedCustody`].
    Granted([u8; CUSTODY_GRANT_ID_LEN]),
    /// `held/<grant id>` — holds one [`HeldCustody`].
    Held([u8; CUSTODY_GRANT_ID_LEN]),
}

/// The grant id as a key segment — refused unless it is exactly
/// [`CUSTODY_GRANT_ID_LEN`] bytes.
fn grant_id_array(grant_id: &[u8]) -> Result<[u8; CUSTODY_GRANT_ID_LEN]> {
    grant_id.try_into().map_err(|_| {
        Error::Encoding(format!(
            "a custody ceremony record's grant id is {} bytes, not {CUSTODY_GRANT_ID_LEN}",
            grant_id.len()
        ))
    })
}

impl CustodyRowKey {
    /// Parse a row key. Strict: only the canonical spelling (lowercase hex,
    /// exactly one grant id) parses, so one record has one key.
    ///
    /// # Errors
    /// Any other string.
    pub fn parse(key: &str) -> Result<Self> {
        let id = |s: &str| -> Result<[u8; CUSTODY_GRANT_ID_LEN]> {
            let lower = s.len() == 2 * CUSTODY_GRANT_ID_LEN
                && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
            if !lower {
                return Err(Error::Encoding(format!(
                    "custody ceremony key segment is not a lowercase hex grant id: {s:?}"
                )));
            }
            let bytes = hex::decode(s).map_err(|e| Error::Encoding(e.to_string()))?;
            grant_id_array(&bytes)
        };
        if let Some(rest) = key.strip_prefix(GRANTED_KEY_PREFIX) {
            return Ok(Self::Granted(id(rest)?));
        }
        if let Some(rest) = key.strip_prefix(HELD_KEY_PREFIX) {
            return Ok(Self::Held(id(rest)?));
        }
        Err(Error::Encoding(format!(
            "not a custody ceremony key: {key:?}"
        )))
    }

    /// The canonical key string.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Granted(id) => format!("{GRANTED_KEY_PREFIX}{}", custody_entry_key(id)),
            Self::Held(id) => format!("{HELD_KEY_PREFIX}{}", custody_entry_key(id)),
        }
    }
}

/// One plane row's value: the record its key names.
#[derive(Debug, Clone, PartialEq)]
pub enum CustodyRecord {
    Granted(GrantedCustody),
    Held(HeldCustody),
}

impl CustodyRecord {
    /// The row key this record lives at.
    ///
    /// # Errors
    /// A grant id that is not [`CUSTODY_GRANT_ID_LEN`] bytes.
    pub fn key(&self) -> Result<CustodyRowKey> {
        Ok(match self {
            Self::Granted(r) => CustodyRowKey::Granted(grant_id_array(&r.grant_id)?),
            Self::Held(r) => CustodyRowKey::Held(grant_id_array(&r.grant_id)?),
        })
    }

    /// The canonical value bytes.
    ///
    /// # Errors
    /// Canonical-encoding failure.
    pub fn encode(&self) -> Result<Vec<u8>> {
        match self {
            Self::Granted(r) => crate::encoding::canonical_encode(r),
            Self::Held(r) => crate::encoding::canonical_encode(r),
        }
        .map(|b| b.to_vec())
    }

    /// The per-record join — the halves [`CustodyConfig::merge`] runs:
    /// [`GrantedCustody::merge`] and [`HeldCustody::merge`].
    ///
    /// # Errors
    /// The two sides are different sides or different ceremonies.
    pub fn merge(&self, other: &Self) -> Result<Self> {
        match (self, other) {
            (Self::Granted(a), Self::Granted(b)) if a.grant_id == b.grant_id => {
                Ok(Self::Granted(a.merge(b)))
            }
            (Self::Held(a), Self::Held(b)) if a.grant_id == b.grant_id => {
                Ok(Self::Held(a.merge(b)))
            }
            _ => Err(Error::Encoding(
                "custody ceremony records name different ceremonies".into(),
            )),
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
            "custody ceremony row carries a field this build does not know".into(),
        ));
    }
    Ok(decoded)
}

/// Decode one `fauna.state.custody-ceremony` row: the key's prefix picks the
/// side, the value must decode as its record exactly (the module doc's
/// posture) and name the same ceremony as the key — a record is never
/// silently filed under another grant id.
///
/// # Errors
/// A key outside the grammar, an undecodable or inexact value, or a
/// key/value mismatch.
pub fn decode_custody_row(key: &str, value: &[u8]) -> Result<CustodyRecord> {
    let parsed = CustodyRowKey::parse(key)?;
    let record = match parsed {
        CustodyRowKey::Granted(_) => CustodyRecord::Granted(decode_exact(value)?),
        CustodyRowKey::Held(_) => CustodyRecord::Held(decode_exact(value)?),
    };
    let named = record.key()?;
    if named != parsed {
        return Err(Error::Encoding(format!(
            "custody ceremony row at {key:?} holds the record for {:?}",
            named.render()
        )));
    }
    Ok(record)
}

impl CustodyConfig {
    /// Every record as its plane row, `(key, record)`: granted then held,
    /// each in the composite's order.
    ///
    /// # Errors
    /// A record whose grant id is not [`CUSTODY_GRANT_ID_LEN`] bytes.
    pub fn rows(&self) -> Result<Vec<(String, CustodyRecord)>> {
        self.granted
            .iter()
            .cloned()
            .map(CustodyRecord::Granted)
            .chain(self.held.iter().cloned().map(CustodyRecord::Held))
            .map(|r| Ok((r.key()?.render(), r)))
            .collect()
    }

    /// Fold one plane row into the record — the read side of the per-record
    /// rows, through the composite merge so the fold is order-independent.
    ///
    /// # Errors
    /// [`decode_custody_row`]'s refusals.
    pub fn fold_row(&mut self, key: &str, value: &[u8]) -> Result<()> {
        let one = match decode_custody_row(key, value)? {
            CustodyRecord::Granted(r) => Self {
                granted: vec![r],
                held: Vec::new(),
            },
            CustodyRecord::Held(r) => Self {
                granted: Vec::new(),
                held: vec![r],
            },
        };
        *self = self.merge(&one);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Timestamp;

    fn granted(id: u8) -> GrantedCustody {
        GrantedCustody {
            grant_id: vec![id; CUSTODY_GRANT_ID_LEN],
            host: [2u8; 32],
            channel_hex: "aa".repeat(32),
            offer: vec![0xF0, id],
            updated_at: Timestamp(1_000),
            ..Default::default()
        }
    }

    fn held(id: u8) -> HeldCustody {
        HeldCustody {
            grant_id: vec![id; CUSTODY_GRANT_ID_LEN],
            owner: [9u8; 32],
            channel_hex: "bb".repeat(32),
            offer: vec![0x0F, id],
            updated_at: Timestamp(1_200),
            ..Default::default()
        }
    }

    fn sample() -> CustodyConfig {
        CustodyConfig {
            granted: vec![granted(0x1D), granted(0x2E)],
            held: vec![held(0x3F)],
        }
    }

    /// Split and fold are inverses, each row decodes back to its record
    /// under its own key, and the keys are the documented grammar.
    #[test]
    fn rows_round_trip_through_the_fold() {
        let cfg = sample();
        let rows = cfg.rows().unwrap();
        let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                format!("granted/{}", "1d".repeat(16)),
                format!("granted/{}", "2e".repeat(16)),
                format!("held/{}", "3f".repeat(16)),
            ]
        );
        let mut folded = CustodyConfig::default();
        for (k, r) in rows.iter().rev() {
            let v = r.encode().unwrap();
            assert_eq!(&decode_custody_row(k, &v).unwrap(), r);
            assert_eq!(CustodyRowKey::parse(k).unwrap().render(), *k);
            folded.fold_row(k, &v).unwrap();
        }
        assert_eq!(folded, cfg);
    }

    /// A record filed under another grant id or the other side, a key
    /// outside the grammar, and a record whose grant id the grammar cannot
    /// spell are all refused.
    #[test]
    fn misfiled_or_unspellable_rows_are_refused() {
        let v = CustodyRecord::Granted(granted(0x1D)).encode().unwrap();
        let own = format!("granted/{}", "1d".repeat(16));
        assert!(decode_custody_row(&own, &v).is_ok());
        for bad in [
            format!("granted/{}", "2e".repeat(16)),
            format!("held/{}", "1d".repeat(16)),
            format!("granted/{}", "1D".repeat(16)),
            format!("granted/{}", "1d".repeat(15)),
            format!("granted/{}/x", "1d".repeat(16)),
            "self".to_string(),
        ] {
            assert!(decode_custody_row(&bad, &v).is_err(), "{bad}");
        }
        let short = CustodyConfig {
            held: vec![HeldCustody {
                grant_id: vec![0x3F; 4],
                ..held(0x3F)
            }],
            ..Default::default()
        };
        assert!(short.rows().is_err());
    }

    /// The row decode is strict though the value types are tolerant: a
    /// newer build's field is refused on the plane, where a tolerant decode would
    /// strip it.
    #[test]
    fn a_newer_field_is_refused_on_the_row() {
        #[derive(serde::Serialize)]
        struct Newer<'a> {
            #[serde(flatten)]
            record: &'a HeldCustody,
            from_the_future: u8,
        }
        let r = held(0x3F);
        let key = format!("held/{}", "3f".repeat(16));
        let bytes = crate::encoding::canonical_encode(&Newer {
            record: &r,
            from_the_future: 1,
        })
        .unwrap();
        let tolerant: HeldCustody = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(tolerant, r, "the value type itself stays tolerant");
        assert!(decode_custody_row(&key, &bytes).is_err());
    }
}
