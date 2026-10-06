using FaunaApp.Core.Helpers;
using uniffi.fauna_core;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Locks windows's single <c>SourceGlyph → emoji</c> map (the one platform-specific
/// half of source-icon rendering — render-model.md § Deltas → D5). The byte set
/// must match the other emoji clients (apple's <c>ConversationsUI.glyphEmoji</c>):
/// fox / envelope(+VS16) / butterfly / bolt / globe / antenna / box / bridge. The map is shared by
/// BOTH the conversations rail and the feed badge, so this one test guards both
/// surfaces against drift. Escape-sequence expectations so the asserted bytes (the
/// envelope's emoji-presentation selector in particular) are unambiguous.
/// </summary>
public class SourceGlyphAssetTests
{
    [Fact]
    public void Emoji_MapsEveryConcept()
    {
        Assert.Equal("\U0001F98A", SourceGlyphAsset.Emoji(SourceGlyph.Fox));         // 🦊 Fauna
        // ✉️ Email — U+2709 + U+FE0F (emoji-presentation selector → color on Windows).
        Assert.Equal("✉️", SourceGlyphAsset.Emoji(SourceGlyph.Envelope));
        Assert.Equal("\U0001F98B", SourceGlyphAsset.Emoji(SourceGlyph.Butterfly));   // 🦋 Bluesky
        Assert.Equal("⚡", SourceGlyphAsset.Emoji(SourceGlyph.Bolt));            // ⚡ Nostr
        Assert.Equal("\U0001F310", SourceGlyphAsset.Emoji(SourceGlyph.Globe));       // 🌐 Fediverse
        Assert.Equal("\U0001F4E1", SourceGlyphAsset.Emoji(SourceGlyph.Unknown));     // 📡 unknown
        Assert.Equal("\U0001F4E6", SourceGlyphAsset.Emoji(SourceGlyph.Archive));     // 📦 archive import
        Assert.Equal("\U0001F309", SourceGlyphAsset.Emoji(SourceGlyph.Bridge));      // 🌉 a bridge
    }
}
