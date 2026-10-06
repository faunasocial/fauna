//! The **authoring** half of the feature plane — the one editor an admin uses
//! to write the nest-wide document, a guardian to limit one ward, and a person
//! to limit themselves (`docs/goal/architecture/dynamic-features.md`
//! § Authoring surfaces).
//!
//! # One editor, three hosts
//!
//! The quota grammar is identical at every tier that can express one, so the
//! app side is one editor too: the admin Nest page hosts it at
//! [`AuthoringTier::Admin`], the Feature limits section at
//! [`AuthoringTier::SelfImposed`], and the family policy screen at the
//! ward-keyed [`AuthoringTier::Guardian`]. The hosts differ in exactly two
//! things — where the save goes, and the sentence naming whom it binds — and
//! both are decided here, so an app paints and forwards keystrokes and nothing
//! else (priorities #1 and #2).
//!
//! The guardian tier mints no kind of its own (`family-safety.md` § Wire & data
//! shape): its read is the ward's `fauna.family.status` entry, folded to the
//! same authored items by [`ward_authored_items`], and its write is the ward's
//! freshly-read policy with only the `features` sub-document replaced
//! ([`ward_policy_with`]), through `fauna.family.policy.update`.
//!
//! # Seeded from the AUTHORED document, never the effective one
//!
//! A save is a whole-document replace. `fauna.features.status` answers the
//! *effective* meet, in which a bound this tier authored that lost the MIN to a
//! tighter tier is invisible — an editor seeded from it would silently drop that
//! bound on its next save. So every read here goes through the authored-document
//! kinds (`fauna.features.policy.get` / `fauna.features.self_limits.get`), and a
//! save re-reads before the editor repaints ([`FeaturesClient::save`]).
//!
//! # Tighten-only is rendered, never enforced twice
//!
//! Nothing here refuses a looser value: the nest's meet already makes relaxing
//! unrepresentable, and a client-side "too loose" check would be a second
//! answer to a question the nest deliberately owns. What the editor owes is
//! honesty about effect — a cell whose value the **ceiling** (the meet of the
//! tiers outside this one, composed nest-side and carried on the read) already
//! binds at or below gets a *no effect* note. The ceiling arrives finished; this
//! module compares against it and composes nothing (the crate-wide pin,
//! [`crate::view_model`]'s `this_crate_never_recomposes_the_meet`).

use serde::{Deserialize, Serialize};

use fauna_client_family::FamilyClient;
use fauna_core::feature_gate::{
    Availability, BoundSource, FeaturePolicy, GatedFeature, MagnitudeUnit, QuotaDimension,
    RuleTier, Window, WindowedBounds, entry,
};
use fauna_core::localized::LocalizedText;
use fauna_protocol::RpcRequester;
use fauna_protocol::family::{FamilyWardInfo, ReachPolicy};
use fauna_protocol::features::{
    AuthoredPolicyItem, FeaturePolicyReadReply, FeaturePolicyReadRequest, FeaturePolicyUpdateReply,
    FeaturePolicyUpdateRequest,
};

use crate::FeaturesClient;
use crate::row::{dimension_label, magnitude, window_label};
use crate::view_model::{capability_token, denied_by_key, feature_label};

// ── The tier ─────────────────────────────────────────────────────────────

/// Which tier an editor authors — the only thing that tells the three hosts
/// apart.
///
/// **The tier picks the KIND, client-side; it is never a wire field.** A tier
/// field on the request would be a second, forgeable answer to a question the
/// nest's dispatch already answers (a self-limits caller could name the admin
/// tier), so the protocol has none and this enum exists only to choose between
/// the two kinds of each pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthoringTier {
    /// Tier 3 — the admin's nest-wide document. Admin-class kinds.
    Admin,
    /// Tier 4 — a guardian's document for ONE ward, keyed by the ward's actor
    /// id. It mints no kind of its own: the read is the ward's entry on
    /// `fauna.family.status` and the write rides `fauna.family.policy.update`
    /// (`family-safety.md` § Wire & data shape — the guardian's feature-limits
    /// editor seed).
    Guardian { ward: [u8; 32] },
    /// Tier 5 — the bearer's own document. User-class kinds.
    SelfImposed,
}

impl AuthoringTier {
    /// The authored-document read for this tier.
    pub fn read_kind(self) -> &'static str {
        match self {
            AuthoringTier::Admin => "fauna.features.policy.get",
            AuthoringTier::Guardian { .. } => "fauna.family.status",
            AuthoringTier::SelfImposed => "fauna.features.self_limits.get",
        }
    }

    /// The whole-document write for this tier.
    pub fn write_kind(self) -> &'static str {
        match self {
            AuthoringTier::Admin => "fauna.features.policy.update",
            AuthoringTier::Guardian { .. } => "fauna.family.policy.update",
            AuthoringTier::SelfImposed => "fauna.features.self_limits.update",
        }
    }

    /// `admin` | `guardian` | `self` — the client-rendering vocabulary the
    /// faces carry across their boundary (the [`crate::row`] tier spelling, so
    /// one concept has one name on every app).
    pub fn key(self) -> &'static str {
        self.rule_tier().as_str()
    }

    /// The inverse of [`Self::key`] for the two tiers a bare key can name —
    /// `None` for anything else, **including `guardian`**: a guardian editor is
    /// opened by the ward-keyed call, never from a key that names no ward.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "admin" => Some(AuthoringTier::Admin),
            "self" => Some(AuthoringTier::SelfImposed),
            _ => None,
        }
    }

    /// The rule-setter tier this editor writes.
    pub fn rule_tier(self) -> RuleTier {
        match self {
            AuthoringTier::Admin => RuleTier::Admin,
            AuthoringTier::Guardian { .. } => RuleTier::Guardian,
            AuthoringTier::SelfImposed => RuleTier::SelfImposed,
        }
    }
}

// ── The host rows ────────────────────────────────────────────────────────

/// One registry member's row on an authoring host: its name and the tier's
/// authored document in words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthoredRow {
    /// The stable key — `payments` | `zaps` | `p2p-share`.
    pub feature: String,
    pub name: LocalizedText,
    /// The tier's AUTHORED document in words ([`authored_summary`]).
    pub summary: LocalizedText,
    /// Whether the tier holds a document for this member — readable or not.
    /// Decides whether *Remove limit* renders.
    pub has_document: bool,
    /// A stored document the nest cannot decode (enforced as a deny).
    pub unreadable: bool,
}

/// The tier's authored document for one member, in words.
///
/// Five states, never collapsed into each other:
///
/// - **unreadable** — a stored document the nest cannot decode. It is enforced
///   as a deny (§ Fail posture), so reading it as *No limit set* would be the
///   silent gate in its third costume; it says so instead.
/// - **none** — the tier has no opinion. *Not* an authored allow: an absent
///   tier drops out of the meet entirely.
/// - **off** — an authored deny.
/// - **on** — an authored allow with no bounds.
/// - **on, N limits** — bounds authored; the detail is the editor's to show.
pub fn authored_summary(item: &AuthoredPolicyItem) -> LocalizedText {
    summary_of(item.policy.as_ref(), item.unreadable)
}

fn summary_of(policy: Option<&FeaturePolicy>, unreadable: bool) -> LocalizedText {
    if unreadable {
        return LocalizedText::key("features.authored_unreadable");
    }
    let Some(policy) = policy else {
        return LocalizedText::key("features.authored_none");
    };
    if policy.availability.denies() {
        return LocalizedText::key("features.authored_off");
    }
    match bound_count(policy) {
        0 => LocalizedText::key("features.authored_on"),
        1 => LocalizedText::key("features.authored_limited_one"),
        n => LocalizedText::key_arg("features.authored_limited", "count", n.to_string()),
    }
}

