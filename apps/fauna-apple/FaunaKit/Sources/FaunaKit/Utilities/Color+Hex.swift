import SwiftUI

/// Parse a `#RRGGBB` (or `RRGGBB`) hex string into a SwiftUI `Color`.
///
/// The shared-Rust badge presentation (`fauna_core::content_category` /
/// `fauna_protocol::spam`) hands every native app hex colour strings (`tint` /
/// `accent`) so no client hard-codes the palette (moderation.md § Where logic
/// lives, drift #157) — the apple analogue of linux painting the same hex via
/// Pango markup and web via CSS. A malformed string falls back to `.gray` rather
/// than trapping, so an off-list producer colour can never crash a render.
public extension Color {
    init(hex: String) {
        let cleaned = hex.hasPrefix("#") ? String(hex.dropFirst()) : hex
        guard cleaned.count == 6, let rgb = UInt64(cleaned, radix: 16) else {
            self = .gray
            return
        }
        self.init(
            red: Double((rgb >> 16) & 0xFF) / 255.0,
            green: Double((rgb >> 8) & 0xFF) / 255.0,
            blue: Double(rgb & 0xFF) / 255.0
        )
    }
}
