import Foundation
import Testing
@testable import FaunaKit

// Pin the request card's key-replacement line (`ConsentCardRow.ends`;
// connected-apps.md § User actions): a card whose snapshot row carries `ends`
// ends its text with the resolved line, after the scope lines and sets; a card
// without it gains nothing. tui's `consent_card_elements` is the reference shape.

private func card(ends: LocalizedText?) -> ConsentCardRow {
    ConsentCardRow(
        consentIdHex: "ab", code: "123456", clientId: "https://app.example/client.json",
        clientName: "Example", scopeDescriptions: ["Read your posts"], sets: [], ends: ends)
}

@Test func cardWithEndsShowsTheResolvedLineLast() {
    let ends = LocalizedText(key: "connected_apps.consent_ends_holder", args: [:])
    let text = consentCardText(card(ends: ends))
    #expect(!renderLocalizedText(ends).isEmpty)
    #expect(text.hasSuffix("\n" + renderLocalizedText(ends)))
    #expect(text.contains("Read your posts"))
}

@Test func cardWithoutEndsShowsNothingNew() {
    let withEnds = consentCardText(card(ends: LocalizedText(key: "connected_apps.consent_ends_writer", args: [:])))
    let without = consentCardText(card(ends: nil))
    #expect(withEnds.hasPrefix(without))
    #expect(!without.contains(renderLocalizedText(LocalizedText(key: "connected_apps.consent_ends_writer", args: [:]))))
}
