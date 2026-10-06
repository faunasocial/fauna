using uniffi.fauna_core;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// Windows's single <c>SourceGlyph → native asset</c> map — the one genuinely
/// platform-specific half of source-icon rendering. Shared Rust owns both the
/// <em>concept</em> (<c>fauna_core::source_glyph::SourceGlyph</c>, resolved by
/// <c>Rail::glyph()</c> for the conversations rail and <c>SourceKind::glyph()</c>
/// for the feed badge) and, since the D5 lift, the <c>concept → emoji</c> map
/// itself (<c>fauna_core::source_glyph::SourceGlyph::emoji()</c>, exported over
/// UniFFI as <c>sourceGlyphEmoji</c>); windows keeps only this call, and it
/// serves BOTH surfaces (the conversations rail via
/// <c>ConversationsPage.ThreadRow.ProtocolGlyphFor</c> and the feed badge via
/// <c>FeedPostItem</c>), so a within-client rail-vs-badge split is impossible
/// (render-model.md § Deltas → D5).
/// </summary>
/// <remarks><c>internal</c> because its parameter is the UniFFI-generated
/// <c>SourceGlyph</c> enum (emitted <c>internal</c>); the FaunaApp WinUI app and
/// FaunaApp.Tests reach it via <c>[InternalsVisibleTo]</c>.</remarks>
internal static class SourceGlyphAsset
{
    /// <summary>The emoji a <see cref="SourceGlyph"/> concept renders as on windows.</summary>
    internal static string Emoji(SourceGlyph glyph) => FaunaFfiMethods.SourceGlyphEmoji(glyph);
}
