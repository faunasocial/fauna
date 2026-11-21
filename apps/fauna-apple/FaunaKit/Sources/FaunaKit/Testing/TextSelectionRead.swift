// The macos and ios legs (the ios one at the end of this file) of the e2e selection reads — `POST /element/select-text` and
// `POST /page/selection` on `InProcessAutomationServer` (`docs/goal/architecture/
// e2e-conventions.md` § Driver primitives → *Reading whether text can be
// selected*; the rule witnessed is `docs/goal/ui/README.md` § Text the person
// reads can be copied).
//
// **Two backings.** SwiftUI backs a selectable `Text` on macOS with one of two
// private views, matched by class name: an `AppKitTextInteractionView` (a bare
// `Text` in a hosting view — what a hosted unit test gets), or a
// `SelectionTextField` — a selectable `NSTextField` inside a `SelectionOverlay`
// platform view, 2 pt wider than the text on each side — which the app's pages
// get (measured in the e2e app 2026-11-19: no `AppKitTextInteractionView` in
// its window at all). Both are found inside the element's frame with a few
// points of slack.
//
// **Select All, never a pointer drag.** Those views select through gesture
// recognizers which
// answer only while the app is the active app, and an e2e app cannot count on
// being that (activation is one machine-wide state parallel runs would take from
// each other, and the system refuses it while another app holds the front). So
// this does what a person's click followed by Edit ▸ Select All does: the
// element's own text view becomes the window's first responder and receives the
// platform's `selectAll:` action, sent to whatever the window made first
// responder (the view itself, or a text field's field editor). No selection
// range is ever written and no flag is set — a text with selection off has no
// such view, and answers no selection.
//
// **The read is the responder's own Services write** (`writeSelection(to:types:)`)
// into a pasteboard made for the one read and released after it — never the
// general pasteboard, which other tests read.
#if DEBUG && os(macOS)
import AppKit

@MainActor
enum TextSelectionRead {
    /// The classes SwiftUI backs a selectable `Text` with on macOS (see the
    /// file header), matched by name: they are private, so no type check is
    /// possible.
    static let textViewClassNames = ["AppKitTextInteractionView", "SelectionTextField"]

    /// How far a text view may overhang the element's frame and still be its
    /// text: `SelectionTextField` sits 2 pt outside the text on each side.
    static let frameSlack: CGFloat = 4

    static func isSelectableTextView(_ view: NSView) -> Bool {
        let name = String(describing: type(of: view))
        return textViewClassNames.contains { name.contains($0) }
    }

    /// The selectable text view the element's frame holds: the first view, in
    /// subview order under the window's content view, whose class name is one
    /// of `textViewClassNames` and whose window-space frame lies inside `frame`
    /// give or take `frameSlack` (the registration sentinel sits in the
    /// element's background, so its frame is the element's). `nil` when the
    /// element shows no selectable text.
    static func textView(in root: NSView, holdingFrame frame: CGRect) -> NSView? {
        let slack = frame.insetBy(dx: -frameSlack, dy: -frameSlack)
        var stack: [NSView] = [root]
        while !stack.isEmpty {
            let view = stack.removeFirst()
            if isSelectableTextView(view), !view.isHiddenOrHasHiddenAncestor {
                let rect = view.convert(view.bounds, to: nil)
                if rect.width > 0, rect.height > 0, slack.contains(rect) { return view }
            }
            stack.append(contentsOf: view.subviews)
        }
        return nil
    }

    /// The selectable text view the sentinel's element holds, `nil` when there
    /// is none (or the sentinel is detached).
    static func textView(forSentinel sentinel: NSView) -> NSView? {
        guard let window = sentinel.window, let root = window.contentView else { return nil }
        return textView(in: root, holdingFrame: sentinel.convert(sentinel.bounds, to: nil))
    }

    /// The route's decision, whole: an element whose `selectable` attribute is
    /// `"false"` (an actuable control's caption, a row that acts on a click) or
    /// whose frame holds no selectable text view answers `""` WITHOUT a gesture;
    /// otherwise Select All on its text view, answering what it selected.
    static func selectText(selectableAttribute: String?, sentinel: NSView?) -> String {
        explainedSelectText(selectableAttribute: selectableAttribute, sentinel: sentinel).selected
    }

