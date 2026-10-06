//! The `fauna.state.blessed-nests` plane rows — the key grammar, the strict
//! value decode and the per-row join of the user's per-nest blessing verdicts
//! (`config-dissolution.md` owns the
//! kind's birth, plane-only, and § Phases and gates → *Bounded rows* the row
//! shape; `docs/goal/ui/nests.md` § Expiry / renewal → *Duration and
//! blessing* owns what a blessing is; [`merge_blessed_nests`] owns the
//! shipped rule).
//!
//! **One row per nest, never one per account.** The list grows by one entry
//! per nest the user ever blessed or un-blessed, and an un-blessed entry is
//! kept (`blessed: false`, so the newer verdict can win a merge), so a `self`
//! row would be bounded by use, not by shape. Key `<nest_id hex64>` — the
//! lowercase hex of the nest's 32-byte identity, no prefix (the kind has one
//! family) — holds one [`BlessedNest`]; the value must name its key's nest.
//! Every row is bounded by its own shape: a 32-byte id, a bool, a stamp.
//!
//! The per-row join is the per-`nest_id` half of the shipped rule
//! ([`BlessedNest::join`]: the newer `at` wins, an equal `at` goes to the
//! un-blessed side). The composite list is the READ fold over the account's
//! rows ([`fold_blessed_nest_rows`]), sorted by `nest_id` — exactly what
//! [`merge_blessed_nests`] returns for the replicas that wrote the rows.
//!
//! **Decode posture — strict on the plane.** The kind is
//! CrdtPerField, so a tolerant reader would strip a newer build's field from
//! the bytes it re-encodes (`config-dissolution.md` P4). [`BlessedNest`]
//! stays tolerant (a `deny_unknown_fields` there would make an older build
//! refuse a newer build's record), so the plane gets its strictness the way the
//! ledger's shared types do: a row must re-encode to exactly its own bytes. A
//! newer field is refused, never stripped.

use crate::data::{BlessedNest, merge_blessed_nests};
use crate::encoding::{canonical_decode, canonical_encode};
use crate::error::{Error, Result};

/// The plane key of `nest_id`'s row — its lowercase hex64.
#[must_use]
pub fn blessed_nest_key(nest_id: &[u8; 32]) -> String {
    crate::hex32::encode(nest_id)
}

impl BlessedNest {
    /// The row's plane key — the lowercase hex of `nest_id` (a `nest_id` that
    /// is not 32 bytes spells a key the grammar refuses).
    #[must_use]
    pub fn plane_key(&self) -> String {
        hex::encode(&self.nest_id)
    }

    /// The canonical value bytes of this row.
    ///
    /// # Errors
    /// Canonical-encoding failure.
    pub fn encode_row(&self) -> Result<Vec<u8>> {
        canonical_encode(self).map(|b| b.to_vec())
    }

    /// The per-row join — [`BlessedNest::join`], refusing two different
    /// nests.
    ///
    /// # Errors
    /// The two rows are about different nests.
    pub fn merge(&self, other: &Self) -> Result<Self> {
        if self.nest_id != other.nest_id {
            return Err(Error::Encoding(
                "blessed-nest rows about different nests".into(),
            ));
        }
        Ok(self.join(other))
    }
}

/// Decode one `fauna.state.blessed-nests` row: the key must be a lowercase
/// hex64 nest id, the value must decode and re-encode to its own bytes (the
/// strict posture, module docs), hold a 32-byte `nest_id`, and name the key's
/// nest.
///
/// # Errors
/// An unparseable key, an undecodable or non-canonical value, an unknown
/// field, a `nest_id` that is not 32 bytes, or a value filed under another
/// nest's key.
pub fn decode_blessed_nest_row(key: &str, value: &[u8]) -> Result<BlessedNest> {
    if !crate::hex32::is_lowercase_hex64(key) {
        return Err(Error::Encoding(format!("not a blessed-nests key: {key:?}")));
    }
    let row: BlessedNest = canonical_decode(value)?;
    if canonical_encode(&row)?.as_slice() != value {
        return Err(Error::Encoding(
            "blessed-nest row does not re-encode to its own bytes".into(),
        ));
    }
    if row.nest_id.len() != 32 {
        return Err(Error::Encoding(format!(
            "blessed-nest row {key:?} holds a {}-byte nest id",
            row.nest_id.len()
        )));
    }
    if row.plane_key() != key {
        return Err(Error::Encoding(format!(
            "blessed-nest row at {key:?} holds the entry for {:?}",
            row.plane_key()
        )));
    }
    Ok(row)
}

/// Every entry of `list` as its plane row, `(key, row)`, in key order, one
/// row per nest — a list holding two entries for one nest joins them.
#[must_use]
pub fn blessed_nest_rows(list: &[BlessedNest]) -> Vec<(String, BlessedNest)> {
    let mut rows = std::collections::BTreeMap::<String, BlessedNest>::new();
    for entry in list {
        let key = entry.plane_key();
        let joined = match rows.remove(&key) {
            Some(held) => held.join(entry),
            None => entry.clone(),
        };
        rows.insert(key, joined);
    }
    rows.into_iter().collect()
}

