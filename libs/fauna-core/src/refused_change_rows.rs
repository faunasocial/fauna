//! The `fauna.state.refused-scheduling-changes` plane row — the key, the
//! strict value decode and the owner's three gestures on the refused inbound
//! scheduling changes (`config-dissolution.md` owns the kind's
//! birth, plane-only;
//! `inbound-scheduling-authority.md` § *Where the record rests* owns what the
//! record is; [`RefusedSchedulingChanges::merge`] owns the merge rule).
//!
//! **One row per account, at key [`REFUSED_CHANGES_ROW_KEY`] (`self`).** The
//! list is bounded by construction, not by use: at most
//! [`MAX_REFUSED_SCHEDULING_CHANGES`] rows, each string at its byte ceiling,
//! within [`crate::data::REFUSED_SCHEDULING_CHANGES_BYTE_BUDGET`] — a `const`
//! assertion keeps that arithmetic honest, and the row's size pin
//! (`merge_policy::tests::a_full_refused_changes_row_seals_under_half_the_entry_cap`)
//! measures the sealed row under half the per-entry cap
//! (`config-dissolution.md` § Phases and gates → *Bounded rows*).
//!
//! **Decode posture — strict on the plane.** The kind is
//! CrdtPerField, so a tolerant reader would strip a newer build's field from
//! the bytes it re-encodes (`config-dissolution.md` P4). `RefusedSchedulingChange`
//! cannot take `deny_unknown_fields` — its `#[serde(flatten)] extra` catch-all
//! keeps an older reader from dropping a newer row field, and serde
//! refuses the two together — so the plane takes the deployment-seed rows'
//! posture: a value must re-encode to its own bytes and every row's `extra`
//! must be empty (an unknown field lands in `extra` and would re-encode
//! unchanged, so the round-trip alone cannot see it). A newer field is
//! refused, never stripped. The value must also be **held**: a fixed point of
//! the ceilings (bounded strings, one row per key, key order, both count
//! ceilings), so every value the arm joins is one the merge itself could have
//! produced.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::data::{
    RefusedSchedulingChange, RefusedSchedulingChanges, cap_refused_scheduling_changes,
};
use crate::encoding::{canonical_decode, canonical_encode};
use crate::error::{Error, Result};

/// The one row's key — one refused-change list per account.
pub const REFUSED_CHANGES_ROW_KEY: &str = "self";

impl RefusedSchedulingChanges {
    /// The canonical value bytes of the row.
    ///
    /// # Errors
    /// Canonical-encoding failure.
    pub fn encode_row(&self) -> Result<Vec<u8>> {
        canonical_encode(self).map(|b| b.to_vec())
    }

    /// Record one refused inbound scheduling change, answering **whether
    /// anything changed** — so a caller can skip a write that would store
    /// identical bytes.
    ///
    /// A repeat attempt on a key already on file does not add a row: it bumps
    /// that row's [`RefusedSchedulingChange::occurrences`] and
    /// `last_refused_at`, which is what re-opens a dismissed row when the
    /// attempt is genuinely new. A **re-drain of the same message** reaches
    /// here too (the inbound cursor is per-session), and is deliberately
    /// counted rather than suppressed: the two are indistinguishable at this
    /// seam, and over-counting an attempt only over-reports a refusal the user
    /// is already being told about, while suppressing one could hide a live
    /// attempt.
    ///
    /// `incoming.occurrences` / `dismissed_through` are ignored — a caller
    /// reports one refusal and this owns the counting.
    pub fn record(&mut self, mut incoming: RefusedSchedulingChange) -> bool {
        // Bounded before anything reads it — the key included, so the row it
        // finds or makes is the one that will rest.
        incoming.bound_to_budget();
        let key = incoming.key();
        if let Some(row) = self.rows.iter_mut().find(|row| row.key() == key) {
            row.occurrences = row.occurrences.saturating_add(1);
            row.last_refused_at = row.last_refused_at.max(incoming.last_refused_at);
            row.first_refused_at = row.first_refused_at.min(incoming.first_refused_at);
            // A title the stored event gained since the first attempt is worth
            // adopting; an empty one never overwrites a name already on file.
            if !incoming.summary.is_empty() {
                row.summary = incoming.summary;
            }
            // The bump can move the row past a sibling in the ceilings' order.
            cap_refused_scheduling_changes(&mut self.rows);
            return true;
        }
        self.rows.push(RefusedSchedulingChange {
            occurrences: 1,
            dismissed_through: 0,
            ..incoming
        });
        cap_refused_scheduling_changes(&mut self.rows);
        // A row the ceilings cut immediately is not a change worth writing
        // for — it is not on file afterwards.
        self.rows.iter().any(|row| row.key() == key)
    }

