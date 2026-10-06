//! The `fauna.state.deployment-seeds` plane rows — the key grammar, the
//! strict value decode and the per-row join of the multi-nest deployment-seed
//! custody (`config-dissolution.md` owns
//! the kind's birth, plane-only, and § Phases and gates → *Bounded rows* the
//! row shape; `nest/box-recovery.md` § Trust & audience owns what the seeds
//! are; [`DeploymentSeedEntry::merge_seed_map`] owns the shipped rule).
//!
//! **One row per custodied box, never one per account.** The map grows by one
//! entry per box the admin identity administers, and a rotated box's
//! predecessor stays custodied (marked `superseded_by`, never deleted), so a
//! per-account row grows with use; each entry carries a 32-byte seed, the
//! box's irrecoverable identity. The key is the entry's `nest_actor_id` in
//! lowercase hex (`<hex64>`): the map's own key, and the id is public — every
//! client TOFU-pins it.
//!
//! The per-row join is [`DeploymentSeedEntry::merge`] — [`Self::fold_from`]'s
//! lattice (label join, present-wins supersession), the same function the
//! whole-map union folds through. The composite map is the READ fold
//! ([`fold_deployment_seed_row`] through [`DeploymentSeedEntry::merge_seed_map`]);
//! [`deployment_seed_rows`] is the other direction.
//!
//! **Decode posture — strict on the plane.** The kind is
//! CrdtPerField, so a tolerant reader would strip a newer build's field from
//! the bytes it re-encodes (`config-dissolution.md` P4). `DeploymentSeedEntry`
//! cannot take `deny_unknown_fields` — its `#[serde(flatten)] extra` catch-all
//! is what keeps an older reader from dropping a newer entry field, and
//! serde refuses the two together — so the plane gets its strictness the way
//! the succession ledger's shared value types do: a row must re-encode to its
//! own bytes, and its `extra` must be empty (an unknown field lands in `extra`
//! and would re-encode unchanged, so the round-trip alone cannot see it). A
//! newer field is refused, never stripped.
//!
//! **A row is self-consistent or refused.** Its value must name its key's box,
//! and its seed must derive to that id (the recovery reads' check, moved to the door): a row that passes is one
//! whose seed can be trusted, and two rows at one key carry the same seed.
//!
//! [`Self::fold_from`]: DeploymentSeedEntry::fold_from

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::data::DeploymentSeedEntry;
use crate::encoding::{canonical_decode, canonical_encode};
use crate::error::{Error, Result};

impl DeploymentSeedEntry {
    /// The row's plane key — the box's `nest_actor_id`, lowercase hex.
    #[must_use]
    pub fn plane_key(&self) -> String {
        crate::hex32::encode(&self.nest_actor_id)
    }

    /// The canonical value bytes of this entry's row.
    ///
    /// # Errors
    /// Canonical-encoding failure.
    pub fn encode_row(&self) -> Result<Vec<u8>> {
        canonical_encode(self).map(|b| b.to_vec())
    }

    /// The per-row join — the half [`Self::merge_seed_map`] runs on one box:
    /// [`Self::fold_from`]'s lattice over two custody rows for the same box.
    ///
    /// # Errors
    /// The two sides name different boxes, or carry different seeds for one
    /// box (a forged or corrupt row — the seed IS the id's preimage).
    pub fn merge(&self, other: &Self) -> Result<Self> {
        if self.nest_actor_id != other.nest_actor_id || self.seed != other.seed {
            return Err(Error::Encoding(
                "deployment-seed rows name different boxes or seeds".into(),
            ));
        }
        let mut joined = self.clone();
        joined.fold_from(other.clone());
        Ok(joined)
    }
}

/// Decode `value` as `T`, refusing anything that does not re-encode to the
/// same bytes (a non-canonical encoding, or a field this build drops).
fn decode_exact<T: Serialize + DeserializeOwned>(value: &[u8]) -> Result<T> {
    let decoded: T = canonical_decode(value)?;
    if canonical_encode(&decoded)?.as_slice() != value {
        return Err(Error::Encoding(
            "deployment-seed row does not re-encode to its own bytes".into(),
        ));
    }
    Ok(decoded)
}

/// Decode one `fauna.state.deployment-seeds` row: the key must be a lowercase
/// hex64 id, the value must decode and re-encode to its own bytes with an
/// empty `extra` (the strict posture, module docs), name the key's box, and
/// carry a seed that derives to it.
///
/// # Errors
/// An unparseable key, an undecodable or non-canonical value, an unknown
/// field, a value filed under another box's key, or a seed that is not the
/// id's preimage.
pub fn decode_deployment_seed_row(key: &str, value: &[u8]) -> Result<DeploymentSeedEntry> {
    if !crate::hex32::is_lowercase_hex64(key) {
        return Err(Error::Encoding(format!(
            "not a deployment-seeds key: {key:?}"
        )));
    }
    let entry: DeploymentSeedEntry = decode_exact(value)?;
    if !entry.extra.is_empty() {
        return Err(Error::Encoding(format!(
            "deployment-seed row {key:?} carries fields this build does not know: {:?}",
            entry.extra.keys().collect::<Vec<_>>()
        )));
    }
    if entry.plane_key() != key {
        return Err(Error::Encoding(format!(
            "deployment-seed row at {key:?} holds the entry for {:?}",
            entry.plane_key()
        )));
    }
    if crate::data::DeploymentSeedEntry::nest_actor_id_for_seed(entry.seed.to_array())
        != entry.nest_actor_id
    {
        return Err(Error::Encoding(format!(
            "deployment-seed row {key:?} holds a seed that is not its id's preimage"
        )));
    }
    Ok(entry)
}

