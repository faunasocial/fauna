using System.Collections.Generic;
using System.Linq;
using FaunaApp.Core.Markdown;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Conformance for the windows compose inline-markdown applier
/// (<c>docs/goal/ui/conversations.md</c> § Compose-field inline markdown styling). The pure
/// decorator turns the shared <c>fauna_core::markdown::decoration_map</c> byte ranges (reached
/// via the REAL UniFFI <c>FaunaFfiMethods.DecorationMap</c> — the native <c>fauna_ffi</c> dll
/// loads in the test host, memory <c>reference_windows_dotnet_test_loads_native_ffi</c>) into
/// flat, gap-filled <c>ComposeRun</c>s the overlay renders: content kinds styled, markers
/// dimmed off the caret's line + revealed on it, byte→UTF-16 offset conversion (multi-byte
/// safe). Windows twin of linux <c>compose_decoration.rs</c> + android <c>MarkdownCompose.kt</c>
/// (priority #1/#3/#4) — one shared definition, no per-app re-derivation.
/// </summary>
public class ComposeMarkdownDecoratorTests
{
    private static ComposeRun RunAt(IReadOnlyList<ComposeRun> runs, int offset) =>
        runs.Single(r => offset >= r.Start && offset < r.Start + r.Length);

    /// <summary>The shared show-markers dim set (marker byte-ranges off the caret's line) from the
    /// REAL UniFFI <c>FaunaFfiMethods.ComposeShowMarkersDimRanges</c>, for a caret at UTF-16
    /// <paramref name="caretUtf16"/> — the byte→UTF-16 conversion mirrors the applier
    /// (<c>DmComposeBar.Decoration.cs</c>). So <c>BuildRuns</c>'s marker dim/reveal is now driven by
    /// shared Rust, not a C# re-derivation.</summary>
    private static IReadOnlyList<FfiRevealSpan> ShowDim(string text, int caretUtf16) =>
        FaunaFfiMethods.ComposeShowMarkersDimRanges(
            text, (ulong)ComposeMarkdownDecorator.Utf16IndexToUtf8Byte(text, caretUtf16));

    // ── byte (UTF-8, decoration_map's unit) → UTF-16 (C# string) index ──

    [Fact]
    public void Utf8ByteToUtf16Index_Ascii_IsIdentity()
    {
        Assert.Equal(0, ComposeMarkdownDecorator.Utf8ByteToUtf16Index("hello", 0));
        Assert.Equal(5, ComposeMarkdownDecorator.Utf8ByteToUtf16Index("hello", 5));
    }

    [Fact]
    public void Utf8ByteToUtf16Index_TwoByteAccent_LandsPastIt()
    {
        // "café x": c,a,f = 3 bytes, é (U+00E9) = 2 bytes; byte 5 is just past é = UTF-16 index 4.
        Assert.Equal(4, ComposeMarkdownDecorator.Utf8ByteToUtf16Index("café x", 5));
        Assert.Equal(0, ComposeMarkdownDecorator.Utf8ByteToUtf16Index("café x", 0));
    }

    [Fact]
    public void Utf8ByteToUtf16Index_FourByteEmoji_CrossesSurrogatePair()
    {
        // 👋 U+1F44B = 4 UTF-8 bytes, 2 UTF-16 code units; byte 4 is just past it = UTF-16 index 2.
        Assert.Equal(2, ComposeMarkdownDecorator.Utf8ByteToUtf16Index("👋x", 4));
    }

    // ── content styling over the real shared decoration_map ──

    [Fact]
    public void BuildRuns_Italic_StylesContent_RevealsMarkersOnCaretLine()
    {
        const string text = "hello *world*";
        var runs = ComposeMarkdownDecorator.BuildRuns(text, FaunaFfiMethods.DecorationMap(text), ShowDim(text, 0));
        Assert.Equal(ComposeRunStyle.Italic, RunAt(runs, 8).Style); // inside "world"
        Assert.Equal(ComposeRunStyle.Plain, RunAt(runs, 6).Style);  // '*' revealed (caret on its line)
    }

    [Fact]
    public void BuildRuns_Bold_Code_BoldItalic()
    {
        const string b = "**b**";
        Assert.Equal(ComposeRunStyle.Bold,
            RunAt(ComposeMarkdownDecorator.BuildRuns(b, FaunaFfiMethods.DecorationMap(b), ShowDim(b, 0)), 2).Style);
        const string c = "`c`";
        Assert.Equal(ComposeRunStyle.Code,
            RunAt(ComposeMarkdownDecorator.BuildRuns(c, FaunaFfiMethods.DecorationMap(c), ShowDim(c, 0)), 1).Style);
        const string x = "***x***";
        Assert.Equal(ComposeRunStyle.BoldItalic,
            RunAt(ComposeMarkdownDecorator.BuildRuns(x, FaunaFfiMethods.DecorationMap(x), ShowDim(x, 0)), 3).Style);
    }

    [Fact]
    public void BuildRuns_Heading_StylesContent_CarriesLevel()
    {
        const string text = "# Hi";
        var h = RunAt(ComposeMarkdownDecorator.BuildRuns(text, FaunaFfiMethods.DecorationMap(text), ShowDim(text, 0)), 2);
        Assert.Equal(ComposeRunStyle.Heading, h.Style);
        Assert.Equal(1, h.HeadingLevel);
    }

