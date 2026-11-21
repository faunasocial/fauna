import Testing

#if DEBUG && os(macOS)
import AppKit
import SwiftUI

@testable import FaunaKit

// The macos selection read behind `POST /element/select-text` and
// `POST /page/selection` (`Testing/TextSelectionRead.swift`;
// `docs/goal/architecture/e2e-conventions.md` § Driver primitives → *Reading
// whether text can be selected*). Each test hosts real SwiftUI text in a real
// window and asserts the PLATFORM's answer — SwiftUI's own text view selecting
// under `selectAll:` and writing its selection through Services — never a value
// the code under test set.
//
// No `NSApp.nextEvent` pumping and no `setActivationPolicy` here: both killed the
// `swift test` process silently (measured 2026-11-19). A short
// `RunLoop.main.run(until:)` turn, as `ScreenCaptureTests` uses, is enough for
// SwiftUI to realize its views.

private let shown = "a value you can copy (selection read test)"

@MainActor
private func host<V: View>(_ content: V) -> (NSWindow, NSView) {
    _ = NSApplication.shared
    let window = NSWindow(
        contentRect: NSRect(x: 240, y: 240, width: 420, height: 160),
        styleMask: [.titled], backing: .buffered, defer: false)
    window.isReleasedWhenClosed = false
    let hosting = NSHostingView(rootView: content.frame(width: 400, height: 140))
    window.contentView = hosting
    window.orderFrontRegardless()
    hosting.layoutSubtreeIfNeeded()
    RunLoop.main.run(until: Date().addingTimeInterval(0.2))
    return (window, hosting)
}

@MainActor
private func wholeWindow(_ window: NSWindow) -> CGRect {
    window.contentView.map { $0.convert($0.bounds, to: nil) } ?? .zero
}

@Test @MainActor func selectAllOnASelectableTextSelectsAndReadsItsWholeText() throws {
    let (window, root) = host(Text(shown).textSelection(.enabled))
    defer { window.close() }
    let view = try #require(
        TextSelectionRead.textView(in: root, holdingFrame: wholeWindow(window)),
        "a selectable Text must be backed by SwiftUI's own text view")
    #expect(TextSelectionRead.selection(of: view) == nil,
            "nothing is selected before the gesture, so the Services write fails")
    #expect(TextSelectionRead.selectAll(in: view) == shown)
    #expect(window.firstResponder === view)
    // `/page/selection` reads the same selection back from the first responder.
    #expect(TextSelectionRead.selection(of: try #require(window.firstResponder)) == shown)
}

@Test @MainActor func textWithSelectionOffHasNoTextViewToSelect() {
    let (window, root) = host(Text(shown).textSelection(.disabled))
    defer { window.close() }
    #expect(TextSelectionRead.textView(in: root, holdingFrame: wholeWindow(window)) == nil,
            "text with selection off offers no text view, so the route answers no selection")
}

/// A sentinel the size of the hosted content, as the registry plants one in the
/// element's background.
@MainActor
private func sentinel(over root: NSView) -> NSView {
    let probe = NSView(frame: root.bounds)
    root.addSubview(probe)
    return probe
}

@Test @MainActor func theRouteSelectsThroughTheElementsSentinel() {
    let (window, root) = host(Text(shown).textSelection(.enabled))
    defer { window.close() }
    #expect(TextSelectionRead.selectText(
        selectableAttribute: "true", sentinel: sentinel(over: root)) == shown)
    #expect(TextSelectionRead.currentSelection() == shown,
            "`/page/selection` reads the selection the route left standing")
}

@Test @MainActor func theRouteRefusesAnElementPublishedUnselectableWithoutAGesture() {
    let (window, root) = host(Text(shown).textSelection(.enabled))
    defer { window.close() }
    let before = window.firstResponder
    #expect(TextSelectionRead.selectText(
        selectableAttribute: "false", sentinel: sentinel(over: root)) == "")
    #expect(window.firstResponder === before, "a refusal makes no gesture")
}

@Test @MainActor func theRouteAnswersNoSelectionForTextWithSelectionOff() {
    let (window, root) = host(Text(shown).textSelection(.disabled))
    defer { window.close() }
    #expect(TextSelectionRead.selectText(
        selectableAttribute: nil, sentinel: sentinel(over: root)) == "")
    #expect(TextSelectionRead.selectText(selectableAttribute: "true", sentinel: nil) == "")
}

/// Stands in for SwiftUI's private `SelectionTextField` (matched by name), the
/// backing the app's pages get: a selectable, non-editable `NSTextField`, whose
/// selection lives in the window's field editor once it is first responder.
private final class ProbeSelectionTextField: NSTextField {}

@Test @MainActor func aSelectableTextFieldSelectsThroughItsFieldEditor() throws {
    let (window, root) = host(Color.clear)
    defer { window.close() }
    let field = ProbeSelectionTextField(labelWithString: shown)
    field.isSelectable = true
    // 2 pt wider than the element on each side, as SwiftUI places it.
    field.frame = NSRect(x: 18, y: 40, width: 304, height: 20)
    root.addSubview(field)
    let element = CGRect(x: 20, y: 40, width: 300, height: 20)
    let view = try #require(
        TextSelectionRead.textView(in: root, holdingFrame: root.convert(element, to: nil)),
        "a text field overhanging the element by 2 pt is still its text")
    #expect(view === field)
    let (selected, why) = TextSelectionRead.explainedSelectAll(in: view)
    #expect(selected == shown, "\(why ?? "")")
    #expect(TextSelectionRead.currentSelection() == shown,
            "`/page/selection` reads the field editor's selection")
}

@Test @MainActor func aTextViewOutsideTheElementsFrameIsNotItsText() {
    let (window, root) = host(Text(shown).textSelection(.enabled))
    defer { window.close() }
    // A frame the text does not lie inside — another element's — finds nothing.
    let elsewhere = CGRect(x: 0, y: 0, width: 4, height: 4)
    #expect(TextSelectionRead.textView(in: root, holdingFrame: elsewhere) == nil)
}

// The ios route's decision over its Copy counts (`LongPressVerdict`, UIKit-free;
// the long-press itself is witnessed in e2e on the simulator alone).

private func verdict(_ selected: String, _ before: Int, _ after: Int, _ dismissed: Int)
    -> LongPressVerdict.Outcome {
    LongPressVerdict.decide(
        .init(selected: selected, copyBefore: before, copyAfter: after, copyAfterDismiss: dismissed),
        pressedAt: "the press", copyLabel: "Copy")
}

@Test func aRaisedCopyActionIsOfferedOnceTheMenuIsDown() {
    #expect(verdict("", 1, 2, 1) == .answer(selected: "", copyOffered: true, why: nil),
            "a page's own Copy label before the press is counted, never read as the menu")
}

@Test func anInPlaceSelectionAnswersWithoutACopyAction() {
    #expect(verdict("word", 0, 0, 0) == .answer(selected: "word", copyOffered: false, why: nil))
}

@Test func aPressThatRaisedNothingSaysWhy() {
    guard case let .answer(selected, offered, why) = verdict("", 0, 0, 0) else {
        Issue.record("no menu was raised, so none can stand"); return
    }
    #expect(selected == "" && !offered)
    #expect(why?.contains("raised no \"Copy\" action") == true)
}

@Test func aMenuStillStandingIsAnErrorNeverAReading() {
    guard case let .menuStands(what) = verdict("", 0, 1, 1) else {
        Issue.record("a standing menu must not answer a reading"); return
    }
    #expect(what.contains("still stands"))
}
#endif
