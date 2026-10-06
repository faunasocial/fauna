import SwiftUI

/// nest_recovery — box-selection hub for total-box-loss recovery
/// (docs/goal/architecture/nest/box-recovery.md § Recovery UI (step 4), Task E).
///
/// Shared by macOS and iOS (priority #2, mirrors `AwaitingManualDnsView`): lists
/// the admin's custodied boxes, one selectable `recover-box-item` row per box.
/// `OnboardingVM.loadRecoveryBoxesIfNeeded` populates the machine's list on
/// appear — a reachable-nest read (the shared `deploymentSeeds()` getter) with the
/// device's own store as the fall-back (`RecoverableBoxes`).
/// Selecting a row enables the two re-provision method buttons (cloud /
/// self-hosted); an empty seed map shows `recover-box-empty-message`. The linux
/// GTK reference (`apps/fauna-linux/src/views/onboarding/nest_recovery.rs`) is
/// the richest existing implementation this mirrors.
public struct NestRecoveryView: View {
    @Bindable var vm: OnboardingVM

    public init(vm: OnboardingVM) {
        self.vm = vm
    }

    public var body: some View {
        let boxes = vm.machine.recoveryBoxes()
        let selected = vm.machine.recoverySelectedNestId()
        let hasSelection = selected != nil

        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text(L.onboarding.recovery.title)
                    .font(.title2)
                    .accessibilityIdentifier(Ids.pageHeading)

                Text(L.onboarding.recovery.subtitle)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                if boxes.isEmpty {
                    automationText(Ids.recoverBoxEmptyMessage, L.onboarding.recovery.emptyMessage)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                } else {
                    Text(L.onboarding.recovery.boxListLabel)
                        .foregroundStyle(.secondary)

                    VStack(spacing: 8) {
                        ForEach(Array(boxes.enumerated()), id: \.offset) { index, nestActorId in
                            boxRow(nestActorId: nestActorId, index: index, selected: selected == nestActorId)
                        }
                    }
                    .accessibilityIdentifier(Ids.recoverBoxList)
                    .accessibilityElement(children: .contain)
                }

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                VStack(spacing: 12) {
                    Button(L.onboarding.recovery.methodCloud) { vm.recoverViaCloud() }
                        .buttonStyle(.borderedProminent)
                        .frame(maxWidth: .infinity)
                        .disabled(!hasSelection)
                        .accessibilityIdentifier(Ids.recoverMethodCloudButton)
                        .automationActivate(
                            Ids.recoverMethodCloudButton,
                            isEnabled: { vm.machine.recoverySelectedNestId() != nil }
                        ) { vm.recoverViaCloud() }

                    Button(L.onboarding.recovery.methodSelfhosted) { vm.recoverViaSelfhosted() }
                        .buttonStyle(.bordered)
                        .frame(maxWidth: .infinity)
                        .disabled(!hasSelection)
                        .accessibilityIdentifier(Ids.recoverMethodSelfhostedButton)
                        .automationActivate(
                            Ids.recoverMethodSelfhostedButton,
                            isEnabled: { vm.machine.recoverySelectedNestId() != nil }
                        ) { vm.recoverViaSelfhosted() }
                }

                Button(L.common.back) { vm.recoveryBack() }
                    .accessibilityIdentifier(Ids.recoverBackButton)
                    .automationActivate(Ids.recoverBackButton) { vm.recoveryBack() }
            }
            .padding()
        }
        .task { await vm.loadRecoveryBoxesIfNeeded() }
    }

    @ViewBuilder
    private func boxRow(nestActorId: String, index: Int, selected: Bool) -> some View {
        Button {
            vm.selectRecoveryBox(nestActorId)
        } label: {
            HStack {
                Image(systemName: selected ? "largecircle.fill.circle" : "circle")
                    .foregroundStyle(selected ? Color.accentColor : Color.secondary)
                VStack(alignment: .leading, spacing: 2) {
                    Text(shortNestId(id: nestActorId))
                        .font(.system(.body, design: .monospaced))
                    Text(L.onboarding.recovery.boxItemHint)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
            }
            .contentShape(Rectangle())
            .padding(8)
        }
        .buttonStyle(.plain)
        .background(selected ? Color.accentColor.opacity(0.12) : Color.clear)
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .accessibilityIdentifier("recover-box-item-\(index)")
        .automationActivate("recover-box-item-\(index)") { vm.selectRecoveryBox(nestActorId) }
    }
}