    /// `selectText` with the step that decided an empty answer named in `why`
    /// (`nil` when something was selected) — the route returns it beside
    /// `selected`, so a red e2e read says which step answered no selection
    /// (convention 6) rather than only that none was made.
    static func explainedSelectText(
        selectableAttribute: String?, sentinel: NSView?
    ) -> (selected: String, why: String?) {
        if selectableAttribute == "false" {
            return ("", "the element is published selectable=false")
        }
        guard let sentinel else { return ("", "no sentinel view for the element") }
        guard let window = sentinel.window, let root = window.contentView else {
            return ("", "the sentinel is detached from any window")
        }
        let frame = sentinel.convert(sentinel.bounds, to: nil)
        guard let view = textView(in: root, holdingFrame: frame) else {
            return ("", "no selectable text view inside the element's frame \(describe(frame)); "
                + "text views in its window: \(textViewFrames(in: root)); "
                + "views over the frame: \(classesOver(frame, in: root.superview ?? root))")
        }
        let (selected, why) = explainedSelectAll(in: view)
        let at = describe(view.convert(view.bounds, to: nil))
        return (selected, why.map { "\($0) (text view at \(at), element frame \(describe(frame)))" })
    }

    /// Make `view` its window's first responder and send `selectAll:` — the
    /// action Edit ▸ Select All sends — to what the window then holds as first
    /// responder (the view itself, or a text field's field editor), then answer
    /// what that selection holds (`""` when nothing could be selected). The
    /// selection is left standing.
    static func selectAll(in view: NSView) -> String {
        explainedSelectAll(in: view).selected
    }

