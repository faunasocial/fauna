import Testing
import Foundation
@testable import FaunaKit

// `joiningNestRuleLineText` — the joining surface's `onboarding-nest-rules-item`
// text (`docs/goal/behavior/nest-rules.md` § Acceptance at joining, and a change
// of rules): the rule's line, and under a rule this device cannot apply its
// not-applied note on a second line, as tui's `joining_rule_line` paints it. A
// key missing from the catalog resolves to itself, so the keys double as text.

private func rule(inert: LocalizedText?) -> NestRuleLine {
    NestRuleLine(
        verdict: LocalizedText(key: "verdict", args: [:]),
        level: nil,
        reason: "the nest's own reason",
        inert: inert)
}

@Test func joiningLineOfAnAppliedRuleIsTheRuleLineAlone() {
    let applied = rule(inert: nil)
    #expect(joiningNestRuleLineText(applied) == nestRuleLineText(applied))
    #expect(!joiningNestRuleLineText(applied).contains("\n"))
}

@Test func joiningLineOfAnInertRuleCarriesTheNoteOnASecondLine() {
    let inert = rule(inert: LocalizedText(key: "not-applied", args: [:]))
    #expect(joiningNestRuleLineText(inert) == "\(nestRuleLineText(inert))\nnot-applied")
}
