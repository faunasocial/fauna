import Testing
@testable import FaunaKit

// These pin that the apple spam UI sources its band buckets from the shared
// `fauna_protocol::spam` contract (via the `fauna-ffi` exports) and converts the 0.0–1.0 slider to the per-mille `u16` wire correctly
// — the same contract linux reads natively and android/web consume.

@Test func bandKeyMapsViaSharedRust() {
    // Mirrors `fauna_ffi::spam` tests: 0‰→aggressive, 500‰→moderate, 900‰→permissive.
    #expect(SpamOptions.bandKey(threshold: 0.0) == "aggressive")
    #expect(SpamOptions.bandKey(threshold: 0.5) == "moderate")
    #expect(SpamOptions.bandKey(threshold: 0.9) == "permissive")
}

@Test func perMilleRoundTrips() {
    #expect(SpamOptions.perMille(0.0) == 0)
    #expect(SpamOptions.perMille(0.5) == 500)
    #expect(SpamOptions.perMille(1.0) == 1000)
    #expect(SpamOptions.threshold(0) == 0.0)
    #expect(SpamOptions.threshold(500) == 0.5)
    #expect(SpamOptions.threshold(1000) == 1.0)
}

@Test func perMilleClampsOutOfRange() {
    #expect(SpamOptions.perMille(-0.3) == 0)
    #expect(SpamOptions.perMille(1.7) == 1000)
}
