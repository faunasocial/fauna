import SwiftUI

// EXCISED BY `FAUNA_EXCISE_P2P_SHARE` — the `p2p-share` member's ceremony renders.
// Their FFI face still sits in the store-safe flavor, so without this the excised
// build would compile fine and ship every `offline-share-*` / `offline-receive-*`
// id (the reason is written once, at the top of `SharePlaneModel.swift`).
#if !FAUNA_EXCISE_P2P_SHARE

/// The co-present offline-share panel (`p2p.md` § Offline share initiation) —
/// entry buttons (`offline-share-button` / `offline-receive-button`) when
/// closed, the open initiator/recipient panel (own code, peer-code input,
/// Begin/Expect, status, Cancel) when a panel is open. Shared by macOS + iOS
/// (one FaunaKit surface, priority #2). Reference:
/// `apps/fauna-linux/src/views/devices_folders/folders.rs`'s
/// `OfflineShareHandles` + `render_offline_share` (the same two-face shape,
/// GTK's mutable-widget repaint translated to SwiftUI's declarative
/// re-render) and `wire_offline_share_section`'s click wiring.
///
/// **The consent card mints NO new ids** — `GroupInvitationRow` below reuses
/// the `folder-pending-share` knock trio `FolderPendingShareRow` already
/// paints, appended into the SAME indexed list
/// (`FoldersContent.pendingSharesSection`) so a user sees ONE list of things
/// awaiting an answer. **A landed scope lists as an ordinary `folder-row`**
/// (`GroupScopeRow` below) — no rename, no leave button (severance is the
/// authority's mint, not a self-scoped drop — `account-data-plane.md` § The
/// recipient-set scheme).
struct OfflineShareSectionView: View {
    let vm: DevicesMachineVM

    var body: some View {
        if let view = vm.offlineShareView {
            let gates = offlineShareGates(view: view)
            VStack(alignment: .leading, spacing: 8) {
                Text(L.folders.offlineShareSection)
                    .font(.headline)
                if gates.showsEntryButtons {
                    entryButtons
                } else if gates.showsCodeWidgets {
                    openPanel(view, gates)
                }
            }
        }
    }

    @ViewBuilder
    private var entryButtons: some View {
        HStack(spacing: 8) {
            Button(L.folders.offlineShareStart) { vm.openOfflineSharePanel(.initiate) }
                .accessibilityIdentifier(Ids.offlineShareButton)
                .automationActivate(Ids.offlineShareButton) { vm.openOfflineSharePanel(.initiate) }
            Button(L.folders.offlineShareReceive) { vm.openOfflineSharePanel(.receive) }
                .accessibilityIdentifier(Ids.offlineReceiveButton)
                .automationActivate(Ids.offlineReceiveButton) { vm.openOfflineSharePanel(.receive) }
        }
    }

    /// The typed compare code's live parse, for the inline hint text only
    /// (`codeHint`) — the act gates come from `offlineShareGates(view:)`,
    /// never a re-derivation of this parse.
    private var peerCodeParsed: PeerCodeParsed? { vm.offlineSharePeerCodeParsed }

    private var peerCodeBinding: Binding<String> {
        Binding(get: { vm.offlineSharePeerCodeInput }, set: { vm.offlineSharePeerCodeInput = $0 })
    }

    private var codeHint: String? {
        guard let error = peerCodeParsed?.error else { return nil }
        return offlineShareCodeErrorLabel(error: error).map(renderLocalizedText)
    }

    @ViewBuilder
    private func openPanel(_ view: OfflineShareView, _ gates: OfflineShareGates) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(L.folders.offlineShareOwnCodeLabel)
                .font(.caption)
                .foregroundStyle(.secondary)
            automationText(Ids.offlineShareOwnCode, view.ownCode)
                .font(.caption.monospaced())
                .textSelection(.enabled)
            // The safety sentence: the one place a user learns that handing
            // the code over IN PERSON is the mechanism, and that the
            // addressing candidates at the end are part of it
            // (`p2p.md` § Offline share initiation → contract point 1).
            Text(L.folders.offlineShareOwnCodeHelp)
                .font(.caption2)
                .foregroundStyle(.secondary)

            TextField(L.folders.offlineSharePeerCodeLabel, text: peerCodeBinding)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.offlineSharePeerCodeInput)
                .automationField(Ids.offlineSharePeerCodeInput, text: peerCodeBinding)

            // Typing guidance for a malformed/own/empty compare code —
            // chrome beside the input, never `error-message` (e2e
            // convention 2 reserves that for what an action actually did).
            if let hint = codeHint {
                Text(hint)
                    .font(.caption2)
                    .foregroundStyle(.orange)
            }

            // Exactly one of Begin/Expect is ever shown at once, matching
            // which panel is open.
            if view.panel == .initiate {
                Button(L.folders.offlineShareBegin) { Task { await vm.beginOfflineShare() } }
                    .buttonStyle(.borderedProminent)
                    .disabled(!gates.canBegin)
                    .accessibilityIdentifier(Ids.offlineShareBeginButton)
                    .automationActivate(Ids.offlineShareBeginButton, isEnabled: { gates.canBegin }) {
                        Task { await vm.beginOfflineShare() }
                    }
            } else {
                Button(L.folders.offlineShareExpect) { vm.expectOfflineShare() }
                    .buttonStyle(.borderedProminent)
                    .disabled(!gates.canExpect)
                    .accessibilityIdentifier(Ids.offlineReceiveExpectButton)
                    .automationActivate(Ids.offlineReceiveExpectButton, isEnabled: { gates.canExpect }) {
                        vm.expectOfflineShare()
                    }
            }

            automationText(
                Ids.offlineShareStatus, renderLocalizedText(offlineShareStatusLabel(status: view.status)))
                .font(.caption)
                .foregroundStyle(.secondary)

            // Any open panel can be closed, and an in-flight ceremony can be
            // abandoned.
            Button(L.common.cancel) { vm.cancelOfflineSharePanel() }
                .buttonStyle(.plain)
                .accessibilityIdentifier(Ids.offlineShareCancelButton)
                .automationActivate(Ids.offlineShareCancelButton) { vm.cancelOfflineSharePanel() }
        }
    }
}

