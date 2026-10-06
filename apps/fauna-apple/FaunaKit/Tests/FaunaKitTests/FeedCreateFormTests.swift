import Testing
import Foundation
@testable import FaunaKit

// Pure, nest-free tests for `FeedCreateForm` — the shared create-feed form state
// lifted out of the per-platform `FeedFormView` (iOS) / `MacFeedFormView` (macOS)
// shells so the rule-input validation (`canAddRule`) and the `(type, value,
// required)` triple-building (`addRule`) live once (priority #1/#2). Only the
// SwiftUI body stays per-platform; both bind to this model. These pin the
// staging logic so a regression fails fast WITHOUT the slow e2e harness. The
// value-semantics half (CSV / count / hours / label 0–10 packing) lives in shared
// Rust and is covered by `FeedCodecTests` + `fauna-client-feed`.
//
// Rule-type → input-kind mapping under test (from the shared `ruleTypeOptions()`
// catalog, `libs/fauna-ffi/src/feed_rules.rs`):
//   BodyContains → .text, MinReplies → .number, HasMedia → .toggle,
//   LabelBelow → .textAndNumber.

private func select(_ value: String) -> FfiRuleTypeOption {
    ruleTypeOptions().first { $0.value == value }!
}

// MARK: - canAddRule gating per input kind

@Test func canAddRuleTextRequiresNonEmptyValue() {
    var form = FeedCreateForm()
    form.selectedRuleType = select("BodyContains")        // .text
    #expect(form.canAddRule == false)                     // empty
    form.newRuleValue = "   "
    #expect(form.canAddRule == false)                     // whitespace only
    form.newRuleValue = "rust"
    #expect(form.canAddRule == true)
}

@Test func canAddRuleNumberRequiresParseableInt() {
    var form = FeedCreateForm()
    form.selectedRuleType = select("MinReplies")          // .number
    form.newRuleValue = "not-a-number"
    #expect(form.canAddRule == false)
    form.newRuleValue = "5"
    #expect(form.canAddRule == true)
}

@Test func canAddRuleToggleIsAlwaysAddable() {
    var form = FeedCreateForm()
    form.selectedRuleType = select("HasMedia")            // .toggle
    #expect(form.canAddRule == true)                      // no value needed
}

@Test func canAddRuleTextAndNumberRequiresBothValueAndThreshold() {
    var form = FeedCreateForm()
    form.selectedRuleType = select("LabelBelow")          // .textAndNumber
    form.newRuleValue = "spam"
    #expect(form.canAddRule == false)                     // threshold missing
    form.newRuleThreshold = "not-a-number"
    #expect(form.canAddRule == false)
    form.newRuleThreshold = "5"
    #expect(form.canAddRule == true)
}

// MARK: - addRule builds the right triple and resets inputs

@Test func addRuleTextTrimsAndStages() {
    var form = FeedCreateForm()
    form.selectedRuleType = select("BodyContains")
    form.newRuleValue = "  rust, svelte  "
    form.addRule()
    #expect(form.rules.count == 1)
    #expect(form.rules[0].type == "BodyContains")
    #expect(form.rules[0].value == "rust, svelte")        // trimmed (inner commas preserved for the codec)
    #expect(form.rules[0].required == true)
    #expect(form.newRuleValue == "")                      // input reset
}

@Test func addRuleToggleStagesEmptyValueAndCarriesRequired() {
    var form = FeedCreateForm()
    form.selectedRuleType = select("HasMedia")
    form.newRuleRequired = false
    form.addRule()
    #expect(form.rules[0].type == "HasMedia")
    #expect(form.rules[0].value == "")
    #expect(form.rules[0].required == false)
    #expect(form.newRuleRequired == true)                 // reset to default
}

@Test func addRuleTextAndNumberPacksCategoryColonThreshold() {
    // The form packs "category:threshold"; the shared Rust `parse_label` splits it
    // back (feed.md § Filter rule types). No float / scaling on the Swift side.
    var form = FeedCreateForm()
    form.selectedRuleType = select("LabelBelow")
    form.newRuleValue = "  spam  "
    form.newRuleThreshold = "  5  "
    form.addRule()
    #expect(form.rules[0].type == "LabelBelow")
    #expect(form.rules[0].value == "spam:5")
    #expect(form.newRuleThreshold == "")                  // reset
}

