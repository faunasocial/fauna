//! The `fauna.state.folder-keys` plane rows — the key grammar, the row values,
//! the per-row join and the READ fold of the account's shared-folder
//! content-key custody (`config-dissolution.md` owns
//! the kind's birth, plane-only, and § Phases and gates →
//! *Bounded rows* the row shape; `mls-group-key-material.md` § M2 content-key
//! mechanism → *Custody (per holder)* owns what the keys are;
//! [`FoldersConfig::merge`] owns the shipped rule).
//!
//! **One row per entity, never one per account.** `FoldersConfig` is made of
//! per-entity sub-records carrying key material, and one of them grows with
//! use: a set's generation history gains a generation on every member removal
//! and can never be pruned (a content key, once distributed, cannot be
//! regenerated — the bounded-set-plus-marker remedy is refused, as for the
//! ledger). The kind's rows, the key's first segment dispatching:
//!
//! - `set/nonce/<hex64>` or `set/channel/<hex64>` holds one custody entry's
//!   metadata ([`FolderSetRecord`] — nonce, channel, name, create stamp,
//!   tombstone), keyed by the entry's identity exactly as
//!   [`FoldersConfig::merge`] keys it: the nonce, else (a member's first
//!   ingest, a name-less bind) the channel ([`SetIdentity`]).
//! - `gen/<digest hex64>` holds ONE content-key generation and the identity of
//!   the set it belongs to ([`FolderGenerationRow`]), the digest covering the
//!   whole row. Write-once. This is what bounds the history: each generation
//!   is its own small fixed row, however many removals a set sees — and two
//!   devices rotating concurrently to one version each write their own row,
//!   so both keys survive (the shipped rule keeps the loser in `prior`).
//! - `removal/<digest hex64>` holds one staged member-removal rotation and its
//!   `settled` marker ([`FolderRemovalRow`]), the digest over the staging
//!   identity the merge dedups on (every field but the two it folds, `commit`
//!   and `gated_attempted`, so enriching or settling a staging keeps its key).
//!   A CRDT kind has no deletion, so a staging the orchestration is done with
//!   is *settled*, never dropped — its fresh generation written as a `gen/`
//!   row by the same settle — and leaves the fold on every replica; a stale
//!   replica's unsettled copy cannot resurrect it (the
//!   `fauna.state.subscriptions` removal row's shape).
//! - `foreign/<channel hex64>` holds one foreign set ([`ForeignFolder`]).
//!
//! Digests are [`canonical_tiebreak_key`] (BLAKE3 over the canonical encoding,
//! the plaintext buffer zeroized), as the subscriptions rows'. Row keys are
//! blinded on the wire, and a 256-bit digest of a record carrying a uniformly
//! random 256-bit key reveals nothing of it.
//!
//! **One statement of the rule (P1).** Every row is a single-entity
//! `FoldersConfig` ([`FolderKeyRecord::as_config`]); the plane arm is
//! [`FoldersConfig::merge`] over two of them ([`FolderKeyRecord::merge`]; the
//! settled marker ORs beside it), and the READ fold ([`FoldersConfig::fold`])
//! is the same merge over every row but the settled removals — so folding two
//! replicas' rows gives exactly [`FoldersConfig::merge`] of the two.
//!
//! **What the plane does not drop.** Beyond a settled removal, a consumer may drop a
//! record in place that has no plane twin: a forgotten
//! foreign record, the entry `migrate_set_identity` joins into another, an
//! entry whose identity moves from channel to nonce. Each stays a row, as each
//! already resurrects from any stale device; each reader decides its
//! treatment of them.
//!
//! **Decode posture — strict.** A row's value must decode as its key's type,
//! re-encode to the SAME canonical bytes and name its key. The three row types
//! born here carry `deny_unknown_fields` besides; the value types the rows
//! carry (`ContentKeyGeneration`, `FolderPendingRemoval`, `ForeignFolder`)
//! keep their tolerant serde, so the round-trip check is what refuses a newer build's field on
//! the plane rather than silently stripping it.