    /// Dismiss the row `key` names (the owner's *I have seen this*), answering
    /// whether it was open to begin with.
    ///
    /// Dismisses **through the attempts counted right now**, so a later attempt
    /// re-opens the row rather than landing silently on a closed one.
    pub fn dismiss(&mut self, key: &str) -> bool {
        let Some(row) = self.rows.iter_mut().find(|row| row.key() == key) else {
            return false;
        };
        if !row.is_open() {
            return false;
        }
        row.dismissed_through = row.occurrences;
        true
    }

    /// The rows every Events surface renders — open only, most recent attempt
    /// first.
    ///
    /// A *projection*: the at-rest list carries dismissed rows too (dropping
    /// them would let a re-drain re-raise what the owner closed), and a surface
    /// that filtered them itself would re-derive
    /// [`RefusedSchedulingChange::is_open`] in seven places.
    #[must_use]
    pub fn open(&self) -> Vec<RefusedSchedulingChange> {
        let mut open: Vec<RefusedSchedulingChange> = self
            .rows
            .iter()
            .filter(|row| row.is_open())
            .cloned()
            .collect();
        open.sort_by(|a, b| {
            b.last_refused_at
                .cmp(&a.last_refused_at)
                .then_with(|| a.key().cmp(&b.key()))
        });
        open
    }
}

/// Decode `value` as `T`, refusing anything that does not re-encode to the
/// same bytes (a non-canonical encoding, or a field this build drops).
fn decode_exact<T: Serialize + DeserializeOwned>(value: &[u8]) -> Result<T> {
    let decoded: T = canonical_decode(value)?;
    if canonical_encode(&decoded)?.as_slice() != value {
        return Err(Error::Encoding(
            "refused-change row does not re-encode to its own bytes".into(),
        ));
    }
    Ok(decoded)
}

