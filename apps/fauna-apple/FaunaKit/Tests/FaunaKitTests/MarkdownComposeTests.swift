import Testing
import Foundation
#if canImport(AppKit)
import AppKit
#elseif canImport(UIKit)
import UIKit
#endif
@testable import FaunaKit

// The apple compose markdown surface routes through shared Rust:
//  - the toolbar wrap rule (`fauna_core::markdown::wrap_selection` via `wrapMarkdownSelection`),
//    now selection-aware on the body field (`MarkdownCompose.wrap`);
//  - the inline-styling decoration map (`fauna_core::markdown::decoration_map` via
//    `decorationMap`), applied by `MarkdownDecorator`.
// These tests pin the FFI binding + the apple-side splice/attribute glue.

// MARK: - Toolbar wrap (empty-selection insertion + selection-aware wrap)

@Test func toolbarInsertionWrapsPlaceholderViaSharedRule() {
    #expect(MarkdownCompose.insertion(prefix: "**", suffix: "**") == "**text**")
    #expect(MarkdownCompose.insertion(prefix: "*", suffix: "*") == "*text*")
    #expect(MarkdownCompose.insertion(prefix: "`", suffix: "`") == "`text`")
    #expect(MarkdownCompose.insertion(prefix: "[", suffix: "](url)") == "[text](url)")
}

@Test func sharedWrapKeepsTrailingSpaceOutsideMarkers() {
    // The bug the shared rule fixes: a word-selection's trailing space stays OUTSIDE the
    // markers, so it no longer collides an adjacent emphasis span into `*italic ***bold**`.
    let w = wrapMarkdownSelection(selected: "italic ", prefix: "*", suffix: "*", placeholder: "text")
    #expect(w.replacement == "*italic* ")
    #expect(w.beforeCore == "*")
    #expect(w.core == "italic")
}

@Test func wrapSplicesOverSelectionAndReSelectsCore() {
    // Select "italic " (trailing space, like a double-click word selection) in
    // "say italic now" and italic-wrap: the trailing space stays outside the markers and
    // the re-selection covers the core "italic".
    let text = "say italic now"
    let sel = NSRange(location: 4, length: 7) // "italic " (incl. trailing space)
    let result = MarkdownCompose.wrap(text: text, selection: sel, prefix: "*", suffix: "*")
    #expect(result.text == "say *italic* now")
    #expect((result.text as NSString).substring(with: result.selection) == "italic")
}

@Test func wrapEmptySelectionInsertsPlaceholderAtCaret() {
    let text = "ab"
    let sel = NSRange(location: 1, length: 0) // caret between a and b
    let result = MarkdownCompose.wrap(text: text, selection: sel, prefix: "**", suffix: "**")
    #expect(result.text == "a**text**b")
    #expect((result.text as NSString).substring(with: result.selection) == "text")
}

@Test func wrapIsMultibyteSafe() {
    // An accented selection must re-select correctly (offsets measured in UTF-16, not bytes).
    let text = "café"
    let sel = NSRange(location: 0, length: 4) // "café" (4 UTF-16 units)
    let result = MarkdownCompose.wrap(text: text, selection: sel, prefix: "**", suffix: "**")
    #expect(result.text == "**café**")
    #expect((result.text as NSString).substring(with: result.selection) == "café")
}

// MARK: - Decoration applier (MarkdownDecorator over NSTextStorage)
//
// Marker visibility has two modes, flipped per editor by `markdown-marker-toggle-button`
// (`conversations.md` § Compose-field inline markdown styling): `markersShown: false` —
// the ratified DEFAULT — conceals inline emphasis markers with a caret-edge reveal
// (shared `composeDecorationPlan`), `true` shows them dimmed off the caret's line
// (shared `composeShowMarkersDimRanges`). Content styling is identical in both.

// MARK: Shown mode (the toggle pressed)

@Test func decoratorShownModeStylesContentAndDimsOffCaretLineMarkers() {
    // "a *b* c\nx": decoration_map ⇒ marker(2,3) italic(3,4) marker(4,5) on line 1. With
    // the caret on line 2 the line-1 markers dim; the content keeps the italic font and
    // plain runs stay base.
    let storage = NSTextStorage(string: "a *b* c\nx")
    let base = MarkdownDecorator.baseFont
    MarkdownDecorator.apply(to: storage, baseFont: base, caretLocation: 8, markersShown: true)

    var range = NSRange()
    let markerColor = storage.attribute(.foregroundColor, at: 2, effectiveRange: &range) as? PlatformColor
    #expect(markerColor == MarkdownDecorator.dimColor)
    let contentFont = storage.attribute(.font, at: 3, effectiveRange: &range) as? PlatformFont
    #expect(contentFont == MarkdownDecorator.styled(base, bold: false, italic: true))
    let plainFont = storage.attribute(.font, at: 0, effectiveRange: &range) as? PlatformFont
    #expect(plainFont == base)
    // Shown mode never conceals — that is the whole point of the toggle.
    #expect(storage.attribute(.faunaConcealedMarker, at: 2, effectiveRange: &range) == nil)
}

