//! Scan obligation framework types and evaluation logic.
//!
//! Obligations declare what to scan, which classifiers to use,
//! and what actions to take. Evaluation logic is shared between
//! the nest (Zone 1) and client (Zone 2).

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::data::Timestamp;
use crate::identity::ActorId;
use crate::label::ContentLabel;
use crate::localized::LocalizedText;

/// Severity-ordered enforcement actions. Discriminant values match
/// severity: lower = more severe. Used for sort-by-severity.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ObligationAction {
    /// Most severe — refuse to accept, do not store content.
    Reject = 0,
    /// Store with restricted access (author + admin only).
    Quarantine = 1,
    /// Store but exclude from feeds and firehose.
    SuppressFromFeeds = 2,
    /// Slow down or cap delivery from the source.
    RateLimit = 3,
    /// Send callback to obligation publisher's endpoint.
    Notify = 4,
    /// Record in audit trail, no content action.
    Log = 5,
    /// Least severe — store and serve normally, attach labels.
    LabelOnly = 6,
    /// Legal-compulsion takedown (`moderation.md` § Categories & enforcement
    /// item 1 / `content-moderation-and-ranking.md` Q5): the content is
    /// **withheld from every viewer and tombstoned** ("removed under legal
    /// obligation [reference]"), never hard-deleted, appealable + audited. This
    /// is **admin-initiated under legal compulsion**, NOT produced by
    /// [`evaluate_obligations`] rule-matching — so its high discriminant (which
    /// severity-sorts it *last* in that ingest path) is inert: the takedown
    /// never flows through the rule-severity sort. It is appended at `7` purely
    /// to keep the `#[repr(u8)]` wire additive (`version-compatibility.md`).
    TakenDown = 7,
    /// Client render verb — render the item **collapsed** with a reveal
    /// affordance (`family-safety.md` § Content policy). Produced by the
    /// client-side render engine ([`render_verdict`]), never by the retired
    /// server ingest path; appended at `8` for `#[repr(u8)]` wire additivity.
    /// Its high discriminant makes the evaluator's ascending-severity sort treat
    /// it as *least* severe, which is why [`render_verdict`] applies its own
    /// render precedence (`Block` > `Collapse`) instead of reading the sort head.
    Collapse = 8,
    /// Client render verb — render the item **blocked**: no reveal, a
    /// policy-naming placeholder (mirrors the legal-takedown tombstone shape).
    /// Also the fail-closed verdict for a policy value a client cannot parse
    /// (`family-safety.md` § Content policy). Same ClientRender-only, additive-`9`
    /// rationale as [`Self::Collapse`].
    Block = 9,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EnforcementPoint {
    Ingest = 0,
    Store = 1,
    Index = 2,
    Firehose = 3,
    FeedQuery = 4,
    Serve = 5,
    BridgeOutbound = 6,
    /// The client's post-decrypt render path — where [`render_verdict`] composes
    /// the viewer's own thresholds with any guardian content floor
    /// (`family-safety.md` § Content policy). No server enforcement point; the
    /// nest never sees sealed content.
    ClientRender = 7,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[repr(u8)]
pub enum AttestationLevel {
    Any = 0,
    SignedMinimum = 1,
    TeeRequired = 2,
    ThresholdRequired = 3,
    ZkRequired = 4,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObligationRule {
    pub category: String,
    /// Minimum label confidence that triggers this rule, as **per-mille**
    /// (`0–1000` == probability `[0.0, 1.0]`). Integer for the dag-cbor float ban
    /// (`serialization.md`) — the same per-mille encoding the live spam/label path
    /// already uses (`scoring.rs`'s `LabelAbove { min_confidence_permille }`,
    /// `SpamPreferences.spam_threshold`). A [`ContentLabel::confidence`] (`f64` in
    /// `[0, 1]`, produced locally post-decrypt) triggers when it is `>=` this
    /// threshold's probability.
    pub min_confidence_permille: u16,
    pub action: ObligationAction,
    pub requires_attestation: AttestationLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObligationActionRecord {
    pub content_ref: crate::label::ContentRef,
    pub obligation_id: ActorId,
    pub rule_index: u32,
    pub label: ContentLabel,
    pub action_taken: ObligationAction,
    pub timestamp: Timestamp,
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

/// Evaluate obligation rules against labels for a piece of content.
///
/// Returns triggered (action, rule_index, label) triples sorted by severity
/// (Reject first, LabelOnly last). If multiple obligations produce
/// conflicting actions, the most severe wins.
pub fn evaluate_obligations(
    rules: &[ObligationRule],
    labels: &[ContentLabel],
    _enforcement_point: EnforcementPoint,
) -> Vec<(ObligationAction, usize, ContentLabel)> {
    let mut triggered: Vec<(ObligationAction, usize, ContentLabel)> = Vec::new();

    for (rule_idx, rule) in rules.iter().enumerate() {
        for label in labels {
            // The rule threshold is per-mille (wire-safe integer); the label
            // confidence is a locally-produced `f64` in `[0, 1]`. Compare in
            // probability space — divide the threshold rather than scaling the
            // label, so a `1000`-per-mille rule needs exactly `1.0`.
            if label.category == rule.category
                && label.confidence >= f64::from(rule.min_confidence_permille) / 1000.0
            {
                triggered.push((rule.action, rule_idx, label.clone()));
                break;
            }
        }
    }

    triggered.sort_by_key(|(action, _, _)| *action as u8);
    triggered
}

/// Map an [`ObligationAction`] discriminant (the `action: u8` a
/// `fauna.moderation.actions` reply row carries — the wire encoding of this
/// enum) to its human label, so a moderation-queue row shows "Quarantined" /
/// "Labeled" instead of a raw `u8`. The single shared-Rust source of the
/// action-label map, co-located with the enum it describes and mirroring the
/// category-label home [`crate::content_category::content_label_style`]; the
/// UniFFI (`fauna-ffi` `obligationActionLabel`) and wasm
/// (`fauna-wasm` `obligationActionLabel`) faces wrap this one definition so no
/// client re-derives the map (moderation.md § Where logic lives, priority #2/#4).
///
/// The discriminants are severity-ordered (lower = more severe):
/// `0 Reject` / `1 Quarantine` / `2 SuppressFromFeeds` / `3 RateLimit` /
/// `4 Notify` / `5 Log` / `6 LabelOnly`. `4 Notify` (a publisher callback with
/// no user-visible content change) and any unrecognized discriminant fall back
/// to the neutral "Flagged" label rather than panicking, keeping the queue
/// forward-compatible if a new action lands.
pub fn obligation_action_label(action: u8) -> LocalizedText {
    let key = match action {
        0 => "moderation.action.rejected",
        1 => "moderation.action.quarantined",
        2 => "moderation.action.suppressed",
        3 => "moderation.action.rate_limited",
        5 => "moderation.action.logged",
        6 => "moderation.action.labeled",
        7 => "moderation.action.taken_down",
        // 4 (Notify) and any unknown discriminant render as the neutral
        // "Flagged" — both are "the system acted, no visible content change".
        _ => "moderation.action.flagged",
    };
    LocalizedText::key(key)
}

/// The visible **legal-takedown tombstone** a client renders in place of a
/// post's body once it has been taken down under legal compulsion
/// (`moderation.md` § Categories & enforcement item 1 — "a visible tombstone
/// ('removed under legal obligation [reference]')"). Keyed on
/// `moderation.legal_takedown.tombstone` with a single `{reference}`
/// substitution carrying the legal-obligation reference the authority supplied.
/// The single shared-Rust source of the tombstone text — the UniFFI
/// (`fauna-ffi` `legalTakedownTombstone`) and wasm (`fauna-wasm`
/// `legalTakedownTombstone`) faces wrap this one definition so no client
/// hand-rolls the string (moderation.md § Where logic lives, priority #2/#4;
/// mirrors [`obligation_action_label`] / [`crate::content_category::content_label_style`]).
pub fn legal_takedown_tombstone(reference: &str) -> LocalizedText {
    LocalizedText::key_arg(
        "moderation.legal_takedown.tombstone",
        "reference",
        reference,
    )
}

// ----------------------------------------------------------------------------
// Client render-enforcement engine (family-safety.md § Content policy)
//
// The general "collapse/hide flagged content in your own view" stage
// moderation.md § Categories & enforcement item 1 has always promised but the
// code never built. Every user gets it (their own spam/phishing thresholds →
// collapse); for a supervised account the guardian's per-category floor composes
// in, strictest-wins. Pure, WASM-safe, no wire of its own — rules are assembled
// from data that already crosses the wire (SpamPreferences, the guardian policy
// sub-doc) and evaluated against locally-produced post-decrypt labels.
// ----------------------------------------------------------------------------

/// The single hard-coded confidence threshold (per-mille) at which a **guardian
/// content floor** triggers. `family-safety.md` § Content policy deliberately
/// gives the guardian floor no per-category confidence knob in v1.x — the floor
/// is a coarse `inherit | collapse | block`, and this constant is where "how
/// confident before the floor bites" lives (no-operator bucket 1; a per-mille
/// knob can join additively if real households need it). `500` per-mille (0.5
/// probability) matches the built-in label filter's long-standing default.
pub const GUARDIAN_FLOOR_TRIGGER_PERMILLE: u16 = 500;

/// The four canonical **negative** categories a guardian content floor ranges
/// over (`family-safety.md` § Content policy). `trusted` is a positive signal and
/// takes no floor, so it is absent here.
pub const GUARDIAN_FLOOR_CATEGORIES: [&str; 4] = ["nsfw", "spam", "phishing", "commercial"];

/// The minimum interval between **Guardian Notify** reports (`family-safety.md`
/// § Guardian Notify — *"batched (at most hourly)"*). A supervised ward's client
/// accumulates its per-category guardian-floor enforcement deltas and flushes them
/// via `fauna.family.notify_report` no more than once per this window. The single
/// shared constant so every app batches identically (priority #2) — one hour.
pub const NOTIFY_REPORT_MIN_INTERVAL_SECS: i64 = 3600;

/// A guardian's per-category render floor (`family-safety.md` § Content policy):
/// `inherit` lets the ward's own preferences decide, `collapse` renders the item
/// collapsed with a reveal, `block` renders it blocked with no reveal. A value a
/// client cannot parse deserializes to [`Self::Unknown`] and renders **fail-closed**
/// (`block`) — the same rule as the reach knobs (`family-safety.md` § Content policy).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ContentFloor {
    /// Default — the ward's own [`ContentLabel`]-derived preferences decide.
    #[default]
    Inherit,
    /// Render the item collapsed, with a reveal affordance.
    Collapse,
    /// Render the item blocked: no reveal, a policy-naming placeholder.
    Block,
    /// A floor value this client's build does not recognize (a newer nest sent a
    /// value added after this client shipped). Renders fail-closed as `block`
    /// (see [`rules_from_guardian_policy`]). Deserialize-only catch-all; the write
    /// path (Slice B) validates closed values, so this is never serialized back
    /// — and `skip_serializing` makes a path that tried fail loudly rather than
    /// write `"unknown"` over a newer value (`transport.md` § Rule 3 in full,
    /// *Open, collapsing*).
    #[serde(other, skip_serializing)]
    Unknown,
}

/// A guardian's content policy — a per-category floor over the four negative
/// canonical categories (`family-safety.md` § Content policy). Absent categories
/// default to [`ContentFloor::Inherit`], so a partial document is valid and means
/// "floor only the categories named, inherit the rest".
impl ContentFloor {
    /// Parse the stored/wire value. Unrecognized values **fail closed to
    /// [`Self::Unknown`]**, which every render + label path treats as `block`
    /// (`family-safety.md` § Content policy — *"A rule value a client cannot parse
    /// renders fail-closed (`block`)"*). A newer nest could store a value this
    /// binary does not know (evolution is additive-everywhere,
    /// `version-compatibility.md`); blocking is the strict option that voids no
    /// guardian decision. Mirrors [`crate::data::UnknownSenderMail::from_wire`].
    pub fn from_wire(value: &str) -> Self {
        match value {
            "inherit" => Self::Inherit,
            "collapse" => Self::Collapse,
            "block" => Self::Block,
            _ => Self::Unknown,
        }
    }

    /// The canonical wire/DB string the policy editor's select writes back to
    /// `guardian_policies.content_*`. [`Self::Unknown`] is a deserialize-only
    /// catch-all the write path never produces; it renders as `block` here so a
    /// defensive round-trip stays fail-closed.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Collapse => "collapse",
            Self::Block | Self::Unknown => "block",
        }
    }

    /// The ratified picker order (`family-safety.md` § Content policy — *"inherit |
    /// collapse | block"*), default first. [`Self::Unknown`] is deserialize-only
    /// and never a picker option.
    pub const ORDER: [Self; 3] = [Self::Inherit, Self::Collapse, Self::Block];

    /// The option an unrepresentable selection degrades to — the strict `block`,
    /// not the permissive `inherit`. Named rather than left as an index so a
    /// picker's out-of-range fallback states the safety rule (mirrors
    /// [`crate::data::UnknownSenderMail::FAIL_CLOSED`]).
    pub const FAIL_CLOSED: Self = Self::Block;
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentPolicy {
    #[serde(default)]
    pub nsfw: ContentFloor,
    #[serde(default)]
    pub spam: ContentFloor,
    #[serde(default)]
    pub phishing: ContentFloor,
    #[serde(default)]
    pub commercial: ContentFloor,
}

impl ContentPolicy {
    /// The floor for a given category name, or [`ContentFloor::Inherit`] for any
    /// category outside the four negative canonicals.
    pub fn floor_for(&self, category: &str) -> ContentFloor {
        match category {
            "nsfw" => self.nsfw,
            "spam" => self.spam,
            "phishing" => self.phishing,
            "commercial" => self.commercial,
            _ => ContentFloor::Inherit,
        }
    }

    /// Compose two policies **strictest-wins**, per category: `block` (and the
    /// fail-closed [`ContentFloor::Unknown`], which renders as `block`) over
    /// `collapse` over `inherit`. Per-category strictest-wins over floors is the
    /// same answer [`render_verdict_composed`] gives over their rules, because
    /// every guardian-floor rule triggers at the one
    /// [`GUARDIAN_FLOOR_TRIGGER_PERMILLE`].
    pub fn strictest_with(&self, other: &ContentPolicy) -> ContentPolicy {
        fn rank(floor: ContentFloor) -> u8 {
            match floor {
                ContentFloor::Inherit => 0,
                ContentFloor::Collapse => 1,
                ContentFloor::Block | ContentFloor::Unknown => 2,
            }
        }
        fn stricter(a: ContentFloor, b: ContentFloor) -> ContentFloor {
            if rank(b) > rank(a) { b } else { a }
        }
        ContentPolicy {
            nsfw: stricter(self.nsfw, other.nsfw),
            spam: stricter(self.spam, other.spam),
            phishing: stricter(self.phishing, other.phishing),
            commercial: stricter(self.commercial, other.commercial),
        }
    }
}

/// The **kids-app content floor** (`family-safety.md` § The account age band →
/// the kids-app bullet, item (4)): every one of the four
/// [`GUARDIAN_FLOOR_CATEGORIES`] floored at [`ContentFloor::Block`]. Compiled
/// into the `kids` build flavor only — `fauna-ffi`'s `kids-floor` feature
/// composes it through [`with_compiled_floor`]; every other build carries it as
/// inert data.
///
/// It floors the render verdict and nothing else: `content_notify` is
/// deliberately NOT floored (a floor that reported would be a disclosure the
/// guardian did not opt into), so Guardian Notify counting
/// ([`guardian_enforced_categories`]) keeps reading the guardian's own policy.
pub const KIDS_CONTENT_FLOOR: ContentPolicy = ContentPolicy {
    nsfw: ContentFloor::Block,
    spam: ContentFloor::Block,
    phishing: ContentFloor::Block,
    commercial: ContentFloor::Block,
};

/// The guardian policy a render composes once a build's **compiled-in floor**
/// is applied (`dynamic-features.md` § Compile-time excision, the kids floor as
/// its second tier-0 application): the account's guardian policy and the
/// compiled floor, strictest-wins per category ([`ContentPolicy::strictest_with`]).
///
/// The floor rides the guardian input of [`render_verdict_composed`] rather
/// than adding a source of its own, so the render engine stays one engine and
/// a floored item is attributed [`VerdictSource::GuardianFloor`] — the family
/// policy placeholder, which is what a kids-flavor account (supervised by
/// construction) sees. `compiled = None` (every non-kids build) returns the
/// guardian policy unchanged; an unsupervised viewer in a floored build gets
/// the floor alone — defense in depth on the device, never a substitute for
/// the account's policy.
pub fn with_compiled_floor(
    guardian: Option<&ContentPolicy>,
    compiled: Option<&ContentPolicy>,
) -> Option<ContentPolicy> {
    match (guardian, compiled) {
        (Some(g), Some(c)) => Some(g.strictest_with(c)),
        (Some(p), None) | (None, Some(p)) => Some(*p),
        (None, None) => None,
    }
}

/// What a render path should do with an item, after composing the viewer's own
/// preferences with any guardian floor (`family-safety.md` § Content policy). The
/// output of [`render_verdict`]; ordered least→most restrictive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderVerdict {
    /// No label present — render normally.
    Show,
    /// A label is present but nothing escalated it — render normally with a
    /// label badge (moderation.md's existing "labels produce a badge" behavior).
    Badge,
    /// Render collapsed, with a reveal affordance.
    Collapse,
    /// Render blocked, no reveal, a policy-naming placeholder.
    Block,
}

impl RenderVerdict {
    /// The canonical wire string the client bindings hand back — the web
    /// `contentRenderVerdict` and the native `content_render_verdict` both return
    /// this vocabulary, so it lives here rather than being spelled out once per
    /// binding (mirrors [`ContentFloor::as_str`]). A client branches on these four
    /// values to pick its render treatment.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Show => "show",
            Self::Badge => "badge",
            Self::Collapse => "collapse",
            Self::Block => "block",
        }
    }
}

