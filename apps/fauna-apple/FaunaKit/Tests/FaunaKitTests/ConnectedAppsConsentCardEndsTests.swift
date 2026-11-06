import Foundation
import Testing
@testable import FaunaKit

// Pin the request card's lines as the view reads them off the shared composition
// (`connectedAppsRequestCardLines`; connected-apps.md § Where logic lives): the
// order and roles the view styles by, the key-replacement `ends` line last
// (connected-apps.md § User actions) and absent without one, the permission
// sets after the scopes, and the cli's reach line on the cli's card only.

private func card(
    clientId: String = "https://app.example/client.json",
    scopes: [String] = ["Read your posts"],
    sets: [ConsentSetRow] = [],
    ends: LocalizedText? = nil
) -> ConsentCardRow {
    ConsentCardRow(
        consentIdHex: "ab", code: "123456", clientId: clientId,
        clientName: "Example", scopeDescriptions: scopes, sets: sets, ends: ends)
}

@Test func theCardReadsHeadingWhoScopesInTheSharedOrder() {
    let lines = consentCardLines(card(scopes: ["Read your posts", "Write posts"]))
    #expect(lines.map(\.role) == [.heading, .who, .scopesHeading, .scope, .scope])
    #expect(lines[0].text == L.atprotoSettings.consentHeading)
    #expect(lines[1].text.contains("Example"))
    #expect(lines[3].text.contains("Read your posts"))
    #expect(lines[4].text.contains("Write posts"))
}

@Test func cardWithEndsShowsTheResolvedLineLast() {
    let ends = LocalizedText(key: "connected_apps.consent_ends_holder", args: [:])
    let lines = consentCardLines(card(ends: ends))
    #expect(!renderLocalizedText(ends).isEmpty)
    #expect(lines.last == ConsentCardLine(role: .ends, text: renderLocalizedText(ends)))
}

@Test func cardWithoutEndsShowsNothingNew() {
    let ends = LocalizedText(key: "connected_apps.consent_ends_writer", args: [:])
    let withEnds = consentCardLines(card(ends: ends))
    let without = consentCardLines(card(ends: nil))
    #expect(Array(withEnds.dropLast()) == without)
    #expect(!without.contains { $0.role == .ends })
}

@Test func permissionSetLinesFollowTheScopes() {
    let set = ConsentSetRow(
        nsid: "app.example.authFull", title: "Full access", details: nil,
        memberDescriptions: ["Read your posts"])
    let lines = consentCardLines(card(sets: [set]))
    let roles = lines.map(\.role)
    #expect(Array(roles.prefix(4)) == [.heading, .who, .scopesHeading, .scope])
    #expect(roles.dropFirst(4).allSatisfy { $0 == .set })
    #expect(roles.count > 4)
    #expect(lines[4].text.contains("app.example.authFull"))
}

@Test func onlyTheClisCardCarriesTheReachLine() {
    let cli = consentCardLines(card(clientId: "https://fauna.social/apps/cli/client-metadata.json"))
    let other = consentCardLines(card())
    #expect(cli.last?.role == .reach)
    #expect(cli.last?.text == L.lookup("connected_apps.consent_assistant_reach"))
    #expect(!other.contains { $0.role == .reach })
}