/// How many bounds a document carries — every windowed cell plus the
/// per-operation cap.
fn bound_count(policy: &FeaturePolicy) -> usize {
    let windowed: usize = [
        QuotaDimension::Operations,
        QuotaDimension::Counterparties,
        QuotaDimension::Volume,
    ]
    .iter()
    .map(|d| {
        Window::ALL
            .iter()
            .filter(|w| policy.bounds(*d).get(**w).is_some())
            .count()
    })
    .sum();
    windowed + usize::from(policy.per_operation_max.is_some())
}

/// The row for one member.
pub fn authored_row(item: &AuthoredPolicyItem) -> AuthoredRow {
    AuthoredRow {
        feature: item.feature.as_str().to_string(),
        name: feature_label(item.feature),
        summary: authored_summary(item),
        has_document: item.policy.is_some() || item.unreadable,
        unreadable: item.unreadable,
    }
}

/// One tier's authored documents, for the members this nest carries — the
/// whole of what an authoring host renders and opens its editor over.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AuthoredSurface {
    /// Registry order. A member whose capability token the nest does not
    /// advertise has already been dropped ([`authored_surface`]).
    pub items: Vec<AuthoredPolicyItem>,
    /// Whom the tier's documents bind, where the editor's title has to name
    /// them: the ward's handle at the guardian tier, `None` at the other two.
    pub subject: Option<String>,
}

impl AuthoredSurface {
    /// Every row, in registry order.
    pub fn rows(&self) -> Vec<AuthoredRow> {
        self.items.iter().map(authored_row).collect()
    }

    /// The authored item for one member, by stable key.
    pub fn item(&self, feature: &str) -> Option<&AuthoredPolicyItem> {
        self.items.iter().find(|i| i.feature.as_str() == feature)
    }

    /// An editor opened over one member at `tier`, or `None` when this
    /// surface does not carry the member.
    pub fn editor(&self, tier: AuthoringTier, feature: &str) -> Option<PolicyEditor> {
        self.item(feature).map(|item| {
            let mut editor = PolicyEditor::open(tier, item.clone());
            editor.subject = self.subject.clone();
            editor
        })
    }
}

/// Fold a read reply into the surface a host renders, dropping every member
/// this nest build does not carry.
///
/// **The same hide rule as the transparency rows** ([`crate::affordance`]'s
/// `hidden` arm, and for the same reason): `fauna_core::feature_gate::registry`
/// is not cfg-gated, so a payments-excised nest still answers for `payments` —
/// and an editor row for a plane the artifact does not carry would author a
/// document nothing evaluates.
pub fn authored_surface(
    reply: FeaturePolicyReadReply,
    nest_capabilities: &[String],
) -> AuthoredSurface {
    AuthoredSurface {
        items: reply
            .features
            .into_iter()
            // A feature this build does not know is never offered for editing
            // (`transport.md` § Rule 3 in full).
            .filter(|item| item.feature.is_known())
            .filter(|item| carried(item.feature, nest_capabilities))
            .collect(),
        ..Default::default()
    }
}

/// Whether this nest build carries `feature`'s plane — its capability token is
/// advertised, or it has none to advertise.
fn carried(feature: GatedFeature, nest_capabilities: &[String]) -> bool {
    match capability_token(feature) {
        Some(token) => fauna_protocol::discovery::capability::supports(nest_capabilities, token),
        None => true,
    }
}

// ── The guardian tier's seed: the ward's status entry ────────────────────

/// A ward's `fauna.family.status` entry as the authored items the editor opens
/// over — the guardian tier's read, mapped to the shape the other two tiers'
/// read kinds answer with (`family-safety.md` § Wire & data shape).
///
/// One item per member of the entry's ceiling, which the nest fills for every
/// registry member on every ward entry it answers.
///
/// ⚠ While the entry is flagged `features_unreadable`, `policy.features` holds
/// the **enforced deny** of every member, not anything the guardian authored —
/// every item is then `policy: None, unreadable: true`, never seeded from it.
pub fn ward_authored_items(ward: &FamilyWardInfo) -> Vec<AuthoredPolicyItem> {
    let authored = ward
        .policy
        .features
        .as_ref()
        .filter(|_| !ward.features_unreadable);
    ward.features_ceiling
        .iter()
        .map(|entry| AuthoredPolicyItem {
            feature: entry.feature,
            policy: authored.and_then(|documents| documents.get(entry.feature.as_str()).cloned()),
            unreadable: ward.features_unreadable,
            ceiling: entry.ceiling,
            extra: Default::default(),
        })
        .collect()
}

/// The guardian host's whole surface for one ward — the document in words,
/// and the unsupported members dropped as on every host.
pub fn ward_surface(ward: &FamilyWardInfo, nest_capabilities: &[String]) -> AuthoredSurface {
    AuthoredSurface {
        subject: Some(ward.handle.clone()),
        ..authored_surface(
            FeaturePolicyReadReply {
                features: ward_authored_items(ward),
                extra: Default::default(),
            },
            nest_capabilities,
        )
    }
}

/// The document a guardian-tier save sends: the ward's policy **as freshly
/// read**, with only `features` replaced by the whole sub-document — `feature`
/// set, or removed for `None`.
///
/// The map is always sent, even empty: absent means *unchanged* on this kind,
/// so absence could never clear the last limit. Members this build does not
/// know ride along untouched. Over an unreadable stored document the map
/// starts EMPTY — what the entry carries then is the enforced deny, and
/// writing that back would turn a storage fault into an authored one.
pub fn ward_policy_with(
    ward: &FamilyWardInfo,
    feature: GatedFeature,
    policy: Option<FeaturePolicy>,
) -> ReachPolicy {
    let mut document = ward.policy.clone();
    let mut features = document
        .features
        .take()
        .filter(|_| !ward.features_unreadable)
        .unwrap_or_default();
    match policy {
        Some(policy) => {
            features.insert(feature.as_str().to_string(), policy);
        }
        None => {
            features.remove(feature.as_str());
        }
    }
    document.features = Some(features);
    document
}

// ── The editor ───────────────────────────────────────────────────────────

/// One editable bound: a declared (dimension, window) cell, or the
/// per-operation cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorSlot {
    Window(QuotaDimension, Window),
    PerOperation,
}

/// The editable slots for `feature`, in the order the read paints them:
/// dimension major, window minor, only the dimensions the registry DECLARES for
/// the member, then the per-operation cap last where the member declares
/// `volume`.
///
/// Rendering only declared dimensions makes the nest's `UndeclaredDimension`
/// refusal unreachable from an app rather than merely handled.
pub fn editor_slots(feature: GatedFeature) -> Vec<EditorSlot> {
    let e = entry(feature);
    let mut slots = Vec::new();
    for dimension in [
        QuotaDimension::Operations,
        QuotaDimension::Counterparties,
        QuotaDimension::Volume,
    ] {
        if !e.declares(dimension) {
            continue;
        }
        for window in Window::ALL {
            slots.push(EditorSlot::Window(dimension, window));
        }
    }
    if e.declares(QuotaDimension::Volume) {
        slots.push(EditorSlot::PerOperation);
    }
    slots
}

/// The shared editor's state: the tier, the authored item it was opened over
/// (which carries the ceiling its notes compare against), the On/Off choice and
/// the typed text of every slot.
///
/// Pure and clock-free; the host owns it and forwards every keystroke to it.
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyEditor {
    tier: AuthoringTier,
    item: AuthoredPolicyItem,
    slots: Vec<EditorSlot>,
    on: bool,
    texts: Vec<String>,
    /// Whom the limit binds, where the title names them — the ward's handle at
    /// the guardian tier ([`AuthoredSurface::subject`]).
    subject: Option<String>,
}

