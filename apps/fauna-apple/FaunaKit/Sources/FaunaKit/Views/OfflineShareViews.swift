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
/// (`GroupScopeRow` below) — no local seat config, no name (v1 sets are
/// nameless), no leave button (severance is the authority's mint, not a
/// self-scoped drop — `account-data-plane.md` § The recipient-set scheme).
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

/// One thin, read-only `folder-row` for a shared set this device holds the
/// machinery for (the co-present ceremony's own listing —
/// `p2p.md` § Offline share initiation). Not expandable: no local seat
/// config, no rename, no leave (severance is the authority's mint, not a
/// self-scoped roster drop — `account-data-plane.md` § The recipient-set
/// scheme). The set's identity is its short scope id (nameless in v1), and
/// its `folder-shared-badge` reads "Shared by ‹them›" on someone else's
/// scope, "Shared · N" on your own — the same two readings the M2 rows use.
/// Mirrors linux's `build_group_scope_row`.
struct GroupScopeRow: View {
    let scope: FfiGroupScope

    private var title: String { L.folders.offlineShareSet(code: scope.shortId) }

    private var badgeText: String {
        if let who = scope.sharedBy {
            return L.devices.sharedBy(who: who)
        }
        return L.devices.sharedBadge(count: String(scope.memberCount))
    }

    var body: some View {
        HStack {
            Text(title)
                .font(.body)
            Spacer()
            automationText(Ids.folderSharedBadge, badgeText)
                .font(.caption2)
                .foregroundStyle(.tint)
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.folderRow)
        // Presence + name read for the indexed `folder-row`. Unlike the
        // owner row there is no activate — nothing to expand.
        .automationValue(Ids.folderRow, text: { title })
    }
}

#endif  // !FAUNA_EXCISE_P2P_SHARE
