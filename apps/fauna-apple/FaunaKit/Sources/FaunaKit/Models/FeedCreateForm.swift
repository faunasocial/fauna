import Foundation

/// Shared mutable state for the create-feed form, lifted out of the per-platform
/// `FeedFormView` (iOS) / `MacFeedFormView` (macOS) shells so the rule-input
/// validation (`canAddRule`) and the `(type, value, required)` triple-building
/// (`addRule`) live once (priority #1/#2). Only the SwiftUI body — an iOS modal
/// `Form` vs a macOS inline `VStack`, a legitimate platform-idiom divergence —
/// stays per-platform; both shells hold this as `@State` and bind to it.
///
/// The *value semantics* (CSV / count / hours / label 0–10 packing) stay in shared
/// Rust (`fauna_client_feed::encode_filter_rule`, reached via the UniFFI
/// `encodeFilterRule` codec that `FeedVM.createFeed` runs each `FilterRuleInput`
/// through). This model only assembles the raw `(type, value, required)` UI triples
/// that codec expects, per `docs/goal/ui/feed.md` § Filter rule types — no JSON,
/// no float, no scaling on the Swift side.
public struct FeedCreateForm {
    /// One staged filter rule before it's encoded to the wire `FilterRule`.
    public typealias DraftRule = (type: String, value: String, required: Bool)

    public var name = ""
    public var combination = "all"
    public var rules: [DraftRule] = []
    public var selectedRuleType: FfiRuleTypeOption = ruleTypeOptions()[0]
    public var newRuleValue = ""
    public var newRuleRequired = true
    public var newRuleThreshold = ""

    /// The `feed-factor-select` picker's head: the built-in factors every user
    /// can weight from a fresh account (`engagement`, `trending`), from the
    /// shared `fauna_client_feed::builtin_factor_options` catalog (feed.md §
    /// Where logic lives → Feed factor-picker built-ins) — never a Swift
    /// literal, so a new built-in reaches this picker with no app change.
    /// Growing the picker with the caller's subscribed labeler factors is
    /// future work for whichever session lands apple's Community-labelers
    /// catalog.
    public static let builtinFactors: [FfiFactorOption] = builtinFactorOptions()

    /// The `feed-factor-select` picker's third source (topic-factors.md §
    /// Authoring surface & picker): the user's trained-topic factors, fetched
    /// async by the caller (`FeedVM.trainedFactorRows()`) and staged here so
    /// the picker's `ForEach` can grow after the shared built-ins — mirrors
    /// windows' `AppendTrainedTopicFactorsAsync`.
    public var trainedFactorOptions: [TrainedFactorOption] = []

    public var factors: [FactorWeightInput] = []
    public var selectedFactorKey: String = FeedCreateForm.builtinFactors[0].value
    public var newFactorWeight = "1.0"
    public var newFactorGlobal = false

    public init() {}

    /// Whether the staged `(selectedRuleType, newRuleValue, newRuleThreshold)`
    /// inputs are complete enough to add — gates the `feed-add-rule-button`.
    /// Delegates to the shared `fauna_client_feed::can_add_rule` predicate
    /// (this form was its lift source; priority #2 — one predicate, not two
    /// copies that can drift).
    public var canAddRule: Bool {
        FaunaKit.canAddRule(inputKind: selectedRuleType.inputKind, value: newRuleValue, threshold: newRuleThreshold)
    }

    /// Stage the current inputs as a `(type, value, required)` triple and reset
    /// them. `textAndNumber` packs `"category:threshold"`, which the shared Rust
    /// `parse_label` splits back (feed.md § Filter rule types).
    public mutating func addRule() {
        let ruleType = selectedRuleType.value
        var value: String
        var required = true
        switch selectedRuleType.inputKind {
        case .text, .number:
            value = newRuleValue.trimmingCharacters(in: .whitespaces)
        case .toggle:
            value = ""
            required = newRuleRequired
        case .textAndNumber:
            let category = newRuleValue.trimmingCharacters(in: .whitespaces)
            let threshold = newRuleThreshold.trimmingCharacters(in: .whitespaces)
            value = "\(category):\(threshold)"
        }
        rules.append((type: ruleType, value: value, required: required))
        newRuleValue = ""
        newRuleThreshold = ""
        newRuleRequired = true
    }