/// Build the render rules implied by a viewer's **own** spam/phishing preferences
/// (`family-safety.md` § Content policy — "their thresholds → `Collapse`"). A
/// user's own threshold collapses (never blocks — `block` is a guardian-only
/// floor); the threshold value is the user's slider, per-mille. This is the
/// general, every-user half of the engine that un-darks moderation.md § item 1's
/// long-unbuilt "optionally collapse flagged content in their own view".
pub fn rules_from_preferences(
    spam_threshold_permille: u16,
    phishing_threshold_permille: u16,
) -> Vec<ObligationRule> {
    vec![
        ObligationRule {
            category: "spam".into(),
            min_confidence_permille: spam_threshold_permille,
            action: ObligationAction::Collapse,
            requires_attestation: AttestationLevel::Any,
        },
        ObligationRule {
            category: "phishing".into(),
            min_confidence_permille: phishing_threshold_permille,
            action: ObligationAction::Collapse,
            requires_attestation: AttestationLevel::Any,
        },
    ]
}

/// Build the render rules implied by a **guardian** content policy
/// (`family-safety.md` § Content policy). Each category floor maps: `inherit` →
/// no rule (the ward's own preferences decide), `collapse` → a `Collapse` rule,
/// `block` (and the fail-closed [`ContentFloor::Unknown`]) → a `Block` rule, each
/// keyed at the hard-coded [`GUARDIAN_FLOOR_TRIGGER_PERMILLE`].
pub fn rules_from_guardian_policy(policy: &ContentPolicy) -> Vec<ObligationRule> {
    let mut rules = Vec::new();
    for category in GUARDIAN_FLOOR_CATEGORIES {
        let action = match policy.floor_for(category) {
            // The ward's own preferences decide — the guardian adds no rule.
            ContentFloor::Inherit => continue,
            ContentFloor::Collapse => ObligationAction::Collapse,
            // `block` and the fail-closed `Unknown` (a value this client cannot
            // parse) both render blocked (family-safety.md § Content policy).
            ContentFloor::Block | ContentFloor::Unknown => ObligationAction::Block,
        };
        rules.push(ObligationRule {
            category: category.to_string(),
            min_confidence_permille: GUARDIAN_FLOOR_TRIGGER_PERMILLE,
            action,
            requires_attestation: AttestationLevel::Any,
        });
    }
    rules
}

/// Compose `rules` (typically [`rules_from_preferences`] ∪
/// [`rules_from_guardian_policy`]) against a piece of content's `labels` and
/// return the **strictest** render verdict (`family-safety.md` § Content policy).
///
/// Precedence is `Block > Collapse > Badge > Show`: any triggered `Block` rule
/// wins, else any triggered `Collapse` rule, else a present-but-unescalated label
/// badges, else the item shows. The precedence is applied explicitly rather than
/// read off [`evaluate_obligations`]'s severity sort, because the render verbs sit
/// at the high (`8`/`9`) discriminants for wire additivity and would sort as
/// *least* severe there — see [`ObligationAction::Collapse`].
pub fn render_verdict(labels: &[ContentLabel], rules: &[ObligationRule]) -> RenderVerdict {
    let triggered = evaluate_obligations(rules, labels, EnforcementPoint::ClientRender);
    // Apply the render precedence explicitly (Block > Collapse). The render verbs
    // sit at the high `8`/`9` discriminants for wire additivity, so the
    // evaluator's ascending-severity sort would rank them backwards — see
    // `ObligationAction::Collapse`.
    if triggered
        .iter()
        .any(|(a, _, _)| *a == ObligationAction::Block)
    {
        RenderVerdict::Block
    } else if triggered
        .iter()
        .any(|(a, _, _)| *a == ObligationAction::Collapse)
    {
        RenderVerdict::Collapse
    } else if labels.is_empty() {
        RenderVerdict::Show
    } else {
        // A label is present but nothing escalated it — the moderation.md
        // "labels produce a badge" behavior is the floor.
        RenderVerdict::Badge
    }
}

/// The **client render path's** verdict entry point — the twin of [`render_verdict`]
/// that keys on the lightweight [`ContentLabelEntry`] (`{category,
/// confidence_per_mille}`) every app already holds per post/message
/// ([`crate::content_category::ContentLabelEntry`], embedded on `FeedPostItem.labels`
/// / `MessageSnapshot.labels`), rather than the heavyweight signed
/// [`crate::label::ContentLabel`] the retired server-ingest path used.
///
/// Integer-only comparison — the entry's `confidence_per_mille` is already the same
/// per-mille encoding as [`ObligationRule::min_confidence_permille`], so there is no
/// float round-trip (and no dag-cbor float concern). Same strictest-wins precedence
/// as [`render_verdict`] (`Block > Collapse > Badge > Show`), applied explicitly
/// because the render verbs sit at the high `8`/`9` discriminants for wire additivity
/// (see [`ObligationAction::Collapse`]).
pub fn render_verdict_entries(
    labels: &[crate::content_category::ContentLabelEntry],
    rules: &[ObligationRule],
) -> RenderVerdict {
    let mut has_block = false;
    let mut has_collapse = false;
    for rule in rules {
        for label in labels {
            if label.category == rule.category
                && label.confidence_per_mille >= rule.min_confidence_permille
            {
                match rule.action {
                    ObligationAction::Block => has_block = true,
                    ObligationAction::Collapse => has_collapse = true,
                    // Only the two client render verbs are meaningful here; any
                    // other action a rule assembler produced is ignored at render.
                    _ => {}
                }
                break;
            }
        }
    }
    if has_block {
        RenderVerdict::Block
    } else if has_collapse {
        RenderVerdict::Collapse
    } else if labels.is_empty() {
        RenderVerdict::Show
    } else {
        RenderVerdict::Badge
    }
}

