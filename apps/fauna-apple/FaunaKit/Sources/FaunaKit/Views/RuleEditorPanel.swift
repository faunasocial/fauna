// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// Which write a rule editor's host sends — the offline gate greys Save from it
/// (`fauna.admin.content_policy.set` | `fauna.family.policy.update`).
public enum RuleEditorWrite {
    case adminContentPolicySet
    case familyPolicyUpdate
}

/// The shared `rule-editor` component's paint — ONE view for every mounting
/// (`nest-rules.md` § The shared rule editor → *Three mountings, one shape*):
/// the admin Nest page hosts it in `admin-nest-rules-section`, the Family page in
/// `family-policy-rules-section` for the selected ward. The twin of tui
/// `rule_editor.rs`, web `RuleEditor.svelte` and android `RuleEditor.kt`.
///
/// **This view paints; it decides nothing.** The rule list, the kinds, the
/// categories, the verdicts, the level scale, every label and defect sentence and
/// whether the document may be saved are `FfiRuleEditor`'s (shared Rust,
/// `fauna_client_policy`), so the editor cannot differ between hosts or across
/// the 7 apps. A host owns the editor handle, the save and the status line; this
/// forwards the edits and calls `onSave`.
///
/// `status` is `rule-editor-status`'s text — the host's last verdict; empty shows
/// the editor's own defects. `onChange` fires on any edit, so the host clears a
/// stale "saved". `loadLabelers` / `loadModule` are the labeler picker's reads.
public struct RuleEditorPanel: View {
    let editor: FfiRuleEditor
    let write: RuleEditorWrite
    let status: String
    let saving: Bool
    let onSave: () -> Void
    let onChange: () -> Void
    let loadLabelers: () async throws -> [String]
    let loadModule: (String) async throws -> Data?

    @State private var model: RuleEditorView
    /// The keywords inputs as typed: a single-line input re-painted from the
    /// stored terms would eat a trailing comma mid-typing, so the typed text is
    /// kept per row while the editor holds the parsed terms (web's twin).
    @State private var typedKeywords: [Int: String] = [:]
    /// The publisher's subscribed labelers a rule may name, read beside the
    /// document.
    @State private var subscribed: [String] = []

    public init(
        editor: FfiRuleEditor,
        write: RuleEditorWrite,
        status: String,
        saving: Bool,
        onSave: @escaping () -> Void,
        onChange: @escaping () -> Void,
        loadLabelers: @escaping () async throws -> [String],
        loadModule: @escaping (String) async throws -> Data?
    ) {
        self.editor = editor
        self.write = write
        self.status = status
        self.saving = saving
        self.onSave = onSave
        self.onChange = onChange
        self.loadLabelers = loadLabelers
        self.loadModule = loadModule
        _model = State(initialValue: editor.view())
    }

    private func label(_ text: LocalizedText?) -> String {
        text.map(renderLocalizedText) ?? ""
    }

    private func refresh() {
        model = editor.view()
        onChange()
    }

