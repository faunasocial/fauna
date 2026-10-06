import SwiftUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif

// Inline markdown **decoration applier** for the shared compose field
// (`docs/goal/ui/conversations.md` § Compose-field inline markdown styling). The
// buffer keeps the **literal markdown source** (wire format unchanged); this is the
// *decoration layer* — it applies styled `NSAttributedString` attributes over the
// source byte ranges from the shared `fauna_core::markdown::decoration_map` (via the
// ungated `decorationMap` UniFFI face), styling content (bold/italic/code/heading/
// blockquote/link). The same tokenizer feeds the render path, so the editor preview
// and the sent message never disagree.
//
// **Marker visibility has two modes**, flipped per editor by
// `markdown-marker-toggle-button` (`markersShown`; default `false`, no persistence):
//
//   - **hidden (default)** — `composeDecorationPlan` decides: inline emphasis markers
//     (`*`/`**`/`` ` ``/`[]()`) are CONCEALED (zero-width, undrawn) unless the caret is
//     inside their run (the caret-edge reveal, which dims them instead), and structural
//     prefixes (`# `, `> `, `- `) off the caret's line are dimmed.
//   - **shown** — `composeShowMarkersDimRanges` decides: nothing is concealed, every
//     marker off the caret's *line* is dimmed, markers on it revealed at full opacity.
//
// Both rules live in shared Rust, so apple applies exactly the policy web, linux,
// windows and android apply (priorities #2/#4) instead of re-deriving a caret-line
// test locally. Content styling is identical in both modes.
//
// Apple is the macOS+iOS leg of the cross-app fan-out (linux is the lead reference
// `apps/fauna-linux/src/views/conversations/compose_decoration.rs`; windows
// `DmComposeBar.Decoration.cs`; android over a Compose `VisualTransformation`). One
// `MarkdownDecorator` covers both apple platforms (the `NSTextStorage`/
// `NSAttributedString` API is identical on AppKit + UIKit; only the font/color
// **factories** differ, behind `#if`). The representable that hosts it
// (`MarkdownComposeField`) backs `dm-text-field` with an `NSTextView`/`UITextView` so
// the buffer exposes a `textStorage` to attribute **and** a `selectedRange` — which
// also makes the shared markdown-toolbar wrap selection-aware (see `MarkdownCompose`).

// Module-internal platform aliases (apple has no `PlatformFont`/`PlatformColor` yet;
// this is the first representable). Reuse these for any future AppKit/UIKit interop.
#if os(macOS)
typealias PlatformFont = NSFont
typealias PlatformColor = NSColor
#else
typealias PlatformFont = UIFont
typealias PlatformColor = UIColor
#endif

extension NSAttributedString.Key {
    /// Marks a run the compose field CONCEALS in markers-hidden mode. Carried on the
    /// text storage (never on the wire) and read back by
    /// `MarkdownConcealingGlyphDelegate` during glyph generation — the apple twin of
    /// linux's `md-hidden` GTK tag and windows' `Hidden` CharacterFormat run.
    static let faunaConcealedMarker = NSAttributedString.Key("faunaConcealedMarker")
}

// MARK: - Glyph-level concealment

