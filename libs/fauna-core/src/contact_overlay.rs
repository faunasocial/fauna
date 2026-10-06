//! The private contact overlay — the value of one `fauna.state.contact-overlay`
//! entry: the viewer's own **nickname**, **notes** and **labels** on another
//! person, which nobody else ever sees (`docs/goal/ui/contacts.md` § The
//! private overlay owns the record, the merge, the bounds and the succession
//! fold; `docs/goal/ui/profile.md` § The private section owns the edit
//! surface this module's staged-form diff serves).
//!
//! One entry per person, keyed [`overlay_key`] — the other person's actor id,
//! **not** gated on a contact edge: a knock sender, a blocked stranger and a
//! creator you follow can all carry one, and it outlives the edge.
//!
//! The merge is **per-register last-writer** ([`ContactOverlay::join`]): each
//! of nickname, notes and every label resolves independently on its
//! [`Stamp`], so a nickname set on one device and notes edited on another both
//! survive. Labels are per-label *presence registers* — a removal is a register
//! whose value is `None`, retained as a marker — so a removal propagates
//! instead of being resurrected by a stale replica. The join is a join **in
//! bytes** (commutative, associative, idempotent), which is what lets the
//! plane's byte-equality echo-stop settle two replicas.
//!
//! Clearing is a write, never a tombstone (the plane's `CrdtPerField` policy
//! admits none): [`ContactOverlay::cleared`] sets every register to `None`
//! under a fresh stamp, and an all-`None` overlay reads everywhere as *no
//! overlay* ([`ContactOverlay::is_empty`]).
//!
//! Wasm-safe on purpose (no clock, no I/O): web's later leg reuses it
//! unchanged.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::data::lww_rank;
use crate::encoding::canonical_tiebreak_key;
use crate::localized::LocalizedText;

/// Longest nickname, in characters after trimming.
pub const MAX_NICKNAME_CHARS: usize = 64;
/// Largest notes body, in bytes (16 KiB).
pub const MAX_NOTES_BYTES: usize = 16 * 1024;
/// Longest label, in bytes after trimming and whitespace-collapse.
pub const MAX_LABEL_BYTES: usize = 48;
/// Most live labels one person may carry.
pub const MAX_LIVE_LABELS: usize = 32;
/// Most label map keys one item may hold, removal markers included. At the cap
/// the writer refuses a new label rather than pruning a marker — pruning is
/// what would let a stale replica resurrect a removed label.
pub const MAX_LABEL_KEYS: usize = 128;

/// The logical key of the overlay on the person whose actor id is
/// `actor_id_hex` — the id lowercased, so every device spells it alike.
/// `None` for anything that is not a 64-hex actor id.
pub fn overlay_key(actor_id_hex: &str) -> Option<String> {
    let trimmed = actor_id_hex.trim();
    (trimmed.len() == 64 && trimmed.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| trimmed.to_ascii_lowercase())
}

/// A register's write stamp: the plane's `(at_ms, writer)` total order — the
/// exact shape and rank of `fauna_protocol::merge_policy::LwwStamp`, carried
/// here because `fauna-core` sits below that crate; both rank through
/// [`lww_rank`], so the two can never order a pair differently.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    /// Unix milliseconds at the authoring writer's clock.
    pub at_ms: i64,
    /// The authoring writer's 32-byte id — the deterministic tiebreak.
    #[serde(with = "serde_bytes")]
    pub writer: [u8; 32],
}

impl Stamp {
    pub fn new(at_ms: i64, writer: [u8; 32]) -> Self {
        Self { at_ms, writer }
    }

    /// The total order: later millisecond wins, ties by writer bytes.
    pub fn rank(&self) -> ([u8; 8], [u8; 32]) {
        lww_rank(self.at_ms, self.writer)
    }

    /// This stamp, lifted just past `prev` when `prev` would otherwise outrank
    /// it — so a local edit always takes effect locally, even when another
    /// device's clock ran ahead and stamped the register "in the future".
    fn after(self, prev: Stamp) -> Stamp {
        if self.rank() > prev.rank() {
            self
        } else {
            Stamp {
                at_ms: prev.at_ms.saturating_add(1),
                writer: self.writer,
            }
        }
    }
}

/// One last-writer register: a value and the stamp that wrote it. The default
/// — stamp zero, `None` — is "never written" and loses to any real write.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Register {
    pub stamp: Stamp,
    /// `None` = cleared (or, for a label, removed).
    #[serde(default)]
    pub value: Option<String>,
}

impl Register {
    fn new(stamp: Stamp, value: Option<String>) -> Self {
        Self { stamp, value }
    }