    /// `selectAll(in:)` with the refusing step named in `why`.
    static func explainedSelectAll(in view: NSView) -> (selected: String, why: String?) {
        guard let window = view.window else { return ("", "the text view has no window") }
        guard window.makeFirstResponder(view) else {
            return ("", "the window refused the text view as first responder")
        }
        if let field = view as? NSTextField {
            // A text field's own Select All: it installs the window's field
            // editor over itself and selects the whole text in it.
            field.selectText(nil)
        }
        let responder: NSResponder = window.firstResponder ?? view
        let kind = String(describing: type(of: responder))
        // An explicit target that does not implement the action raises rather
        // than answering false, so ask first.
        guard responder.responds(to: #selector(NSResponder.selectAll(_:))),
              NSApp.sendAction(#selector(NSResponder.selectAll(_:)), to: responder, from: nil)
        else {
            return ("", "the first responder (\(kind)) did not take selectAll:")
        }
        guard let text = selection(of: responder) else {
            var detail = ""
            if let text = responder as? NSTextView {
                detail = "; string length \((text.string as NSString).length), "
                    + "selected range \(NSStringFromRange(text.selectedRange()))"
            }
            return ("", "the first responder (\(kind)) served no string selection\(detail)")
        }
        return text.isEmpty ? ("", "the Services write held an empty string") : (text, nil)
    }

    /// Every selectable text view under `root`, with its window-space frame,
    /// for a failure message.
    static func textViewFrames(in root: NSView) -> String {
        var found: [String] = []
        var stack: [NSView] = [root]
        while !stack.isEmpty {
            let view = stack.removeFirst()
            if isSelectableTextView(view) {
                let hidden = view.isHiddenOrHasHiddenAncestor ? " hidden" : ""
                found.append(describe(view.convert(view.bounds, to: nil)) + hidden)
            }
            stack.append(contentsOf: view.subviews)
        }
        return found.isEmpty ? "none" : "\(found.count): " + found.prefix(12).joined(separator: ", ")
    }

    /// The class names (with frames) of the views under `root` whose
    /// window-space frame meets `frame`, innermost last — what the element's
    /// pixels are drawn by, for a failure message.
    static func classesOver(_ frame: CGRect, in root: NSView) -> String {
        var found: [String] = []
        var stack: [NSView] = [root]
        while !stack.isEmpty {
            let view = stack.removeFirst()
            let rect = view.convert(view.bounds, to: nil)
            if rect.intersects(frame) {
                found.append(String(describing: type(of: view)) + describe(rect))
            }
            stack.append(contentsOf: view.subviews)
        }
        return "\(found.count): " + found.suffix(16).joined(separator: ", ")
    }

    private static func describe(_ r: CGRect) -> String {
        String(format: "(%.1f,%.1f %.1fx%.1f)", r.minX, r.minY, r.width, r.height)
    }

    /// What `responder`'s selection holds, read through its own Services write
    /// into a private pasteboard; `nil` when it serves no string selection.
    static func selection(of responder: NSResponder) -> String? {
        if let text = responder as? NSTextView {
            // A text view (a text field's field editor) holds its selection as a
            // range over its own string; its Services write declines for a
            // non-editable field editor (measured 2026-11-19), so the range is
            // read directly — still the platform's selection, never one written.
            let range = text.selectedRange()
            let whole = text.string as NSString
            guard range.length > 0, NSMaxRange(range) <= whole.length else { return nil }
            return whole.substring(with: range)
        }
        guard let requestor = responder.validRequestor(forSendType: .string, returnType: nil)
            as? NSServicesMenuRequestor else { return nil }
        let board = NSPasteboard.withUniqueName()
        defer { board.releaseGlobally() }
        // An optional protocol method: a requestor that does not implement it
        // serves no selection.
        guard requestor.writeSelection?(to: board, types: [.string]) == true else { return nil }
        return board.string(forType: .string)
    }

    /// What the current selection holds: the key window's first responder's,
    /// else the first visible window's whose first responder serves one; `""`
    /// when nothing is selected anywhere (web's `Selection.toString()` shape).
    static func currentSelection() -> String {
        var windows = NSApp.windows.filter { $0.isVisible }
        if let key = NSApp.keyWindow {
            windows.removeAll { $0 === key }
            windows.insert(key, at: 0)
        }
        for window in windows {
            if let responder = window.firstResponder,
               let text = selection(of: responder), !text.isEmpty {
                return text
            }
        }
        return ""
    }
}
#endif

// The ios route's decision over its three Copy counts, kept free of UIKit so a
// host unit test pins it (`TextSelectionReadTests`); the gesture itself exists
// only in the ios simulator and is witnessed in e2e alone.
#if DEBUG
enum LongPressVerdict {
    /// What the press left, read by the ios route: the in-place selection, and
    /// how many showing labels carried UIKit's Copy label before the press,
    /// after it, and after the route took its menu down.
    struct Reading: Equatable {
        let selected: String
        let copyBefore: Int
        let copyAfter: Int
        let copyAfterDismiss: Int
    }

    enum Outcome: Equatable {
        /// `{selected, copy_offered, why}` — `why` only when both are empty.
        case answer(selected: String, copyOffered: Bool, why: String?)
        /// The menu the press raised still stands: the route answers an error
        /// naming it, never a reading.
        case menuStands(String)
    }

    static func decide(_ reading: Reading, pressedAt: String, copyLabel: String) -> Outcome {
        if reading.copyAfterDismiss > reading.copyBefore {
            return .menuStands(
                "the edit menu the press raised still stands after the dismissal "
                + "(\(reading.copyAfterDismiss) showing \"\(copyLabel)\" labels, "
                + "\(reading.copyBefore) before the press)")
        }
        let offered = reading.copyAfter > reading.copyBefore
        let why: String? = reading.selected.isEmpty && !offered
            ? pressedAt + " selected nothing in place and raised no \"\(copyLabel)\" action"
            : nil
        return .answer(selected: reading.selected, copyOffered: offered, why: why)
    }
}
#endif

// The ios leg (`e2e-conventions.md`, the same section — the ios long-press
// paragraph and its Implementation status bullet).
//
// **A long-press, synthesised inside the app process.** The selectable `Text`'s
// `TextSelectionInteractionView` implements `copy:` alone — no `selectAll:`, and
// no responder in the chain takes it — so the selection exists only as the
// gesture a person makes: a long-press on a word. This makes that gesture: a
// `UITouch` carrying an IOKit digitizer event, delivered through the app's own
// `UIApplication.sendEvent`, the way in-process UI test frameworks deliver
// touches. It needs no privacy grant and moves no machine-wide pointer (the two
// costs macos rejected `CGEvent` posting for). No selection range is ever
// written — writing `selectedTextRange` would be a flag set for the test.
//
// **Every private entry point is looked up by name at run time** — the UITouch
// and UIEvent setters by selector, the IOKit functions by `dlsym` — and one that
// is missing answers a `Failure` naming it, never a crash and never an empty
// selection a test would read as "not selectable". They move between iOS
// releases; the measured set is iOS 27.0's.
//
// **The read** is the text view's own `UITextInput`: `text(in: selectedTextRange)`.
//
// **Measured on the real app (2026-11-19, iOS 27.0 simulator): inside the app's
// layout the press selects nothing in place.** On the feed's `error-message` and
// the device card's `device-own-fingerprint` no `TextSelectionInteractionView`
// exists before or after the press; the text is a `CGDrawingView`, and the press
// raises the edit menu with Copy and Share… (whole-text copy) — unlike a bare
// `Text` in a stack, which carries the view and selects a word. Both are the
// platform's own selection, so `selectText` answers either: the in-place
// selection (`selected`), or whether the press raised the platform's Copy
// action (`copy_offered`) — recognised by UIKit's own Copy label, counted over
// every window's showing labels before and after the press, so a page's own
// Copy button is no reading and no private class is matched.
//
// **The route takes its menu down before it answers**, so no menu stands into a
// shared app's next test: first the published `UIEditMenuInteraction
// .dismissMenu()` on every window's views, then — only if the Copy count is
// still up — a synthesised tap away from the menu and the element; a count
// still above its pre-press value is an error naming the menu, never a reading.
#if DEBUG && os(iOS)
import UIKit
import Darwin

enum TextSelectionRead {
    /// The class SwiftUI backs a selectable `Text` with on iOS, matched by
    /// name: it is private, so no type check is possible.
    static let textViewClassName = "TextSelectionInteractionView"

    /// The long-press's measured timings (the design's probe on the iOS 27.0
    /// simulator): the finger held this long, a stationary event every
    /// `stepMs`, then `settleMs` after the lift for the selection to stand.
    static let holdMs = 1200
    static let stepMs = 100
    static let settleMs = 400

    /// Why the gesture could not be made — most often a private entry point
    /// this iOS does not have, named.
    struct Failure: Error, CustomStringConvertible {
        let description: String
    }

    // MARK: finding the text view

    /// The selectable text view the element's frame holds: the first view, in
    /// subview order under `root`, whose class name carries
    /// `textViewClassName` and whose window-space frame lies inside `frame`
    /// (the registration sentinel sits in the element's background, so its
    /// frame is the element's). `nil` when the element shows no selectable text.
    @MainActor
    static func textView(in root: UIView, holdingFrame frame: CGRect) -> UIView? {
        let slack = frame.insetBy(dx: -1, dy: -1)
        var stack: [UIView] = [root]
        while !stack.isEmpty {
            let view = stack.removeFirst()
            if String(describing: type(of: view)).contains(textViewClassName),
               !isHiddenOrHasHiddenAncestor(view) {
                let rect = view.convert(view.bounds, to: nil)
                if rect.width > 0, rect.height > 0, slack.contains(rect) { return view }
            }
            stack.append(contentsOf: view.subviews)
        }
        return nil
    }

    /// The selectable text view the sentinel's element holds, `nil` when there
    /// is none (or the sentinel is detached).
    @MainActor
    static func textView(forSentinel sentinel: UIView) -> UIView? {
        guard let window = sentinel.window else { return nil }
        return textView(in: window, holdingFrame: sentinel.convert(sentinel.bounds, to: nil))
    }

    @MainActor
    private static func isHiddenOrHasHiddenAncestor(_ view: UIView) -> Bool {
        var next: UIView? = view
        while let current = next {
            if current.isHidden || current.alpha == 0 { return true }
            next = current.superview
        }
        return false
    }

    /// Where the finger goes down, in window space: on the text's first
    /// glyph — a small inset from the view's leading edge, at the first line's
    /// mid-height. The view's `UITextInput` reports an empty document until
    /// its first selection (`position(from:offset:)` answers nil), so the point
    /// comes from the view's frame, never from a text position.
    @MainActor
    static func pressPoint(in view: UIView) -> CGPoint {
        let bounds = view.bounds
        let lineHalf = UIFont.preferredFont(forTextStyle: .body).lineHeight / 2
        let inset = min(3, bounds.width / 2)
        let rtl = view.effectiveUserInterfaceLayoutDirection == .rightToLeft
        let local = CGPoint(
            x: rtl ? bounds.maxX - inset : bounds.minX + inset,
            y: bounds.minY + min(bounds.height / 2, lineHalf))
        return view.convert(local, to: nil)
    }

    // MARK: reading

    /// What `view`'s selection holds, through its own `UITextInput`; `nil`
    /// when it is no text input.
    @MainActor
    static func selection(of view: UIView) -> String? {
        guard let input = view as? UITextInput else { return nil }
        guard let range = input.selectedTextRange, !range.isEmpty else { return "" }
        return input.text(in: range) ?? ""
    }

    /// What the current selection holds: the first-responder text input's of
    /// the key window, else of the first window that has one; `""` when
    /// nothing is selected anywhere (web's `Selection.toString()` shape).
    @MainActor
    static func currentSelection() -> String {
        let windows = UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows)
            .filter { !$0.isHidden }
            .sorted { $0.isKeyWindow && !$1.isKeyWindow }
        for window in windows {
            if let responder = firstResponder(in: window),
               let text = selection(of: responder), !text.isEmpty {
                return text
            }
        }
        return ""
    }

    @MainActor
    private static func firstResponder(in root: UIView) -> UIView? {
        var stack: [UIView] = [root]
        while !stack.isEmpty {
            let view = stack.removeFirst()
            if view.isFirstResponder, view is UITextInput { return view }
            stack.append(contentsOf: view.subviews)
        }
        return nil
    }

    // MARK: the gesture

    /// What the route answers: the in-place selection, whether the press
    /// raised the platform's Copy action, and — when both are empty — why, for
    /// the driver's failure message (convention 6). `copyLabel` and
    /// `labelFromTable` say which label was counted (the English word when
    /// UIKit's table has none); `dismissedBy` which step took the menu down.
    struct Answer {
        let selected: String
        let copyOffered: Bool
        let why: String?
        var copyLabel: String = ""
        var labelFromTable = true
        var dismissedBy: String?
    }

    // MARK: the Copy action

    /// The label UIKit itself gives its Copy command, from UIKit's own string
    /// table in the device's language; the English word (and `false`) when the
    /// table has no such entry.
    static func copyLabel() -> (label: String, fromTable: Bool) {
        let missing = "\u{1}no-entry"
        let bundle = Bundle(for: UIApplication.self)
        for table in [nil, "Localizable"] as [String?] {
            let value = bundle.localizedString(forKey: "Copy", value: missing, table: table)
            if value != missing, !value.isEmpty { return (value, true) }
        }
        return ("Copy", false)
    }

    /// Every showing window of the app, the key window first.
    @MainActor
    static func showingWindows() -> [UIWindow] {
        UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows)
            .filter { !$0.isHidden }
            .sorted { $0.isKeyWindow && !$1.isKeyWindow }
    }