/// Makes every `.faunaConcealedMarker` run **zero-width and undrawn** by answering the
/// TextKit-1 glyph-generation hook with `NSLayoutManager.GlyphProperty.null` for the
/// characters the decorator concealed. The apple twin of linux's `invisible(true)` GTK
/// tag and windows' `Hidden` CharacterFormat run: the characters stay in the text
/// storage — caret, selection, undo, the persisted draft and the sent bytes all still
/// see the literal markdown source — only their glyphs are suppressed.
///
/// Requires the TextKit **1** stack, which both representables below opt into
/// explicitly: TextKit 2 exposes no per-character glyph hook, so a text view that
/// silently upgraded would draw the markers at full width. `MarkdownDecorator.apply`
/// therefore also paints concealed runs `clear`, so such a view degrades to
/// "invisible but occupying space" rather than to "markers visible".
final class MarkdownConcealingGlyphDelegate: NSObject, NSLayoutManagerDelegate {
    func layoutManager(
        _ layoutManager: NSLayoutManager,
        shouldGenerateGlyphs glyphs: UnsafePointer<CGGlyph>,
        properties props: UnsafePointer<NSLayoutManager.GlyphProperty>,
        characterIndexes charIndexes: UnsafePointer<Int>,
        font aFont: PlatformFont,
        forGlyphRange glyphRange: NSRange
    ) -> Int {
        guard let storage = layoutManager.textStorage, storage.length > 0 else { return 0 }

        var patched = [NSLayoutManager.GlyphProperty]()
        patched.reserveCapacity(glyphRange.length)
        var concealedAny = false
        for i in 0..<glyphRange.length {
            var property = props[i]
            let charIndex = charIndexes[i]
            if charIndex >= 0, charIndex < storage.length,
               storage.attribute(.faunaConcealedMarker, at: charIndex, effectiveRange: nil) != nil {
                property.insert(.null)
                concealedAny = true
            }
            patched.append(property)
        }
        // Returning 0 means "I changed nothing, use your own properties" — the common
        // path (shown mode, or a body with no concealable markers).
        guard concealedAny else { return 0 }

        return patched.withUnsafeBufferPointer { buffer in
            guard let base = buffer.baseAddress else { return 0 }
            layoutManager.setGlyphs(
                glyphs,
                properties: base,
                characterIndexes: charIndexes,
                font: aFont,
                forGlyphRange: glyphRange
            )
            return glyphRange.length
        }
    }
}

// MARK: - Shared decoration applier

/// Applies the shared inline-markdown decoration map to an `NSTextStorage`. Shared by
/// the macOS (`NSTextView`) and iOS (`UITextView`) representables below — the storage
/// API is identical; only the font/color factories are `#if`-branched.
enum MarkdownDecorator {
    /// The compose field's base (unstyled) font. New input and plain runs use this; the
    /// styled kinds derive bold/italic/mono/heading variants from it.
    static var baseFont: PlatformFont {
        #if os(macOS)
        return NSFont.preferredFont(forTextStyle: .body)
        #else
        return UIFont.preferredFont(forTextStyle: .body)
        #endif
    }

    /// Re-style `storage` from scratch: content styling from the shared `decorationMap`,
    /// marker visibility from the shared marker faces (see `markerRanges`). Idempotent
    /// (resets to base then re-applies, mirroring linux `remove_all_tags` + re-apply).
    ///
    /// `caretLocation` is the caret's **UTF-16** offset (selection start); both reveal
    /// rules are caret-relative, so every call passes the field's live caret.
    /// `markersShown` is the per-editor `markdown-marker-toggle-button` state — `false`
    /// (the ratified default) conceals inline emphasis markers, `true` falls back to the
    /// all-dimmed live preview. Twin of linux `compose_decoration.rs::apply(buffer,
    /// markers_shown)` and windows `DmComposeBar.SetMarkersShown`.
    static func apply(
        to storage: NSTextStorage,
        baseFont: PlatformFont,
        caretLocation: Int,
        markersShown: Bool
    ) {
        let source = storage.string
        let ns = source as NSString
        let full = NSRange(location: 0, length: ns.length)

        storage.beginEditing()
        defer { storage.endEditing() }
        // Reset to base — the font, the default (label) color, and any concealment from
        // the previous pass, so a removed marker/style leaves no stale run behind.
        storage.setAttributes([.font: baseFont, .foregroundColor: labelColor], range: full)

        let decos = decorationMap(src: source)
        guard !decos.isEmpty else { return }

        let map = byteToUTF16(source)

        // Content styling — identical in BOTH modes. Marker kinds are deliberately not
        // handled here: their visibility is the shared plan's business (below), so apple
        // no longer re-derives "is this marker on the caret's line" locally.
        for d in decos {
            guard let lo = map[Int(d.start)], let hi = map[Int(d.end)], hi > lo, hi <= ns.length else { continue }
            let range = NSRange(location: lo, length: hi - lo)
            switch d.kind {
            case "bold":
                storage.addAttribute(.font, value: styled(baseFont, bold: true, italic: false), range: range)
            case "italic":
                storage.addAttribute(.font, value: styled(baseFont, bold: false, italic: true), range: range)
            case "bold_italic":
                storage.addAttribute(.font, value: styled(baseFont, bold: true, italic: true), range: range)
            case "code":
                storage.addAttribute(.font, value: mono(baseFont.pointSize), range: range)
            case "link", "image":
                storage.addAttribute(.foregroundColor, value: linkColor, range: range)
                storage.addAttribute(.underlineStyle, value: NSUnderlineStyle.single.rawValue, range: range)
            case "heading":
                storage.addAttribute(.font, value: heading(baseFont, level: d.level), range: range)
            case "blockquote":
                storage.addAttribute(.font, value: styled(baseFont, bold: false, italic: true), range: range)
                storage.addAttribute(.foregroundColor, value: dimColor, range: range)
                // The quote INDENT, over the whole quoted line: a paragraph attribute, so
                // it takes the paragraph range, not just the decorated span. The same
                // 12pt the rendered bubble indents a quote by (`conversations.md`
                // § Compose-field inline markdown styling — "quote indent").
                storage.addAttribute(.paragraphStyle, value: quoteParagraph, range: ns.paragraphRange(for: range))
            default:
                break // "marker" / "list_marker" (the plan owns them) and unknown kinds
            }
        }

        let treatment = markerRanges(source: source, caretLocation: caretLocation, markersShown: markersShown)
        for range in treatment.dim {
            storage.addAttribute(.foregroundColor, value: dimColor, range: range)
        }
        for range in treatment.conceal {
            storage.addAttribute(.faunaConcealedMarker, value: true, range: range)
            // The concealment proper is glyph-level (`MarkdownConcealingGlyphDelegate`
            // gives these characters `.null` glyphs, so they take no width and are never
            // drawn). The clear foreground is the belt-and-braces half: a text view that
            // lost the glyph delegate — or somehow ran TextKit 2 — still hides the marker
            // instead of showing it at full strength.
            storage.addAttribute(.foregroundColor, value: PlatformColor.clear, range: range)
        }
    }

