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

    public init(_ accessibilityId: String, text: String) {
        self.accessibilityId = accessibilityId
        self.text = text
    }

    public var body: some View {
        Button {
            copy()
        } label: {
            if showConfirmation {
                Text(L.common.copied)
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
        Pasteboard.copy(text)
        copied = text
        showConfirmation = true
        // Category 1 (observability.md § What must be logged): a displayed
        // success line also logs, at its matching `info` level — the
        // confirmation TEXT only, never the copied value (§ 2's redaction rule).
        logMessage(level: .info, target: "fauna.copy_button", message: L.common.copied)
        Task {
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            showConfirmation = false
        }
    }
}
