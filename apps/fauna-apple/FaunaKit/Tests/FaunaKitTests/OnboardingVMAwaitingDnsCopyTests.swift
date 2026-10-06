import Foundation
import Testing
@testable import FaunaKit

// Non-e2e unit test for `OnboardingVM.awaitingDnsCopyEnabled` — the apple leg
// of onboarding.md § "Almost ready" surface, *Two modes*, second derived
// difference: "Copy all" is disabled (never hidden) when the resumed
// standard-path run has no DNS records to add. Mirrors the tui pin
// (`apps/fauna-tui/src/automation.rs::copy_all_is_disabled_in_the_records_less_mode_and_live_with_records`)
// against the SAME shared `OnboardingMachine` seeding functions, so both
// legs pin the identical rule rather than each re-deriving it.

@MainActor
private func seedAlmostReady(_ vm: OnboardingVM) {
    vm.machine.seedAwaitingManualDns(
        nestUrl: "https://nest.example",
        handle: "alice",
        dnsRecords: [
            DnsRecordPlain(recordType: "A", name: "@", value: "203.0.113.7", ttl: 300, priority: nil)
        ],
        claimCode: "claim-abc"
    )
}

@MainActor
private func seedAlmostReadyRecordsLess(_ vm: OnboardingVM) {
    vm.machine.seedAwaitingManualDns(
        nestUrl: "https://nest.example",
        handle: "alice",
        dnsRecords: [],
        claimCode: "claim-abc"
    )
}

@Test @MainActor func copyAllIsDisabledInTheRecordsLessModeAndLiveWithRecords() {
    let recordsLess = OnboardingVM()
    seedAlmostReadyRecordsLess(recordsLess)
    #expect(
        recordsLess.awaitingDnsCopyEnabled == false,
        "a resumed standard-path run has no records, so Copy all must be inert"
    )

    let withRecords = OnboardingVM()
    seedAlmostReady(withRecords)
    #expect(
        withRecords.awaitingDnsCopyEnabled == true,
        "with records to add at a registrar, Copy all is exactly the affordance the mode exists for"
    )
}
