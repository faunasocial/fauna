//! The `fauna.state.moderation` plane rows — the key grammar, the row values,
//! the READ fold and the write diff (`content-moderation-and-ranking.md`
//! § Implementation status today → *The moderation kind's rows — one per
//! keyword, one per hidden id* owns the ruling; `config-dissolution.md`
//! § Migration + compatibility → *Bounded rows* owns why a growing list is
//! never one row).
//!
//! **Three row families, never one row per account:**
//!
//! - `keyword/<blake3 hex64 of the trimmed, lowercased term>` holds one
//!   [`MutedKeyword`] — the term in its display case and its weight. The key
//!   is the normalizer's dedupe identity
//!   (`fauna_client_config::normalize_muted_keywords` collapses terms that are
//!   equal lowercased and trimmed), so one term is one row however it is
//!   spelled.
//! - `hidden/<normalized subject id>` holds one [`HiddenContentMark`] — a
//!   report subject the user hid by reporting it (`moderation.md` § Corollary
//!   — block also hides).
//! - `spam/<normalized post id>` holds one [`SpamMark`] — a post the user
//!   marked as spam from its actions menu (`report-sharing.md` § Report
//!   capture → *The post's mark-as-spam verb*).
//!
//! Each row is latest-wins on its entry's `LwwStamp` (the kind's registered
//! policy, unchanged): an add or a re-weight is a put, a removal a stamped
//! tombstone. Two devices that each add a word before seeing the other's
//! write therefore keep both — two adds are two rows — and a removal still
//! propagates.
//!
//! [`ModerationConfig`] stays the type every reader takes (the scorer, the
//! render verdict, the muted-words page): [`ModerationConfig::fold`] builds it
//! from the live rows, keywords, hidden ids and spam marks each oldest-stamp
//! first, and
//! [`ModerationConfig::changes_to`] is the other direction — the row writes
//! that take one record to another, which is how every shared delta in
//! `fauna_client_config::preference_records` reaches the plane unchanged.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::data::{ModerationConfig, MutedKeyword};
use crate::encoding::canonical_decode;
use crate::error::{Error, Result};

/// Key prefix of a muted-keyword row: `keyword/<blake3 hex64>`.
pub const KEYWORD_KEY_PREFIX: &str = "keyword/";

/// Key prefix of a hidden-content row: `hidden/<normalized subject id>`.
pub const HIDDEN_KEY_PREFIX: &str = "hidden/";

/// Key prefix of a spam-mark row: `spam/<normalized post id>`.
pub const SPAM_KEY_PREFIX: &str = "spam/";

/// The longest muted term, in UTF-8 bytes, a write accepts. A Rust constant —
/// nobody chooses it: it bounds a keyword row so the widest one seals well
/// under half the per-entry cap (the size pin beside the kind's registration),
/// and no real word or phrase comes near it.
pub const MUTED_KEYWORD_MAX_BYTES: usize = 1024;

/// The longest report-subject id, in bytes, a hide accepts. The apps mint
/// fixed-width lowercase hex (a post's or a message's cid, an account's actor
/// id); this bounds a hidden row's key and value the way
/// [`MUTED_KEYWORD_MAX_BYTES`] bounds a keyword row. A spam mark's post id
/// is the same fixed-width hex and takes the same bound.
pub const HIDDEN_CONTENT_ID_MAX_BYTES: usize = 256;

impl MutedKeyword {
    /// This entry's logical key on the plane — [`Self::plane_key_of`] over its
    /// term.
    #[must_use]
    pub fn plane_key(&self) -> String {
        Self::plane_key_of(&self.keyword)
    }

    /// The key of the row that holds `term`: the lowercase-hex blake3 digest of
    /// the trimmed, lowercased term, behind [`KEYWORD_KEY_PREFIX`]. Fixed
    /// length however long the term, and blind to its case — the normalizer's
    /// dedupe identity. Frozen with the kind: a row already written is
    /// addressed by it.
    #[must_use]
    pub fn plane_key_of(term: &str) -> String {
        let identity = term.trim().to_lowercase();
        format!(
            "{KEYWORD_KEY_PREFIX}{}",
            blake3::hash(identity.as_bytes()).to_hex()
        )
    }
}

