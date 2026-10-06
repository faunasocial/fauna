import SwiftUI

/// The onboarding **"Almost ready"** surface — post-provisioning, DNS-pending.
///
/// Shared by macOS and iOS (priority #2): rendered whenever
/// `wizard_outcome() == AwaitingManualDns`, which is true on **both** paths that
/// reach it — the same-session exit from `dns_post_instructions`, and the
/// relaunch hydration (`seed_awaiting_manual_dns`). It is deliberately **not** an
/// `OnboardingStep`, so the surface is uniform across both entries and the shared
/// `OnboardingStep` enum (matched by all seven apps) never grows a case.
///
/// Renders `awaiting_manual_dns_snapshot()` and polls `recheck_manual_dns()` on a
/// client-owned cadence (`awaitingDnsPollMs()`, shared Rust) until the freshly-provisioned nest comes online and the
/// admin claim completes; the machine then routes itself off `AwaitingManualDns`
/// (to the post-claim `NatModeChoice` step, or directly to `LoggedIn` on the
/// already-claimed recovery edge). Reference impls: linux
/// `views/onboarding/awaiting_manual_dns.rs`, web `routes/onboarding/+page.svelte`.
///
/// Authority: `docs/goal/behavior/onboarding.md` § "Almost ready" surface.
public struct AwaitingManualDnsView: View {
    @Bindable var vm: OnboardingVM

    /// Platform hand-off, invoked once the claim advances the machine off
    /// `AwaitingManualDns`. macOS resets its `.done` router state and iOS its
    /// onboarding gate, so the container re-renders the machine's new step
    /// (`NatModeChoice`) or routes to the authenticated app (`LoggedIn`). The
    /// slot is already cleared by the time this fires.
    let onClaimAdvanced: () -> Void

    public init(vm: OnboardingVM, onClaimAdvanced: @escaping () -> Void) {
        self.vm = vm
        self.onClaimAdvanced = onClaimAdvanced
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text(L.onboarding.awaitingDns.title)
                    .font(.title2)
                    .accessibilityIdentifier(Ids.pageHeading)

                // Status line — the snapshot's localized message carries all the
                // state wording (Pending/Checking/Claiming/Claimed/Error); the view
                // never re-derives it, so all seven apps say the same thing.
                automationText(Ids.awaitingDnsStatus, renderLocalizedText(vm.awaitingDnsSnapshot.message))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)

                // The records the user must add at their registrar. The label and the
                // copy button read the SAME shared formatter, so they can't disagree.
                ScrollView {
                    automationText(Ids.awaitingDnsRecords, vm.awaitingDnsRecordsText)
                        .font(.system(.body, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(minHeight: 160)

                HStack {
                    Button(L.onboarding.awaitingDns.copyButton) { copyRecords() }
                        .disabled(!vm.awaitingDnsCopyEnabled)
                        .accessibilityIdentifier(Ids.awaitingDnsCopyButton)
                        .automationActivate(Ids.awaitingDnsCopyButton,
                                            isEnabled: { vm.awaitingDnsCopyEnabled },
                                            value: { vm.awaitingDnsRecordsText }) { copyRecords() }

                    // Explicit "Check now" — the same single-shot probe the timer
                    // fires. Disabled while a probe/claim is in flight so the user
                    // can't stack them.
                    Button(L.onboarding.awaitingDns.recheckButton) {
                        Task { await performRecheck() }
                    }
                    .disabled(vm.isAwaitingDnsBusy)
                    .accessibilityIdentifier(Ids.awaitingDnsRecheckButton)
                    .automationActivate(Ids.awaitingDnsRecheckButton,
                                        isEnabled: { !vm.isAwaitingDnsBusy }) {
                        Task { await performRecheck() }
                    }
                }

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
            .padding()
        }
        .task {
            // The poll is bounded by the surface predicate, so it can't outlive the
            // surface (the task is cancelled when the view leaves the hierarchy).
            // `Done` is also the still-waiting state (recheck returns the current
            // step), so the exit check reads `isAwaitingManualDns`, never the step.
            // Cadence read live from shared Rust (`awaitingDnsPollMs()`), never a
            // literal — read by all 7 apps, never seven hand-copied numbers.
            while !Task.isCancelled && vm.isAwaitingManualDns {
                try? await Task.sleep(for: .milliseconds(awaitingDnsPollMs()))
                guard !Task.isCancelled, vm.isAwaitingManualDns else { break }
                await performRecheck()
            }
        }
    }

    /// One recheck, shared by the timer and the "Check now" button so they can't
    /// diverge. On the claim completing (the machine leaves `AwaitingManualDns`)
    /// hand back to the container. The slot is deliberately NOT cleared here —
    /// only at `LoggedIn` (`OnboardingVM.clearAwaitingDnsSlot`; `onboarding.md`
    /// § Long-term store contract, ratified 2026-09-21): a force-quit on the NAT
    /// page or on the trust offer must relaunch back into this surface, whose
    /// first poll takes the already-claimed resume and asks once more, instead
    /// of dropping into the app with the offer lost.
    private func performRecheck() async {
        await vm.recheckManualDns()
        if !vm.isAwaitingManualDns {
            onClaimAdvanced()
        }
    }

    private func copyRecords() {
        Pasteboard.copy(vm.awaitingDnsRecordsText)
    }
}
