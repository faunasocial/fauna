using System.Collections.Generic;
using System.Text;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Markdown;

/// <summary>
/// The visual style of one <see cref="ComposeRun"/>. Content kinds carry a style; markers
/// are either <see cref="Conceal"/> (hidden — the hide-by-default mode), <see cref="DimMarker"/>
/// (shown dimmed), or <see cref="Plain"/> (shown un-styled so the raw markers can be edited) —
/// the Obsidian "source on the active line" / live-preview model.
/// </summary>
internal enum ComposeRunStyle
{
    Plain,
    Bold,
    Italic,
    BoldItalic,
    Code,
    Link,
    Heading,
    Blockquote,
    DimMarker,
    /// <summary>A concealed (zero-width, not rendered) marker run — the hide-by-default mode's
    /// inline emphasis markers, applied as <c>ITextCharacterFormat.Hidden</c>. The literal
    /// source text is unchanged (still in the buffer + ValuePattern), only its rendering.</summary>
    Conceal,
}

/// <summary>
/// One contiguous styled run over the compose buffer, in **UTF-16** (C# string / TextBox
/// selection) offsets. <see cref="HeadingLevel"/> is 1–4 when <see cref="Style"/> is
/// <see cref="ComposeRunStyle.Heading"/>, else 0. Runs returned by
/// <see cref="ComposeMarkdownDecorator.BuildRuns"/> tile the whole buffer gap-free.
/// </summary>
internal sealed record ComposeRun(int Start, int Length, ComposeRunStyle Style, int HeadingLevel);

/// <summary>
/// Inline markdown styling for the windows DM compose field (<c>dm-text-field</c>),
/// <c>docs/goal/ui/conversations.md</c> § Compose-field inline markdown styling. The buffer
/// keeps the **literal markdown source** (Obsidian/Typora live-preview, NOT WYSIWYG); this is
/// the decoration layer — it turns the shared <c>fauna_core::markdown::decoration_map</c> byte
/// ranges (reached via <c>FaunaFfiMethods.DecorationMap</c>) into flat, gap-filled
/// <see cref="ComposeRun"/>s the overlay renders. The same shared tokenizer feeds the render
/// path, so the compose preview and the sent message can never disagree.
///
/// Windows twin of linux <c>apps/fauna-linux/src/views/conversations/compose_decoration.rs</c>
/// and android <c>MarkdownCompose.kt::buildComposeDecoration</c> (priority #1/#3/#4 — one
/// shared definition per concept, no per-app re-derivation). Pure (no UI types), so it is
/// unit-tested against the real native FFI in <c>FaunaApp.Tests</c>.
/// </summary>
internal static class ComposeMarkdownDecorator
{
    /// <summary>UTF-8 byte length of a Unicode code point — the offset unit
    /// <c>decoration_map</c> returns.</summary>
    private static int Utf8Len(int codePoint) => codePoint switch
    {
        < 0x80 => 1,
        < 0x800 => 2,
        < 0x10000 => 3,
        _ => 4,
    };

    /// <summary>
    /// Convert a UTF-8 byte offset (<c>decoration_map</c>'s unit, over the raw Rust
    /// <c>str</c>) to a UTF-16 <c>string</c> index (the TextBox's offset unit). Decoration
    /// offsets always fall on a char boundary, so this lands exactly; clamps to the string
    /// length defensively.
    /// </summary>
    internal static int Utf8ByteToUtf16Index(string text, long byteOffset)
    {
        if (byteOffset <= 0) return 0;
        int bytes = 0;
        int i = 0;
        while (i < text.Length)
        {
            if (bytes >= byteOffset) break;
            int cp = char.ConvertToUtf32(text, i);
            bytes += Utf8Len(cp);
            i += char.IsSurrogatePair(text, i) ? 2 : 1;
        }
        return i;
    }

    /// <summary>
    /// Convert a UTF-16 <c>string</c> index (the RichEditBox caret unit) to a UTF-8 byte
    /// offset over the raw Rust <c>str</c> — the unit <c>compose_decoration_plan</c>'s
    /// <c>caret</c> argument expects. Inverse of <see cref="Utf8ByteToUtf16Index"/>; caret
    /// positions land on a char boundary, so this is exact; clamps defensively.
    /// </summary>
    internal static long Utf16IndexToUtf8Byte(string text, int utf16Index)
    {
        if (string.IsNullOrEmpty(text) || utf16Index <= 0) return 0;
        int clamp = utf16Index > text.Length ? text.Length : utf16Index;
        long bytes = 0;
        int i = 0;
        while (i < clamp)
        {
            int cp = char.ConvertToUtf32(text, i);
            bytes += Utf8Len(cp);
            i += char.IsSurrogatePair(text, i) ? 2 : 1;
        }
        return bytes;
    }