/// One report subject the user hid by reporting it — a `hidden/` row's value.
/// A struct rather than the bare id so a later field is additive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiddenContentMark {
    /// The subject's normalized id ([`normalize_hidden_id`]).
    pub id: String,
}

impl HiddenContentMark {
    /// This mark's logical key on the plane.
    #[must_use]
    pub fn plane_key(&self) -> String {
        Self::plane_key_of(&self.id)
    }

    /// The key of the row that holds the normalized id `id`.
    #[must_use]
    pub fn plane_key_of(id: &str) -> String {
        format!("{HIDDEN_KEY_PREFIX}{id}")
    }
}

/// One post the user marked as spam — a `spam/` row's value. A struct rather
/// than the bare id so a later field is additive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpamMark {
    /// The post's normalized id ([`normalize_hidden_id`]).
    pub id: String,
}

impl SpamMark {
    /// This mark's logical key on the plane.
    #[must_use]
    pub fn plane_key(&self) -> String {
        Self::plane_key_of(&self.id)
    }

    /// The key of the row that holds the normalized id `id`.
    #[must_use]
    pub fn plane_key_of(id: &str) -> String {
        format!("{SPAM_KEY_PREFIX}{id}")
    }
}

/// Normalize one report subject id: trimmed and lowercased, so a case-varied
/// hex id is the same entry. `None` for a blank. A spam mark's post id is
/// normalized the same way.
#[must_use]
pub fn normalize_hidden_id(id: &str) -> Option<String> {
    let id = id.trim().to_ascii_lowercase();
    (!id.is_empty()).then_some(id)
}

/// One row write that takes a [`ModerationConfig`] towards another
/// ([`ModerationConfig::changes_to`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModerationChange {
    /// Put this keyword at its own key — a new term, or a re-spelled or
    /// re-weighted one.
    PutKeyword(MutedKeyword),
    /// Tombstone the keyword row at this key — a term taken off the list.
    DropKeyword(String),
    /// Put this mark at its own key — a newly hidden subject.
    PutHidden(HiddenContentMark),
    /// Tombstone the hidden row at this key — a subject shown again.
    DropHidden(String),
    /// Put this mark at its own key — a post newly marked as spam.
    PutSpam(SpamMark),
    /// Tombstone the spam row at this key — a post un-marked.
    DropSpam(String),
}

/// A row's value names a different key than the one it is stored at: a read
/// keyed by a term or an id would otherwise find another one. Fails loudly —
/// silently dropping it would hide a writer bug.
fn misfiled(key: &str, names: &str) -> Error {
    Error::Encoding(format!(
        "the moderation row at {key:?} names {names:?} — a row's value must name its own key"
    ))
}

