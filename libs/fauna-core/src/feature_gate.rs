//! The controversial-class feature gate — the registry, the tier meet, and the
//! one shared verdict function.
//!
//! Authority: `docs/goal/architecture/dynamic-features.md` (§ The gated-feature
//! registry, § The rule-setter model, § The policy shape, § Wire & data shape).
//! This module is the *policy plane* half of that doc's charter — capability (a),
//! dynamic gating. Capability (b), compile-time excision, is the cargo feature
//! spine and has no runtime representation here on purpose: an excised artifact
//! does not carry the feature at all, so tier 0 is unrepresentable at runtime
//! (§ What "completely compiled away" means, item 5).
//!
//! **One catalog, consumed by the nest and all 7 apps** (priority #1), so the
//! gated set can never diverge per app. Feature keys are the same stable strings
//! the cargo features and the capability tokens use.
//!
//! The shape deliberately mirrors two existing shared decisions rather than
//! copying them: [`crate::data::supervised_reach_verdict`] (a pure function from
//! state + context to a small verdict enum, enforced at the nest floor) and
//! [`crate::obligation::render_verdict_composed`] (strictest-wins composition of
//! several rule sources). This composes *operation* verdicts rather than render
//! verdicts, so the code is separate and the shape is shared.
//!
//! # What this plane never does
//!
//! No gate weakens encryption, reads content, or adds a per-item ledger
//! (§ What this is NOT). Everything evaluated here is the plaintext-floor
//! operation metadata the nest already sees: how many times, toward how many
//! distinct parties, of what machine-comparable magnitude.

use serde::{Deserialize, Serialize};

/// A controversial-class feature, by its stable key.
///
/// The string form is load-bearing in three places at once — the cargo feature
/// that excises it, the capability token that advertises it, and the policy
/// documents that bound it — so it is spelled explicitly per variant rather than
/// derived from the identifier by a rename rule (§ Wire & data shape: "Feature
/// keys are stable strings, shared with capability tokens").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum GatedFeature {
    /// Real-money content payments (§ Charter members — criteria 1, 2).
    #[serde(rename = "payments")]
    Payments,
    /// Lightning zap receipts and signer designation — a **subset member** of
    /// [`GatedFeature::Payments`] (§ Charter members — criteria 1, 2).
    #[serde(rename = "zaps")]
    Zaps,
    /// Bulk cross-user shared-set transfer (§ Charter members — criteria 2, 3).
    #[serde(rename = "p2p-share")]
    P2pShare,
    /// A feature a newer nest knows and this build does not — the open arm of
    /// `transport.md` § Rule 3 in full (*Open, collapsing*). Every reply that
    /// lists features stays decodable when the registry grows; the unknown
    /// entry gates nothing this build knows, no surface renders it
    /// ([`GatedFeature::is_known`] filters it out of every row and editor), and
    /// the nest refuses it in a write. Never in [`GatedFeature::ALL`] or the
    /// registry; [`entry`] answers it with a deny-everything stand-in.
    ///
    /// Collapsing rather than carrying because no reader writes back a feature
    /// it did not author: the nest serializes features only from its own
    /// registry, and an app echoes only a member it opened an editor for. It is
    /// never serialized — encoding it fails loudly.
    #[serde(other, skip_serializing)]
    Unknown,
}

impl GatedFeature {
    /// Every registry member, in registry order. Callers that must act on the
    /// whole gated set (a status read, an admin UI listing) iterate this rather
    /// than hand-listing members, so a new member cannot be missed on one
    /// surface (priority #1).
    pub const ALL: [GatedFeature; 3] = [
        GatedFeature::Payments,
        GatedFeature::Zaps,
        GatedFeature::P2pShare,
    ];

    /// The stable key. [`GatedFeature::Unknown`] answers `"unknown"`, which
    /// [`GatedFeature::from_key`] never resolves and the nest never stores (its
    /// writes refuse the arm before keying anything by it).
    pub fn as_str(&self) -> &'static str {
        match self {
            GatedFeature::Payments => "payments",
            GatedFeature::Zaps => "zaps",
            GatedFeature::P2pShare => "p2p-share",
            GatedFeature::Unknown => "unknown",
        }
    }

    /// Whether this build knows the feature — false only for the open arm.
    pub fn is_known(&self) -> bool {
        !matches!(self, GatedFeature::Unknown)
    }

    /// Parse a stable key. Unknown keys are `None` rather than an error type:
    /// a newer peer naming a feature this build has never heard of is the
    /// additive-everywhere case, not a fault.
    pub fn from_key(key: &str) -> Option<Self> {
        GatedFeature::ALL.into_iter().find(|f| f.as_str() == key)
    }
}

/// A quota dimension. Which dimensions apply to a feature is declared by its
/// registry entry; a dimension the registry does not declare for a feature is
/// **unrepresentable** in that feature's policy (§ The quota grammar).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaDimension {
    /// Operation count per window — files shared per week, claims minted per day.
    Operations,
    /// **Distinct** counterparties per window — the fan-out bound, the dimension
    /// that separates person-to-person from mass distribution.
    ///
    /// Distinctness is resolved by the caller against the feature's *own*
    /// existing records (a share roster, a claim table), never by a new
    /// per-counterparty log: boundary 2 permits no per-item ledger beyond what
    /// the feature's mechanism already stores. What this module sees is the
    /// resulting count.
    Counterparties,
    /// Magnitude per window — bytes moved, or machine-comparable value in
    /// millisatoshis. The unit is declared per feature by [`FeatureEntry::unit`].
    Volume,
}

/// The unit of the [`QuotaDimension::Volume`] dimension for a feature. Declared
/// per feature so a magnitude can never be compared or rendered in the wrong
/// unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MagnitudeUnit {
    Bytes,
    /// Millisatoshis — the machine-comparable value unit Fauna already uses for
    /// a priced tier (`fauna_protocol::subscriptions::TierAskingPrice`). Fauna
    /// never parses provider-side prices (monetization.md § The asking price),
    /// so a value cap binds only where an msat price exists.
    Millisats,
}

impl QuotaDimension {
    /// The stable key — the one spelling of this dimension on the wire, at
    /// rest, and in a message. Same contract as [`GatedFeature::as_str`], and
    /// the reason it is a dedicated map rather than a `Debug`/`Display`
    /// derive: **these strings are at rest**, so a rename of the Rust variant
    /// must not silently re-key existing quota buckets and hand every account
    /// a fresh quota.
    ///
    /// Lifted here 2026-08-23 from FOUR byte-identical hand-written copies —
    /// this crate's own `UndeclaredDimension` message,
    /// `fauna_client_features::row::dimension_key`, the nest's
    /// `feature_gate::dimension_label` and its `db::feature_gate::dimension_key`
    /// (the at-rest column spelling). Nothing had held them to each other.
    pub fn as_str(&self) -> &'static str {
        match self {
            QuotaDimension::Operations => "operations",
            QuotaDimension::Counterparties => "counterparties",
            QuotaDimension::Volume => "volume",
        }
    }
}

/// A quota window. A closed enum on purpose (§ The quota grammar) — an open
/// duration would make the meet's per-window cells unbounded and let a
/// rule-setter express a window no counter can answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Window {
    Day,
    Week,
    Month,
}

impl Window {
    /// Every window, tightest first. Verdict evaluation walks windows in this
    /// order so a refusal names the tightest window a violation trips.
    pub const ALL: [Window; 3] = [Window::Day, Window::Week, Window::Month];

    /// How many day buckets this window sums (§ Usage accounting: counters are
    /// day buckets, windows are evaluated by summing them). Trailing windows,
    /// not calendar ones — a calendar boundary is a reset a distributor can
    /// simply wait for, and it would make the bound depend on the day of month.
    pub fn days(&self) -> u32 {
        match self {
            Window::Day => 1,
            Window::Week => 7,
            Window::Month => 30,
        }
    }

    /// The stable key — see [`QuotaDimension::as_str`] for why this is a
    /// dedicated map. Lifted 2026-08-23 from two byte-identical copies
    /// (`fauna_client_features::row::window_key`, the nest's
    /// `feature_gate::window_label`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Window::Day => "day",
            Window::Week => "week",
            Window::Month => "month",
        }
    }
}

/// Who set a restriction. Ordered outermost-scope first, matching the doc's
/// tier numbering (§ The tiers).
///
/// Tier 0 (excision) has no variant: it is the artifact's own compiled absence,
/// with nothing to evaluate at runtime. The ratified rule-setter *priority*
/// order (governments > app stores > guardians > admins) resolves design
/// tension between rule-setters and has deliberately **no mechanical effect
/// here** — the meet is order-free (§ The rule-setter model).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleTier {
    /// Tier 1 — structural defaults. Principal: nobody (Rust constants).
    Structural,
    /// Tier 2 — a region's administering authority, as published policy data.
    Region,
    /// Tier 3 — the nest admin, for every account on this nest.
    Admin,
    /// Tier 4 — the ward's guardian, for one supervised account.
    Guardian,
    /// Tier 5 — the user, for themselves. Spelled `self` on the wire, the
    /// same string [`RuleTier::as_str`] returns.
    #[serde(rename = "self")]
    SelfImposed,
    /// A tier a newer nest attributes a bound to and this build cannot name —
    /// the open arm of `transport.md` § Rule 3 in full (*Open, collapsing*).
    /// Attribution only: it renders as *"another rule-setter"*, and the bound
    /// it is attached to is shown and enforced like any other. Never
    /// serialized — the nest builds every tier it sends, so encoding this
    /// fails loudly.
    #[serde(other, skip_serializing)]
    Unknown,
}

