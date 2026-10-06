import SwiftUI

/// Markdown formatting toolbar with bold, italic, code, and link buttons.
public struct MarkdownToolbar: View {
    @Binding public var text: String

    public init(text: Binding<String>) {
        self._text = text
    }

    public var body: some View {
        HStack(spacing: 12) {
            Button { wrap("**") } label: {
                Image(systemName: "bold")
            }
            .accessibilityIdentifier(Ids.markdownBoldButton)

            // Italic uses `*` (not `_`) to stay uniform with linux/web/android and the
            // DM compose bar's toolbar; the shared renderer accepts both, but the toolbar
            // emits one marker family (priority #1/#3).
            Button { wrap("*") } label: {
                Image(systemName: "italic")
            }
            .accessibilityIdentifier(Ids.markdownItalicButton)

            Button { wrap("`") } label: {
                Image(systemName: "chevron.left.forwardslash.chevron.right")
            }
            .accessibilityIdentifier(Ids.markdownCodeButton)

            Button { insertLink() } label: {
                Image(systemName: "link")
            }
            .accessibilityIdentifier(Ids.markdownLinkButton)
        }
        .font(.caption)
        .buttonStyle(.bordered)
        .controlSize(.small)
        .accessibilityIdentifier(Ids.markdownToolbar)
    }

    // Wrap rule is shared (`fauna_core::markdown::wrap_selection` via the
    // `wrapMarkdownSelection` UniFFI face); `MarkdownCompose.insertion` runs it over an
    // empty selection (SwiftUI exposes no selection at the iOS 17 floor — see
    // `MarkdownCompose`) and we append the wrapped placeholder.
    private func wrap(_ marker: String) {
        text += MarkdownCompose.insertion(prefix: marker, suffix: marker)
    }

    private func insertLink() {
        text += MarkdownCompose.insertion(prefix: "[", suffix: "](url)")
    }
}