use serde::{Deserialize, Serialize};

use crate::data::{FolderKeyCustody, FolderPendingRemoval, FoldersConfig, ForeignFolder};
use crate::encoding::{canonical_decode, canonical_encode, canonical_tiebreak_key};
use crate::error::{Error, Result};
use crate::folder_keys::{ContentKeyGeneration, FolderContentKeys};
use crate::identity::ActorId;

/// Key prefix of a custody entry's metadata row.
pub const SET_KEY_PREFIX: &str = "set/";
/// Key prefix of a generation row.
pub const GEN_KEY_PREFIX: &str = "gen/";
/// Key prefix of a staged-removal row.
pub const REMOVAL_KEY_PREFIX: &str = "removal/";
/// Key prefix of a foreign-set row.
pub const FOREIGN_KEY_PREFIX: &str = "foreign/";

/// A custody entry's identity — [`FoldersConfig::merge`]'s union key: the set
/// nonce, else the channel.
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `consensus` ground: every reader must reach the same
/// decision from the value, so there is no unknown arm and a reader that cannot
/// decode one fails rather than guess). A new variant is an edit to
/// `tools/check-additive-evolution/enum_ledger.txt`, made in the same change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SetIdentity {
    Nonce(#[serde(with = "serde_bytes")] [u8; 32]),
    Channel(#[serde(with = "serde_bytes")] [u8; 32]),
}

impl SetIdentity {
    /// The identity of `entry`, or `None` for an entry with neither a nonce
    /// nor a channel (never produced; such an entry has no row).
    #[must_use]
    pub fn of(entry: &FolderKeyCustody) -> Option<Self> {
        match (entry.set_nonce, entry.channel_id) {
            (Some(nonce), _) => Some(Self::Nonce(nonce)),
            (None, Some(channel)) => Some(Self::Channel(channel)),
            (None, None) => None,
        }
    }

    /// The `set/` row key of this identity.
    #[must_use]
    pub fn plane_key(&self) -> String {
        match self {
            Self::Nonce(n) => format!("{SET_KEY_PREFIX}nonce/{}", crate::hex32::encode(n)),
            Self::Channel(c) => format!("{SET_KEY_PREFIX}channel/{}", crate::hex32::encode(c)),
        }
    }

    /// The bare entry carrying only this identity — what a generation row
    /// folds into.
    fn bare_entry(&self) -> FolderKeyCustody {
        match *self {
            Self::Nonce(n) => FolderKeyCustody {
                set_nonce: Some(n),
                ..Default::default()
            },
            Self::Channel(c) => FolderKeyCustody {
                channel_id: Some(c),
                ..Default::default()
            },
        }
    }
}

/// A custody entry's metadata — [`FolderKeyCustody`] without its `keys`,
/// which live one generation per `gen/` row.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderSetRecord {
    /// [`FolderKeyCustody::channel_id`].
    #[serde(with = "serde_bytes")]
    pub channel_id: Option<[u8; 32]>,
    /// [`FolderKeyCustody::set_nonce`].
    #[serde(with = "serde_bytes")]
    pub set_nonce: Option<[u8; 32]>,
    /// [`FolderKeyCustody::name`].
    pub name: Option<String>,
    /// [`FolderKeyCustody::created_at`].
    pub created_at: u64,
    /// [`FolderKeyCustody::retired_at`].
    pub retired_at: Option<u64>,
    /// [`FolderKeyCustody::lifted_at`] — absent from the encoding while
    /// `None`, so a record that never saw a lift keeps its bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifted_at: Option<u64>,
    /// [`FolderKeyCustody::minted_by`] — absent while `None`, as `lifted_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minted_by: Option<ActorId>,
    /// [`FolderKeyCustody::replaces`] — absent while `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "serde_bytes")]
    pub replaces: Option<[u8; 32]>,
    /// [`FolderKeyCustody::retired_by_pick`] — absent while `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "serde_bytes")]
    pub retired_by_pick: Option<[u8; 32]>,
    /// [`FolderKeyCustody::served_at`] — absent while `None`, so a
    /// never-served record keeps its bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served_at: Option<u64>,
    /// [`FolderKeyCustody::unserved_at`] — absent while `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unserved_at: Option<u64>,
}