    /// <summary>The content style for a snake_case <c>decoration_map</c> kind, or <c>null</c>
    /// for the marker kinds (handled by the caret-reveal path). Mirrors the render path's
    /// styling (<c>DocumentRenderer</c>/<c>DocumentRenderTests</c>) so compose preview reads like
    /// the sent message — and android <c>MarkdownCompose.contentStyle</c>.</summary>
    private static ComposeRunStyle? ContentStyle(string kind) => kind switch
    {
        "bold" => ComposeRunStyle.Bold,
        "italic" => ComposeRunStyle.Italic,
        "bold_italic" => ComposeRunStyle.BoldItalic,
        "code" => ComposeRunStyle.Code,
        "link" or "image" => ComposeRunStyle.Link,
        "heading" => ComposeRunStyle.Heading,
        "blockquote" => ComposeRunStyle.Blockquote,
        _ => null, // marker / list_marker / unknown
    };

    /// <summary>
    /// Build the inline-styled runs for the compose field in **show-markers (dimmed live-preview)
    /// mode** from the raw markdown <paramref name="text"/>, the shared <c>decoration_map</c> ranges
    /// (<paramref name="decorations"/>, **byte** offsets), and the shared show-markers dim set
    /// (<paramref name="dimMarkerRanges"/>, **byte** ranges from
    /// <c>fauna_core::markdown::compose_show_markers_dim_ranges</c> via
    /// <c>FaunaFfiMethods.ComposeShowMarkersDimRanges</c>). Content ranges get a visual style; a
    /// marker range is dimmed (DimMarker) when it is in the shared dim set (off the caret's line),
    /// else shown un-dimmed (Plain) so the raw markers on the caret's line can be edited — the
    /// caret-line reveal rule now lives in shared Rust (priority #2/#4), no longer re-derived here.
    /// The returned runs are flat, contiguous, and gap-filled (plain text between decorations);
    /// adjacent same-style runs are coalesced. Decoration only styles — text content/length is
    /// unchanged — so the overlay needs no offset remapping (goal doc: "start with dim, not true-hide").
    ///
    /// <c>decoration_map</c>'s inline scanner is flat (non-nested, ascending, non-overlapping),
    /// so a single linear walk is exact; degenerate/overlapping ranges are skipped defensively.
    /// </summary>
    internal static IReadOnlyList<ComposeRun> BuildRuns(
        string text,
        IReadOnlyList<FfiMdDecoration> decorations,
        IReadOnlyList<FfiRevealSpan> dimMarkerRanges)
    {
        var runs = new List<ComposeRun>();
        if (string.IsNullOrEmpty(text)) return runs;

        // Marker byte-starts to DIM (the shared show-markers reveal set: markers off the caret's
        // line). A marker decoration NOT in this set sits on the caret's line → revealed (Plain).
        var dimStarts = new HashSet<ulong>();
        foreach (var r in dimMarkerRanges) dimStarts.Add(r.start);

        int cursor = 0; // UTF-16 index covered so far

        void Emit(int start, int length, ComposeRunStyle style, int headingLevel)
        {
            if (length <= 0) return;
            // Coalesce with the previous run when same style + contiguous.
            if (runs.Count > 0)
            {
                var last = runs[^1];
                if (last.Style == style && last.HeadingLevel == headingLevel &&
                    last.Start + last.Length == start)
                {
                    runs[^1] = last with { Length = last.Length + length };
                    return;
                }
            }
            runs.Add(new ComposeRun(start, length, style, headingLevel));
        }

        foreach (var d in decorations)
        {
            int s = Utf8ByteToUtf16Index(text, (long)d.start);
            int e = Utf8ByteToUtf16Index(text, (long)d.end);
            if (e <= s || s < cursor) continue; // skip degenerate / overlapping ranges

            if (s > cursor) Emit(cursor, s - cursor, ComposeRunStyle.Plain, 0); // plain gap

            var style = ContentStyle(d.kind);
            if (style is ComposeRunStyle content)
            {
                Emit(s, e - s, content, content == ComposeRunStyle.Heading ? d.level : 0);
            }
            else
            {
                // Marker: dimmed off the caret's line (in the shared dim set), else revealed (Plain).
                bool dim = dimStarts.Contains(d.start);
                Emit(s, e - s, dim ? ComposeRunStyle.DimMarker : ComposeRunStyle.Plain, 0);
            }
            cursor = e;
        }

        if (cursor < text.Length) Emit(cursor, text.Length - cursor, ComposeRunStyle.Plain, 0);
        return runs;
    }

