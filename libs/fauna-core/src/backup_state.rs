//! The backup-destination state: the destination list and the unattested
//! marks raised on carried-across destinations, with the one merge rule
//! the plane arm calls (`config-dissolution.md`, the kinds table's
//! `fauna.state.backup` row; `backup-destinations.md`
//! owns the destinations, `succession-aftermath.md` § Re-key scope the marks).
//!
//! [`BackupState::merge`] is the shipped rule (P1): the marks' verdict-precedence union,
//! then BOTH destination lists pruned of every destination a `Removed` mark
//! names, BEFORE the whole-record latest-wins pick. The pick rides the
//! record's own embedded `updated_at` stamp (P3).

use serde::{Deserialize, Serialize};

use crate::data::{BackupConfig, DestinationUnattestedMark, Timestamp, UnattestedVerdict};
use crate::error::{Error, Result};
use crate::identity::ActorId;
use crate::latest_wins::theirs_wins;
use crate::succession_ledger::verdict_precedence;

/// The backup-destination state of one account: the list the Backups page
/// renders and the backup coordinator drives, the marks the succession
/// aftermath raised on it, and the stamp the list's latest-wins pick rides.
///
/// The composite the plane arm folds to: the destination list + the
/// unattested destination marks + the embedded `updated_at`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupState {
    /// The destination list — pruned of every destination a `Removed` mark
    /// names, once it has been through [`Self::merge`].
    pub backup: BackupConfig,
    /// One mark per `(destination, raising event)`, in canonical order.
    pub marks: Vec<DestinationUnattestedMark>,
    /// The stamp of the write that produced `backup`.
    pub updated_at: Timestamp,
}

impl BackupState {
    /// The cross-device join.
    ///
    /// **The marks union first**, per `(destination_id, predecessor)`, the
    /// surviving verdict the [`verdict_precedence`] minimum, sorted
    /// canonically. **A `Removed` verdict is then ENFORCED, not merely
    /// recorded:** on the member and grant planes the removal happens in
    /// another system entirely (an MLS commit, a nest-side revoke) and the
    /// verdict is only its record; here the removal *is* a deletion inside
    /// `backup`, which rides whole-record latest-wins — so a stale peer's
    /// newer write would resurrect the row, rendering **clean** with the
    /// verdict at rest. Pruning is what makes the verdict mean what it says.
    ///
    /// ⚠ **Both candidates are pruned BEFORE the latest-wins pick, never the
    /// winner after it** — and the difference is idempotence, not taste.
    /// `theirs_wins` is a max over a total order on the compared values, so it
    /// is idempotent only while the value it returns is one of the values it
    /// compares. Filtering the winner returns a *third* value, which the next
    /// merge tiebreaks differently: re-absorbing an input then flips the pick
    /// and `merge(merge(a,b), a) != merge(a,b)`, the exact ping-pong the join
    /// laws exist to forbid. Pruning first keeps the winner an element of the
    /// candidate set, so the max stays a max.
    ///
    /// Convergent by construction: both replicas prune against the same
    /// unioned marks, and `Removed` is the precedence minimum, so no later
    /// merge can un-prune a row. Safe against a genuine re-add because
    /// `destination_id` is a fresh uuid per enroll, so a re-added destination
    /// can never collide with a removed one's key.
    ///
    /// The destination list itself deliberately stays whole-record
    /// latest-wins: a destination row is a *recreatable* preference
    /// (re-addable from any app's Backups page), and latest-wins is what keeps
    /// a **removal** propagating across the fleet — on this plane the
    /// security-relevant direction, since the row being removed is often the
    /// one a seed thief planted.
    ///
    /// The stamp of the result is the winner's.
    #[must_use]
    pub fn merge(&self, other: &Self) -> Self {
        let marks = join_marks(&self.marks, &other.marks);
        let ours = prune_removed(&self.backup, &marks);
        let theirs = prune_removed(&other.backup, &marks);
        let (backup, updated_at) = if theirs_wins(&ours, self.updated_at, &theirs, other.updated_at)
        {
            (theirs, other.updated_at)
        } else {
            (ours, self.updated_at)
        };
        Self {
            backup,
            marks,
            updated_at,
        }
    }
}

