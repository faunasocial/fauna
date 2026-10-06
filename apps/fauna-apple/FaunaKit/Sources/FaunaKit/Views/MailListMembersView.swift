import SwiftUI

/// The user-facing **mail-list-members** page (`docs/goal/behavior/mail-mass-mailing.md`),
/// shared by macOS + iOS (one FaunaKit view), scoped to one
/// mailing list. A dumb renderer of `MailListMembersSnapshot` + dispatcher of
/// `MailListMembersAction` over the shared `MailListMembersMachine` (via
/// `MailListMembersVM`); no business logic here. Element IDs match
/// `tests/e2e-unified/ui.yaml` `mail-list-members` / `mail-list-members-list`
/// exactly. Reference implementation: linux
/// `apps/fauna-linux/src/settings/mail_list_members.rs`.
///
/// Reached scoped to a specific list via the `mail-lists` row's "Members"
/// button (`onNavigateToMembers`, wired by the settings shell), or directly
/// via the settings rail's own `mail-list-members` slot with nothing
/// selected — the two call sites (`SettingsShellView`/`SettingsView`) pass
/// this view no id at all in that case, and `MailListMembersVM.configure`
/// resolves the placeholder to the caller's first owned list before vending
/// the scoped machine (`mail-mass-mailing.md` § Per-app render status), never
/// a silent fall-through to another view.
public struct MailListMembersView: View {
    /// Placeholder list id for the embedded seed and for a direct rail visit
    /// with no owned lists at all (the honest empty state) — `configure`
    /// resolves it to the first owned list when one exists.
    public static let placeholderListId = "00000000000000000000000000000000"

    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = MailListMembersVM()

    @State private var showingAddSheet = false
    @State private var showingImportSheet = false

    private let listIdHex: String
    private let listName: String

    public init(listIdHex: String = MailListMembersView.placeholderListId, listName: String = L.mailLists.membersTitle) {
        self.listIdHex = listIdHex
        self.listName = listName
    }

    public var body: some View {
        // Eager `ScrollView { VStack }`, NOT a lazy `Form` (rule 6 —
        // apple-e2e-automation.md § Registration rules): mirrors `MailAliasesView`
        // — a member roster that grows past one screenful must not leave rows
        // unregistered below the fold on iOS. The add/import SHEETS below keep
        // their `Form` — a modal presentation isn't the pooled scroll list.
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                summarySection
                listSection
                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .pageTitle(listName)
        .task {
            guard let client else { return }
            await vm.configure(api: client.api, listIdHex: listIdHex, listName: listName)
        }
        .sheet(isPresented: $showingAddSheet) {
            MailListMemberAddSheet(vm: vm)
        }
        .sheet(isPresented: $showingImportSheet) {
            MailListMemberImportSheet(vm: vm)
        }
    }

    // MARK: - Summary + actions

    private var summarySection: some View {
        VStack(alignment: .leading, spacing: 8) {
            automationText(Ids.mailListMembersSummary, summaryText)
            Button(L.mailLists.addMemberButton) { showingAddSheet = true }
                .accessibilityIdentifier(Ids.mailListMembersAddButton)
                .automationActivate(Ids.mailListMembersAddButton) { showingAddSheet = true }
            Button(L.mailLists.importButton) { showingImportSheet = true }
                .accessibilityIdentifier(Ids.mailListMembersImportButton)
                .automationActivate(Ids.mailListMembersImportButton) { showingImportSheet = true }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var summaryText: String {
        guard let snap = vm.snapshot else {
            return L.mailLists.summaryFmt(subscribed: "0", unsubscribed: "0")
        }
        return L.mailLists.summaryFmt(
            subscribed: "\(snap.subscribedCount)",
            unsubscribed: "\(snap.unsubscribedCount)"
        )
    }

    // MARK: - Members list

    private var listSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            let members = vm.snapshot?.members ?? []
            // Loading-is-not-empty (`ui/README.md` § List pages). Once loaded,
            // an empty roster shows no extra reason — the summary line above
            // already says "0 subscribed · 0 unsubscribed" (matches linux/tui,
            // which likewise render no placeholder once a snapshot has landed,
            // regardless of member count). Was previously borrowing the Lists
            // page's `empty` string ("No lists yet"), which on a page headed
            // "Members" reads as the wrong claim — `mail_lists.members_loading`
            // is the page's own key for the one state it does need to announce.
            if !vm.loaded {
                Text(L.mailLists.membersLoading).foregroundStyle(.secondary)
            } else if !members.isEmpty {
                ForEach(Array(members.enumerated()), id: \.element.address) { _, member in
                    memberRow(member)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityIdentifier(Ids.mailListMembersList)
    }

    @ViewBuilder
    private func memberRow(_ member: MemberView) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            automationText(Ids.mailListMembersListItemAddress, member.address)
                .font(.headline)
                .textSelection(.enabled)
            automationText(Ids.mailListMembersListItemSubscribedAt, subscribedAtText(member))
                .font(.caption)
                .foregroundStyle(.secondary)
            automationText(
                Ids.mailListMembersListItemStatus,
                renderLocalizedText(memberStatusLabel(status: member.status))
            )
            .font(.caption)
            .foregroundStyle(.secondary)
            HStack {
                Button(L.mailLists.unsubscribe) {
                    unsubscribe(member)
                }
                .disabled(member.status != .subscribed)
                .accessibilityIdentifier(Ids.mailListMembersListItemUnsubscribeButton)
                .automationActivate(
                    Ids.mailListMembersListItemUnsubscribeButton,
                    isEnabled: { member.status == .subscribed }
                ) { unsubscribe(member) }
                // Unlike the aliases toggle, the two directions are two separate
                // controls here, each with its own status predicate — so each
                // declares its own kind and no discriminant is needed. The gate
                // folds into the call site's predicate, never replacing it.
                .faunaGate("fauna.bridges.unsubscribe_list_member")
                Button(L.mailLists.resubscribe) {
                    resubscribe(member)
                }
                .disabled(member.status != .unsubscribed)
                .accessibilityIdentifier(Ids.mailListMembersListItemResubscribeButton)
                .automationActivate(
                    Ids.mailListMembersListItemResubscribeButton,
                    isEnabled: { member.status == .unsubscribed }
                ) { resubscribe(member) }
                .faunaGate("fauna.bridges.resubscribe_list_member")
            }
            .controlSize(.small)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.mailListMembersListItem)
    }

    private func unsubscribe(_ member: MemberView) {
        Task { await vm.dispatch(.unsubscribe(address: member.address)) }
    }

    private func resubscribe(_ member: MemberView) {
        Task { await vm.dispatch(.resubscribe(address: member.address)) }
    }

    // MARK: - Helpers

    private func subscribedAtText(_ member: MemberView) -> String {
        if let ms = member.subscribedAtMs {
            return "subscribed \(ValueFormat.absoluteDate(epochMs: ms))"
        }
        return ""
    }
}

// MARK: - Add-member sheet (`mail-list-members-add-sheet-*`)

struct MailListMemberAddSheet: View {
    let vm: MailListMembersVM