    /// The register join: the higher stamp; an exact stamp tie resolves on the
    /// values' canonical bytes (the `canonical_tiebreak_key` rule), never on which
    /// replica merges — so it is a max over a total order, hence a join.
    pub fn join(&self, other: &Self) -> Self {
        let key = |r: &Register| (r.stamp.rank(), canonical_tiebreak_key(&r.value));
        if key(other) > key(self) {
            other.clone()
        } else {
            self.clone()
        }
    }
}

/// The overlay record — additive like every wire type: `#[serde(default)]`
/// fields plus a trailing `extra` catch-all, so an older binary re-seals what
/// it does not know.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactOverlay {
    /// Your own name for them.
    #[serde(default)]
    pub nickname: Register,
    /// Free-form notes.
    #[serde(default)]
    pub notes: Register,
    /// Label key = the label's Unicode-lowercased form ([`fold_label`]), so
    /// "Family" and "family" are one label; the register's value is its
    /// display spelling as last written, `None` once removed (the marker is
    /// retained).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, Register>,
    /// Forward-compat catch-all: fields a newer build added, preserved across
    /// a re-seal. Joined per key (the larger canonical bytes win a collision —
    /// arbitrary, but identical at every replica).
    #[serde(flatten)]
    pub extra: BTreeMap<String, fauna_cbor::Value>,
}

impl ContactOverlay {
    /// The nickname, when one is set.
    pub fn nickname(&self) -> Option<&str> {
        self.nickname.value.as_deref()
    }

    /// The notes, when any are written.
    pub fn notes(&self) -> Option<&str> {
        self.notes.value.as_deref()
    }

    /// The live labels' display spellings, ordered by their folded form.
    pub fn live_labels(&self) -> Vec<&str> {
        self.labels
            .values()
            .filter_map(|r| r.value.as_deref())
            .collect()
    }

    /// Whether this overlay says nothing — every register `None`. An emptied
    /// item reads everywhere exactly as an absent one.
    pub fn is_empty(&self) -> bool {
        self.nickname.value.is_none()
            && self.notes.value.is_none()
            && self.labels.values().all(|r| r.value.is_none())
    }

    /// The join: per-register last-writer, labels per key. Commutative,
    /// associative and idempotent — every component is (a register join, a
    /// key-union of register joins, a key-union of max-by-bytes).
    pub fn join(&self, other: &Self) -> Self {
        let mut labels = self.labels.clone();
        for (key, theirs) in &other.labels {
            labels
                .entry(key.clone())
                .and_modify(|ours| *ours = ours.join(theirs))
                .or_insert_with(|| theirs.clone());
        }
        let mut extra = self.extra.clone();
        for (key, theirs) in &other.extra {
            extra
                .entry(key.clone())
                .and_modify(|ours| {
                    let enc = |v: &fauna_cbor::Value| {
                        crate::encoding::canonical_encode(v).unwrap_or_default()
                    };
                    if enc(theirs) > enc(ours) {
                        *ours = theirs.clone();
                    }
                })
                .or_insert_with(|| theirs.clone());
        }
        Self {
            nickname: self.nickname.join(&other.nickname),
            notes: self.notes.join(&other.notes),
            labels,
            extra,
        }
    }

    /// "Remove everything I wrote about this person": every register — every
    /// label key included — set to `None` under `stamp` (lifted past each
    /// register's own stamp). The emptied item stays; it is a few dozen bytes.
    pub fn cleared(&self, stamp: Stamp) -> Self {
        let clear = |r: &Register| Register::new(stamp.after(r.stamp), None);
        Self {
            nickname: clear(&self.nickname),
            notes: clear(&self.notes),
            labels: self
                .labels
                .iter()
                .map(|(k, r)| (k.clone(), clear(r)))
                .collect(),
            extra: self.extra.clone(),
        }
    }

    /// The editable form of this overlay — what the private section stages
    /// from, and the baseline [`changed_registers`] diffs against.
    pub fn form(&self) -> OverlayForm {
        OverlayForm {
            nickname: self.nickname().unwrap_or_default().to_string(),
            notes: self.notes().unwrap_or_default().to_string(),
            labels: self.live_labels().into_iter().map(str::to_string).collect(),
        }
    }