impl PolicyEditor {
    /// Open over `item`, seeded from its **authored** document: *Off* for an
    /// authored deny, *On* otherwise, and every slot's text from the bound the
    /// document carries (empty — no opinion — where it carries none).
    ///
    /// An unreadable document opens EMPTY: there is nothing to seed from, and
    /// re-saving or removing from this editor is the in-app recovery the fail
    /// posture promises.
    pub fn open(tier: AuthoringTier, item: AuthoredPolicyItem) -> Self {
        let slots = editor_slots(item.feature);
        let unit = entry(item.feature).unit;
        let authored = if item.unreadable {
            None
        } else {
            item.policy.clone()
        };
        let on = authored.as_ref().is_none_or(|p| !p.availability.denies());
        let texts = slots
            .iter()
            .map(|slot| {
                authored
                    .as_ref()
                    .and_then(|p| slot_value(p, *slot))
                    .map(|v| input_text(*slot, unit, v))
                    .unwrap_or_default()
            })
            .collect();
        PolicyEditor {
            tier,
            item,
            slots,
            on,
            texts,
            subject: None,
        }
    }

    /// Re-open over the same member from a fresh read — what a host does after
    /// every save, so the editor shows what the nest now stores rather than what
    /// was typed. `None` when the read no longer carries the member.
    pub fn reseeded(&self, reply: &FeaturePolicyReadReply) -> Option<PolicyEditor> {
        reply
            .features
            .iter()
            .find(|i| i.feature == self.item.feature)
            .map(|item| PolicyEditor {
                subject: self.subject.clone(),
                ..PolicyEditor::open(self.tier, item.clone())
            })
    }

    pub fn tier(&self) -> AuthoringTier {
        self.tier
    }

    pub fn feature(&self) -> GatedFeature {
        self.item.feature
    }

    /// The stable key of the member being edited.
    pub fn feature_key(&self) -> &'static str {
        self.item.feature.as_str()
    }

    pub fn is_on(&self) -> bool {
        self.on
    }

    pub fn set_on(&mut self, on: bool) {
        self.on = on;
    }

    /// Replace one slot's typed text. An out-of-range index is ignored — a
    /// stale keystroke from a repaint must not panic a host.
    pub fn set_cell(&mut self, index: usize, text: impl Into<String>) {
        if let Some(slot) = self.texts.get_mut(index) {
            *slot = text.into();
        }
    }

    /// One slot's typed text (empty for an out-of-range index).
    pub fn cell_text(&self, index: usize) -> &str {
        self.texts.get(index).map(String::as_str).unwrap_or("")
    }

    /// How many slots the member has (whether or not *On* renders them).
    pub fn cell_count(&self) -> usize {
        self.slots.len()
    }

    /// Whether the tier holds a document — the *Remove limit* gate.
    pub fn has_document(&self) -> bool {
        self.item.policy.is_some() || self.item.unreadable
    }

    /// The document a save writes, or the localized reason it cannot be.
    ///
    /// *Off* is a deny with no bounds, whatever the cells hold (they are not
    /// rendered while Off, so a stale typo there must not block the save). *On*
    /// is `limit` when at least one cell carries a bound and `allow` when none —
    /// the rule-setter chooses whether and how much, never the serialization.
    pub fn draft(&self) -> Result<FeaturePolicy, LocalizedText> {
        if !self.on {
            return Ok(FeaturePolicy::DENIED);
        }
        let unit = entry(self.item.feature).unit;
        let mut policy = FeaturePolicy::NO_OPINION;
        for (slot, text) in self.slots.iter().zip(&self.texts) {
            let Some(value) = parse_input(*slot, unit, text)? else {
                continue;
            };
            match *slot {
                EditorSlot::Window(dimension, window) => {
                    let bounds = bounds_mut(&mut policy, dimension);
                    *bounds = bounds.tightened_with(window, value);
                }
                EditorSlot::PerOperation => policy.per_operation_max = Some(value),
            }
        }
        policy.availability = if policy == FeaturePolicy::NO_OPINION {
            Availability::Allow
        } else {
            Availability::Limit
        };
        Ok(policy)
    }

    /// Everything the editor paints, derived once here.
    pub fn view(&self) -> PolicyEditorView {
        let unit = entry(self.item.feature).unit;
        let cells = if self.on {
            self.slots
                .iter()
                .zip(&self.texts)
                .map(|(slot, text)| {
                    let (note, note_magnitude) = self.note(*slot, unit, text);
                    EditorCell {
                        label: slot_label(*slot, unit),
                        text: text.clone(),
                        note,
                        note_magnitude,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        PolicyEditorView {
            feature: self.feature_key().to_string(),
            tier: self.tier.key().to_string(),
            title: match self.tier {
                AuthoringTier::Admin => LocalizedText::key_arg(
                    "features.editor_title_admin",
                    "feature",
                    feature_label(self.item.feature).key,
                ),
                AuthoringTier::SelfImposed => LocalizedText::key_arg(
                    "features.editor_title_self",
                    "feature",
                    feature_label(self.item.feature).key,
                ),
                // `{ward}` is a handle, not a key: it misses the nested lookup
                // and is substituted verbatim.
                AuthoringTier::Guardian { .. } => LocalizedText::key_args(
                    "features.editor_title_guardian",
                    [
                        ("feature", feature_label(self.item.feature).key),
                        ("ward", self.subject.clone().unwrap_or_default()),
                    ],
                ),
            },
            on: self.on,
            cells,
            off_note: self
                .item
                .ceiling
                .denied_by
                .map(|tier| LocalizedText::key(denied_by_key(tier))),
            can_remove: self.has_document(),
            hint: LocalizedText::key("features.editor_hint"),
        }
    }

    /// The no-effect note for one slot, when the ceiling already binds at or
    /// below the typed value. Text that does not parse, or an empty cell,
    /// carries no note: there is no value to compare.
    fn note(
        &self,
        slot: EditorSlot,
        unit: MagnitudeUnit,
        text: &str,
    ) -> (Option<LocalizedText>, Option<LocalizedText>) {
        let Ok(Some(value)) = parse_input(slot, unit, text) else {
            return (None, None);
        };
        let ceiling: Option<BoundSource> = match slot {
            EditorSlot::Window(dimension, window) => {
                self.item.ceiling.bounds(dimension).get(window)
            }
            EditorSlot::PerOperation => self.item.ceiling.per_operation_max,
        };
        match ceiling {
            Some(bound) if bound.limit <= value => (
                Some(LocalizedText::key_arg(
                    no_effect_key(bound.tier),
                    "limit",
                    bound.limit.to_string(),
                )),
                is_magnitude(slot).then(|| magnitude(bound.limit, unit)),
            ),
            _ => (None, None),
        }
    }
}

/// Everything the shared editor paints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyEditorView {
    /// The stable key of the member being edited.
    pub feature: String,
    /// `admin` | `guardian` | `self`.
    pub tier: String,
    /// The member and whom the limit binds. Its `{feature}` is a key — resolve
    /// with [`LocalizedText::resolve_nested`].
    pub title: LocalizedText,
    /// Which of the two radios is marked.
    pub on: bool,
    /// One per editable slot while *On*; empty while *Off* (a deny carries no
    /// bounds, so there is nothing to type).
    pub cells: Vec<EditorCell>,
    /// Set when an outer tier already denies the member — the *Off* choice (and
    /// every bound) has no effect. The `denied_by_*` sentence names who.
    pub off_note: Option<LocalizedText>,
    /// Whether *Remove limit* renders — only while a document exists.
    pub can_remove: bool,
    /// The one-line rule every cell follows: empty is no limit, zero is one.
    pub hint: LocalizedText,
}

/// One editable cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorCell {
    /// What the cell bounds, naming the unit a magnitude is typed in. Its
    /// substitutions are keys — resolve with [`LocalizedText::resolve_nested`].
    pub label: LocalizedText,
    /// The typed text, verbatim.
    pub text: String,
    /// The no-effect note, present only when the ceiling already binds at or
    /// below the typed value. `{limit}` is a finished number **unless**
    /// [`Self::note_magnitude`] is `Some`, in which case the app resolves that
    /// and substitutes it instead — [`crate::RowCell::magnitudes`]' two-level
    /// shape, for the same reason.
    pub note: Option<LocalizedText>,
    pub note_magnitude: Option<LocalizedText>,
}

/// A cell's no-effect note, fully composed (`None` when it has none) — the
/// native-Rust half of [`EditorCell::note_magnitude`]'s two-level contract, as
/// [`crate::cell_value_text`] is for the transparency cells.
pub fn editor_note_text<F, S>(cell: &EditorCell, lookup: F) -> Option<String>
where
    F: Fn(&str) -> Option<S> + Copy,
    S: AsRef<str>,
{
    let note = cell.note.as_ref()?;
    let Some(magnitude) = &cell.note_magnitude else {
        return Some(note.resolve(lookup));
    };
    let mut composed = note.clone();
    composed
        .args
        .insert("limit".into(), magnitude.resolve(lookup));
    Some(composed.resolve(lookup))
}

/// The verdict line after a successful save.
pub fn saved_text() -> LocalizedText {
    LocalizedText::key("features.editor_saved")
}

/// The verdict line after a successful *Remove limit*.
pub fn removed_text() -> LocalizedText {
    LocalizedText::key("features.editor_removed")
}

// ── The call surface ─────────────────────────────────────────────────────

/// Why a save did not land.
#[derive(Debug, Clone, PartialEq)]
pub enum SaveError<E> {
    /// A cell the shared parser could not read — nothing was dispatched.
    Invalid(LocalizedText),
    /// The nest refused, or the transport failed.
    Rpc(E),
}

impl<E: std::fmt::Display> SaveError<E> {
    /// The sentence for the host page's `error-message`.
    pub fn text(&self) -> LocalizedText {
        match self {
            SaveError::Invalid(reason) => reason.clone(),
            SaveError::Rpc(e) => {
                LocalizedText::key_arg("features.editor_save_failed", "error", e.to_string())
            }
        }
    }
}

/// A landed write: its verdict line and the fresh authored read the editor
/// re-seeds from ([`PolicyEditor::reseeded`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    pub status: LocalizedText,
    pub reply: FeaturePolicyReadReply,
}