    /// <summary>
    /// Build the inline-styled runs for the compose field in **hide-by-default** mode: content
    /// styling still comes from the shared <c>decoration_map</c> (<paramref name="decorations"/>),
    /// but marker visibility comes from the shared <c>compose_decoration_plan</c>
    /// (<paramref name="plan"/>) — inline emphasis markers in <c>plan.hide</c> become
    /// <see cref="ComposeRunStyle.Conceal"/> (hidden, zero-width) and those in <c>plan.dim</c>
    /// (the caret-edge reveal + structural prefixes off the caret line) become
    /// <see cref="ComposeRunStyle.DimMarker"/>. Markers in neither set are left plain (no run).
    /// Both lists are **byte** ranges over the raw source, converted to UTF-16 here.
    ///
    /// Unlike <see cref="BuildRuns"/> the runs do NOT tile the buffer — only the styled-content
    /// and concealed/dimmed marker runs are emitted (the applier resets to plain first, and a
    /// <see cref="ComposeRunStyle.Plain"/> run is a no-op). Windows twin of linux
    /// <c>compose_decoration.rs</c>'s hide branch (md-hidden / md-marker tags) +
    /// web <c>composeHideExtension</c> (conversations.md § Compose-field inline markdown styling;
    /// design tracked internally).
    /// </summary>
    internal static IReadOnlyList<ComposeRun> BuildHideRuns(
        string text,
        IReadOnlyList<FfiMdDecoration> decorations,
        FfiComposeMarkerPlan plan)
    {
        var runs = new List<ComposeRun>();
        if (string.IsNullOrEmpty(text)) return runs;

        // Content styling from decoration_map (markers skipped — the plan owns their visibility).
        foreach (var d in decorations)
        {
            if (ContentStyle(d.kind) is not ComposeRunStyle content) continue;
            int s = Utf8ByteToUtf16Index(text, (long)d.start);
            int e = Utf8ByteToUtf16Index(text, (long)d.end);
            if (e > s)
                runs.Add(new ComposeRun(s, e - s, content, content == ComposeRunStyle.Heading ? d.level : 0));
        }

        // Marker visibility from the shared plan: hide → conceal, dim → dimmed.
        void EmitMarkers(FfiRevealSpan[] spans, ComposeRunStyle style)
        {
            foreach (var r in spans)
            {
                int s = Utf8ByteToUtf16Index(text, (long)r.start);
                int e = Utf8ByteToUtf16Index(text, (long)r.end);
                if (e > s) runs.Add(new ComposeRun(s, e - s, style, 0));
            }
        }
        EmitMarkers(plan.hide, ComposeRunStyle.Conceal);
        EmitMarkers(plan.dim, ComposeRunStyle.DimMarker);
        return runs;
    }

    /// <summary>
    /// The text the user SEES — the raw source with every <see cref="ComposeRunStyle.Conceal"/>
    /// run spliced out (concealment is zero-width, so a hidden marker is not visible). In
    /// show-markers mode no run conceals, so this returns the source verbatim. This is what the
    /// e2e <c>compose_visible_text</c> reads (windows publishes it on the RichEditBox
    /// <c>AutomationProperties.HelpText</c>) — the windows twin of web's concealed-excluding
    /// <c>textContent</c> and linux's <c>include_hidden_chars=false</c> buffer read. The literal
    /// source (ValuePattern / <c>compose_body_text</c> / the sent bytes) is never touched.
    /// </summary>
    internal static string VisibleText(string text, IReadOnlyList<ComposeRun> runs)
    {
        if (string.IsNullOrEmpty(text)) return text ?? string.Empty;

        var hidden = new List<(int Start, int End)>();
        foreach (var r in runs)
        {
            if (r.Style != ComposeRunStyle.Conceal) continue;
            int start = r.Start < 0 ? 0 : (r.Start > text.Length ? text.Length : r.Start);
            int end = r.Start + r.Length;
            if (end > text.Length) end = text.Length;
            if (end > start) hidden.Add((start, end));
        }
        if (hidden.Count == 0) return text;

        hidden.Sort((a, b) => a.Start.CompareTo(b.Start));
        var sb = new StringBuilder(text.Length);
        int cursor = 0;
        foreach (var (start, end) in hidden)
        {
            if (start > cursor) sb.Append(text, cursor, start - cursor);
            if (end > cursor) cursor = end;
        }
        if (cursor < text.Length) sb.Append(text, cursor, text.Length - cursor);
        return sb.ToString();
    }
}
