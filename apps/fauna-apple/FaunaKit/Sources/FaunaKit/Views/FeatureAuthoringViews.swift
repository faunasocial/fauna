import SwiftUI

/// The element ids one host of the shared `feature-policy-editor` family paints
/// its rows with (`docs/goal/architecture/dynamic-features.md` § Authoring
/// surfaces): the admin Nest page and the Family page's guardian section share
/// the row shape and differ only in the id family; the Status page's rows carry
/// their own `feature-limits-own-*` pair inside the transparency row instead
/// (`FeatureLimitsSection`).
public struct FeatureHostIds {
    let row: String
    let name: String
    let summary: String
    let editButton: String

    public static let admin = FeatureHostIds(
        row: Ids.adminNestFeatureLimitsRow, name: Ids.adminNestFeatureLimitsName,
        summary: Ids.adminNestFeatureLimitsSummary, editButton: Ids.adminNestFeatureLimitsEditButton)

    public static let guardian = FeatureHostIds(
        row: Ids.familyPolicyFeatureLimitsRow, name: Ids.familyPolicyFeatureLimitsName,
        summary: Ids.familyPolicyFeatureLimitsSummary, editButton: Ids.familyPolicyFeatureLimitsEditButton)
}

/// One row per authored member of the host's tier — name, the authored document
/// in words, an Edit button, and under the row whose editor is open the shared
/// editor itself. The admin and guardian hosts paint this; the Status rows call
/// ``FeaturePolicyEditorView`` directly from inside `FeatureLimitsSection`.
public struct FeatureAuthoringRows: View {
    let host: FeatureAuthoringHost
    let ids: FeatureHostIds

    public init(host: FeatureAuthoringHost, ids: FeatureHostIds) {
        self.host = host
        self.ids = ids
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let readError = host.readError {
                ErrorBanner(message: readError)
            }
            if let rows = host.rows {
                ForEach(Array(rows.enumerated()), id: \.offset) { index, row in
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(alignment: .center, spacing: 8) {
                            VStack(alignment: .leading, spacing: 2) {
                                automationText(ids.name, renderLocalizedText(row.name))
                                    .font(.headline)
                                automationText(ids.summary, renderLocalizedText(row.summary))
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            Spacer(minLength: 8)
                            Button(L.features.adminEdit) {
                                Task { await host.open(row.feature) }
                            }
                            .accessibilityIdentifier(ids.editButton)
                            .automationActivate(ids.editButton) {
                                Task { await host.open(row.feature) }
                            }
                        }
                        if host.openFeature == row.feature {
                            FeaturePolicyEditorView(host: host)
                        }
                    }
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier(ids.row)
                    .automationValue(ids.row, text: { renderLocalizedText(row.name) })
                    .automationScope(ids.row, index: index)
                }
            }
            if let error = host.error, host.openFeature == nil {
                ErrorBanner(message: error)
            }
        }
    }
}

/// The shared `feature-policy-editor` family's paint — ONE render for all three
/// of its hosts (admin Nest, a Status feature-limits row, the Family guardian
/// section). Twin of windows' `FeatureAuthoringPainter`, linux's
/// `feature_editor.rs`, tui's, web's and android's editor paints.
///
/// **This paints; it decides nothing.** Draft, cell list and order, labels,
/// no-effect notes, whether *Remove limit* renders and the parser behind *Save*
/// are the shared editor's, reached through ``FeatureAuthoringHost``.
public struct FeaturePolicyEditorView: View {
    let host: FeatureAuthoringHost

    public init(host: FeatureAuthoringHost) {
        self.host = host
    }