    @Environment(\.dismiss) private var dismiss
    @State private var address = ""
    @State private var submitting = false
    @State private var error: String?

    var body: some View {
        Form {
            Section {
                TextField(L.mailLists.addMemberPlaceholder, text: $address)
                    #if !os(macOS)
                    .textInputAutocapitalization(.never)
                    #endif
                    .accessibilityIdentifier(Ids.mailListMembersAddSheetAddressInput)
                    .automationField(Ids.mailListMembersAddSheetAddressInput, text: $address)
            }
            if let error {
                Section { ErrorBanner(message: error) }
            }
            Section {
                Button(submitting ? "…" : L.mailLists.addMemberSubmit) {
                    Task { await submit() }
                }
                .disabled(submitting || address.trimmingCharacters(in: .whitespaces).isEmpty)
                .accessibilityIdentifier(Ids.mailListMembersAddSheetSubmitButton)
                .automationActivate(
                    Ids.mailListMembersAddSheetSubmitButton,
                    isEnabled: { !submitting && !address.trimmingCharacters(in: .whitespaces).isEmpty }
                ) { Task { await submit() } }
                .faunaGate("fauna.bridges.add_list_member")
                Button(L.mailLists.addMemberCancel, role: .cancel) { dismiss() }
                    .accessibilityIdentifier(Ids.mailListMembersAddSheetCancelButton)
                    .automationActivate(Ids.mailListMembersAddSheetCancelButton) { dismiss() }
            }
        }
        .formStyle(.grouped)
    }

    private func submit() async {
        submitting = true
        defer { submitting = false }
        await vm.dispatch(.addMember(address: address.trimmingCharacters(in: .whitespaces)))
        if let e = vm.errorMessage { error = e } else { dismiss() }
    }
}

// MARK: - Import sheet (`mail-list-members-import-sheet-*`)

struct MailListMemberImportSheet: View {
    let vm: MailListMembersVM

    @Environment(\.dismiss) private var dismiss
    @State private var addresses = ""
    @State private var submitting = false
    @State private var error: String?

    var body: some View {
        Form {
            Section(L.mailLists.importPlaceholder) {
                // One address per line (≤ 10000). A multi-line editor.
                TextEditor(text: $addresses)
                    .frame(minHeight: 140)
                    .font(.body.monospaced())
                    .accessibilityIdentifier(Ids.mailListMembersImportSheetInput)
                    .automationField(Ids.mailListMembersImportSheetInput, text: $addresses)
            }
            if let error {
                Section { ErrorBanner(message: error) }
            }
            Section {
                Button(submitting ? "…" : L.mailLists.importSubmit) {
                    Task { await submit() }
                }
                .disabled(submitting || addresses.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .accessibilityIdentifier(Ids.mailListMembersImportSheetSubmitButton)
                .automationActivate(
                    Ids.mailListMembersImportSheetSubmitButton,
                    isEnabled: { !submitting && !addresses.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
                ) { Task { await submit() } }
                .faunaGate("fauna.bridges.batch_import_list_members")
                Button(L.mailLists.importCancel, role: .cancel) { dismiss() }
                    .accessibilityIdentifier(Ids.mailListMembersImportSheetCancelButton)
                    .automationActivate(Ids.mailListMembersImportSheetCancelButton) { dismiss() }
            }
        }
        .formStyle(.grouped)
    }

    private func submit() async {
        submitting = true
        defer { submitting = false }
        await vm.dispatch(.batchImport(addresses: addresses))
        if let e = vm.errorMessage { error = e } else { dismiss() }
    }
}
