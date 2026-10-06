using FaunaApp.Core.Helpers;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Compose-toolbar authoring conformance for <see cref="MarkdownAuthoring.WrapSelection"/>.
/// Calls the REAL shared wrap (<c>fauna_core::markdown::wrap_selection</c> via
/// <c>FaunaFfiMethods.WrapMarkdownSelection</c> — the native dll loads in the test host,
/// memory <c>reference_windows_dotnet_test_loads_native_ffi</c>) plus the windows-side
/// splice, so this locks both the shared whitespace rule and the .NET offset math.
/// </summary>
public class MarkdownAuthoringTests
{
    [Fact]
    public void WrapSelection_TrailingSpace_StaysOutsideMarkers()
    {
        // Double-click "italic" selects "italic " (with the trailing space). The space
        // must end up OUTSIDE the markers: `*italic* `, not `*italic *`.
        var edit = MarkdownAuthoring.WrapSelection("italic ", 0, 7, "*", "*");

        Assert.Equal("*italic* ", edit.Text);
        Assert.Equal(1, edit.SelectionStart);   // just after the opening `*`
        Assert.Equal(6, edit.SelectionLength);  // "italic"
    }

    [Fact]
    public void WrapSelection_AdjacentWords_ItalicThenBold_NoMarkerCollision()
    {
        // The exact reported bug, end-to-end: in "italic bold", double-click "italic"
        // (selection picks up the trailing space) → italic, then double-click "bold" →
        // bold. Wrapping the raw selection produced `*italic ***bold**`; the shared wrap
        // yields the correct `*italic* **bold**`.
        const string text = "italic bold";

        var afterItalic = MarkdownAuthoring.WrapSelection(text, 0, 7, "*", "*");
        Assert.Equal("*italic* bold", afterItalic.Text);

        var boldStart = afterItalic.Text.IndexOf("bold", System.StringComparison.Ordinal);
        var afterBold = MarkdownAuthoring.WrapSelection(afterItalic.Text, boldStart, 4, "**", "**");
        Assert.Equal("*italic* **bold**", afterBold.Text);
    }

    [Fact]
    public void WrapSelection_EmptySelection_WrapsPlaceholder()
    {
        var edit = MarkdownAuthoring.WrapSelection("", 0, 0, "**", "**");

        Assert.Equal("**text**", edit.Text);
        Assert.Equal(2, edit.SelectionStart);   // just after `**`
        Assert.Equal(4, edit.SelectionLength);  // "text"
    }
}