    public var body: some View {
        if let view = host.view {
            let title = renderLocalizedTextNested(view.title)
            VStack(alignment: .leading, spacing: 6) {
                automationText(Ids.featurePolicyEditorTitle, title)
                    .font(.subheadline.weight(.semibold))

                // Exactly two choices, never a third "Limit": whether On
                // carries bounds is the cells' business. `state` rides the
                // registry's value (the uniform get_attr(id, "state") idiom).
                radio(Ids.featurePolicyEditorOnRadio, L.features.editorOn, selected: view.on) {
                    host.setOn(true)
                }
                radio(Ids.featurePolicyEditorOffRadio, L.features.editorOff, selected: !view.on) {
                    host.setOn(false)
                }

                // An outer tier already denies: whatever is chosen here changes
                // nothing, and the editor says so rather than refusing anything.
                if let offNote = view.offNote {
                    Text(renderLocalizedText(offNote))
                        .font(.caption).foregroundStyle(.secondary)
                }
                if !view.cells.isEmpty {
                    Text(renderLocalizedText(view.hint))
                        .font(.caption).foregroundStyle(.secondary)
                }

                ForEach(Array(view.cells.enumerated()), id: \.offset) { index, cell in
                    cellView(index: index, cell: cell)
                }

                HStack(spacing: 8) {
                    Button(L.features.editorSave) { Task { await host.save() } }
                        .accessibilityIdentifier(Ids.featurePolicyEditorSaveButton)
                        .automationActivate(Ids.featurePolicyEditorSaveButton) {
                            Task { await host.save() }
                        }
                        .authoringGate(tier: view.tier)
                    if view.canRemove {
                        Button(L.features.editorRemove) { Task { await host.remove() } }
                            .accessibilityIdentifier(Ids.featurePolicyEditorRemoveButton)
                            .automationActivate(Ids.featurePolicyEditorRemoveButton) {
                                Task { await host.remove() }
                            }
                            .authoringGate(tier: view.tier)
                    }
                    // Cancel is local and stays live.
                    Button(L.features.editorCancel) { host.cancel() }
                        .accessibilityIdentifier(Ids.featurePolicyEditorCancelButton)
                        .automationActivate(Ids.featurePolicyEditorCancelButton) {
                            host.cancel()
                        }
                }

                if let status = host.status {
                    automationText(Ids.featurePolicyEditorStatus, status)
                        .font(.caption)
                }
                if let error = host.error {
                    ErrorBanner(message: error)
                }
            }
            .padding(10)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(Ids.featurePolicyEditor)
            .automationValue(Ids.featurePolicyEditor, text: { title })
        }
    }

    @ViewBuilder
    private func radio(_ id: String, _ label: String, selected: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 6) {
                Image(systemName: selected ? "largecircle.fill.circle" : "circle")
                Text(label)
            }
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier(id)
        .automationActivate(id, value: { selected ? "on" : "off" }, perform: action)
    }

    @ViewBuilder
    private func cellView(index: Int, cell: FfiEditorCell) -> some View {
        let label = renderLocalizedTextNested(cell.label)
        let binding = Binding<String>(
            get: { host.view?.cells[safe: index]?.text ?? cell.text },
            set: { host.setCell(index, $0) })
        VStack(alignment: .leading, spacing: 2) {
            automationText(Ids.featurePolicyEditorCellLabel, label)
                .font(.caption)
            TextField("", text: binding)
                .textFieldStyle(.roundedBorder)
                .frame(maxWidth: 220)
                .accessibilityIdentifier(Ids.featurePolicyEditorCellInput)
                .automationField(Ids.featurePolicyEditorCellInput, text: binding)
            // Present only when the ceiling already binds at or below the typed
            // value — the note is the shared editor's.
            if let note = host.view?.cells[safe: index].flatMap(editorNoteText) {
                automationText(Ids.featurePolicyEditorCellNote, note)
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.featurePolicyEditorCell)
        .automationValue(Ids.featurePolicyEditorCell, text: { label })
        .automationScope(Ids.featurePolicyEditorCell, index: index)
    }
}

private extension Array {
    subscript(safe index: Int) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}

private extension View {
    /// Save and Remove are `OnlineOnly`: the offline gate greys them from the
    /// kind the tier's write ends in (`AuthoringTier::write_kind`), declared as
    /// literals so `offline-gate-check` can grade them against the shared table.
    @ViewBuilder
    func authoringGate(tier: String) -> some View {
        switch tier {
        case "admin": faunaGate("fauna.features.policy.update")
        case "guardian": faunaGate("fauna.family.policy.update")
        default: faunaGate("fauna.features.self_limits.update")
        }
    }
}
