import Foundation
import Testing
@testable import FaunaKit

#if !FAUNA_EXCISE_PAYMENTS

// Non-e2e unit tests for `SubscriptionsVM.providerReturnUrl(origin:)` — the §4
// provider form's post-checkout redirect display (monetization.md § Pillar 2 →
// *Visitor entitlement doors*, ruling (2)). The string is the shared
// `payments_return_url`; the VM only decides WHEN there is one.

@Suite("SubscriptionsVM return-URL display")
@MainActor
struct SubscriptionsVMReturnUrlTests {
    @Test("no web origin yields no return URL")
    func nilWithoutOrigin() {
        let vm = SubscriptionsVM()
        vm.formProviderKind = "stripe"
        #expect(vm.providerReturnUrl(origin: nil) == nil)
    }

    @Test("no selected kind yields no return URL")
    func nilWithoutKind() {
        let vm = SubscriptionsVM()
        #expect(vm.formProviderKind.isEmpty)
        #expect(vm.providerReturnUrl(origin: "https://alice.example.org") == nil)
    }

    @Test("an origin and a kind yield the shared string, per kind")
    func sharedStringPerKind() throws {
        let origin = "https://alice.example.org"
        let vm = SubscriptionsVM()
        for kind in ["stripe", "paddle", "lemonsqueezy"] {
            vm.formProviderKind = kind
            let url = try #require(vm.providerReturnUrl(origin: origin))
            #expect(url == paymentsReturnUrl(siteUrl: origin, kind: kind))
            #expect(url.hasPrefix(origin + "/"), "the display is over the site origin, never the nest URL")
            #expect(url.contains(kind))
        }
    }
}

#endif
