import SwiftUI

/// The shared **Settings → Sessions** sub-page (macOS + iOS, one FaunaKit view)
/// — `docs/goal/ui/sessions.md`; ui.yaml page `sessions`; rail slot directly
/// after Devices (`settings.md` § Navigation model). Where the account is signed
/// in right now, as one escalation ladder, top to bottom:
///
/// 1. **The list** — one `session-card` per live session, in the shared fold's
///    order (this app's own first); each card's kind, detail line, mark and
///    whether it can be revoked are the fold's, painted as given.
/// 2. **Sign out everywhere else** — the two-press inline confirm (no native
///    dialog, ruled 2026-09-25) and the always-visible revoke note.
/// 3. **Lock this account for 24 hours** — the REQUIRED warning (always visible,
///    never behind the confirm), the `LOCK` field and the button.
///
/// A paint shell over ``SessionsVM``. A lock that lands calls `onLocked`, which
/// each shell wires to leave for its launch surface (leg 2). Container = eager
/// `ScrollView { VStack }` so every row registers with the in-process driver
/// (`apple-e2e-automation.md` § Registration rules, rule 6). Twin of tui's and
/// linux's `settings/sessions.rs` and android's `SessionsScreen`.
public struct SessionsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    /// The session's one Devices VM — its machine names device-minted sessions.
    @Environment(DevicesMachineVM.self) private var devicesVM
    @State private var vm = SessionsVM()
    private let reloadToken: Int
    private let onLocked: () -> Void

    public init(reloadToken: Int = 0, onLocked: @escaping () -> Void) {
        self.reloadToken = reloadToken
        self.onLocked = onLocked
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                automationText(Ids.pageHeading, L.sessions.title)
                    .font(.title2)

                if let error = vm.snapshot?.error {
                    ErrorBanner(message: renderLocalizedText(error))
                }

                // 1. The list. `sessions-empty` is the fail-safe for a reply that
                // carries no rows — never painted before the first read lands.
                if let snapshot = vm.snapshot {
                    if snapshot.rows.isEmpty {
                        automationText(Ids.sessionsEmpty, L.sessions.empty)
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                    ForEach(Array(snapshot.rows.enumerated()), id: \.element.tokenId) { index, row in
                        card(row, index: index)
                    }
                }

                Divider().padding(.vertical, 4)
                revokeOthersSection

                Divider().padding(.vertical, 4)
                lockoutSection
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .pageTitle(L.sessions.title)
        .task(id: reloadToken) {
            guard let ctx = await context() else { return }
            await vm.hydrate(ctx)
        }
        .onDisappear { SessionsTestState.paintedTokenIds = nil }
    }

    // MARK: - The list

    private func card(_ row: FfiSessionRow, index: Int) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                automationText(Ids.sessionKind, renderLocalizedText(row.kind))
                    .font(.headline)
                Spacer()
                if let mark = row.mark {
                    automationText(Ids.sessionThisMarkBadge, renderLocalizedText(mark))
                        .font(.caption)
                        .foregroundStyle(Color.accentColor)
                }
            }
            automationText(Ids.sessionDetail, renderLocalizedText(row.detail))
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            // Absent on this app's own row — leaving this app is Sign out.
            if row.canRevoke {
                Button(L.sessions.revoke) { act { await vm.revoke(row.tokenId, $0) } }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(vm.busy)
                    .accessibilityIdentifier(Ids.sessionRevokeButton)
                    .automationActivate(Ids.sessionRevokeButton, isEnabled: { !vm.busy }) {
                        act { await vm.revoke(row.tokenId, $0) }
                    }
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color.secondary.opacity(0.08)))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.sessionCard)
        .automationValue(Ids.sessionCard, text: { row.tokenId })
        .automationScope(Ids.sessionCard, index: index)
    }

    // MARK: - Sign out everywhere else

    @ViewBuilder
    private var revokeOthersSection: some View {
        if vm.revokeOthersArmed {
            HStack(spacing: 8) {
                Button(L.sessions.revokeOthersConfirm, role: .destructive) {
                    act { await vm.confirmRevokeOthers($0) }
                }
                .buttonStyle(.borderedProminent)
                .tint(.red)
                .disabled(vm.busy)
                .accessibilityIdentifier(Ids.sessionsRevokeOthersConfirmButton)
                .automationActivate(Ids.sessionsRevokeOthersConfirmButton, isEnabled: { !vm.busy }) {
                    act { await vm.confirmRevokeOthers($0) }
                }

                Button(L.sessions.revokeOthersCancel) { vm.cancelRevokeOthers() }
                    .buttonStyle(.bordered)
                    .accessibilityIdentifier(Ids.sessionsRevokeOthersCancelButton)
                    .automationActivate(Ids.sessionsRevokeOthersCancelButton) { vm.cancelRevokeOthers() }
            }
        } else {
            Button(L.sessions.revokeOthers) { vm.armRevokeOthers() }
                .buttonStyle(.bordered)
                .disabled(vm.busy)
                .accessibilityIdentifier(Ids.sessionsRevokeOthersButton)
                .automationActivate(Ids.sessionsRevokeOthersButton, isEnabled: { !vm.busy }) {
                    vm.armRevokeOthers()
                }
        }
        automationText(Ids.sessionsRevokeNote, L.sessions.revokeNote)
            .font(.caption)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }

    // MARK: - The 24-hour lock

    @ViewBuilder
    private var lockoutSection: some View {
        automationText(Ids.sessionsLockoutWarning, L.sessions.lockoutWarning)
            .font(.callout)
            .fixedSize(horizontal: false, vertical: true)
        TextField(L.sessions.lockoutConfirmPlaceholder, text: $vm.lockWord)
            .textFieldStyle(.roundedBorder)
            .autocorrectionDisabled()
            #if os(iOS)
            .textInputAutocapitalization(.characters)
            #endif
            .accessibilityIdentifier(Ids.sessionsLockoutConfirmField)
            .automationField(Ids.sessionsLockoutConfirmField, text: $vm.lockWord)
        Button(L.sessions.lockoutButton, role: .destructive) { act { await vm.lock($0) } }
            .buttonStyle(.borderedProminent)
            .tint(.red)
            .disabled(!lockEnabled)
            .accessibilityIdentifier(Ids.sessionsLockoutButton)
            .automationActivate(Ids.sessionsLockoutButton, isEnabled: { lockEnabled }) {
                act { await vm.lock($0) }
            }
    }

    private var lockEnabled: Bool { !vm.busy && vm.lockWord == vm.lockConfirmWord }

    // MARK: - Plumbing

    /// Referenced by both each `Button` and its `automationActivate`, so the two
    /// never diverge (apple-e2e-automation.md § Resolved design point).
    private func act(_ body: @escaping (SessionsVM.Context) async -> Void) {
        Task {
            guard let ctx = await context() else { return }
            await body(ctx)
        }
    }

    private func context() async -> SessionsVM.Context? {
        guard let client else { return nil }
        // The session's Devices machine, built once (a no-op after the first).
        if devicesVM.machine == nil { await devicesVM.configure(api: client.api) }
        return SessionsVM.Context(
            api: client.api, devices: devicesVM.machine, ownDeviceId: client.deviceId,
            onLocked: onLocked)
    }
}
