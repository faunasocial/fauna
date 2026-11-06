import SwiftUI

/// Cross-user **folder Sharing** views (`docs/goal/ui/folders.md` § Sharing a
/// folder; the shape the linux LEAD established and the other 5
/// `*-folders-ui` clients mirror, priority #1). Shared by macOS + iOS (one FaunaKit
/// surface, priority #2).
///
/// **Owner side** — the "Shared with" member roster + share + remove + "Shared · N"
/// badge on an expanded `folder-row` (`FolderSharedWithSection` / `FolderMemberRow`
/// / `FolderShareSheet`), with the multi-writer Phase 1 access-grant surface the
/// other five apps carry: `folder-share-role-select` on the share sheet,
/// `folder-member-role-select` + `folder-member-cap-input` on every member row,
/// and the two writer warnings (`folder-writer-uncapped-warning`,
/// `folder-writer-published-warning`) in both placements.
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
/// (the owner roster read `fauna.folders.members.list_actors`, the grant write
/// `fauna.folders.members.set_access`, the access catalog `memberAccessOptions()`,
/// the reach rule `writerGrantReach`, + the
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
            FolderShareSheet(vm: vm, folder: folder) { showShareSheet = false }
        }
        // A `fauna://folder-share` route naming this set opens the flow its
        // button opens (`ShareRouteOpen`); the Share inside stays the user's act.
        .task(id: ShareRouteOpen.shared.generation) {
            if ShareRouteOpen.shared.takeFolderShare(id: folder.id) { showShareSheet = true }
        }
    }
}

/// One `folder-member-item` — a person the set is shared with:
/// `folder-member-handle` + `folder-member-status` ("Active") +
/// `folder-member-remove-button`, then the member's owner-editable-in-place
/// **access**: `folder-member-role-select` (Reader | Writer) +
/// `folder-member-cap-input` (blank = uncapped, commits on submit) +
/// the writer warnings. Both edits write the FULL `(access, cap)` pair —
/// `members.set_access` upserts the whole row (mirrors linux / android).
/// `.accessibilityElement(children: .contain)` keeps the child ids queryable
/// under the indexed container (the memory'd Section-clobbers-children rule).
struct FolderMemberRow: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary
    let member: FfiFolderActorMember

    /// The cap being typed; re-seeded whenever the nest's answer for this row
    /// changes (a roster re-read after any write repaints it).
    @State private var capText: String

    init(vm: DevicesMachineVM, folder: FolderSummary, member: FfiFolderActorMember) {
        self.vm = vm
        self.folder = folder
        self.member = member
        _capText = State(initialValue: Self.capText(member.byteCap))
    }

    /// The nest's grant — no grant row reads as Reader (the fail-safe default).
    /// The select paints this, never a local flip: the re-read is the answer.
    private var access: String { member.access ?? "reader" }

    private static func capText(_ cap: Int64?) -> String { cap.map(String.init) ?? "" }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
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
            HStack {
                MemberAccessPicker(id: Ids.folderMemberRoleSelect, current: access) { newAccess in
                    commit(access: newAccess)
                }
                TextField(L.devices.memberByteCapPlaceholder, text: $capText)
                    .textFieldStyle(.roundedBorder)
                    .font(.caption)
                    .frame(maxWidth: 140)
                    .accessibilityLabel(L.devices.memberByteCap)
                    .accessibilityIdentifier(Ids.folderMemberCapInput)
                    // Activate commits (the driver's `click` after typing), as
                    // Enter does for a person.
                    .automationField(Ids.folderMemberCapInput, text: $capText, commit: { commit(access: access) })
                    .onSubmit { commit(access: access) }
            }
            FolderWriterWarnings(
                folder: folder, access: access, uncapped: parseCountI64(input: capText) == nil)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.folderMemberItem)
        // Presence anchor for the indexed `folder-member-item` (one Entry per
        // member, so `count("folder-member-item")` is the member count).
        .automationValue(Ids.folderMemberItem, text: { member.display })
        .onChange(of: member.byteCap) { _, cap in capText = Self.capText(cap) }
    }

    /// A commit that would change nothing writes nothing. Load-bearing on macOS,
    /// not just thrift: an `NSTextField` fires its submit action whenever it
    /// ENDS editing — including when an alert (the declassify confirm) takes key
    /// focus — and a write + roster re-read landing mid-presentation re-renders
    /// the window under the alert, which crashed AppKit's dialog bridge.
    private func commit(access newAccess: String) {
        let cap = parseCountI64(input: capText)
        guard newAccess != access || cap != member.byteCap else { return }
        Task {
            await vm.setFolderMemberAccess(
                name: folder.name, memberActorIdHex: member.actorId, access: newAccess, byteCap: cap)
        }
    }

    private func removeMember() async {
        guard let groupId = folder.mlsGroupId else { return }
        await vm.removeFolderMember(
            name: folder.name, memberActorIdHex: member.actorId, groupIdHex: groupId)
    }
}