/// One `folder-pending-share` card for a co-present ceremony invitation — the
/// consent card's group arm, mints NO new ids (per the row's own note: "the
/// consent card mints no ids"). The set is nameless in v1, so the card names
/// the two things that ARE known: who is handing it over, and the short
/// scope id both people can see on their own screens. Mirrors
/// `FolderPendingShareRow`'s shape and linux's `build_group_invitation_row`.
/// `.accessibilityElement(children: .contain)` keeps the child button ids
/// queryable under the indexed container (the memory'd Section-clobbers-
/// children rule).
struct GroupInvitationRow: View {
    let vm: DevicesMachineVM
    let invitation: FfiPendingGroupShare

    private var byLabel: String {
        L.folders.offlineShareFrom(who: invitation.initiator, code: invitation.shortId)
    }

    var body: some View {
        HStack {
            Text(byLabel)
                .font(.caption)
            Spacer()
            // Accept — consent to the offered scope. Needs BOTH the a11y id
            // and `automationActivate` (a bare id is invisible to the
            // in-process driver — the memory'd rule). A dead control while no
            // seat is bound yet (the row's own 2026-08-20 finding) — the
            // gesture itself is a silent no-op then, mirroring the two entry
            // buttons' own silence in that state.
            Button(L.common.accept) {
                Task { await vm.consentToGroupShare(scopeId: invitation.scopeId) }
            }
            .font(.caption)
            .buttonStyle(.borderless)
            .accessibilityIdentifier(Ids.folderShareAcceptButton)
            .automationActivate(Ids.folderShareAcceptButton) {
                Task { await vm.consentToGroupShare(scopeId: invitation.scopeId) }
            }
            // Decline — ack-and-drop; never joins.
            Button(L.common.decline) {
                Task { await vm.declineGroupShare(scopeId: invitation.scopeId) }
            }
            .font(.caption)
            .foregroundStyle(.red)
            .buttonStyle(.borderless)
            .accessibilityIdentifier(Ids.folderShareDeclineButton)
            .automationActivate(Ids.folderShareDeclineButton) {
                Task { await vm.declineGroupShare(scopeId: invitation.scopeId) }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.folderPendingShare)
        // Presence anchor for the indexed `folder-pending-share` (continues
        // the SAME index the M2 shares occupy — one list of things awaiting
        // an answer).
        .automationValue(Ids.folderPendingShare, text: { byLabel })
    }
}

/// One `folder-row` for a shared set this device holds the machinery for (the
/// co-present ceremony's own listing — `p2p.md` § Offline share initiation).
/// Its body is the device-local folder binding and nothing else: no rename,
/// no leave — severance is the authority's mint, not a self-scoped roster drop
/// (`account-data-plane.md` § The recipient-set scheme). A folder bound here
/// runs an engine with no nest leg, its content moving over the group plane
/// (`group-set.md` § Build design — the group set). The row reads as the
/// set's recorded name, else its short scope id, and its
/// `folder-shared-badge` reads "Shared by ‹them›" on someone else's scope,
/// "Shared · N" on your own — the same two readings the M2 rows use. Mirrors
/// linux's `build_group_scope_row`.
///
/// `onToggle` is nil on a client with no binding to host (iOS): the row then
/// stays a label, with nothing to expand.
struct GroupScopeRow<BindingContent: View>: View {
    let scope: FfiGroupScope
    let isExpanded: Bool
    let onToggle: (() -> Void)?
    @ViewBuilder let binding: () -> BindingContent

    private var title: String { scope.pageLabel }

    private var badgeText: String {
        if let who = scope.sharedBy {
            return L.devices.sharedBy(who: who)
        }
        return L.devices.sharedBadge(count: String(scope.memberCount))
    }

    var body: some View {
        // Presence + name read for the indexed `folder-row`; the activate is
        // the click toggle, as on the owner row.
        if let onToggle {
            row(onToggle).automationActivate(Ids.folderRow, text: { title }, perform: onToggle)
        } else {
            row(nil).automationValue(Ids.folderRow, text: { title })
        }
    }

    private func row(_ onToggle: (() -> Void)?) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                if let onToggle {
                    Button(action: onToggle) {
                        HStack(spacing: 4) {
                            Image(systemName: isExpanded ? "chevron.down" : "chevron.right")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                            Text(title)
                                .font(.body)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                } else {
                    Text(title)
                        .font(.body)
                }
                Spacer()
                automationText(Ids.folderSharedBadge, badgeText)
                    .font(.caption2)
                    .foregroundStyle(.tint)
            }
            if isExpanded {
                binding()
            }
        }
        .padding(.vertical, 2)
        // `.contain` keeps the child ids (badge, the expander body's binding
        // controls) queryable alongside this container id.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.folderRow)
    }
}

#endif  // !FAUNA_EXCISE_P2P_SHARE

#if !FAUNA_EXCISE_P2P_SHARE
extension FfiGroupScope {
    /// A group set's label on the page: its recorded name, else its short id.
    var pageLabel: String {
        name ?? L.folders.offlineShareSet(code: shortId)
    }
}
#endif