    /// Write `changes` onto this overlay, each changed register stamped
    /// `stamp` (lifted past the register it replaces); untouched registers
    /// keep their stamps. Refuses — writing nothing — when the result would
    /// carry more than [`MAX_LIVE_LABELS`] live labels or more than
    /// [`MAX_LABEL_KEYS`] label keys.
    pub fn apply(&self, changes: &OverlayChanges, stamp: Stamp) -> Result<Self, LocalizedText> {
        let mut next = self.clone();
        if let Some(value) = &changes.nickname {
            next.nickname = Register::new(stamp.after(self.nickname.stamp), value.clone());
        }
        if let Some(value) = &changes.notes {
            next.notes = Register::new(stamp.after(self.notes.stamp), value.clone());
        }
        for (key, value) in &changes.labels {
            let prev = self.labels.get(key).map(|r| r.stamp).unwrap_or_default();
            next.labels
                .insert(key.clone(), Register::new(stamp.after(prev), value.clone()));
        }
        let live = next.labels.values().filter(|r| r.value.is_some()).count();
        if live > MAX_LIVE_LABELS && live > self.live_labels().len() {
            return Err(LocalizedText::key_arg(
                "profile.private_too_many_labels",
                "max",
                MAX_LIVE_LABELS.to_string(),
            ));
        }
        if next.labels.len() > MAX_LABEL_KEYS && next.labels.len() > self.labels.len() {
            return Err(LocalizedText::key("profile.private_label_history_full"));
        }
        Ok(next)
    }
}

/// The private section's staged form: plain text as the user sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct OverlayForm {
    pub nickname: String,
    pub notes: String,
    /// Display spellings, in the order shown.
    pub labels: Vec<String>,
}

/// The registers a Save writes — only the ones the user changed, because
/// re-stamping an untouched field would let this device's stale copy beat
/// another device's newer edit of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverlayChanges {
    /// `Some(new value)` when the nickname changed (`Some(None)` = cleared).
    pub nickname: Option<Option<String>>,
    /// Likewise for the notes.
    pub notes: Option<Option<String>>,
    /// Folded label key → its new value (`Some(spelling)` added or re-spelled,
    /// `None` removed).
    pub labels: BTreeMap<String, Option<String>>,
}

impl OverlayChanges {
    /// Whether the Save has nothing to write.
    pub fn is_empty(&self) -> bool {
        self.nickname.is_none() && self.notes.is_none() && self.labels.is_empty()
    }
}

/// A label's display form: trimmed, internal whitespace collapsed to single
/// spaces.
pub fn normalize_label(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A label's map key: its normalized form, Unicode-lowercased.
pub fn fold_label(raw: &str) -> String {
    normalize_label(raw).to_lowercase()
}

/// Validate one label the user is about to add; the normalized spelling on
/// success. Used by the add button so a refusal lands before staging.
pub fn validate_label(raw: &str) -> Result<String, LocalizedText> {
    let label = normalize_label(raw);
    if label.is_empty() {
        return Err(LocalizedText::key("profile.private_label_empty"));
    }
    if label.len() > MAX_LABEL_BYTES {
        return Err(LocalizedText::key_arg(
            "profile.private_label_too_long",
            "max",
            MAX_LABEL_BYTES.to_string(),
        ));
    }
    Ok(label)
}

fn normalize_nickname(raw: &str) -> Result<Option<String>, LocalizedText> {
    let nickname = raw.trim();
    if nickname.is_empty() {
        return Ok(None);
    }
    if nickname.chars().count() > MAX_NICKNAME_CHARS {
        return Err(LocalizedText::key_arg(
            "profile.private_nickname_too_long",
            "max",
            MAX_NICKNAME_CHARS.to_string(),
        ));
    }
    Ok(Some(nickname.to_string()))
}

fn normalize_notes(raw: &str) -> Result<Option<String>, LocalizedText> {
    if raw.trim().is_empty() {
        return Ok(None);
    }
    if raw.len() > MAX_NOTES_BYTES {
        return Err(LocalizedText::key_arg(
            "profile.private_notes_too_long",
            "max",
            (MAX_NOTES_BYTES / 1024).to_string(),
        ));
    }
    Ok(Some(raw.to_string()))
}

/// Folded key → display spelling for a form's labels (a later duplicate wins).
fn label_map(labels: &[String]) -> Result<BTreeMap<String, String>, LocalizedText> {
    let mut map = BTreeMap::new();
    for raw in labels {
        let label = validate_label(raw)?;
        map.insert(label.to_lowercase(), label);
    }
    Ok(map)
}

/// The staged form → changed-register diff: what differs between the form as
/// the user opened it (`baseline`) and as they are saving it (`staged`), after
/// normalization and bounds validation. Diffing against the *baseline* rather
/// than the live overlay is the point — a field another device edited while
/// this form was open is not "changed" here, so this Save does not overwrite
/// it.
pub fn changed_registers(
    baseline: &OverlayForm,
    staged: &OverlayForm,
) -> Result<OverlayChanges, LocalizedText> {
    let mut changes = OverlayChanges::default();
    let nickname = normalize_nickname(&staged.nickname)?;
    if nickname != normalize_nickname(&baseline.nickname).unwrap_or(None) {
        changes.nickname = Some(nickname);
    }
    let notes = normalize_notes(&staged.notes)?;
    if notes != normalize_notes(&baseline.notes).unwrap_or(None) {
        changes.notes = Some(notes);
    }
    let before = label_map(&baseline.labels).unwrap_or_default();
    let after = label_map(&staged.labels)?;
    for (key, spelling) in &after {
        if before.get(key) != Some(spelling) {
            changes.labels.insert(key.clone(), Some(spelling.clone()));
        }
    }
    for key in before.keys() {
        if !after.contains_key(key) {
            changes.labels.insert(key.clone(), None);
        }
    }
    Ok(changes)
}

/// The private section's staged edits — the one staging rule every app's
/// Profile page runs (`profile.md` § The private section: staged edits, one
/// Save), so app glue owns the text widgets and nothing else.
///
/// Staging is lazy on purpose: until the first edit every field reads the
/// **live** form the caller passes (the overlay projection's), so a sibling
/// device's edit re-paints an untouched section. The first edit snapshots that
/// live form as the diff **baseline**, so [`Self::changes`] names only what
/// this user changed here — a field another device edited meanwhile is not
/// re-stamped from this device's stale copy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverlayEditor {
    /// `(baseline, staged)`; `None` until the first edit.
    edit: Option<(OverlayForm, OverlayForm)>,
}

