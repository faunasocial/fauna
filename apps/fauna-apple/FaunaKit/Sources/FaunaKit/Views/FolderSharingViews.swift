import SwiftUI

/// Cross-user **folder Sharing** views (`docs/goal/ui/folders.md` § Sharing a
/// folder; the shape the linux LEAD established and the other 5
/// `*-folders-ui` clients mirror, priority #1). Shared by macOS + iOS (one FaunaKit
/// surface, priority #2).
///
/// **Owner side** — the "Shared with" member roster + share + remove + "Shared · N"
/// badge on an expanded `folder-row` (`FolderSharedWithSection` / `FolderMemberRow`
/// / `FolderShareSheet`).
///
/// **Recipient side (pending knocks)** — the page-level "Shared with you" area:
/// `FolderPendingShareRow` renders one `folder-pending-share` (a stranger's staged
/// share) with `folder-share-accept-button` (join the MLS group off the chat rail)
/// / `folder-share-decline-button` (ack-and-drop, never joins). A *contact's* share
/// auto-joins via the B2 gate, so it never knocks. Still absent (a documented
/// follow-on): `folder-leave-button` (no leave primitive yet) and the joined
/// shared-with-me list row + "Shared by ‹handle›" badge (blocked on the member-visible
/// `fauna.folders.list` projection, goal doc § line 139).
///
/// All state + orchestration lives in shared Rust reached via `DevicesMachineVM`
/// (the owner roster read `fauna.folders.members.list_actors` + the
/// `folders_share` / `folders_remove_member` author fns; the recipient
/// `folders_pending_shares` / `folders_accept_share` / `folders_decline_share`
/// fns — all over the one live conversations `MlsEngine`); these views are dumb renderers.

/// The "Shared with" section under an expanded `folder-row`: a header
/// (`folder-share-button` → the share sheet) and the member roster
/// (`folder-member-item` rows, or the "not shared yet" note).
struct FolderSharedWithSection: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary
    @State private var showShareSheet = false

    /// `role == "member"` actors (owner excluded) from the VM's eager-loaded roster.
    /// The filter is shared Rust (`folderMemberActors`) — never re-derive it locally.
    private var members: [FfiFolderActorMember] {
        folderMemberActors(actors: vm.folderActors[folder.name] ?? [])
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(L.devices.sharedWith)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                // `folder-share-button` — opens the reused recipient-picker share
                // sheet (priority #2 — no new picker ids).
                Button(L.devices.shareButton) { showShareSheet = true }
                    .font(.caption)
                    .accessibilityIdentifier(Ids.folderShareButton)
                    .automationActivate(Ids.folderShareButton) { showShareSheet = true }
            }

            if members.isEmpty {
                Text(L.devices.notSharedYet)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(Array(members.enumerated()), id: \.offset) { offset, member in
                    FolderMemberRow(vm: vm, folder: folder, member: member)
                        // Scope path for the indexed `folder-member-item` (mirrors
                        // the `media-item` ForEach) so scoped child reads resolve.
                        .automationScope(Ids.folderMemberItem, index: offset)
                }
            }
        }
        .padding(.leading, 8)
        .sheet(isPresented: $showShareSheet) {
            FolderShareSheet(vm: vm, folderName: folder.name) { showShareSheet = false }
        }
    }
}

/// One `folder-member-item` — a person the set is shared with:
/// `folder-member-handle` + `folder-member-status` ("Active") +
/// `folder-member-remove-button`. `.accessibilityElement(children: .contain)`
/// keeps the child ids queryable under the indexed container (the memory'd
/// Section-clobbers-children rule).
struct FolderMemberRow: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary
    let member: FfiFolderActorMember

    var body: some View {
        HStack {
            // Precomputed shared label: the nest-resolved handle, else the
            // canonical short id (12 chars + `…`) — never the raw 64-hex actor
            // id. Computed once in `FfiFolderActorMember` via the shared
            // `account_display_label` rule (value-formatting.md § Account display
            // label — the fourth consumer, after shared_by / owner display).
            automationText(Ids.folderMemberHandle, member.display)
                .font(.caption)
            // Client-derived status: every returned member reads "Active" — the nest
            // read reports only actors the share reached; "Pending" is a future
            // optimistic state (folders.md § Sharing; there is no nest status field).
            automationText(Ids.folderMemberStatus, L.common.active)
                .font(.caption2)
                .foregroundStyle(.secondary)
            Spacer()
            // `folder-member-remove-button` — rotates the content key for forward
            // secrecy. Needs BOTH the a11y id and `automationActivate` (a bare id is
            // invisible to the in-process driver — the memory'd rule).
            Button(L.common.remove) { Task { await removeMember() } }
                .font(.caption)
                .foregroundStyle(.red)
                .buttonStyle(.plain)
                .accessibilityIdentifier(Ids.folderMemberRemoveButton)
                .automationActivate(Ids.folderMemberRemoveButton) { Task { await removeMember() } }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.folderMemberItem)
        // Presence anchor for the indexed `folder-member-item` (one Entry per
        // member, so `count("folder-member-item")` is the member count).
        .automationValue(Ids.folderMemberItem, text: { member.display })
    }

    private func removeMember() async {
        guard let groupId = folder.mlsGroupId else { return }
        await vm.removeFolderMember(
            name: folder.name, memberActorIdHex: member.actorId, groupIdHex: groupId)
    }
}