@Test func decoratorShownModeRevealsMarkersOnCaretLine() {
    // "*a*\n*b*": caret inside line 2 ⇒ line-1 markers dimmed, line-2 markers revealed.
    // The rule is the shared `composeShowMarkersDimRanges`, not a local caret-line test.
    let storage = NSTextStorage(string: "*a*\n*b*")
    let base = MarkdownDecorator.baseFont
    MarkdownDecorator.apply(to: storage, baseFont: base, caretLocation: 5, markersShown: true)

    var range = NSRange()
    let line1Marker = storage.attribute(.foregroundColor, at: 0, effectiveRange: &range) as? PlatformColor
    #expect(line1Marker == MarkdownDecorator.dimColor)
    let line2Marker = storage.attribute(.foregroundColor, at: 4, effectiveRange: &range) as? PlatformColor
    #expect(line2Marker == MarkdownDecorator.labelColor)
}

@Test func decoratorByteRangesAreMultibyteSafe() {
    // "é*x*": é is 2 UTF-8 bytes / 1 UTF-16 unit, so decoration_map's byte marker(2,3) +
    // italic(3,4) must convert to UTF-16 so "x" (index 2) is styled, not mid-é.
    let storage = NSTextStorage(string: "é*x*")
    let base = MarkdownDecorator.baseFont
    MarkdownDecorator.apply(to: storage, baseFont: base, caretLocation: 0, markersShown: true)

    var range = NSRange()
    let xFont = storage.attribute(.font, at: 2, effectiveRange: &range) as? PlatformFont
    #expect(xFont == MarkdownDecorator.styled(base, bold: false, italic: true))
    let eFont = storage.attribute(.font, at: 0, effectiveRange: &range) as? PlatformFont
    #expect(eFont == base)
}

// MARK: Hidden mode (the default)

@Test func decoratorHidesInlineMarkersByDefault() {
    // "**bold** trailing" with the caret parked in the trailing word: both `**` runs are
    // concealed, the content keeps its bold font, and the buffer still holds every
    // character (concealment is visual only — the send carries the true bytes).
    let source = "**bold** trailing"
    let storage = NSTextStorage(string: source)
    let base = MarkdownDecorator.baseFont
    MarkdownDecorator.apply(
        to: storage, baseFont: base,
        caretLocation: (source as NSString).length, markersShown: false
    )

    var range = NSRange()
    #expect(storage.attribute(.faunaConcealedMarker, at: 0, effectiveRange: &range) != nil)
    #expect(storage.attribute(.faunaConcealedMarker, at: 6, effectiveRange: &range) != nil)
    let contentFont = storage.attribute(.font, at: 2, effectiveRange: &range) as? PlatformFont
    #expect(contentFont == MarkdownDecorator.styled(base, bold: true, italic: false))
    #expect(storage.attribute(.faunaConcealedMarker, at: 2, effectiveRange: &range) == nil)
    #expect(storage.string == source)
}

@Test func decoratorRevealsTheRunUnderTheCaretInsteadOfConcealing() {
    // The caret-edge reveal: with the caret INSIDE the `**bold**` run the markers must be
    // editable, so they dim rather than conceal. This is what makes the trailing word in
    // the cross-app acceptance test load-bearing.
    let storage = NSTextStorage(string: "**bold** trailing")
    let base = MarkdownDecorator.baseFont
    MarkdownDecorator.apply(to: storage, baseFont: base, caretLocation: 3, markersShown: false)

    var range = NSRange()
    #expect(storage.attribute(.faunaConcealedMarker, at: 0, effectiveRange: &range) == nil)
    let markerColor = storage.attribute(.foregroundColor, at: 0, effectiveRange: &range) as? PlatformColor
    #expect(markerColor == MarkdownDecorator.dimColor)
}

// MARK: The published visible text (what `compose_visible_text` reads)

@Test func visibleTextSplicesOutConcealedMarkers() {
    let source = "**bold** trailing"
    let visible = MarkdownDecorator.visibleText(
        source: source, caretLocation: (source as NSString).length, markersShown: false
    )
    #expect(visible == "bold trailing")
}