    /// The showing labels carrying exactly `label`, over every window.
    @MainActor
    static func copyLabels(_ label: String) -> [UILabel] {
        var found: [UILabel] = []
        for window in showingWindows() {
            var stack: [UIView] = [window]
            while !stack.isEmpty {
                let view = stack.removeFirst()
                if let shown = view as? UILabel, shown.text == label,
                   !isHiddenOrHasHiddenAncestor(shown), shown.window != nil {
                    found.append(shown)
                }
                stack.append(contentsOf: view.subviews)
            }
        }
        return found
    }

    /// The first dismissal: the published `dismissMenu()` of every edit-menu
    /// interaction on every window's views. Answers how many it asked.
    @MainActor
    static func dismissEditMenus() -> Int {
        var asked = 0
        for window in showingWindows() {
            var stack: [UIView] = [window]
            while !stack.isEmpty {
                let view = stack.removeFirst()
                for case let interaction as UIEditMenuInteraction in view.interactions {
                    interaction.dismissMenu()
                    asked += 1
                }
                stack.append(contentsOf: view.subviews)
            }
        }
        return asked
    }

    /// Where the second dismissal taps, in the window of the menu's first
    /// Copy label: a point inside the window's safe area, outside the menu (the
    /// widest ancestor of the label still narrower than the window) and outside
    /// the element, hit-testing to no control. `nil` when no such point exists.
    @MainActor
    static func tapAway(from label: UILabel, element: CGRect) -> (UIWindow, CGPoint)? {
        guard let window = label.window else { return nil }
        var menu = label.convert(label.bounds, to: nil)
        var next = label.superview
        while let view = next, view !== window {
            let rect = view.convert(view.bounds, to: nil)
            if rect.width < window.bounds.width * 0.95 { menu = menu.union(rect) }
            next = view.superview
        }
        let area = window.bounds.inset(by: window.safeAreaInsets).insetBy(dx: 12, dy: 12)
        let candidates = [
            CGPoint(x: area.midX, y: area.maxY - 40), CGPoint(x: area.minX, y: area.midY),
            CGPoint(x: area.maxX, y: area.midY), CGPoint(x: area.midX, y: area.midY),
            CGPoint(x: area.midX, y: area.minY + 80),
        ]
        let avoid = [menu.insetBy(dx: -8, dy: -8), element.insetBy(dx: -8, dy: -8)]
        for point in candidates where !avoid.contains(where: { $0.contains(point) }) {
            let hit = window.hitTest(point, with: nil)
            var control = false
            var up = hit
            while let view = up {
                if view is UIControl { control = true; break }
                up = view.superview
            }
            if !control { return (window, point) }
        }
        return nil
    }