    /// The marker treatment for `source` with the caret at `caretLocation` (UTF-16),
    /// resolved by the **shared** faces so all six GUI apps apply one policy
    /// (priorities #2/#4):
    ///
    ///   - `markersShown == false` → `composeDecorationPlan`: `hide` = inline emphasis
    ///     markers to conceal, `dim` = the caret-edge-revealed inline markers plus
    ///     structural prefixes off the caret's line.
    ///   - `markersShown == true` → `composeShowMarkersDimRanges`: nothing concealed,
    ///     every marker off the caret's *line* dimmed.
    ///
    /// Both faces speak **byte** offsets into the raw source; this converts the caret in
    /// and the spans back out to the `NSTextStorage`'s UTF-16 unit.
    static func markerRanges(
        source: String,
        caretLocation: Int,
        markersShown: Bool
    ) -> (conceal: [NSRange], dim: [NSRange]) {
        let ns = source as NSString
        guard ns.length > 0 else { return ([], []) }

        let map = byteToUTF16(source)
        let caretByte = utf16ToByte(source, min(max(caretLocation, 0), ns.length))

        func ranges(_ spans: [FfiRevealSpan]) -> [NSRange] {
            spans.compactMap { span in
                guard let lo = map[Int(span.start)], let hi = map[Int(span.end)],
                      hi > lo, hi <= ns.length else { return nil }
                return NSRange(location: lo, length: hi - lo)
            }
        }

        if markersShown {
            return ([], ranges(composeShowMarkersDimRanges(src: source, caret: UInt64(caretByte))))
        }
        let plan = composeDecorationPlan(src: source, caret: UInt64(caretByte))
        return (ranges(plan.hide), ranges(plan.dim))
    }

    /// The text the user SEES — `source` with every concealed marker run spliced out
    /// (concealment is zero-width, so a hidden marker is not visible). In shown mode
    /// nothing conceals and this returns the source verbatim.
    ///
    /// This is what the cross-app e2e `compose_visible_text` reads: apple publishes it
    /// on `dm-text-field`'s automation `visible` attribute, the apple twin of web's
    /// concealed-excluding `textContent`, linux's `include_hidden_chars=false` buffer read
    /// and windows' `AutomationProperties.HelpText`. The literal source — the draft,
    /// `/element/text`, the sent bytes — is never touched.
    static func visibleText(source: String, caretLocation: Int, markersShown: Bool) -> String {
        let conceal = markerRanges(
            source: source, caretLocation: caretLocation, markersShown: markersShown
        ).conceal
        guard !conceal.isEmpty else { return source }

        let ns = source as NSString
        var out = ""
        var cursor = 0
        for range in conceal.sorted(by: { $0.location < $1.location }) {
            let start = min(max(range.location, 0), ns.length)
            let end = min(range.location + range.length, ns.length)
            if start > cursor {
                out += ns.substring(with: NSRange(location: cursor, length: start - cursor))
            }
            if end > cursor { cursor = end }
        }
        if cursor < ns.length { out += ns.substring(from: cursor) }
        return out
    }

