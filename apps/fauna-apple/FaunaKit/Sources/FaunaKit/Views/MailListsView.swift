import SwiftUI

/// The user-facing **mail-lists** page (`docs/goal/behavior/mail-mass-mailing.md`),
/// shared by macOS + iOS (one FaunaKit view, thin per-target call sites).
/// A dumb renderer of `MailListsSnapshot` + dispatcher of
/// `MailListsAction` over the shared `MailListsMachine` (via `MailListsVM`); no
/// business logic here. Element IDs match `tests/e2e-unified/ui.yaml` `mail-lists` /
/// `mail-lists-list` exactly. Reference implementation: linux
/// `apps/fauna-linux/src/settings/mail_lists.rs`.
///
/// The list backend is live (`mail-mass-mailing.md` § Implementation status
/// today); the per-row "Members" button (`mail-lists-list-item-members-button`)
/// opens the `mail-list-members` page scoped to that row's list via
/// `onNavigateToMembers` (mirrors linux's `on_navigate_to_members`,
/// `mail_lists.rs` — the shell composes it from the members page's own
/// selection state, `settings_shell.rs`'s pattern).
///
/// On macOS this is a sub-page of the Settings sidebar-swap shell
/// (`SettingsShellView` — `mail-lists` / `mail-list-members`); on iOS it is a
/// Settings sub-page (NavigationLink). The add/edit form is a sheet (like
/// `MailAliasesView`).
public struct MailListsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = MailListsVM()

    /// Which sheet is open, and with what data. `.sheet(item:)`, not
    /// `.sheet(isPresented:)` + a separate `editing` var: the two-state-var
    /// form raced — `beginEdit` set `editing` then `showingSheet`, but the
    /// presented sheet's own `onAppear` still read `editing == nil` (traced
    /// live via a debug log; a real SwiftUI content-closure/state-propagation
    /// race on first presentation, not a hydrate-timing issue). `.sheet(item:)`
    /// guarantees the content closure receives the exact value that triggered
    /// presentation.
    private enum ListSheetMode: Identifiable {
        case add
        case edit(ListView)

        var id: String {
            switch self {
            case .add: return "add"
            case .edit(let list): return list.listIdHex
            }
        }

        var editing: ListView? {
            if case .edit(let list) = self { return list }
            return nil
        }
    }

    @State private var sheetMode: ListSheetMode?
    /// The list whose destructive Delete is currently armed (two-click inline
    /// confirm, no modal — `nil` = nothing armed). Mirrors `MailAliasesView`'s
    /// `armedDeleteId` / linux `wire_two_click`.
    @State private var armedDeleteId: String?

    let onNavigateToMembers: (String, String) -> Void

    public init(onNavigateToMembers: @escaping (String, String) -> Void) {
        self.onNavigateToMembers = onNavigateToMembers
    }

    public var body: some View {
        // Eager `ScrollView { VStack }`, NOT a lazy `Form` (rule 6 —
        // apple-e2e-automation.md § Registration rules): an iOS `Form` lazily
        // realizes AND POOLS its rows, so a list row deleted from
        // `vm.snapshot?.lists` (`mail-lists-list-item-delete-button` two-click
        // delete) can linger past its real removal — the same delete-zombie
        // class rule 6 fixed for iOS Events and `MailAliasesView`. Cost: rows
        // lose the grouped `Form` styling (accepted rule-6 production-UI
        // tradeoff). The add/edit sheet keeps its `Form` — a modal presentation
        // isn't the pooled scroll list.
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                headerSection
                listSection
                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .pageTitle(L.mailLists.title)
        .task {
            guard let client else { return }
            await vm.configure(api: client.api)
        }
        .sheet(item: $sheetMode) { mode in
            MailListAddSheet(vm: vm, editing: mode.editing)
        }
    }

    // MARK: - Header

    private var headerSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.mailLists.description)
                .font(.caption)
                .foregroundStyle(.secondary)
            Button(L.mailLists.addButton) {
                beginAdd()
            }
            .accessibilityIdentifier(Ids.mailListsAddButton)
            .automationActivate(
                Ids.mailListsAddButton,
                isEnabled: { !vm.localDomains.isEmpty }
            ) { beginAdd() }
            .disabled(vm.localDomains.isEmpty)
            if vm.localDomains.isEmpty {
                Text(L.mailLists.noDomain)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func beginAdd() {
        sheetMode = .add
    }

    // MARK: - Lists

    private var listSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            let lists = vm.snapshot?.lists ?? []
            // Loading-is-not-empty (`ui/README.md` § List pages): an empty
            // `lists` pre-hydrate must not read as "no lists yet".
            if !vm.loaded {
                Text(L.mailLists.loading).foregroundStyle(.secondary)
            } else if lists.isEmpty {
                Text(L.mailLists.empty).foregroundStyle(.secondary)
            } else {
                ForEach(Array(lists.enumerated()), id: \.element.listIdHex) { _, list in
                    listRow(list)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityIdentifier(Ids.mailListsList)
    }

    @ViewBuilder
    private func listRow(_ list: ListView) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            automationText(Ids.mailListsListItemName, "\(list.friendlyName) — \(list.address)")
                .font(.headline)
                .textSelection(.enabled)
            // Bare count, no "members" suffix — matches linux/tui exactly
            // (`view.member_count.to_string()`; no other app adds the word).
            automationText(Ids.mailListsListItemMemberCount, "\(list.memberCount)")
                .font(.caption)
                .foregroundStyle(.secondary)
            automationText(Ids.mailListsListItemLastSend, lastSendText(list))
                .font(.caption)
                .foregroundStyle(.secondary)
            automationText(
                Ids.mailListsListItemQuota,
                "\(list.sendsToday) sends · \(list.recipientsToday) recipients today"
            )
            .font(.caption)
            .foregroundStyle(.secondary)
            HStack {
                Button(L.mailLists.edit) {
                    beginEdit(list)
                }
                .accessibilityIdentifier(Ids.mailListsListItemEditButton)
                .automationActivate(Ids.mailListsListItemEditButton) { beginEdit(list) }
                Button(L.mailLists.members) {
                    onNavigateToMembers(list.listIdHex, list.friendlyName)
                }
                .accessibilityIdentifier(Ids.mailListsListItemMembersButton)
                .automationActivate(Ids.mailListsListItemMembersButton) {
                    onNavigateToMembers(list.listIdHex, list.friendlyName)
                }
                Button(
                    armedDeleteId == list.listIdHex ? L.mailLists.deleteConfirm : L.mailLists.delete,
                    role: .destructive
                ) {
                    tapDelete(list)
                }
                .accessibilityIdentifier(Ids.mailListsListItemDeleteButton)
                // `text:` reports the armed/unarmed label — without it
                // `/element/text` falls back to `""` regardless of the real
                // rendered title (`AutomationRegistry.paintedTexts()`'s
                // `entry.text?() ?? entry.value?() ?? ""`), so the two-click
                // confirm's wording was never actually readable.
                .automationActivate(
                    Ids.mailListsListItemDeleteButton,
                    text: { armedDeleteId == list.listIdHex ? L.mailLists.deleteConfirm : L.mailLists.delete }
                ) { tapDelete(list) }
                // Arm and confirm are the same control (two-click inline), so
                // this is the control that issues Delete — same call as the
                // aliases row's overflow delete. Edit and Members beside it are
                // a sheet opener and a navigation, and stay live.
                .faunaGate("fauna.bridges.delete_account_list")
            }
            .controlSize(.small)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.mailListsListItem)
    }

    private func beginEdit(_ list: ListView) {
        sheetMode = .edit(list)
    }

    /// Two-click inline delete arm-then-confirm (no modal). Shared by the row's
    /// delete `Button` and its `.automationActivate` so the e2e's two taps on
    /// the same id arm then dispatch Delete (mirrors `MailAliasesView.tapDelete`
    /// / linux `wire_two_click`). Reads `armedDeleteId` live.
    private func tapDelete(_ list: ListView) {
        tapArmedDelete(list.listIdHex, armed: $armedDeleteId) {
            await vm.dispatch(.delete(listIdHex: list.listIdHex))
        }
    }

    // MARK: - Helpers

    private func lastSendText(_ list: ListView) -> String {
        if let ms = list.lastSendAtMs {
            return "last send \(ValueFormat.absoluteDate(epochMs: ms))"
        }
        return "no sends yet"
    }
}

