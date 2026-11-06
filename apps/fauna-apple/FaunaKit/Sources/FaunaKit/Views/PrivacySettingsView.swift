import SwiftUI

/// The user-facing **Privacy** settings sub-page (`docs/goal/ui/settings.md` § Privacy),
/// shared by macOS + iOS — one FaunaKit view, bare-called from
/// each platform's settings shell (and the macOS Moderation page). Renders the inbox-mode
/// selector, the email-filter list/editor, and the shared `SpamPreferencesView` over the
/// shared `PrivacySettingsVM`.
///
/// Inbox modes use the **canonical** set — `open` / `allow_knock` / `contacts_only` /
/// `closed` — matching `tests/e2e-unified/ui.yaml` `inbox-mode-selector` and the linux lead
/// (`apps/fauna-linux/src/settings/privacy.rs`). The radio rows carry the spec'd
/// `inbox-mode-<id>` IDs. (Before this lift iOS rendered a bare `Picker` sending a
/// non-canonical set `allow_all`/`allow_confirmed`/`deny_all` with no IDs — a conformance
/// bug this fixes.) Session bits come from the shared `FaunaClient` (its own secret
/// + `actor_id_from_secret`), so no `MacAppState`/`AppState` seam is needed.
public struct PrivacySettingsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = PrivacySettingsVM()

    /// Per-platform app-state writeback for the selected inbox mode so the e2e
    /// state snapshot (`get_state("settings").inbox_mode`) reflects it — the
    /// shared view itself can't reach the platform `AppState`/`MacAppState` (the
    /// `@Environment` types differ across targets), so each settings shell injects
    /// it (the documented "pass app-state side-effects via `init`" seam).
    private let onInboxModeChanged: ((String) -> Void)?

    // Email-filter draft form state: the whole draft (rule inputs, action inputs,
    // order, `continue`), so an edit of any filter round-trips through the shared
    // encoders unchanged.
    @State private var draft = EmailFilterDraft()

    public init(onInboxModeChanged: ((String) -> Void)? = nil) {
        self.onInboxModeChanged = onInboxModeChanged
    }

    public var body: some View {
        // Eager `ScrollView { VStack }`, NOT a lazy `Form` (rule 6 —
        // apple-e2e-automation.md § Registration rules): an iOS `Form` lazily
        // realizes AND POOLS its rows, so an email-filter row deleted from
        // `vm.emailFilters` (`filter-delete`) can linger past its real removal —
        // the server-side delete succeeds but the pooled `filter-item`/`filter-delete`
        // registry entries never drop. Same delete-zombie class already fixed for
        // MailAliasesView/NostrSettingsView. Cost: rows lose the grouped
        // `Form` styling (accepted rule-6 production-UI tradeoff).
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                VStack(alignment: .leading, spacing: 8) {
                    Text(L.status.inboxPrivacy.title).font(.headline)
                    // The value set, order, label AND description come from the
                    // shared Rust catalog, not hand-typed literals
                    // (settings.md § Where logic lives → *The inbox-mode
                    // selector's rows*) — mirrors
                    // `AdminUsersHubView.registrationModePicker`.
                    ForEach(inboxModeOptions(), id: \.value) { option in
                        Button(action: { Task { await vm.updateInboxMode(option.value) } }) {
                            HStack {
                                Image(systemName: vm.inboxMode == option.value ? "circle.inset.filled" : "circle")
                                VStack(alignment: .leading) {
                                    Text(renderLocalizedText(option.label)).fontWeight(.medium)
                                    Text(renderLocalizedText(option.desc)).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                        }
                        .buttonStyle(.plain)
                        .disabled(vm.inboxModeLoading)
                        .accessibilityIdentifier("inbox-mode-\(option.value)")
                        .automationActivate("inbox-mode-\(option.value)",
                                            isEnabled: { !vm.inboxModeLoading }) {
                            Task { await vm.updateInboxMode(option.value) }
                        }
                    }
                    if let error = vm.inboxModeError {
                        ErrorBanner(message: error)
                    } else if vm.inboxMode == nil {
                        // Painted while the real mode is unfetched — the four
                        // radios above already show none selected (the `==`
                        // against a `nil` inboxMode is false for every
                        // option), and this names why on the page's shared
                        // error-message element (settings.md § Privacy
                        // sub-page item 6).
                        ErrorBanner(message: L.settings.privacyPage.inboxModeUnknown)
                    }
                }

                VStack(alignment: .leading, spacing: 8) {
                    Text(L.settings.privacyPage.emailFilters).font(.headline)
                    if vm.emailFilters.isEmpty {
                        Text(L.status.emailFilters.none)
                            .foregroundStyle(.secondary)
                    }
                    ForEach(Array(vm.emailFilters.enumerated()), id: \.element.id) { offset, filter in
                        HStack {
                            automationText(Ids.filterName, filter.name).fontWeight(.medium)
                            Spacer()
                            automationText(Ids.filterAction, filter.action.label)
                                .font(.caption2)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .background(.secondary.opacity(0.15))
                                .clipShape(Capsule())
                            // The full form's gate: it covers every condition and
                            // action this form collects (`...For` is the narrower
                            // single-value-condition form's gate).
                            if emailFilterIsEditable(rules: filter.rules, action: filter.action) {
                                Button {
                                    Task { await beginEdit(filter.id) }
                                } label: {
                                    Image(systemName: "pencil")
                                }
                                .controlSize(.small)
                                .accessibilityIdentifier(Ids.filterEdit)
                                .automationActivate(Ids.filterEdit) {
                                    Task { await beginEdit(filter.id) }
                                }
                            }
                            // The post-succession review mark and its Keep half
                            // (`succession-aftermath.md` § Adjudicating what the
                            // aftermath carries across) — only on a rule the
                            // aftermath carried across and the owner has not
                            // answered. No remove button of its own: `filter-delete`
                            // below is the Remove half (no second removal mechanism).
                            if vm.filterMarks.contains(filter.id) {
                                automationText(Ids.filterUnattestedMark,
                                               L.settings.privacyPage.filterInherited)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                Button(L.settings.privacyPage.filterKeep) {
                                    Task { await vm.keepFilterMark(id: filter.id) }
                                }
                                .controlSize(.small)
                                .accessibilityIdentifier(Ids.filterReviewKeepButton)
                                .automationActivate(Ids.filterReviewKeepButton) {
                                    Task { await vm.keepFilterMark(id: filter.id) }
                                }
                            }
                            Button(L.common.delete, role: .destructive) {
                                Task { await vm.deleteFilter(id: filter.id) }
                            }
                            .controlSize(.small)
                            .accessibilityIdentifier(Ids.filterDelete)
                            .automationActivate(Ids.filterDelete) {
                                Task { await vm.deleteFilter(id: filter.id) }
                            }
                        }
                        // `.contain` keeps the `filter-item` container id from clobbering the
                        // child ids (memory `apple-section-accessibilityid-clobbers-children`).
                        .accessibilityElement(children: .contain)
                        .accessibilityIdentifier(Ids.filterItem)
                        // Registry read so the in-process driver can count/locate each row
                        // (no a11y tree in-process); value = the row's filter name.
                        .automationValue(Ids.filterItem, text: { filter.name })
                        // Scoped container (goal-doc rule 5): `filter-action`/`filter-delete`
                        // are queried via `scope="filter-item[N]/..."`.
                        .automationScope(Ids.filterItem, index: offset)
                    }

                    if !vm.showFilterForm {
                        Button(L.settings.privacyPage.addFilter) { vm.showFilterForm = true }
                            .accessibilityIdentifier(Ids.addFilterBtn)
                            .automationActivate(Ids.addFilterBtn) { vm.showFilterForm = true }
                    } else {
                        VStack(spacing: 8) {
                            TextField(L.settings.filterName, text: $draft.name)
                                .textFieldStyle(.roundedBorder)
                                .accessibilityIdentifier(Ids.filterNameInput)
                                .automationField(Ids.filterNameInput, text: $draft.name)
                            Picker(L.settings.ruleType, selection: $draft.rule.kind) {
                                // Each option carries its stable key as the
                                // accessibilityIdentifier (the bridge `select(id,value)`
                                // matches the opened menu item by id, not the localized
                                // label — `apple-bridge/Sources/AppleBridge/Actions.swift`
                                // § select; the visible label ≠ the key, e.g. "Sender is"
                                // vs "SenderIs"). Mirrors the `MacEventListView` viewMode
                                // picker.
                                ForEach(EmailFilterOptions.ruleKinds, id: \.tag) {
                                    Text($0.label).tag($0.tag).accessibilityIdentifier($0.tag)
                                }
                            }
                            .accessibilityIdentifier(Ids.filterRuleType)
                            // Picker: read the current tag + select by tag (the wire
                            // value is the stable key, e.g. "SenderIs", = the binding).
                            .automationSelect(Ids.filterRuleType, value: { draft.rule.kind }) { draft.rule.kind = $0 }
                            TextField(L.settings.ruleValue, text: $draft.rule.value)
                                .textFieldStyle(.roundedBorder)
                                #if os(iOS)
                                .textInputAutocapitalization(.never)
                                #endif
                                .accessibilityIdentifier(Ids.filterRuleValue)
                                .automationField(Ids.filterRuleValue, text: $draft.rule.value)
                            // The two conditions with a second input (`email-filters.md`
                            // § Email filter rules): a header's value beside its name,
                            // and a spam score.
                            if draft.rule.kind == "HeaderContains" {
                                filterField(L.settings.privacyPage.headerValue, $draft.rule.headerValue,
                                            Ids.filterRuleHeaderValue)
                            } else if draft.rule.kind == "SpamScoreAtLeast" {
                                filterField(L.settings.privacyPage.spamScore, $draft.rule.spamScore,
                                            Ids.filterRuleSpamScore)
                            }
                            Picker(L.settings.privacyPage.action, selection: $draft.action.kind) {
                                ForEach(EmailFilterOptions.actions, id: \.tag) {
                                    Text($0.label).tag($0.tag).accessibilityIdentifier($0.tag)
                                }
                            }
                            .accessibilityIdentifier(Ids.filterActionSelect)
                            // Picker: read the current tag + select by tag.
                            .automationSelect(Ids.filterActionSelect, value: { draft.action.kind }) { draft.action.kind = $0 }
                            // Each action's own inputs, shown only while it is the
                            // selected action (settings.md § Where logic lives → *Email
                            // filter create-dialog encoding*); every value is validated
                            // by the shared encoder on submit, not here.
                            switch draft.action.kind {
                            case "Forward":
                                filterField(L.settings.privacyPage.forwardAddress,
                                            $draft.action.forwardAddress, Ids.filterForwardAddress,
                                            email: true)
                                Toggle(L.settings.privacyPage.keepLocalCopy,
                                       isOn: $draft.action.keepLocalCopy)
                                    .accessibilityIdentifier(Ids.filterKeepLocalCopy)
                                    // Toggle: activate flips the same bound state a tap
                                    // would; value backs the `state` read ("on"/"off").
                                    .automationActivate(Ids.filterKeepLocalCopy,
                                                        value: { draft.action.keepLocalCopy ? "on" : "off" }) {
                                        draft.action.keepLocalCopy.toggle()
                                    }
                            case "FileInto":
                                filterField(L.settings.privacyPage.fileIntoMailbox,
                                            $draft.action.fileIntoMailbox, Ids.filterFileIntoMailbox)
                            case "AddLabel":
                                filterField(L.settings.privacyPage.addLabel,
                                            $draft.action.label, Ids.filterAddLabel)
                            case "AutoReply":
                                filterField(L.settings.privacyPage.autoReplySubject,
                                            $draft.action.autoReplySubject, Ids.filterAutoReplySubject)
                                filterField(L.settings.privacyPage.autoReplyBody,
                                            $draft.action.autoReplyBody, Ids.filterAutoReplyBody)
                                filterField(L.settings.privacyPage.autoReplyIntervalHours,
                                            $draft.action.autoReplyIntervalHours,
                                            Ids.filterAutoReplyIntervalHours)
                            default:
                                EmptyView()
                            }
                            // The rule's place in the order and its `continue` flag
                            // (`email-filters.md` § Email filter rules: rules run in
                            // order, the first match wins unless it is marked to keep
                            // going).
                            filterField(L.settings.privacyPage.priority, $draft.priority,
                                        Ids.filterPriorityInput)
                            Toggle(L.settings.privacyPage.continueMatching,
                                   isOn: $draft.continueOnMatch)
                                .accessibilityIdentifier(Ids.filterContinueToggle)
                                .automationActivate(Ids.filterContinueToggle,
                                                    value: { draft.continueOnMatch ? "on" : "off" }) {
                                    draft.continueOnMatch.toggle()
                                }
                            HStack {
                                if vm.editingFilterId != nil {
                                    Button(L.common.save) { submitFilter() }
                                    .disabled(!canSubmitFilter)
                                    .buttonStyle(.borderedProminent)
                                    .controlSize(.small)
                                    .accessibilityIdentifier(Ids.saveFilter)
                                    .automationActivate(Ids.saveFilter,
                                                        isEnabled: { canSubmitFilter }) {
                                        submitFilter()
                                    }
                                } else {
                                    Button(L.common.create) { submitFilter() }
                                    .disabled(!canSubmitFilter)
                                    .buttonStyle(.borderedProminent)
                                    .controlSize(.small)
                                    .accessibilityIdentifier(Ids.createFilter)
                                    .automationActivate(Ids.createFilter,
                                                        isEnabled: { canSubmitFilter }) {
                                        submitFilter()
                                    }
                                }
                                Button(L.common.cancel) {
                                    vm.showFilterForm = false
                                    vm.editingFilterId = nil
                                }
                                    .controlSize(.small)
                            }
                        }
                    }
                    if let error = vm.emailFilterError {
                        ErrorBanner(message: error)
                    }
                }

                SpamPreferencesView(vm: vm)
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        #if os(iOS)
        .pageTitle(L.settings.privacyPage.title)
        #endif
        .task {
            // The client's OWN actor, never the registry's active account: that
            // is, on a bound instance, possibly
            // another account (`FaunaClient.ownSecretHex`).
            if let client, let actorId = client.ownActorIdHex {
                vm.configure(api: client.api, actorId: actorId, client: client,
                             onInboxModeChanged: onInboxModeChanged)
            }
            await vm.loadAll()
        }
    }

    /// Whether the draft names its filter and fills the condition's own input: a
    /// spam-score condition reads the score field, every other the value field.
    /// The shared encoder still validates what was typed.
    private var canSubmitFilter: Bool {
        let conditionFilled = draft.rule.kind == "SpamScoreAtLeast"
            ? !draft.rule.spamScore.isEmpty
            : !draft.rule.value.isEmpty
        return !draft.name.isEmpty && conditionFilled && !vm.creatingFilter
    }

    /// One labelled text input of the filter form, registered under `id`.
    private func filterField(_ label: String, _ text: Binding<String>, _ id: String,
                             email: Bool = false) -> some View {
        TextField(label, text: text)
            .textFieldStyle(.roundedBorder)
            #if os(iOS)
            .textInputAutocapitalization(.never)
            .keyboardType(email ? .emailAddress : .default)
            #endif
            .accessibilityIdentifier(id)
            .automationField(id, text: text)
    }

    /// Submit the email-filter draft — create or update depending on
    /// `vm.editingFilterId`, the same shared form either way. Extracted so
    /// each mode's Button and its `automationActivate` sibling invoke the
    /// exact same path (no drift).
    private func submitFilter() {
        Task {
            if let id = vm.editingFilterId {
                await vm.updateFilter(id: id, draft)
            } else {
                await vm.createFilter(draft)
            }
            // Keep the kinds selected; clear what was typed.
            draft = EmailFilterDraft(
                rule: FfiFilterRuleInputs(kind: draft.rule.kind, value: ""),
                action: FfiFilterActionInputs(kind: draft.action.kind, rejectReason: "",
                                              forwardAddress: "", keepLocalCopy: true))
        }
    }

    /// Open the shared form pre-populated for editing `id` (`filter-edit`).
    private func beginEdit(_ id: Int64) async {
        guard let loaded = await vm.beginEditFilter(id: id) else { return }
        draft = loaded
    }
}