    [Fact]
    public void BuildRuns_DimsMarkersOffCaretLine_RevealsOnCaretLine()
    {
        // Real-FFI conformance: the marker dim/reveal is driven by the shared
        // fauna_core::markdown::compose_show_markers_dim_ranges (via ShowDim), not a C# LineOf.
        const string text = "a\n*b*"; // the two '*' markers sit on line 1 (UTF-16 offsets 2 and 4)
        var decos = FaunaFfiMethods.DecorationMap(text);

        var caretLine0 = ComposeMarkdownDecorator.BuildRuns(text, decos, ShowDim(text, 0));
        Assert.Equal(ComposeRunStyle.DimMarker, RunAt(caretLine0, 2).Style); // off-line '*' dimmed
        Assert.Equal(ComposeRunStyle.Italic, RunAt(caretLine0, 3).Style);    // "b" content always styled

        var caretLine1 = ComposeMarkdownDecorator.BuildRuns(text, decos, ShowDim(text, 3));
        Assert.Equal(ComposeRunStyle.Plain, RunAt(caretLine1, 2).Style);     // on-line '*' revealed
    }

    [Fact]
    public void BuildRuns_PlainText_CoversWholeStringContiguously()
    {
        const string text = "just text";
        var runs = ComposeMarkdownDecorator.BuildRuns(text, FaunaFfiMethods.DecorationMap(text), ShowDim(text, 0));
        Assert.All(runs, r => Assert.Equal(ComposeRunStyle.Plain, r.Style));
        Assert.Equal(0, runs[0].Start);
        Assert.Equal(text.Length, runs.Sum(r => r.Length)); // full, gap-free coverage
    }

    [Fact]
    public void BuildRuns_Empty_NoRuns()
    {
        Assert.Empty(ComposeMarkdownDecorator.BuildRuns("", FaunaFfiMethods.DecorationMap(""), ShowDim("", 0)));
    }

    // ── hide-by-default mode: markers concealed off the caret, revealed (dimmed) under it ──
    // The shared plan (fauna_core::markdown::compose_decoration_plan over the REAL UniFFI
    // FaunaFfiMethods.ComposeDecorationPlan) decides marker visibility; content styling still
    // comes from decoration_map. Windows twin of linux compose_decoration.rs's hide branch
    // (md-hidden vs md-marker) + web composeHideExtension (conversations.md § Compose-field
    // inline markdown styling; design tracked internally).

    [Fact]
    public void BuildHideRuns_ConcealsInlineMarkersAwayFromCaret()
    {
        // Caret in " rest" (byte 13) → both `**` runs are inline emphasis away from the caret
        // → CONCEAL; the "bold" content is still styled. Mirrors the Rust core test
        // compose_decoration_plan_hides_inline_markers_away_from_caret.
        const string text = "**bold** rest";
        var decos = FaunaFfiMethods.DecorationMap(text);
        var plan = FaunaFfiMethods.ComposeDecorationPlan(text, 13UL);
        var runs = ComposeMarkdownDecorator.BuildHideRuns(text, decos, plan);
        Assert.Equal(ComposeRunStyle.Conceal, RunAt(runs, 0).Style);  // leading `**` concealed
        Assert.Equal(ComposeRunStyle.Conceal, RunAt(runs, 6).Style);  // trailing `**` concealed
        Assert.Equal(ComposeRunStyle.Bold, RunAt(runs, 3).Style);     // content still styled
    }

    [Fact]
    public void BuildHideRuns_RevealsMarkerUnderCaretAsDim()
    {
        // Caret inside the bold run (byte 4) → its markers move to DIM (shown dimmed + editable),
        // never concealed. Mirrors compose_decoration_plan_reveals_dimmed_marker_under_caret.
        const string text = "**bold** rest";
        var decos = FaunaFfiMethods.DecorationMap(text);
        var plan = FaunaFfiMethods.ComposeDecorationPlan(text, 4UL);
        var runs = ComposeMarkdownDecorator.BuildHideRuns(text, decos, plan);
        Assert.Equal(ComposeRunStyle.DimMarker, RunAt(runs, 0).Style);     // `**` revealed (dimmed)
        Assert.DoesNotContain(runs, r => r.Style == ComposeRunStyle.Conceal);
    }

    [Fact]
    public void VisibleText_RemovesConcealedMarkers()
    {
        // The text the user SEES in hide mode = source minus the concealed `**` runs. This is what
        // the e2e compose_visible_text reads (windows exposes it via the RichEditBox HelpText).
        const string text = "**bold** rest";
        var plan = FaunaFfiMethods.ComposeDecorationPlan(text, 13UL);
        var runs = ComposeMarkdownDecorator.BuildHideRuns(text, FaunaFfiMethods.DecorationMap(text), plan);
        Assert.Equal("bold rest", ComposeMarkdownDecorator.VisibleText(text, runs));
    }

    [Fact]
    public void VisibleText_NoConcealment_ReturnsSourceVerbatim()
    {
        // Caret in the run → markers dim (not conceal), so nothing is hidden and the visible text
        // equals the full source — the toggle-shown / caret-on-run case the acceptance test asserts
        // exactly (vis_shown == "**bold** trailing").
        const string text = "**bold** rest";
        var plan = FaunaFfiMethods.ComposeDecorationPlan(text, 4UL);
        var runs = ComposeMarkdownDecorator.BuildHideRuns(text, FaunaFfiMethods.DecorationMap(text), plan);
        Assert.Equal(text, ComposeMarkdownDecorator.VisibleText(text, runs));
    }
}