/// Every entry of a custody map as its plane row, `(key, entry)`, in key
/// order, one row per box (a map holding two copies of one box joins them).
#[must_use]
pub fn deployment_seed_rows(map: &[DeploymentSeedEntry]) -> Vec<(String, DeploymentSeedEntry)> {
    DeploymentSeedEntry::merge_seed_map(map, &[])
        .into_iter()
        .map(|e| (e.plane_key(), e))
        .collect()
}

/// Fold one plane row into a custody map through the shipped rule — the read
/// side of the per-box rows.
///
/// # Errors
/// Any refusal of [`decode_deployment_seed_row`].
pub fn fold_deployment_seed_row(
    map: &mut Vec<DeploymentSeedEntry>,
    key: &str,
    value: &[u8],
) -> Result<()> {
    let entry = decode_deployment_seed_row(key, value)?;
    *map = DeploymentSeedEntry::merge_seed_map(map, &[entry]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(seed: u8, domain: Option<&str>, superseded_by: Option<u8>) -> DeploymentSeedEntry {
        DeploymentSeedEntry {
            nest_actor_id: crate::data::DeploymentSeedEntry::nest_actor_id_for_seed([seed; 32]),
            seed: [seed; 32].into(),
            domain: domain.map(str::to_string),
            superseded_by: superseded_by
                .map(|s| crate::data::DeploymentSeedEntry::nest_actor_id_for_seed([s; 32])),
            ..Default::default()
        }
    }

    fn fold(rows: &[(String, DeploymentSeedEntry)]) -> Vec<DeploymentSeedEntry> {
        let mut out = Vec::new();
        for (k, e) in rows {
            fold_deployment_seed_row(&mut out, k, &e.encode_row().unwrap()).unwrap();
        }
        out
    }

    /// The fold is the shipped rule: two devices' maps, split into rows and
    /// folded back, read exactly as
    /// [`DeploymentSeedEntry::merge_seed_map`] of the two — a box custodied
    /// on one side only, and one box both hold with a label on one side and
    /// the rotation mark on the other.
    #[test]
    fn the_row_fold_reads_as_the_shipped_merge() {
        let a = vec![
            entry(0x11, None, Some(0x12)),
            entry(0x21, Some("a.example"), None),
        ];
        let b = vec![
            entry(0x11, Some("old.example"), None),
            entry(0x12, Some("new.example"), None),
        ];
        let mut rows = deployment_seed_rows(&a);
        rows.extend(deployment_seed_rows(&b));
        let folded = fold(&rows);
        assert_eq!(folded, DeploymentSeedEntry::merge_seed_map(&a, &b));
        assert_eq!(folded.len(), 3, "one entry per box");
        let old = folded
            .iter()
            .find(|e| e.nest_actor_id == entry(0x11, None, None).nest_actor_id)
            .unwrap();
        assert_eq!(old.domain.as_deref(), Some("old.example"));
        assert!(old.superseded_by.is_some(), "the rotation mark survives");
        // Order-free, and the fold's own rows fold back to it.
        rows.reverse();
        assert_eq!(fold(&rows), folded);
        assert_eq!(fold(&deployment_seed_rows(&folded)), folded);
    }

    /// A key outside the grammar, a misfiled value, a self-inconsistent seed,
    /// a mismatched join and a field from a newer build are all refused.
    #[test]
    fn junk_misfiled_forged_or_newer_rows_are_refused() {
        let e = entry(0x11, Some("a.example"), None);
        let key = e.plane_key();
        let value = e.encode_row().unwrap();
        assert!(decode_deployment_seed_row(&key, &value).is_ok());
        assert!(decode_deployment_seed_row("self", &value).is_err());
        assert!(decode_deployment_seed_row(&key.to_uppercase(), &value).is_err());
        let other = entry(0x22, None, None);
        assert!(decode_deployment_seed_row(&other.plane_key(), &value).is_err());
        assert!(e.merge(&other).is_err());
        // A seed that is not its id's preimage.
        let forged = DeploymentSeedEntry {
            seed: [0x77; 32].into(),
            ..e.clone()
        };
        assert!(decode_deployment_seed_row(&key, &forged.encode_row().unwrap()).is_err());
        assert!(e.merge(&forged).is_err());
        // A field from a newer build lands in `extra` and re-encodes
        // unchanged — the empty-`extra` check is what refuses it.
        let mut newer = e.clone();
        newer
            .extra
            .insert("from_the_future".into(), fauna_cbor::Value::Integer(1));
        assert!(decode_deployment_seed_row(&key, &newer.encode_row().unwrap()).is_err());
    }
}
