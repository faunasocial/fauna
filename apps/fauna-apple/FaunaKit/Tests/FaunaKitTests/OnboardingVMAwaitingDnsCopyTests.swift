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

// The "Almost ready" exit (onboarding-provisioning.md § "Almost ready" surface →
// *Exit*): `abandonAwaitingManualDns()` clears the slot and re-lands at
// `handle_entry` holding the identity, so the surface (keyed on the outcome)
// disappears. The identity is seeded first, exactly as the relaunch-hydration glue
// does — an identity-less machine has nowhere to land but the start. Mirrors the
// e2e journey `test_awaiting_manual_dns.py` against the SAME shared machine.
@Test @MainActor func abandoningTheAlmostReadySurfaceLandsAtHandleEntry() {
    let vm = OnboardingVM()
    // A deterministic 32-byte test key, spelled by construction so no 64-hex literal
    // sits in a shipped file for the secret scan to read as a real key.
    vm.machine.seedIdentity(secret: String(repeating: "0", count: 63) + "1")
    seedAlmostReady(vm)
    #expect(vm.isAwaitingManualDns == true, "the seeder puts the machine on the Almost-ready outcome")
    #expect(vm.awaitingDnsFallthroughEnabled == true, "no claim is in flight, so the exit is live")

    vm.abandonAwaitingManualDns()

    #expect(vm.isAwaitingManualDns == false, "the exit retires the Almost-ready outcome")
    #expect(vm.step == .handleEntry, "same self, a different nest: the machine re-lands at handle_entry")
}
