import SwiftUI

// Read-only text the person reads can be selected and copied
// (`docs/goal/ui/README.md` § Text the person reads can be copied).
//
// ONE decision per app rather than a per-view flag — the apple twin of linux's
// `read_text.rs`: each app root applies `.faunaReadOnlyText()`, which turns on
// SwiftUI's own selection (`.textSelection(.enabled)`, rule 1) for every `Text`
// beneath it. `.textSelection` is an inherited environment setting, so every
// page, its status and outcome lines, values, explanatory copy and user content
// carry it without each view remembering to. Control captions stay controls on
// their own (a `Button`, `Toggle`, `Picker` or `Menu` label is not selectable
// text), and a surface whose click ACTS — a row that opens or selects
// something, a card, a dismiss-on-tap overlay — opts its subtree out with
// `.faunaActsOnClick()`, so selecting never changes what a click on it does
// (rule 3). SwiftUI repaints a `Text` whose string is unchanged without
// dropping its selection (rule 2).
//
// Both modifiers set `faunaTextSelectable` beside the platform setting, in the
// same call, so the automation registry reads the decision the platform paints
// with — never a flag of its own — and publishes it as the `selectable`
// attribute (`AutomationRegistry.folding(_:textSelectable:)`), the read behind
// the e2e driver's `is_text_selectable`.

// The `faunaTextSelectable` environment value itself is declared in
// `Testing/AutomationRegistry.swift`, beside `faunaPossibleReason`, so that file
// keeps typechecking standalone for iOS.

public extension View {
    /// Every `Text` beneath is selectable and copyable with the platform's own
    /// gesture. Applied once, at each app root (and at the root of any separate
    /// window or sheet scene that does not inherit it).
    func faunaReadOnlyText() -> some View {
        textSelection(.enabled).environment(\.faunaTextSelectable, true)
    }

    /// This subtree acts on a click (opens, selects or dismisses something), so
    /// its text stays unselectable and the click keeps meaning the click
    /// (§ Text the person reads can be copied, rule 3).
    func faunaActsOnClick() -> some View {
        textSelection(.disabled).environment(\.faunaTextSelectable, false)
    }
}