    /// Regenerate glyphs over the whole buffer after a decoration pass. An attribute edit
    /// invalidates *layout* but not *glyphs*, and concealment is a glyph property
    /// (`MarkdownConcealingGlyphDelegate` answers `shouldGenerateGlyphs` from the
    /// `.faunaConcealedMarker` attribute) — so without this a marker that just became
    /// concealed, or just stopped being, would keep its previously generated glyph and
    /// the toggle would appear not to work.
    static func refreshGlyphs(_ layoutManager: NSLayoutManager?, length: Int) {
        guard let lm = layoutManager, length > 0 else { return }
        let full = NSRange(location: 0, length: length)
        lm.invalidateGlyphs(forCharacterRange: full, changeInLength: 0, actualCharacterRange: nil)
        lm.invalidateLayout(forCharacterRange: full, actualCharacterRange: nil)
    }

    /// byte-offset → UTF-16-offset lookup over `s`. The shared markdown faces return
    /// **byte** offsets into the UTF-8 source (`conversations.md`: the shared API returns
    /// byte ranges, each app converts to its native offset unit), but `NSTextStorage`
    /// indexes in **UTF-16**. The map keys every char boundary's byte offset (0…len) to
    /// its UTF-16 offset; decoration offsets are always char boundaries, so they resolve.
    /// Multi-byte safe (emoji = 4 UTF-8 bytes / 2 UTF-16 units).
    static func byteToUTF16(_ s: String) -> [Int: Int] {
        var map = [0: 0]
        var byte = 0
        var u16 = 0
        for ch in s {
            byte += ch.utf8.count
            u16 += ch.utf16.count
            map[byte] = u16
        }
        return map
    }

    /// UTF-16-offset → byte-offset over `s` — the inverse of `byteToUTF16`, for handing a
    /// widget caret (UTF-16) to the shared marker faces (byte offsets). An offset landing
    /// *inside* a character (between the halves of a surrogate pair) rounds DOWN to that
    /// character's start, which is the conservative direction: the caret reads as "before
    /// this char", never as inside the next run.
    static func utf16ToByte(_ s: String, _ utf16Offset: Int) -> Int {
        var byte = 0
        var u16 = 0
        for ch in s {
            if u16 >= utf16Offset { return byte }
            let next = u16 + ch.utf16.count
            if next > utf16Offset { return byte }
            u16 = next
            byte += ch.utf8.count
        }
        return byte
    }

    /// The paragraph style a quoted line carries: a 12pt indent on every line of it.
    static let quoteIndent: CGFloat = 12
    static var quoteParagraph: NSParagraphStyle {
        let style = NSMutableParagraphStyle()
        style.firstLineHeadIndent = quoteIndent
        style.headIndent = quoteIndent
        return style
    }

    /// The styling `storage` holds right now, in linux's `text-runs` JSON shape (its
    /// `automation/agent.rs::text_runs`): one record per run of characters sharing one
    /// set of attributes — `text` (the source, concealed markers included) and `tags`, the
    /// looks that run carries beyond the base font. A run in the base look carries no tag,
    /// so a reader never mistakes a default for a style. Each look names only what it
    /// SETS (`weight` 700 for a bold face, `family` `monospace` for a fixed-pitch one,
    /// `scale` against the base size, `left_margin` for a paragraph indent, `invisible`
    /// for a concealed marker); the rest are null.
    ///
    /// Read off the live storage the text view draws, never recomputed from the shared
    /// decoration plan: the question it answers is whether the app APPLIED the styling.
    static func textRuns(_ storage: NSAttributedString, baseFont: PlatformFont) -> String {
        let ns = storage.string as NSString
        var runs: [[String: Any]] = []
        storage.enumerateAttributes(in: NSRange(location: 0, length: storage.length)) { attrs, range, _ in
            var look: [String: Any] = [:]
            if let font = attrs[.font] as? PlatformFont {
                if isBold(font) { look["weight"] = 700 }
                if isFixedPitch(font) { look["family"] = "monospace" }
                let scale = Double(font.pointSize / baseFont.pointSize)
                if abs(scale - 1) > 0.01 { look["scale"] = scale }
            }
            if let para = attrs[.paragraphStyle] as? NSParagraphStyle,
               max(para.headIndent, para.firstLineHeadIndent) > 0 {
                look["left_margin"] = Double(max(para.headIndent, para.firstLineHeadIndent))
            }
            if attrs[.faunaConcealedMarker] as? Bool == true { look["invisible"] = true }
            var tags: [[String: Any]] = []
            if !look.isEmpty {
                for key in ["weight", "family", "scale", "left_margin"] where look[key] == nil {
                    look[key] = NSNull()
                }
                if look["invisible"] == nil { look["invisible"] = false }
                look["name"] = NSNull()
                tags.append(look)
            }
            runs.append(["text": ns.substring(with: range), "tags": tags])
        }
        guard let data = try? JSONSerialization.data(withJSONObject: runs) else { return "[]" }
        return String(decoding: data, as: UTF8.self)
    }