impl RuleTier {
    /// The stable key — see [`QuotaDimension::as_str`] for why this is a
    /// dedicated map. Lifted 2026-08-23 from two byte-identical copies
    /// (`fauna_client_features::row::tier_key`, the nest's
    /// `feature_gate::tier_label` — whose own doc comment called it "the
    /// vocabulary a client renders *'limited by your admin'* from" while the
    /// client kept its own copy of it).
    ///
    /// This IS the `serde` spelling: [`RuleTier::SelfImposed`] carries
    /// `#[serde(rename = "self")]` over the derive's snake_case, so a reply
    /// body carrying an [`EffectivePolicy`] (`fauna.features.status`, the
    /// authored reads' ceiling), a gate refusal's `details.tier` and every
    /// app's row keys all name a tier one way — pinned by
    /// `a_tier_encodes_as_its_as_str_spelling`. (Until 2026-10-02 the serde
    /// body said `self_imposed`; the user ruled `self`, recorded in
    /// `ratified-breaks.txt`.) Not at rest — the nest stores the tier as an
    /// integer column.
    pub fn as_str(&self) -> &'static str {
        match self {
            RuleTier::Structural => "structural",
            RuleTier::Region => "region",
            RuleTier::Admin => "admin",
            RuleTier::Guardian => "guardian",
            RuleTier::SelfImposed => "self",
            RuleTier::Unknown => "unknown",
        }
    }
}

/// What a rule-setter authored for availability (§ The quota grammar).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// Usable, bounded only by whatever dimensions this tier also set.
    #[default]
    Allow,
    /// Not usable at this tier's scope. Any tier's deny denies.
    Deny,
    /// Explicitly bounded. Semantically `Allow` plus the dimensions below; kept
    /// as an authored value because it is what a rule-setter writes and reads
    /// back, while the *effective* value is derived
    /// ([`EffectivePolicy::availability`]).
    Limit,
    /// A value a newer rule-setter authored that this build cannot read — the
    /// open arm of `transport.md` § Rule 3 in full (*Open, carrying*). It holds
    /// the exact string read and re-emits it, because a policy document is
    /// re-saved whole and folded from the signed region artifact into the
    /// nest's store: dropping or rewriting it would delete a restriction. It
    /// **enforces as [`Availability::Deny`]** ([`Availability::denies`]) — an
    /// unknown availability never grants.
    #[serde(untagged)]
    Other(String),
}

impl Availability {
    /// Whether this authored value turns the feature off at its tier: an
    /// explicit deny, or a value this build cannot read (the restrictive
    /// reading — `transport.md` § Rule 3 in full).
    pub fn denies(&self) -> bool {
        matches!(self, Availability::Deny | Availability::Other(_))
    }
}

/// One dimension's bounds, at most one per window.
///
/// Multiple windows on one dimension are not redundancy — they are the shape
/// that makes the meet **total**. Two tiers bounding the same dimension over
/// different windows ("50 per week" against "10 per day") have no common
/// numeric minimum, so collapsing them to a single bound would have to either
/// invent a conversion or drop a restriction. Keeping one cell per window makes
/// the meet an element-wise `min` — order-free, associative, idempotent — and
/// evaluation a conjunction: every present bound must hold.
///
/// This is the build-time refinement of the doc's "each quota dimension = MIN
/// across tiers": the MIN is per (dimension, window) cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WindowedBounds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_day: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_week: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_month: Option<u64>,
}

impl WindowedBounds {
    /// No opinion on any window.
    pub const UNSET: Self = Self {
        per_day: None,
        per_week: None,
        per_month: None,
    };

    /// A single bound.
    pub fn at(window: Window, limit: u64) -> Self {
        let mut bounds = Self::UNSET;
        *bounds.cell_mut(window) = Some(limit);
        bounds
    }

    pub fn get(&self, window: Window) -> Option<u64> {
        match window {
            Window::Day => self.per_day,
            Window::Week => self.per_week,
            Window::Month => self.per_month,
        }
    }

    fn cell_mut(&mut self, window: Window) -> &mut Option<u64> {
        match window {
            Window::Day => &mut self.per_day,
            Window::Week => &mut self.per_week,
            Window::Month => &mut self.per_month,
        }
    }

    /// Whether this tier expressed any opinion on this dimension. An unset
    /// dimension is "no opinion", **not** "unlimited past tier 1"
    /// (§ Composition — the meet rule).
    pub fn is_unset(&self) -> bool {
        *self == Self::UNSET
    }

    /// Add this bound with the given window, keeping the tighter value if the
    /// cell is already occupied.
    pub fn tightened_with(mut self, window: Window, limit: u64) -> Self {
        let cell = self.cell_mut(window);
        *cell = Some(match *cell {
            Some(existing) => existing.min(limit),
            None => limit,
        });
        self
    }
}

/// A per-feature policy document, identical at every tier that can express one
/// (§ The quota grammar). Every field is `#[serde(default)]`: absent means "no
/// opinion at this tier", which is what makes the document additive-everywhere
/// and lets the guardian tier ride `fauna.family.policy.update` with
/// absent-means-unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FeaturePolicy {
    #[serde(default)]
    pub availability: Availability,
    #[serde(default)]
    pub operations: WindowedBounds,
    #[serde(default)]
    pub counterparties: WindowedBounds,
    #[serde(default)]
    pub volume: WindowedBounds,
    /// A cap on a single operation's magnitude, in the feature's declared unit.
    ///
    /// Not a windowed dimension and deliberately exempt from the
    /// tier-1-always-present rule: "no bound on how big one legitimate item may
    /// be" is the correct personal-use posture (§ The quota grammar sizes tier 1
    /// so the spouses moving hundreds of GB never feel a gate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_operation_max: Option<u64>,
}

impl FeaturePolicy {
    /// A policy expressing nothing — the identity of the meet.
    pub const NO_OPINION: Self = Self {
        availability: Availability::Allow,
        operations: WindowedBounds::UNSET,
        counterparties: WindowedBounds::UNSET,
        volume: WindowedBounds::UNSET,
        per_operation_max: None,
    };

    /// Deny outright, expressing no dimensions.
    pub const DENIED: Self = Self {
        availability: Availability::Deny,
        ..Self::NO_OPINION
    };

    pub fn bounds(&self, dimension: QuotaDimension) -> &WindowedBounds {
        match dimension {
            QuotaDimension::Operations => &self.operations,
            QuotaDimension::Counterparties => &self.counterparties,
            QuotaDimension::Volume => &self.volume,
        }
    }

    /// Reject a document that bounds a dimension the registry does not declare
    /// for this feature (§ The quota grammar: such a bound is unrepresentable).
    ///
    /// Validation rather than a type-level impossibility because the applicable
    /// dimension set is registry *data* — per-feature policy structs would make
    /// the gated set diverge, which is exactly what one catalog prevents.
    pub fn validate(&self, entry: &FeatureEntry) -> Result<(), UndeclaredDimension> {
        for dimension in [
            QuotaDimension::Operations,
            QuotaDimension::Counterparties,
            QuotaDimension::Volume,
        ] {
            if !self.bounds(dimension).is_unset() && !entry.declares(dimension) {
                return Err(UndeclaredDimension {
                    feature: entry.feature,
                    dimension,
                });
            }
        }
        if self.per_operation_max.is_some() && !entry.declares(QuotaDimension::Volume) {
            return Err(UndeclaredDimension {
                feature: entry.feature,
                dimension: QuotaDimension::Volume,
            });
        }
        Ok(())
    }
}

/// A policy document bounded a dimension its feature does not declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UndeclaredDimension {
    pub feature: GatedFeature,
    pub dimension: QuotaDimension,
}

impl std::fmt::Display for UndeclaredDimension {
    /// Names both halves, because either one alone is unactionable: the writer
    /// needs to know *which* dimension was refused and *which* feature does not
    /// declare it. This is a rule-setter-facing message — it reaches an admin or
    /// guardian through the update kind's `invalid_params` — so it says what is
    /// wrong with the document rather than what the code did.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "feature `{}` does not declare the `{}` quota dimension",
            self.feature.as_str(),
            self.dimension.as_str()
        )
    }
}

impl std::error::Error for UndeclaredDimension {}

/// A bound that survived the meet, with the tier that set it — attribution is
/// not decoration: every active restriction must be visible to the person it
/// binds, *including which tier set it* (§ Transparency & auditability, and
/// boundary 4 "no silent gates").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundSource {
    pub limit: u64,
    pub tier: RuleTier,
}

/// One dimension's bounds after the meet, each cell carrying its source tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EffectiveBounds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_day: Option<BoundSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_week: Option<BoundSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_month: Option<BoundSource>,
}

impl EffectiveBounds {
    pub const UNSET: Self = Self {
        per_day: None,
        per_week: None,
        per_month: None,
    };

    pub fn get(&self, window: Window) -> Option<BoundSource> {
        match window {
            Window::Day => self.per_day,
            Window::Week => self.per_week,
            Window::Month => self.per_month,
        }
    }

    fn cell_mut(&mut self, window: Window) -> &mut Option<BoundSource> {
        match window {
            Window::Day => &mut self.per_day,
            Window::Week => &mut self.per_week,
            Window::Month => &mut self.per_month,
        }
    }

    pub fn is_unset(&self) -> bool {
        *self == Self::UNSET
    }
}

