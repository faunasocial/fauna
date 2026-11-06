import Testing
@testable import FaunaKit

@Suite("LostNestOffer")
@MainActor
struct LostNestOfferTests {
    private func settle(
        custody: Bool = true,
        current: Bool = true,
        online: Bool = true,
        transientRetries: Int = .max,
        retries: inout Int
    ) async -> LostNestOffer.Outcome {
        var made = 0
        let outcome = await LostNestOffer.settle(
            custodiesBox: { custody },
            isCurrent: { current },
            stillTransient: {
                made += 1
                return made <= transientRetries
            },
            deviceOnline: { online },
            sleep: { _ in }
        )
        retries = made
        return outcome
    }

    @Test func a_device_with_custody_is_offered_recovery_after_every_retry_failed() async {
        var retries = 0
        #expect(await settle(retries: &retries) == .offer)
        #expect(retries == LostNestOffer.retryDelays.count)
    }

    @Test func the_schedule_is_webs_about_eight_seconds() {
        #expect(LostNestOffer.retryDelays.reduce(.zero, +) == .seconds(8))
    }

    @Test func a_device_custodying_nothing_stays_and_never_retries() async {
        var retries = 0
        #expect(await settle(custody: false, retries: &retries) == .stay)
        #expect(retries == 0)
    }

    @Test func a_device_with_no_network_path_keeps_its_cached_shell() async {
        var retries = 0
        #expect(await settle(online: false, retries: &retries) == .stay)
    }

    @Test func a_nest_that_was_only_down_routes_the_new_verdict() async {
        var retries = 0
        #expect(await settle(transientRetries: 1, retries: &retries) == .verdictChanged)
        #expect(retries == 2)
    }

    @Test func a_superseded_launch_does_nothing() async {
        var retries = 0
        #expect(await settle(current: false, retries: &retries) == .stay)
        #expect(retries == 0)
    }
}
