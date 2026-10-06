import SwiftUI
import FaunaKit

struct FeedFormView: View {
    let vm: FeedVM

    /// The `feed-rule-type-select` catalog — shared FFI (`feed.md` § Where logic
    /// lives → *Feed rule-builder presentation*), cached once per view.
    private static let ruleTypes = ruleTypeOptions()

    @Environment(\.dismiss) private var dismiss

    /// Form state + rule-input validation lifted to shared FaunaKit
    /// (`FeedCreateForm`) so it lives once across macOS + iOS; only this body
    /// — the iOS presentation — is per-platform.
    @State private var form = FeedCreateForm()

    /// An eager `ScrollView { VStack }`, NOT a `Form`: an iOS `Form` is a lazy
    /// `List` whose off-screen sections never register, so the rule-builder's
    /// branch-conditional inputs were undrivable below the fold
    /// (apple-e2e-automation.md rule 6, which applies to both apple apps).
    /// Vertical overflow stays attached by design — the driver reaches
    /// below-the-fold closures without scrolling.
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                VStack(alignment: .leading, spacing: 8) {
                    TextField(L.feed.create.feedName, text: $form.name)
                        .textFieldStyle(.roundedBorder)
                        .accessibilityIdentifier(Ids.feedCreateFeedName)
                        .automationField(Ids.feedCreateFeedName, text: $form.name)
                    Picker(L.feed.create.combination, selection: $form.combination) {
                        Text(L.feed.create.modeAll).tag("all")
                        Text(L.feed.create.modeAny).tag("any")
                    }
                    .accessibilityIdentifier(Ids.feedCombinationSelect)
                    .automationSelect(
                        Ids.feedCombinationSelect,
                        value: { form.combination },
                        set: { form.combination = $0 }
                    )
                }

                VStack(alignment: .leading, spacing: 8) {
                    sectionHeader(L.feed.create.filterRules)

                    // An explicit per-row remove button, because `.onDelete` is a
                    // `List` affordance that dies with the `Form`. macOS
                    // (`MacFeedFormView`) and web (`remove-btn`) already render one,
                    // so this closes a divergence rather than dropping an
                    // affordance (priorities #1/#4); `ui.yaml` `create_feed`
                    // registers no id for it on any app.
                    ForEach(Array(form.rules.enumerated()), id: \.offset) { idx, rule in
                        HStack {
                            Text(renderLocalizedText(ruleSummaryLabel(
                                ruleType: rule.type,
                                value: rule.value,
                                required: rule.required
                            )))
                            Spacer()
                            Button(action: { form.removeRule(at: idx) }) {
                                Image(systemName: "xmark.circle")
                            }
                            .buttonStyle(.plain)
                        }
                    }

                    Picker("", selection: $form.selectedRuleType) {
                        ForEach(Self.ruleTypes, id: \.value) { option in
                            Text(renderLocalizedText(option.label)).tag(option)
                        }
                    }
                    .accessibilityIdentifier(Ids.feedRuleTypeSelect)
                    .automationSelect(
                        Ids.feedRuleTypeSelect,
                        value: { form.selectedRuleType.value },
                        set: { wire in
                            if let match = Self.ruleTypes.first(where: { $0.value == wire }) {
                                form.selectedRuleType = match
                            }
                        }
                    )

                    switch form.selectedRuleType.inputKind {
                    case .text:
                        TextField(L.common.value, text: $form.newRuleValue)
                            .textFieldStyle(.roundedBorder)
                            .accessibilityIdentifier(Ids.feedRuleValueInput)
                            .automationField(Ids.feedRuleValueInput, text: $form.newRuleValue)
                    case .number:
                        TextField(L.common.value, text: $form.newRuleValue)
                            .keyboardType(.numberPad)
                            .textFieldStyle(.roundedBorder)
                            .accessibilityIdentifier(Ids.feedRuleValueInput)
                            .automationField(Ids.feedRuleValueInput, text: $form.newRuleValue)
                    case .toggle:
                        Toggle(isOn: $form.newRuleRequired) {
                            Text(renderLocalizedText(ruleRequiredLabel(required: form.newRuleRequired)))
                        }
                        .accessibilityIdentifier(Ids.feedRuleRequiredToggle)
                        .automationActivate(
                            Ids.feedRuleRequiredToggle,
                            value: { form.newRuleRequired ? "on" : "off" }
                        ) { form.newRuleRequired.toggle() }
                    case .textAndNumber:
                        TextField(L.feed.create.ruleCategory, text: $form.newRuleValue)
                            .textFieldStyle(.roundedBorder)
                            .accessibilityIdentifier(Ids.feedRuleValueInput)
                            .automationField(Ids.feedRuleValueInput, text: $form.newRuleValue)
                        TextField(L.feed.create.ruleThreshold, text: $form.newRuleThreshold)
                            .keyboardType(.decimalPad)
                            .textFieldStyle(.roundedBorder)
                            .accessibilityIdentifier(Ids.feedRuleThresholdInput)
                            .automationField(Ids.feedRuleThresholdInput, text: $form.newRuleThreshold)
                    }

                    Button(L.feed.create.addRule) {
                        form.addRule()
                    }
                    .accessibilityIdentifier(Ids.feedAddRuleButton)
                    .disabled(!form.canAddRule)
                    .automationActivate(
                        Ids.feedAddRuleButton,
                        isEnabled: { form.canAddRule }
                    ) { form.addRule() }
                }

