using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using uniffi.fauna_conversations;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Controls;

public sealed partial class RecipientPicker : UserControl
{
    public event EventHandler<string>? RawInputChanged;
    public event EventHandler<string>? AcceptRequested;
    public event EventHandler<int>? SuggestionPicked;

    public RecipientPicker() { InitializeComponent(); }

    private void OnInputChanged(object sender, TextChangedEventArgs e)
    {
        RawInputChanged?.Invoke(this, InputBox.Text);
    }

    private void OnInputKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == Windows.System.VirtualKey.Enter)
        {
            AcceptRequested?.Invoke(this, InputBox.Text);
            e.Handled = true;
        }
    }

    private void OnSuggestionPicked(object sender, SelectionChangedEventArgs e)
    {
        if (SuggestionsList.SelectedIndex >= 0)
        {
            SuggestionPicked?.Invoke(this, SuggestionsList.SelectedIndex);
            SuggestionsList.SelectedItem = null;
        }
    }

    /// <summary><paramref name="state"/> is the shared <c>RecipientResolveStatus</c>
    /// token (<c>FaunaConversationsMethods.RecipientResolveStatus</c>); <paramref
    /// name="text"/> is that view's already-resolved label (empty for idle).</summary>
    public void UpdateResolveState(string state, string text)
    {
        ResolveStatus.Tag = state;
        // Mirror onto AutomationProperties.HelpText so the FlaUI bridge can
        // read it via get_attr(id, "state"). UIA has no first-class "Tag"
        // surface; HelpText is the standard application-supplied free-text
        // channel and is what get_attr falls back to for non-"disabled"
        // attribute names.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(ResolveStatus, state);
        // Visible status label — the shared resolve-state copy the other five
        // apps render (conversations.unified.recipient_resolve_*). The Tag/
        // HelpText above stays the e2e state channel; this is the user-facing text.
        ResolveStatus.Text = text;
    }

    /// <summary>
    /// <c>recipient-picker-class</c>: the class of the room about to be created,
    /// painted once a recipient chip is committed
    /// (<c>conversation-rooms.md</c> § The three classes). Both arguments come
    /// from shared Rust — the class itself from
    /// <c>prospective_room_class</c> over the committed chips, never derived
    /// here — and <c>null</c> means "nothing to state yet", which is an ABSENT
    /// element rather than an empty one.
    /// </summary>
    public void SetProspectiveClass(string? classLabel, string? classToken)
    {
        if (classLabel is null)
        {
            ProspectiveClass.Visibility = Visibility.Collapsed;
            return;
        }
        ProspectiveClass.Text = classLabel;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            ProspectiveClass, classToken ?? string.Empty);
        ProspectiveClass.Visibility = Visibility.Visible;
    }

    /// <summary>Resolve <paramref name="state"/> through the shared
    /// <c>RecipientResolveStatus</c> state→(token, label) map (conversations.md §
    /// Errors & edge cases) and update the visible/e2e status — the single call both
    /// recipient pickers (conversations + folder share) render from. Internal:
    /// <see cref="ResolveState"/> is a UniFFI-internal enum, so this can't be a
    /// `public` overload (CS0051) — callers in this assembly use it directly, the
    /// `public` (token, text) overload above stays for a non-UniFFI-typed caller.</summary>
    internal void UpdateResolveState(ResolveState state)
    {
        var view = FaunaConversationsMethods.RecipientResolveStatus(state);
        UpdateResolveState(view.token, view.label is { } l ? S.Resolve(l) : "");
    }

    /// <summary>
    /// Replace the chips collection. Items must expose a public
    /// <c>Display</c> string property so the indexed
    /// <c>recipient-picker-chip</c> template can bind to it.
    /// </summary>
    public void SetChips<T>(System.Collections.Generic.IList<T> items)
    {
        ChipsList.ItemsSource = items;
    }
}