/// The marks' union: per `(destination_id, predecessor)`, the
/// [`verdict_precedence`] minimum; sorted canonically.
fn join_marks(
    ours: &[DestinationUnattestedMark],
    theirs: &[DestinationUnattestedMark],
) -> Vec<DestinationUnattestedMark> {
    let mut merged: Vec<DestinationUnattestedMark> = ours.to_vec();
    for theirs in theirs {
        match merged.iter_mut().find(|ours| {
            ours.destination_id == theirs.destination_id && ours.predecessor == theirs.predecessor
        }) {
            Some(ours) => *ours = ours.join(theirs),
            None => merged.push(theirs.clone()),
        }
    }
    merged.sort_by(|a, b| {
        (a.destination_id.as_str(), a.predecessor.0)
            .cmp(&(b.destination_id.as_str(), b.predecessor.0))
    });
    merged
}

/// `backup` without every destination a `Removed` mark in `marks` names.
fn prune_removed(backup: &BackupConfig, marks: &[DestinationUnattestedMark]) -> BackupConfig {
    let mut pruned = backup.clone();
    pruned.destinations.retain(|dest| {
        !marks.iter().any(|m| {
            m.destination_id == dest.destination_id
                && matches!(m.verdict, UnattestedVerdict::Removed)
        })
    });
    pruned
}

impl DestinationUnattestedMark {
    /// The per-mark join: `other`'s verdict replaces this one's only when it
    /// ranks strictly lower ([`verdict_precedence`]). Both sides
    /// must name the same `(destination, predecessor)`; the result keeps this
    /// side's identity fields.
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        let mut joined = self.clone();
        if verdict_precedence(&other.verdict) < verdict_precedence(&self.verdict) {
            joined.verdict = other.verdict.clone();
        }
        joined
    }

    /// This mark's plane-row key: `mark/<destination_id>/<predecessor hex64>`.
    #[must_use]
    pub fn plane_key(&self) -> String {
        BackupRowKey::Mark {
            destination_id: self.destination_id.clone(),
            predecessor: self.predecessor,
        }
        .render()
    }
}

// ── The plane rows (`fauna.state.backup`; `config-dissolution.md` § Phases
// and gates → *Bounded rows*) ──
//
// Two row families, the key's first segment dispatching, as the lead slice's
// do. The marks are **one row per mark** — the ledger ruling's shape for its
// sibling mark planes: an adjudicated mark is kept, never deleted, so a
// per-account mark list grows with every succession for good. The destination
// list is **one row per source box** (`destinations/<source nest hex64>`,
// the value naming its key): a destination list is a fact about one box
// (`backup-destinations.md` § State & data shape → *Destination data model*),
// and within a box it is whole-record latest-wins by design (a removal must
// propagate). Each list row is bounded by construction — every text field
// under its cap and the encoded value under
// [`MAX_BACKUP_DESTINATIONS_VALUE_BYTES`] ([`BackupDestinationsRow::check_bounds`],
// which the plane door enforces) — so it seals under half the per-entry cap
// (the size pin in `fauna_protocol::merge_policy`'s tests).
//
// The `Removed` prune runs at READ on the plane ([`BackupState::from_rows`]):
// the list row's arm is the latest-wins half alone, and the fold prunes the
// bound box's list against every mark of the account. Every replica holding
// the same rows reads the same state, and a stale peer's newer list still
// reads pruned, because the `Removed` mark rides its own row.
//
// Decode posture (P4, CrdtPerField): **strict** — a row value must re-encode to
// exactly its own bytes ([`decode_backup_row`]). The value types keep their
// tolerant serde (a `deny_unknown_fields` on `BackupDestination` would
// make an older build refuse a newer build's record), so the
// round-trip is what refuses a newer writer's field at any depth instead of
// silently stripping it from the re-encoded row.