    public var body: some View {
        let editable = model.editable
        let choices = editor.labelerChoices(subscribed: subscribed)
        VStack(alignment: .leading, spacing: 8) {
            if !editable {
                Text(L.ruleEditor.newerVersion)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            if model.rows.isEmpty {
                Text(L.ruleEditor.noRules)
                    .foregroundStyle(.secondary)
            }
            ForEach(Array(model.rows.enumerated()), id: \.offset) { offset, row in
                ruleRow(row, index: offset, editable: editable, choices: choices)
                    .padding(.vertical, 4)
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier(Ids.ruleEditorItem)
                    .automationValue(Ids.ruleEditorItem, text: { label(row.kindLabel) })
                    .automationScope(Ids.ruleEditorItem, index: offset)
            }
            HStack(spacing: 8) {
                Button(L.ruleEditor.addButton) { addRule() }
                    .disabled(!editable)
                    .accessibilityIdentifier(Ids.ruleEditorAddButton)
                    .automationActivate(Ids.ruleEditorAddButton, isEnabled: { editable }) { addRule() }
                saveButton(editable: editable)
            }
            Text(status.isEmpty ? model.defects.map(renderLocalizedText).joined(separator: " ") : status)
                .font(.caption)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier(Ids.ruleEditorStatus)
                .automationValue(
                    Ids.ruleEditorStatus,
                    text: { status.isEmpty ? model.defects.map(renderLocalizedText).joined(separator: " ") : status })
        }
        .task(id: ObjectIdentifier(editor)) {
            subscribed = (try? await loadLabelers()) ?? []
        }
    }

    private func addRule() {
        _ = editor.addRule(kind: .category)
        refresh()
    }

    /// The gate's kind is spelled out per host: `check-offline-gate-kinds.py`
    /// grades each literal against the shared table.
    @ViewBuilder
    private func saveButton(editable: Bool) -> some View {
        let enabled = !saving && model.canSave
        switch write {
        case .adminContentPolicySet:
            Button(saving ? L.ruleEditor.saving : L.ruleEditor.saveButton) { onSave() }
                .buttonStyle(.borderedProminent)
                .disabled(!enabled)
                .accessibilityIdentifier(Ids.ruleEditorSaveButton)
                .automationActivate(Ids.ruleEditorSaveButton, isEnabled: { enabled }) { onSave() }
                .faunaGate("fauna.admin.content_policy.set")
        case .familyPolicyUpdate:
            Button(saving ? L.ruleEditor.saving : L.ruleEditor.saveButton) { onSave() }
                .buttonStyle(.borderedProminent)
                .disabled(!enabled)
                .accessibilityIdentifier(Ids.ruleEditorSaveButton)
                .automationActivate(Ids.ruleEditorSaveButton, isEnabled: { enabled }) { onSave() }
                .faunaGate("fauna.family.policy.update")
        }
    }

    @ViewBuilder
    private func ruleRow(
        _ row: RuleRow, index: Int, editable: Bool, choices: [LabelerChoice]
    ) -> some View {
        let idx = UInt32(index)
        VStack(alignment: .leading, spacing: 4) {
            Text(L.ruleEditor.itemLabel(rule: String(row.number)))
                .font(.caption)
                .foregroundStyle(.secondary)
            if row.readOnly {
                Text(label(row.kindLabel))
            } else {
                // Every picker's option IS the label the crate painted (the
                // cross-app select contract), so a pick maps back through the
                // view's own lists by label.
                labelPicker(
                    id: Ids.ruleEditorItemKindSelect,
                    title: L.ruleEditor.kindLabel,
                    options: model.kinds.map { label($0.label) },
                    selected: label(row.kindLabel),
                    enabled: editable
                ) { k in
                    if editor.setKind(index: idx, kind: model.kinds[k].kind) {
                        typedKeywords.removeAll()
                        refresh()
                    }
                }
                subject(row, idx: idx, index: index, editable: editable, choices: choices)
                labelPicker(
                    id: Ids.ruleEditorItemVerdictSelect,
                    title: L.ruleEditor.verdictLabel,
                    options: model.verdicts.map { label($0.label) },
                    selected: label(model.verdicts.first { $0.verdict == row.verdict }?.label),
                    enabled: editable
                ) { v in
                    if editor.setVerdict(index: idx, verdict: model.verdicts[v].verdict) { refresh() }
                }
                if let level = row.levelIndex {
                    labelPicker(
                        id: Ids.ruleEditorItemLevelSelect,
                        title: L.ruleEditor.levelLabel,
                        options: model.levels.map { label($0.label) },
                        selected: label(model.levels.indices.contains(Int(level)) ? model.levels[Int(level)].label : nil),
                        enabled: editable
                    ) { l in
                        if editor.setLevel(index: idx, level: UInt32(l)) { refresh() }
                    }
                }
                textInput(
                    id: Ids.ruleEditorItemReasonInput,
                    title: L.ruleEditor.reasonLabel,
                    enabled: editable,
                    text: Binding(
                        get: { row.reasons.first { $0.lang == model.reasonDefaultKey }?.text ?? "" },
                        set: { text in
                            _ = editor.setReason(index: idx, lang: model.reasonDefaultKey, text: text)
                            refresh()
                        }))
            }
            if let inert = row.inert {
                Text(label(inert))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Button(L.ruleEditor.removeButton) {
                if editor.removeRule(index: idx) {
                    // Rows after this one shift up; their typed text is re-read
                    // from the stored terms.
                    typedKeywords.removeAll()
                    refresh()
                }
            }
            .accessibilityIdentifier(Ids.ruleEditorItemRemoveButton)
            .automationActivate(Ids.ruleEditorItemRemoveButton) {
                if editor.removeRule(index: idx) {
                    typedKeywords.removeAll()
                    refresh()
                }
            }
        }
    }

    /// The row's one subject control, by kind: a category picker, the keywords
    /// or prompt input, or the labeler picker.
    @ViewBuilder
    private func subject(
        _ row: RuleRow, idx: UInt32, index: Int, editable: Bool, choices: [LabelerChoice]
    ) -> some View {
        if let category = row.category {
            labelPicker(
                id: Ids.ruleEditorItemCategorySelect,
                title: L.ruleEditor.categoryLabel,
                options: model.categories.map { label($0.label) },
                selected: label(model.categories.first { $0.category == category }?.label),
                enabled: editable
            ) { c in
                if editor.setCategory(index: idx, category: model.categories[c].category) { refresh() }
            }
        } else if let keywords = row.keywords {
            textInput(
                id: Ids.ruleEditorItemKeywordsInput,
                title: L.ruleEditor.keywordsLabel,
                enabled: editable,
                text: Binding(
                    get: { typedKeywords[index] ?? ruleKeywordsInline(terms: keywords) },
                    set: { text in
                        typedKeywords[index] = text
                        _ = editor.setKeywordsInline(index: idx, text: text)
                        refresh()
                    }))
        } else if let labeler = row.labeler {
            labelPicker(
                id: Ids.ruleEditorItemLabelerSelect,
                title: L.ruleEditor.labelerLabel,
                options: choices.map(\.name),
                selected: labeler,
                enabled: editable
            ) { l in
                pickLabeler(choices[l], idx: idx)
            }
        } else if let prompt = row.prompt {
            textInput(
                id: Ids.ruleEditorItemPromptInput,
                title: L.ruleEditor.promptLabel,
                enabled: editable,
                text: Binding(
                    get: { prompt },
                    set: { text in
                        _ = editor.setPrompt(index: idx, text: text)
                        refresh()
                    }))
        }
    }

    private func pickLabeler(_ choice: LabelerChoice, idx: UInt32) {
        Task { @MainActor in
            let ok: Bool
            if let id = choice.labelerId {
                // A subscribed labeler the document does not bundle yet: fetch
                // its module, hand it over under the name.
                let module = try? await loadModule(id)
                ok = module.map { editor.setLabeler(index: idx, name: choice.name, module: $0) } ?? false
            } else {
                ok = editor.chooseBundledLabeler(index: idx, name: choice.name)
            }
            if ok { refresh() }
        }
    }

    /// A cross-app label picker (ui.yaml types `rule-editor-item-*-select` as
    /// `select`): the anchor shows the crate's painted label, and each option is
    /// one — the e2e `RuleEditorActions.pick` drives `driver.select(id, label)`
    /// and `picked` reads the anchor back.
    private func labelPicker(
        id: String, title: String, options: [String], selected: String, enabled: Bool,
        onPick: @escaping (Int) -> Void
    ) -> some View {
        Picker(title, selection: Binding(
            get: { selected },
            set: { picked in
                if let k = options.firstIndex(of: picked) { onPick(k) }
            })
        ) {
            // The current label stays a tagged option so a crate-painted label
            // that is not among `options` (a stale read) never leaves the
            // picker without a selection.
            ForEach(Array(options.enumerated()), id: \.offset) { _, option in
                Text(option).tag(option)
            }
        }
        .pickerStyle(.menu)
        .disabled(!enabled)
        .accessibilityIdentifier(id)
        .automationSelect(
            id,
            value: { selected },
            options: { options },
            isEnabled: { enabled }
        ) { picked in
            if let k = options.firstIndex(of: picked) { onPick(k) }
        }
    }

    private func textInput(id: String, title: String, enabled: Bool, text: Binding<String>) -> some View {
        TextField(title, text: text)
            .textFieldStyle(.roundedBorder)
            .disabled(!enabled)
            .accessibilityIdentifier(id)
            .automationField(id, text: text, isEnabled: { enabled })
    }
}

#endif // !FAUNA_KIDS