/// The member-access picker (`folder-share-role-select` / `folder-member-role-select`,
/// Reader | Writer). Values + labels come from the shared `memberAccessOptions()`
/// catalog (folders.md § Sharing), never hand-rolled; the driver picks the wire
/// value (`reader`/`writer`), the person reads the label. Same shared-catalog
/// picker shape as `FolderConflictPolicyPicker`.
struct MemberAccessPicker: View {
    let id: String
    let current: String
    let onPick: (String) -> Void

    private var options: [MemberAccessOption] { memberAccessOptions() }

    var body: some View {
        Picker(L.devices.memberAccess, selection: Binding(
            get: { current },
            set: { onPick($0) }
        )) {
            ForEach(options, id: \.value) { option in
                Text(renderLocalizedText(option.label)).tag(option.value)
            }
        }
        .controlSize(.small)
        .fixedSize()
        .accessibilityIdentifier(id)
        .automationSelect(
            id,
            value: { current },
            options: { options.map { renderLocalizedText($0.label) } },
            optionKeys: { options.map(\.value) },
            set: { onPick($0) }
        )
    }
}

/// The two writer warnings, stacked and independent (folders.md § Sharing):
/// `folder-writer-uncapped-warning` while Writer is granted with no byte cap
/// (advisory — the owner's quota is unbounded), and
/// `folder-writer-published-warning` while Writer is granted on a folder readable
/// beyond its members. The reach test is the shared `writerGrantReach`, fed the
/// NORMALIZED audience (an unparseable column never claims world-readable) —
/// never re-derived here.
struct FolderWriterWarnings: View {
    let folder: FolderSummary
    let access: String
    let uncapped: Bool

    private var published: String? {
        writerGrantReach(
            access: access,
            audience: normalizeAudience(value: folder.audience, bound: folder.mlsGroupId != nil),
            paywalled: folder.webPaywallTier != nil
        ).map(renderLocalizedText)
    }

    var body: some View {
        if access == "writer" && uncapped {
            automationText(Ids.folderWriterUncappedWarning, L.devices.writerUncappedWarning)
                .font(.caption2)
                .foregroundStyle(.red)
        }
        if let published {
            automationText(Ids.folderWriterPublishedWarning, published)
                .font(.caption2)
                .foregroundStyle(.red)
        }
    }
}

/// Share sheet — reuses the recipient-picker **input** id (`recipient-picker-input`)
/// + `recipient-resolve-status` (priority #2 — no new picker ids) plus the
/// `folder-share-confirm` commit button (`optional_elements`, present only while
/// open). On confirm the typed handle is resolved + shared via the shared
/// `resolveRecipient` + `folders_share` (mirrors linux's dumb-text-entry +
/// resolve-on-confirm; the full stateful conversations picker is coupled to the
/// conversations manager, so co-opting it would drag folder concepts into that
/// manager — not done, priority #2). Carries the share-time
/// `folder-share-role-select` grant (Reader default) and the writer warnings — a
/// share-time writer grant always carries no cap, so picking Writer always shows
/// the uncapped one (the owner caps it on the member row afterwards).
struct FolderShareSheet: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary
    let onDismiss: () -> Void

    @State private var input = ""
    @State private var access = "reader"
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

            MemberAccessPicker(id: Ids.folderShareRoleSelect, current: access) { access = $0 }
            FolderWriterWarnings(folder: folder, access: access, uncapped: true)

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
        if await vm.shareFolder(
            name: folder.name, recipientInput: handle, access: access == "writer" ? "writer" : nil
        ) {
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