impl OverlayEditor {
    /// What the section shows: the staged edits once editing began, else
    /// `live`.
    pub fn form(&self, live: &OverlayForm) -> OverlayForm {
        match &self.edit {
            Some((_, staged)) => staged.clone(),
            None => live.clone(),
        }
    }

    /// Whether an edit is staged (the fields no longer follow the live form).
    pub fn is_staged(&self) -> bool {
        self.edit.is_some()
    }

    fn staged_mut(&mut self, live: &OverlayForm) -> &mut OverlayForm {
        &mut self
            .edit
            .get_or_insert_with(|| (live.clone(), live.clone()))
            .1
    }

    pub fn set_nickname(&mut self, live: &OverlayForm, value: String) {
        self.staged_mut(live).nickname = value;
    }

    pub fn set_notes(&mut self, live: &OverlayForm, value: String) {
        self.staged_mut(live).notes = value;
    }

    /// Stage the label typed into the add field; a label already staged under
    /// another spelling is re-spelled in place of being duplicated. A refusal
    /// stages nothing.
    pub fn add_label(&mut self, live: &OverlayForm, raw: &str) -> Result<(), LocalizedText> {
        let label = validate_label(raw)?;
        let folded = label.to_lowercase();
        let staged = self.staged_mut(live);
        staged.labels.retain(|l| fold_label(l) != folded);
        staged.labels.push(label);
        Ok(())
    }

    /// Unstage the label shown at `index` (out of range: nothing).
    pub fn remove_label(&mut self, live: &OverlayForm, index: usize) {
        let staged = self.staged_mut(live);
        if index < staged.labels.len() {
            staged.labels.remove(index);
        }
    }

    /// What a Save writes: `None` when nothing is staged or the staged form
    /// equals its baseline (the caller then [`Self::reset`]s and writes
    /// nothing); a bounds refusal keeps the staged edits.
    pub fn changes(&self) -> Result<Option<OverlayChanges>, LocalizedText> {
        let Some((baseline, staged)) = &self.edit else {
            return Ok(None);
        };
        let changes = changed_registers(baseline, staged)?;
        Ok((!changes.is_empty()).then_some(changes))
    }

    /// Drop the staging, so the fields read the live form again — after a
    /// landed Save, or when the page moves to another person.
    pub fn reset(&mut self) {
        self.edit = None;
    }
}

/// The derived label vocabulary — the union of live labels across every
/// overlay, offered for completion when labelling someone. Never its own kind.
/// One spelling per folded label (the most recently written), ordered by the
/// folded form.
pub fn label_vocabulary<'a>(overlays: impl IntoIterator<Item = &'a ContactOverlay>) -> Vec<String> {
    let mut best: BTreeMap<&str, &Register> = BTreeMap::new();
    for overlay in overlays {
        for (key, reg) in &overlay.labels {
            if reg.value.is_none() {
                continue;
            }
            best.entry(key.as_str())
                .and_modify(|cur| {
                    if reg.stamp.rank() > cur.stamp.rank() {
                        *cur = reg;
                    }
                })
                .or_insert(reg);
        }
    }
    best.values().filter_map(|r| r.value.clone()).collect()
}

