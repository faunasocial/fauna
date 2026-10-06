import Foundation
import Testing
@testable import FaunaKit

// The Swift-side wiring of the region content plane's render arm
// (region-blocking.md § The blocked render). The composition itself — which
// source drove the verdict, which verbs get a placeholder, which language of
// the reason shows — is shared Rust, pinned by `fauna_client_region` and
// `fauna-ffi`'s `region::tests`. These pin only what every apple surface
// decides from the decision: a block is always withheld, a collapse until the
// (family-shared) reveal, and no placeholder means the family arms.

private func placeholder(_ verb: String) -> FfiRegionPlaceholder {
    FfiRegionPlaceholder(
        verb: verb, region: "XZ", authorityName: "Synthetic Test Authority",
        reason: "Withheld under the Synthetic Act, section 7.")
}

@Suite struct RegionRenderDecisionTests {
    @Test func aRegionBlockIsWithheldEvenAfterAReveal() {
        let d = RegionRenderDecision(verdict: "block", placeholder: placeholder("block"))
        #expect(d.withheld(revealed: false)?.verb == "block")
        #expect(d.withheld(revealed: true)?.verb == "block")
        #expect(d.isRegionBlocked)
    }

    @Test func aRegionCollapseIsWithheldUntilRevealed() {
        let d = RegionRenderDecision(verdict: "collapse", placeholder: placeholder("collapse"))
        #expect(d.withheld(revealed: false)?.verb == "collapse")
        #expect(d.withheld(revealed: true) == nil)
        #expect(!d.isRegionBlocked)
    }

    // The leaf (region-blocking.md § Region determination): a store build waits
    // on its storefront — never falling back to the OS region — and a
    // self-built build declares the OS region at once. What the storefront's
    // alpha-3 code declares is shared Rust's (`from_storefront_alpha3`).
    @Test func aStoreBuildWaitsOnItsStorefrontAndASelfBuiltOneReadsTheOsRegion() {
        #expect(RegionStore.leaf(storeDistributed: true, osRegion: "NO") == .storefrontPending)
        #expect(RegionStore.leaf(storeDistributed: false, osRegion: "NO") == .systemRegion(code: "NO"))
        #expect(RegionStore.leaf(storeDistributed: false, osRegion: nil) == .systemRegion(code: nil))
    }

    // The test host is a self-built binary: no App Store receipt.
    @Test func thisSelfBuiltTestHostIsNotStoreDistributed() {
        #expect(!RegionStore.isStoreDistributed)
    }

    @Test func aFamilyVerdictPaintsNoRegionPlaceholder() {
        let d = RegionRenderDecision(verdict: "block", placeholder: nil)
        #expect(d.withheld(revealed: false) == nil)
        #expect(!d.isRegionBlocked)
    }
}