// MARK: - Add / edit sheet (`mail-lists-add-sheet-*`)

/// Add a new list (`Create`) or edit an existing one (`Update`). The send-from
/// address (local-part + domain) is immutable on edit — those fields are disabled.
struct MailListAddSheet: View {
    let vm: MailListsVM
    let editing: ListView?

    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var localPart = ""
    @State private var domainIndex = 0
    @State private var description = ""
    @State private var listHelpUrl = ""
    @State private var listArchiveUrl = ""
    @State private var perSend = ""
    @State private var submitting = false
    @State private var error: String?

    private var isEditing: Bool { editing != nil }

    var body: some View {
        Form {
            Section {
                TextField(L.mailLists.namePlaceholder, text: $name)
                    .accessibilityIdentifier(Ids.mailListsAddSheetNameInput)
                    .automationField(Ids.mailListsAddSheetNameInput, text: $name)
                TextField(L.mailLists.localPartPlaceholder, text: $localPart)
                    .disabled(isEditing)
                    .accessibilityIdentifier(Ids.mailListsAddSheetLocalPartInput)
                    .automationField(
                        Ids.mailListsAddSheetLocalPartInput, text: $localPart,
                        isEnabled: { !isEditing }
                    )
                Picker(L.mailLists.domainLabel, selection: $domainIndex) {
                    ForEach(Array(vm.localDomains.enumerated()), id: \.offset) { idx, domain in
                        Text(domain).tag(idx)
                    }
                }
                .disabled(isEditing)
                .accessibilityIdentifier(Ids.mailListsAddSheetDomainPicker)
                .automationSelect(
                    Ids.mailListsAddSheetDomainPicker,
                    value: { vm.localDomains.indices.contains(domainIndex) ? vm.localDomains[domainIndex] : nil },
                    isEnabled: { !isEditing }
                ) { picked in
                    if let idx = vm.localDomains.firstIndex(of: picked) { domainIndex = idx }
                }
                TextField(L.mailLists.descriptionPlaceholder, text: $description)
                    .accessibilityIdentifier(Ids.mailListsAddSheetDescriptionInput)
                    .automationField(Ids.mailListsAddSheetDescriptionInput, text: $description)
                TextField(L.mailLists.listHelpPlaceholder, text: $listHelpUrl)
                    .accessibilityIdentifier(Ids.mailListsAddSheetListHelpUrlInput)
                    .automationField(Ids.mailListsAddSheetListHelpUrlInput, text: $listHelpUrl)
                TextField(L.mailLists.listArchivePlaceholder, text: $listArchiveUrl)
                    .accessibilityIdentifier(Ids.mailListsAddSheetListArchiveUrlInput)
                    .automationField(Ids.mailListsAddSheetListArchiveUrlInput, text: $listArchiveUrl)
                TextField(L.mailLists.perSendPlaceholder, text: $perSend)
                    .accessibilityIdentifier(Ids.mailListsAddSheetPerSendCapInput)
                    .automationField(Ids.mailListsAddSheetPerSendCapInput, text: $perSend)
            }

            if let error {
                Section { ErrorBanner(message: error) }
            }

            Section {
                Button(submitting ? "…" : (isEditing ? L.common.save : L.mailLists.submit)) {
                    Task { await submit() }
                }
                .disabled(submitting || name.trimmingCharacters(in: .whitespaces).isEmpty)
                .accessibilityIdentifier(Ids.mailListsAddSheetSubmitButton)
                .automationActivate(
                    Ids.mailListsAddSheetSubmitButton,
                    isEnabled: { !submitting && !name.trimmingCharacters(in: .whitespaces).isEmpty }
                ) { Task { await submit() } }
                // `submit()` branches on this same `editing != nil` — the
                // aliases-submit case one level over (tui's `MailListsSubmit` is
                // `None` for the same missing-mode reason).
                .faunaGate(isEditing
                           ? "fauna.bridges.update_account_list"
                           : "fauna.bridges.create_account_list")
                Button(L.mailLists.cancel, role: .cancel) { dismiss() }
                    .accessibilityIdentifier(Ids.mailListsAddSheetCancelButton)
                    .automationActivate(Ids.mailListsAddSheetCancelButton) { dismiss() }
            }
        }
        .formStyle(.grouped)
        .onAppear {
            if let editing {
                name = editing.friendlyName
                localPart = editing.localPart
                domainIndex = vm.localDomains.firstIndex(of: editing.localDomain) ?? 0
                description = editing.description
                listHelpUrl = editing.listHelpUrl
                listArchiveUrl = editing.listArchiveUrl
                perSend = editing.recipientsPerSend.map(String.init) ?? ""
            }
        }
    }

    private func submit() async {
        submitting = true
        defer { submitting = false }
        let domain = vm.localDomains.indices.contains(domainIndex) ? vm.localDomains[domainIndex] : ""
        let draft = ListDraft(
            friendlyName: name.trimmingCharacters(in: .whitespaces),
            localPart: localPart.trimmingCharacters(in: .whitespaces),
            localDomain: domain,
            description: description.trimmingCharacters(in: .whitespaces),
            listHelpUrl: listHelpUrl.trimmingCharacters(in: .whitespaces),
            listArchiveUrl: listArchiveUrl.trimmingCharacters(in: .whitespaces),
            recipientsPerSend: UInt32(perSend)
        )
        let action: MailListsAction
        if let editing {
            action = .update(listIdHex: editing.listIdHex, draft: draft)
        } else {
            action = .create(draft: draft)
        }
        await vm.dispatch(action)
        if let e = vm.errorMessage { error = e } else { dismiss() }
    }
}