/// Key prefix of a destination-list row: `destinations/<source nest hex64>`.
pub const DESTINATIONS_KEY_PREFIX: &str = "destinations/";

/// Key prefix of a mark row: `mark/<destination_id>/<predecessor hex64>`.
pub const MARK_KEY_PREFIX: &str = "mark/";

/// The ceiling on a destination-list row's canonical value bytes. The row's
/// size bound: the door refuses a list whose row would encode longer, and
/// the size pin holds a row at exactly this size, sealed, under half the
/// per-entry cap. A byte bound, not an entry count (ruled 2026-09-30,
/// *Bounded rows* → *The backup state*): a coverage row is one entry per
/// (destination, covered folder), and a real entry is about a third of one
/// with every field at its cap, so the byte bound admits the entries a box
/// actually lists with the same safety a count sized for at-cap entries gave.
pub const MAX_BACKUP_DESTINATIONS_VALUE_BYTES: usize = 30 * 1024;

/// Byte cap on `destination_id` (a uuid in every writer).
pub const MAX_DESTINATION_ID_BYTES: usize = 64;
/// Byte cap on `destination_nest_url`.
pub const MAX_DESTINATION_URL_BYTES: usize = 256;
/// Byte cap on `folder_name` — a reserved folder name, or a covered folder's
/// set name `__folder/<source nest hex64>/<folder id>`, which runs to 93 B at
/// the longest folder id.
pub const MAX_DESTINATION_FOLDER_BYTES: usize = 96;
/// Byte cap on `display_name`.
pub const MAX_DESTINATION_LABEL_BYTES: usize = 128;
/// Byte cap on `kind` (a discriminator string).
pub const MAX_DESTINATION_KIND_BYTES: usize = 32;
/// Byte cap on `custodian_device_id`.
pub const MAX_CUSTODIAN_DEVICE_ID_BYTES: usize = 128;

/// The key of `source_nest`'s destination-list row.
#[must_use]
pub fn destinations_key(source_nest: &[u8; 32]) -> String {
    BackupRowKey::Destinations {
        source_nest: *source_nest,
    }
    .render()
}

/// A destination-list row's value: the box it lists for, the list, and its
/// own stamp (P3 — the latest-wins half rides this embedded
/// stamp).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupDestinationsRow {
    /// The source box's identity as the connection proves it — the row's
    /// key names the same box ([`decode_backup_row`] refuses a mismatch).
    #[serde(with = "serde_bytes")]
    pub source_nest: [u8; 32],
    /// The destination list, as its writer left it (unpruned: the prune is
    /// the read fold's).
    pub backup: BackupConfig,
    /// The writer's stamp — strictly above the stored row's, so a write on a
    /// device whose clock trails is still the newer one.
    pub updated_at: Timestamp,
}

impl BackupDestinationsRow {
    /// The row arm's join: whole-record latest-wins on the embedded stamp,
    /// an equal stamp settled by the list itself — the same `theirs_wins`
    /// every whole-record kind picks with, so a max over a total order (a join on
    /// bytes). Both sides are one box's row ([`BackupRecord::merge`] refuses
    /// two boxes').
    #[must_use]
    pub fn merge(&self, other: &Self) -> Self {
        if theirs_wins(
            &self.backup,
            self.updated_at,
            &other.backup,
            other.updated_at,
        ) {
            other.clone()
        } else {
            self.clone()
        }
    }

