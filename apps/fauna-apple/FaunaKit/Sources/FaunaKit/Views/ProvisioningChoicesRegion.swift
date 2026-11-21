import SwiftUI

/// `nest_provisioning`'s choices region on a run entered from the
/// choose-providers page (`docs/goal/behavior/onboarding-provisioning.md` § 3d
/// *The choices on the setting-things-up page*): the price confirm for a bought
/// domain — agreed here, at the purchase, gating *Buy and set up* — then the
/// server's location, mail mode, SSH door, update channel and type, every one
/// with a default. Paints nothing while `provisioningChoicesVisible()` is false,
/// so the old `dns_config` → `vps_config` path's page is unchanged.
///
/// Shared by macOS (`MacNestProvisioningView`) and iOS (`NestProvisioningView`);
/// placed directly under the price summary, as tui's
/// `wizard/nest_provisioning.rs` paints it. Free `@ViewBuilder` for the same
/// untracked-read reason as `vpsProviderSection`.
@ViewBuilder
public func provisioningChoicesRegion(vm: OnboardingVM) -> some View {
    if vm.machine.provisioningChoicesVisible() {
        if vm.machine.provisioningPriceConfirmVisible() {
            DnsPriceConfirmButton(onConfirm: { vm.machine.confirmPrice() })
        }
        vpsChoicesSection(vm: vm)
    }
}

public extension OnboardingVM {
    /// *Buy and set up*'s answer: dead on a run entered from the
    /// choose-providers page while the machine names a reason (the price not
    /// agreed, no server type); possible everywhere else, as before. Paint it
    /// with `faunaPossible` + `FaunaPossibilityReason` (rule 5: the reason
    /// beneath the button).
    var provisioningStartPossibility: Possibility {
        machine.provisioningStartPossibility()
    }
}
