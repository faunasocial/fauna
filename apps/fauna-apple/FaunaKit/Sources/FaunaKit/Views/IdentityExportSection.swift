import SwiftUI

/// The identity-export QR (settings.md § Identity export) — the counterpart of the
/// wizard's `identity_import` step: a second device scans this instead of the user
/// copying a 64-hex secret between machines by hand.
///
/// **Both halves are shared Rust** (settings.md § Where logic lives): the payload is
/// `IdentityQr::to_uri(secret, handle)` (exactly the URI the import parser accepts, so
/// export → scan → import is closed) and the rendering is `fauna_core::qr_matrix` → a
/// boolean grid at EC level M, which each app paints with its own toolkit. **No
/// client links a platform QR library** — that would be one encoder per app to keep in step
/// (priorities #1/#2). Apple was the last holdout: this drew through CoreImage's
/// `CIFilter.qrCodeGenerator` (plus an `NSImage`/`UIImage` `#if` fork) until 2026-07-12.
///
/// Reveal gate: the description and the toggle always render; the warning and the QR
/// appear only after the user presses show. Hiding is not a security control — it keeps
/// the secret off screen by accident (e.g. mid-screen-share). Nothing is persisted and no
/// server call is made.
public struct IdentityExportSection: View {
    let secretHex: String?
    /// The current handle, encoded into the QR so a scanned import pre-fills the
    /// handle step — the QR carries the `(identity, handle)` payload
    /// (`docs/goal/behavior/onboarding.md` §1.identity_import). `nil` writes the
    /// bare secret-only QR.
    let handle: String?

    /// The encoded grid, held as optional state rather than a `Bool` reveal flag: hiding
    /// then *drops* the encoded secret instead of retaining it off-screen (what linux,
    /// web and android do). `nil` = collapsed.
    @State private var matrix: QrMatrix?

    public init(secretHex: String?, handle: String? = nil) {
        self.secretHex = secretHex
        self.handle = handle
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            automationText(Ids.identityExportDescription, L.settings.identityExport.desc)
                .font(.caption)
                .foregroundStyle(.secondary)

            if let matrix {
                automationText(Ids.identityExportWarning, L.settings.identityExport.warning)
                    .font(.caption)
                    .foregroundStyle(.orange)

                QrCodeView(matrix: matrix)
                    .frame(width: 200, height: 200)
                    .accessibilityIdentifier(Ids.identityExportQr)
                    // Registers the QR's presence for the driver's reveal-gate read
                    // (`is_visible`); the value is the grid's side, not the secret —
                    // the payload never leaves the view.
                    .automationValue(Ids.identityExportQr, text: { "\(matrix.size)" })
            }

            Button(toggleLabel) { toggleQr() }
                .accessibilityIdentifier(Ids.identityExportShowQrButton)
                // ONE toggle whose label flips show_qr ↔ hide_qr (never a second
                // button); `value` backs the driver's label read, and the closure fires
                // the same `toggleQr()` the Button does.
                .automationActivate(
                    Ids.identityExportShowQrButton,
                    value: { toggleLabel }
                ) {
                    toggleQr()
                }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.identityExportSection)
        .automationValue(Ids.identityExportSection, text: { matrix == nil ? "hidden" : "shown" })
    }

    private var toggleLabel: String {
        matrix == nil ? L.settings.identityExport.showQr : L.settings.identityExport.hideQr
    }

    /// Reveal or collapse. Revealing encodes through the two shared-Rust faces; collapsing
    /// drops the matrix, so the encoded secret is not retained off-screen.
    private func toggleQr() {
        if matrix != nil {
            matrix = nil
            return
        }
        guard let secretHex else { return }
        // `qrMatrix` throws only on an oversize payload — an identity URI never is, but
        // a failure must not crash Settings, so it collapses instead.
        matrix = try? qrMatrix(data: identity_qr_encode(secretHex, handle: handle))
    }
}

/// Paints a shared-Rust `QrMatrix` (row-major, `modules[y * size + x]`, `true` = dark)
/// with SwiftUI's `Canvas` — the apple twin of linux's `DrawingArea` `draw_qr`, windows'
/// XAML `Canvas`, android's Compose `Canvas`, and web's inline SVG. One platform-agnostic
/// path, so the old `NSImage`/`UIImage` `#if` fork is gone.
struct QrCodeView: View {
    let matrix: QrMatrix

    var body: some View {
        Canvas { context, size in
            let side = Int(matrix.size)
            // The quiet zone comes from the shared constant, never a hard-coded 4: the
            // matrix has none baked in, and a code without one is refused by many
            // scanners.
            let quiet = Int(qrQuietZoneModules())
            let modulesPerSide = side + 2 * quiet
            guard side > 0, matrix.modules.count >= side * side else { return }

            let scale = min(size.width, size.height) / CGFloat(modulesPerSide)

            // Paint the whole box light first — that *is* the quiet zone at the edges.
            // Dark-on-light is set EXPLICITLY, not from theme colors: a theme-inverted QR
            // does not scan, so this is the one place dark mode is deliberately ignored.
            context.fill(Path(CGRect(origin: .zero, size: size)), with: .color(.white))

            for y in 0..<side {
                for x in 0..<side where matrix.modules[y * side + x] {
                    let rect = CGRect(
                        x: CGFloat(x + quiet) * scale,
                        y: CGFloat(y + quiet) * scale,
                        width: scale,
                        height: scale)
                    context.fill(Path(rect), with: .color(.black))
                }
            }
        }
    }
}