    /// The route's decision, whole, called OFF the main thread: an element
    /// whose `selectable` attribute is `"false"` answers `""` WITHOUT a
    /// gesture; otherwise the long-press on the element's text, answering what
    /// it selected.
    ///
    /// **The text view may not exist before the press.** Inside the app's own
    /// layout (a list, a scroll view) SwiftUI draws a selectable `Text` as a
    /// plain drawing view and builds its `TextSelectionInteractionView` only
    /// when the person's long-press asks for one (measured 2026-11-19 on the
    /// feed's `error-message`: no such view anywhere in the window before the
    /// press); a bare `Text` in a stack has one from the start. So the press
    /// goes on the text view when there is one, else on the element's own
    /// frame (the sentinel's, which is the `Text`'s), and the selection is
    /// read from the text view the element's frame holds AFTER the press — a
    /// text with selection off never gets one, and answers `""`.
    static func selectText(
        resolve: @MainActor () -> (selectableAttribute: String?, sentinel: UIView?)
    ) -> Result<Answer, Failure> {
        guard !Thread.isMainThread else {
            return .failure(Failure(description:
                "the long-press must be driven off the main thread (its waits would block the gesture)"))
        }
        let found: (target: UIView?, sentinel: UIView?, reason: String?) = onMain {
            let (selectable, sentinel) = resolve()
            if selectable == "false" { return (nil, nil, "the element is published selectable=false") }
            guard let sentinel, sentinel.window != nil else {
                return (nil, nil, "the element's sentinel is not realized")
            }
            return (textView(forSentinel: sentinel) ?? sentinel, sentinel, nil)
        }
        guard let target = found.target, found.sentinel != nil else {
            return .success(Answer(selected: "", copyOffered: false, why: found.reason))
        }
        let (label, fromTable) = copyLabel()
        let copyBefore: Int = onMain { copyLabels(label).count }
        var pressedAt = ""
        do {
            let finger: Finger = try onMain {
                guard let window = target.window else {
                    throw Failure(description: "the element's text is not in a window")
                }
                let point = pressPoint(in: target)
                let hit = window.hitTest(point, with: nil).map { "\(type(of: $0))" } ?? "nothing"
                pressedAt = "the long-press at \(point) on \(type(of: target)) "
                    + "\(target.convert(target.bounds, to: nil)) (hit-testing to \(hit))"
                return try Finger(window: window, point: point)
            }
            try onMain { try finger.send(.began) }
            var held = 0
            while held < holdMs {
                Thread.sleep(forTimeInterval: Double(stepMs) / 1000)
                held += stepMs
                try onMain { try finger.send(.stationary) }
            }
            try onMain { try finger.send(.ended) }
        } catch let failure as Failure {
            return .failure(failure)
        } catch {
            return .failure(Failure(description: "\(error)"))
        }
        Thread.sleep(forTimeInterval: Double(settleMs) / 1000)
        // The element is resolved again after the press: the press may rebuild
        // the view the pre-press sentinel sat in (measured: detached at once).
        let after: (selected: String, copy: Int, element: CGRect, detail: String) = onMain {
            let sentinel = resolve().sentinel
            let element = sentinel.map { $0.convert($0.bounds, to: nil) } ?? .null
            var selected = ""
            var detail = ""
            if let sentinel, let view = textView(forSentinel: sentinel) {
                selected = selection(of: view) ?? ""
            } else if let sentinel {
                detail = "; no \(textViewClassName) inside the element's frame; "
                    + candidates(near: sentinel)
            } else {
                detail = "; the element's sentinel is gone after the press; " + menus()
            }
            return (selected, copyLabels(label).count, element, detail)
        }
        // Take down the menu the press raised, then verify it is gone.
        var dismissedBy: String?
        var copyAfterDismiss = after.copy
        if after.copy > copyBefore {
            let asked: Int = onMain { dismissEditMenus() }
            copyAfterDismiss = waitForCopyCount(label, atMost: copyBefore)
            if copyAfterDismiss <= copyBefore {
                dismissedBy = "dismissMenu (\(asked) edit-menu interactions asked)"
            } else {
                do {
                    let finger: Finger? = try onMain {
                        guard let shown = copyLabels(label).first,
                              let (window, point) = tapAway(from: shown, element: after.element)
                        else { return nil }
                        let finger = try Finger(window: window, point: point)
                        try finger.send(.began)
                        return finger
                    }
                    if let finger {
                        Thread.sleep(forTimeInterval: Double(stepMs) / 1000)
                        try onMain { try finger.send(.ended) }
                    }
                } catch let failure as Failure {
                    return .failure(failure)
                } catch {
                    return .failure(Failure(description: "\(error)"))
                }
                copyAfterDismiss = waitForCopyCount(label, atMost: copyBefore)
                if copyAfterDismiss <= copyBefore {
                    dismissedBy = "a tap away from the menu, after dismissMenu "
                        + "(\(asked) interactions asked) left it standing"
                }
            }
        }
        let reading = LongPressVerdict.Reading(
            selected: after.selected, copyBefore: copyBefore,
            copyAfter: after.copy, copyAfterDismiss: copyAfterDismiss)
        switch LongPressVerdict.decide(reading, pressedAt: pressedAt + after.detail, copyLabel: label) {
        case .menuStands(let what):
            return .failure(Failure(description: what
                + (dismissedBy == nil ? "; dismissMenu and a tap away both left it" : "")))
        case let .answer(selected, offered, why):
            return .success(Answer(
                selected: selected, copyOffered: offered, why: why,
                copyLabel: label, labelFromTable: fromTable, dismissedBy: dismissedBy))
        }
    }