/// Keep the tighter of two cells. On an exact tie the **outermost-scope** tier
/// is named (the lowest [`RuleTier`]) — deterministic, and it attributes the
/// restriction to the tier the bound person can do least about, which is the
/// more useful thing to show them. A tie is not a precedence claim: both tiers
/// bound identically, so neither ordering changes what is enforced.
fn meet_cell(left: Option<BoundSource>, right: Option<BoundSource>) -> Option<BoundSource> {
    match (left, right) {
        (Some(a), Some(b)) => Some(
            if b.limit < a.limit || (b.limit == a.limit && b.tier < a.tier) {
                b
            } else {
                a
            },
        ),
        (some, None) | (None, some) => some,
    }
}

/// The effective policy for an (account, feature) pair — the meet of every
/// applicable tier, with each surviving restriction attributed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectivePolicy {
    pub feature: GatedFeature,
    /// The tier that denied, if any. Any tier's deny denies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denied_by: Option<RuleTier>,
    #[serde(default)]
    pub operations: EffectiveBounds,
    #[serde(default)]
    pub counterparties: EffectiveBounds,
    #[serde(default)]
    pub volume: EffectiveBounds,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_operation_max: Option<BoundSource>,
}

impl EffectivePolicy {
    pub fn bounds(&self, dimension: QuotaDimension) -> &EffectiveBounds {
        match dimension {
            QuotaDimension::Operations => &self.operations,
            QuotaDimension::Counterparties => &self.counterparties,
            QuotaDimension::Volume => &self.volume,
        }
    }

    /// The derived availability a client renders: `Deny` if any tier denied,
    /// `Limit` if any bound survived the meet, else `Allow`. Derived rather than
    /// stored so "limited" can never disagree with the bounds actually enforced.
    pub fn availability(&self) -> Availability {
        if self.denied_by.is_some() {
            return Availability::Deny;
        }
        let bounded = self.operations.is_unset()
            && self.counterparties.is_unset()
            && self.volume.is_unset()
            && self.per_operation_max.is_none();
        if bounded {
            Availability::Allow
        } else {
            Availability::Limit
        }
    }
}

/// Observed usage for one (account, feature) pair — each dimension summed over
/// each window from the day buckets (§ Usage accounting). Coarse counts only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UsageCounters {
    pub operations: WindowCounts,
    pub counterparties: WindowCounts,
    pub volume: WindowCounts,
}

impl UsageCounters {
    pub fn get(&self, dimension: QuotaDimension) -> &WindowCounts {
        match dimension {
            QuotaDimension::Operations => &self.operations,
            QuotaDimension::Counterparties => &self.counterparties,
            QuotaDimension::Volume => &self.volume,
        }
    }
}

/// One dimension's observed totals per window.
///
/// Serde-carried because § Transparency requires the bound person to see their
/// **remaining** quota, and remaining is `limit - observed` — so the observed
/// side rides the transparency read rather than being recomputed client-side
/// from a second, divergent notion of "the window".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WindowCounts {
    pub day: u64,
    pub week: u64,
    pub month: u64,
}

impl WindowCounts {
    pub fn get(&self, window: Window) -> u64 {
        match window {
            Window::Day => self.day,
            Window::Week => self.week,
            Window::Month => self.month,
        }
    }
}

/// The operation being gated. Carries only what a bound can be evaluated
/// against — never content, never an identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateOp {
    pub feature: GatedFeature,
    /// The gate-surface identifier from the registry entry, for the refusal and
    /// the audit row.
    pub surface: &'static str,
    /// How many counterparties this operation newly introduces within the
    /// window — `0` for a repeat recipient, `n` for a batch admitting `n` new
    /// members. Resolved by the caller against the feature's own records.
    pub new_counterparties: u64,
    /// The operation's magnitude in the feature's declared unit; `0` when the
    /// operation moves nothing measurable.
    pub magnitude: u64,
}

impl GateOp {
    /// The dimension delta this operation contributes. An operation is always
    /// exactly one operation — that is what "operation count" counts.
    ///
    /// `pub`: the verdict function (below) is the only *judge*, but a caller
    /// that already holds an `Allow` still needs this exact number to spend —
    /// two independently-maintained copies would risk applying a different
    /// delta than the one the verdict measured.
    pub fn delta(&self, dimension: QuotaDimension) -> u64 {
        match dimension {
            QuotaDimension::Operations => 1,
            QuotaDimension::Counterparties => self.new_counterparties,
            QuotaDimension::Volume => self.magnitude,
        }
    }
}

/// The verdict for one gated operation (§ Evaluation points).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureVerdict {
    Allow,
    /// A tier denied the feature outright.
    Deny {
        tier: RuleTier,
    },
    /// A quota bound would be exceeded by this operation. Carries everything a
    /// typed Dim-4 refusal must name: which dimension, over which window, the
    /// limit, what is already observed, and the binding tier.
    OverQuota {
        dimension: QuotaDimension,
        /// `None` for the per-operation magnitude cap, which is not windowed.
        window: Option<Window>,
        limit: u64,
        observed: u64,
        tier: RuleTier,
    },
}

impl FeatureVerdict {
    pub fn is_allowed(&self) -> bool {
        matches!(self, FeatureVerdict::Allow)
    }

    /// The tier binding this refusal, for the refusal message and the
    /// transparency read. `None` only when the verdict allows.
    pub fn binding_tier(&self) -> Option<RuleTier> {
        match self {
            FeatureVerdict::Allow => None,
            FeatureVerdict::Deny { tier } => Some(*tier),
            FeatureVerdict::OverQuota { tier, .. } => Some(*tier),
        }
    }
}

/// A registry entry — the shared-Rust catalog row for one gated feature
/// (§ Wire & data shape: "The registry is shared Rust (`fauna_core`): feature
/// keys, per-feature applicable dimensions, tier-1 constants, gate-surface
/// identifiers").
#[derive(Debug, Clone)]
pub struct FeatureEntry {
    pub feature: GatedFeature,
    /// The dimensions a policy may bound for this feature. Anything else is
    /// unrepresentable ([`FeaturePolicy::validate`]).
    pub dimensions: &'static [QuotaDimension],
    /// The unit of [`QuotaDimension::Volume`] for this feature.
    pub unit: MagnitudeUnit,
    /// The superset this feature is a **subset member** of, if any
    /// (§ Charter members — `zaps` is a subset member of `payments`).
    pub superset: Option<GatedFeature>,
    /// Tier 1's structural constants — always present in the meet, so the
    /// effective policy is never unbounded on a declared dimension
    /// (§ Composition — the meet rule).
    pub tier1: FeaturePolicy,
    /// Where availability and quotas bind, as stable identifiers (§ Charter
    /// members' "Gate surfaces" column). Named here so the nest, the refusal,
    /// and the audit row all use one vocabulary.
    pub gate_surfaces: &'static [&'static str],
}

impl FeatureEntry {
    pub fn declares(&self, dimension: QuotaDimension) -> bool {
        self.dimensions.contains(&dimension)
    }
}

// ---------------------------------------------------------------------------
// Gate-surface identifiers
// ---------------------------------------------------------------------------

/// `payments` sell side (§ Charter members).
pub const SURFACE_PAYMENTS_PROVIDER_CONFIGURE: &str = "payments.provider.configure";
pub const SURFACE_PAYMENTS_TIER_PRICE: &str = "payments.tier.price";
pub const SURFACE_PAYMENTS_PAYWALL_DESIGNATE: &str = "payments.paywall.designate";
pub const SURFACE_PAYMENTS_CLAIM_MINT: &str = "payments.claim.mint";
/// `payments` buy side.
pub const SURFACE_PAYMENTS_CLAIM_REDEEM: &str = "payments.claim.redeem";
pub const SURFACE_PAYMENTS_UNLOCK_PURCHASE: &str = "payments.unlock.purchase";
pub const SURFACE_PAYMENTS_TIP_SEND: &str = "payments.tip.send";

/// `zaps` receive side (§ Charter members). The send side is unbuilt.
pub const SURFACE_ZAPS_SIGNER_DESIGNATE: &str = "zaps.signer.designate";
pub const SURFACE_ZAPS_RECEIPT_INGEST: &str = "zaps.receipt.ingest";

/// `p2p-share` (§ Charter members). Member admission is the fan-out chokepoint.
pub const SURFACE_P2P_SHARE_MEMBER_ADMIT: &str = "p2p-share.member.admit";
pub const SURFACE_P2P_SHARE_TRANSFER: &str = "p2p-share.transfer";

// ---------------------------------------------------------------------------
// Tier-1 structural constants
// ---------------------------------------------------------------------------
//
// Sizing rule (§ The quota grammar): **generous for personal use, prohibitive
// only at distribution scale**. The spouses moving hundreds of GB between their
// own devices — two counterparties, huge bytes — must never feel a gate;
// sharing to hundreds of strangers must be structurally impossible.
//
// The two member classes are sized differently *because their membership
// criteria differ*, which is the whole reasoning behind these numbers:
//
// - `payments` and `zaps` are gated under criteria 1 + 2 (app-store policy
//   risk, regulatory divergence) and **not** criterion 3. Scale is not what
//   makes them controversial, so tier 1 must not cap a successful creator: its
//   job is an anti-runaway ceiling — a compromised account or a looping client
//   minting claims — well above any human pattern. Jurisdictional value
//   thresholds (AML and the like) differ so widely that a fleet-wide constant
//   could only be wrong; those belong to the region tier, which tightens.
// - `p2p-share` is gated under criteria 2 + 3, where the **scale inversion is
//   the point**: the counterparty bound is the structural gate that makes the
//   Pirate-Bay shape impossible without anyone's goodwill, while operations and
//   volume stay generous enough that no personal use meets them.