    /// The row's size bound (*Bounded rows*): each text field under its cap,
    /// and the canonical value under [`MAX_BACKUP_DESTINATIONS_VALUE_BYTES`].
    ///
    /// # Errors
    /// The first bound the row breaks — [`BackupBoundsError`] says which.
    pub fn check_bounds(&self) -> std::result::Result<(), BackupBoundsError> {
        for d in &self.backup.destinations {
            let fields: [(&'static str, usize, usize); 6] = [
                (
                    "destination_id",
                    d.destination_id.len(),
                    MAX_DESTINATION_ID_BYTES,
                ),
                (
                    "destination_nest_url",
                    d.destination_nest_url.len(),
                    MAX_DESTINATION_URL_BYTES,
                ),
                (
                    "folder_name",
                    d.folder_name.len(),
                    MAX_DESTINATION_FOLDER_BYTES,
                ),
                (
                    "display_name",
                    d.display_name.as_deref().map_or(0, str::len),
                    MAX_DESTINATION_LABEL_BYTES,
                ),
                ("kind", d.kind.len(), MAX_DESTINATION_KIND_BYTES),
                (
                    "custodian_device_id",
                    d.custodian_device_id.as_deref().map_or(0, str::len),
                    MAX_CUSTODIAN_DEVICE_ID_BYTES,
                ),
            ];
            for (field, len, cap) in fields {
                if len > cap {
                    return Err(BackupBoundsError::FieldTooLong {
                        destination_id: d.destination_id.clone(),
                        field,
                        len,
                        cap,
                    });
                }
            }
        }
        let len = crate::encoding::canonical_encode(self)
            .map_err(|e| BackupBoundsError::Encode(e.to_string()))?
            .len();
        if len > MAX_BACKUP_DESTINATIONS_VALUE_BYTES {
            return Err(BackupBoundsError::ListFull { len });
        }
        Ok(())
    }
}

/// Why a destination list is outside its row's bounds.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackupBoundsError {
    /// The list as a whole is full: its row would encode past
    /// [`MAX_BACKUP_DESTINATIONS_VALUE_BYTES`]. The user-facing refusal —
    /// remove a destination or a covered folder first.
    #[error(
        "backup destinations: the row encodes to {len} B, over the {MAX_BACKUP_DESTINATIONS_VALUE_BYTES} B ceiling"
    )]
    ListFull { len: usize },
    /// One entry's text field is over its cap.
    #[error("backup destination {destination_id:?}: {field} is {len} B, over its {cap} B cap")]
    FieldTooLong {
        destination_id: String,
        field: &'static str,
        len: usize,
        cap: usize,
    },
    /// The row did not encode.
    #[error("backup destinations: {0}")]
    Encode(String),
}

/// A parsed `fauna.state.backup` row key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupRowKey {
    /// `destinations/<source nest hex64>` — holds that box's one
    /// [`BackupDestinationsRow`].
    Destinations { source_nest: [u8; 32] },
    /// `mark/<destination_id>/<predecessor hex64>` — holds one
    /// [`DestinationUnattestedMark`].
    Mark {
        destination_id: String,
        predecessor: ActorId,
    },
}

impl BackupRowKey {
    /// Parse a row key. Strict: only the canonical spelling parses (a
    /// lowercase 64-hex source nest; a non-empty destination id and a
    /// lowercase 64-hex predecessor as the last segment — fixed-width, so the
    /// split is unambiguous whatever the id holds), so one row has one key.
    ///
    /// # Errors
    /// Any other string — the bare `destinations` phase A keyed included.
    pub fn parse(key: &str) -> Result<Self> {
        if let Some(source_nest) = key.strip_prefix(DESTINATIONS_KEY_PREFIX)
            && crate::hex32::is_lowercase_hex64(source_nest)
        {
            let source_nest =
                crate::hex32::decode(source_nest).map_err(|e| Error::Encoding(e.to_string()))?;
            return Ok(Self::Destinations { source_nest });
        }
        if let Some(rest) = key.strip_prefix(MARK_KEY_PREFIX)
            && let Some((destination_id, predecessor)) = rest.rsplit_once('/')
            && !destination_id.is_empty()
            && crate::hex32::is_lowercase_hex64(predecessor)
        {
            let predecessor =
                crate::hex32::decode(predecessor).map_err(|e| Error::Encoding(e.to_string()))?;
            return Ok(Self::Mark {
                destination_id: destination_id.to_string(),
                predecessor: ActorId(predecessor),
            });
        }
        Err(Error::Encoding(format!("not a backup-state key: {key:?}")))
    }

