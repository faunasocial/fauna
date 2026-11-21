import SwiftUI

/// The account switcher `launch_retry` carries when the device holds more than one
/// account — the way back from a failed append finish (`onboarding.md` § Wizard exit
/// handling → *The finished wizard holds until the session is signed in*; ui.yaml
/// `onboarding.launch_retry.optional_elements`). Shared by macOS + iOS; tui's
/// `launch::switcher_elements` is the lead.
///
/// - `account-switcher-list` heads it; one `account-switcher-item` per registry entry,
///   in registry (add) order.
/// - The registry's **active** account — the one whose launch just failed — is a plain
///   row; every other row is a switch.
/// - A switch runs through the same Stage-2 gate as the Settings switcher
///   (`AccountSwitcherVM.requestSwitch`: the native re-auth prompt for a flagged
///   target, so these apps paint no in-app prompt trio) and then the app's own
///   `onSwitch` seam — the same `switchAccount` the Settings switcher runs, which
///   sets the registry active and re-runs the launch as that account. The finished
///   wizard's deferred work never follows it: the switch teardown drops every
///   onboarding latch, so nothing of the failed account runs on the other one's
///   `Online`.
/// - A refused switch is painted on `error-message` and the surface stays.
///
/// Absent with fewer than two accounts. The rows are read **synchronously** at
/// construction, not from a `.task`, so they register with the automation registry
/// in document order — the e2e addresses a row by its flat index.
public struct LaunchRetryAccountSwitcher: View {
    @State private var vm: AccountSwitcherVM
    private let onSwitch: @MainActor (String, Bool) async throws -> Void

    /// `onSwitch` — `(actorId, confirmed)`, the app's account-switch seam
    /// (`appState.onSwitchAccount`); throws the registry's refusal before any teardown.
    public init(onSwitch: @escaping @MainActor (String, Bool) async throws -> Void) {
        // No session is served on `launch_retry`, so the active row is the registry's
        // active pointer (`AccountSwitcherVM.servedActorId`'s fallback).
        let vm = AccountSwitcherVM()
        vm.reload()
        _vm = State(initialValue: vm)
        self.onSwitch = onSwitch
    }

    public var body: some View {
        if vm.accounts.count >= 2 {
            VStack(alignment: .leading, spacing: 8) {
                Text(L.settings.accountPage.accounts)
                    .font(.headline)
                VStack(spacing: 0) {
                    ForEach(vm.accounts, id: \.actorId) { entry in
                        row(entry)
                    }
                }
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(Ids.accountSwitcherList)
                .automationValue(Ids.accountSwitcherList, text: { "\(vm.accounts.count)" })
                if let error = vm.error {
                    ErrorBanner(message: error)
                }
            }
            .frame(maxWidth: 360, alignment: .leading)
            .onAccountRegistryChanged { vm.reload() }
        }
    }

    @ViewBuilder
    private func row(_ entry: FfiAccountEntry) -> some View {
        let title = vm.label(for: entry)
        if vm.isActive(entry) {
            HStack {
                Text(title).lineLimit(1).truncationMode(.middle)
                Spacer()
                Image(systemName: "checkmark.circle.fill")
                    .foregroundStyle(.green)
                    .accessibilityLabel(L.common.active)
            }
            .padding(.vertical, 6)
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier(Ids.accountSwitcherItem)
            .automationActivate(Ids.accountSwitcherItem,
                                isEnabled: { false },
                                value: { title }) {}
        } else {
            Button {
                activate(entry)
            } label: {
                HStack {
                    Text(title).lineLimit(1).truncationMode(.middle)
                    Spacer()
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.borderless)
            .padding(.vertical, 6)
            .accessibilityIdentifier(Ids.accountSwitcherItem)
            .automationActivate(Ids.accountSwitcherItem, value: { title }) {
                activate(entry)
            }
        }
    }

    /// The Settings switcher's gesture: the Stage-2 gate, then the app's switch in its
    /// own `Task`; a refusal comes back to `vm.error` (`error-message`).
    private func activate(_ entry: FfiAccountEntry) {
        vm.requestSwitch(to: entry) { [vm] actorId, confirmed in
            Task { @MainActor in
                do {
                    try await onSwitch(actorId, confirmed)
                } catch {
                    vm.reportSwitchRefused(error)
                }
            }
        }
    }
}