/// A viewer's **own** spam/phishing thresholds, as
/// [`render_verdict_composed`] takes them.
///
/// The two arrive together or not at all — they are one
/// `fauna.spam.get_preferences` read — so they travel as one struct rather than
/// two `Option<u16>`s a caller could half-fill. That shape is deliberate: when
/// each binding gated its own assembly on `if let (Some(spam), Some(phishing))`,
/// a half-known pair silently composed **no** own-threshold rule at all, which
/// fails open. Here the state is unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewerThresholds {
    /// The viewer's own `spam_threshold` slider, per-mille.
    pub spam_permille: u16,
    /// The viewer's own `phishing_threshold` slider, per-mille.
    pub phishing_permille: u16,
}

/// **The** client render verdict for one piece of content — the whole
/// composition, in one shared call (`family-safety.md` § Content policy).
///
/// Composes, **strictest-wins** (`Block > Collapse > Badge > Show`):
///
/// 1. the viewer's OWN spam/phishing thresholds → `Collapse`
///    ([`rules_from_preferences`]) — every user, the un-darking of
///    `moderation.md` § Categories & enforcement item 1; and
/// 2. when supervised, the guardian's per-category floor
///    ([`rules_from_guardian_policy`]) — `collapse`/`block` on top, with an
///    unparseable floor staying fail-closed to `block`.
///
/// Every app render surface routes through here: linux calls it directly,
/// web via the wasm `contentRenderVerdict`, and apple/android/windows/tui via the
/// UniFFI `content_render_verdict`. The assembly — which rules exist, in what
/// order, gated on what — is the load-bearing part of the design and is spec'd
/// once for every app, so it is implemented once here rather than per binding
/// (priority #2/#4). Callers pass only ingredients they already hold: the
/// item's reduced label list, the guardian policy straight off `family.status`
/// (`None` for an unsupervised viewer), and the viewer's own thresholds (`None`
/// until `fauna.spam.get_preferences` has landed).
pub fn render_verdict_composed(
    labels: &[crate::content_category::ContentLabelEntry],
    guardian_policy: Option<&ContentPolicy>,
    own_thresholds: Option<ViewerThresholds>,
    region_policies: &[crate::region_policy::RegionRuleSet],
) -> ComposedVerdict {
    // Each source is folded SEPARATELY rather than into one flattened rule
    // vector, because the verdict now has to say which source produced it and a
    // flattened vector has forgotten by the time it answers. The precedence is
    // unchanged: strictest wins across the sources exactly as it used to win
    // across the concatenated rules.
    let own_action = own_thresholds.and_then(|own| {
        strongest_render_action(
            labels,
            &rules_from_preferences(own.spam_permille, own.phishing_permille),
        )
    });
    let guardian_action = guardian_policy
        .and_then(|policy| strongest_render_action(labels, &rules_from_guardian_policy(policy)));
    // Most specific first, as `RegionRegistry::chain` returns them.
    type RegionHit<'a> = (
        ObligationAction,
        &'a crate::region_policy::RegionRuleSet,
        &'a crate::region_policy::RegionRule,
    );
    let region_hits: Vec<RegionHit<'_>> = region_policies
        .iter()
        .filter_map(|set| {
            strongest_region_rule(labels, set).map(|(action, rule)| (action, set, rule))
        })
        .collect();

    let strictest = [
        own_action,
        guardian_action,
        region_hits.iter().map(|(a, _, _)| *a).max_by_key(severity),
    ]
    .into_iter()
    .flatten()
    .max_by_key(severity);

    let Some(winning) = strictest else {
        return ComposedVerdict {
            verdict: if labels.is_empty() {
                RenderVerdict::Show
            } else {
                RenderVerdict::Badge
            },
            source: None,
        };
    };

    // Attribution among sources that tie at the winning strictness. A region
    // takes it first: a region block carries an authority's own reason the user
    // is entitled to read verbatim (region-blocking.md invariant 1), where the
    // family placeholder carries no such external obligation and the viewer's
    // own threshold is the viewer's own choice. Within the regions, the most
    // specific on the chain — the authority closest to the user.
    let source = if let Some((_, set, rule)) = region_hits.iter().find(|(a, _, _)| *a == winning) {
        Some(VerdictSource::Region(RegionAttribution {
            region: set.region.clone(),
            authority_name: set.authority_name.clone(),
            reason_code: rule.reason_code.clone(),
            reason: rule.reason.clone(),
        }))
    } else if guardian_action == Some(winning) {
        Some(VerdictSource::GuardianFloor)
    } else {
        Some(VerdictSource::OwnThresholds)
    };

    ComposedVerdict {
        verdict: match winning {
            ObligationAction::Block => RenderVerdict::Block,
            _ => RenderVerdict::Collapse,
        },
        source,
    }
}

/// Render severity, for `max_by_key`: `Block` outranks `Collapse`, and nothing
/// else is a render verb.
fn severity(action: &ObligationAction) -> u8 {
    match action {
        ObligationAction::Block => 2,
        ObligationAction::Collapse => 1,
        _ => 0,
    }
}

/// The strictest render verb `rules` produce over `labels`, or `None` when none
/// of them fires.
///
/// The **same predicate** [`render_verdict_entries`] uses (a label in the rule's
/// category at or above its per-mille trigger), so no source can fire here and
/// not there.
fn strongest_render_action(
    labels: &[crate::content_category::ContentLabelEntry],
    rules: &[ObligationRule],
) -> Option<ObligationAction> {
    rules
        .iter()
        .filter(|rule| {
            labels.iter().any(|label| {
                label.category == rule.category
                    && label.confidence_per_mille >= rule.min_confidence_permille
            })
        })
        .map(|rule| rule.action)
        .filter(|action| severity(action) > 0)
        .max_by_key(severity)
}

/// The strictest firing rule in one region's set, with the rule itself so its
/// reason can be attributed.
fn strongest_region_rule<'a>(
    labels: &[crate::content_category::ContentLabelEntry],
    set: &'a crate::region_policy::RegionRuleSet,
) -> Option<(ObligationAction, &'a crate::region_policy::RegionRule)> {
    set.rules
        .iter()
        .filter(|entry| {
            labels.iter().any(|label| {
                label.category == entry.rule.category
                    && label.confidence_per_mille >= entry.rule.min_confidence_permille
            })
        })
        .filter(|entry| severity(&entry.rule.action) > 0)
        .max_by_key(|entry| severity(&entry.rule.action))
        .map(|entry| (entry.rule.action, entry))
}

/// Which of the composed sources produced the verdict, with everything the
/// placeholder needs to name it.
///
/// The point of carrying this is that a blocked item must say *whose* rule
/// blocked it: the family plane's placeholder names the family policy, and a
/// region's must name the region and its authority rather than a generic
/// "policy" (`region-blocking.md` § Where it composes — the render seam).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerdictSource {
    /// The viewer's own spam/phishing thresholds
    /// (`moderation.md` § Categories & enforcement item 1).
    OwnThresholds,
    /// The guardian's per-category floor
    /// (`family-client-enforcement.md` § Content policy).
    GuardianFloor,
    /// A region's published content policy
    /// (`region-blocking.md` § The content plane).
    Region(RegionAttribution),
    /// The viewer reported this item, or its author, and so hid it for
    /// themselves (`moderation.md` § Corollary — block also hides): painted as
    /// the "you reported this" placeholder
    /// (`moderation.report.hidden_placeholder`).
    Reported,
}

/// What a region-sourced verdict shows the user.
///
/// `reason` is the authority's own text, shown **verbatim** under the app's own
/// frame — the frame is the app's i18n, the reason is the authority's, and an
/// app never paraphrases an authority (invariant 1). Use
/// [`crate::region_policy::RegionRule::reason_text`]'s fallback rule to pick the
/// entry for the viewer's language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionAttribution {
    pub region: crate::region_authority::RegionCode,
    /// As the registry names the administering authority — never as the
    /// artifact names itself.
    pub authority_name: String,
    pub reason_code: String,
    pub reason: std::collections::BTreeMap<String, String>,
}

/// A render verdict together with the source that drove it.
///
/// `source` is `None` exactly when no rule fired — a [`RenderVerdict::Show`] or
/// [`RenderVerdict::Badge`] has nothing to attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposedVerdict {
    pub verdict: RenderVerdict,
    pub source: Option<VerdictSource>,
}

impl ComposedVerdict {
    /// The region attribution, when a region rule drove this verdict.
    pub fn region(&self) -> Option<&RegionAttribution> {
        match &self.source {
            Some(VerdictSource::Region(a)) => Some(a),
            _ => None,
        }
    }

    /// Whether the viewer's own report hid this item.
    pub fn reported(&self) -> bool {
        self.source == Some(VerdictSource::Reported)
    }
}

/// Did the viewer report this item — its own id, or its author for an account
/// report? `hidden_content` is the `fauna.state.moderation` hidden-content list, stored
/// lowercase; the ids compare case-insensitively.
pub fn reported_by_viewer(
    hidden_content: &[String],
    item_id: &str,
    author_id: Option<&str>,
) -> bool {
    hidden_content.iter().any(|hidden| {
        hidden.eq_ignore_ascii_case(item_id)
            || author_id.is_some_and(|author| hidden.eq_ignore_ascii_case(author))
    })
}

/// **The** client render verdict for one identified item — [`render_verdict_composed`]
/// plus the viewer's own reports (`moderation.md` § Corollary — block also
/// hides): an item the viewer reported, or whose author they reported, is a
/// `Block` attributed to [`VerdictSource::Reported`], which paints the "you
/// reported this" placeholder. A reporter who did not block still stops seeing
/// what they reported, and nothing changes for anyone else.
///
/// A region's own `Block` keeps its attribution: the authority's reason is the
/// user's to read verbatim (`region-blocking.md` invariant 1), and a
/// placeholder of the viewer's own adds nothing it lacks.
pub fn render_verdict_for_item(
    hidden_content: &[String],
    item_id: &str,
    author_id: Option<&str>,
    labels: &[crate::content_category::ContentLabelEntry],
    guardian_policy: Option<&ContentPolicy>,
    own_thresholds: Option<ViewerThresholds>,
    region_policies: &[crate::region_policy::RegionRuleSet],
) -> ComposedVerdict {
    let composed =
        render_verdict_composed(labels, guardian_policy, own_thresholds, region_policies);
    if composed.verdict == RenderVerdict::Block && composed.region().is_some() {
        return composed;
    }
    if reported_by_viewer(hidden_content, item_id, author_id) {
        return ComposedVerdict {
            verdict: RenderVerdict::Block,
            source: Some(VerdictSource::Reported),
        };
    }
    composed
}

/// The **guardian-floor** categories a piece of content triggers, for Guardian
/// Notify counting (`family-safety.md` § Guardian Notify). Given the item's
/// post-decrypt [`ContentLabelEntry`](crate::content_category::ContentLabelEntry)
/// list and the guardian's [`ContentPolicy`], returns each of the four negative
/// canonical categories whose guardian floor (`collapse`, `block`, and the
/// fail-closed `Unknown`) **bites** on this item — the categories for which "the
/// guardian's policy is acting" (§ Guardian Notify reports *category + count,
/// never content*).
///
/// This is the single shared decision of *what counts as a guardian enforcement
/// event*, so no client drifts on how Notify counts (priority #2/#4). Two
/// properties bind it to the render path so counting and rendering can never
/// diverge:
/// - it uses the **same predicate** as [`render_verdict_entries`] (a label in the
///   category at or above [`GUARDIAN_FLOOR_TRIGGER_PERMILLE`]);
/// - it counts **only the guardian floor**, never the ward's own-threshold
///   collapses — Notify is a lens on the *guardian's* policy, not the ward's own
///   choices (§ Guardian Notify). Pass the guardian [`ContentPolicy`] alone (not a
///   composed rule set): an `inherit` floor contributes nothing here even when the
///   ward's own preference would collapse the same item.
///
/// The returned slice order follows [`GUARDIAN_FLOOR_CATEGORIES`] (deterministic).
pub fn guardian_enforced_categories(
    labels: &[crate::content_category::ContentLabelEntry],
    policy: &ContentPolicy,
) -> Vec<&'static str> {
    let mut out = Vec::new();
    for category in GUARDIAN_FLOOR_CATEGORIES {
        // Only a collapse/block/unknown floor is the guardian "acting"; an
        // `inherit` floor adds no guardian rule (the ward's own preferences decide
        // — not Notify's subject).
        let enforces = matches!(
            policy.floor_for(category),
            ContentFloor::Collapse | ContentFloor::Block | ContentFloor::Unknown
        );
        if !enforces {
            continue;
        }
        // A label in this category at or above the guardian trigger threshold is
        // what makes the floor bite — the exact predicate render_verdict_entries
        // uses, so an item is counted for precisely the categories whose guardian
        // floor also drove its render collapse/block.
        let bites = labels.iter().any(|l| {
            l.category.as_str() == category
                && l.confidence_per_mille >= GUARDIAN_FLOOR_TRIGGER_PERMILLE
        });
        if bites {
            out.push(category);
        }
    }
    out
}