/// The two items an identity-succession fold writes (`contacts.md` § The
/// private overlay → *When a person's identity succeeds*).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuccessionFold {
    /// The successor's overlay with the predecessor's live registers joined in.
    pub successor: ContactOverlay,
    /// The predecessor's overlay with every carried register cleared.
    pub predecessor: ContactOverlay,
}

/// Fold the `predecessor`'s overlay forward onto its verified `successor` —
/// `None` when the predecessor says nothing (the fixed point a reconcile
/// reaches).
///
/// Only the predecessor's **live** registers are carried, each through the
/// ordinary register join (so where both carry a nickname the newer stamp
/// wins, and labels union): a `None` there is either a removal or the fold's
/// own earlier clear, and carrying the latter would let a second fold wipe
/// what the first one moved. Each carried register is then cleared on the
/// predecessor **one millisecond past its own stamp**, never at a wall-clock
/// stamp, so:
///
/// - the result depends on the two inputs alone — two devices folding the
///   same pair write the same bytes;
/// - the clear outranks exactly what was carried, while a straggler's later
///   edit of the old identity outranks the clear, survives the plane join and
///   is carried by the next fold (the reconcile) instead of being lost.
pub fn fold_succession(
    predecessor: &ContactOverlay,
    successor: &ContactOverlay,
) -> Option<SuccessionFold> {
    if predecessor.is_empty() {
        return None;
    }
    let live = |r: &Register| {
        if r.value.is_some() {
            r.clone()
        } else {
            Register::default()
        }
    };
    let carried = ContactOverlay {
        nickname: live(&predecessor.nickname),
        notes: live(&predecessor.notes),
        labels: predecessor
            .labels
            .iter()
            .filter(|(_, r)| r.value.is_some())
            .map(|(k, r)| (k.clone(), r.clone()))
            .collect(),
        extra: BTreeMap::new(),
    };
    let clear = |r: &Register| {
        if r.value.is_some() {
            Register::new(Stamp::new(r.stamp.at_ms.saturating_add(1), [0; 32]), None)
        } else {
            r.clone()
        }
    };
    Some(SuccessionFold {
        successor: successor.join(&carried),
        predecessor: ContactOverlay {
            nickname: clear(&predecessor.nickname),
            notes: clear(&predecessor.notes),
            labels: predecessor
                .labels
                .iter()
                .map(|(k, r)| (k.clone(), clear(r)))
                .collect(),
            extra: predecessor.extra.clone(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{canonical_decode, canonical_encode};

    fn stamp(at_ms: i64, w: u8) -> Stamp {
        Stamp::new(at_ms, [w; 32])
    }

    fn reg(at_ms: i64, w: u8, v: Option<&str>) -> Register {
        Register::new(stamp(at_ms, w), v.map(str::to_string))
    }

    fn overlay(nick: Register, notes: Register, labels: &[(&str, Register)]) -> ContactOverlay {
        ContactOverlay {
            nickname: nick,
            notes,
            labels: labels
                .iter()
                .map(|(k, r)| (k.to_string(), r.clone()))
                .collect(),
            extra: BTreeMap::new(),
        }
    }

    /// Three overlays differing in every register, including exact stamp
    /// ties, removal markers and disjoint label keys.
    fn samples() -> Vec<ContactOverlay> {
        vec![
            ContactOverlay::default(),
            overlay(
                reg(10, 1, Some("Mum")),
                reg(5, 1, Some("likes tea")),
                &[
                    ("family", reg(10, 1, Some("Family"))),
                    ("work", reg(3, 1, None)),
                ],
            ),
            overlay(
                reg(10, 1, Some("Mother")), // exact stamp tie with the above
                reg(20, 2, None),
                &[
                    ("family", reg(12, 2, None)),
                    ("book club", reg(4, 2, Some("Book club"))),
                ],
            ),
            overlay(
                reg(30, 3, None),
                reg(20, 2, Some("other")), // exact tie with the above's notes
                &[
                    ("work", reg(9, 3, Some("Work"))),
                    ("family", reg(12, 2, Some("family"))),
                ],
            ),
        ]
    }

    fn bytes(o: &ContactOverlay) -> Vec<u8> {
        canonical_encode(o).unwrap()
    }

    #[test]
    fn the_join_is_commutative_associative_and_idempotent_in_bytes() {
        let s = samples();
        for a in &s {
            assert_eq!(bytes(&a.join(a)), bytes(a));
            for b in &s {
                assert_eq!(bytes(&a.join(b)), bytes(&b.join(a)));
                for c in &s {
                    assert_eq!(bytes(&a.join(b).join(c)), bytes(&a.join(&b.join(c))));
                }
            }
        }
    }

    #[test]
    fn a_nickname_on_one_device_and_notes_on_another_both_survive() {
        let base = ContactOverlay::default();
        let d1 = base
            .apply(
                &OverlayChanges {
                    nickname: Some(Some("Mum".into())),
                    ..Default::default()
                },
                stamp(100, 1),
            )
            .unwrap();
        let d2 = base
            .apply(
                &OverlayChanges {
                    notes: Some(Some("likes tea".into())),
                    ..Default::default()
                },
                stamp(50, 2),
            )
            .unwrap();
        let merged = d1.join(&d2);
        assert_eq!(merged.nickname(), Some("Mum"));
        assert_eq!(merged.notes(), Some("likes tea"));
    }

    #[test]
    fn a_removed_label_is_not_resurrected_by_a_stale_replica() {
        let stale = overlay(
            Register::default(),
            Register::default(),
            &[("family", reg(10, 1, Some("Family")))],
        );
        let mut changes = OverlayChanges::default();
        changes.labels.insert("family".into(), None);
        let removed = stale.apply(&changes, stamp(20, 2)).unwrap();
        assert!(removed.live_labels().is_empty());
        assert!(stale.join(&removed).live_labels().is_empty());
        assert!(removed.join(&stale).live_labels().is_empty());
        // The marker is retained, so the removal keeps winning.
        assert!(stale.join(&removed).labels.contains_key("family"));
    }

    #[test]
    fn a_local_write_outranks_a_register_stamped_in_the_future() {
        let ahead = overlay(reg(1_000, 9, Some("Old")), Register::default(), &[]);
        let next = ahead
            .apply(
                &OverlayChanges {
                    nickname: Some(Some("New".into())),
                    ..Default::default()
                },
                stamp(5, 1),
            )
            .unwrap();
        assert_eq!(next.join(&ahead).nickname(), Some("New"));
    }

    #[test]
    fn clearing_writes_none_everywhere_and_reads_as_no_overlay() {
        let full = samples()[1].clone();
        assert!(!full.is_empty());
        let cleared = full.cleared(stamp(1, 1));
        assert!(cleared.is_empty());
        assert!(full.join(&cleared).is_empty());
        assert_eq!(cleared.labels.len(), full.labels.len());
    }

    #[test]
    fn the_diff_writes_only_changed_registers() {
        let baseline = OverlayForm {
            nickname: "Mum".into(),
            notes: "likes tea".into(),
            labels: vec!["Family".into(), "Work".into()],
        };
        let mut staged = baseline.clone();
        assert!(changed_registers(&baseline, &staged).unwrap().is_empty());

        staged.nickname = "  Mother ".into();
        staged.labels = vec!["family".into(), "Book  club".into()];
        let changes = changed_registers(&baseline, &staged).unwrap();
        assert_eq!(changes.nickname, Some(Some("Mother".into())));
        assert_eq!(changes.notes, None);
        assert_eq!(changes.labels.get("family"), Some(&Some("family".into())));
        assert_eq!(
            changes.labels.get("book club"),
            Some(&Some("Book club".into()))
        );
        assert_eq!(changes.labels.get("work"), Some(&None));

        staged.nickname = "   ".into();
        let cleared = changed_registers(&baseline, &staged).unwrap();
        assert_eq!(cleared.nickname, Some(None));
    }

    #[test]
    fn the_editor_follows_the_live_form_until_the_first_edit_then_diffs_from_it() {
        let mut live = OverlayForm {
            nickname: "Mum".into(),
            notes: "likes tea".into(),
            labels: vec!["Family".into()],
        };
        let mut editor = OverlayEditor::default();
        assert_eq!(editor.form(&live), live);
        assert_eq!(
            editor.changes(),
            Ok(None),
            "nothing staged, nothing to write"
        );

        // A sibling device's edit re-paints the untouched section.
        live.notes = "likes coffee".into();
        assert_eq!(editor.form(&live).notes, "likes coffee");

        editor.set_nickname(&live, "Mother".into());
        assert!(editor.is_staged());
        // The sibling writes again; the staged form keeps its own baseline, so
        // the notes this user never touched are not part of the Save.
        live.notes = "likes cocoa".into();
        assert_eq!(editor.form(&live).notes, "likes coffee");
        let changes = editor.changes().unwrap().expect("the nickname changed");
        assert_eq!(changes.nickname, Some(Some("Mother".into())));
        assert_eq!(changes.notes, None);
        assert!(changes.labels.is_empty());

        editor.reset();
        assert_eq!(editor.form(&live), live);
    }

    #[test]
    fn the_editor_stages_labels_and_refuses_a_bad_one_without_staging() {
        let live = OverlayForm {
            labels: vec!["Family".into()],
            ..Default::default()
        };
        let mut editor = OverlayEditor::default();
        assert!(editor.add_label(&live, "   ").is_err());
        assert!(!editor.is_staged(), "a refused label stages nothing");

        editor.add_label(&live, " Book   club ").unwrap();
        editor.add_label(&live, "FAMILY").unwrap();
        assert_eq!(editor.form(&live).labels, vec!["Book club", "FAMILY"]);
        editor.remove_label(&live, 0);
        editor.remove_label(&live, 9);
        let changes = editor.changes().unwrap().expect("a label was re-spelled");
        assert_eq!(
            changes.labels,
            BTreeMap::from([("family".to_string(), Some("FAMILY".to_string()))])
        );

        // Back at the baseline: staged, but nothing to write.
        let mut same = OverlayEditor::default();
        same.set_notes(&live, String::new());
        assert_eq!(same.changes(), Ok(None));

        // A bounds refusal surfaces at the diff and keeps the staging.
        let mut long = OverlayEditor::default();
        long.set_nickname(&live, "x".repeat(MAX_NICKNAME_CHARS + 1));
        assert!(long.changes().is_err());
        assert!(long.is_staged());
    }

    #[test]
    fn a_save_does_not_overwrite_a_field_another_device_changed_meanwhile() {
        // The form was opened on notes "a"; another device wrote "b" since.
        let live = overlay(Register::default(), reg(50, 2, Some("b")), &[]);
        let baseline = OverlayForm {
            notes: "a".into(),
            ..Default::default()
        };
        let staged = OverlayForm {
            nickname: "Mum".into(),
            notes: "a".into(),
            labels: vec![],
        };
        let changes = changed_registers(&baseline, &staged).unwrap();
        let saved = live.apply(&changes, stamp(60, 1)).unwrap();
        assert_eq!(saved.notes(), Some("b"));
        assert_eq!(saved.nickname(), Some("Mum"));
    }

    #[test]
    fn the_bounds_refuse_with_localized_text() {
        let base = OverlayForm::default();
        let too_long = OverlayForm {
            nickname: "x".repeat(MAX_NICKNAME_CHARS + 1),
            ..Default::default()
        };
        assert_eq!(
            changed_registers(&base, &too_long).unwrap_err().key,
            "profile.private_nickname_too_long"
        );
        // 64 multi-byte characters are fine: the bound is in characters.
        let wide = OverlayForm {
            nickname: "é".repeat(MAX_NICKNAME_CHARS),
            ..Default::default()
        };
        assert!(changed_registers(&base, &wide).is_ok());
        let notes = OverlayForm {
            notes: "n".repeat(MAX_NOTES_BYTES + 1),
            ..Default::default()
        };
        assert_eq!(
            changed_registers(&base, &notes).unwrap_err().key,
            "profile.private_notes_too_long"
        );
        assert_eq!(
            validate_label("   ").unwrap_err().key,
            "profile.private_label_empty"
        );
        assert_eq!(
            validate_label(&"l".repeat(MAX_LABEL_BYTES + 1))
                .unwrap_err()
                .key,
            "profile.private_label_too_long"
        );
    }

    #[test]
    fn the_label_caps_refuse_the_new_label_rather_than_pruning() {
        let mut full = ContactOverlay::default();
        for i in 0..MAX_LIVE_LABELS {
            full.labels
                .insert(format!("l{i}"), reg(1, 1, Some(&format!("L{i}"))));
        }
        let mut add = OverlayChanges::default();
        add.labels.insert("new".into(), Some("New".into()));
        assert_eq!(
            full.apply(&add, stamp(2, 1)).unwrap_err().key,
            "profile.private_too_many_labels"
        );

        let mut markers = ContactOverlay::default();
        for i in 0..MAX_LABEL_KEYS {
            markers.labels.insert(format!("m{i}"), reg(1, 1, None));
        }
        assert_eq!(
            markers.apply(&add, stamp(2, 1)).unwrap_err().key,
            "profile.private_label_history_full"
        );
        // Re-adding a label whose marker already exists takes no new key.
        let mut readd = OverlayChanges::default();
        readd.labels.insert("m0".into(), Some("M0".into()));
        assert!(markers.apply(&readd, stamp(2, 1)).is_ok());
    }

    #[test]
    fn the_label_vocabulary_is_the_union_of_live_labels() {
        let s = samples();
        let vocab = label_vocabulary(&s);
        // "family" is live on sample 1 (Family @10) and sample 3 (family @12):
        // the newer spelling wins; sample 2's removal does not hide it.
        assert_eq!(vocab, vec!["Book club", "family", "Work"]);
    }

    #[test]
    fn the_record_round_trips_and_preserves_unknown_fields() {
        let o = samples()[1].clone();
        assert_eq!(canonical_decode::<ContactOverlay>(&bytes(&o)).unwrap(), o);

        #[derive(Serialize)]
        struct Newer {
            nickname: Register,
            future_field: u64,
        }
        let newer = canonical_encode(&Newer {
            nickname: reg(1, 1, Some("x")),
            future_field: 7,
        })
        .unwrap();
        let decoded: ContactOverlay = canonical_decode(&newer).unwrap();
        assert_eq!(decoded.nickname(), Some("x"));
        assert!(decoded.extra.contains_key("future_field"));
        assert!(canonical_encode(&decoded).unwrap().len() >= newer.len());
    }

    #[test]
    fn a_succession_fold_carries_every_live_register_and_empties_the_predecessor() {
        let predecessor = overlay(
            reg(10, 1, Some("Mum")),
            reg(5, 1, Some("likes tea")),
            &[
                ("family", reg(10, 1, Some("Family"))),
                ("work", reg(3, 1, None)),
            ],
        );
        // The successor was already labelled, and named more recently.
        let successor = overlay(
            reg(20, 2, Some("Mother")),
            Register::default(),
            &[("book club", reg(4, 2, Some("Book club")))],
        );
        let fold = fold_succession(&predecessor, &successor).expect("something to carry");
        assert_eq!(
            fold.successor.nickname(),
            Some("Mother"),
            "the newer stamp wins"
        );
        assert_eq!(fold.successor.notes(), Some("likes tea"));
        assert_eq!(fold.successor.live_labels(), vec!["Book club", "Family"]);
        assert!(fold.predecessor.is_empty());
        // Replicated anywhere, the emptied predecessor still dominates the
        // registers it carried.
        assert!(predecessor.join(&fold.predecessor).is_empty());
        assert_eq!(fold_succession(&fold.predecessor, &fold.successor), None);
    }

    #[test]
    fn two_devices_folding_the_same_pair_write_the_same_bytes() {
        for pred in samples() {
            for succ in samples() {
                let a = fold_succession(&pred, &succ);
                let b = fold_succession(&pred, &succ);
                assert_eq!(
                    a.as_ref()
                        .map(|f| (bytes(&f.successor), bytes(&f.predecessor))),
                    b.as_ref()
                        .map(|f| (bytes(&f.successor), bytes(&f.predecessor)))
                );
                assert_eq!(a.is_none(), pred.is_empty());
            }
        }
    }

    #[test]
    fn a_straggler_edit_after_the_fold_survives_and_folds_without_undoing_it() {
        let predecessor = overlay(reg(10, 1, Some("Mum")), Register::default(), &[]);
        let fold = fold_succession(&predecessor, &ContactOverlay::default()).unwrap();
        // A device that never saw the fold edits the old identity's notes
        // later, then syncs.
        let straggler = predecessor
            .apply(
                &OverlayChanges {
                    notes: Some(Some("moved house".into())),
                    ..Default::default()
                },
                stamp(40, 3),
            )
            .unwrap();
        let merged = fold.predecessor.join(&straggler);
        assert_eq!(
            merged.notes(),
            Some("moved house"),
            "the later edit survives"
        );
        assert_eq!(merged.nickname(), None, "what was carried stays carried");
        let again = fold_succession(&merged, &fold.successor).expect("the reconcile folds it");
        assert_eq!(again.successor.notes(), Some("moved house"));
        assert_eq!(
            again.successor.nickname(),
            Some("Mum"),
            "the predecessor's cleared registers never wipe what the first fold moved"
        );
    }

    #[test]
    fn a_removal_on_the_predecessor_is_not_carried_onto_the_successor() {
        let predecessor = overlay(
            Register::default(),
            reg(5, 1, Some("n")),
            &[("family", reg(30, 1, None))],
        );
        let successor = overlay(
            Register::default(),
            Register::default(),
            &[("family", reg(20, 2, Some("Family")))],
        );
        let fold = fold_succession(&predecessor, &successor).unwrap();
        assert_eq!(fold.successor.live_labels(), vec!["Family"]);
    }

    #[test]
    fn the_overlay_key_is_the_lowercased_actor_id() {
        let id = "AB".repeat(32);
        assert_eq!(overlay_key(&id), Some("ab".repeat(32)));
        assert_eq!(overlay_key("abc"), None);
        assert_eq!(overlay_key(&"zz".repeat(32)), None);
    }
}
