import Foundation
import Testing

@testable import FaunaKit

// Read-only text is selectable on every page (`docs/goal/ui/README.md` § Text the
// person reads can be copied). The decision is made once per app root
// (`.faunaReadOnlyText()`, `Core/ReadOnlyText.swift`) and opted out of only where a
// click acts (`.faunaActsOnClick()`); the automation registry publishes it as the
// `selectable` attribute the e2e driver's `is_text_selectable` reads. These pin the
// fold, the two roots, and the shared read-only components keeping their selection.

private let appleRoot = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()  // FaunaKitTests
    .deletingLastPathComponent()  // Tests
    .deletingLastPathComponent()  // FaunaKit
    .deletingLastPathComponent()  // fauna-apple
    .standardizedFileURL

/// A source file's CODE lines — comments may name the modifiers to explain them.
private func codeLines(_ relativePath: String) throws -> String {
    let text = try String(contentsOf: appleRoot.appendingPathComponent(relativePath), encoding: .utf8)
    return text.split(separator: "\n", omittingEmptySubsequences: false)
        .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
        .joined(separator: "\n")
}

@Suite("Read-only text is selectable")
struct ReadOnlyTextSelectableTests {
    @Test("a read-only element under the root decision answers selectable")
    @MainActor func readOnlyTextAnswersSelectable() {
        let folded = AutomationRegistry.folding(
            AutomationRegistry.Entry(text: { "a message" }), textSelectable: true)
        #expect(folded.attributes?()["selectable"] == "true")
    }

    @Test("text under an acts-on-click subtree answers not selectable")
    @MainActor func actsOnClickAnswersNotSelectable() {
        let folded = AutomationRegistry.folding(
            AutomationRegistry.Entry(text: { "a row" }), textSelectable: false)
        #expect(folded.attributes?()["selectable"] == "false")
    }

    @Test("an actuable control's caption is exempt whatever its environment")
    @MainActor func controlCaptionIsExempt() {
        let folded = AutomationRegistry.folding(
            AutomationRegistry.Entry(activate: {}, text: { "Save" }), textSelectable: true)
        #expect(folded.attributes?()["selectable"] == "false")
    }

    @Test("the fold keeps the entry's other attributes and its own selectable")
    @MainActor func foldKeepsOtherAttributes() {
        let folded = AutomationRegistry.folding(
            AutomationRegistry.Entry(
                text: { "x" }, attributes: { ["reason": "r", "selectable": "false"] }),
            textSelectable: true)
        #expect(folded.attributes?()["reason"] == "r")
        #expect(folded.attributes?()["selectable"] == "false")
    }

    @Test("both app roots make the one decision")
    func bothRootsApplyTheDecision() throws {
        let mac = try codeLines("Fauna-macOS/App/FaunaMacApp.swift")
        // The main window and the onboarding window.
        #expect(mac.components(separatedBy: ".faunaReadOnlyText()").count - 1 >= 2)
        let ios = try codeLines("Fauna-iOS/App/FaunaApp.swift")
        #expect(ios.contains(".faunaReadOnlyText()"))
    }

    @Test("the shared read-only components keep their selection",
          arguments: [
              "FaunaKit/Sources/FaunaKit/Views/ErrorBanner.swift",
              "FaunaKit/Sources/FaunaKit/Views/LabeledFieldRow.swift",
              "FaunaKit/Sources/FaunaKit/Views/StatusLegSections.swift",
              "FaunaKit/Sources/FaunaKit/Core/Possibility.swift",
              "FaunaKit/Sources/FaunaKit/Core/OfflineGate.swift",
          ])
    func sharedComponentsKeepSelection(_ path: String) throws {
        let code = try codeLines(path)
        #expect(code.contains(".textSelection(.enabled)"), "\(path) lost its selection")
        #expect(!code.contains(".textSelection(.disabled)"), "\(path) turns selection off")
        #expect(!code.contains(".faunaActsOnClick()"), "\(path) opts out of selection")
    }
}