    /// Polls the Copy count (every `stepMs`, up to 1.5 s) until it is at most
    /// `limit`, answering the last count read — the menu's dismissal animates.
    static func waitForCopyCount(_ label: String, atMost limit: Int) -> Int {
        var count = Int.max
        var waited = 0
        while waited <= 1500 {
            count = onMain { copyLabels(label).count }
            if count <= limit { return count }
            Thread.sleep(forTimeInterval: Double(stepMs) / 1000)
            waited += stepMs
        }
        return count
    }

    /// For the no-text-view reason: the window's text views (class and frame)
    /// and the classes of the views whose frame overlaps the element's.
    @MainActor
    private static func candidates(near sentinel: UIView) -> String {
        guard let window = sentinel.window else { return "detached; " + menus() }
        let frame = sentinel.convert(sentinel.bounds, to: nil)
        var textViews: [String] = []
        var overlapping: [String] = []
        var stack: [UIView] = [window]
        while !stack.isEmpty {
            let view = stack.removeFirst()
            let name = String(describing: type(of: view))
            let rect = view.convert(view.bounds, to: nil)
            if name.contains(textViewClassName) {
                textViews.append("\(name) \(rect) hidden=\(isHiddenOrHasHiddenAncestor(view))")
            } else if rect.intersects(frame), rect.width < window.bounds.width * 0.98 {
                overlapping.append("\(name) \(rect)")
            }
            stack.append(contentsOf: view.subviews)
        }
        return "text views: [\(textViews.prefix(6).joined(separator: "; "))]; "
            + "overlapping: [\(overlapping.prefix(10).joined(separator: "; "))]; " + menus()
    }