    // MARK: Platform font/color factories

    #if os(macOS)
    static var labelColor: NSColor { .labelColor }
    static var dimColor: NSColor { .tertiaryLabelColor }
    static var linkColor: NSColor { .linkColor }

    static func styled(_ base: NSFont, bold: Bool, italic: Bool) -> NSFont {
        var traits: NSFontDescriptor.SymbolicTraits = []
        if bold { traits.insert(.bold) }
        if italic { traits.insert(.italic) }
        let descriptor = base.fontDescriptor.withSymbolicTraits(traits)
        return NSFont(descriptor: descriptor, size: base.pointSize) ?? base
    }

    static func mono(_ size: CGFloat) -> NSFont { .monospacedSystemFont(ofSize: size, weight: .regular) }

    static func heading(_ base: NSFont, level: UInt8) -> NSFont {
        NSFont.boldSystemFont(ofSize: base.pointSize * headingScale(level))
    }

    static func isBold(_ font: NSFont) -> Bool { font.fontDescriptor.symbolicTraits.contains(.bold) }
    static func isFixedPitch(_ font: NSFont) -> Bool { font.isFixedPitch }
    #else
    static var labelColor: UIColor { .label }
    static var dimColor: UIColor { .tertiaryLabel }
    static var linkColor: UIColor { .link }

    static func styled(_ base: UIFont, bold: Bool, italic: Bool) -> UIFont {
        var traits: UIFontDescriptor.SymbolicTraits = []
        if bold { traits.insert(.traitBold) }
        if italic { traits.insert(.traitItalic) }
        guard let descriptor = base.fontDescriptor.withSymbolicTraits(traits) else { return base }
        return UIFont(descriptor: descriptor, size: base.pointSize)
    }

    static func mono(_ size: CGFloat) -> UIFont { .monospacedSystemFont(ofSize: size, weight: .regular) }

    static func heading(_ base: UIFont, level: UInt8) -> UIFont {
        UIFont.boldSystemFont(ofSize: base.pointSize * headingScale(level))
    }

    static func isBold(_ font: UIFont) -> Bool { font.fontDescriptor.symbolicTraits.contains(.traitBold) }
    static func isFixedPitch(_ font: UIFont) -> Bool {
        font.fontDescriptor.symbolicTraits.contains(.traitMonoSpace)
    }
    #endif

    /// Heading point-size multiplier by level (1–4; 0 ⇒ treat as level 1). Mirrors the
    /// linux compose tag (`md-heading` scale 1.3) but tapers by level like the renderer.
    static func headingScale(_ level: UInt8) -> CGFloat {
        switch level {
        case 0, 1: return 1.5
        case 2: return 1.3
        case 3: return 1.15
        default: return 1.05
        }
    }
}

// MARK: - Live-view handle

/// A weak door onto the real text view behind a `MarkdownComposeField`, for the reads and
/// key presses only that view can answer: the styling its storage holds
/// (`textRuns`) and a caret key driven through its own caret-move action (`pressKey`).
/// The host that registers the field's automation entry owns one (`@State`) and hands it
/// to the field, whose representable fills `textView` when it makes the view — so the
/// registration and the live view meet without the registry reaching into SwiftUI.
@MainActor
public final class MarkdownFieldHandle {
    #if os(macOS)
    weak var textView: NSTextView?
    #else
    weak var textView: UITextView?
    #endif