// MARK: - canCreate gating + filterRules builder

@Test func canCreateRequiresANameAndNothingElse() {
    var form = FeedCreateForm()
    #expect(form.canCreate == false)                      // no name
    form.name = "My Feed"
    // A name ALONE is enough. This assertion used to read `== false` with the
    // comment "name but no rules": the old gate also demanded a staged rule,
    // which made a factor-only feed uncreatable through the real UI while web
    // and linux both allowed it (priorities #1/#4). See `FeedCreateForm
    // .canCreate` for the full story and how the actuation gate surfaced it.
    #expect(form.canCreate == true)
    form.selectedRuleType = select("BodyContains")
    form.newRuleValue = "rust"
    form.addRule()
    #expect(form.canCreate == true)                       // still true with a rule
    form.name = "   "
    #expect(form.canCreate == false)                      // whitespace-only name
}

@Test func filterRulesMapsStagedTriplesToFilterRuleInput() {
    var form = FeedCreateForm()
    form.selectedRuleType = select("BodyContains")
    form.newRuleValue = "rust"
    form.addRule()
    form.selectedRuleType = select("HasMedia")
    form.newRuleRequired = true
    form.addRule()

    let inputs = form.filterRules
    #expect(inputs.count == 2)
    #expect(inputs[0].ruleType == "BodyContains")
    #expect(inputs[0].value == "rust")
    #expect(inputs[1].ruleType == "HasMedia")
    #expect(inputs[1].required == true)
}

// MARK: - staged-rule removal

@Test func removeRuleDropsTheStagedRow() {
    var form = FeedCreateForm()
    form.selectedRuleType = select("BodyContains")
    form.newRuleValue = "a"; form.addRule()
    form.newRuleValue = "b"; form.addRule()
    form.removeRule(at: 0)
    #expect(form.rules.count == 1)
    #expect(form.rules[0].value == "b")
}

// MARK: - factor-weight editor (content-moderation-and-ranking.md § Composition)

// The picker's head comes from the shared `builtinFactorOptions()` catalog
// (feed.md § Where logic lives → Feed factor-picker built-ins): `engagement`
// first, then `trending`, and the default selection is the first built-in.
@Test func factorPickerStartsFromTheSharedBuiltIns() {
    #expect(Array(FeedCreateForm.builtinFactors.map(\.value).prefix(2)) == ["engagement", "trending"])
    #expect(FeedCreateForm().selectedFactorKey == "engagement")
}

@Test func canAddFactorRequiresParseableWeight() {
    var form = FeedCreateForm()
    form.newFactorWeight = "not-a-number"
    #expect(form.canAddFactor == false)
    form.newFactorWeight = "2.0"
    #expect(form.canAddFactor == true)
}

@Test func addFactorScalesWeightPermilleAndResetsInputs() {
    var form = FeedCreateForm()
    form.selectedFactorKey = "engagement"
    form.newFactorWeight = "2.0"
    form.addFactor()
    #expect(form.factors.count == 1)
    #expect(form.factors[0].factor == "engagement")
    #expect(form.factors[0].weightPermille == 2000)
    #expect(form.factors[0].global == false)
    #expect(form.newFactorWeight == "1.0")                // reset to default
    #expect(form.newFactorGlobal == false)                // reset to default
}

@Test func addFactorCarriesGlobalToggleAndResetsIt() {
    var form = FeedCreateForm()
    form.newFactorWeight = "1.5"
    form.newFactorGlobal = true
    form.addFactor()
    #expect(form.factors[0].global == true)
    #expect(form.newFactorGlobal == false)                // reset after staging
}

@Test func removeFactorDropsTheStagedRow() {
    var form = FeedCreateForm()
    form.newFactorWeight = "1.0"; form.addFactor()
    form.newFactorWeight = "2.0"; form.addFactor()
    form.removeFactor(at: 0)
    #expect(form.factors.count == 1)
    #expect(form.factors[0].weightPermille == 2000)
}