                VStack(alignment: .leading, spacing: 8) {
                    sectionHeader(L.feed.create.factors)

                    ForEach(Array(form.factors.enumerated()), id: \.offset) { idx, factor in
                        HStack {
                            Text(factor.factor)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                            Text(formatWeightPermille(weightPermille: factor.weightPermille))
                            Spacer()
                            Button(action: { form.removeFactor(at: idx) }) {
                                Image(systemName: "xmark.circle")
                            }
                            .buttonStyle(.plain)
                        }
                    }

                    Picker(L.feed.create.factors, selection: $form.selectedFactorKey) {
                        ForEach(FeedCreateForm.builtinFactors, id: \.value) { option in
                            Text(renderLocalizedText(option.label)).tag(option.value)
                        }
                        ForEach(form.trainedFactorOptions) { option in
                            Text(option.label).tag(option.key)
                        }
                    }
                    .accessibilityIdentifier(Ids.feedFactorSelect)
                    .automationSelect(
                        Ids.feedFactorSelect,
                        value: { form.selectedFactorKey },
                        set: { form.selectedFactorKey = $0 }
                    )

                    TextField(L.feed.create.factorWeightPlaceholder, text: $form.newFactorWeight)
                        .keyboardType(.decimalPad)
                        .textFieldStyle(.roundedBorder)
                        .accessibilityIdentifier(Ids.feedFactorWeightInput)
                        .automationField(Ids.feedFactorWeightInput, text: $form.newFactorWeight)

                    Toggle(isOn: $form.newFactorGlobal) {
                        Text(L.feed.create.factorGlobalToggle)
                    }
                    .accessibilityIdentifier(Ids.feedFactorGlobalToggle)
                    .automationActivate(
                        Ids.feedFactorGlobalToggle,
                        value: { form.newFactorGlobal ? "on" : "off" }
                    ) { form.newFactorGlobal.toggle() }

                    Button(L.feed.create.addFactor) {
                        form.addFactor()
                    }
                    .accessibilityIdentifier(Ids.feedAddFactorButton)
                    .disabled(!form.canAddFactor)
                    .automationActivate(
                        Ids.feedAddFactorButton,
                        isEnabled: { form.canAddFactor }
                    ) { form.addFactor() }
                }

                VStack(spacing: 8) {
                    Button(L.feed.create.title) {
                        submitCreateFeed()
                    }
                    .accessibilityIdentifier(Ids.createFeed)
                    .buttonStyle(.borderedProminent)
                    .disabled(!form.canCreate || vm.creatingFeed)
                    .frame(maxWidth: .infinity)
                    .automationActivate(
                        Ids.createFeed,
                        isEnabled: { form.canCreate && !vm.creatingFeed }
                    ) { submitCreateFeed() }

                    Button(role: .cancel) {
                        dismiss()
                    } label: {
                        Text(L.common.cancel)
                    }
                    .accessibilityIdentifier(Ids.feedCreateCancel)
                    .frame(maxWidth: .infinity)
                    .automationActivate(Ids.feedCreateCancel) { dismiss() }
                }
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .navigationTitle(L.feed.create.title)
        .navigationBarTitleDisplayMode(.inline)
        // Grow the factor picker with the user's trained topics (the third
        // source, alongside the static `engagement` option) — fire-and-forget,
        // best-effort (mirrors windows' `AppendTrainedTopicFactorsAsync`; a
        // fetch fault just leaves the picker short, never blocks feed creation).
        .task {
            let rows = await vm.trainedFactorRows()
            form.trainedFactorOptions = rows.compactMap { row in
                row.factorKey.map { TrainedFactorOption(key: $0, label: row.name) }
            }
        }
    }

    /// The `Section(_:)` header a `Form` used to draw for free.
    private func sectionHeader(_ text: String) -> some View {
        Text(text)
            .font(.caption)
            .foregroundStyle(.secondary)
    }

    /// The create-feed button's action, factored out so the automation sibling
    /// drives the exact same code path the Button does. Each rule is encoded via
    /// the shared `encode_filter_rule` codec inside the manager (feed.md § Where
    /// logic lives).
    private func submitCreateFeed() {
        Task {
            await vm.createFeed(name: form.name, combination: form.combination,
                                rules: form.filterRules, factors: form.factors)
            dismiss()
        }
    }
}