/// Millisatoshis per satoshi — the msat magnitudes below are written in terms of
/// satoshis, which is how every human statement about them is phrased. Owned by
/// [`crate::money::MSATS_PER_SAT`].
use crate::money::MSATS_PER_SAT as MSAT_PER_SAT;

/// Tier-1 constants for `payments`.
///
/// - **operations** 1,000/day and 10,000/month: a very successful individual
///   creator transacts in the tens per day; a thousand is an anti-runaway
///   ceiling, not a business cap.
/// - **counterparties** 1,000/day and 5,000/month: a viral post can genuinely
///   sell to a four-figure audience in a day, so this must not bite; a payment
///   *processor's* fan-out is orders of magnitude above it.
/// - **volume** 21,000,000 sat/day and 210,000,000 sat/month (0.21 / 2.1 BTC):
///   far above an individual's earnings, far below a rail's throughput.
const TIER1_PAYMENTS: FeaturePolicy = FeaturePolicy {
    availability: Availability::Limit,
    operations: WindowedBounds {
        per_day: Some(1_000),
        per_week: None,
        per_month: Some(10_000),
    },
    counterparties: WindowedBounds {
        per_day: Some(1_000),
        per_week: None,
        per_month: Some(5_000),
    },
    volume: WindowedBounds {
        per_day: Some(21_000_000 * MSAT_PER_SAT),
        per_week: None,
        per_month: Some(210_000_000 * MSAT_PER_SAT),
    },
    per_operation_max: None,
};

/// Tier-1 constants for `zaps`.
///
/// Zaps are small, numerous, and inbound: receipts arrive without the account's
/// action, so the operation ceiling is higher than the payments plane's while
/// the value ceiling is an order of magnitude lower — 2,100,000 sat/day is a
/// tipping pattern no individual approaches, and a rail would clear it instantly.
const TIER1_ZAPS: FeaturePolicy = FeaturePolicy {
    availability: Availability::Limit,
    operations: WindowedBounds {
        per_day: Some(2_000),
        per_week: None,
        per_month: Some(20_000),
    },
    counterparties: WindowedBounds {
        per_day: Some(1_000),
        per_week: None,
        per_month: Some(10_000),
    },
    volume: WindowedBounds {
        per_day: Some(2_100_000 * MSAT_PER_SAT),
        per_week: None,
        per_month: Some(21_000_000 * MSAT_PER_SAT),
    },
    per_operation_max: None,
};

/// Tier-1 constants for `p2p-share`.
///
/// - **counterparties** 50/week and 100/month — the structural gate. Family,
///   friends, a class, a team: personal sharing reaches tens of people over a
///   month, never hundreds. The month bound binds harder than the week bound
///   (4 × 50 > 100), which is deliberate: it allows a burst (moving a household
///   onto a shared album) without allowing a sustained fan-out. **Doubled from
///   25/week · 50/month when OQ-4 was ratified (user directive 2026-08-11).**
///   The asymmetry that decided it: tier 1 is a ceiling *no* tier can raise —
///   the meet only ever tightens — so a too-tight constant binds real users
///   with no runtime remedy and needs a release to fix, while a too-generous
///   one is tightened by any of the five tiers below. Err generous here.
/// - **operations** 500/day and 5,000/month: a photo-library migration is many
///   share operations in one sitting and must not trip.
/// - **volume** 1 TB/day and 10 TB/month: the "hundreds of GB between spouses"
///   case sits an order of magnitude below this.
/// - **per-operation max** unset: one legitimate file may be arbitrarily large.
const TIER1_P2P_SHARE: FeaturePolicy = FeaturePolicy {
    availability: Availability::Limit,
    operations: WindowedBounds {
        per_day: Some(500),
        per_week: None,
        per_month: Some(5_000),
    },
    counterparties: WindowedBounds {
        per_day: None,
        per_week: Some(50),
        per_month: Some(100),
    },
    volume: WindowedBounds {
        per_day: Some(1_000_000_000_000),
        per_week: None,
        per_month: Some(10_000_000_000_000),
    },
    per_operation_max: None,
};

/// The registry — one catalog, consumed by the nest and all 7 apps.
static REGISTRY: [FeatureEntry; 3] = [
    FeatureEntry {
        feature: GatedFeature::Payments,
        dimensions: &[
            QuotaDimension::Operations,
            QuotaDimension::Counterparties,
            QuotaDimension::Volume,
        ],
        unit: MagnitudeUnit::Millisats,
        superset: None,
        tier1: TIER1_PAYMENTS,
        gate_surfaces: &[
            SURFACE_PAYMENTS_PROVIDER_CONFIGURE,
            SURFACE_PAYMENTS_TIER_PRICE,
            SURFACE_PAYMENTS_PAYWALL_DESIGNATE,
            SURFACE_PAYMENTS_CLAIM_MINT,
            SURFACE_PAYMENTS_CLAIM_REDEEM,
            SURFACE_PAYMENTS_UNLOCK_PURCHASE,
            SURFACE_PAYMENTS_TIP_SEND,
        ],
    },
    FeatureEntry {
        feature: GatedFeature::Zaps,
        dimensions: &[
            QuotaDimension::Operations,
            QuotaDimension::Counterparties,
            QuotaDimension::Volume,
        ],
        unit: MagnitudeUnit::Millisats,
        superset: Some(GatedFeature::Payments),
        tier1: TIER1_ZAPS,
        gate_surfaces: &[SURFACE_ZAPS_SIGNER_DESIGNATE, SURFACE_ZAPS_RECEIPT_INGEST],
    },
    FeatureEntry {
        feature: GatedFeature::P2pShare,
        dimensions: &[
            QuotaDimension::Operations,
            QuotaDimension::Counterparties,
            QuotaDimension::Volume,
        ],
        unit: MagnitudeUnit::Bytes,
        superset: None,
        tier1: TIER1_P2P_SHARE,
        gate_surfaces: &[SURFACE_P2P_SHARE_MEMBER_ADMIT, SURFACE_P2P_SHARE_TRANSFER],
    },
];

/// The whole registry, in canonical order.
pub fn registry() -> &'static [FeatureEntry] {
    &REGISTRY
}

/// The stand-in [`entry`] answers for [`GatedFeature::Unknown`]: denied at
/// tier 1, no dimension declared, no gate surface. Not in [`registry`] — it is
/// the restrictive reading of a feature this build cannot name, so a path that
/// reaches it by mistake gates nothing it knows and allows nothing.
static UNKNOWN_ENTRY: FeatureEntry = FeatureEntry {
    feature: GatedFeature::Unknown,
    dimensions: &[],
    unit: MagnitudeUnit::Bytes,
    superset: None,
    tier1: FeaturePolicy::DENIED,
    gate_surfaces: &[],
};

/// The registry entry for a feature. Total — every known [`GatedFeature`] has
/// a row, which is what makes the catalog and the enum one thing rather than
/// two that can drift; the open arm answers the deny-everything stand-in.
pub fn entry(feature: GatedFeature) -> &'static FeatureEntry {
    if !feature.is_known() {
        return &UNKNOWN_ENTRY;
    }
    REGISTRY
        .iter()
        .find(|e| e.feature == feature)
        .expect("every GatedFeature has a registry entry (pinned by registry_is_total)")
}

/// Compose the effective policy for one (account, feature) pair.
///
/// `authored` carries what each tier actually wrote, in any order — the meet is
/// order-free, so callers need not sort. Tier 1's constants are folded in from
/// the registry automatically and need not (and should not) be passed.
///
/// **The subset edge.** For a subset member (`zaps` under `payments`), the
/// superset's *availability* also applies: denying the payments plane denies the
/// zap surface with it, exactly as excising `payments` excises `zaps`
/// (§ Charter members). Quota *bounds* do not inherit — each member's
/// operations are counted against its own dimensions, so a superset's bound
/// never silently consumes a subset's budget. Pass the superset's authored
/// policies as `superset_authored`; `None` when the feature has no superset or
/// the caller has already established the superset allows.
pub fn effective_policy(
    feature: GatedFeature,
    authored: &[(RuleTier, FeaturePolicy)],
    superset_authored: &[(RuleTier, FeaturePolicy)],
) -> EffectivePolicy {
    let entry = entry(feature);
    let mut effective = EffectivePolicy {
        feature,
        denied_by: None,
        operations: EffectiveBounds::UNSET,
        counterparties: EffectiveBounds::UNSET,
        volume: EffectiveBounds::UNSET,
        per_operation_max: None,
    };

    let tier1 = [(RuleTier::Structural, entry.tier1.clone())];
    for (tier, policy) in tier1.iter().chain(authored.iter()) {
        if policy.availability.denies() {
            effective.denied_by = Some(match effective.denied_by {
                Some(existing) => existing.min(*tier),
                None => *tier,
            });
        }
        for dimension in [
            QuotaDimension::Operations,
            QuotaDimension::Counterparties,
            QuotaDimension::Volume,
        ] {
            // A bound on a dimension the registry does not declare is
            // unrepresentable; drop it rather than enforce it, so a malformed
            // document can never invent a gate.
            if !entry.declares(dimension) {
                continue;
            }
            let authored_bounds = *policy.bounds(dimension);
            let target = match dimension {
                QuotaDimension::Operations => &mut effective.operations,
                QuotaDimension::Counterparties => &mut effective.counterparties,
                QuotaDimension::Volume => &mut effective.volume,
            };
            for window in Window::ALL {
                if let Some(limit) = authored_bounds.get(window) {
                    let cell = target.cell_mut(window);
                    *cell = meet_cell(*cell, Some(BoundSource { limit, tier: *tier }));
                }
            }
        }
        if let Some(limit) = policy.per_operation_max
            && entry.declares(QuotaDimension::Volume)
        {
            effective.per_operation_max = meet_cell(
                effective.per_operation_max,
                Some(BoundSource { limit, tier: *tier }),
            );
        }
    }

    // The subset edge: availability only, and only for a feature that actually
    // has a superset. Gated on the registry rather than on the caller passing
    // an empty slice — a feature with no superset has no edge to inherit along,
    // so a stray argument must not be able to invent one.
    if let Some(superset) = entry.superset {
        for (tier, policy) in superset_authored {
            if policy.availability.denies() {
                effective.denied_by = Some(match effective.denied_by {
                    Some(existing) => existing.min(*tier),
                    None => *tier,
                });
            }
        }
        if entry_denies_at_tier1(superset) {
            effective.denied_by = Some(match effective.denied_by {
                Some(existing) => existing.min(RuleTier::Structural),
                None => RuleTier::Structural,
            });
        }
    }

    effective
}