    /// The canonical key string.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Destinations { source_nest } => format!(
                "{DESTINATIONS_KEY_PREFIX}{}",
                crate::hex32::encode(source_nest)
            ),
            Self::Mark {
                destination_id,
                predecessor,
            } => format!(
                "{MARK_KEY_PREFIX}{destination_id}/{}",
                crate::hex32::encode(&predecessor.0)
            ),
        }
    }
}

/// One plane row's value: the record its key names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupRecord {
    Destinations(BackupDestinationsRow),
    Mark(DestinationUnattestedMark),
}

impl BackupRecord {
    /// The canonical value bytes.
    ///
    /// # Errors
    /// Canonical-encoding failure.
    pub fn encode(&self) -> Result<Vec<u8>> {
        match self {
            Self::Destinations(r) => crate::encoding::canonical_encode(r),
            Self::Mark(m) => crate::encoding::canonical_encode(m),
        }
    }

    /// The per-row join — the halves [`BackupState::merge`] runs.
    ///
    /// # Errors
    /// The two sides are different row types, two boxes' lists, or
    /// different marks.
    pub fn merge(&self, other: &Self) -> Result<Self> {
        match (self, other) {
            (Self::Destinations(a), Self::Destinations(b)) if a.source_nest == b.source_nest => {
                Ok(Self::Destinations(a.merge(b)))
            }
            (Self::Mark(a), Self::Mark(b))
                if a.destination_id == b.destination_id && a.predecessor == b.predecessor =>
            {
                Ok(Self::Mark(a.join(b)))
            }
            _ => Err(Error::Encoding(
                "backup-state rows name different records".into(),
            )),
        }
    }
}

/// Decode one `fauna.state.backup` row: the key picks the record type, the
/// value must decode as it AND re-encode to exactly its own bytes (the strict
/// posture — see the section comment), and the value must name its own key
/// (a list row its box, a mark its destination and predecessor).
///
/// # Errors
/// An unparseable key, an undecodable or non-round-tripping value, or a
/// key/value mismatch.
pub fn decode_backup_row(key: &str, value: &[u8]) -> Result<BackupRecord> {
    let record = match BackupRowKey::parse(key)? {
        BackupRowKey::Destinations { source_nest } => {
            let row: BackupDestinationsRow = crate::encoding::canonical_decode(value)?;
            if row.source_nest != source_nest {
                return Err(Error::Encoding(format!(
                    "backup-state row at {key:?} holds the list of box {}",
                    crate::hex32::encode(&row.source_nest)
                )));
            }
            BackupRecord::Destinations(row)
        }
        BackupRowKey::Mark { .. } => BackupRecord::Mark(crate::encoding::canonical_decode(value)?),
    };
    if record.encode()? != value {
        return Err(Error::Encoding(format!(
            "backup-state row {key:?} does not round-trip (a field this build does not know?)"
        )));
    }
    if let BackupRecord::Mark(m) = &record
        && m.plane_key() != key
    {
        return Err(Error::Encoding(format!(
            "backup-state row at {key:?} holds the mark for {:?}",
            m.plane_key()
        )));
    }
    Ok(record)
}

/// Every destination-list row among an account's rows, decoded — one per
/// box the account has kept a list on, in key order as given. The all-boxes
/// read: the succession aftermath's mark raise marks every destination any
/// of them lists, and the rotated-box re-file looks for its predecessor's
/// among them (`backup-destinations.md` § *Destination data model*).
///
/// # Errors
/// A row [`decode_backup_row`] refuses.
pub fn destination_lists<'a>(
    rows: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Result<Vec<BackupDestinationsRow>> {
    let mut lists = Vec::new();
    for (key, value) in rows {
        if let BackupRecord::Destinations(row) = decode_backup_row(key, value)? {
            lists.push(row);
        }
    }
    Ok(lists)
}

