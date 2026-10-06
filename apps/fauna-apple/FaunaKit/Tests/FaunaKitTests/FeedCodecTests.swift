import Testing
import Foundation
@testable import FaunaKit

// Pure, nest-free test for the shared feed-rule codec wired through UniFFI
// (`normalizeFilterRules`). The create-feed form builds `[FilterRuleInput]`
// triples that the shared codec encodes into the typed `rules` wire list; this
// guards the Swift seam — that the generated bindings exist and round-trip
// field-for-field (CSV spacing, the label 0–10 scale, CreatedAfter hours all
// invert in Rust, zero Swift JSON/float). The wider round-trip wire conformance
// lives nest-side (`conformance_feed.rs`) and the codec logic in `fauna-client-feed`.
//
// (The old `Ffi* → FaunaKit-struct` feed mappings these tests also covered were
// removed with `FeedFFIMapping.swift` when the Feed page moved to the shared
// `FfiFeedManager` snapshot — the 2026-06-16 lift.)

@Test func filterRuleEncodeDecodeRoundTripsThroughSharedCodec() throws {
    let triples = [
        FilterRuleInput(ruleType: "BodyContains", value: "rust,svelte", required: false),
        FilterRuleInput(ruleType: "HasMedia", value: "", required: true),
        FilterRuleInput(ruleType: "LabelBelow", value: "spam:5", required: false),
        FilterRuleInput(ruleType: "CreatedAfter", value: "24", required: false),
    ]
    let decoded = try normalizeFilterRules(rules: triples.map {
        FfiFilterRule(ruleType: $0.ruleType, value: $0.value, required: $0.required)
    })
    #expect(decoded.count == 4)
    #expect(decoded[0].ruleType == "BodyContains")
    #expect(decoded[0].value == "rust, svelte")  // re-joined with ", "
    #expect(decoded[1].ruleType == "HasMedia")
    #expect(decoded[1].required == true)
    #expect(decoded[2].value == "spam:5")        // 500‰ → "5"
    #expect(decoded[3].value == "24")            // 86_400_000_000µs → "24"
}
