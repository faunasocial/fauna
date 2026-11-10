import SwiftUI

/// `settings-nest-rules-section` — the rules surface (`nest-rules.md`
/// § Acceptance at joining, and a change of rules → *The rules surface*): the
/// rules in force for the nest this session reads through — per rule the verdict,
/// the level and the nest's own reason — its version and issue time, and any rule
/// not applied on this device. A paint of the shared book's `summary` (every
/// label already chosen in `fauna_client_policy`), never an app-side fold —
/// ``RegionSettingsSection``'s twin, beside it on the Status page; tui's
/// `nest_rules::settings_elements`, android's `NestRulesSettingsSection`.
///
/// Opening this surface is reading the change: it clears the one-line notice.
/// Absent while the nest has published no rules.
public struct NestRulesSettingsSection: View {
    let nestRules: NestRulesStore

    public init(nestRules: NestRulesStore) {
        self.nestRules = nestRules
    }

    public var body: some View {
        Group {
            // `is_shown`, not "rules non-empty": a document none of which applies
            // here paints with no rule listed.
            if let summary = nestRules.summary(), nestRulesSummaryIsShown(summary) {
                GroupBox(L.nestRules.settingsTitle(nest: summary.domain)) {
                    VStack(alignment: .leading, spacing: 6) {
                        automationText(
                            Ids.settingsNestRulesVersion,
                            L.nestRules.version(
                                sequence: String(summary.sequence),
                                issued: formatUnixLocal(secs: summary.issuedAt)))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        // The whole document's note, unscoped — a rule's own sits in its item.
                        if let why = summary.notAppliedSentence {
                            automationText(Ids.settingsNestRulesInertNotice, renderLocalizedText(why))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        ForEach(Array(summary.rules.enumerated()), id: \.offset) { index, rule in
                            ruleItem(rule, index: index)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(8)
                }
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(Ids.settingsNestRulesSection)
                .automationValue(
                    Ids.settingsNestRulesSection,
                    text: { L.nestRules.settingsTitle(nest: summary.domain) })
            }
        }
        // Showing the surface is reading the change.
        .onAppear { nestRules.dismissNotice() }
    }

    @ViewBuilder
    private func ruleItem(_ rule: NestRuleLine, index: Int) -> some View {
        let line = nestRuleLineText(rule)
        VStack(alignment: .leading, spacing: 2) {
            Text(line)
                .accessibilityIdentifier(Ids.settingsNestRulesItem)
                .automationValue(Ids.settingsNestRulesItem, text: { line })
            if let inert = rule.inert {
                automationText(Ids.settingsNestRulesInertNotice, renderLocalizedText(inert))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 2)
        .automationScope(Ids.settingsNestRulesItem, index: index)
    }
}

/// `NestRulesSummary::is_shown` — rules listed, or the whole document not applied
/// here; only an applied empty document paints nothing.
func nestRulesSummaryIsShown(_ summary: NestRulesSummary) -> Bool {
    !summary.rules.isEmpty || summary.notApplied != nil
}

/// One rule as both rules surfaces list it: verdict, level where the rule has
/// one, and the nest's own reason verbatim.
func nestRuleLineText(_ rule: NestRuleLine) -> String {
    let verdict = renderLocalizedText(rule.verdict)
    return rule.level.map {
        L.nestRules.ruleLine(verdict: verdict, level: renderLocalizedText($0), reason: rule.reason)
    } ?? L.nestRules.ruleLineNoLevel(verdict: verdict, reason: rule.reason)
}

/// One rule as the joining surface lists it: the rule's line, and under a rule
/// this device cannot apply (`NestRuleLine::inert`) its not-applied note on a
/// second line of the same item (`nest-rules.md` § Acceptance at joining, and a
/// change of rules) — tui's `nest_rules::joining_rule_line`. The settings surface
/// paints the note as its own `settings-nest-rules-inert-notice` instead.
func joiningNestRuleLineText(_ rule: NestRuleLine) -> String {
    let line = nestRuleLineText(rule)
    return rule.inert.map { "\(line)\n\(renderLocalizedText($0))" } ?? line
}

/// `onboarding-nest-rules` — the nest's rules at joining (`nest-rules.md` §
/// Acceptance at joining, *Joining is acceptance*): the heading naming the nest,
/// followed by the not-applied sentence when none of the document applies here,
/// then one `onboarding-nest-rules-item` per rule. Placed directly above
/// ``TermsNotice`` on `invite_request` and `handle_entry`; absent while the
/// machine has no verified read. A paint of `OnboardingMachine::joining_nest_rules`
/// — tui's `nest_rules::joining_elements`.
public struct JoiningNestRules: View {
    let summary: NestRulesSummary?

    public init(summary: NestRulesSummary?) {
        self.summary = summary
    }

    public var body: some View {
        if let summary, nestRulesSummaryIsShown(summary) {
            let heading = L.nestRules.joiningHeading(nest: summary.domain)
            let text = summary.notAppliedSentence.map { "\(heading) \(renderLocalizedText($0))" } ?? heading
            VStack(alignment: .leading, spacing: 2) {
                automationText(Ids.onboardingNestRules, text)
                    .font(.footnote)
                    .fixedSize(horizontal: false, vertical: true)
                ForEach(Array(summary.rules.enumerated()), id: \.offset) { index, rule in
                    let line = joiningNestRuleLineText(rule)
                    Text(line)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier(Ids.onboardingNestRulesItem)
                        .automationValue(Ids.onboardingNestRulesItem, text: { line })
                        .automationScope(Ids.onboardingNestRulesItem, index: index)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// The one-line notice when the session nest changed its rules (`nest-rules.md`
/// § Acceptance at joining, and a change of rules): names the nest, and a tap is
/// the way to the rules surface (Settings → Status), where opening the surface
/// reads the change and clears it (``NestRulesStore/dismissNotice()``). Mounted
/// with the shell's other global chrome; the text is the shared frame
/// (`nest_rules.changed_notice`) and the notice adds no element id — android's
/// `NestRulesChangedNotice`, web's `NestRulesNotice.svelte`.
public struct NestRulesChangedNotice: View {
    let nestRules: NestRulesStore
    let onOpen: () -> Void

    public init(nestRules: NestRulesStore, onOpen: @escaping () -> Void) {
        self.nestRules = nestRules
        self.onOpen = onOpen
    }

    public var body: some View {
        if let nest = nestRules.changedNotice {
            Button(action: onOpen) {
                Text(L.nestRules.changedNotice(nest: nest))
                    .font(.footnote)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 8)
            }
            .buttonStyle(.plain)
        }
    }
}
