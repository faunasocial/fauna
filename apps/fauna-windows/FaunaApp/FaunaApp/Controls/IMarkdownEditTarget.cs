namespace FaunaApp.Controls;

/// <summary>
/// The minimal editable-text surface the <see cref="MarkdownToolbar"/> drives when it
/// wraps the selection in markers (bold / italic / code / link) or prefixes a line
/// (heading / list). Implemented by the compose field so the toolbar is agnostic to
/// whether that field is a plain <c>TextBox</c> or a <see cref="MarkdownRichEditBox"/>
/// decoration surface (docs/goal/ui/conversations.md § Compose-field inline markdown
/// styling — the shared toolbar-wrap must keep working over either control).
///
/// All offsets are UTF-16 (C# string) indices — the unit
/// <c>FaunaApp.Core.Helpers.MarkdownAuthoring.WrapSelection</c> operates in.
/// </summary>
public interface IMarkdownEditTarget
{
    /// <summary>The full literal text of the field (markdown markers present).</summary>
    string Text { get; set; }

    /// <summary>Caret / selection anchor, UTF-16 offset.</summary>
    int SelectionStart { get; set; }

    /// <summary>Selected length in UTF-16 units (0 = collapsed caret).</summary>
    int SelectionLength { get; set; }

    /// <summary>Return keyboard focus to the field after a toolbar action.</summary>
    void FocusTextInput();
}