impl<R: RpcRequester> FeaturesClient<R> {
    /// One tier's authored documents — `fauna.features.policy.get` or
    /// `fauna.features.self_limits.get`, the tier picking the kind; at the
    /// guardian tier, the ward's `fauna.family.status` entry mapped to the same
    /// items ([`ward_authored_items`]) — empty when the caller no longer guards
    /// the ward.
    pub async fn authored(&self, tier: AuthoringTier) -> Result<FeaturePolicyReadReply, R::Error> {
        if let AuthoringTier::Guardian { ward } = tier {
            let entry = self.ward_entry(&ward).await?;
            return Ok(FeaturePolicyReadReply {
                features: entry.as_ref().map(ward_authored_items).unwrap_or_default(),
                extra: Default::default(),
            });
        }
        self.nest
            .request(tier.read_kind(), FeaturePolicyReadRequest::default())
            .await
    }

    /// The caller's guardian-side entry for `ward`, freshly read; `None` when
    /// the caller does not guard that account.
    async fn ward_entry(&self, ward: &[u8; 32]) -> Result<Option<FamilyWardInfo>, R::Error> {
        let status = FamilyClient::new(&self.nest).status().await?;
        Ok(status
            .wards
            .into_iter()
            .find(|entry| entry.actor_id.as_slice() == ward))
    }

    /// Write (`Some`) or clear (`None`) one tier's document for one member — a
    /// whole-document replace. Clearing returns the tier to *no opinion*, which
    /// is not an authored allow.
    ///
    /// At the guardian tier the document rides `fauna.family.policy.update`:
    /// the ward's policy is re-read and sent back verbatim with only its
    /// `features` map replaced ([`ward_policy_with`]). A ward the caller no
    /// longer guards dispatches nothing — there is no policy to carry, and a
    /// default one would reset the reach knobs.
    pub async fn author(
        &self,
        tier: AuthoringTier,
        feature: GatedFeature,
        policy: Option<FeaturePolicy>,
    ) -> Result<FeaturePolicyUpdateReply, SaveError<R::Error>> {
        if let AuthoringTier::Guardian { ward } = tier {
            let entry = self
                .ward_entry(&ward)
                .await
                .map_err(SaveError::Rpc)?
                .ok_or_else(|| {
                    SaveError::Invalid(LocalizedText::key("features.editor_ward_missing"))
                })?;
            FamilyClient::new(&self.nest)
                .policy_update(ward.to_vec(), ward_policy_with(&entry, feature, policy))
                .await
                .map_err(SaveError::Rpc)?;
            return Ok(FeaturePolicyUpdateReply {
                ok: true,
                extra: Default::default(),
            });
        }
        self.nest
            .request(
                tier.write_kind(),
                FeaturePolicyUpdateRequest {
                    feature,
                    policy,
                    extra: Default::default(),
                },
            )
            .await
            .map_err(SaveError::Rpc)
    }

    /// **The call an authoring host makes** — the tier's authored documents
    /// joined with the nest's capability set, the unsupported members dropped.
    pub async fn authored_surface(&self, tier: AuthoringTier) -> Result<AuthoredSurface, R::Error> {
        let capabilities = self.node_capabilities().await?;
        if let AuthoringTier::Guardian { ward } = tier {
            let entry = self.ward_entry(&ward).await?;
            return Ok(entry
                .map(|entry| ward_surface(&entry, &capabilities))
                .unwrap_or_default());
        }
        let reply = self.authored(tier).await?;
        Ok(authored_surface(reply, &capabilities))
    }

    /// Save the editor's draft: parse (a refusal dispatches nothing), write the
    /// whole document, then re-read so the editor re-seeds from what the nest
    /// now stores.
    pub async fn save(&self, editor: &PolicyEditor) -> Result<Saved, SaveError<R::Error>> {
        let policy = editor.draft().map_err(SaveError::Invalid)?;
        self.author(editor.tier(), editor.feature(), Some(policy))
            .await?;
        let reply = self.authored(editor.tier()).await.map_err(SaveError::Rpc)?;
        Ok(Saved {
            status: saved_text(),
            reply,
        })
    }

    /// *Remove limit* — send the absent policy, then re-read.
    pub async fn remove(&self, editor: &PolicyEditor) -> Result<Saved, SaveError<R::Error>> {
        self.author(editor.tier(), editor.feature(), None).await?;
        let reply = self.authored(editor.tier()).await.map_err(SaveError::Rpc)?;
        Ok(Saved {
            status: removed_text(),
            reply,
        })
    }
}

// ── Parsing and formatting (the one shared pair) ─────────────────────────

fn is_magnitude(slot: EditorSlot) -> bool {
    matches!(
        slot,
        EditorSlot::Window(QuotaDimension::Volume, _) | EditorSlot::PerOperation
    )
}

