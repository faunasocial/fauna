import Testing
import Foundation
@testable import FaunaKit

// D5 conversations-rail icon (`docs/goal/architecture/render-model.md` § Deltas
// → D5). The canonical *concept* per source lives in shared Rust
// (`fauna_core::source_glyph::SourceGlyph` + `Rail::glyph` / `rail_glyph`,
// tested in `fauna-conversations`); apple owns only the `SourceGlyph → emoji`
// rendering. These guard the apple seam: the per-concept emoji map and the
// full rail→concept→emoji chain through the `railGlyph` FFI binding, so the
// user-ratified mapping (Fauna→Fox, Email→Envelope, a bridge with
// no identity to hand→Bridge) can't silently drift on apple. Mirrors linux's
// `source_glyph_emoji_is_exhaustive`.

@Test func glyphEmojiMapsEveryConcept() {
    // Exhaustive over the concept set — a new SourceGlyph variant breaks this
    // (forces the apple map to pick its rendering, like every other app).
    #expect(ConversationsUI.glyphEmoji(.fox) == "🦊")
    // U+2709 + U+FE0F (emoji-presentation selector) — full-size color on Apple.
    #expect(ConversationsUI.glyphEmoji(.envelope) == "✉️")
    #expect(ConversationsUI.glyphEmoji(.butterfly) == "🦋")
    #expect(ConversationsUI.glyphEmoji(.bolt) == "⚡")
    #expect(ConversationsUI.glyphEmoji(.globe) == "🌐")
    #expect(ConversationsUI.glyphEmoji(.unknown) == "📡")
    #expect(ConversationsUI.glyphEmoji(.archive) == "📦")
    #expect(ConversationsUI.glyphEmoji(.bridge) == "🌉")
}

@Test func railGlyphChainMatchesRatifiedMapping() {
    // The off-snapshot path the recipient picker uses: rail → concept (shared
    // `railGlyph` FFI) → apple emoji. Pins the canonical mapping end-to-end.
    #expect(ConversationsUI.glyphEmoji(railGlyph(rail: .faunaMls)) == "🦊")
    #expect(ConversationsUI.glyphEmoji(railGlyph(rail: .smtp)) == "✉️")
    #expect(ConversationsUI.glyphEmoji(railGlyph(rail: .bridged)) == "🌉")
}