/// Decode the `fauna.state.refused-scheduling-changes` row: the key must be
/// [`REFUSED_CHANGES_ROW_KEY`], the value must decode and re-encode to its own
/// bytes with every row's `extra` empty, and it must be held — a fixed point of
/// the ceilings (module docs).
///
/// # Errors
/// Another key, an undecodable or non-canonical value, an unknown field, or a
/// value the ceilings would change.
pub fn decode_refused_changes_row(key: &str, value: &[u8]) -> Result<RefusedSchedulingChanges> {
    if key != REFUSED_CHANGES_ROW_KEY {
        return Err(Error::Encoding(format!(
            "not a refused-scheduling-changes key: {key:?}"
        )));
    }
    let list: RefusedSchedulingChanges = decode_exact(value)?;
    if let Some(row) = list.rows.iter().find(|row| !row.extra.is_empty()) {
        return Err(Error::Encoding(format!(
            "refused-change row {:?} carries fields this build does not know: {:?}",
            row.key(),
            row.extra.keys().collect::<Vec<_>>()
        )));
    }
    if list.merge(&RefusedSchedulingChanges::default()) != list {
        return Err(Error::Encoding(
            "refused-change list is not held under its ceilings".into(),
        ));
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{MAX_REFUSED_CHANGES_PER_AUTHOR, MAX_REFUSED_SCHEDULING_CHANGES};

    /// A deterministic xorshift64 stream — dependency-free fuzz input.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }
    }

    fn row(
        uid: u64,
        author: u64,
        at: i64,
        occurrences: u32,
        dismissed: u32,
    ) -> RefusedSchedulingChange {
        RefusedSchedulingChange {
            uid_hash: format!("{uid:064x}"),
            author: Some(format!("{:064x}", author + 1)),
            author_home_nest_url: String::new(),
            sender_address: String::new(),
            method: "CANCEL".into(),
            reason: "not_the_organizer".into(),
            summary: if uid.is_multiple_of(2) {
                "Kickoff".into()
            } else {
                "Standup".into()
            },
            first_refused_at: at - 10,
            last_refused_at: at,
            occurrences,
            dismissed_through: dismissed,
            extra: Default::default(),
        }
    }

    /// One replica's held list: a handful of keys over enough authors that
    /// both ceilings cut, the same keys recurring across replicas with
    /// different counts, stamps and dismissals.
    fn replica(rng: &mut Rng) -> RefusedSchedulingChanges {
        let n = rng.next(26);
        let rows = (0..n)
            .map(|_| {
                let occurrences = 1 + rng.next(4) as u32;
                row(
                    rng.next(12),
                    rng.next(9),
                    100 + rng.next(40) as i64,
                    occurrences,
                    rng.next(u64::from(occurrences) + 1) as u32,
                )
            })
            .collect();
        RefusedSchedulingChanges { rows }.merge(&RefusedSchedulingChanges::default())
    }

    fn bytes(list: &RefusedSchedulingChanges) -> Vec<u8> {
        list.encode_row().unwrap()
    }

    /// **The join laws, on bytes, over the capped merge** — commutative,
    /// associative, idempotent, across replicas the ceilings cut. The
    /// associative law is the one a cap can break: a row one merge cuts may
    /// come back through a later replica's newer attempt, and whatever the
    /// cut row carried must then not matter (the lexicographic per-key join,
    /// [`RefusedSchedulingChange::absorb`]).
    #[test]
    fn the_capped_merge_is_a_join_on_bytes() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut cut = 0;
        for _ in 0..3000 {
            let (a, b, c) = (replica(&mut rng), replica(&mut rng), replica(&mut rng));
            let ab = a.merge(&b);
            assert_eq!(bytes(&a.merge(&a)), bytes(&a), "idempotent");
            assert_eq!(bytes(&ab), bytes(&b.merge(&a)), "commutative");
            assert_eq!(
                bytes(&ab.merge(&c)),
                bytes(&a.merge(&b.merge(&c))),
                "associative"
            );
            if a.rows.len() + b.rows.len() > ab.rows.len() + 3 {
                cut += 1;
            }
        }
        assert!(cut > 100, "the fixture must exercise the ceilings ({cut})");
        const {
            assert!(MAX_REFUSED_CHANGES_PER_AUTHOR * 9 > MAX_REFUSED_SCHEDULING_CHANGES);
        }
    }

    /// Record, dismiss, re-open: the owner's gestures on the value type.
    #[test]
    fn a_later_attempt_re_opens_a_dismissed_row() {
        let mut list = RefusedSchedulingChanges::default();
        assert!(list.record(row(1, 1, 100, 9, 9)));
        let key = list.rows[0].key();
        assert_eq!(list.rows[0].occurrences, 1, "the recorder owns the count");
        assert!(list.dismiss(&key));
        assert!(!list.dismiss(&key), "a repeat dismissal changes nothing");
        assert!(list.open().is_empty());
        assert!(list.record(row(1, 1, 200, 1, 0)));
        assert_eq!(list.open().len(), 1, "a later attempt re-opens it");
        assert_eq!(list.rows[0].dismissed_through, 1);
    }

    /// The strict decode: the row's own bytes pass; another key, a newer
    /// field, a non-canonical encoding and a list the ceilings would change
    /// are refused.
    #[test]
    fn a_misfiled_newer_or_unheld_row_is_refused() {
        let mut list = RefusedSchedulingChanges::default();
        list.record(row(1, 1, 100, 1, 0));
        list.record(row(2, 2, 110, 1, 0));
        let value = list.encode_row().unwrap();
        assert_eq!(decode_refused_changes_row("self", &value).unwrap(), list);
        assert!(decode_refused_changes_row("other", &value).is_err());
        let mut newer = list.clone();
        newer.rows[0]
            .extra
            .insert("from_the_future".into(), fauna_cbor::Value::Integer(1));
        assert!(decode_refused_changes_row("self", &newer.encode_row().unwrap()).is_err());
        let mut unsorted = list.clone();
        unsorted.rows.reverse();
        assert!(decode_refused_changes_row("self", &unsorted.encode_row().unwrap()).is_err());
        let mut long = list.clone();
        long.rows[0].summary = "s".repeat(10_000);
        assert!(decode_refused_changes_row("self", &long.encode_row().unwrap()).is_err());
        let mut crowded = RefusedSchedulingChanges {
            rows: (0..5).map(|uid| row(uid, 1, 100, 1, 0)).collect(),
        };
        crowded
            .rows
            .sort_by_cached_key(RefusedSchedulingChange::key);
        assert!(
            decode_refused_changes_row("self", &crowded.encode_row().unwrap()).is_err(),
            "five rows for one author exceed the per-author ceiling"
        );
    }
}