    /// What a press raised instead of a selection: every window's class, and
    /// the menu, context-menu and preview views in it (with the labels of the
    /// menu's items when they are readable).
    @MainActor
    private static func menus() -> String {
        var parts: [String] = []
        for scene in UIApplication.shared.connectedScenes {
            guard let windowScene = scene as? UIWindowScene else { continue }
            for window in windowScene.windows where !window.isHidden {
                var found: [String] = []
                var labels: [String] = []
                var stack: [UIView] = [window]
                while !stack.isEmpty {
                    let view = stack.removeFirst()
                    let name = String(describing: type(of: view))
                    if name.contains("Menu") || name.contains("Preview") || name.contains("Context") {
                        found.append(name)
                    }
                    if let label = view as? UILabel, let text = label.text, !text.isEmpty,
                       found.contains(where: { $0.contains("Menu") }) {
                        labels.append(text)
                    }
                    stack.append(contentsOf: view.subviews)
                }
                parts.append("\(type(of: window)): [\(Set(found).sorted().prefix(8).joined(separator: ", "))]"
                    + (labels.isEmpty ? "" : " labels \(labels.prefix(8))"))
            }
        }
        return "windows: " + parts.joined(separator: " | ")
    }

    private static func onMain<T>(_ work: @MainActor () throws -> T) rethrows -> T {
        try DispatchQueue.main.sync { try MainActor.assumeIsolated(work) }
    }

    /// One finger: a `UITouch` driven through its private setters, each event
    /// carrying an IOKit digitizer event (a hand transducer with one finger
    /// appended), delivered through `UIApplication`'s own touches event.
    @MainActor
    final class Finger {
        typealias HIDEvent = UnsafeMutableRawPointer

        private typealias CreateDigitizer = @convention(c) (
            CFAllocator?, UInt64, UInt32, UInt32, UInt32, UInt32, UInt32,
            Double, Double, Double, Double, Double, Bool, Bool, UInt32) -> HIDEvent?
        private typealias CreateFinger = @convention(c) (
            CFAllocator?, UInt64, UInt32, UInt32, UInt32,
            Double, Double, Double, Double, Double, Bool, Bool, UInt32) -> HIDEvent?
        private typealias AppendEvent = @convention(c) (HIDEvent, HIDEvent, UInt32) -> Void

        private typealias SetObject = @convention(c) (NSObject, Selector, AnyObject?) -> Void
        private typealias SetInt = @convention(c) (NSObject, Selector, Int) -> Void
        private typealias SetBool = @convention(c) (NSObject, Selector, Bool) -> Void
        private typealias SetDouble = @convention(c) (NSObject, Selector, Double) -> Void
        private typealias SetPointBool = @convention(c) (NSObject, Selector, CGPoint, Bool) -> Void
        private typealias SetPointer = @convention(c) (NSObject, Selector, HIDEvent?) -> Void
        private typealias AddTouch = @convention(c) (NSObject, Selector, AnyObject, Bool) -> Void
        private typealias NoArgument = @convention(c) (NSObject, Selector) -> Void

        private let touch = UITouch()
        private let window: UIWindow
        private let point: CGPoint
        private let createDigitizer: CreateDigitizer
        private let createFinger: CreateFinger
        private let append: AppendEvent