impl FolderSetRecord {
    /// `entry`'s metadata.
    #[must_use]
    pub fn of(entry: &FolderKeyCustody) -> Self {
        Self {
            channel_id: entry.channel_id,
            set_nonce: entry.set_nonce,
            name: entry.name.clone(),
            created_at: entry.created_at,
            retired_at: entry.retired_at,
            lifted_at: entry.lifted_at,
            minted_by: entry.minted_by,
            replaces: entry.replaces,
            retired_by_pick: entry.retired_by_pick,
            served_at: entry.served_at,
            unserved_at: entry.unserved_at,
        }
    }

    /// The key-less custody entry this record describes.
    #[must_use]
    pub fn to_entry(&self) -> FolderKeyCustody {
        FolderKeyCustody {
            channel_id: self.channel_id,
            keys: None,
            set_nonce: self.set_nonce,
            name: self.name.clone(),
            created_at: self.created_at,
            retired_at: self.retired_at,
            lifted_at: self.lifted_at,
            minted_by: self.minted_by,
            replaces: self.replaces,
            retired_by_pick: self.retired_by_pick,
            served_at: self.served_at,
            unserved_at: self.unserved_at,
        }
    }
}

/// One content-key generation of one set — the value of a `gen/` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderGenerationRow {
    /// The set the generation belongs to.
    pub set: SetIdentity,
    /// The generation.
    pub generation: ContentKeyGeneration,
}

impl FolderGenerationRow {
    /// The row's plane key — the digest of the whole row.
    #[must_use]
    pub fn plane_key(&self) -> String {
        format!(
            "{GEN_KEY_PREFIX}{}",
            crate::hex32::encode(&canonical_tiebreak_key(self))
        )
    }
}

/// One staged member-removal rotation and its `settled` marker — the value of
/// a `removal/` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderRemovalRow {
    /// The staged removal, carrying its irrecoverable fresh generation.
    pub removal: FolderPendingRemoval,
    /// Monotone (OR): once any device settles the removal it stays settled. A
    /// settled row leaves the fold's `pending_removals`; its fresh generation
    /// is written as a `gen/` row by the same settle
    /// (`fauna_account_plane::folder_key_rows::settle_pending_removal`), so
    /// settling never strands the key.
    pub settled: bool,
}

impl FolderRemovalRow {
    /// The row's plane key — the digest of the staging identity, so enriching
    /// (`commit`, `gated_attempted`) or settling the row keeps its key.
    #[must_use]
    pub fn plane_key(&self) -> String {
        #[derive(Serialize)]
        struct StagingIdentity<'a> {
            #[serde(with = "serde_bytes")]
            channel_id: &'a [u8; 32],
            name: &'a str,
            removed_member: &'a ActorId,
            new_generation: &'a ContentKeyGeneration,
        }
        let r = &self.removal;
        format!(
            "{REMOVAL_KEY_PREFIX}{}",
            crate::hex32::encode(&canonical_tiebreak_key(&StagingIdentity {
                channel_id: &r.channel_id,
                name: &r.name,
                removed_member: &r.removed_member,
                new_generation: &r.new_generation,
            }))
        )
    }
}

/// One plane row's value: the record its key names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderKeyRecord {
    Set(FolderSetRecord),
    Generation(FolderGenerationRow),
    Removal(FolderRemovalRow),
    Foreign(ForeignFolder),
}

