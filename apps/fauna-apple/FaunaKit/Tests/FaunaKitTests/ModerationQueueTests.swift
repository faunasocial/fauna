import Testing
import Foundation
@testable import FaunaKit

/// Unit coverage for the shared Moderation queue presentation — the apple analogue
/// of linux's `views/moderation.rs` tests. Pins the pure row formatting
/// (`ModerationQueueVM` helpers) and the shared-Rust label resolution
/// (`contentLabelStyle` / `obligationActionLabel` → `renderLocalizedText`) that the
/// `ContentLabelBadge` + queue rows render from, so no client hard-codes the
/// category/action vocabulary (moderation.md § Where logic lives, drift #157).

// NOTE: confidence per-mille → whole-percent rounding and content-id truncation
// are now thin passthroughs over shared Rust (`fauna_core::format::confidence_percent`
// half-up + `short_id` 12-chars-+…), whose unit tests own the contract
// (`libs/fauna-core/src/format.rs` `confidence_percent_rounds_half_up` +
// `short_id_truncates_at_twelve_chars`, supersets of the old Swift duplicates).
// No apple-side re-test of the shared rounding/truncation logic (priority #2/#4).

// MARK: - Category badge label resolution (the ContentLabelBadge source)

@Test func contentLabelBadgeResolvesCanonicalCategoryLabels() {
    #expect(renderLocalizedText(contentLabelStyle(category: "spam").label) == "Spam")
    #expect(renderLocalizedText(contentLabelStyle(category: "trusted").label) == "Trusted")
    #expect(renderLocalizedText(contentLabelStyle(category: "nsfw").label) == "NSFW")
    #expect(renderLocalizedText(contentLabelStyle(category: "phishing").label) == "Phishing")
    #expect(renderLocalizedText(contentLabelStyle(category: "commercial").label) == "Commercial")
}

@Test func contentLabelBadgeRendersOffListCategoryCapitalizedVerbatim() {
    // An off-list category falls back to the raw string capitalized (grey badge);
    // no i18n key matches, so renderLocalizedText echoes it (moderation.md § Categories).
    #expect(renderLocalizedText(contentLabelStyle(category: "csam").label) == "Csam")
}

@Test func contentLabelStyleCarriesHexColours() {
    // The badge paints `accent`/`tint` hex from the shared map (no per-app palette).
    #expect(contentLabelStyle(category: "spam").tint == "#EF4444")
    #expect(contentLabelStyle(category: "spam").accent == "#DC2626")
}

// MARK: - DM bubble content-label resolution (`primaryContentLabel`, the
// `DmMessageBubble` content-label-badge data source — moderation.md § Per-row
// badge data path). The winner-picking algorithm is shared Rust and owns its
// own tests (`libs/fauna-ffi/src/content_category.rs`
// `primary_content_label_picks_the_highest_confidence_entry` /
// `_of_empty_is_none`); these confirm the FFI binding round-trips correctly,
// mirroring android's `ConversationDetailContentTest.kt`
// `contentLabelBadgeRendersHighestConfidenceLabel` / `contentLabelBadgeAbsentWhenUnlabelled`.

@Test func contentLabelBadgeRendersHighestConfidenceLabel() {
    let labels = [
        ContentLabelEntry(category: "spam", confidencePerMille: 400),
        ContentLabelEntry(category: "nsfw", confidencePerMille: 900),
    ]
    #expect(primaryContentLabel(labels: labels)?.category == "nsfw")
}

@Test func contentLabelBadgeAbsentWhenUnlabelled() {
    #expect(primaryContentLabel(labels: []) == nil)
}

// MARK: - Enforcement-action label resolution (the queue row action column)

@Test func obligationActionLabelResolvesDiscriminants() {
    #expect(renderLocalizedText(obligationActionLabel(action: 0)) == "Rejected")
    #expect(renderLocalizedText(obligationActionLabel(action: 1)) == "Quarantined")
    #expect(renderLocalizedText(obligationActionLabel(action: 6)) == "Labeled")
    // 4 (Notify) and any unknown discriminant fall back to the neutral "Flagged".
    #expect(renderLocalizedText(obligationActionLabel(action: 4)) == "Flagged")
    #expect(renderLocalizedText(obligationActionLabel(action: 99)) == "Flagged")
}

// MARK: - Moderation-queue union (`ModerationQueueVM`'s local-detection reader)

// These pin the Swift-side wiring of the shared `moderationQueue(server:local:)`
// façade (moderation.md § Layout & flow) — the merge/dedupe ALGORITHM itself is
// shared Rust and owns its own tests (`libs/fauna-client-moderation`); no
// re-derivation here (priority #2/#4), just confirming the FFI binding round-trips
// a server row's action and a local row's blank action correctly.

@Test func moderationQueueLocalDetectionRendersBlankAction() {
    let local = LocalDetection(
        contentId: "abc123", contentType: "message", category: "spam",
        confidencePerMille: 900, timestamp: 1000)
    let rows = moderationQueue(server: [], local: [local])
    #expect(rows.count == 1)
    #expect(rows[0].source == .local)
    // A local-only detection carries NO enforcement action — never fabricate one
    // (moderation.md § Don't do these).
    #expect(rows[0].action == nil)
}

@Test func moderationQueueServerRowCarriesItsAction() {
    let action = FfiObligationAction(
        id: 1, contentType: "post", contentId: "def456", category: "spam",
        confidencePerMille: 800, action: 1, timestamp: 2000)
    let rows = moderationQueue(server: [action], local: [])
    #expect(rows.count == 1)
    #expect(rows[0].source == .server)
    #expect(rows[0].action == 1)
}