    public init() {}

    /// The live storage's applied styling (`MarkdownDecorator.textRuns`); `nil` before the
    /// view exists.
    public func textRuns() -> String? {
        guard let storage = textView?.textStorage else { return nil }
        return MarkdownDecorator.textRuns(storage, baseFont: MarkdownDecorator.baseFont)
    }

    /// Drive one named caret key — `ArrowLeft`, `ArrowRight`, `Home`, `End` — through the
    /// text view's own caret movement, so its selection-change delegate (the caret-edge
    /// marker reveal) runs exactly as for a real key. `nil` on success, else the refusal
    /// sentence: another key, or no view yet, is refused, never acked (convention 11).
    public func pressKey(_ key: String) -> String? {
        guard let tv = textView else { return "press_key \(key.debugDescription): the compose field has no view yet" }
        #if os(macOS)
        // The responder actions AppKit's key bindings send for these keys.
        switch key {
        case "ArrowLeft": tv.moveLeft(nil)
        case "ArrowRight": tv.moveRight(nil)
        case "Home": tv.moveToBeginningOfLine(nil)
        case "End": tv.moveToEndOfLine(nil)
        default: return "press_key \(key.debugDescription) is not driven on apple"
        }
        #else
        // UIKit's hardware-key caret movement is a new `selectedTextRange` over the
        // `UITextInput` positions — the same assignment fires `textViewDidChangeSelection`.
        guard let current = tv.selectedTextRange else { return "press_key \(key.debugDescription): no caret" }
        let target: UITextPosition?
        switch key {
        case "ArrowLeft": target = tv.position(from: current.start, offset: -1) ?? tv.beginningOfDocument
        case "ArrowRight": target = tv.position(from: current.end, offset: 1) ?? tv.endOfDocument
        case "Home":
            target = tv.tokenizer.position(from: current.start, toBoundary: .line, inDirection: .storage(.backward))
                ?? tv.beginningOfDocument
        case "End":
            target = tv.tokenizer.position(from: current.end, toBoundary: .line, inDirection: .storage(.forward))
                ?? tv.endOfDocument
        default: return "press_key \(key.debugDescription) is not driven on apple"
        }
        guard let target else { return "press_key \(key.debugDescription): no caret position" }
        tv.selectedTextRange = tv.textRange(from: target, to: target)
        #endif
        return nil
    }
}

// MARK: - SwiftUI wrapper

/// `dm-text-field`: the shared compose body field. A plain-text `NSTextView`/`UITextView`
/// (via `MarkdownTextEditorRepresentable`) whose `textStorage` is live-decorated with
/// inline markdown styling, wrapped with a placeholder overlay + rounded border + a
/// 1…`maxLines` auto-grow height so it reads like the previous SwiftUI `TextField`.
/// `selection` is two-way so the markdown toolbar can wrap the *current* selection (see
/// `MarkdownCompose.wrap`). `markersShown` is the host's per-editor
/// `markdown-marker-toggle-button` state — `false` (the ratified default) conceals the
/// inline emphasis markers. The accessibility id lives on the underlying text view (set
/// in the representable) so XCUITest finds the focusable element.
public struct MarkdownComposeField: View {
    @Binding var text: String
    @Binding var selection: NSRange
    let placeholder: String
    let minLines: Int
    let maxLines: Int
    let markersShown: Bool
    let handle: MarkdownFieldHandle?

    @State private var measuredHeight: CGFloat = 0

    public init(
        text: Binding<String>,
        selection: Binding<NSRange>,
        placeholder: String,
        minLines: Int = 1,
        maxLines: Int = 6,
        markersShown: Bool = false,
        handle: MarkdownFieldHandle? = nil
    ) {
        self._text = text
        self._selection = selection
        self.placeholder = placeholder
        self.minLines = minLines
        self.maxLines = maxLines
        self.markersShown = markersShown
        self.handle = handle
    }

    private var lineHeight: CGFloat { MarkdownDecorator.baseFont.pointSize * 1.3 }
    // Vertical text-container inset (top+bottom), matched in the representable.
    private var verticalInset: CGFloat { 12 }
    private var minHeight: CGFloat { CGFloat(minLines) * lineHeight + verticalInset }
    private var maxHeight: CGFloat { CGFloat(maxLines) * lineHeight + verticalInset }

