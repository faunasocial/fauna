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

    // Email-filter draft form state.
    @State private var filterName = ""
    @State private var filterRuleType = "SenderIs"
    @State private var filterRuleValue = ""
    /// The whole action-inputs struct, not just the kind tag: a Forward's
    /// destination and copy mode and a Reject's reason ride in it, so an edit
    /// of any action round-trips through the shared encoder unchanged.
    @State private var filterAction = Self.blankActionInputs(kind: "Allow")

    private static func blankActionInputs(kind: String) -> FfiFilterActionInputs {
        FfiFilterActionInputs(kind: kind, rejectReason: "", forwardAddress: "", keepLocalCopy: true)
    }

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
                            if emailFilterIsEditableFor(rules: filter.rules, action: filter.action,
                                                        actionKinds: EmailFilterOptions.actions.map(\.tag)) {
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
                            TextField(L.settings.filterName, text: $filterName)
                                .textFieldStyle(.roundedBorder)
                                .accessibilityIdentifier(Ids.filterNameInput)
                                .automationField(Ids.filterNameInput, text: $filterName)
                            Picker(L.settings.ruleType, selection: $filterRuleType) {
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
                            .automationSelect(Ids.filterRuleType, value: { filterRuleType }) { filterRuleType = $0 }
                            TextField(L.settings.ruleValue, text: $filterRuleValue)
                                .textFieldStyle(.roundedBorder)
                                #if os(iOS)
                                .textInputAutocapitalization(.never)
                                #endif
                                .accessibilityIdentifier(Ids.filterRuleValue)
                                .automationField(Ids.filterRuleValue, text: $filterRuleValue)
                            Picker(L.settings.privacyPage.action, selection: $filterAction.kind) {
                                ForEach(EmailFilterOptions.actions, id: \.tag) {
                                    Text($0.label).tag($0.tag).accessibilityIdentifier($0.tag)
                                }
                            }
                            .accessibilityIdentifier(Ids.filterActionSelect)
                            // Picker: read the current tag + select by tag.
                            .automationSelect(Ids.filterActionSelect, value: { filterAction.kind }) { filterAction.kind = $0 }
                            // The Forward inputs — shown only while the action reads
                            // Forward (settings.md § Where logic lives → *Email filter
                            // create-dialog encoding*); the destination is validated by
                            // the shared encoder on submit, not here.
                            if filterAction.kind == "Forward" {
                                TextField(L.settings.privacyPage.forwardAddress,
                                          text: $filterAction.forwardAddress)
                                    .textFieldStyle(.roundedBorder)
                                    #if os(iOS)
                                    .textInputAutocapitalization(.never)
                                    .keyboardType(.emailAddress)
                                    #endif
                                    .accessibilityIdentifier(Ids.filterForwardAddress)
                                    .automationField(Ids.filterForwardAddress,
                                                     text: $filterAction.forwardAddress)
                                Toggle(L.settings.privacyPage.keepLocalCopy,
                                       isOn: $filterAction.keepLocalCopy)
                                    .accessibilityIdentifier(Ids.filterKeepLocalCopy)
                                    // Toggle: activate flips the same bound state a tap
                                    // would; value backs the `state` read ("on"/"off").
                                    .automationActivate(Ids.filterKeepLocalCopy,
                                                        value: { filterAction.keepLocalCopy ? "on" : "off" }) {
                                        filterAction.keepLocalCopy.toggle()
                                    }
                            }
                            HStack {
                                if vm.editingFilterId != nil {
                                    Button(L.common.save) { submitFilter() }
                                    .disabled(filterName.isEmpty || filterRuleValue.isEmpty || vm.creatingFilter)
                                    .buttonStyle(.borderedProminent)
                                    .controlSize(.small)
                                    .accessibilityIdentifier(Ids.saveFilter)
                                    .automationActivate(Ids.saveFilter,
                                                        isEnabled: { !filterName.isEmpty && !filterRuleValue.isEmpty && !vm.creatingFilter }) {
                                        submitFilter()
                                    }
                                } else {
                                    Button(L.common.create) { submitFilter() }
                                    .disabled(filterName.isEmpty || filterRuleValue.isEmpty || vm.creatingFilter)
                                    .buttonStyle(.borderedProminent)
                                    .controlSize(.small)
                                    .accessibilityIdentifier(Ids.createFilter)
                                    .automationActivate(Ids.createFilter,
                                                        isEnabled: { !filterName.isEmpty && !filterRuleValue.isEmpty && !vm.creatingFilter }) {
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

    /// Submit the email-filter draft — create or update depending on
    /// `vm.editingFilterId`, the same shared form either way. Extracted so
    /// each mode's Button and its `automationActivate` sibling invoke the
    /// exact same path (no drift).
    private func submitFilter() {
        Task {
            if let id = vm.editingFilterId {
                await vm.updateFilter(id: id, name: filterName, ruleType: filterRuleType,
                                      ruleValue: filterRuleValue, action: filterAction)
            } else {
                await vm.createFilter(name: filterName, ruleType: filterRuleType,
                                      ruleValue: filterRuleValue, action: filterAction)
            }
            filterName = ""
            filterRuleValue = ""
            filterAction = Self.blankActionInputs(kind: filterAction.kind)
        }
    }

    /// Open the shared form pre-populated for editing `id` (`filter-edit`).
    private func beginEdit(_ id: Int64) async {
        guard let draft = await vm.beginEditFilter(id: id) else { return }
        filterName = draft.name
        filterRuleType = draft.ruleType
        filterRuleValue = draft.ruleValue
        filterAction = draft.action
    }
}
