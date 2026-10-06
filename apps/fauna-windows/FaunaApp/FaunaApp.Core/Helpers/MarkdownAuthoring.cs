using uniffi.fauna_ffi;

namespace FaunaApp.Core.Helpers;

/// <summary>The new text + selection a compose toolbar should apply after toggling an
/// inline style (bold/italic/code). The selection re-covers the wrapped core so the
/// user can keep toggling.</summary>
public record ToolbarEdit(string Text, int SelectionStart, int SelectionLength);

/// <summary>
/// Compose-toolbar authoring helpers (the write side of Markdown; the read side is
/// <see cref="DocumentRenderer"/>). The actual wrap rule lives once in shared Rust
/// (<c>fauna_core::markdown::wrap_selection</c>, reached via <c>FaunaFfiMethods.WrapMarkdownSelection</c>)
/// so every app wraps identically instead of each re-deriving it (priority #1/#2/#4);
/// this only does the platform-side splice into the <c>TextBox</c>'s string + selection.
/// </summary>
public static class MarkdownAuthoring
{
    /// <summary>Toggle an inline style on the selection <c>[selectionStart, +selectionLength)</c>
    /// of <paramref name="text"/> by wrapping it in <paramref name="prefix"/>/<paramref name="suffix"/>.
    /// The shared wrap keeps any leading/trailing whitespace OUTSIDE the markers, so a
    /// double-click word-selection's trailing space no longer produces <c>*italic *</c>
    /// (which collides with an adjacent <c>**bold**</c> into <c>*italic ***bold**</c> and
    /// isn't valid CommonMark). An empty/all-whitespace selection wraps
    /// <paramref name="placeholder"/>.
    ///
    /// Offsets are derived from the returned substrings' .NET (UTF-16) lengths, so the
    /// math matches <c>TextBox.SelectionStart/Length</c> units even for multi-byte cores.</summary>
    public static ToolbarEdit WrapSelection(
        string? text,
        int selectionStart,
        int selectionLength,
        string prefix,
        string suffix,
        string placeholder = "text")
    {
        text ??= "";
        var selected = text.Substring(selectionStart, selectionLength);
        var w = FaunaFfiMethods.WrapMarkdownSelection(selected, prefix, suffix, placeholder);
        var newText = text.Substring(0, selectionStart)
            + w.replacement
            + text.Substring(selectionStart + selectionLength);
        return new ToolbarEdit(newText, selectionStart + w.beforeCore.Length, w.core.Length);
    }
}
