import Foundation
import Testing
@testable import FaunaKit

// Pin `resolveComposedText`, the one Swift resolver for a shared-Rust
// `ComposedText` (mirrors `ComposedText::resolve`; android's `resolveComposed`
// and web's are the twins): each nested text resolves first and enters the
// template's args under its name, winning over a plain arg of the same name;
// then the outer template resolves; an unknown key surfaces rather than
// vanishing.

@Test func aNestedTextResolvesFirstAndFillsTheOuterTemplate() {
    let inner = LocalizedText(key: "connected_apps.consent_assistant_reach", args: [:])
    let outer = LocalizedText(key: "connected_apps.bullet", args: [:])
    let text = resolveComposedText(ComposedText(text: outer, nested: ["text": inner]))
    #expect(text == "  • " + L.lookup("connected_apps.consent_assistant_reach"))
}

@Test func aNestedTextWinsOverAPlainArgOfTheSameName() {
    let outer = LocalizedText(key: "connected_apps.indented", args: ["text": "plain"])
    let inner = LocalizedText(key: "connected_apps.consent_ends_holder", args: [:])
    let text = resolveComposedText(ComposedText(text: outer, nested: ["text": inner]))
    #expect(text == "  " + L.lookup("connected_apps.consent_ends_holder"))
    #expect(!text.contains("plain"))
}

@Test func plainArgsSubstituteWhenNothingIsNested() {
    let outer = LocalizedText(key: "connected_apps.bullet", args: ["text": "Read your posts"])
    #expect(resolveComposedText(ComposedText(text: outer, nested: [:])) == "  • Read your posts")
}

@Test func anUnknownKeySurfacesAsItself() {
    let outer = LocalizedText(key: "connected_apps.no_such_key_row_1751", args: [:])
    #expect(resolveComposedText(ComposedText(text: outer, nested: [:])) == "connected_apps.no_such_key_row_1751")
}
