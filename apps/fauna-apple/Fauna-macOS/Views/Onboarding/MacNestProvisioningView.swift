import SwiftUI
import FaunaKit

/// Page 6 of handle-first onboarding: nest provisioning progress (macOS).
///
/// Snapshot-driven: reads `provisioningSnapshot()` on every observer tick and
/// renders the four-step pipeline (Domain, Server, Dns, Online) plus the
/// lifecycle controls (start / cancel / retry / continue / back). The
/// orchestrator owns every state transition; this view is purely presentation.
/// Mirrors `NestProvisioningView` (iOS), `apps/fauna-linux/.../nest_provisioning.rs`,
/// and the web `+page.svelte` `nest_provisioning` block. Implementation follows
/// `docs/goal/behavior/onboarding.md` §6.
struct MacNestProvisioningView: View {
    @Bindable var vm: OnboardingVM
    @State private var nowMs: UInt64 = Date.nowEpochMillis

    var body: some View {
        let snap = vm.machine.provisioningSnapshot()

        VStack(alignment: .leading, spacing: 16) {
            automationText(Ids.pageHeading, L.onboarding.nestProvisioning.title)
                .font(.title2)

            ProvisioningPriceBom(items: vm.machine.billOfMaterials())

            if snap.overall == .idle {
                Button(L.onboarding.nestProvisioning.startButton) {
                    Task { await vm.machine.runProvisioning() }
                }
                .keyboardShortcut(.defaultAction)
                .accessibilityIdentifier(Ids.provisioningStartButton)
                .automationActivate(Ids.provisioningStartButton) {
                    Task { await vm.machine.runProvisioning() }
                }
            }

            VStack(spacing: 8) {
                ForEach(Array(snap.steps.enumerated()), id: \.offset) { offset, step in
                    ProvisioningStepRow(snapshot: step)
                        .automationScope(Ids.provisioningStepRow, index: offset)
                }
            }
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(Ids.provisioningProgress)
            .automationValue(Ids.provisioningProgress, text: { "\(snap.steps.count) steps" })

            if let elapsed = provisioningElapsed(
                startedAtMs: snap.startedAtMs,
                finishedAtMs: snap.finishedAtMs,
                nowMs: nowMs
            ) {
                automationText(Ids.provisioningElapsed, renderLocalizedText(elapsed))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            if let msg = snap.finalError ?? vm.errorMessage {
                automationText(Ids.errorMessage, msg)
                    .foregroundColor(.red)
            }

            HStack {
                Button(L.common.back) { vm.provisioningBack() }
                .accessibilityIdentifier(Ids.provisioningBackButton)
                .automationActivate(Ids.provisioningBackButton) { vm.provisioningBack() }

                Spacer()

                if snap.overall == .running {
                    Button(L.common.cancel) {
                        vm.machine.cancelProvisioning()
                    }
                    .accessibilityIdentifier(Ids.provisioningCancelButton)
                    .automationActivate(Ids.provisioningCancelButton) {
                        vm.machine.cancelProvisioning()
                    }
                }

                // Retry resumes a stopped run from either terminal state —
                // Failed or Cancelled (idempotency skips done steps). Without
                // the Cancelled case a soft-cancel strands the user with only
                // Back. Per docs/goal/behavior/onboarding.md §6.
                //
                // Drives `runProvisioning()` to completion rather than the
                // fire-and-forget `retryProvisioning()`/`startProvisioning()`:
                // their Rust bodies do a bare `tokio::spawn`, which requires an
                // ambient tokio runtime on the calling thread — SwiftUI's main
                // thread has none, same class of bug other apps' GTK / Compose
                // UI threads hit for this exact orchestrator. `run_provisioning_inner`
                // resets the snapshot + cancel flag at entry, so it doubles as the
                // retry entry point — no separate retry call needed.
                if snap.overall == .failed || snap.overall == .cancelled {
                    Button(L.common.retry) {
                        Task { await vm.machine.runProvisioning() }
                    }
                    .accessibilityIdentifier(Ids.provisioningRetryButton)
                    .automationActivate(Ids.provisioningRetryButton) {
                        Task { await vm.machine.runProvisioning() }
                    }
                }

                Button(L.common.continue) {
                    _ = vm.machine.continueFromProvisioning()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(snap.overall != .succeeded)
                .accessibilityIdentifier(Ids.provisioningContinueButton)
                .automationActivate(
                    Ids.provisioningContinueButton,
                    isEnabled: { snap.overall == .succeeded }
                ) {
                    _ = vm.machine.continueFromProvisioning()
                }
                // Copy comprehensibility rule 5 — why Continue is disabled
                // (`ui/README.md` § Copy comprehensibility).
                if snap.overall != .succeeded, let reason = vm.machine.provisioningContinueBlockedReason() {
                    Text(renderLocalizedText(reason))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding()
        .frame(minWidth: 480)
        .onReceive(everySecondTicker()) { _ in
            if snap.overall == .running { nowMs = Date.nowEpochMillis }
        }
    }
}