impl ModerationConfig {
    /// The READ fold over the account's live `fauna.state.moderation` rows,
    /// each given as `(order, key, value)` where `order` is the row's stamp
    /// rank (`LwwStamp::rank`): keywords, hidden ids and spam marks each oldest first, so
    /// the page lists words in the order they were added.
    ///
    /// A value that does not decode, or does not name its own key, is
    /// refused. A key outside the three families — the retired whole-record
    /// `self` row an alpha account may still hold — is read past with a log
    /// line, never refused: the 0.1.x compat-free window owes it no lift.
    pub fn fold<O: Ord>(rows: impl IntoIterator<Item = (O, String, Vec<u8>)>) -> Result<Self> {
        let mut keywords: Vec<(O, MutedKeyword)> = Vec::new();
        let mut hidden: Vec<(O, String)> = Vec::new();
        let mut spam: Vec<(O, String)> = Vec::new();
        for (order, key, value) in rows {
            if key.starts_with(KEYWORD_KEY_PREFIX) {
                let keyword = canonical_decode::<MutedKeyword>(&value)?;
                if keyword.plane_key() != key {
                    return Err(misfiled(&key, &keyword.keyword));
                }
                keywords.push((order, keyword));
            } else if key.starts_with(HIDDEN_KEY_PREFIX) {
                let mark = canonical_decode::<HiddenContentMark>(&value)?;
                if mark.plane_key() != key {
                    return Err(misfiled(&key, &mark.id));
                }
                hidden.push((order, mark.id));
            } else if key.starts_with(SPAM_KEY_PREFIX) {
                let mark = canonical_decode::<SpamMark>(&value)?;
                if mark.plane_key() != key {
                    return Err(misfiled(&key, &mark.id));
                }
                spam.push((order, mark.id));
            } else {
                tracing::info!(
                    key,
                    "fauna.state.moderation: reading past a row outside the keyword/, \
                     hidden/ and spam/ families (the retired whole-record row)"
                );
            }
        }
        keywords.sort_by(|a, b| a.0.cmp(&b.0));
        hidden.sort_by(|a, b| a.0.cmp(&b.0));
        spam.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Self {
            muted_keywords: keywords.into_iter().map(|(_, k)| k).collect(),
            hidden_content: hidden.into_iter().map(|(_, id)| id).collect(),
            spam_marked: spam.into_iter().map(|(_, id)| id).collect(),
        })
    }

    /// The row writes that take `self` (the record as the fold read it) to
    /// `after` (the record a shared delta left): a put for every keyword or
    /// hidden id `after` adds or changes, a tombstone for every one it drops,
    /// and nothing for a row it leaves as it was — a re-put would mint a stamp
    /// that outranks a sibling's concurrent edit of the same row.
    ///
    /// Refuses a term over [`MUTED_KEYWORD_MAX_BYTES`] or a hidden id over
    /// [`HIDDEN_CONTENT_ID_MAX_BYTES`] (or a spam mark's post id over the same
    /// bound) among the puts, before anything is
    /// written.
    pub fn changes_to(&self, after: &Self) -> Result<Vec<ModerationChange>> {
        let mut changes = Vec::new();
        let before_keywords: BTreeMap<String, &MutedKeyword> = self
            .muted_keywords
            .iter()
            .map(|k| (k.plane_key(), k))
            .collect();
        let after_keys: std::collections::BTreeSet<String> = after
            .muted_keywords
            .iter()
            .map(MutedKeyword::plane_key)
            .collect();
        for keyword in &after.muted_keywords {
            if before_keywords.get(&keyword.plane_key()) == Some(&keyword) {
                continue;
            }
            if keyword.keyword.len() > MUTED_KEYWORD_MAX_BYTES {
                return Err(Error::Encoding(format!(
                    "a muted word may be at most {MUTED_KEYWORD_MAX_BYTES} bytes long"
                )));
            }
            changes.push(ModerationChange::PutKeyword(keyword.clone()));
        }
        for key in before_keywords.keys() {
            if !after_keys.contains(key) {
                changes.push(ModerationChange::DropKeyword(key.clone()));
            }
        }
        for id in &after.hidden_content {
            if self.hidden_content.contains(id) {
                continue;
            }
            if id.len() > HIDDEN_CONTENT_ID_MAX_BYTES {
                return Err(Error::Encoding(format!(
                    "a hidden subject id may be at most {HIDDEN_CONTENT_ID_MAX_BYTES} bytes long"
                )));
            }
            changes.push(ModerationChange::PutHidden(HiddenContentMark {
                id: id.clone(),
            }));
        }
        for id in &self.hidden_content {
            if !after.hidden_content.contains(id) {
                changes.push(ModerationChange::DropHidden(
                    HiddenContentMark::plane_key_of(id),
                ));
            }
        }
        for id in &after.spam_marked {
            if self.spam_marked.contains(id) {
                continue;
            }
            if id.len() > HIDDEN_CONTENT_ID_MAX_BYTES {
                return Err(Error::Encoding(format!(
                    "a spam-marked post id may be at most {HIDDEN_CONTENT_ID_MAX_BYTES} bytes long"
                )));
            }
            changes.push(ModerationChange::PutSpam(SpamMark { id: id.clone() }));
        }
        for id in &self.spam_marked {
            if !after.spam_marked.contains(id) {
                changes.push(ModerationChange::DropSpam(SpamMark::plane_key_of(id)));
            }
        }
        Ok(changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::canonical_encode;

    fn row<T: Serialize>(order: u32, key: String, value: &T) -> (u32, String, Vec<u8>) {
        (order, key, canonical_encode(value).unwrap().to_vec())
    }

    #[test]
    fn a_keyword_key_is_blind_to_case_and_surrounding_space() {
        assert_eq!(
            MutedKeyword::plane_key_of("  Spoiler "),
            MutedKeyword::plane_key_of("spoiler")
        );
        let key = MutedKeyword::new("x".repeat(MUTED_KEYWORD_MAX_BYTES)).plane_key();
        assert_eq!(key.len(), KEYWORD_KEY_PREFIX.len() + 64);
    }

    #[test]
    fn the_fold_orders_each_family_by_stamp_and_reads_past_other_keys() {
        let late = MutedKeyword::new("Late");
        let early = MutedKeyword::new("early");
        let mark = HiddenContentMark { id: "ab".into() };
        let spam_late = SpamMark { id: "c2".into() };
        let spam_early = SpamMark { id: "c1".into() };
        let folded = ModerationConfig::fold([
            row(3, late.plane_key(), &late),
            row(1, "self".into(), &ModerationConfig::default()),
            row(2, mark.plane_key(), &mark),
            row(5, spam_late.plane_key(), &spam_late),
            row(1, early.plane_key(), &early),
            row(4, spam_early.plane_key(), &spam_early),
        ])
        .unwrap();
        assert_eq!(folded.muted_keywords, vec![early, late]);
        assert_eq!(folded.hidden_content, vec!["ab".to_string()]);
        assert_eq!(folded.spam_marked, vec!["c1".to_string(), "c2".to_string()]);
    }

    #[test]
    fn the_fold_refuses_a_row_that_names_another_key() {
        let keyword = MutedKeyword::new("one");
        let r = ModerationConfig::fold([row(1, MutedKeyword::plane_key_of("two"), &keyword)]);
        assert!(r.is_err());
        let mark = HiddenContentMark { id: "aa".into() };
        let r = ModerationConfig::fold([row(1, HiddenContentMark::plane_key_of("bb"), &mark)]);
        assert!(r.is_err());
        let spam = SpamMark { id: "aa".into() };
        let r = ModerationConfig::fold([row(1, SpamMark::plane_key_of("bb"), &spam)]);
        assert!(r.is_err());
    }

    #[test]
    fn changes_put_what_is_new_or_changed_and_drop_what_left() {
        let before = ModerationConfig {
            muted_keywords: vec![MutedKeyword::new("kept"), MutedKeyword::new("gone")],
            hidden_content: vec!["aa".into(), "bb".into()],
            spam_marked: vec!["d1".into(), "d2".into()],
        };
        let reweighted = MutedKeyword {
            keyword: "kept".into(),
            weight: -500,
        };
        let after = ModerationConfig {
            muted_keywords: vec![reweighted.clone(), MutedKeyword::new("new")],
            hidden_content: vec!["aa".into(), "cc".into()],
            spam_marked: vec!["d2".into(), "d3".into()],
        };
        assert_eq!(
            before.changes_to(&after).unwrap(),
            vec![
                ModerationChange::PutKeyword(reweighted),
                ModerationChange::PutKeyword(MutedKeyword::new("new")),
                ModerationChange::DropKeyword(MutedKeyword::plane_key_of("gone")),
                ModerationChange::PutHidden(HiddenContentMark { id: "cc".into() }),
                ModerationChange::DropHidden(HiddenContentMark::plane_key_of("bb")),
                ModerationChange::PutSpam(SpamMark { id: "d3".into() }),
                ModerationChange::DropSpam(SpamMark::plane_key_of("d1")),
            ]
        );
        assert!(after.changes_to(&after).unwrap().is_empty());
    }

    #[test]
    fn a_term_over_the_cap_is_refused_before_anything_is_written() {
        let after = ModerationConfig {
            muted_keywords: vec![MutedKeyword::new("x".repeat(MUTED_KEYWORD_MAX_BYTES + 1))],
            ..Default::default()
        };
        assert!(ModerationConfig::default().changes_to(&after).is_err());
        let at_cap = ModerationConfig {
            muted_keywords: vec![MutedKeyword::new("x".repeat(MUTED_KEYWORD_MAX_BYTES))],
            ..Default::default()
        };
        assert_eq!(
            ModerationConfig::default()
                .changes_to(&at_cap)
                .unwrap()
                .len(),
            1
        );
        let spam_over = ModerationConfig {
            spam_marked: vec!["f".repeat(HIDDEN_CONTENT_ID_MAX_BYTES + 1)],
            ..Default::default()
        };
        assert!(ModerationConfig::default().changes_to(&spam_over).is_err());
    }
}