/// Share sheet — reuses the recipient-picker **input** id (`recipient-picker-input`)
/// + `recipient-resolve-status` (priority #2 — no new picker ids) plus the
/// `folder-share-confirm` commit button (`optional_elements`, present only while
/// open). On confirm the typed handle is resolved + shared via the shared
/// `resolveRecipient` + `folders_share` (mirrors linux's dumb-text-entry +
/// resolve-on-confirm; the full stateful conversations picker is coupled to the
/// conversations manager, so co-opting it would drag folder concepts into that
/// manager — not done, priority #2).
struct FolderShareSheet: View {
    let vm: DevicesMachineVM
    let folderName: String
    let onDismiss: () -> Void

    @State private var input = ""
    /// Drives `recipient-resolve-status` — the state→(token, label) map is
    /// shared Rust (`recipientResolveStatus`), so this only ever needs to hold
    /// the raw `ResolveState`, never re-derive its text/token locally.
    @State private var resolveState: ResolveState = .idle

    private var resolveStatus: ResolveStatusView { recipientResolveStatus(state: resolveState) }

    private var resolveText: String {
        resolveStatus.label.map(renderLocalizedText) ?? ""
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L.devices.shareButton).font(.headline)

            TextField(L.conversations.unified.recipientPickerPlaceholder, text: $input)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.recipientPickerInput)
                .automationField(Ids.recipientPickerInput, text: $input)
                .onSubmit { Task { await confirm() } }

            Text(resolveText)
                .font(.caption)
                .foregroundStyle(resolveState == .error ? .red : .secondary)
                .frame(minHeight: 14, alignment: .leading)
                .accessibilityElement()
                .accessibilityIdentifier(Ids.recipientResolveStatus)
                .accessibilityValue(resolveStatus.token)
                .automationValue(Ids.recipientResolveStatus,
                                 text: { resolveText }, value: { resolveStatus.token })

            HStack {
                Spacer()
                Button(L.common.cancel) { onDismiss() }
                // `folder-share-confirm` — commit the picked recipient.
                Button(L.devices.shareButton) { Task { await confirm() } }
                    .buttonStyle(.borderedProminent)
                    .disabled(input.trimmingCharacters(in: .whitespaces).isEmpty)
                    .accessibilityIdentifier(Ids.folderShareConfirm)
                    .automationActivate(
                        Ids.folderShareConfirm,
                        isEnabled: { !input.trimmingCharacters(in: .whitespaces).isEmpty }
                    ) { Task { await confirm() } }
            }
        }
        .padding(20)
        .frame(minWidth: 360)
    }

    private func confirm() async {
        let handle = input.trimmingCharacters(in: .whitespaces)
        guard !handle.isEmpty else { return }
        resolveState = .resolving
        if await vm.shareFolder(name: folderName, recipientInput: handle) {
            onDismiss()
        } else {
            // The page `error-message` carries the detail; the picker just flags it.
            resolveState = .error
        }
    }
}

/// One `folder-pending-share` — a stranger's staged (knocked) cross-user share the
/// recipient hasn't decided on yet: the sharer identity ("Shared by ‹who›") +
/// `folder-share-accept-button` (join the MLS group off the chat rail) +
/// `folder-share-decline-button` (ack-and-drop, never joins). Rendered inside the
/// page-level "Shared with you" section (see `FoldersContent.pendingSharesSection`).
/// `.accessibilityElement(children: .contain)` keeps the child button ids queryable
/// under the indexed container (the memory'd Section-clobbers-children rule).
struct FolderPendingShareRow: View {
    let vm: DevicesMachineVM
    let share: FfiPendingShare

    /// The sharer — the shared precomputed `sharedByDisplay` (handle when the nest
    /// stamped one, else the canonical short id; `value-formatting.md` § Account
    /// display label). Empty only for a fully unstamped cross-nest share — the one
    /// locale-dependent branch left to the client (a cross-nest sharer stays
    /// handle-less **by design**: an asserted cross-nest identity would be
    /// spoofable, so the nest only resolves same-nest handles — `folders.md`
    /// § Sharing — Recipient gate).
    private var who: String {
        share.sharedByDisplay.isEmpty ? L.common.unknown : share.sharedByDisplay
    }
    private var byLabel: String { L.devices.sharedBy(who: who) }

    var body: some View {
        HStack {
            Text(byLabel)
                .font(.caption)
            Spacer()
            // Accept — join the MLS group + ack (bypasses the contact gate). Needs
            // BOTH the a11y id and `automationActivate` (a bare id is invisible to the
            // in-process driver — the memory'd rule).
            Button(L.common.accept) { Task { await vm.acceptPendingShare(inboxId: share.inboxId) } }
                .font(.caption)
                .buttonStyle(.borderless)
                .accessibilityIdentifier(Ids.folderShareAcceptButton)
                .automationActivate(Ids.folderShareAcceptButton) {
                    Task { await vm.acceptPendingShare(inboxId: share.inboxId) }
                }
            // Decline — ack-and-drop; never joins.
            Button(L.common.decline) { Task { await vm.declinePendingShare(inboxId: share.inboxId) } }
                .font(.caption)
                .foregroundStyle(.red)
                .buttonStyle(.borderless)
                .accessibilityIdentifier(Ids.folderShareDeclineButton)
                .automationActivate(Ids.folderShareDeclineButton) {
                    Task { await vm.declinePendingShare(inboxId: share.inboxId) }
                }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.folderPendingShare)
        // Presence anchor for the indexed `folder-pending-share` (one Entry per
        // knock, so `count("folder-pending-share")` is the pending count).
        .automationValue(Ids.folderPendingShare, text: { byLabel })
    }
}
