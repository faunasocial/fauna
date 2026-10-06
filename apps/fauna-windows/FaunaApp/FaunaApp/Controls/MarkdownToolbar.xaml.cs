using System;
using FaunaApp.Core.Helpers;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FaunaApp.Controls;

public sealed partial class MarkdownToolbar : UserControl
{
    /// <summary>The compose field this toolbar edits. Set by the host
    /// (<see cref="DmComposeBar"/>) in code; the seam lets the toolbar drive either a
    /// plain TextBox or a <see cref="MarkdownRichEditBox"/> decoration surface without
    /// caring which (conversations.md § Compose-field inline markdown styling — the
    /// shared toolbar-wrap is additive and unchanged by the styling rewrite).</summary>
    public IMarkdownEditTarget? Target { get; set; }

    /// <summary>Raised when the markdown-marker-toggle-button flips this editor's inline-marker
    /// visibility. <c>true</c> = show markers (dimmed), <c>false</c> = hide (default). The host
    /// (<see cref="DmComposeBar"/>) wires it to <see cref="DmComposeBar.SetMarkersShown"/> — the
    /// decoration applier owns the per-editor state, so the toggle is a pure signal (no state
    /// here; conversations.md § Compose-field inline markdown styling — per-editor, no persistence).</summary>
    public event Action<bool>? MarkersShownChanged;

    public MarkdownToolbar()
    {
        this.InitializeComponent();
    }

    /// <summary>
    /// Set the toolbar's enabled state. Flips <c>IsEnabled</c> on both
    /// the host UserControl (visual propagation) AND the inner
    /// ContentControl (which carries the <c>markdown-toolbar</c>
    /// AutomationId — UIA reads its IsEnabled directly). Without
    /// flipping the inner ControlControl, FlaUI's
    /// <c>get_attr(id, "disabled")</c> reads <c>IsEnabled = true</c>
    /// regardless of parent state.
    /// </summary>
    public void SetToolbarEnabled(bool enabled)
    {
        IsEnabled = enabled;
        ToolbarHost.IsEnabled = enabled;
    }

    private void WrapSelection(string prefix, string suffix)
    {
        if (Target is null) return;
        // Shared wrap (fauna_core::markdown::wrap_selection over UniFFI) keeps edge
        // whitespace outside the markers — see MarkdownAuthoring.WrapSelection.
        var edit = MarkdownAuthoring.WrapSelection(
            Target.Text,
            Target.SelectionStart,
            Target.SelectionLength,
            prefix,
            suffix);
        Target.Text = edit.Text;
        Target.SelectionStart = edit.SelectionStart;
        Target.SelectionLength = edit.SelectionLength;
        Target.FocusTextInput();
    }

    private void PrefixLine(string prefix)
    {
        if (Target is null) return;
        var text = Target.Text ?? "";
        if (text.Length == 0)
        {
            Target.Text = prefix;
            Target.SelectionStart = prefix.Length;
            Target.FocusTextInput();
            return;
        }
        var pos = Target.SelectionStart;
        // Find the start of the current line
        var lineStart = pos > 0 ? text.LastIndexOf('\n', pos - 1) + 1 : 0;
        var newText = text.Substring(0, lineStart) + prefix + text.Substring(lineStart);
        Target.Text = newText;
        Target.SelectionStart = pos + prefix.Length;
        Target.FocusTextInput();
    }

    private void Bold_Click(object sender, RoutedEventArgs e) => WrapSelection("**", "**");
    // Italic uses `*` (not `_`) to stay uniform with linux/web/android and with this
    // toolbar's own asterisk-family bold (`**`); the shared renderer accepts both.
    private void Italic_Click(object sender, RoutedEventArgs e) => WrapSelection("*", "*");
    private void Code_Click(object sender, RoutedEventArgs e) => WrapSelection("`", "`");
    private void Link_Click(object sender, RoutedEventArgs e) => WrapSelection("[", "](url)");
    private void Heading_Click(object sender, RoutedEventArgs e) => PrefixLine("## ");
    private void List_Click(object sender, RoutedEventArgs e) => PrefixLine("- ");

    // markdown-marker-toggle-button: signal the host to show / hide this editor's inline markers.
    private void MarkerToggle_Checked(object sender, RoutedEventArgs e) => MarkersShownChanged?.Invoke(true);
    private void MarkerToggle_Unchecked(object sender, RoutedEventArgs e) => MarkersShownChanged?.Invoke(false);
}