fn entry_denies_at_tier1(feature: GatedFeature) -> bool {
    entry(feature).tier1.availability.denies()
}

/// **The one shared verdict function** (§ Evaluation points): may this operation
/// proceed under this effective policy and these observed counters?
///
/// Pure and total — the nest composes it at each gate surface, so the gate holds
/// against non-conforming and version-skewed clients, and a client renders the
/// same answer for the courtesy layer without a second implementation.
///
/// Evaluation order is deterministic so a refusal is reproducible: availability
/// first, then dimensions in registry-declaration order, windows tightest-first,
/// and the per-operation cap last. The first violation found is returned —
/// there is no value in enumerating every bound a distribution-scale operation
/// trips.
pub fn feature_verdict(
    entry: &FeatureEntry,
    policy: &EffectivePolicy,
    usage: &UsageCounters,
    op: &GateOp,
) -> FeatureVerdict {
    // Each of the three arguments names a feature, so a caller can mis-pair
    // them. That is a programming error, but the safe handling of one in a
    // *gate* is refusal — never evaluating some other feature's quotas by
    // accident, and never a panic either: this function stands between an
    // operation and its limits, so it stays total (§ Fail posture: the gate
    // fails closed for the operation, never open). A `debug_assert` was the
    // obvious reflex here and is the wrong tool twice over — it would be loud
    // in tests and silent in production, and it would make the safe path
    // untestable by panicking before reaching it.
    if entry.feature != op.feature || entry.feature != policy.feature {
        return FeatureVerdict::Deny {
            tier: RuleTier::Structural,
        };
    }

    if let Some(tier) = policy.denied_by {
        return FeatureVerdict::Deny { tier };
    }

    for &dimension in entry.dimensions {
        let delta = op.delta(dimension);
        let observed = usage.get(dimension);
        let bounds = policy.bounds(dimension);
        for window in Window::ALL {
            let Some(bound) = bounds.get(window) else {
                continue;
            };
            let already = observed.get(window);
            // Saturating: a counter at u64::MAX is a broken counter, and the
            // fail-closed posture for an unanswerable counter is refusal
            // (§ Fail posture: counters unavailable → fails closed).
            if already.saturating_add(delta) > bound.limit {
                return FeatureVerdict::OverQuota {
                    dimension,
                    window: Some(window),
                    limit: bound.limit,
                    observed: already,
                    tier: bound.tier,
                };
            }
        }
    }

    if let Some(cap) = policy.per_operation_max
        && op.magnitude > cap.limit
    {
        return FeatureVerdict::OverQuota {
            dimension: QuotaDimension::Volume,
            window: None,
            limit: cap.limit,
            observed: op.magnitude,
            tier: cap.tier,
        };
    }

    FeatureVerdict::Allow
}