impl FolderKeyRecord {
    /// The row's plane key.
    ///
    /// # Errors
    /// A set record with neither a nonce nor a channel (it has no key).
    pub fn plane_key(&self) -> Result<String> {
        Ok(match self {
            Self::Set(r) => SetIdentity::of(&r.to_entry())
                .ok_or_else(|| {
                    Error::Encoding("a folder-keys set entry with neither nonce nor channel".into())
                })?
                .plane_key(),
            Self::Generation(r) => r.plane_key(),
            Self::Removal(r) => r.plane_key(),
            Self::Foreign(f) => {
                format!(
                    "{FOREIGN_KEY_PREFIX}{}",
                    crate::hex32::encode(&f.channel_id)
                )
            }
        })
    }

    /// The value's canonical bytes.
    ///
    /// # Errors
    /// Canonical-encoding failure.
    pub fn encode(&self) -> Result<Vec<u8>> {
        match self {
            Self::Set(r) => canonical_encode(r),
            Self::Generation(r) => canonical_encode(r),
            Self::Removal(r) => canonical_encode(r),
            Self::Foreign(r) => canonical_encode(r),
        }
    }

    /// This row as a single-entity [`FoldersConfig`] — what both the arm and
    /// the fold hand to [`FoldersConfig::merge`]. A settled removal is none
    /// (it has left the fold).
    #[must_use]
    pub fn as_config(&self) -> FoldersConfig {
        match self {
            Self::Set(r) => FoldersConfig {
                sets: vec![r.to_entry()],
                ..Default::default()
            },
            Self::Generation(r) => FoldersConfig {
                sets: vec![FolderKeyCustody {
                    keys: Some(FolderContentKeys {
                        current: r.generation.clone(),
                        prior: Vec::new(),
                    }),
                    ..r.set.bare_entry()
                }],
                ..Default::default()
            },
            Self::Removal(r) if r.settled => FoldersConfig::default(),
            Self::Removal(r) => FoldersConfig {
                pending_removals: vec![r.removal.clone()],
                ..Default::default()
            },
            Self::Foreign(f) => FoldersConfig {
                foreign_sets: vec![f.clone()],
                ..Default::default()
            },
        }
    }

    /// **The per-row arm** — the halves [`FoldersConfig::merge`] runs: a set
    /// row joins field-wise ([`FolderKeyCustody::join`]); a generation row is
    /// write-once (its key digests the whole row, so one key holds one value);
    /// a removal folds its `commit` and `gated_attempted` as the merge does,
    /// and its `settled` marker ORs; a foreign record folds per field.
    ///
    /// # Errors
    /// The two records name different rows, or two differing generation rows
    /// share one key.
    pub fn merge(&self, other: &Self) -> Result<Self> {
        let key = self.plane_key()?;
        if other.plane_key()? != key {
            return Err(Error::Encoding(
                "folder-keys records name different rows".into(),
            ));
        }
        let joined = |a: &FoldersConfig, b: &FoldersConfig| a.merge(b);
        Ok(match (self, other) {
            (Self::Generation(a), Self::Generation(b)) => {
                if a != b {
                    return Err(Error::Encoding(
                        "a folder-keys generation row is write-once; two differing values \
                         share one key"
                            .into(),
                    ));
                }
                Self::Generation(a.clone())
            }
            (Self::Set(a), Self::Set(b)) => {
                Self::Set(FolderSetRecord::of(&a.to_entry().join(&b.to_entry())))
            }
            (Self::Removal(a), Self::Removal(b)) => {
                let one = |r: &FolderRemovalRow| FoldersConfig {
                    pending_removals: vec![r.removal.clone()],
                    ..Default::default()
                };
                let mut m = joined(&one(a), &one(b)).pending_removals;
                match (m.pop(), m.is_empty()) {
                    (Some(removal), true) => Self::Removal(FolderRemovalRow {
                        removal,
                        settled: a.settled || b.settled,
                    }),
                    _ => {
                        return Err(Error::Encoding(
                            "folder-keys removal join did not stay one row".into(),
                        ));
                    }
                }
            }
            (Self::Foreign(_), Self::Foreign(_)) => {
                let mut m = joined(&self.as_config(), &other.as_config()).foreign_sets;
                match (m.pop(), m.is_empty()) {
                    (Some(f), true) => Self::Foreign(f),
                    _ => {
                        return Err(Error::Encoding(
                            "folder-keys foreign join did not stay one row".into(),
                        ));
                    }
                }
            }
            _ => {
                return Err(Error::Encoding(
                    "folder-keys records name different rows".into(),
                ));
            }
        })
    }
}