fn bounds_mut(policy: &mut FeaturePolicy, dimension: QuotaDimension) -> &mut WindowedBounds {
    match dimension {
        QuotaDimension::Operations => &mut policy.operations,
        QuotaDimension::Counterparties => &mut policy.counterparties,
        QuotaDimension::Volume => &mut policy.volume,
    }
}

fn slot_value(policy: &FeaturePolicy, slot: EditorSlot) -> Option<u64> {
    match slot {
        EditorSlot::Window(dimension, window) => policy.bounds(dimension).get(window),
        EditorSlot::PerOperation => policy.per_operation_max,
    }
}

fn slot_label(slot: EditorSlot, unit: MagnitudeUnit) -> LocalizedText {
    match slot {
        EditorSlot::Window(QuotaDimension::Volume, window) => LocalizedText::key_arg(
            match unit {
                MagnitudeUnit::Bytes => "features.editor_volume_label_bytes",
                MagnitudeUnit::Millisats => "features.editor_volume_label_sats",
            },
            "window",
            window_label(window).key,
        ),
        EditorSlot::Window(dimension, window) => LocalizedText::key_args(
            "features.quota_label",
            [
                ("dimension", dimension_label(dimension).key),
                ("window", window_label(window).key),
            ],
        ),
        EditorSlot::PerOperation => LocalizedText::key(match unit {
            MagnitudeUnit::Bytes => "features.editor_per_operation_bytes",
            MagnitudeUnit::Millisats => "features.editor_per_operation_sats",
        }),
    }
}

/// Per-tier keys for the same translator-agreement reason as `denied_by_*`.
fn no_effect_key(tier: RuleTier) -> &'static str {
    match tier {
        RuleTier::Structural => "features.no_effect_structural",
        RuleTier::Region => "features.no_effect_region",
        RuleTier::Admin => "features.no_effect_admin",
        RuleTier::Guardian => "features.no_effect_guardian",
        RuleTier::SelfImposed => "features.no_effect_self",
        RuleTier::Unknown => "features.no_effect_other",
    }
}

/// Millisats per sat — people type whole sats, the wire stores millisats.
use fauna_core::money::MSATS_PER_SAT;

/// The byte units, largest first, with the locale-invariant symbols
/// [`fauna_core::format::parse_byte_size`] reads back (1024-based).
const BYTE_UNITS: [(u64, &str); 4] = [
    (1 << 40, "TB"),
    (1 << 30, "GB"),
    (1 << 20, "MB"),
    (1 << 10, "KB"),
];

/// The text a stored value seeds its cell with — chosen so it parses back to
/// the **same** value, because a save is a whole-document replace and a seed
/// that drifted would change a bound the rule-setter never touched.
///
/// - counts: the number;
/// - bytes: the largest 1024-unit that divides the value exactly, else the bare
///   byte count (never the one-decimal display form, which is lossy);
/// - millisats: whole sats. A document written through this editor always
///   holds whole sats; one written another way that does not rounds DOWN,
///   which can only tighten.
pub fn input_text(slot: EditorSlot, unit: MagnitudeUnit, value: u64) -> String {
    if !is_magnitude(slot) {
        return value.to_string();
    }
    match unit {
        MagnitudeUnit::Bytes => BYTE_UNITS
            .iter()
            .find(|(size, _)| value >= *size && value.is_multiple_of(*size))
            .map(|(size, symbol)| format!("{} {symbol}", value / size))
            .unwrap_or_else(|| value.to_string()),
        MagnitudeUnit::Millisats => (value / MSATS_PER_SAT).to_string(),
    }
}

/// Parse one cell. `Ok(None)` is an empty cell — **no opinion**, never zero.
pub fn parse_input(
    slot: EditorSlot,
    unit: MagnitudeUnit,
    text: &str,
) -> Result<Option<u64>, LocalizedText> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if !is_magnitude(slot) {
        return whole_number(trimmed)
            .map(Some)
            .ok_or_else(|| invalid("features.editor_invalid_count", trimmed));
    }
    match unit {
        MagnitudeUnit::Bytes => fauna_core::format::parse_byte_size(trimmed)
            .map(Some)
            .ok_or_else(|| invalid("features.editor_invalid_size", trimmed)),
        MagnitudeUnit::Millisats => whole_number(trimmed)
            .and_then(|sats| sats.checked_mul(MSATS_PER_SAT))
            .map(Some)
            .ok_or_else(|| invalid("features.editor_invalid_sats", trimmed)),
    }
}