/// Test-fixture builders shared by every consumer testing against
/// [`FeaturePolicy`] — `pub` (not nested in `mod tests`) so a downstream
/// crate's own unit tests AND its separate `tests/` integration binaries
/// both reach it (an integration-test crate can't see another crate's
/// `#[cfg(test)]`-private items; `debug_assertions` covers that case the way
/// a bare `#[cfg(test)]` cannot). `bins/fauna-nest`'s `db::feature_gate` unit
/// tests and its `tests/conformance_feature_gate.rs` both hand-rolled this
/// byte-identically before this lift — found by the same-name arm of the
/// dev-fleet near-duplicate-function scanner.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub mod test_support {
    use super::{Availability, FeaturePolicy, QuotaDimension, Window, WindowedBounds};

    /// A policy that bounds one dimension at one window and says nothing
    /// else, so a test names exactly the bound it is exercising.
    pub fn bound_at(dimension: QuotaDimension, window: Window, limit: u64) -> FeaturePolicy {
        let bounds = WindowedBounds::UNSET.tightened_with(window, limit);
        let mut policy = FeaturePolicy {
            availability: Availability::Limit,
            ..FeaturePolicy::NO_OPINION
        };
        match dimension {
            QuotaDimension::Operations => policy.operations = bounds,
            QuotaDimension::Counterparties => policy.counterparties = bounds,
            QuotaDimension::Volume => policy.volume = bounds,
        }
        policy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_usage() -> UsageCounters {
        UsageCounters::default()
    }

    /// **The at-rest vocabulary, pinned literally.**
    ///
    /// These strings key quota buckets in the nest's `feature_*` tables and
    /// ride the wire to every app. A rename of a Rust *variant* is free; a
    /// change to one of these *strings* silently re-keys existing buckets and
    /// hands every account a fresh quota (`db::feature_gate::dimension_key`'s
    /// own comment says so). Until 2026-08-23 there were four hand-written
    /// copies of the dimension map alone and NOTHING held them to each other,
    /// so this is the guard that was missing rather than a restatement of
    /// `as_str`: it fails on a changed literal, and a new variant will not
    /// compile past the exhaustive matches below without being spelled here.
    #[test]
    fn the_at_rest_vocabulary_is_exactly_this() {
        for (dimension, expected) in [
            (QuotaDimension::Operations, "operations"),
            (QuotaDimension::Counterparties, "counterparties"),
            (QuotaDimension::Volume, "volume"),
        ] {
            assert_eq!(dimension.as_str(), expected);
        }
        for (window, expected) in [
            (Window::Day, "day"),
            (Window::Week, "week"),
            (Window::Month, "month"),
        ] {
            assert_eq!(window.as_str(), expected);
        }
        for (tier, expected) in [
            (RuleTier::Structural, "structural"),
            (RuleTier::Region, "region"),
            (RuleTier::Admin, "admin"),
            (RuleTier::Guardian, "guardian"),
            // `self`, the serde spelling too (`#[serde(rename = "self")]`) —
            // see `RuleTier::as_str`.
            (RuleTier::SelfImposed, "self"),
        ] {
            assert_eq!(tier.as_str(), expected);
        }

        // Exhaustiveness: a new variant fails to compile here, which is the
        // point at which someone must decide its at-rest spelling rather than
        // discover it from a bucket that stopped counting.
        fn _dimension_is_exhaustive(d: QuotaDimension) -> &'static str {
            match d {
                QuotaDimension::Operations
                | QuotaDimension::Counterparties
                | QuotaDimension::Volume => d.as_str(),
            }
        }
        fn _window_is_exhaustive(w: Window) -> &'static str {
            match w {
                Window::Day | Window::Week | Window::Month => w.as_str(),
            }
        }
        fn _tier_is_exhaustive(t: RuleTier) -> &'static str {
            match t {
                RuleTier::Structural
                | RuleTier::Region
                | RuleTier::Admin
                | RuleTier::Guardian
                | RuleTier::SelfImposed
                | RuleTier::Unknown => t.as_str(),
            }
        }
    }

    /// Within each vocabulary every spelling is distinct — two variants
    /// sharing a key would merge two quota buckets into one, which reads as a
    /// generous limit rather than as a bug.
    #[test]
    fn no_two_variants_share_an_at_rest_spelling() {
        use std::collections::BTreeSet;
        let dimensions: BTreeSet<&str> = [
            QuotaDimension::Operations,
            QuotaDimension::Counterparties,
            QuotaDimension::Volume,
        ]
        .iter()
        .map(|d| d.as_str())
        .collect();
        assert_eq!(dimensions.len(), 3);

        let windows: BTreeSet<&str> = Window::ALL.iter().map(|w| w.as_str()).collect();
        assert_eq!(windows.len(), Window::ALL.len());

        let tiers: BTreeSet<&str> = [
            RuleTier::Structural,
            RuleTier::Region,
            RuleTier::Admin,
            RuleTier::Guardian,
            RuleTier::SelfImposed,
        ]
        .iter()
        .map(|t| t.as_str())
        .collect();
        assert_eq!(tiers.len(), 5);
    }

    fn op(feature: GatedFeature, surface: &'static str) -> GateOp {
        GateOp {
            feature,
            surface,
            new_counterparties: 0,
            magnitude: 0,
        }
    }

    // -- the registry -------------------------------------------------------

    #[test]
    fn registry_is_total_and_ordered() {
        assert_eq!(REGISTRY.len(), GatedFeature::ALL.len());
        for (row, feature) in REGISTRY.iter().zip(GatedFeature::ALL) {
            assert_eq!(
                row.feature, feature,
                "registry order matches GatedFeature::ALL"
            );
        }
        for feature in GatedFeature::ALL {
            assert_eq!(entry(feature).feature, feature);
        }
    }

    #[test]
    fn feature_keys_round_trip_and_match_the_ratified_strings() {
        assert_eq!(GatedFeature::Payments.as_str(), "payments");
        assert_eq!(GatedFeature::Zaps.as_str(), "zaps");
        assert_eq!(GatedFeature::P2pShare.as_str(), "p2p-share");
        for feature in GatedFeature::ALL {
            assert_eq!(GatedFeature::from_key(feature.as_str()), Some(feature));
        }
        assert_eq!(GatedFeature::from_key("messaging"), None);
    }

    /// The key is the cargo feature name and the capability token, so a rename
    /// here silently un-excises a build. Pinned as literals.
    #[test]
    fn serde_uses_the_stable_keys() {
        let json = serde_json::to_string(&GatedFeature::P2pShare).unwrap();
        assert_eq!(json, "\"p2p-share\"");
        let back: GatedFeature = serde_json::from_str("\"zaps\"").unwrap();
        assert_eq!(back, GatedFeature::Zaps);
    }

    #[test]
    fn zaps_is_a_subset_member_of_payments() {
        assert_eq!(
            entry(GatedFeature::Zaps).superset,
            Some(GatedFeature::Payments)
        );
        assert_eq!(entry(GatedFeature::Payments).superset, None);
        assert_eq!(entry(GatedFeature::P2pShare).superset, None);
    }

    /// § Composition: "tier 1's structural constants are always present, so the
    /// meet is never unbounded on a dimension the registry declares".
    #[test]
    fn tier1_bounds_every_declared_dimension() {
        for row in registry() {
            for &dimension in row.dimensions {
                assert!(
                    !row.tier1.bounds(dimension).is_unset(),
                    "{} declares {dimension:?} but tier 1 leaves it unbounded",
                    row.feature.as_str()
                );
            }
        }
    }

    #[test]
    fn tier1_constants_validate_against_their_own_entry() {
        for row in registry() {
            row.tier1
                .validate(row)
                .expect("tier 1 bounds only declared dimensions");
        }
    }

    #[test]
    fn gate_surfaces_are_namespaced_by_their_feature_key() {
        for row in registry() {
            for surface in row.gate_surfaces {
                assert!(
                    surface.starts_with(&format!("{}.", row.feature.as_str())),
                    "surface {surface} is not namespaced by {}",
                    row.feature.as_str()
                );
            }
        }
    }

    // -- the quota grammar --------------------------------------------------

    #[test]
    fn a_bound_on_an_undeclared_dimension_is_rejected() {
        // Construct an entry that declares only Operations, and bound Volume.
        let narrow = FeatureEntry {
            dimensions: &[QuotaDimension::Operations],
            ..entry(GatedFeature::Payments).clone()
        };
        let policy = FeaturePolicy {
            volume: WindowedBounds::at(Window::Day, 5),
            ..FeaturePolicy::NO_OPINION
        };
        assert_eq!(
            policy.validate(&narrow),
            Err(UndeclaredDimension {
                feature: GatedFeature::Payments,
                dimension: QuotaDimension::Volume,
            })
        );
        // The per-operation cap is a facet of Volume and follows it.
        let capped = FeaturePolicy {
            per_operation_max: Some(1),
            ..FeaturePolicy::NO_OPINION
        };
        assert!(capped.validate(&narrow).is_err());
    }

    #[test]
    fn windows_sum_the_expected_day_buckets() {
        assert_eq!(Window::Day.days(), 1);
        assert_eq!(Window::Week.days(), 7);
        assert_eq!(Window::Month.days(), 30);
    }

    // -- the meet -----------------------------------------------------------

    #[test]
    fn any_tier_deny_denies_and_names_the_outermost_denier() {
        let effective = effective_policy(
            GatedFeature::P2pShare,
            &[
                (RuleTier::SelfImposed, FeaturePolicy::DENIED),
                (RuleTier::Region, FeaturePolicy::DENIED),
            ],
            &[],
        );
        assert_eq!(effective.denied_by, Some(RuleTier::Region));
        assert_eq!(effective.availability(), Availability::Deny);
    }

    #[test]
    fn each_cell_takes_the_minimum_and_keeps_its_source_tier() {
        let admin = FeaturePolicy {
            availability: Availability::Limit,
            counterparties: WindowedBounds::at(Window::Month, 30),
            ..FeaturePolicy::NO_OPINION
        };
        let guardian = FeaturePolicy {
            availability: Availability::Limit,
            counterparties: WindowedBounds::at(Window::Month, 4),
            ..FeaturePolicy::NO_OPINION
        };
        let effective = effective_policy(
            GatedFeature::P2pShare,
            &[(RuleTier::Admin, admin), (RuleTier::Guardian, guardian)],
            &[],
        );
        assert_eq!(
            effective.counterparties.get(Window::Month),
            Some(BoundSource {
                limit: 4,
                tier: RuleTier::Guardian
            })
        );
        // Tier 1's week bound survives untouched — neither tier had an opinion.
        // Derived from the registry, not restated: this test's subject is the
        // meet, so it must not also be a pin on the constant's value.
        assert_eq!(
            effective.counterparties.get(Window::Week),
            Some(BoundSource {
                limit: entry(GatedFeature::P2pShare)
                    .tier1
                    .counterparties
                    .get(Window::Week)
                    .expect("tier 1 bounds p2p-share counterparties per week"),
                tier: RuleTier::Structural
            })
        );
    }

    /// An unset dimension is "no opinion", **not** "unlimited past tier 1".
    #[test]
    fn a_silent_tier_never_relaxes_tier_one() {
        let permissive = FeaturePolicy {
            availability: Availability::Allow,
            ..FeaturePolicy::NO_OPINION
        };
        let effective = effective_policy(
            GatedFeature::P2pShare,
            &[(RuleTier::Admin, permissive)],
            &[],
        );
        // Derived from the registry for the same reason as above: the claim is
        // "tier 1 survives a silent tier", not "tier 1 is any particular number".
        assert_eq!(
            effective.counterparties.get(Window::Month).map(|b| b.limit),
            entry(GatedFeature::P2pShare)
                .tier1
                .counterparties
                .get(Window::Month)
        );
    }

    /// No tier can relax below another — the meet only ever tightens, which is
    /// what makes "may an admin relax past region policy?" unrepresentable.
    #[test]
    fn a_looser_tier_cannot_relax_a_tighter_one() {
        let region = FeaturePolicy {
            availability: Availability::Limit,
            counterparties: WindowedBounds::at(Window::Month, 5),
            ..FeaturePolicy::NO_OPINION
        };
        let admin = FeaturePolicy {
            availability: Availability::Limit,
            counterparties: WindowedBounds::at(Window::Month, 5_000),
            ..FeaturePolicy::NO_OPINION
        };
        let effective = effective_policy(
            GatedFeature::P2pShare,
            &[(RuleTier::Region, region), (RuleTier::Admin, admin)],
            &[],
        );
        assert_eq!(
            effective.counterparties.get(Window::Month).map(|b| b.limit),
            Some(5)
        );
    }

    #[test]
    fn the_meet_is_order_free() {
        let a = (
            RuleTier::Region,
            FeaturePolicy {
                availability: Availability::Limit,
                operations: WindowedBounds::at(Window::Day, 9),
                ..FeaturePolicy::NO_OPINION
            },
        );
        let b = (
            RuleTier::Guardian,
            FeaturePolicy {
                availability: Availability::Limit,
                counterparties: WindowedBounds::at(Window::Week, 3),
                ..FeaturePolicy::NO_OPINION
            },
        );
        let forward = effective_policy(GatedFeature::P2pShare, &[a.clone(), b.clone()], &[]);
        let backward = effective_policy(GatedFeature::P2pShare, &[b, a], &[]);
        assert_eq!(forward, backward);
    }

    #[test]
    fn a_tie_names_the_outermost_scope_deterministically() {
        let same = FeaturePolicy {
            availability: Availability::Limit,
            operations: WindowedBounds::at(Window::Day, 7),
            ..FeaturePolicy::NO_OPINION
        };
        let effective = effective_policy(
            GatedFeature::P2pShare,
            &[
                (RuleTier::SelfImposed, same.clone()),
                (RuleTier::Region, same),
            ],
            &[],
        );
        assert_eq!(
            effective.operations.get(Window::Day),
            Some(BoundSource {
                limit: 7,
                tier: RuleTier::Region
            })
        );
    }

    #[test]
    fn availability_is_derived_from_what_actually_binds() {
        // Every registry member is bounded at tier 1, so the effective
        // availability of an unrestricted account is Limit, never Allow.
        let effective = effective_policy(GatedFeature::Payments, &[], &[]);
        assert_eq!(effective.availability(), Availability::Limit);
        assert_eq!(effective.denied_by, None);
    }

    /// Denying the superset denies the subset member with it — the runtime twin
    /// of "excising `payments` excises `zaps` by construction".
    #[test]
    fn denying_payments_denies_zaps() {
        let effective = effective_policy(
            GatedFeature::Zaps,
            &[],
            &[(RuleTier::Region, FeaturePolicy::DENIED)],
        );
        assert_eq!(effective.denied_by, Some(RuleTier::Region));
    }

    /// …but the reverse does not hold: excising or denying the subset alone
    /// keeps provider-rail payments, which is the finer-grained hatch the Damus
    /// precedent shows a store actually demands.
    #[test]
    fn denying_zaps_leaves_payments_alone() {
        let effective = effective_policy(
            GatedFeature::Payments,
            &[],
            &[(RuleTier::Region, FeaturePolicy::DENIED)],
        );
        // `payments` has no superset, so a superset-authored deny is not
        // applicable to it and must not leak in from the caller.
        assert_eq!(effective.denied_by, None);
    }

    /// Quota bounds do **not** inherit along the subset edge: a zap must not
    /// silently consume the payments plane's budget.
    #[test]
    fn subset_inherits_availability_but_not_bounds() {
        let tight_superset = FeaturePolicy {
            availability: Availability::Limit,
            operations: WindowedBounds::at(Window::Day, 1),
            ..FeaturePolicy::NO_OPINION
        };
        let effective = effective_policy(
            GatedFeature::Zaps,
            &[],
            &[(RuleTier::Region, tight_superset)],
        );
        assert_eq!(effective.denied_by, None);
        assert_eq!(
            effective.operations.get(Window::Day).map(|b| b.limit),
            Some(2_000),
            "zaps keeps its own tier-1 operation ceiling"
        );
    }

    // -- the verdict --------------------------------------------------------

    #[test]
    fn an_ordinary_operation_is_allowed_under_tier_one_alone() {
        let entry = entry(GatedFeature::P2pShare);
        let policy = effective_policy(GatedFeature::P2pShare, &[], &[]);
        let verdict = feature_verdict(
            entry,
            &policy,
            &no_usage(),
            &GateOp {
                new_counterparties: 2,
                magnitude: 400_000_000_000, // 400 GB in one share
                ..op(GatedFeature::P2pShare, SURFACE_P2P_SHARE_MEMBER_ADMIT)
            },
        );
        assert_eq!(verdict, FeatureVerdict::Allow);
    }

    /// The charter's own sizing statement, as a test: the spouses moving
    /// hundreds of GB between their own devices must never feel a gate.
    #[test]
    fn the_spouses_moving_hundreds_of_gigabytes_never_feel_a_gate() {
        let entry = entry(GatedFeature::P2pShare);
        let policy = effective_policy(GatedFeature::P2pShare, &[], &[]);
        let usage = UsageCounters {
            operations: WindowCounts {
                day: 120,
                week: 400,
                month: 1_500,
            },
            counterparties: WindowCounts {
                day: 1,
                week: 1,
                month: 2,
            },
            volume: WindowCounts {
                day: 300_000_000_000,
                week: 900_000_000_000,
                month: 3_000_000_000_000,
            },
        };
        let verdict = feature_verdict(
            entry,
            &policy,
            &usage,
            &GateOp {
                new_counterparties: 0,
                magnitude: 50_000_000_000,
                ..op(GatedFeature::P2pShare, SURFACE_P2P_SHARE_TRANSFER)
            },
        );
        assert_eq!(verdict, FeatureVerdict::Allow);
    }

    /// …and the other half of the same statement: sharing to hundreds of
    /// strangers is structurally impossible, with no rule-setter involved.
    ///
    /// Unlike the meet tests above, this one **pins the ratified values on
    /// purpose** — its subject *is* the sizing (§ Open questions OQ-4), so an
    /// unratified edit to `TIER1_P2P_SHARE` must fail here and send whoever
    /// made it back to the table. The usage fixture sits just under both
    /// bounds, which is what proves the refusal comes from the fan-out rather
    /// than from a pre-existing overage.
    #[test]
    fn fan_out_to_hundreds_of_strangers_is_structurally_refused() {
        let entry = entry(GatedFeature::P2pShare);
        let policy = effective_policy(GatedFeature::P2pShare, &[], &[]);
        let usage = UsageCounters {
            counterparties: WindowCounts {
                day: 40,
                week: 48,
                month: 80,
            },
            ..UsageCounters::default()
        };
        let verdict = feature_verdict(
            entry,
            &policy,
            &usage,
            &GateOp {
                new_counterparties: 200,
                ..op(GatedFeature::P2pShare, SURFACE_P2P_SHARE_MEMBER_ADMIT)
            },
        );
        assert_eq!(
            verdict,
            FeatureVerdict::OverQuota {
                dimension: QuotaDimension::Counterparties,
                window: Some(Window::Week),
                limit: 50,
                observed: 48,
                tier: RuleTier::Structural,
            }
        );
    }

    #[test]
    fn a_deny_short_circuits_before_any_quota_is_consulted() {
        let entry = entry(GatedFeature::Payments);
        let policy = effective_policy(
            GatedFeature::Payments,
            &[(RuleTier::Admin, FeaturePolicy::DENIED)],
            &[],
        );
        let verdict = feature_verdict(
            entry,
            &policy,
            &no_usage(),
            &op(GatedFeature::Payments, SURFACE_PAYMENTS_CLAIM_MINT),
        );
        assert_eq!(
            verdict,
            FeatureVerdict::Deny {
                tier: RuleTier::Admin
            }
        );
        assert_eq!(verdict.binding_tier(), Some(RuleTier::Admin));
    }

    /// A refusal must name the tier that bound it, not merely that it was
    /// bound — boundary 4, and what the Dim-4 typed refusal carries.
    #[test]
    fn an_over_quota_refusal_names_the_binding_tier() {
        let guardian = FeaturePolicy {
            availability: Availability::Limit,
            operations: WindowedBounds::at(Window::Day, 3),
            ..FeaturePolicy::NO_OPINION
        };
        let entry = entry(GatedFeature::Payments);
        let policy = effective_policy(
            GatedFeature::Payments,
            &[(RuleTier::Guardian, guardian)],
            &[],
        );
        let usage = UsageCounters {
            operations: WindowCounts {
                day: 3,
                week: 3,
                month: 3,
            },
            ..UsageCounters::default()
        };
        let verdict = feature_verdict(
            entry,
            &policy,
            &usage,
            &op(GatedFeature::Payments, SURFACE_PAYMENTS_CLAIM_MINT),
        );
        assert_eq!(
            verdict,
            FeatureVerdict::OverQuota {
                dimension: QuotaDimension::Operations,
                window: Some(Window::Day),
                limit: 3,
                observed: 3,
                tier: RuleTier::Guardian,
            }
        );
    }

    /// The bound is a ceiling on the *post-operation* total, so the operation
    /// that would reach the limit exactly is still allowed and the next one is
    /// not. Off-by-one here is the difference between "50 files a week" meaning
    /// 50 and meaning 49.
    #[test]
    fn the_limit_is_inclusive_of_the_operation_that_reaches_it() {
        let entry = entry(GatedFeature::Payments);
        let policy = effective_policy(
            GatedFeature::Payments,
            &[(
                RuleTier::Admin,
                FeaturePolicy {
                    availability: Availability::Limit,
                    operations: WindowedBounds::at(Window::Day, 10),
                    ..FeaturePolicy::NO_OPINION
                },
            )],
            &[],
        );
        let at_nine = UsageCounters {
            operations: WindowCounts {
                day: 9,
                week: 9,
                month: 9,
            },
            ..UsageCounters::default()
        };
        assert_eq!(
            feature_verdict(
                entry,
                &policy,
                &at_nine,
                &op(GatedFeature::Payments, SURFACE_PAYMENTS_CLAIM_MINT)
            ),
            FeatureVerdict::Allow,
            "the tenth operation is the tenth, not the eleventh"
        );
        let at_ten = UsageCounters {
            operations: WindowCounts {
                day: 10,
                week: 10,
                month: 10,
            },
            ..UsageCounters::default()
        };
        assert!(
            !feature_verdict(
                entry,
                &policy,
                &at_ten,
                &op(GatedFeature::Payments, SURFACE_PAYMENTS_CLAIM_MINT)
            )
            .is_allowed()
        );
    }

    #[test]
    fn the_tightest_window_is_the_one_reported() {
        let entry = entry(GatedFeature::Payments);
        let policy = effective_policy(
            GatedFeature::Payments,
            &[(
                RuleTier::Admin,
                FeaturePolicy {
                    availability: Availability::Limit,
                    operations: WindowedBounds {
                        per_day: Some(5),
                        per_week: Some(6),
                        per_month: Some(7),
                    },
                    ..FeaturePolicy::NO_OPINION
                },
            )],
            &[],
        );
        let usage = UsageCounters {
            operations: WindowCounts {
                day: 5,
                week: 6,
                month: 7,
            },
            ..UsageCounters::default()
        };
        let FeatureVerdict::OverQuota { window, .. } = feature_verdict(
            entry,
            &policy,
            &usage,
            &op(GatedFeature::Payments, SURFACE_PAYMENTS_CLAIM_MINT),
        ) else {
            panic!("expected an over-quota verdict");
        };
        assert_eq!(window, Some(Window::Day));
    }

    #[test]
    fn a_repeat_counterparty_does_not_consume_the_fan_out_bound() {
        let entry = entry(GatedFeature::P2pShare);
        let policy = effective_policy(GatedFeature::P2pShare, &[], &[]);
        let usage = UsageCounters {
            counterparties: WindowCounts {
                day: 25,
                week: 25,
                month: 50,
            },
            ..UsageCounters::default()
        };
        // Already at the counterparty ceiling, but sharing again with someone
        // already inside the window costs nothing on that dimension.
        let verdict = feature_verdict(
            entry,
            &policy,
            &usage,
            &GateOp {
                new_counterparties: 0,
                magnitude: 1_000,
                ..op(GatedFeature::P2pShare, SURFACE_P2P_SHARE_TRANSFER)
            },
        );
        assert_eq!(verdict, FeatureVerdict::Allow);
    }

    #[test]
    fn a_per_operation_cap_refuses_without_a_window() {
        let entry = entry(GatedFeature::P2pShare);
        let policy = effective_policy(
            GatedFeature::P2pShare,
            &[(
                RuleTier::Guardian,
                FeaturePolicy {
                    availability: Availability::Limit,
                    per_operation_max: Some(1_000),
                    ..FeaturePolicy::NO_OPINION
                },
            )],
            &[],
        );
        let verdict = feature_verdict(
            entry,
            &policy,
            &no_usage(),
            &GateOp {
                magnitude: 1_001,
                ..op(GatedFeature::P2pShare, SURFACE_P2P_SHARE_TRANSFER)
            },
        );
        assert_eq!(
            verdict,
            FeatureVerdict::OverQuota {
                dimension: QuotaDimension::Volume,
                window: None,
                limit: 1_000,
                observed: 1_001,
                tier: RuleTier::Guardian,
            }
        );
    }

    /// A counter that has overflowed is a broken counter; the posture for an
    /// unanswerable counter is refusal, never a wrap-around that reads as zero
    /// (§ Fail posture).
    #[test]
    fn a_saturated_counter_fails_closed() {
        let entry = entry(GatedFeature::Payments);
        let policy = effective_policy(GatedFeature::Payments, &[], &[]);
        let usage = UsageCounters {
            operations: WindowCounts {
                day: u64::MAX,
                week: u64::MAX,
                month: u64::MAX,
            },
            ..UsageCounters::default()
        };
        assert!(
            !feature_verdict(
                entry,
                &policy,
                &usage,
                &op(GatedFeature::Payments, SURFACE_PAYMENTS_CLAIM_MINT)
            )
            .is_allowed()
        );
    }

    /// Mis-pairing the arguments is a programming error, and a gate's safe
    /// handling of one is refusal rather than evaluating the wrong feature's
    /// quotas. Checked in release too, not only under `debug_assert`.
    #[test]
    fn a_mismatched_entry_fails_closed() {
        let payments = entry(GatedFeature::Payments);
        let p2p_policy = effective_policy(GatedFeature::P2pShare, &[], &[]);
        let verdict = feature_verdict(
            payments,
            &p2p_policy,
            &no_usage(),
            &op(GatedFeature::Payments, SURFACE_PAYMENTS_CLAIM_MINT),
        );
        assert_eq!(
            verdict,
            FeatureVerdict::Deny {
                tier: RuleTier::Structural
            }
        );
    }

    // -- the policy document ------------------------------------------------

    /// Absent fields mean "no opinion", which is what makes the document
    /// additive-everywhere and lets the guardian tier ride the family policy
    /// update with absent-means-unchanged.
    #[test]
    fn an_empty_document_deserializes_to_no_opinion() {
        let policy: FeaturePolicy = serde_json::from_str("{}").unwrap();
        assert_eq!(policy, FeaturePolicy::NO_OPINION);
    }

    /// A document written by a newer peer that carries a field this build has
    /// never heard of must still parse — additive-everywhere, both directions.
    #[test]
    fn an_unknown_field_from_a_newer_peer_still_parses() {
        let policy: FeaturePolicy =
            serde_json::from_str(r#"{"availability":"deny","future_dimension":{"per_day":3}}"#)
                .unwrap();
        assert_eq!(policy.availability, Availability::Deny);
    }
}

