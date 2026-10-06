import Foundation

/// Markdown compose-toolbar wrap for the apple apps, routed through the shared rule
/// (`fauna_core::markdown::wrap_selection` via the `wrapMarkdownSelection` UniFFI face) so
/// the marker strings + placeholder live once in shared Rust instead of being re-derived
/// per client (priority #1/#2). Mirrors the other apps' toolbar wrap (web
/// `MarkdownToolbar.svelte`, android `MarkdownCompose.kt`, windows `MarkdownAuthoring`).
///
/// **Empty-selection only, by platform constraint.** Apple's SwiftUI `TextField` exposes no
/// caret/selection at the iOS **17** deployment floor (`TextField(text:selection:)` is iOS 18 /
/// macOS 15), and these toolbars live in shared FaunaKit (macOS + iOS), so a real selection
/// binding isn't reachable without an iOS-17 availability fork. The buttons therefore append the
/// wrapped placeholder, which is exactly the shared rule's empty-selection case — apple never had
/// the raw-wrap "trailing space inside the markers" bug because there is no selection to wrap.
///
/// A selection-aware splice (re-select the wrapped core; keep a word-selection's edge whitespace
/// outside the markers) is the follow-on, blocked on a selection-reachable compose field: raise the
/// iOS floor to 18, or back the field with an NS/UITextView representable that exposes
/// `selectedRange`. The splice site already goes through the shared rule, so that follow-on only
/// swaps the empty `selected` for the real selected substring and splices `replacement` over the
/// range instead of appending.
public enum MarkdownCompose {
    /// The text a markdown-toolbar button inserts over an *empty* selection: `prefix` +
    /// placeholder + `suffix` per the shared rule — `**` → `**text**`, `*` → `*text*`,
    /// `` ` `` → `` `text` ``, `[`/`](url)` → `[text](url)`. Matches the other apps'
    /// empty-selection output (placeholder `"text"`). Used by the feed composer's
    /// `MarkdownToolbar`, which has no selection-reachable field.
    public static func insertion(prefix: String, suffix: String) -> String {
        wrapMarkdownSelection(selected: "", prefix: prefix, suffix: suffix, placeholder: "text").replacement
    }

    /// **Selection-aware** wrap for a compose field that exposes its `selectedRange`
    /// (the `dm-text-field` `NSTextView`/`UITextView` — see `MarkdownTextEditor`). Splices
    /// the shared rule's `replacement` over the selected `NSRange` and re-selects the
    /// wrapped **core**, so a word-selection's edge whitespace stays OUTSIDE the markers
    /// (`*italic* `, not `*italic *`) — the cross-app target the empty-selection
    /// `insertion` couldn't reach at the iOS 17 floor. An empty selection inserts the
    /// `"text"` placeholder at the caret (re-selected for immediate overtype). All offsets
    /// are measured in UTF-16 (`NSString` length) so the splice + re-select stay multi-byte
    /// correct (`beforeCore`/`core` come back from the shared rule as substrings).
    public static func wrap(
        text: String,
        selection: NSRange,
        prefix: String,
        suffix: String
    ) -> (text: String, selection: NSRange) {
        let ns = text as NSString
        let location = min(max(selection.location, 0), ns.length)
        let length = min(max(selection.length, 0), ns.length - location)
        let sel = NSRange(location: location, length: length)
        let selected = ns.substring(with: sel)

        let wrapped = wrapMarkdownSelection(selected: selected, prefix: prefix, suffix: suffix, placeholder: "text")
        let newText = ns.replacingCharacters(in: sel, with: wrapped.replacement)
        let coreLocation = sel.location + (wrapped.beforeCore as NSString).length
        let coreLength = (wrapped.core as NSString).length
        return (newText, NSRange(location: coreLocation, length: coreLength))
    }
}