    /// Drop one staged rule — the per-row delete button both apple apps now
    /// render (iOS's `.onDelete` swipe went with its `Form`, per rule 6).
    public mutating func removeRule(at index: Int) { rules.remove(at: index) }

    /// Whether the feed can be created — a non-blank name. Callers `&&` in the
    /// VM's in-flight flag (`vm.creatingFeed`).
    ///
    /// **A staged rule is deliberately NOT required** (fixed 2026-08-03). This
    /// also demanded `!rules.isEmpty`, which made a *factor-only* feed — one
    /// composed purely by factor weights, with no filter rules — impossible to
    /// create through the real apple UI: the Create button stayed disabled
    /// forever. web (`!newFeedName.trim()`) and linux (`name.is_empty()`) both
    /// gate on the name alone, so apple was the sole outlier and the strictly
    /// poorer shape (priorities #1/#4 — resolve drift onto the richest existing
    /// pattern). `create_feed` carries rules and factors as independent inputs;
    /// either alone is a valid feed.
    ///
    /// It went unnoticed because the e2e harness could actuate a disabled
    /// control: `automationActivate` called `submitCreateFeed()` directly, so
    /// `test_create_feed_with_factor_weight_sets_local_composition` passed while
    /// doing something no user could do. The actuation gate
    /// (§ The actuation gate, `apple-e2e-automation.md`) is what surfaced it.
    public var canCreate: Bool {
        !name.trimmingCharacters(in: .whitespaces).isEmpty
    }

    /// The staged rules as the `[FilterRuleInput]` `FeedVM.createFeed` encodes
    /// through the shared `encodeFilterRule` codec.
    public var filterRules: [FilterRuleInput] {
        rules.map { FilterRuleInput(ruleType: $0.type, value: $0.value, required: $0.required) }
    }

    /// Whether the staged `(selectedFactorKey, newFactorWeight)` inputs are
    /// complete enough to add — gates the `feed-add-factor-button`.
    public var canAddFactor: Bool {
        !selectedFactorKey.trimmingCharacters(in: .whitespaces).isEmpty
            && Double(newFactorWeight) != nil
    }

    /// Stage the current inputs as a `FactorWeightInput` and reset them. The
    /// decimal multiplier → the wire's signed `weight_permille` is shared Rust
    /// (`parseWeightPermille` — strict-parse + half-away-from-zero, unlike
    /// Swift's lenient `Double(String)` init and its own rounding rule;
    /// `canAddFactor` already gates this on a parseable `newFactorWeight`, so the
    /// shared fn's own unparseable-input fallback is never exercised from the UI).
    public mutating func addFactor() {
        factors.append(FactorWeightInput(
            factor: selectedFactorKey,
            weightPermille: parseWeightPermille(input: newFactorWeight),
            global: newFactorGlobal))
        newFactorWeight = "1.0"
        newFactorGlobal = false
    }

    /// Drop one staged factor — the per-row delete button both apple apps now
    /// render (iOS's `.onDelete` swipe went with its `Form`, per rule 6).
    public mutating func removeFactor(at index: Int) { factors.remove(at: index) }
}

/// One `feed-factor-select` option: a stable wire key (`topic:<hex>`) paired
/// with its display label — the keys-not-labels split every picker in this
/// codebase uses (the picker's `Text(label).tag(key)` shape).
public struct TrainedFactorOption: Identifiable, Equatable {
    public let key: String
    public let label: String
    public var id: String { key }
    public init(key: String, label: String) {
        self.key = key
        self.label = label
    }
}