/// The ward-side **Guardian Notify** counter (`family-safety.md` § Guardian
/// Notify), shared so tui and linux (and any future native app) cannot drift
/// on how Notify counts — this was independently reimplemented identically in
/// both before being lifted here (priority #2). Accumulates **coarse
/// per-category counts only — never a content id** (the whole point of
/// Notify), deduped by `(item id, category)` within the local day so a
/// re-render never re-counts, and batched: the pending deltas flush at most
/// once per [`NOTIFY_REPORT_MIN_INTERVAL_SECS`].
///
/// Every clock value arrives as a parameter rather than being read inside,
/// which is what lets tests drive the cadence directly instead of sleeping
/// for an hour (`testing.md` § point 14 — a sleeping test is *defunct*, not
/// merely slow).
///
/// [`Self::take_due`] returns `(category, count)` pairs rather than the wire
/// `FamilyContentNotice` type: that type lives downstream in
/// `fauna-client-family` (which depends on `fauna-protocol`, which depends on
/// `fauna-core`), so mapping into it is each app's own one-line job.
#[derive(Debug)]
pub struct NotifyAccumulator {
    /// Whether the guardian's `content_notify` knob is on. Counting is a
    /// no-op while off (§ Guardian Notify — "the ward-side counting runs only
    /// when it is on").
    on: bool,
    /// Per-category counts not yet reported to the nest (the delta the nest adds).
    pending: BTreeMap<&'static str, u32>,
    /// `(item id, category)` already counted for [`Self::seen_day`] — dedup
    /// across re-renders of the same item (a repaint must not re-bill the
    /// guardian's readout for content already seen today).
    seen: HashSet<(String, &'static str)>,
    /// The local day of [`Self::seen`], so the dedup set resets at local midnight.
    seen_day: i64,
    /// Last successful flush (epoch secs); `None` = never flushed, so the
    /// first report is eager.
    last_flush: Option<i64>,
    /// The device's UTC offset in minutes at the last record — passed to the
    /// nest so it stamps the ward-local day bucket (§ Screen time day-bucket
    /// rule, the one bucket rule this pillar shares).
    offset_minutes: i32,
}

impl Default for NotifyAccumulator {
    fn default() -> Self {
        Self {
            on: false,
            pending: BTreeMap::new(),
            seen: HashSet::new(),
            seen_day: i64::MIN,
            last_flush: None,
            offset_minutes: 0,
        }
    }
}

impl NotifyAccumulator {
    /// Whether counting is currently on — the cheap early-out a render site
    /// should check before scanning for guardian-enforced categories (the
    /// overwhelmingly common case, since `content_notify` defaults off).
    pub fn is_enabled(&self) -> bool {
        self.on
    }

    /// Set the guardian's `content_notify` knob. Turning it off drops any
    /// pending (not-yet-flushed) counts — they were accrued under consent
    /// that was just withdrawn, so they must not arrive after the fact.
    pub fn set_enabled(&mut self, on: bool) {
        self.on = on;
        if !on {
            self.pending.clear();
        }
    }

    /// Record a guardian-floor render-enforcement on `item_id` for the given
    /// categories. A no-op while disabled or `cats` is empty.
    pub fn record(
        &mut self,
        item_id: &str,
        cats: &[&'static str],
        now_secs: i64,
        offset_minutes: i32,
    ) {
        if !self.on || cats.is_empty() {
            return;
        }
        self.offset_minutes = offset_minutes;
        let local_day = (now_secs + i64::from(offset_minutes) * 60).div_euclid(86_400);
        if local_day != self.seen_day {
            self.seen.clear();
            self.seen_day = local_day;
        }
        for &cat in cats {
            if self.seen.insert((item_id.to_string(), cat)) {
                *self.pending.entry(cat).or_insert(0) += 1;
            }
        }
    }

    /// Drain the batched report if a flush is due (≤ hourly; the first report
    /// is eager). Returns `(category, count)` pairs plus the offset last
    /// recorded with; each app maps the pairs into its own `FamilyContentNotice`
    /// wire list.
    pub fn take_due(&mut self, now_secs: i64) -> Option<(Vec<(&'static str, u32)>, i32)> {
        if self.pending.is_empty() {
            return None;
        }
        // "batched (at most hourly)": at least one full interval between
        // flushes; the first report (no prior flush) is eager.
        if let Some(last) = self.last_flush
            && now_secs - last < NOTIFY_REPORT_MIN_INTERVAL_SECS
        {
            return None;
        }
        let entries = self
            .pending
            .iter()
            .map(|(cat, count)| (*cat, *count))
            .collect();
        self.pending.clear();
        self.last_flush = Some(now_secs);
        Some((entries, self.offset_minutes))
    }
}

/// Client render-enforcement state: the viewer's own thresholds, any
/// guardian floor, and the Guardian Notify counter, bundled into one type —
/// tui (an ordinary `App` field) and linux (a `thread_local!` cell) each
/// hand-rolled this same three-field wiring around
/// [`render_verdict_composed`]/[`guardian_enforced_categories`]/
/// [`NotifyAccumulator`] independently before this lift (the same shape as
/// `screen_time::ScreenLockCore` for the screen-time pillar — this module's
/// own composition was already shared; only the orchestration around it
/// wasn't).
///
/// Default = no thresholds, no floor, counting off = every item renders
/// normally, which is also what an identity change resets to
/// ([`Self::clear_for_identity_change`]).
#[derive(Debug, Default)]
pub struct ContentPolicyState {
    /// The viewer's OWN spam/phishing thresholds (`moderation.md` §
    /// Categories & enforcement item 1), hydrated at login from
    /// `fauna.spam.get_preferences`. `None` until that read lands. Every
    /// user gets this, supervised or not; a guardian floor composes ON TOP.
    own_thresholds: Option<ViewerThresholds>,
    /// The supervised viewer's own guardian content policy, set from the
    /// `fauna.family.status` read. `None` for an unsupervised account — no
    /// floor, only the viewer's own thresholds (if any) decide.
    ward_policy: Option<ContentPolicy>,
    /// Every region on the device's declared ancestor chain whose authority has
    /// published a policy, most specific first — the **third** strictest-wins
    /// source (`region-blocking.md` § The content plane). Empty for a device
    /// whose regions enrol nobody, which is every device today.
    ///
    /// Held here rather than passed per render for the same reason the guardian
    /// floor is: it changes on a policy refresh, not on a frame.
    region_policies: Vec<crate::region_policy::RegionRuleSet>,
    /// What the viewer hid by reporting it — their sealed
    /// `fauna.state.moderation` hidden-content list (`moderation.md` § Corollary —
    /// block also hides), hydrated at login and replaced by each hide the
    /// report sheet writes. Held here, beside the other render sources, so the
    /// one item verdict ([`Self::verdict_for_item`]) reads it on every surface
    /// and an identity change drops it with the rest.
    hidden_content: Vec<String>,
    /// The ward-side Guardian Notify counter.
    notify: NotifyAccumulator,
}

impl ContentPolicyState {
    /// Cache the viewer's own spam/phishing thresholds (from the post-auth
    /// `fauna.spam.get_preferences` read). A later render composes them via
    /// [`Self::verdict_for`]. `None` clears them.
    pub fn set_spam_preferences(&mut self, thresholds: Option<ViewerThresholds>) {
        self.own_thresholds = thresholds;
    }

    /// Set the supervised viewer's guardian content policy (from the
    /// `fauna.family.status` read). A later render enforces it via
    /// [`Self::verdict_for`]. `None` clears the floor (unsupervised, or a
    /// graduated ward).
    pub fn set_ward_content_policy(&mut self, policy: Option<ContentPolicy>) {
        self.ward_policy = policy;
    }

    /// Set whether the guardian's **Guardian Notify** knob (`content_notify`)
    /// is on. Ward-side counting runs only while on; turning it off drops
    /// future counting, pending counts included ([`NotifyAccumulator::set_enabled`]).
    pub fn set_ward_content_notify(&mut self, on: bool) {
        self.notify.set_enabled(on);
    }

    /// The content-policy render verdict for a piece of content, keyed on the
    /// lightweight [`crate::content_category::ContentLabelEntry`] list every
    /// app already holds per post/message.
    pub fn verdict_for(
        &self,
        labels: &[crate::content_category::ContentLabelEntry],
    ) -> ComposedVerdict {
        render_verdict_composed(
            labels,
            self.ward_policy.as_ref(),
            self.own_thresholds,
            &self.region_policies,
        )
    }

    /// **The** render verdict for one identified item — [`Self::verdict_for`]
    /// plus the viewer's own reports, through [`render_verdict_for_item`]:
    /// an item the viewer reported, or whose author they reported, is a
    /// `Block` attributed [`VerdictSource::Reported`]. `item_id` is the id the
    /// report named (a post's cid, a message's record digest); `author_id` the
    /// item's author as hex, when the surface knows it.
    pub fn verdict_for_item(
        &self,
        item_id: &str,
        author_id: Option<&str>,
        labels: &[crate::content_category::ContentLabelEntry],
    ) -> ComposedVerdict {
        render_verdict_for_item(
            &self.hidden_content,
            item_id,
            author_id,
            labels,
            self.ward_policy.as_ref(),
            self.own_thresholds,
            &self.region_policies,
        )
    }

    /// Replace the viewer's reported-and-hidden ids (the stored list a load or
    /// a hide returns — never merged, since the store's own list is the truth).
    pub fn set_hidden_content(&mut self, ids: Vec<String>) {
        self.hidden_content = ids;
    }

    /// Set the region content policies in force for this device — every region
    /// on its declared ancestor chain, most specific first
    /// (`crate::region_authority::RegionRegistry::chain`), each assembled by
    /// `crate::region_policy::rules_from_region_policy`.
    ///
    /// Replaces the whole set rather than merging, because the chain itself
    /// changes when the declared region does, and a merge would leave a
    /// departed region's rules in force.
    pub fn set_region_policies(&mut self, policies: Vec<crate::region_policy::RegionRuleSet>) {
        self.region_policies = policies;
    }

    /// The region policies currently in force, for the transparency surface
    /// (which names each region, its authority, and whether its document is
    /// applied or inert).
    pub fn region_policies(&self) -> &[crate::region_policy::RegionRuleSet] {
        &self.region_policies
    }

    /// Record any **guardian-floor** render-enforcement on `item_id` for
    /// Guardian Notify. A no-op unless the ward's `content_notify` knob is on
    /// AND the guardian floor bites on one of this item's labels (never the
    /// ward's own-threshold collapses). `now_secs`/`offset_minutes` are the
    /// caller's own clock read — this type reads no clock itself, the same
    /// testability rule [`NotifyAccumulator`] follows.
    pub fn note_enforcement(
        &mut self,
        item_id: &str,
        labels: &[crate::content_category::ContentLabelEntry],
        now_secs: i64,
        offset_minutes: i32,
    ) {
        if !self.notify.is_enabled() {
            return;
        }
        let cats = match &self.ward_policy {
            Some(policy) => guardian_enforced_categories(labels, policy),
            None => Vec::new(),
        };
        if cats.is_empty() {
            return;
        }
        self.notify.record(item_id, &cats, now_secs, offset_minutes);
    }

    /// Drain the batched Guardian Notify report if a flush is due (≤ hourly).
    /// Returns `(category, count)` pairs plus the offset — mapping into the
    /// wire `FamilyContentNotice` list stays each app's own one-line job
    /// (same reason as [`NotifyAccumulator::take_due`]).
    pub fn take_notify_report(&mut self, now_secs: i64) -> Option<(Vec<(&'static str, u32)>, i32)> {
        self.notify.take_due(now_secs)
    }

    /// Drop every actor-scoped field because the **identity is changing** —
    /// sign-out, account switch, factory reset. A caller whose own state
    /// resets implicitly with its containing struct (tui's `App`) does not
    /// need this; a caller holding a process-wide instance (linux's
    /// `thread_local!`) must call it explicitly, or an incoming account
    /// inherits the outgoing one's guardian floor and pending Notify counts.
    pub fn clear_for_identity_change(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Timestamp;
    use crate::label::{Attestation, ContentRef, MechanismRef, MechanismType};

    fn make_label(category: &str, confidence: f64) -> ContentLabel {
        ContentLabel {
            content_ref: ContentRef::Post { post_id: [0u8; 32] },
            category: category.to_string(),
            confidence,
            mechanism: MechanismRef {
                mechanism_type: MechanismType::TextClassifier,
                classifier_id: [0u8; 32],
                version: 1,
            },
            attestation: Attestation::Signed,
            obligation_id: None,
            created_at: Timestamp(0),
            signature: vec![],
        }
    }

    #[test]
    fn no_rules_no_triggers() {
        let result = evaluate_obligations(&[], &[], EnforcementPoint::Ingest);
        assert!(result.is_empty());
    }

    #[test]
    fn label_below_threshold_no_trigger() {
        let rules = vec![ObligationRule {
            category: "spam".into(),
            min_confidence_permille: 500,
            action: ObligationAction::Reject,
            requires_attestation: AttestationLevel::Any,
        }];
        let labels = vec![make_label("spam", 0.3)];
        let result = evaluate_obligations(&rules, &labels, EnforcementPoint::Ingest);
        assert!(result.is_empty());
    }

    #[test]
    fn label_at_threshold_triggers() {
        let rules = vec![ObligationRule {
            category: "spam".into(),
            min_confidence_permille: 500,
            action: ObligationAction::Quarantine,
            requires_attestation: AttestationLevel::Any,
        }];
        let labels = vec![make_label("spam", 0.5)];
        let result = evaluate_obligations(&rules, &labels, EnforcementPoint::Ingest);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, ObligationAction::Quarantine);
    }

    #[test]
    fn most_severe_action_sorts_first() {
        let rules = vec![
            ObligationRule {
                category: "spam".into(),
                min_confidence_permille: 300,
                action: ObligationAction::LabelOnly,
                requires_attestation: AttestationLevel::Any,
            },
            ObligationRule {
                category: "csam".into(),
                min_confidence_permille: 900,
                action: ObligationAction::Reject,
                requires_attestation: AttestationLevel::Any,
            },
        ];
        let labels = vec![make_label("spam", 0.8), make_label("csam", 0.95)];
        let result = evaluate_obligations(&rules, &labels, EnforcementPoint::Ingest);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, ObligationAction::Reject);
        assert_eq!(result[1].0, ObligationAction::LabelOnly);
    }

    #[test]
    fn wrong_category_no_trigger() {
        let rules = vec![ObligationRule {
            category: "csam".into(),
            min_confidence_permille: 500,
            action: ObligationAction::Reject,
            requires_attestation: AttestationLevel::Any,
        }];
        let labels = vec![make_label("spam", 0.9)];
        let result = evaluate_obligations(&rules, &labels, EnforcementPoint::Ingest);
        assert!(result.is_empty());
    }

    #[test]
    fn obligation_action_label_maps_every_discriminant() {
        // Every ObligationAction discriminant carries its nested
        // `moderation.action.*` key (mirrors `content_category`'s category map).
        assert_eq!(
            obligation_action_label(ObligationAction::Reject as u8).key,
            "moderation.action.rejected"
        );
        assert_eq!(
            obligation_action_label(ObligationAction::Quarantine as u8).key,
            "moderation.action.quarantined"
        );
        assert_eq!(
            obligation_action_label(ObligationAction::SuppressFromFeeds as u8).key,
            "moderation.action.suppressed"
        );
        assert_eq!(
            obligation_action_label(ObligationAction::RateLimit as u8).key,
            "moderation.action.rate_limited"
        );
        assert_eq!(
            obligation_action_label(ObligationAction::Log as u8).key,
            "moderation.action.logged"
        );
        assert_eq!(
            obligation_action_label(ObligationAction::LabelOnly as u8).key,
            "moderation.action.labeled"
        );
        assert_eq!(
            obligation_action_label(ObligationAction::TakenDown as u8).key,
            "moderation.action.taken_down"
        );
        // Notify (4) and any unknown discriminant → neutral "Flagged", no panic.
        assert_eq!(
            obligation_action_label(ObligationAction::Notify as u8).key,
            "moderation.action.flagged"
        );
        assert_eq!(
            obligation_action_label(200).key,
            "moderation.action.flagged"
        );
    }

    #[test]
    fn legal_takedown_tombstone_carries_reference_arg() {
        let t = legal_takedown_tombstone("EU-DSA-2024/12345");
        assert_eq!(t.key, "moderation.legal_takedown.tombstone");
        assert_eq!(
            t.args.get("reference").map(String::as_str),
            Some("EU-DSA-2024/12345")
        );
        // Resolves the {reference} placeholder through a client's i18n template.
        assert_eq!(
            t.resolve(|k| (k == "moderation.legal_takedown.tombstone")
                .then_some("Removed under legal obligation ({reference})")),
            "Removed under legal obligation (EU-DSA-2024/12345)"
        );
    }

    // --- Client render-enforcement engine (family-safety.md § Content policy) ---

    #[test]
    fn rules_from_preferences_collapses_spam_and_phishing() {
        // A user's own thresholds map to *Collapse* rules (never Block — block is
        // a guardian-only floor), keyed at the user's own per-mille slider value.
        let rules = rules_from_preferences(800, 600);
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].category, "spam");
        assert_eq!(rules[0].min_confidence_permille, 800);
        assert_eq!(rules[0].action, ObligationAction::Collapse);
        assert_eq!(rules[1].category, "phishing");
        assert_eq!(rules[1].min_confidence_permille, 600);
        assert_eq!(rules[1].action, ObligationAction::Collapse);
    }