/// Every mark of the account among its rows, unioned and in canonical
/// order — the box-free half of the fold, which the mark door answers.
///
/// # Errors
/// A row [`decode_backup_row`] refuses.
pub fn destination_marks<'a>(
    rows: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Result<Vec<DestinationUnattestedMark>> {
    let mut marks = Vec::new();
    for (key, value) in rows {
        if let BackupRecord::Mark(m) = decode_backup_row(key, value)? {
            marks = join_marks(&marks, &[m]);
        }
    }
    Ok(marks)
}

impl BackupState {
    /// This state as `source_nest`'s plane rows, `(key, record)`: that box's
    /// list row, then every mark in canonical order.
    #[must_use]
    pub fn rows(&self, source_nest: &[u8; 32]) -> Vec<(String, BackupRecord)> {
        std::iter::once((
            destinations_key(source_nest),
            BackupRecord::Destinations(BackupDestinationsRow {
                source_nest: *source_nest,
                backup: self.backup.clone(),
                updated_at: self.updated_at,
            }),
        ))
        .chain(
            self.marks
                .iter()
                .map(|m| (m.plane_key(), BackupRecord::Mark(m.clone()))),
        )
        .collect()
    }

    /// The READ fold over an account's rows, for ONE box: the list is the
    /// row at `destinations/<source_nest>` (the empty list at stamp 0 when
    /// there is none), every mark row of the account is unioned whatever box
    /// lists its destination, and the list is pruned of every destination a
    /// `Removed` mark names. A list row under another box's identity is
    /// decoded and otherwise ignored — never merged in, never rendered,
    /// never healed from (`backup-destinations.md` § *Destination data
    /// model*).
    ///
    /// # Errors
    /// A row [`decode_backup_row`] refuses — silently skipping one would let
    /// the next write put a bare list over state it held.
    pub fn from_rows<'a>(
        rows: impl IntoIterator<Item = (&'a str, &'a [u8])>,
        source_nest: &[u8; 32],
    ) -> Result<Self> {
        let mut list: Option<BackupDestinationsRow> = None;
        let mut marks: Vec<DestinationUnattestedMark> = Vec::new();
        for (key, value) in rows {
            match decode_backup_row(key, value)? {
                BackupRecord::Destinations(row) if row.source_nest == *source_nest => {
                    list = Some(match list {
                        Some(cur) => cur.merge(&row),
                        None => row,
                    });
                }
                BackupRecord::Destinations(_) => {}
                BackupRecord::Mark(m) => marks = join_marks(&marks, &[m]),
            }
        }
        let list = list.unwrap_or_default();
        Ok(Self {
            backup: prune_removed(&list.backup, &marks),
            marks,
            updated_at: list.updated_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::BackupDestination;

    fn dest(id: &str) -> BackupDestination {
        BackupDestination {
            destination_id: id.to_string(),
            destination_nest_url: format!("https://{id}.example"),
            folder_name: "__mail".to_string(),
            ..Default::default()
        }
    }

    fn mark(id: &str, pred: u8, verdict: UnattestedVerdict) -> DestinationUnattestedMark {
        DestinationUnattestedMark {
            destination_id: id.to_string(),
            predecessor: ActorId([pred; 32]),
            verdict,
        }
    }

    fn state(ids: &[&str], marks: Vec<DestinationUnattestedMark>, at: u64) -> BackupState {
        BackupState {
            backup: BackupConfig {
                destinations: ids.iter().map(|i| dest(i)).collect(),
            },
            marks,
            updated_at: Timestamp(at),
        }
    }

    fn samples() -> Vec<BackupState> {
        vec![
            state(&["a", "b"], vec![mark("a", 1, UnattestedVerdict::Open)], 10),
            state(&["b"], vec![mark("a", 1, UnattestedVerdict::Removed)], 5),
            state(&["c"], vec![mark("b", 2, UnattestedVerdict::Kept)], 10),
            state(
                &["a", "c"],
                vec![mark("c", 3, UnattestedVerdict::Other("later".into()))],
                12,
            ),
            BackupState::default(),
        ]
    }

    #[test]
    fn backup_state_merge_is_a_join() {
        let s = samples();
        for a in &s {
            assert_eq!(a.merge(a), *a, "idempotent");
            for b in &s {
                assert_eq!(a.merge(b), b.merge(a), "commutative");
                for c in &s {
                    assert_eq!(a.merge(b).merge(c), a.merge(&b.merge(c)), "associative");
                }
            }
        }
    }

    #[test]
    fn a_removed_mark_prunes_even_a_newer_list() {
        let merged = state(&["a", "b"], vec![], 20).merge(&state(
            &["b"],
            vec![mark("a", 1, UnattestedVerdict::Removed)],
            5,
        ));
        let ids: Vec<_> = merged
            .backup
            .destinations
            .iter()
            .map(|d| d.destination_id.as_str())
            .collect();
        assert_eq!(ids, ["b"]);
        assert_eq!(merged.updated_at, Timestamp(20));
    }

    const BOX_A: [u8; 32] = [0xA1; 32];
    const BOX_B: [u8; 32] = [0xB2; 32];

    fn list_row(source_nest: [u8; 32], ids: &[&str], at: u64) -> BackupDestinationsRow {
        BackupDestinationsRow {
            source_nest,
            backup: BackupConfig {
                destinations: ids.iter().map(|i| dest(i)).collect(),
            },
            updated_at: Timestamp(at),
        }
    }

    fn encoded(rows: &[(String, BackupRecord)]) -> Vec<(String, Vec<u8>)> {
        rows.iter()
            .map(|(k, r)| (k.clone(), r.encode().unwrap()))
            .collect()
    }

    fn fold(rows: &[(String, Vec<u8>)], source_nest: &[u8; 32]) -> BackupState {
        BackupState::from_rows(
            rows.iter().map(|(k, v)| (k.as_str(), v.as_slice())),
            source_nest,
        )
        .unwrap()
    }

    #[test]
    fn row_keys_round_trip_and_strangers_are_refused() {
        for key in [
            destinations_key(&BOX_A),
            mark("a/b-with-slash", 7, UnattestedVerdict::Open).plane_key(),
        ] {
            assert_eq!(BackupRowKey::parse(&key).unwrap().render(), key);
        }
        for bad in [
            "self",
            // The account-wide key phase A built — retired with no alias.
            "destinations",
            "destinations/",
            "destinations/x",
            "destinations/A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1",
            "mark/",
            "mark//0000000000000000000000000000000000000000000000000000000000000000",
            "mark/a/ABCD",
            "mark/a",
        ] {
            assert!(BackupRowKey::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_rows_fold_back_to_the_merged_state() {
        let s = samples();
        let merged = s.iter().fold(BackupState::default(), |acc, x| acc.merge(x));
        assert_eq!(fold(&encoded(&merged.rows(&BOX_A)), &BOX_A), merged);
    }

    /// The per-box read (`backup-destinations.md` § *Destination data
    /// model*): a box reads its own list and no other's, while every mark of
    /// the account prunes whichever box lists the destination.
    #[test]
    fn a_box_reads_its_own_list_and_every_mark() {
        let rows = encoded(&[
            (
                destinations_key(&BOX_A),
                BackupRecord::Destinations(list_row(BOX_A, &["a", "x"], 5)),
            ),
            (
                destinations_key(&BOX_B),
                BackupRecord::Destinations(list_row(BOX_B, &["b"], 9)),
            ),
            (
                mark("x", 1, UnattestedVerdict::Removed).plane_key(),
                BackupRecord::Mark(mark("x", 1, UnattestedVerdict::Removed)),
            ),
        ]);
        let ids = |s: &BackupState| -> Vec<String> {
            s.backup
                .destinations
                .iter()
                .map(|d| d.destination_id.clone())
                .collect()
        };
        let on_a = fold(&rows, &BOX_A);
        assert_eq!(ids(&on_a), ["a"], "A's list, the removed one pruned");
        assert_eq!(on_a.updated_at, Timestamp(5));
        assert_eq!(on_a.marks.len(), 1);
        assert_eq!(ids(&fold(&rows, &BOX_B)), ["b"]);
        let unknown = fold(&rows, &[0xCC; 32]);
        assert!(
            unknown.backup.destinations.is_empty(),
            "never another box's"
        );
        assert_eq!(unknown.updated_at, Timestamp(0));
        // The all-boxes read sees both lists.
        let lists =
            destination_lists(rows.iter().map(|(k, v)| (k.as_str(), v.as_slice()))).unwrap();
        assert_eq!(lists.len(), 2);
    }

    #[test]
    fn two_boxes_lists_never_merge() {
        let a = BackupRecord::Destinations(list_row(BOX_A, &["a"], 1));
        let b = BackupRecord::Destinations(list_row(BOX_B, &["a"], 1));
        assert!(a.merge(&b).is_err());
        assert!(a.merge(&a).is_ok());
    }

    #[test]
    fn a_misfiled_row_or_an_unknown_field_is_refused() {
        let m = mark("a", 1, UnattestedVerdict::Open);
        let bytes = crate::encoding::canonical_encode(&m).unwrap();
        let other_key = mark("b", 1, UnattestedVerdict::Open).plane_key();
        assert!(decode_backup_row(&other_key, &bytes).is_err(), "misfiled");
        assert!(decode_backup_row(&m.plane_key(), &bytes).is_ok());

        // A list row filed under another box's key.
        let list = crate::encoding::canonical_encode(&list_row(BOX_A, &["a"], 1)).unwrap();
        assert!(decode_backup_row(&destinations_key(&BOX_A), &list).is_ok());
        assert!(
            decode_backup_row(&destinations_key(&BOX_B), &list).is_err(),
            "a list names its own box"
        );

        // A newer build's field nested inside a destination: decodes
        // tolerantly, but does not round-trip, so the row is refused rather
        // than stripped.
        #[derive(Serialize)]
        struct NewerDest<'a> {
            #[serde(flatten)]
            base: &'a BackupDestination,
            zz_newer: u8,
        }
        #[derive(Serialize)]
        struct NewerConfig<'a> {
            destinations: Vec<NewerDest<'a>>,
        }
        #[derive(Serialize)]
        struct NewerRow<'a> {
            #[serde(with = "serde_bytes")]
            source_nest: [u8; 32],
            backup: NewerConfig<'a>,
            updated_at: Timestamp,
        }
        let d = dest("a");
        let newer = crate::encoding::canonical_encode(&NewerRow {
            source_nest: BOX_A,
            backup: NewerConfig {
                destinations: vec![NewerDest {
                    base: &d,
                    zz_newer: 1,
                }],
            },
            updated_at: Timestamp(1),
        })
        .unwrap();
        assert!(decode_backup_row(&destinations_key(&BOX_A), &newer).is_err());
    }

    #[test]
    fn the_bounds_refuse_a_full_list_or_an_oversized_field() {
        let row = |n: usize| BackupDestinationsRow {
            source_nest: BOX_A,
            backup: BackupConfig {
                destinations: (0..n).map(|i| dest(&format!("d{i}"))).collect(),
            },
            updated_at: Timestamp(1),
        };
        // A real-sized list well past the count phase A capped at.
        assert!(row(64).check_bounds().is_ok());
        let full = (1..)
            .find(|n| row(*n).check_bounds().is_err())
            .expect("the byte ceiling binds");
        assert!(matches!(
            row(full).check_bounds(),
            Err(BackupBoundsError::ListFull { .. })
        ));
        let mut long = row(1);
        long.backup.destinations[0].destination_nest_url =
            "x".repeat(MAX_DESTINATION_URL_BYTES + 1);
        assert!(matches!(
            long.check_bounds(),
            Err(BackupBoundsError::FieldTooLong { .. })
        ));
        // A covered folder's set name at the longest folder id fits.
        let mut covered = row(1);
        covered.backup.destinations[0].folder_name =
            format!("__folder/{}/{}", "a".repeat(64), i64::MAX);
        assert!(covered.check_bounds().is_ok());
    }
}