/// A non-negative whole number, tolerating the digit-group separators people
/// type that cannot be mistaken for a decimal point (spaces, underscores).
fn whole_number(text: &str) -> Option<u64> {
    let digits: String = text.chars().filter(|c| *c != ' ' && *c != '_').collect();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn invalid(key: &str, text: &str) -> LocalizedText {
    LocalizedText::key_arg(key, "value", text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::feature_gate::{EffectivePolicy, effective_policy};
    use fauna_protocol::discovery::capability;

    /// An authored item whose ceiling is what the nest would compose from
    /// `outer` — built with the core meet, as the nest's resolver does, because
    /// the editor must read the ceiling exactly as the nest sends it.
    fn item(
        feature: GatedFeature,
        policy: Option<FeaturePolicy>,
        outer: &[(RuleTier, FeaturePolicy)],
    ) -> AuthoredPolicyItem {
        AuthoredPolicyItem {
            feature,
            policy,
            unreadable: false,
            ceiling: effective_policy(feature, outer, &[]),
            extra: Default::default(),
        }
    }

    fn tier1(feature: GatedFeature) -> EffectivePolicy {
        effective_policy(feature, &[], &[])
    }

    fn lookup(key: &str) -> Option<&'static str> {
        fauna_i18n::strings::lookup(key)
    }

    fn ops_day_limit(limit: u64) -> FeaturePolicy {
        FeaturePolicy {
            availability: Availability::Limit,
            operations: WindowedBounds::at(Window::Day, limit),
            ..FeaturePolicy::NO_OPINION
        }
    }

    // ── The tier picks the kind ──────────────────────────────────────────

    #[test]
    fn the_tier_picks_the_kind_and_never_rides_the_wire() {
        assert_eq!(
            AuthoringTier::Admin.read_kind(),
            "fauna.features.policy.get"
        );
        assert_eq!(
            AuthoringTier::Admin.write_kind(),
            "fauna.features.policy.update"
        );
        assert_eq!(
            AuthoringTier::SelfImposed.read_kind(),
            "fauna.features.self_limits.get"
        );
        assert_eq!(
            AuthoringTier::SelfImposed.write_kind(),
            "fauna.features.self_limits.update"
        );
        for tier in [AuthoringTier::Admin, AuthoringTier::SelfImposed] {
            assert_eq!(AuthoringTier::from_key(tier.key()), Some(tier));
        }
        // The guardian tier mints no kind: it rides the family plane's two, and
        // a bare key names no ward, so it never opens an editor.
        let guardian = AuthoringTier::Guardian { ward: WARD };
        assert_eq!(guardian.read_kind(), "fauna.family.status");
        assert_eq!(guardian.write_kind(), "fauna.family.policy.update");
        assert_eq!(guardian.key(), "guardian");
        assert_eq!(AuthoringTier::from_key("guardian"), None);
    }

    // ── The guardian tier's seed and carriage ────────────────────────────

    const WARD: [u8; 32] = [7; 32];

    /// A ward entry as the nest answers it: `authored` is the
    /// guardian's sub-document, `outer` what tiers 1–3 hold for the ward.
    fn ward(
        authored: Option<fauna_protocol::features::GuardianFeaturePolicies>,
        outer: &[(RuleTier, FeaturePolicy)],
    ) -> FamilyWardInfo {
        FamilyWardInfo {
            actor_id: fauna_protocol::ByteBuf::from(WARD.to_vec()),
            handle: "kid".into(),
            policy: ReachPolicy {
                contact_approval: true,
                features: authored,
                ..Default::default()
            },
            features_ceiling: GatedFeature::ALL
                .iter()
                .map(|f| fauna_protocol::features::FeatureCeilingItem {
                    feature: *f,
                    ceiling: effective_policy(*f, outer, &[]),
                    extra: Default::default(),
                })
                .collect(),
            ..Default::default()
        }
    }

    fn documents(
        pairs: &[(GatedFeature, FeaturePolicy)],
    ) -> fauna_protocol::features::GuardianFeaturePolicies {
        pairs
            .iter()
            .map(|(f, p)| (f.as_str().to_string(), p.clone()))
            .collect()
    }

    const ALL_TOKENS: [&str; 2] = [capability::SUBSCRIPTIONS, capability::P2P_SHARE];

    fn all_capabilities() -> Vec<String> {
        ALL_TOKENS.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn a_ward_entry_maps_to_absent_authored_and_ceiling_by_member() {
        let admin = ops_day_limit(4);
        let entry = ward(
            Some(documents(&[(GatedFeature::P2pShare, ops_day_limit(2))])),
            &[(RuleTier::Admin, admin)],
        );
        let items = ward_authored_items(&entry);
        let keys: Vec<_> = items.iter().map(|i| i.feature).collect();
        assert_eq!(keys, GatedFeature::ALL, "registry order, every member");
        for item in &items {
            assert!(!item.unreadable);
            // The ceiling is the entry's own, zipped by member — never composed.
            assert_eq!(
                item.ceiling,
                effective_policy(item.feature, &[(RuleTier::Admin, ops_day_limit(4))], &[])
            );
            assert_eq!(
                item.policy,
                (item.feature == GatedFeature::P2pShare).then(|| ops_day_limit(2)),
                "{:?}: authored where the guardian spoke, absent elsewhere",
                item.feature
            );
        }
    }

    #[test]
    fn an_unreadable_ward_document_is_never_seeded_from_its_enforced_deny() {
        let entry = FamilyWardInfo {
            features_unreadable: true,
            // What the nest reports beside the flag: the deny it enforces.
            ..ward(
                Some(
                    GatedFeature::ALL
                        .iter()
                        .map(|f| (f.as_str().to_string(), FeaturePolicy::DENIED))
                        .collect(),
                ),
                &[],
            )
        };
        let items = ward_authored_items(&entry);
        for item in &items {
            assert!(item.unreadable);
            assert_eq!(item.policy, None, "{:?}", item.feature);
        }
        let surface = ward_surface(&entry, &all_capabilities());
        for row in surface.rows() {
            assert_eq!(row.summary.key, "features.authored_unreadable");
        }
        let editor = surface
            .editor(AuthoringTier::Guardian { ward: WARD }, "p2p-share")
            .expect("editor");
        assert!(editor.is_on(), "opens empty, not as the deny");
        assert!(editor.view().can_remove);

        // And a save starts the map over: the other members' enforced denies
        // must not be written back as authored ones.
        let sent = ward_policy_with(&entry, GatedFeature::P2pShare, Some(ops_day_limit(1)));
        assert_eq!(
            sent.features,
            Some(documents(&[(GatedFeature::P2pShare, ops_day_limit(1))]))
        );
        let cleared = ward_policy_with(&entry, GatedFeature::P2pShare, None);
        assert_eq!(cleared.features, Some(Default::default()));
    }

    #[test]
    fn a_guardian_write_replaces_one_member_and_keeps_the_rest_of_the_policy() {
        let mut authored = documents(&[
            (GatedFeature::Payments, FeaturePolicy::DENIED),
            (GatedFeature::P2pShare, ops_day_limit(2)),
        ]);
        // A newer nest's member this build cannot name rides along.
        authored.insert("holograms".into(), FeaturePolicy::DENIED);
        let entry = ward(Some(authored.clone()), &[]);

        let sent = ward_policy_with(&entry, GatedFeature::P2pShare, Some(ops_day_limit(9)));
        let mut expected = authored.clone();
        expected.insert("p2p-share".into(), ops_day_limit(9));
        assert_eq!(sent.features, Some(expected));
        assert_eq!(
            ReachPolicy {
                features: None,
                ..sent
            },
            ReachPolicy {
                features: None,
                ..entry.policy.clone()
            },
            "every other knob is the ward's own, verbatim"
        );

        // Removing the last member sends the EMPTY map — absent would mean
        // unchanged, and could never clear.
        let last = ward(
            Some(documents(&[(GatedFeature::Zaps, FeaturePolicy::DENIED)])),
            &[],
        );
        assert_eq!(
            ward_policy_with(&last, GatedFeature::Zaps, None).features,
            Some(Default::default())
        );
        // …and so does a first write over no document at all.
        assert_eq!(
            ward_policy_with(&ward(None, &[]), GatedFeature::Zaps, None).features,
            Some(Default::default())
        );
    }

    #[test]
    fn the_guardian_title_names_the_ward() {
        let surface = ward_surface(&ward(None, &[]), &all_capabilities());
        let editor = surface
            .editor(AuthoringTier::Guardian { ward: WARD }, "payments")
            .expect("editor");
        let view = editor.view();
        assert_eq!(view.tier, "guardian");
        let title = view.title.resolve_nested(lookup);
        assert!(title.ends_with("limits only for kid"), "{title:?}");
        assert!(!title.contains("features."), "{title:?}");
        // A re-seed after a save keeps naming them.
        let reply = FeaturePolicyReadReply {
            features: ward_authored_items(&ward(None, &[])),
            extra: Default::default(),
        };
        let again = editor.reseeded(&reply).expect("reseeded").view().title;
        assert_eq!(again, view.title);
    }

    #[test]
    fn a_guardian_save_rereads_writes_the_family_kind_and_rereads() {
        use fauna_client_testkit::{ScriptedRequester, block_on};
        use fauna_protocol::family::{FamilyOkReply, FamilyPolicyUpdateRequest, FamilyStatusReply};

        let encode = |v: &FamilyStatusReply| fauna_protocol::encode_canonical(v).unwrap().to_vec();
        let before = FamilyStatusReply {
            wards: vec![ward(
                Some(documents(&[(
                    GatedFeature::Payments,
                    FeaturePolicy::DENIED,
                )])),
                &[],
            )],
            ..Default::default()
        };
        let after = FamilyStatusReply {
            wards: vec![ward(
                Some(documents(&[
                    (GatedFeature::Payments, FeaturePolicy::DENIED),
                    (GatedFeature::P2pShare, ops_day_limit(3)),
                ])),
                &[],
            )],
            ..Default::default()
        };
        let ok = fauna_protocol::encode_canonical(&FamilyOkReply {
            ok: true,
            ..Default::default()
        })
        .unwrap()
        .to_vec();
        let requester = ScriptedRequester::new([encode(&before), ok, encode(&after)]);
        let client = FeaturesClient::new(&requester);
        let tier = AuthoringTier::Guardian { ward: WARD };
        let mut editor = ward_surface(&before.wards[0], &all_capabilities())
            .editor(tier, "p2p-share")
            .expect("editor");
        editor.set_cell(0, "3");

        let saved = block_on(client.save(&editor)).expect("save");
        assert_eq!(
            requester.kinds(),
            vec![
                "fauna.family.status",
                "fauna.family.policy.update",
                "fauna.family.status"
            ]
        );
        let sent: FamilyPolicyUpdateRequest =
            fauna_protocol::decode_strict(&requester.payloads()[1]).expect("decode");
        assert_eq!(sent.supervised_actor_id.as_slice(), WARD);
        assert!(
            sent.policy.contact_approval,
            "the reach knobs ride verbatim"
        );
        assert_eq!(sent.policy.features, after.wards[0].policy.features);
        let reseeded = editor.reseeded(&saved.reply).expect("reseeded");
        assert_eq!(reseeded.draft(), Ok(ops_day_limit(3)));

        // A ward the caller no longer guards: nothing is written.
        let gone = ScriptedRequester::new([encode(&FamilyStatusReply::default())]);
        let err = block_on(FeaturesClient::new(&gone).save(&editor)).unwrap_err();
        assert!(
            matches!(&err, SaveError::Invalid(t) if t.key == "features.editor_ward_missing"),
            "{err:?}"
        );
        assert_eq!(gone.kinds(), vec!["fauna.family.status"]);
    }

    // ── The summary ──────────────────────────────────────────────────────

    #[test]
    fn the_summary_names_each_authored_state_and_never_collapses_them() {
        let f = GatedFeature::P2pShare;
        let key = |i: &AuthoredPolicyItem| authored_summary(i).key;
        assert_eq!(key(&item(f, None, &[])), "features.authored_none");
        assert_eq!(
            key(&item(f, Some(FeaturePolicy::DENIED), &[])),
            "features.authored_off"
        );
        assert_eq!(
            key(&item(f, Some(FeaturePolicy::NO_OPINION), &[])),
            "features.authored_on"
        );
        assert_eq!(
            key(&item(f, Some(ops_day_limit(5)), &[])),
            "features.authored_limited_one"
        );
        let two = FeaturePolicy {
            per_operation_max: Some(1 << 30),
            ..ops_day_limit(5)
        };
        let s = authored_summary(&item(f, Some(two), &[]));
        assert_eq!(s.key, "features.authored_limited");
        assert_eq!(s.args.get("count").map(String::as_str), Some("2"));

        // Unreadable is enforced as a deny: it must never read "No limit set".
        let unreadable = AuthoredPolicyItem {
            unreadable: true,
            ..item(f, None, &[])
        };
        assert_eq!(key(&unreadable), "features.authored_unreadable");
        assert!(authored_row(&unreadable).has_document);
    }

    #[test]
    fn the_surface_hides_a_member_the_nest_does_not_carry() {
        let reply = FeaturePolicyReadReply {
            features: GatedFeature::ALL
                .iter()
                .map(|f| item(*f, None, &[]))
                .collect(),
            extra: Default::default(),
        };
        let bare = authored_surface(reply.clone(), &[]);
        let keys: Vec<_> = bare.rows().into_iter().map(|r| r.feature).collect();
        // zaps has no token, so nothing may hide it; the other two have one.
        assert_eq!(keys, vec!["zaps"]);

        let full = authored_surface(
            reply,
            &[
                capability::SUBSCRIPTIONS.to_string(),
                capability::P2P_SHARE.to_string(),
            ],
        );
        let keys: Vec<_> = full.rows().into_iter().map(|r| r.feature).collect();
        assert_eq!(keys, vec!["payments", "zaps", "p2p-share"]);
    }

    // ── What the editor expresses ────────────────────────────────────────

    #[test]
    fn only_declared_dimensions_are_slots_and_the_cap_comes_last() {
        for feature in GatedFeature::ALL {
            let e = entry(feature);
            let slots = editor_slots(feature);
            for slot in &slots {
                if let EditorSlot::Window(d, _) = slot {
                    assert!(e.declares(*d), "{feature:?} renders undeclared {d:?}");
                }
            }
            assert_eq!(
                slots.last() == Some(&EditorSlot::PerOperation),
                e.declares(QuotaDimension::Volume),
                "{feature:?}: the per-operation cap is last exactly where volume is declared"
            );
        }
    }

    #[test]
    fn on_with_no_bound_is_allow_with_bounds_is_limit_and_off_is_deny() {
        let f = GatedFeature::P2pShare;
        let mut editor = PolicyEditor::open(AuthoringTier::Admin, item(f, None, &[]));
        assert!(editor.is_on(), "no document opens On");
        assert_eq!(editor.draft(), Ok(FeaturePolicy::NO_OPINION));
        assert_eq!(editor.draft().unwrap().availability, Availability::Allow);

        editor.set_cell(0, "5");
        assert_eq!(editor.draft(), Ok(ops_day_limit(5)));

        // Zero is a bound, not "no opinion".
        editor.set_cell(0, "0");
        assert_eq!(editor.draft(), Ok(ops_day_limit(0)));

        // Off writes a bare deny, whatever the (unrendered) cells hold.
        editor.set_cell(0, "not a number");
        editor.set_on(false);
        assert_eq!(editor.draft(), Ok(FeaturePolicy::DENIED));
        assert!(editor.view().cells.is_empty(), "Off renders no cells");
    }

    #[test]
    fn the_editor_seeds_from_the_authored_document_and_round_trips_it() {
        let f = GatedFeature::P2pShare;
        let authored = FeaturePolicy {
            availability: Availability::Limit,
            operations: WindowedBounds::at(Window::Week, 40),
            volume: WindowedBounds::at(Window::Day, 50 << 30),
            per_operation_max: Some(3 << 20),
            ..FeaturePolicy::NO_OPINION
        };
        let editor = PolicyEditor::open(AuthoringTier::Admin, item(f, Some(authored.clone()), &[]));
        assert_eq!(
            editor.draft(),
            Ok(authored),
            "re-saving untouched must write back exactly what is stored"
        );
        let texts: Vec<_> = editor.view().cells.into_iter().map(|c| c.text).collect();
        assert!(texts.contains(&"50 GB".to_string()), "{texts:?}");
        assert!(texts.contains(&"3 MB".to_string()), "{texts:?}");
        assert!(editor.view().can_remove);

        let off = PolicyEditor::open(
            AuthoringTier::SelfImposed,
            item(f, Some(FeaturePolicy::DENIED), &[]),
        );
        assert!(!off.is_on(), "an authored deny opens Off");
    }

    #[test]
    fn an_unreadable_document_opens_empty_but_is_still_removable() {
        let f = GatedFeature::P2pShare;
        let editor = PolicyEditor::open(
            AuthoringTier::SelfImposed,
            AuthoredPolicyItem {
                unreadable: true,
                ..item(f, None, &[])
            },
        );
        assert!(editor.is_on());
        assert!(editor.view().cells.iter().all(|c| c.text.is_empty()));
        assert!(editor.view().can_remove);
    }

    #[test]
    fn bytes_parse_on_the_shared_scale_and_sats_are_whole() {
        let bytes = EditorSlot::Window(QuotaDimension::Volume, Window::Day);
        assert_eq!(
            parse_input(bytes, MagnitudeUnit::Bytes, "50 GB"),
            Ok(Some(50 << 30))
        );
        assert_eq!(
            parse_input(bytes, MagnitudeUnit::Millisats, "21 000"),
            Ok(Some(21_000_000))
        );
        assert_eq!(
            parse_input(bytes, MagnitudeUnit::Millisats, "1.5")
                .unwrap_err()
                .key,
            "features.editor_invalid_sats"
        );
        assert_eq!(
            parse_input(bytes, MagnitudeUnit::Bytes, "lots")
                .unwrap_err()
                .key,
            "features.editor_invalid_size"
        );
        let count = EditorSlot::Window(QuotaDimension::Operations, Window::Day);
        let err = parse_input(count, MagnitudeUnit::Bytes, "-3").unwrap_err();
        assert_eq!(err.key, "features.editor_invalid_count");
        assert_eq!(err.args.get("value").map(String::as_str), Some("-3"));
        assert_eq!(parse_input(count, MagnitudeUnit::Bytes, "  "), Ok(None));

        // The seed form always parses back to the same value.
        for v in [
            0,
            1,
            1000,
            1024,
            1536 << 20,
            (50 << 30) + 1,
            1_000_000_000_000,
        ] {
            assert_eq!(
                parse_input(
                    bytes,
                    MagnitudeUnit::Bytes,
                    &input_text(bytes, MagnitudeUnit::Bytes, v)
                ),
                Ok(Some(v)),
                "{v}"
            );
        }
    }

    // ── Tighten-only is rendered ─────────────────────────────────────────

    #[test]
    fn a_value_at_or_looser_than_the_ceiling_is_noted_never_refused() {
        let f = GatedFeature::P2pShare;
        let built_in = tier1(f)
            .operations
            .per_day
            .expect("tier 1 bounds ops/day")
            .limit;
        let mut editor = PolicyEditor::open(AuthoringTier::Admin, item(f, None, &[]));

        editor.set_cell(0, (built_in * 10).to_string());
        let cell = &editor.view().cells[0];
        let note = cell.note.as_ref().expect("a looser value owes the note");
        assert_eq!(note.key, "features.no_effect_structural");
        assert_eq!(
            editor_note_text(cell, lookup).as_deref(),
            Some(format!("No effect: Fauna's built-in limit is already {built_in}.").as_str())
        );
        assert!(editor.draft().is_ok(), "nothing refuses a looser value");

        // Equal is already bound too — the typed value changes nothing.
        editor.set_cell(0, built_in.to_string());
        assert!(editor.view().cells[0].note.is_some());

        editor.set_cell(0, (built_in - 1).to_string());
        assert!(
            editor.view().cells[0].note.is_none(),
            "a tighter value has an effect"
        );
    }

    #[test]
    fn the_note_names_the_outer_tier_that_binds_and_resolves_a_magnitude() {
        let f = GatedFeature::P2pShare;
        let admin = FeaturePolicy {
            availability: Availability::Limit,
            volume: WindowedBounds::at(Window::Day, 1 << 30),
            ..FeaturePolicy::NO_OPINION
        };
        let volume_day = editor_slots(f)
            .iter()
            .position(|s| *s == EditorSlot::Window(QuotaDimension::Volume, Window::Day))
            .unwrap();
        let mut editor = PolicyEditor::open(
            AuthoringTier::SelfImposed,
            item(f, None, &[(RuleTier::Admin, admin)]),
        );
        editor.set_cell(volume_day, "2 GB");
        let cell = &editor.view().cells[volume_day];
        assert_eq!(
            cell.note.as_ref().map(|n| n.key.as_str()),
            Some("features.no_effect_admin")
        );
        assert_eq!(
            editor_note_text(cell, lookup).as_deref(),
            Some("No effect: your nest admin already limits this to 1 GB.")
        );
    }

    #[test]
    fn an_outer_deny_notes_the_off_choice() {
        let f = GatedFeature::P2pShare;
        let editor = PolicyEditor::open(
            AuthoringTier::SelfImposed,
            item(f, None, &[(RuleTier::Admin, FeaturePolicy::DENIED)]),
        );
        assert_eq!(
            editor.view().off_note.map(|n| n.key).as_deref(),
            Some("features.denied_by_admin")
        );
        let free = PolicyEditor::open(AuthoringTier::Admin, item(f, None, &[]));
        assert_eq!(free.view().off_note, None);
    }

    #[test]
    fn remove_renders_only_while_a_document_exists() {
        let f = GatedFeature::Zaps;
        assert!(
            !PolicyEditor::open(AuthoringTier::Admin, item(f, None, &[]))
                .view()
                .can_remove
        );
        assert!(
            PolicyEditor::open(
                AuthoringTier::Admin,
                item(f, Some(FeaturePolicy::DENIED), &[])
            )
            .view()
            .can_remove
        );
    }

    // ── The strings ──────────────────────────────────────────────────────

    #[test]
    fn every_editor_key_resolves() {
        let mut keys = vec![
            "features.authored_none",
            "features.authored_off",
            "features.authored_on",
            "features.authored_limited_one",
            "features.authored_limited",
            "features.authored_unreadable",
            "features.editor_title_admin",
            "features.editor_title_self",
            "features.editor_title_guardian",
            "features.editor_ward_missing",
            "features.editor_hint",
            "features.editor_volume_label_bytes",
            "features.editor_volume_label_sats",
            "features.editor_per_operation_bytes",
            "features.editor_per_operation_sats",
            "features.editor_saved",
            "features.editor_removed",
            "features.editor_invalid_count",
            "features.editor_invalid_size",
            "features.editor_invalid_sats",
            "features.editor_save_failed",
        ];
        for tier in [
            RuleTier::Structural,
            RuleTier::Region,
            RuleTier::Admin,
            RuleTier::Guardian,
            RuleTier::SelfImposed,
        ] {
            keys.push(no_effect_key(tier));
        }
        for key in keys {
            assert!(
                lookup(key).is_some(),
                "{key} is not in i18n/strings/en.yaml"
            );
        }
    }

    /// Every painted label and title resolves with no raw key left in it — the
    /// nested `{feature}` / `{window}` substitutions are keys themselves.
    #[test]
    fn no_raw_key_reaches_a_painted_editor() {
        for feature in GatedFeature::ALL {
            for tier in [
                AuthoringTier::Admin,
                AuthoringTier::Guardian { ward: WARD },
                AuthoringTier::SelfImposed,
            ] {
                let view = PolicyEditor::open(tier, item(feature, None, &[])).view();
                let mut painted = vec![view.title.resolve_nested(lookup)];
                painted.extend(view.cells.iter().map(|c| c.label.resolve_nested(lookup)));
                for text in painted {
                    assert!(!text.contains("features."), "{feature:?}: {text:?}");
                }
            }
        }
    }

    // ── The wire ─────────────────────────────────────────────────────────

    #[test]
    fn save_parses_writes_the_tiers_kind_and_rereads() {
        use fauna_client_testkit::{ScriptedRequester, block_on};

        let update = fauna_protocol::encode_canonical(&FeaturePolicyUpdateReply {
            ok: true,
            extra: Default::default(),
        })
        .expect("encode")
        .to_vec();
        let reread = fauna_protocol::encode_canonical(&FeaturePolicyReadReply::default())
            .expect("encode")
            .to_vec();
        let requester = ScriptedRequester::new([update, reread]);
        let client = FeaturesClient::new(&requester);
        let mut editor = PolicyEditor::open(
            AuthoringTier::SelfImposed,
            item(GatedFeature::P2pShare, None, &[]),
        );

        // A refusal dispatches nothing.
        editor.set_cell(0, "many");
        assert!(matches!(
            block_on(client.save(&editor)),
            Err(SaveError::Invalid(_))
        ));
        assert!(requester.kinds().is_empty());

        editor.set_cell(0, "3");
        let saved = block_on(client.save(&editor)).expect("save");
        assert_eq!(saved.status.key, "features.editor_saved");
        assert_eq!(
            requester.kinds(),
            vec![
                "fauna.features.self_limits.update",
                "fauna.features.self_limits.get"
            ]
        );
        let sent: FeaturePolicyUpdateRequest =
            fauna_protocol::decode_strict(&requester.payloads()[0]).expect("decode");
        assert_eq!(sent.feature, GatedFeature::P2pShare);
        assert_eq!(sent.policy, Some(ops_day_limit(3)));
    }
}