impl FoldersConfig {
    /// Every entity as its plane row, `(key, record)`: per custody entry its
    /// `set/` row then one `gen/` row per generation (current first), then the
    /// stagings (unsettled — the composite holds only live sentinels), then the
    /// foreign sets.
    ///
    /// # Errors
    /// A custody entry with neither a nonce nor a channel (no key; never
    /// produced).
    pub fn rows(&self) -> Result<Vec<(String, FolderKeyRecord)>> {
        let mut records = Vec::new();
        for entry in &self.sets {
            let set = SetIdentity::of(entry).ok_or_else(|| {
                Error::Encoding("a folder-keys set entry with neither nonce nor channel".into())
            })?;
            records.push(FolderKeyRecord::Set(FolderSetRecord::of(entry)));
            for generation in entry.keys.iter().flat_map(FolderContentKeys::generations) {
                records.push(FolderKeyRecord::Generation(FolderGenerationRow {
                    set,
                    generation: generation.clone(),
                }));
            }
        }
        records.extend(self.pending_removals.iter().map(|r| {
            FolderKeyRecord::Removal(FolderRemovalRow {
                removal: r.clone(),
                settled: false,
            })
        }));
        records.extend(
            self.foreign_sets
                .iter()
                .cloned()
                .map(FolderKeyRecord::Foreign),
        );
        records
            .into_iter()
            .map(|r| Ok((r.plane_key()?, r)))
            .collect()
    }

    /// **The READ fold** over an account's `fauna.state.folder-keys` rows:
    /// [`Self::merge`] over every row's single-entity config (a settled
    /// removal contributing none) — so the fold of two replicas' rows is the
    /// [`FoldersConfig::merge`] of the two.
    ///
    /// # Errors
    /// Any row [`decode_folder_key_row`] refuses.
    pub fn fold<'a>(rows: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Result<Self> {
        // The merge unions every list by identity and joins what shares one,
        // so one merge over the rows' concatenation is the pairwise fold —
        // without re-merging the accumulated config per row.
        let mut all = Self::default();
        for (key, value) in rows {
            let row = decode_folder_key_row(key, value)?.as_config();
            all.sets.extend(row.sets);
            all.pending_removals.extend(row.pending_removals);
            all.foreign_sets.extend(row.foreign_sets);
        }
        Ok(Self::default().merge(&all))
    }
}

/// Decode `value` as `T`, refusing anything that does not re-encode to the
/// same bytes — the plane's strict posture over value types whose serde stays
/// tolerant (module docs).
fn decode_exact<T: Serialize + serde::de::DeserializeOwned>(value: &[u8], what: &str) -> Result<T> {
    let decoded: T = canonical_decode(value)?;
    if canonical_encode(&decoded)?.as_slice() != value {
        return Err(Error::Encoding(format!(
            "folder-keys {what} row does not re-encode to its own bytes \
             (an unknown field, or a non-canonical encoding)"
        )));
    }
    Ok(decoded)
}