/// The open arms of `transport.md` § Rule 3 in full, each proven against a
/// test-only twin of the enum standing in for a NEWER writer — one extra
/// variant the real type has never heard of (the `OlderPeerFilterAction`
/// pattern, run the other way).
#[cfg(test)]
mod unknown_arm_tests {
    use super::*;
    use crate::encoding::{canonical_decode, canonical_encode};

    /// The newer registry: today's members plus one this build lacks.
    #[derive(Serialize)]
    enum NewerGatedFeature {
        #[serde(rename = "payments")]
        Payments,
        #[serde(rename = "dowsing")]
        Dowsing,
    }

    /// The newer tier set: today's tiers plus one this build cannot name.
    #[derive(Serialize)]
    #[serde(rename_all = "snake_case")]
    enum NewerRuleTier {
        Admin,
        Council,
    }

    /// The newer availability vocabulary.
    #[derive(Serialize)]
    #[serde(rename_all = "snake_case")]
    enum NewerAvailability {
        Deny,
        Suspend,
    }

    #[derive(Serialize)]
    struct NewerList {
        features: Vec<NewerGatedFeature>,
    }

    #[derive(Deserialize)]
    struct List {
        features: Vec<GatedFeature>,
    }

    #[derive(Serialize)]
    struct NewerBoundSource {
        limit: u64,
        tier: NewerRuleTier,
    }

