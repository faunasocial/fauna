import SwiftUI
import FaunaKit

struct MacFeedFormView: View {
    let vm: FeedVM

    /// The `feed-rule-type-select` catalog — shared FFI (`feed.md` § Where logic
    /// lives → *Feed rule-builder presentation*), cached once per view.
    private static let ruleTypes = ruleTypeOptions()

    /// Form state + rule-input validation lifted to shared FaunaKit
    /// (`FeedCreateForm`) so it lives once across macOS + iOS; only this body
    /// — a macOS inline `VStack` — is per-platform.
    @State private var form = FeedCreateForm()

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField(L.feed.create.feedName, text: $form.name)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.feedCreateFeedName)
                .automationField(Ids.feedCreateFeedName, text: $form.name)

            Picker(L.feed.create.combination, selection: $form.combination) {
                Text(L.feed.create.modeAll).tag("all")
                Text(L.feed.create.modeAny).tag("any")
            }
            .pickerStyle(.segmented)
            .accessibilityIdentifier(Ids.feedCombinationSelect)
            .automationSelect(
                Ids.feedCombinationSelect,
                value: { form.combination },
                set: { form.combination = $0 }
            )

            ForEach(Array(form.rules.enumerated()), id: \.offset) { idx, rule in
                HStack {
                    Text(renderLocalizedText(ruleSummaryLabel(
                        ruleType: rule.type,
                        value: rule.value,
                        required: rule.required
                    ))).font(.caption)
                    Spacer()
                    Button(action: { form.removeRule(at: idx) }) {
                        Image(systemName: "xmark.circle")
                    }
                    .buttonStyle(.plain)
                    .controlSize(.small)
                }
            }

            HStack {
                Picker("", selection: $form.selectedRuleType) {
                    ForEach(Self.ruleTypes, id: \.value) { option in
                        Text(renderLocalizedText(option.label)).tag(option)
                    }
                }
                .frame(maxWidth: 150)
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
                case .text, .number:
                    TextField(L.common.value, text: $form.newRuleValue)
                        .textFieldStyle(.roundedBorder)
                        .frame(maxWidth: 120)
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
                        .frame(maxWidth: 80)
                        .accessibilityIdentifier(Ids.feedRuleValueInput)
                        .automationField(Ids.feedRuleValueInput, text: $form.newRuleValue)
                    TextField(L.feed.create.ruleThresholdShort, text: $form.newRuleThreshold)
                        .textFieldStyle(.roundedBorder)
                        .frame(maxWidth: 50)
                        .accessibilityIdentifier(Ids.feedRuleThresholdInput)
                        .automationField(Ids.feedRuleThresholdInput, text: $form.newRuleThreshold)
                }

                Button(action: { form.addRule() }) {
                    Image(systemName: "plus.circle")
                }
                .accessibilityIdentifier(Ids.feedAddRuleButton)
                .disabled(!form.canAddRule)
                .automationActivate(
                    Ids.feedAddRuleButton,
                    isEnabled: { form.canAddRule }
                ) { form.addRule() }
            }

            Text(L.feed.create.factors).font(.caption).foregroundStyle(.secondary)

            ForEach(Array(form.factors.enumerated()), id: \.offset) { idx, factor in
                HStack {
                    Text(factor.factor).font(.caption)
                    Text(formatWeightPermille(weightPermille: factor.weightPermille))
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button(action: { form.removeFactor(at: idx) }) {
                        Image(systemName: "xmark.circle")
                    }
                    .buttonStyle(.plain)
                    .controlSize(.small)
                }
            }

            HStack {
                Picker(L.feed.create.factors, selection: $form.selectedFactorKey) {
                    ForEach(FeedCreateForm.builtinFactors, id: \.value) { option in
                        Text(renderLocalizedText(option.label)).tag(option.value)
                    }
                    ForEach(form.trainedFactorOptions) { option in
                        Text(option.label).tag(option.key)
                    }
                }
                .frame(maxWidth: 150)
                .accessibilityIdentifier(Ids.feedFactorSelect)
                .automationSelect(
                    Ids.feedFactorSelect,
                    value: { form.selectedFactorKey },
                    set: { form.selectedFactorKey = $0 }
                )

                TextField(L.feed.create.factorWeightPlaceholder, text: $form.newFactorWeight)
                    .textFieldStyle(.roundedBorder)
                    .frame(maxWidth: 60)
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

                Button(action: { form.addFactor() }) {
                    Image(systemName: "plus.circle")
                }
                .accessibilityIdentifier(Ids.feedAddFactorButton)
                .disabled(!form.canAddFactor)
                .automationActivate(
                    Ids.feedAddFactorButton,
                    isEnabled: { form.canAddFactor }
                ) { form.addFactor() }
            }

            HStack {
                Button(L.feed.create.title) {
                    submitCreateFeed()
                }
                .disabled(!form.canCreate || vm.creatingFeed)
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.createFeed)
                .automationActivate(
                    Ids.createFeed,
                    isEnabled: { form.canCreate && !vm.creatingFeed }
                ) { submitCreateFeed() }

                Button(L.common.cancel) {
                    vm.showCreateForm = false
                }
                .controlSize(.small)
                .accessibilityIdentifier(Ids.feedCreateCancel)
                .automationActivate(Ids.feedCreateCancel) { vm.showCreateForm = false }
            }
        }
        .padding(.vertical, 4)
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

    /// The create-feed button's action, factored out so the automation sibling
    /// drives the exact same code path the Button does. Each rule is encoded via
    /// the shared `encode_filter_rule` codec inside the manager.
    private func submitCreateFeed() {
        Task { await vm.createFeed(name: form.name, combination: form.combination,
                                   rules: form.filterRules, factors: form.factors) }
    }
}