    public var body: some View {
        ZStack(alignment: .topLeading) {
            MarkdownTextEditorRepresentable(
                text: $text,
                selection: $selection,
                markersShown: markersShown,
                handle: handle,
                onHeightChange: { height in
                    let clamped = min(max(height, minHeight), maxHeight)
                    if abs(clamped - measuredHeight) > 0.5 {
                        // Defer past the current SwiftUI update pass (the representable
                        // reports height from `update*View`); a direct write there would
                        // mutate `@State` mid-update.
                        DispatchQueue.main.async { measuredHeight = clamped }
                    }
                }
            )
            .frame(height: min(max(measuredHeight, minHeight), maxHeight))

            if text.isEmpty {
                Text(placeholder)
                    .foregroundStyle(.secondary)
                    .padding(.leading, 9)
                    .padding(.top, 6)
                    .allowsHitTesting(false)
            }
        }
        .background(
            RoundedRectangle(cornerRadius: 6)
                .strokeBorder(Color.secondary.opacity(0.35), lineWidth: 1)
        )
    }
}

// MARK: - Platform representable

#if os(macOS)
struct MarkdownTextEditorRepresentable: NSViewRepresentable {
    @Binding var text: String
    @Binding var selection: NSRange
    var markersShown: Bool
    var handle: MarkdownFieldHandle?
    var onHeightChange: (CGFloat) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSTextView.scrollableTextView()
        scroll.drawsBackground = false
        scroll.borderType = .noBorder
        scroll.hasVerticalScroller = true
        guard let tv = scroll.documentView as? NSTextView else { return scroll }
        tv.delegate = context.coordinator
        tv.isRichText = false // plain text: the decorator owns all attributes
        tv.allowsUndo = true
        tv.drawsBackground = false
        tv.font = MarkdownDecorator.baseFont
        tv.textContainerInset = NSSize(width: 4, height: 6)
        tv.typingAttributes = [.font: MarkdownDecorator.baseFont, .foregroundColor: MarkdownDecorator.labelColor]
        // Markdown markers (`*`, `` ` ``, `>` …) must survive verbatim — no smart quotes/dashes.
        tv.isAutomaticQuoteSubstitutionEnabled = false
        tv.isAutomaticDashSubstitutionEnabled = false
        tv.isAutomaticTextReplacementEnabled = false
        // Marker concealment is a TextKit-**1** glyph property. Taking the legacy layout
        // manager here pins the view to TextKit 1 before its first layout (rather than
        // letting a later `layoutManager` access downgrade it mid-flight) and hands it the
        // delegate that suppresses concealed marker runs.
        tv.layoutManager?.delegate = context.coordinator.glyphDelegate
        tv.setAccessibilityIdentifier(Ids.dmTextField)
        handle?.textView = tv
        tv.string = text
        context.coordinator.decorate(tv)
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.parent = self
        guard let tv = scroll.documentView as? NSTextView else { return }
        context.coordinator.isProgrammatic = true
        defer { context.coordinator.isProgrammatic = false }
        if tv.string != text {
            tv.string = text
        }
        let clamped = clampRange(selection, length: (tv.string as NSString).length)
        if tv.selectedRange() != clamped {
            tv.setSelectedRange(clamped)
        }
        context.coordinator.decorate(tv)
        context.coordinator.reportHeight(tv)
    }

    final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: MarkdownTextEditorRepresentable
        var isProgrammatic = false
        /// Strongly held here because `NSLayoutManager.delegate` is weak.
        let glyphDelegate = MarkdownConcealingGlyphDelegate()
        init(_ parent: MarkdownTextEditorRepresentable) { self.parent = parent }

        func textDidChange(_ notification: Notification) {
            guard !isProgrammatic, let tv = notification.object as? NSTextView else { return }
            parent.text = tv.string
            parent.selection = tv.selectedRange()
            decorate(tv)
            reportHeight(tv)
        }

        func textViewDidChangeSelection(_ notification: Notification) {
            guard !isProgrammatic, let tv = notification.object as? NSTextView else { return }
            parent.selection = tv.selectedRange()
            decorate(tv) // re-run the caret-relative reveal for the new caret
        }

        func decorate(_ tv: NSTextView) {
            guard let storage = tv.textStorage else { return }
            MarkdownDecorator.apply(
                to: storage,
                baseFont: MarkdownDecorator.baseFont,
                caretLocation: tv.selectedRange().location,
                markersShown: parent.markersShown
            )
            MarkdownDecorator.refreshGlyphs(tv.layoutManager, length: (tv.string as NSString).length)
        }

        func reportHeight(_ tv: NSTextView) {
            guard let lm = tv.layoutManager, let tc = tv.textContainer else { return }
            lm.ensureLayout(for: tc)
            parent.onHeightChange(lm.usedRect(for: tc).height + tv.textContainerInset.height * 2)
        }
    }
}
#else
struct MarkdownTextEditorRepresentable: UIViewRepresentable {
    @Binding var text: String
    @Binding var selection: NSRange
    var markersShown: Bool
    var handle: MarkdownFieldHandle?
    var onHeightChange: (CGFloat) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeUIView(context: Context) -> UITextView {
        // TextKit **1**, explicitly: marker concealment is a glyph property, and only the
        // TextKit-1 layout manager exposes the per-character `shouldGenerateGlyphs` hook
        // the concealing delegate answers. TextKit 2 has no equivalent.
        let tv = UITextView(usingTextLayoutManager: false)
        tv.delegate = context.coordinator
        tv.backgroundColor = .clear
        tv.font = MarkdownDecorator.baseFont
        tv.textContainerInset = UIEdgeInsets(top: 6, left: 4, bottom: 6, right: 4)
        tv.typingAttributes = [.font: MarkdownDecorator.baseFont, .foregroundColor: MarkdownDecorator.labelColor]
        tv.isScrollEnabled = true
        // Markdown markers must survive verbatim — no smart quotes/dashes.
        tv.smartQuotesType = .no
        tv.smartDashesType = .no
        tv.layoutManager.delegate = context.coordinator.glyphDelegate
        tv.accessibilityIdentifier = "dm-text-field"
        handle?.textView = tv
        tv.text = text
        context.coordinator.decorate(tv)
        return tv
    }