    #[test]
    fn rules_from_guardian_policy_maps_each_floor() {
        // inherit → no rule; collapse → Collapse rule; block → Block rule; the
        // fail-closed Unknown → Block rule. All keyed at the hard-coded trigger.
        let policy = ContentPolicy {
            nsfw: ContentFloor::Collapse,
            spam: ContentFloor::Block,
            phishing: ContentFloor::Inherit,
            commercial: ContentFloor::Unknown,
        };
        let rules = rules_from_guardian_policy(&policy);
        // phishing (inherit) contributes no rule.
        assert_eq!(rules.len(), 3);
        let by_cat = |c: &str| rules.iter().find(|r| r.category == c);
        assert_eq!(by_cat("nsfw").unwrap().action, ObligationAction::Collapse);
        assert_eq!(by_cat("spam").unwrap().action, ObligationAction::Block);
        assert!(by_cat("phishing").is_none());
        // Unknown floor renders fail-closed as Block (family-safety.md § Content policy).
        assert_eq!(
            by_cat("commercial").unwrap().action,
            ObligationAction::Block
        );
        for r in &rules {
            assert_eq!(r.min_confidence_permille, GUARDIAN_FLOOR_TRIGGER_PERMILLE);
        }
    }

    #[test]
    fn rules_from_guardian_policy_all_inherit_is_empty() {
        assert!(rules_from_guardian_policy(&ContentPolicy::default()).is_empty());
    }

    #[test]
    fn render_verdict_no_label_shows() {
        assert_eq!(
            render_verdict(&[], &rules_from_preferences(800, 600)),
            RenderVerdict::Show
        );
    }

    #[test]
    fn render_verdict_label_below_threshold_badges() {
        // A label is present but the user's threshold does not escalate it — the
        // moderation.md "labels produce a badge" behavior is the floor.
        let labels = vec![make_label("spam", 0.5)];
        assert_eq!(
            render_verdict(&labels, &rules_from_preferences(800, 600)),
            RenderVerdict::Badge
        );
    }

    #[test]
    fn render_verdict_own_threshold_collapses() {
        let labels = vec![make_label("spam", 0.85)];
        assert_eq!(
            render_verdict(&labels, &rules_from_preferences(800, 600)),
            RenderVerdict::Collapse
        );
    }

    #[test]
    fn render_verdict_guardian_inherit_defers_to_ward_preference() {
        // Guardian floor is inherit (no rule) → the ward's own Collapse still
        // applies: "a ward may set themselves stricter than the guardian floor".
        let mut rules = rules_from_guardian_policy(&ContentPolicy::default()); // all inherit
        rules.extend(rules_from_preferences(800, 600));
        let labels = vec![make_label("spam", 0.9)];
        assert_eq!(render_verdict(&labels, &rules), RenderVerdict::Collapse);
    }

    #[test]
    fn render_verdict_strictest_wins_block_over_collapse() {
        // The user's own Collapse rule AND a guardian Block floor both trigger on
        // the same label — Block wins even though it sits at the *higher* (less
        // severe by discriminant) enum value.
        let mut rules = rules_from_preferences(800, 600); // spam → Collapse @800
        rules.extend(rules_from_guardian_policy(&ContentPolicy {
            spam: ContentFloor::Block,
            ..Default::default()
        })); // spam → Block @500
        let labels = vec![make_label("spam", 0.9)];
        assert_eq!(render_verdict(&labels, &rules), RenderVerdict::Block);
    }

    #[test]
    fn render_verdict_guardian_floor_triggers_at_hard_coded_threshold() {
        // A guardian collapse floor bites at exactly GUARDIAN_FLOOR_TRIGGER_PERMILLE.
        let rules = rules_from_guardian_policy(&ContentPolicy {
            nsfw: ContentFloor::Collapse,
            ..Default::default()
        });
        let at = f64::from(GUARDIAN_FLOOR_TRIGGER_PERMILLE) / 1000.0;
        assert_eq!(
            render_verdict(&[make_label("nsfw", at)], &rules),
            RenderVerdict::Collapse
        );
        // A hair below the threshold: label present but floor does not bite → Badge.
        assert_eq!(
            render_verdict(&[make_label("nsfw", at - 0.001)], &rules),
            RenderVerdict::Badge
        );
    }