/// Decode one `fauna.state.folder-keys` row: the key's first segment picks the
/// record type, the value must decode as it and re-encode to the same bytes (a
/// newer build's field is refused, never stripped), and the value must name
/// its key.
///
/// # Errors
/// A key outside the grammar, an undecodable or non-canonical value, or a
/// value not naming its key.
pub fn decode_folder_key_row(key: &str, value: &[u8]) -> Result<FolderKeyRecord> {
    let record = if key.starts_with(SET_KEY_PREFIX) {
        FolderKeyRecord::Set(decode_exact(value, "set")?)
    } else if key.starts_with(GEN_KEY_PREFIX) {
        FolderKeyRecord::Generation(decode_exact(value, "generation")?)
    } else if key.starts_with(REMOVAL_KEY_PREFIX) {
        FolderKeyRecord::Removal(decode_exact(value, "removal")?)
    } else if key.starts_with(FOREIGN_KEY_PREFIX) {
        FolderKeyRecord::Foreign(decode_exact(value, "foreign")?)
    } else {
        return Err(Error::Encoding(format!("not a folder-keys key: {key:?}")));
    };
    // The value names its key; the key is rendered from the value in lowercase
    // hex, so any other spelling of it is refused here too.
    let named = record.plane_key()?;
    if named != key {
        return Err(Error::Encoding(format!(
            "folder-keys row at {key:?} holds the record for {named:?}"
        )));
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn generation(version: u64, key: u8) -> ContentKeyGeneration {
        ContentKeyGeneration {
            version,
            key: [key; 32].into(),
            rotated_at: version * 1_000,
        }
    }

    fn keys(gens: &[ContentKeyGeneration]) -> FolderContentKeys {
        gens.iter()
            .map(|g| FolderContentKeys {
                current: g.clone(),
                prior: vec![],
            })
            .reduce(|a, b| a.merge(&b))
            .unwrap()
    }

    fn nonce_entry(nonce: u8, gens: &[ContentKeyGeneration]) -> FolderKeyCustody {
        FolderKeyCustody {
            channel_id: Some([nonce ^ 0xF0; 32]),
            keys: (!gens.is_empty()).then(|| keys(gens)),
            set_nonce: Some([nonce; 32]),
            name: Some(format!("set-{nonce}")),
            created_at: 7,
            ..Default::default()
        }
    }

    fn encoded(rows: Vec<(String, FolderKeyRecord)>) -> Vec<(String, Vec<u8>)> {
        rows.into_iter()
            .map(|(k, r)| (k, r.encode().unwrap()))
            .collect()
    }

    fn fold(rows: &[(String, Vec<u8>)]) -> FoldersConfig {
        FoldersConfig::fold(rows.iter().map(|(k, v)| (k.as_str(), v.as_slice()))).unwrap()
    }

    fn sample() -> FoldersConfig {
        FoldersConfig {
            sets: vec![
                nonce_entry(1, &[generation(1, 0x11), generation(2, 0x12)]),
                // A member's first ingest: keyed by channel, no nonce yet.
                FolderKeyCustody {
                    channel_id: Some([0x33; 32]),
                    keys: Some(keys(&[generation(3, 0x33)])),
                    ..Default::default()
                },
                // An owner-only set: a nonce, a name, no keys.
                nonce_entry(4, &[]),
            ],
            pending_removals: vec![FolderPendingRemoval {
                channel_id: [0xF1; 32],
                name: "set-1".into(),
                removed_member: ActorId([0x55; 32]),
                new_generation: generation(3, 0x13),
                commit: Some(vec![1, 2, 3]),
                gated_attempted: false,
            }],
            foreign_sets: vec![ForeignFolder {
                channel_id: [0x77; 32],
                mls_group_id: vec![0x78; 16],
                home_nest_url: "https://home.example".into(),
                home_nest_actor_id: None,
                set_name: Some("photos".into()),
                access: Some("reader".into()),
                content_key_floor: Some(2),
                ..Default::default()
            }],
        }
    }

    /// The fold of a canonical config's rows is the config itself — the rows
    /// lose nothing, keys included.
    #[test]
    fn a_config_round_trips_through_its_rows() {
        let cfg = FoldersConfig::default().merge(&sample());
        assert_eq!(fold(&encoded(cfg.rows().unwrap())), cfg);
        assert_eq!(cfg.rows().unwrap().len(), 3 + 3 + 1 + 1, "sets + gens + 2");
    }

    /// Folding two replicas' rows together (same-key rows joined by the arm,
    /// as the plane does) is `FoldersConfig::merge` of the two — a concurrent
    /// rotation to one version keeping both keys.
    #[test]
    fn the_fold_of_two_replicas_rows_is_their_merge() {
        let a = sample();
        let mut b = sample();
        b.sets[0] = nonce_entry(1, &[generation(1, 0x11), generation(2, 0x22)]);
        b.sets[0].retired_at = Some(99);
        b.pending_removals[0].gated_attempted = true;
        b.pending_removals[0].commit = None;
        b.foreign_sets[0].content_key_floor = Some(5);
        let mut joined: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for (k, v) in encoded(a.rows().unwrap())
            .into_iter()
            .chain(encoded(b.rows().unwrap()))
        {
            let next = match joined.get(&k) {
                Some(cur) => decode_folder_key_row(&k, cur)
                    .unwrap()
                    .merge(&decode_folder_key_row(&k, &v).unwrap())
                    .unwrap()
                    .encode()
                    .unwrap(),
                None => v,
            };
            joined.insert(k, next);
        }
        let rows: Vec<(String, Vec<u8>)> = joined.into_iter().collect();
        let merged = a.merge(&b);
        assert_eq!(fold(&rows), merged);
        assert_eq!(merged.sets[0].keys.as_ref().unwrap().keys_for(2).count(), 2);
        assert_eq!(merged.sets[0].retired_at, Some(99));
    }

    /// A settled removal leaves the fold, and its key does not move: settling
    /// is the same row, joined.
    #[test]
    fn a_settled_removal_leaves_the_fold_under_the_same_key() {
        let cfg = FoldersConfig::default().merge(&sample());
        let removal = cfg.pending_removals[0].clone();
        let open = FolderKeyRecord::Removal(FolderRemovalRow {
            removal: removal.clone(),
            settled: false,
        });
        let settled = FolderKeyRecord::Removal(FolderRemovalRow {
            removal: FolderPendingRemoval {
                commit: None,
                gated_attempted: true,
                ..removal
            },
            settled: true,
        });
        let key = open.plane_key().unwrap();
        assert_eq!(
            settled.plane_key().unwrap(),
            key,
            "enriched + settled keeps the key"
        );
        let joined = open.merge(&settled).unwrap();
        assert_eq!(joined, settled.merge(&open).unwrap());
        let FolderKeyRecord::Removal(j) = &joined else {
            unreachable!()
        };
        assert!(j.settled && j.removal.gated_attempted);
        assert_eq!(j.removal.commit.as_deref(), Some(&[1u8, 2, 3][..]));

        let mut rows = encoded(cfg.rows().unwrap());
        for (k, v) in &mut rows {
            if *k == key {
                *v = joined.encode().unwrap();
            }
        }
        assert!(fold(&rows).pending_removals.is_empty());
    }

    #[test]
    fn keys_are_the_values_own_and_lowercase() {
        for (key, record) in sample().rows().unwrap() {
            let value = record.encode().unwrap();
            assert!(decode_folder_key_row(&key, &value).is_ok());
            assert!(decode_folder_key_row(&key.to_uppercase(), &value).is_err());
            assert!(decode_folder_key_row(&format!("{key}0"), &value).is_err());
        }
        assert!(decode_folder_key_row("self", &[]).is_err());
    }

    #[test]
    fn a_value_filed_under_another_rows_key_is_refused() {
        let rows = sample().rows().unwrap();
        let (set_key, set) = &rows[0];
        let (gen_key, generation) = &rows[1];
        assert!(decode_folder_key_row(set_key, &generation.encode().unwrap()).is_err());
        assert!(decode_folder_key_row(gen_key, &set.encode().unwrap()).is_err());
        let (other_gen_key, _) = &rows[2];
        assert!(decode_folder_key_row(other_gen_key, &generation.encode().unwrap()).is_err());
    }
}
