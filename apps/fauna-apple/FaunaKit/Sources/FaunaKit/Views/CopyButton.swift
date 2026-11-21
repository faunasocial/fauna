import SwiftUI

/// Shared copy-to-clipboard button with accessibility identifier for e2e tests.
///
/// All apps that need `*-copy-btn` elements should use this component.
/// It handles platform-specific clipboard APIs internally.
///
/// Usage:
/// ```swift
/// CopyButton("account-actor-id-copy-btn", text: actorId)
/// ```
public struct CopyButton: View {
    let accessibilityId: String
    let text: String
    /// The exact string last written to the clipboard by THIS button, `nil`
    /// before any tap — the `copied` attr's read value (ui.yaml
    /// `profile-actor-id-copy-btn`'s contract: "the exact string it put on
    /// the clipboard, cleared on every profile open"). Cleared via
    /// `.onChange(of: text)` since a changed bound value (a different
    /// profile, a different id) is a fresh subject with nothing copied yet.
    @State private var copied: String?
    /// Drives the transient "Copied!" label (linux/web parity — both revert
    /// after 2s); separate from `copied` above, which the automation attr
    /// keeps set until `text` changes rather than timing out.
    @State private var showConfirmation = false

    /// A visible caption in place of the copy icon (`admin-dns`'s per-record
    /// "Copy" button); `nil` keeps the icon.
    let title: String?

    /// Where the copy's outcome goes beyond the button's own confirmation —
    /// a page that owns an error line puts a failure there (`admin-users`'s
    /// `admin-users-action-error`); `nil` keeps the button self-contained.
    let onOutcome: ((CopyOutcome) -> Void)?

    public init(
        _ accessibilityId: String, text: String, title: String? = nil,
        onOutcome: ((CopyOutcome) -> Void)? = nil
    ) {
        self.accessibilityId = accessibilityId
        self.text = text
        self.title = title
        self.onOutcome = onOutcome
    }

    public var body: some View {
        Button {
            copy()
        } label: {
            if showConfirmation {
                Text(L.common.copied)
                    .font(.caption)
            } else if let title {
                Text(title)
                    .font(.caption)
            } else {
                Image(systemName: "doc.on.doc")
                    .font(.caption)
            }
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier(accessibilityId)
        // In-process e2e driver actuation, wired once here so every
        // `*-copy-btn` call site is covered. Fires the same `copy()` the
        // Button action does, and reports what it copied through the
        // `copied` attribute rather than the element's read value (no driver
        // reads the OS clipboard — the web-settings copy-link-button
        // contract) — env-gated no-op in production.
        .automationActivate(accessibilityId, value: { text }, attributes: { ["copied": copied ?? ""] }) {
            copy()
        }
        .onChange(of: text) { copied = nil; showConfirmation = false }
    }

    private func copy() {
        // `CopyOutcome.attempt` logs a displayed success at `info`
        // (observability.md § What must be logged, category 1) — the
        // confirmation TEXT only, never the copied value (§ 2's redaction rule).
        let outcome = CopyOutcome.attempt(text)
        copied = outcome.copiedText
        showConfirmation = outcome.copiedText != nil
        onOutcome?(outcome)
        guard showConfirmation else { return }
        Task {
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            showConfirmation = false
        }
    }
}