    func updateUIView(_ tv: UITextView, context: Context) {
        context.coordinator.parent = self
        context.coordinator.isProgrammatic = true
        defer { context.coordinator.isProgrammatic = false }
        if tv.text != text {
            tv.text = text
        }
        let clamped = clampRange(selection, length: (tv.text as NSString).length)
        if tv.selectedRange != clamped {
            tv.selectedRange = clamped
        }
        context.coordinator.decorate(tv)
        context.coordinator.reportHeight(tv)
    }

    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: MarkdownTextEditorRepresentable
        var isProgrammatic = false
        /// Strongly held here because `NSLayoutManager.delegate` is weak.
        let glyphDelegate = MarkdownConcealingGlyphDelegate()
        init(_ parent: MarkdownTextEditorRepresentable) { self.parent = parent }

        func textViewDidChange(_ tv: UITextView) {
            guard !isProgrammatic else { return }
            parent.text = tv.text
            parent.selection = tv.selectedRange
            decorate(tv)
            reportHeight(tv)
        }

        func textViewDidChangeSelection(_ tv: UITextView) {
            guard !isProgrammatic else { return }
            parent.selection = tv.selectedRange
            decorate(tv) // re-run the caret-relative reveal for the new caret
        }

        func decorate(_ tv: UITextView) {
            MarkdownDecorator.apply(
                to: tv.textStorage,
                baseFont: MarkdownDecorator.baseFont,
                caretLocation: tv.selectedRange.location,
                markersShown: parent.markersShown
            )
            MarkdownDecorator.refreshGlyphs(tv.layoutManager, length: (tv.text as NSString).length)
        }

        func reportHeight(_ tv: UITextView) {
            let width = tv.bounds.width > 0 ? tv.bounds.width : 300
            let size = tv.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude))
            parent.onHeightChange(size.height)
        }
    }
}
#endif

/// Clamp an `NSRange` so it never exceeds the live text length (the toolbar's re-select
/// or an external edit can outrun the buffer between renders).
func clampRange(_ range: NSRange, length: Int) -> NSRange {
    let location = min(max(range.location, 0), length)
    let len = min(max(range.length, 0), length - location)
    return NSRange(location: location, length: len)
}