    #[derive(Serialize)]
    struct NewerPolicy {
        availability: NewerAvailability,
    }

    /// A list naming a feature this build lacks still decodes; the unknown
    /// member resolves to the deny-everything stand-in and gates nothing known.
    #[test]
    fn an_unknown_feature_decodes_and_gates_nothing_known() {
        let bytes = canonical_encode(&NewerList {
            features: vec![NewerGatedFeature::Payments, NewerGatedFeature::Dowsing],
        })
        .unwrap();
        let list: List = canonical_decode(&bytes).expect("the list decodes");
        assert_eq!(
            list.features,
            vec![GatedFeature::Payments, GatedFeature::Unknown]
        );
        assert!(!GatedFeature::Unknown.is_known());
        assert!(!GatedFeature::ALL.contains(&GatedFeature::Unknown));
        assert!(registry().iter().all(|e| e.feature.is_known()));
        assert_eq!(GatedFeature::from_key(GatedFeature::Unknown.as_str()), None);

        let stand_in = entry(GatedFeature::Unknown);
        assert!(stand_in.dimensions.is_empty() && stand_in.gate_surfaces.is_empty());
        let policy = effective_policy(GatedFeature::Unknown, &[], &[]);
        assert_eq!(policy.availability(), Availability::Deny);
        assert_eq!(policy.denied_by, Some(RuleTier::Structural));

        // Collapsing: never written back.
        assert!(canonical_encode(&GatedFeature::Unknown).is_err());
    }

    /// One spelling per tier: a reply body (serde) names a tier exactly as
    /// `as_str` does for a refusal's `details.tier` and every app's row keys.
    /// `Unknown` is left out — it is never serialized (encoding it fails).
    #[test]
    fn a_tier_encodes_as_its_as_str_spelling() {
        for tier in [
            RuleTier::Structural,
            RuleTier::Region,
            RuleTier::Admin,
            RuleTier::Guardian,
            RuleTier::SelfImposed,
        ] {
            let bytes = canonical_encode(&tier).unwrap();
            assert_eq!(bytes, canonical_encode(&tier.as_str()).unwrap(), "{tier:?}");
            assert_eq!(canonical_decode::<RuleTier>(&bytes).unwrap(), tier);
        }
    }

    /// An unknown tier on a bound decodes; the bound itself survives intact.
    #[test]
    fn an_unknown_tier_decodes_and_keeps_its_bound() {
        let bytes = canonical_encode(&NewerBoundSource {
            limit: 7,
            tier: NewerRuleTier::Council,
        })
        .unwrap();
        let bound: BoundSource = canonical_decode(&bytes).expect("the bound decodes");
        assert_eq!(bound.limit, 7);
        assert_eq!(bound.tier, RuleTier::Unknown);

        let known: BoundSource = canonical_decode(
            &canonical_encode(&NewerBoundSource {
                limit: 7,
                tier: NewerRuleTier::Admin,
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(known.tier, RuleTier::Admin);

        assert!(canonical_encode(&RuleTier::Unknown).is_err());
    }

    /// An unknown availability decodes carried, denies at its tier, and
    /// re-encodes byte-identically — the policy document can be re-saved and
    /// re-folded without deleting the restriction.
    #[test]
    fn an_unknown_availability_is_carried_and_denies() {
        let bytes = canonical_encode(&NewerPolicy {
            availability: NewerAvailability::Suspend,
        })
        .unwrap();
        let policy: FeaturePolicy = canonical_decode(&bytes).expect("the policy decodes");
        assert_eq!(policy.availability, Availability::Other("suspend".into()));
        assert!(policy.availability.denies());
        // The value itself re-encodes exactly (the document around it gains
        // its defaulted empty bounds, as any older document does).
        assert_eq!(
            canonical_encode(&policy.availability).unwrap(),
            canonical_encode(&NewerAvailability::Suspend).unwrap()
        );
        let resaved: FeaturePolicy = canonical_decode(&canonical_encode(&policy).unwrap()).unwrap();
        assert_eq!(resaved, policy);

        let effective = effective_policy(GatedFeature::P2pShare, &[(RuleTier::Admin, policy)], &[]);
        assert_eq!(effective.denied_by, Some(RuleTier::Admin));
        assert_eq!(effective.availability(), Availability::Deny);

        // The known spellings still land on their own arms, not the carry.
        let deny: FeaturePolicy = canonical_decode(
            &canonical_encode(&NewerPolicy {
                availability: NewerAvailability::Deny,
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(deny.availability, Availability::Deny);
    }
}