@Test func visibleTextIsTheVerbatimSourceWhenMarkersShown() {
    let source = "**bold** trailing"
    let visible = MarkdownDecorator.visibleText(
        source: source, caretLocation: (source as NSString).length, markersShown: true
    )
    #expect(visible == source)
}

@Test func visibleTextKeepsTheRevealedRunUnderTheCaret() {
    // Caret inside the run ⇒ nothing concealed ⇒ the visible text is the full source.
    let visible = MarkdownDecorator.visibleText(
        source: "**bold** trailing", caretLocation: 3, markersShown: false
    )
    #expect(visible == "**bold** trailing")
}

@Test func visibleTextIsMultibyteSafe() {
    // The shared plan speaks bytes; a 4-byte emoji before the run must not shift the
    // splice. "😀 **b** x": the `**` pairs sit at UTF-16 [3,5) and [6,8).
    let source = "😀 **b** x"
    let visible = MarkdownDecorator.visibleText(
        source: source, caretLocation: (source as NSString).length, markersShown: false
    )
    #expect(visible == "😀 b x")
}

// MARK: Concealment is real — the glyphs occupy no width

/// Lay `source` out through the same TextKit-1 stack + glyph delegate the compose field
/// uses, and measure the whole line plus one marker run.
private func layoutProbe(
    source: String,
    markersShown: Bool,
    caret: Int,
    markerRange: NSRange
) -> (total: CGFloat, marker: CGRect) {
    let storage = NSTextStorage(string: source)
    let layout = NSLayoutManager()
    let container = NSTextContainer(size: CGSize(width: 100_000, height: 100_000))
    container.lineFragmentPadding = 0
    storage.addLayoutManager(layout)
    layout.addTextContainer(container)
    let glyphDelegate = MarkdownConcealingGlyphDelegate()
    layout.delegate = glyphDelegate

    return withExtendedLifetime(glyphDelegate) {
        MarkdownDecorator.apply(
            to: storage, baseFont: MarkdownDecorator.baseFont,
            caretLocation: caret, markersShown: markersShown
        )
        MarkdownDecorator.refreshGlyphs(layout, length: storage.length)
        layout.ensureLayout(for: container)
        let glyphs = layout.glyphRange(forCharacterRange: markerRange, actualCharacterRange: nil)
        let marker = glyphs.length == 0
            ? CGRect.zero
            : layout.boundingRect(forGlyphRange: glyphs, in: container)
        return (layout.usedRect(for: container).width, marker)
    }
}

@Test func concealedMarkersRenderZeroWidth() {
    // The mechanism, not just the attribute: a concealed `**` must produce NO advance —
    // otherwise the user sees a gap where the marker was, and "hidden" is a lie. This is
    // the assertion that fails loudly if the glyph delegate ever stops being installed
    // (e.g. a text view silently upgrading to TextKit 2, which has no such hook).
    let source = "**bold** trailing"
    let caretAway = (source as NSString).length
    let opener = NSRange(location: 0, length: 2)

    let hidden = layoutProbe(source: source, markersShown: false, caret: caretAway, markerRange: opener)
    let shown = layoutProbe(source: source, markersShown: true, caret: caretAway, markerRange: opener)

    #expect(hidden.marker.width == 0)
    #expect(shown.marker.width > 0)
    // And the line as a whole is narrower without the four concealed marker characters.
    #expect(hidden.total < shown.total)
}

// MARK: Offset conversion

@Test func byteToUTF16MapsCharBoundaries() {
    // "café x": é is 2 bytes / 1 UTF-16 unit, so byte 5 (just past é) is UTF-16 index 4.
    let map = MarkdownDecorator.byteToUTF16("café x")
    #expect(map[0] == 0)
    #expect(map[5] == 4)
    #expect(map[7] == 6) // end: "café x" = 7 bytes, 6 UTF-16 units
}

@Test func utf16ToByteInvertsByteToUTF16() {
    // The caret conversion the shared marker faces need. "café x" ⇒ UTF-16 4 is byte 5.
    #expect(MarkdownDecorator.utf16ToByte("café x", 0) == 0)
    #expect(MarkdownDecorator.utf16ToByte("café x", 4) == 5)
    #expect(MarkdownDecorator.utf16ToByte("café x", 6) == 7)
    // Past the end clamps to the end rather than running off it.
    #expect(MarkdownDecorator.utf16ToByte("café x", 99) == 7)
    // An offset landing between the halves of a surrogate pair rounds DOWN to that
    // character's start, so the caret never reads as inside the following run.
    #expect(MarkdownDecorator.utf16ToByte("😀a", 1) == 0)
    #expect(MarkdownDecorator.utf16ToByte("😀a", 2) == 4)
}