        /// Looks every IOKit function up first, so a missing one fails the
        /// gesture before any touch went down.
        init(window: UIWindow, point: CGPoint) throws {
            self.window = window
            self.point = point
            guard let iokit = dlopen("/System/Library/Frameworks/IOKit.framework/IOKit", RTLD_NOW) else {
                throw Failure(description: "IOKit could not be opened for the digitizer event")
            }
            func symbol<T>(_ name: String, _: T.Type) throws -> T {
                guard let pointer = dlsym(iokit, name) else {
                    throw Failure(description: "IOKit has no \(name) on this iOS")
                }
                return unsafeBitCast(pointer, to: T.self)
            }
            createDigitizer = try symbol("IOHIDEventCreateDigitizerEvent", CreateDigitizer.self)
            createFinger = try symbol("IOHIDEventCreateDigitizerFingerEvent", CreateFinger.self)
            append = try symbol("IOHIDEventAppendEvent", AppendEvent.self)
        }

        private func call<T>(_ object: NSObject, _ name: String, _: T.Type) throws -> (T, Selector) {
            let selector = NSSelectorFromString(name)
            guard object.responds(to: selector) else {
                throw Failure(description: "\(type(of: object)) has no \(name) on this iOS")
            }
            return (unsafeBitCast(object.method(for: selector), to: T.self), selector)
        }

        /// A hand transducer event with one finger appended, at `point`. The
        /// event is created +1 and handed to UIKit, which keeps it as long as
        /// the touch needs it; the few bytes a press leaves are DEBUG-only.
        private func digitizerEvent(_ phase: UITouch.Phase) throws -> HIDEvent {
            let time = mach_absolute_time()
            let touching = phase != .ended && phase != .cancelled
            let mask: UInt32 = phase == .moved ? 4 : 3  // position, or range + touch
            guard let hand = createDigitizer(
                    kCFAllocatorDefault, time, 3, 0, 0, 2, 0, 0, 0, 0, 0, 0, false, touching, 0),
                  let finger = createFinger(
                    kCFAllocatorDefault, time, 1, 2, mask, Double(point.x), Double(point.y),
                    0, 0, 0, touching, touching, 0) else {
                throw Failure(description: "IOKit made no digitizer event")
            }
            append(hand, finger, 0)
            return hand
        }

        func send(_ phase: UITouch.Phase) throws {
            if phase == .began {
                let hit = window.hitTest(point, with: nil)
                let (setWindow, setWindowSelector) = try call(touch, "setWindow:", SetObject.self)
                setWindow(touch, setWindowSelector, window)
                let (setView, setViewSelector) = try call(touch, "setView:", SetObject.self)
                setView(touch, setViewSelector, hit)
                if touch.responds(to: NSSelectorFromString("setGestureView:")) {
                    let (setGestureView, setGestureViewSelector) = try call(touch, "setGestureView:", SetObject.self)
                    setGestureView(touch, setGestureViewSelector, hit)
                }
                let (setFirst, setFirstSelector) = try call(touch, "_setIsFirstTouchForView:", SetBool.self)
                setFirst(touch, setFirstSelector, true)
                let (setTapCount, setTapCountSelector) = try call(touch, "setTapCount:", SetInt.self)
                setTapCount(touch, setTapCountSelector, 1)
            }
            let (setLocation, setLocationSelector) = try call(touch, "_setLocationInWindow:resetPrevious:", SetPointBool.self)
            setLocation(touch, setLocationSelector, point, phase == .began)
            let (setPhase, setPhaseSelector) = try call(touch, "setPhase:", SetInt.self)
            setPhase(touch, setPhaseSelector, phase.rawValue)
            let (setTimestamp, setTimestampSelector) = try call(touch, "setTimestamp:", SetDouble.self)
            setTimestamp(touch, setTimestampSelector, ProcessInfo.processInfo.systemUptime)
            let hid = try digitizerEvent(phase)
            let (setTouchHID, setTouchHIDSelector) = try call(touch, "_setHidEvent:", SetPointer.self)
            setTouchHID(touch, setTouchHIDSelector, hid)

            let app = UIApplication.shared
            let eventSelector = NSSelectorFromString("_touchesEvent")
            guard app.responds(to: eventSelector),
                  let event = app.perform(eventSelector)?.takeUnretainedValue() as? UIEvent else {
                throw Failure(description: "UIApplication has no _touchesEvent on this iOS")
            }
            let (clear, clearSelector) = try call(event, "_clearTouches", NoArgument.self)
            clear(event, clearSelector)
            let (setEventHID, setEventHIDSelector) = try call(event, "_setHIDEvent:", SetPointer.self)
            setEventHID(event, setEventHIDSelector, hid)
            let (addTouch, addTouchSelector) = try call(event, "_addTouch:forDelayedDelivery:", AddTouch.self)
            addTouch(event, addTouchSelector, touch, false)
            app.sendEvent(event)
        }
    }
}
#endif