/// The READ fold over `rows` (`(key, value)`) through the shipped rule — the
/// composite list, sorted by `nest_id`, as [`merge_blessed_nests`] returns
/// it.
///
/// # Errors
/// Any refusal of [`decode_blessed_nest_row`].
pub fn fold_blessed_nest_rows<'a>(
    rows: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Result<Vec<BlessedNest>> {
    let mut folded = Vec::new();
    for (key, value) in rows {
        fold_blessed_nest_row(&mut folded, key, value)?;
    }
    Ok(folded)
}

/// Fold one plane row into `folded` through the shipped rule — the read side
/// of the per-nest rows; `folded` stays sorted by `nest_id`.
///
/// # Errors
/// Any refusal of [`decode_blessed_nest_row`].
pub fn fold_blessed_nest_row(folded: &mut Vec<BlessedNest>, key: &str, value: &[u8]) -> Result<()> {
    let row = decode_blessed_nest_row(key, value)?;
    *folded = merge_blessed_nests(folded, std::slice::from_ref(&row));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nest(id: u8, blessed: bool, at: u64) -> BlessedNest {
        BlessedNest {
            nest_id: vec![id; 32],
            blessed,
            at,
        }
    }

    fn fold(rows: &[(String, BlessedNest)]) -> Vec<BlessedNest> {
        let encoded: Vec<(String, Vec<u8>)> = rows
            .iter()
            .map(|(k, r)| (k.clone(), r.encode_row().unwrap()))
            .collect();
        fold_blessed_nest_rows(encoded.iter().map(|(k, v)| (k.as_str(), v.as_slice()))).unwrap()
    }

    /// The fold is the shipped rule: two devices' lists, split into rows and
    /// folded back, read exactly as [`merge_blessed_nests`] of the two — a
    /// newer verdict on one side, a tie that un-blesses, a nest only one side
    /// holds — in any row order.
    #[test]
    fn the_row_fold_reads_as_the_shipped_merge() {
        let a = vec![
            nest(1, true, 100),
            nest(2, true, 50),
            nest(3, false, 90),
            nest(4, true, 10),
        ];
        let b = vec![nest(1, false, 120), nest(2, false, 50), nest(3, true, 80)];
        let mut rows = blessed_nest_rows(&a);
        rows.extend(blessed_nest_rows(&b));
        let folded = fold(&rows);
        assert_eq!(folded, merge_blessed_nests(&a, &b));
        assert_eq!(folded, merge_blessed_nests(&b, &a));
        rows.reverse();
        assert_eq!(fold(&rows), folded, "order-free");
        assert_eq!(
            fold(&blessed_nest_rows(&folded)),
            folded,
            "the fold's own rows fold back"
        );
    }

    /// The write stamp: a re-assert writes nothing, a toggle supersedes its
    /// prior verdict even on a clock behind it, and a first verdict takes
    /// `now` — the stamp the plane door writes with.
    #[test]
    fn the_verdict_stamp_supersedes_its_prior() {
        let id = [7u8; 32];
        assert_eq!(
            BlessedNest::verdict(None, &id, true, 100),
            Some(BlessedNest {
                nest_id: id.to_vec(),
                blessed: true,
                at: 100
            })
        );
        let prior = BlessedNest {
            nest_id: id.to_vec(),
            blessed: true,
            at: 100,
        };
        assert_eq!(BlessedNest::verdict(Some(&prior), &id, true, 500), None);
        assert_eq!(
            BlessedNest::verdict(Some(&prior), &id, false, 40).map(|v| v.at),
            Some(101),
            "a clock behind the prior verdict still supersedes it"
        );
        assert_eq!(
            BlessedNest::verdict(Some(&prior), &id, false, 400).map(|v| v.at),
            Some(400)
        );
    }

    /// A key outside the grammar, a misfiled value, a short nest id, a
    /// cross-nest join, and a field from a newer build are all refused.
    #[test]
    fn junk_misfiled_misshapen_or_newer_rows_are_refused() {
        let row = nest(0xab, true, 100);
        let key = row.plane_key();
        let value = row.encode_row().unwrap();
        assert_eq!(key, blessed_nest_key(&[0xab; 32]));
        assert_eq!(decode_blessed_nest_row(&key, &value).unwrap(), row);
        assert!(decode_blessed_nest_row("self", &value).is_err());
        assert!(decode_blessed_nest_row(&key.to_uppercase(), &value).is_err());
        // Filed under another nest.
        let other = nest(2, true, 100);
        assert!(decode_blessed_nest_row(&other.plane_key(), &value).is_err());
        assert!(row.merge(&other).is_err());
        // A 31-byte nest id.
        let mut short = row.clone();
        short.nest_id.pop();
        assert!(decode_blessed_nest_row(&key, &short.encode_row().unwrap()).is_err());
        // A field from a newer build, and a non-canonical encoding.
        #[derive(serde::Serialize)]
        struct Newer<'a> {
            #[serde(flatten)]
            record: &'a BlessedNest,
            from_the_future: u8,
        }
        let newer = canonical_encode(&Newer {
            record: &row,
            from_the_future: 1,
        })
        .unwrap();
        assert!(decode_blessed_nest_row(&key, &newer).is_err());
        let mut loose = vec![0xb8, value[0] & 0x1f];
        loose.extend_from_slice(&value[1..]);
        assert!(decode_blessed_nest_row(&key, &loose).is_err());
    }
}