    #[test]
    fn render_verdict_fail_closed_unknown_floor_blocks() {
        // End-to-end fail-closed: a guardian floor value this client cannot parse
        // renders `block` (family-safety.md § Content policy).
        let policy: ContentPolicy = serde_json::from_str(r#"{"nsfw":"restrict"}"#).unwrap();
        assert_eq!(policy.nsfw, ContentFloor::Unknown);
        let rules = rules_from_guardian_policy(&policy);
        let labels = vec![make_label("nsfw", 0.9)];
        assert_eq!(render_verdict(&labels, &rules), RenderVerdict::Block);
    }

    // --- render_verdict_entries: the client render path's entry point ---

    fn make_entry(category: &str, per_mille: u16) -> crate::content_category::ContentLabelEntry {
        crate::content_category::ContentLabelEntry {
            category: category.to_string(),
            confidence_per_mille: per_mille,
        }
    }

    /// One region's assembled rule set, as
    /// `region_policy::rules_from_region_policy` produces it.
    fn region_set(
        code: &str,
        factor: &str,
        permille: u16,
        verdict: crate::region_policy::ContentVerdict,
    ) -> crate::region_policy::RegionRuleSet {
        use crate::region_policy::{ContentPolicyDocument, ContentRule, GRAMMAR_VERSION};
        let document = ContentPolicyDocument {
            version: GRAMMAR_VERSION,
            rules: vec![ContentRule {
                factor: factor.into(),
                min_permille: permille,
                verdict,
                reason_code: format!("{code}-1"),
                reason: std::collections::BTreeMap::from([(
                    crate::region_policy::REASON_DEFAULT_KEY.to_string(),
                    format!("Restricted in {code}."),
                )]),
                extra: Default::default(),
            }],
            scorers: Vec::new(),
            extra: Default::default(),
        };
        let region = crate::region_authority::RegionCode::parse(code).unwrap();
        crate::region_policy::rules_from_region_policy(
            &document,
            &region,
            &format!("{code} authority"),
        )
    }

    #[test]
    fn a_region_policy_is_the_third_strictest_wins_source() {
        // The case region-blocking.md § Where it composes turns on: the viewer's
        // own threshold would only collapse, the guardian defers (`inherit`),
        // and the region blocks — so the item is blocked, and the placeholder
        // must be able to name the region rather than a generic "policy".
        use crate::region_policy::ContentVerdict;

        let labels = [make_entry("nsfw", 900)];
        let guardian_inherits = ContentPolicy::default(); // every floor `inherit`
        let own = ViewerThresholds {
            spam_permille: 500,
            phishing_permille: 500,
        };
        let regions = [region_set("NO", "nsfw", 800, ContentVerdict::Block)];

        let composed =
            render_verdict_composed(&labels, Some(&guardian_inherits), Some(own), &regions);
        assert_eq!(composed.verdict, RenderVerdict::Block);
        let attribution = composed.region().expect("attributed to the region source");
        assert_eq!(attribution.region.as_str(), "NO");
        assert_eq!(attribution.authority_name, "NO authority");
        assert_eq!(attribution.reason_code, "NO-1");
        assert_eq!(
            attribution.reason.get("default").map(String::as_str),
            Some("Restricted in NO.")
        );
    }

    #[test]
    fn the_precedence_across_the_three_sources_is_untouched() {
        use crate::region_policy::ContentVerdict;
        let labels = [make_entry("spam", 900)];
        let own = ViewerThresholds {
            spam_permille: 800,
            phishing_permille: 600,
        };
        let guardian_blocks = ContentPolicy {
            spam: ContentFloor::Block,
            ..Default::default()
        };
        let region_collapses = [region_set("NO", "spam", 100, ContentVerdict::Collapse)];

        // A region that only collapses does not soften a guardian block:
        // `Block > Collapse` is the engine's precedence and the third source
        // joins it rather than reordering it.
        let composed = render_verdict_composed(
            &labels,
            Some(&guardian_blocks),
            Some(own),
            &region_collapses,
        );
        assert_eq!(composed.verdict, RenderVerdict::Block);
        assert_eq!(composed.source, Some(VerdictSource::GuardianFloor));

        // A region alone still collapses.
        let composed = render_verdict_composed(&labels, None, None, &region_collapses);
        assert_eq!(composed.verdict, RenderVerdict::Collapse);
        assert!(composed.region().is_some());

        // No source fires: a present-but-unescalated label still badges, and
        // there is nothing to attribute.
        let composed =
            render_verdict_composed(&[make_entry("nsfw", 10)], None, None, &region_collapses);
        assert_eq!(composed.verdict, RenderVerdict::Badge);
        assert_eq!(composed.source, None);
    }

    #[test]
    fn a_tie_between_a_region_and_the_family_floor_is_attributed_to_the_region() {
        // Both are true, and the user is shown one placeholder. It names the
        // region, because a region block carries an authority's own reason the
        // user is entitled to read (invariant 1) while the family placeholder
        // carries no such external obligation.
        use crate::region_policy::ContentVerdict;
        let labels = [make_entry("spam", 900)];
        let guardian_blocks = ContentPolicy {
            spam: ContentFloor::Block,
            ..Default::default()
        };
        let regions = [region_set("NO", "spam", 100, ContentVerdict::Block)];

        let composed = render_verdict_composed(&labels, Some(&guardian_blocks), None, &regions);
        assert_eq!(composed.verdict, RenderVerdict::Block);
        assert_eq!(composed.region().map(|a| a.region.as_str()), Some("NO"));
    }

    #[test]
    fn the_most_specific_region_on_the_chain_takes_the_attribution() {
        // The chain is applied most-specific-first, so when a subdivision and
        // its country both block, the placeholder names the subdivision's
        // authority — the one closest to the user.
        use crate::region_policy::ContentVerdict;
        let labels = [make_entry("nsfw", 900)];
        let regions = [
            region_set("NO-03", "nsfw", 100, ContentVerdict::Block),
            region_set("NO", "nsfw", 100, ContentVerdict::Block),
        ];
        let composed = render_verdict_composed(&labels, None, None, &regions);
        assert_eq!(composed.region().map(|a| a.region.as_str()), Some("NO-03"));

        // …but strictness still outranks specificity: a subdivision that merely
        // collapses does not take the attribution from a country that blocks.
        let regions = [
            region_set("NO-03", "nsfw", 100, ContentVerdict::Collapse),
            region_set("NO", "nsfw", 100, ContentVerdict::Block),
        ];
        let composed = render_verdict_composed(&labels, None, None, &regions);
        assert_eq!(composed.verdict, RenderVerdict::Block);
        assert_eq!(composed.region().map(|a| a.region.as_str()), Some("NO"));
    }

    #[test]
    fn an_inert_region_document_enforces_nothing_through_the_engine() {
        // The end-to-end of region_policy's inert rule: a document at a grammar
        // version this build does not implement reaches the engine as a rule set
        // with no rules, so the item renders as if that region published
        // nothing — never a blanket block over an unreadable document.
        use crate::region_policy::{ContentPolicyDocument, GRAMMAR_VERSION};
        let mut document = ContentPolicyDocument {
            version: GRAMMAR_VERSION,
            rules: Vec::new(),
            scorers: Vec::new(),
            extra: Default::default(),
        };
        document.version = GRAMMAR_VERSION + 3;
        let region = crate::region_authority::RegionCode::parse("NO").unwrap();
        let set =
            crate::region_policy::rules_from_region_policy(&document, &region, "NO authority");

        let composed = render_verdict_composed(
            &[make_entry("nsfw", 900)],
            None,
            None,
            std::slice::from_ref(&set),
        );
        assert_eq!(composed.verdict, RenderVerdict::Badge);
        assert_eq!(composed.source, None);
        // …and the surface can still say *why* nothing was applied.
        assert!(set.status.is_inert());
    }

    #[test]
    fn the_state_threads_the_region_source_through_verdict_for() {
        // The path tui and linux actually render through — a region source that
        // reached `render_verdict_composed` but not `ContentPolicyState` would
        // be built and unreachable.
        use crate::region_policy::ContentVerdict;
        let mut state = ContentPolicyState::default();
        state.set_spam_preferences(Some(ViewerThresholds {
            spam_permille: 500,
            phishing_permille: 500,
        }));
        assert_eq!(
            state.verdict_for(&[make_entry("nsfw", 900)]).verdict,
            RenderVerdict::Badge,
            "no region policy yet: nsfw is not one of the viewer's own thresholds"
        );

        state.set_region_policies(vec![region_set("NO", "nsfw", 800, ContentVerdict::Block)]);
        let composed = state.verdict_for(&[make_entry("nsfw", 900)]);
        assert_eq!(composed.verdict, RenderVerdict::Block);
        assert_eq!(composed.region().map(|a| a.region.as_str()), Some("NO"));
        assert_eq!(state.region_policies().len(), 1);

        // An identity change drops them with everything else actor-scoped.
        state.clear_for_identity_change();
        assert!(state.region_policies().is_empty());
        assert_eq!(
            state.verdict_for(&[make_entry("nsfw", 900)]).verdict,
            RenderVerdict::Badge
        );
    }

    #[test]
    fn render_verdict_entries_matches_the_content_label_path() {
        // The entry-based twin must give the SAME verdict as render_verdict for the
        // same category/confidence — it just keys on the lightweight per-mille entry
        // every app already holds.
        let rules = rules_from_guardian_policy(&ContentPolicy {
            spam: ContentFloor::Block,
            ..Default::default()
        });
        // No label → Show.
        assert_eq!(render_verdict_entries(&[], &rules), RenderVerdict::Show);
        // A spam entry above the guardian floor → Block.
        assert_eq!(
            render_verdict_entries(&[make_entry("spam", 900)], &rules),
            RenderVerdict::Block
        );
        // A label present but below the floor → Badge (moderation.md's badge floor).
        assert_eq!(
            render_verdict_entries(&[make_entry("spam", 499)], &rules),
            RenderVerdict::Badge
        );
        // A different category the floor doesn't cover → Badge (present, unescalated).
        assert_eq!(
            render_verdict_entries(&[make_entry("trusted", 900)], &rules),
            RenderVerdict::Badge
        );
    }

    #[test]
    fn render_verdict_entries_triggers_exactly_at_the_permille_threshold() {
        let rules = rules_from_guardian_policy(&ContentPolicy {
            nsfw: ContentFloor::Collapse,
            ..Default::default()
        });
        // Integer comparison: `>=` the per-mille threshold, no float round-trip.
        assert_eq!(
            render_verdict_entries(
                &[make_entry("nsfw", GUARDIAN_FLOOR_TRIGGER_PERMILLE)],
                &rules
            ),
            RenderVerdict::Collapse
        );
        assert_eq!(
            render_verdict_entries(
                &[make_entry("nsfw", GUARDIAN_FLOOR_TRIGGER_PERMILLE - 1)],
                &rules
            ),
            RenderVerdict::Badge
        );
    }

    #[test]
    fn render_verdict_entries_strictest_wins_and_fails_closed() {
        // Block (guardian) wins over Collapse (own pref) on the same label.
        let mut rules = rules_from_preferences(800, 600);
        rules.extend(rules_from_guardian_policy(&ContentPolicy {
            spam: ContentFloor::Block,
            ..Default::default()
        }));
        assert_eq!(
            render_verdict_entries(&[make_entry("spam", 900)], &rules),
            RenderVerdict::Block
        );
        // A guardian floor value this client cannot parse renders block.
        let policy: ContentPolicy = serde_json::from_str(r#"{"phishing":"restrict"}"#).unwrap();
        assert_eq!(
            render_verdict_entries(
                &[make_entry("phishing", 900)],
                &rules_from_guardian_policy(&policy)
            ),
            RenderVerdict::Block
        );
    }

    // --- render_verdict_for_item: the viewer's own reports ---

    /// What the viewer reported — the item, or its author — blocks for them
    /// with the `Reported` attribution, case-insensitively; anything else
    /// renders exactly as the composed verdict would.
    #[test]
    fn a_reported_item_or_author_blocks_for_the_reporter() {
        let hidden = vec!["abcd".to_string(), "a1a1".to_string()];
        let reported = |item: &str, author: Option<&str>| {
            render_verdict_for_item(&hidden, item, author, &[], None, None, &[])
        };
        for v in [reported("ABCD", None), reported("zz", Some("A1A1"))] {
            assert_eq!(v.verdict, RenderVerdict::Block);
            assert!(v.reported());
            assert_eq!(v.region(), None);
        }
        let other = reported("zz", Some("b2b2"));
        assert_eq!(other, render_verdict_composed(&[], None, None, &[]));
        assert!(!other.reported());
        assert!(!reported_by_viewer(&[], "abcd", None));
    }

    // --- render_verdict_composed: the ONE assembly every app consumes ---

    #[test]
    fn render_verdict_composed_matches_a_hand_assembled_verdict() {
        // The composed entry point must be exactly `rules_from_preferences` ∪
        // `rules_from_guardian_policy` ∪ `render_verdict_entries` — the three steps
        // the linux/wasm/ffi surfaces each used to hand-write. Asserted against a
        // hand-assembly so a future change to the composition order or gating is
        // caught here rather than drifting one app at a time.
        let policy = ContentPolicy {
            spam: ContentFloor::Block,
            nsfw: ContentFloor::Collapse,
            ..Default::default()
        };
        let labels = [make_entry("spam", 900), make_entry("nsfw", 700)];

        let mut hand = rules_from_preferences(800, 600);
        hand.extend(rules_from_guardian_policy(&policy));

        assert_eq!(
            render_verdict_composed(
                &labels,
                Some(&policy),
                Some(ViewerThresholds {
                    spam_permille: 800,
                    phishing_permille: 600,
                }),
                &[],
            )
            .verdict,
            render_verdict_entries(&labels, &hand),
        );
    }

    #[test]
    fn render_verdict_composed_gates_each_half_independently() {
        let labels = [make_entry("spam", 900)];
        let guardian_blocks_spam = ContentPolicy {
            spam: ContentFloor::Block,
            ..Default::default()
        };
        let own = ViewerThresholds {
            spam_permille: 800,
            phishing_permille: 600,
        };

        // Neither half → no rule fires; a present-but-unescalated label badges.
        assert_eq!(
            render_verdict_composed(&labels, None, None, &[]).verdict,
            RenderVerdict::Badge
        );
        // Own thresholds only (the every-user half, moderation.md § item 1) → Collapse.
        assert_eq!(
            render_verdict_composed(&labels, None, Some(own), &[]).verdict,
            RenderVerdict::Collapse
        );
        // Guardian floor only (an unsupervised-preferences ward) → Block.
        assert_eq!(
            render_verdict_composed(&labels, Some(&guardian_blocks_spam), None, &[]).verdict,
            RenderVerdict::Block
        );
        // Both → strictest wins: the guardian's Block over the own Collapse.
        assert_eq!(
            render_verdict_composed(&labels, Some(&guardian_blocks_spam), Some(own), &[]).verdict,
            RenderVerdict::Block
        );
    }

    #[test]
    fn render_verdict_composed_keeps_an_unparseable_floor_fail_closed() {
        // family-safety.md § Content policy: "A rule value a client cannot parse
        // renders fail-closed (block)" — the composed path must not soften it.
        let policy: ContentPolicy = serde_json::from_str(r#"{"phishing":"restrict"}"#).unwrap();
        assert_eq!(
            render_verdict_composed(&[make_entry("phishing", 900)], Some(&policy), None, &[])
                .verdict,
            RenderVerdict::Block
        );
    }

    // --- the kids-app compiled floor (family-safety.md § The account age band) ---

    #[test]
    fn the_kids_floor_is_every_guardian_category_at_block() {
        for category in GUARDIAN_FLOOR_CATEGORIES {
            assert_eq!(
                KIDS_CONTENT_FLOOR.floor_for(category),
                ContentFloor::Block,
                "{category} must be floored at block"
            );
        }
    }

    #[test]
    fn a_guardian_policy_below_the_floor_renders_at_the_floor() {
        let floored = with_compiled_floor(
            Some(&ContentPolicy {
                nsfw: ContentFloor::Collapse,
                ..ContentPolicy::default()
            }),
            Some(&KIDS_CONTENT_FLOOR),
        );
        for category in GUARDIAN_FLOOR_CATEGORIES {
            assert_eq!(
                render_verdict_composed(&[make_entry(category, 900)], floored.as_ref(), None, &[]),
                ComposedVerdict {
                    verdict: RenderVerdict::Block,
                    source: Some(VerdictSource::GuardianFloor),
                },
                "{category}: an inherit or collapse guardian floor renders at the compiled block"
            );
        }
        // Below the trigger, the floor does not bite: the label only badges.
        assert_eq!(
            render_verdict_composed(&[make_entry("nsfw", 10)], floored.as_ref(), None, &[]).verdict,
            RenderVerdict::Badge
        );
    }

    #[test]
    fn a_stricter_guardian_policy_wins_over_the_compiled_floor() {
        let collapse_floor = ContentPolicy {
            spam: ContentFloor::Collapse,
            ..ContentPolicy::default()
        };
        let guardian_blocks_spam = ContentPolicy {
            spam: ContentFloor::Block,
            ..ContentPolicy::default()
        };
        let composed = with_compiled_floor(Some(&guardian_blocks_spam), Some(&collapse_floor));
        assert_eq!(composed.unwrap().spam, ContentFloor::Block);
        assert_eq!(
            render_verdict_composed(&[make_entry("spam", 900)], composed.as_ref(), None, &[])
                .verdict,
            RenderVerdict::Block
        );
        // A fail-closed floor the client cannot parse stays block under any floor.
        let unknown = ContentPolicy {
            phishing: ContentFloor::Unknown,
            ..ContentPolicy::default()
        };
        assert_eq!(
            unknown.strictest_with(&collapse_floor).phishing.as_str(),
            "block"
        );
    }

    #[test]
    fn no_compiled_floor_leaves_the_guardian_policy_untouched() {
        let guardian = ContentPolicy {
            commercial: ContentFloor::Collapse,
            ..ContentPolicy::default()
        };
        assert_eq!(with_compiled_floor(Some(&guardian), None), Some(guardian));
        assert_eq!(with_compiled_floor(None, None), None);
        assert_eq!(
            with_compiled_floor(None, Some(&KIDS_CONTENT_FLOOR)),
            Some(KIDS_CONTENT_FLOOR),
            "an unsupervised viewer in a floored build gets the floor alone"
        );
    }

    /// `content_notify` is deliberately not floored: Notify counts the
    /// guardian's OWN policy, so an inherit guardian under the kids floor
    /// reports nothing even though the item renders blocked.
    #[test]
    fn the_kids_floor_never_reaches_guardian_notify() {
        let inherit = ContentPolicy::default();
        let labels = [make_entry("nsfw", 900)];
        let floored = with_compiled_floor(Some(&inherit), Some(&KIDS_CONTENT_FLOOR));
        assert_eq!(
            render_verdict_composed(&labels, floored.as_ref(), None, &[]).verdict,
            RenderVerdict::Block
        );
        assert!(guardian_enforced_categories(&labels, &inherit).is_empty());
    }

    #[test]
    fn render_verdict_as_str_is_the_wire_vocabulary() {
        // The wasm + ffi bindings both return these four strings to their clients;
        // the mapping lives here so the two can never disagree.
        assert_eq!(RenderVerdict::Show.as_str(), "show");
        assert_eq!(RenderVerdict::Badge.as_str(), "badge");
        assert_eq!(RenderVerdict::Collapse.as_str(), "collapse");
        assert_eq!(RenderVerdict::Block.as_str(), "block");
    }

    #[test]
    fn content_floor_wire_round_trip_and_fail_closed_helpers() {
        // as_str ∘ from_wire is identity for the three catalog values.
        for v in ContentFloor::ORDER {
            assert_eq!(ContentFloor::from_wire(v.as_str()), v);
        }
        // Unrecognized → Unknown → renders as "block".
        assert_eq!(ContentFloor::from_wire("restrict"), ContentFloor::Unknown);
        assert_eq!(ContentFloor::Unknown.as_str(), "block");
        assert_eq!(ContentFloor::FAIL_CLOSED, ContentFloor::Block);
        assert_ne!(ContentFloor::FAIL_CLOSED, ContentFloor::Inherit);
        assert_eq!(
            ContentFloor::ORDER,
            [
                ContentFloor::Inherit,
                ContentFloor::Collapse,
                ContentFloor::Block
            ]
        );
    }

    #[test]
    fn content_floor_serde_round_trips_and_degrades_unknown() {
        // Wire is dag-cbor; the serde *derive* behavior asserted here is
        // format-agnostic (serde_json is the convenient proxy).
        assert_eq!(
            serde_json::from_str::<ContentFloor>(r#""inherit""#).unwrap(),
            ContentFloor::Inherit
        );
        assert_eq!(
            serde_json::from_str::<ContentFloor>(r#""collapse""#).unwrap(),
            ContentFloor::Collapse
        );
        assert_eq!(
            serde_json::from_str::<ContentFloor>(r#""block""#).unwrap(),
            ContentFloor::Block
        );
        // An unrecognized value (a newer nest) degrades to Unknown, not an error.
        assert_eq!(
            serde_json::from_str::<ContentFloor>(r#""quarantine""#).unwrap(),
            ContentFloor::Unknown
        );
        // Lowercase on the wire.
        assert_eq!(
            serde_json::to_string(&ContentFloor::Block).unwrap(),
            r#""block""#
        );
    }

    #[test]
    fn content_policy_partial_document_defaults_to_inherit() {
        // A document naming only one category leaves the rest at Inherit
        // ("floor only the categories named, inherit the rest").
        let policy: ContentPolicy = serde_json::from_str(r#"{"nsfw":"block"}"#).unwrap();
        assert_eq!(policy.nsfw, ContentFloor::Block);
        assert_eq!(policy.spam, ContentFloor::Inherit);
        assert_eq!(policy.phishing, ContentFloor::Inherit);
        assert_eq!(policy.commercial, ContentFloor::Inherit);
        // Empty document → all inherit.
        assert_eq!(
            serde_json::from_str::<ContentPolicy>("{}").unwrap(),
            ContentPolicy::default()
        );
    }

    // --- guardian_enforced_categories: Guardian Notify counting ---

    #[test]
    fn guardian_enforced_categories_reports_the_biting_floor_category() {
        // A guardian block floor on spam + a spam label above the trigger → spam
        // is counted (the guardian's policy is acting).
        let policy = ContentPolicy {
            spam: ContentFloor::Block,
            ..Default::default()
        };
        assert_eq!(
            guardian_enforced_categories(&[make_entry("spam", 900)], &policy),
            vec!["spam"]
        );
        // A collapse floor bites the same way.
        let policy = ContentPolicy {
            nsfw: ContentFloor::Collapse,
            ..Default::default()
        };
        assert_eq!(
            guardian_enforced_categories(&[make_entry("nsfw", 900)], &policy),
            vec!["nsfw"]
        );
    }

    #[test]
    fn guardian_enforced_categories_ignores_inherit_and_own_threshold() {
        // An all-inherit guardian policy counts nothing — even for a label the
        // ward's OWN threshold would collapse. Notify reports the guardian's
        // policy acting, not the ward's own choices (§ Guardian Notify).
        assert!(
            guardian_enforced_categories(&[make_entry("spam", 999)], &ContentPolicy::default())
                .is_empty()
        );
        // A floor on a category with no matching label counts nothing.
        let policy = ContentPolicy {
            phishing: ContentFloor::Block,
            ..Default::default()
        };
        assert!(guardian_enforced_categories(&[make_entry("spam", 999)], &policy).is_empty());
    }

    #[test]
    fn guardian_enforced_categories_uses_the_render_trigger_threshold() {
        // The same GUARDIAN_FLOOR_TRIGGER_PERMILLE boundary render_verdict_entries
        // uses: at the threshold it bites, a hair below it does not — so counting
        // never diverges from what actually rendered collapsed/blocked.
        let policy = ContentPolicy {
            spam: ContentFloor::Collapse,
            ..Default::default()
        };
        assert_eq!(
            guardian_enforced_categories(
                &[make_entry("spam", GUARDIAN_FLOOR_TRIGGER_PERMILLE)],
                &policy
            ),
            vec!["spam"]
        );
        assert!(
            guardian_enforced_categories(
                &[make_entry("spam", GUARDIAN_FLOOR_TRIGGER_PERMILLE - 1)],
                &policy
            )
            .is_empty()
        );
    }

    #[test]
    fn guardian_enforced_categories_fail_closed_unknown_floor_counts() {
        // A floor value this client cannot parse renders `block` (fail-closed) and
        // is likewise *counted* — the guardian's (newer) policy is acting.
        let policy: ContentPolicy = serde_json::from_str(r#"{"commercial":"restrict"}"#).unwrap();
        assert_eq!(policy.commercial, ContentFloor::Unknown);
        assert_eq!(
            guardian_enforced_categories(&[make_entry("commercial", 900)], &policy),
            vec!["commercial"]
        );
    }

    #[test]
    fn guardian_enforced_categories_returns_all_biting_categories_in_canonical_order() {
        // Multiple floors bite on a multi-labeled item → all counted, in
        // GUARDIAN_FLOOR_CATEGORIES order (nsfw, spam, phishing, commercial).
        let policy = ContentPolicy {
            nsfw: ContentFloor::Block,
            spam: ContentFloor::Collapse,
            phishing: ContentFloor::Inherit,
            commercial: ContentFloor::Block,
        };
        let labels = vec![
            make_entry("commercial", 800),
            make_entry("spam", 800),
            make_entry("nsfw", 800),
            make_entry("phishing", 800), // inherit floor → not counted
        ];
        assert_eq!(
            guardian_enforced_categories(&labels, &policy),
            vec!["nsfw", "spam", "commercial"]
        );
    }

    // --- NotifyAccumulator (Guardian Notify batching/dedup) ---

    #[test]
    fn notify_accumulator_dedups_per_item_and_batches_hourly() {
        let mut acc = NotifyAccumulator {
            on: true,
            ..Default::default()
        };
        // The same item recorded twice counts once (the repaint case); a second
        // item is +1.
        acc.record("post-1", &["spam"], 1000, 120);
        acc.record("post-1", &["spam"], 1005, 120);
        acc.record("post-2", &["spam"], 1010, 120);
        // The first report is eager (no prior flush) and carries the delta + offset.
        let (entries, offset) = acc.take_due(1010).expect("first batch is due");
        assert_eq!(offset, 120);
        assert_eq!(entries, vec![("spam", 2)]);
        // Drained; nothing to send again immediately.
        assert!(acc.take_due(1011).is_none());
        // A new event within the hour accumulates but does NOT flush.
        acc.record("post-3", &["spam"], 1100, 120);
        assert!(acc.take_due(1100).is_none());
        // Once a full interval has passed, the accumulated delta flushes.
        let (entries, _) = acc
            .take_due(1010 + NOTIFY_REPORT_MIN_INTERVAL_SECS)
            .expect("second batch is due after the interval");
        assert_eq!(entries, vec![("spam", 1)]);
    }

    #[test]
    fn notify_accumulator_disabled_counts_nothing() {
        let mut acc = NotifyAccumulator::default(); // on = false
        acc.record("post-1", &["spam"], 1000, 0);
        assert!(acc.take_due(1_000_000).is_none());
    }

    #[test]
    fn notify_accumulator_resets_dedup_at_local_day() {
        let mut acc = NotifyAccumulator {
            on: true,
            ..Default::default()
        };
        acc.record("post-1", &["spam"], 1000, 0);
        // The same item on the next local day is a fresh enforcement → counted again.
        acc.record("post-1", &["spam"], 1000 + 86_400, 0);
        let (entries, _) = acc.take_due(1_000_000).unwrap();
        assert_eq!(entries, vec![("spam", 2)]);
    }

    #[test]
    fn notify_accumulator_set_enabled_false_drops_pending() {
        let mut acc = NotifyAccumulator::default();
        acc.set_enabled(true);
        assert!(acc.is_enabled());
        acc.record("post-1", &["spam"], 1000, 0);
        // Disabling drops the pending delta rather than letting it flush after
        // consent was withdrawn.
        acc.set_enabled(false);
        assert!(!acc.is_enabled());
        assert!(acc.take_due(1_000_000).is_none());
    }

    // ── ContentPolicyState (the tui/linux orchestration lift) ───────────────

    fn entry(category: &str, per_mille: u16) -> crate::content_category::ContentLabelEntry {
        crate::content_category::ContentLabelEntry {
            category: category.into(),
            confidence_per_mille: per_mille,
        }
    }

    fn thresholds(spam: u16, phishing: u16) -> ViewerThresholds {
        ViewerThresholds {
            spam_permille: spam,
            phishing_permille: phishing,
        }
    }

    #[test]
    fn content_policy_state_unset_renders_show_or_badge() {
        let state = ContentPolicyState::default();
        assert_eq!(state.verdict_for(&[]).verdict, RenderVerdict::Show);
        // A label present but no rule to escalate it → Badge (surfaces render
        // it normally, same as Show, beside the `content-label-badge`).
        assert_eq!(
            state.verdict_for(&[entry("spam", 900)]).verdict,
            RenderVerdict::Badge
        );
    }

    #[test]
    fn content_policy_state_own_threshold_collapses_for_every_user() {
        // Unsupervised (no guardian policy): the viewer's own spam threshold
        // alone collapses a spam-labeled item — the un-darking of
        // moderation.md § item 1, which § Content policy builds "once, for
        // every user".
        let mut state = ContentPolicyState::default();
        state.set_spam_preferences(Some(thresholds(500, 300)));
        assert_eq!(
            state.verdict_for(&[entry("spam", 900)]).verdict,
            RenderVerdict::Collapse
        );
        assert_eq!(
            state.verdict_for(&[entry("phishing", 400)]).verdict,
            RenderVerdict::Collapse
        );
        // Below the viewer's threshold → just a badge.
        assert_eq!(
            state.verdict_for(&[entry("spam", 400)]).verdict,
            RenderVerdict::Badge
        );
    }

    #[test]
    fn content_policy_state_guardian_block_wins_over_own_collapse() {
        // The viewer's own threshold would collapse; the guardian's block
        // floor composes on top and wins (strictest-wins).
        let mut state = ContentPolicyState::default();
        state.set_spam_preferences(Some(thresholds(500, 300)));
        state.set_ward_content_policy(Some(ContentPolicy {
            spam: ContentFloor::Block,
            ..Default::default()
        }));
        assert_eq!(
            state.verdict_for(&[entry("spam", 900)]).verdict,
            RenderVerdict::Block
        );
    }

    #[test]
    fn content_policy_state_guardian_only_still_enforces_without_own_prefs() {
        // A supervised account whose spam prefs never loaded still gets the
        // guardian floor.
        let mut state = ContentPolicyState::default();
        state.set_ward_content_policy(Some(ContentPolicy {
            nsfw: ContentFloor::Collapse,
            ..Default::default()
        }));
        assert_eq!(
            state.verdict_for(&[entry("nsfw", 900)]).verdict,
            RenderVerdict::Collapse
        );
    }

    #[test]
    fn content_policy_state_an_unparseable_floor_fails_closed_to_block() {
        // § Content policy: "A rule value a client cannot parse renders
        // fail-closed (`block`)" — `Unknown` is what `ContentFloor::from_wire`
        // yields for a value this client is too old to name, so a newer
        // guardian setting can never silently fail OPEN.
        let mut state = ContentPolicyState::default();
        state.set_ward_content_policy(Some(ContentPolicy {
            spam: ContentFloor::Unknown,
            ..Default::default()
        }));
        assert_eq!(
            state.verdict_for(&[entry("spam", 900)]).verdict,
            RenderVerdict::Block
        );
    }

    #[test]
    fn content_policy_state_note_enforcement_counts_only_the_guardian_floor_when_on() {
        let mut state = ContentPolicyState::default();
        state.set_ward_content_policy(Some(ContentPolicy {
            spam: ContentFloor::Block,
            ..Default::default()
        }));
        // Knob off (the default) → nothing counted even with a guardian
        // block floor.
        state.note_enforcement("p1", &[entry("spam", 900)], 1000, 0);
        assert!(state.take_notify_report(1000).is_none());
        // Knob on → the guardian block floor's category is counted, once
        // per item.
        state.set_ward_content_notify(true);
        state.note_enforcement("p1", &[entry("spam", 900)], 1000, 0);
        state.note_enforcement("p1", &[entry("spam", 900)], 1000, 0);
        let (entries, _) = state.take_notify_report(1000).expect("a due report");
        assert_eq!(entries, vec![("spam", 1)]);
    }

    #[test]
    fn content_policy_state_note_enforcement_ignores_the_wards_own_threshold() {
        let mut state = ContentPolicyState::default();
        state.set_ward_content_notify(true);
        // No guardian policy (all inherit): a collapse driven by the ward's
        // OWN spam threshold is NOT reported — Notify is a lens on the
        // *guardian's* policy, not the ward's own choices.
        state.set_spam_preferences(Some(thresholds(500, 300)));
        state.note_enforcement("p1", &[entry("spam", 900)], 1000, 0);
        assert!(state.take_notify_report(1000).is_none());
    }

    #[test]
    fn content_policy_state_turning_the_knob_off_drops_pending_counts() {
        let mut state = ContentPolicyState::default();
        state.set_ward_content_policy(Some(ContentPolicy {
            spam: ContentFloor::Block,
            ..Default::default()
        }));
        state.set_ward_content_notify(true);
        state.note_enforcement("p1", &[entry("spam", 900)], 1000, 0);
        // The guardian turns Notify off before the batch ever flushed: the
        // pending counts go with it rather than arriving after consent was
        // withdrawn.
        state.set_ward_content_notify(false);
        assert!(state.take_notify_report(1000).is_none());
    }

    /// The held hide list reaches the one item verdict: a reported item or
    /// author blocks attributed `Reported`, anything else composes as before,
    /// and an identity change drops the outgoing account's list.
    #[test]
    fn content_policy_state_item_verdict_reads_the_held_hide_list() {
        let mut state = ContentPolicyState::default();
        state.set_hidden_content(vec!["abcd".into(), "a1a1".into()]);
        let item = state.verdict_for_item("ABCD", None, &[]);
        assert_eq!(item.verdict, RenderVerdict::Block);
        assert!(item.reported());
        assert!(state.verdict_for_item("zz", Some("a1a1"), &[]).reported());
        let other = state.verdict_for_item("zz", Some("b2b2"), &[entry("spam", 900)]);
        assert_eq!(other.verdict, RenderVerdict::Badge);
        assert!(!other.reported());

        state.clear_for_identity_change();
        assert!(!state.verdict_for_item("abcd", None, &[]).reported());
    }

    #[test]
    fn content_policy_state_clear_for_identity_change_drops_every_field() {
        let mut state = ContentPolicyState::default();
        // A supervised ward mid-session: a guardian floor, the ward's own
        // thresholds, the Notify knob on, and one counted enforcement pending.
        state.set_spam_preferences(Some(thresholds(500, 300)));
        state.set_ward_content_policy(Some(ContentPolicy {
            spam: ContentFloor::Block,
            ..Default::default()
        }));
        state.set_ward_content_notify(true);
        state.note_enforcement("p1", &[entry("spam", 900)], 1000, 0);

        state.clear_for_identity_change();

        // 1. The outgoing ward's pending counts must not flush as the
        //    incoming actor — the report carries no identity of its own.
        assert!(
            state.take_notify_report(1000).is_none(),
            "the outgoing ward's pending Notify counts survived the identity change"
        );
        // 2. The outgoing ward's guardian floor must not bind the incoming
        //    account.
        assert_eq!(
            state.verdict_for(&[entry("spam", 900)]).verdict,
            RenderVerdict::Badge,
            "the outgoing ward's guardian content floor survived the identity change"
        );
        // 3. The per-item dedup must not carry over: an item the OUTGOING
        //    ward already saw would otherwise go silently uncounted for the
        //    incoming one.
        state.set_ward_content_policy(Some(ContentPolicy {
            spam: ContentFloor::Block,
            ..Default::default()
        }));
        state.set_ward_content_notify(true);
        state.note_enforcement("p1", &[entry("spam", 900)], 1000, 0);
        let (entries, _) = state
            .take_notify_report(1000)
            .expect("the incoming ward's first enforcement on p1 must count");
        assert_eq!(entries, vec![("spam", 1)]);
    }
}
