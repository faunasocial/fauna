using System;
using System.Linq;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using uniffi.fauna_conversations;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Controls;

/// <summary>A suggestion row; exposes the public <c>Display</c> the template binds.</summary>
public sealed class SuggestionItem
{
    public SuggestionItem(string display, int index) { Display = display; Index = index; }
    public string Display { get; }
    public int Index { get; }
}

/// <summary>A committed chip: its address and the × that removes it
/// (<c>recipient-picker-chip[i]</c>); <c>Index</c> is what the × removes.</summary>
public sealed class ChipRow
{
    public ChipRow(string address, int index) { Display = $"{address} ×"; Index = index; }
    public string Display { get; }
    public int Index { get; }
}

public sealed partial class RecipientPicker : UserControl
{
    public event EventHandler<string>? RawInputChanged;
    public event EventHandler<string>? AcceptRequested;
    public event EventHandler<int>? SuggestionPicked;
    /// <summary>A chip's × was pressed — the chip's index, for the shared
    /// <c>remove_recipient_chip(i)</c> (conversations.md § User actions).</summary>
    public event EventHandler<int>? ChipRemoved;

    public RecipientPicker() { InitializeComponent(); }

    private bool _paintingInput;

    private void OnInputChanged(object sender, TextChangedEventArgs e)
    {
        if (_paintingInput) return;
        RawInputChanged?.Invoke(this, InputBox.Text);
    }

    /// <summary>
    /// Paint the shared picker state's <c>raw_input</c> into the box — the path a
    /// RESTORED draft's typed address reaches the screen (conversation-drafts.md §
    /// Persistence). It is a repaint, not a keystroke: it raises no
    /// <see cref="RawInputChanged"/> (the restore already issued the probe), and an
    /// unchanged text is left alone so a snapshot tick never moves the caret.
    /// </summary>
    public void SetInputText(string text)
    {
        if (InputBox.Text == text) return;
        _paintingInput = true;
        try { InputBox.Text = text; }
        finally { _paintingInput = false; }
    }

    private void OnInputKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == Windows.System.VirtualKey.Enter)
        {
            AcceptRequested?.Invoke(this, InputBox.Text);
            e.Handled = true;
        }
    }

    private void OnSuggestionClicked(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: int index })
            SuggestionPicked?.Invoke(this, index);
    }

    private void OnChipClicked(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: int index })
            ChipRemoved?.Invoke(this, index);
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

    /// <summary>The line under the input naming the bridges that serve the account
    /// (<c>ConversationsSnapshot.bridges</c> labels, as declared). The nest matches an
    /// address to its bridge — the picker only lists (<c>conversations.md</c> § Where
    /// logic lives → <i>The <c>Bridged</c> adapter</i>, ruling 2 (d)). Empty collapses it.</summary>
    public void SetBridgeLabels(System.Collections.Generic.IReadOnlyList<string> labels)
    {
        if (labels.Count == 0)
        {
            BridgesLine.Visibility = Visibility.Collapsed;
            return;
        }
        BridgesLine.Text = S.Get("conversations/unified/recipient_picker_bridges")
            .Replace("{bridges}", string.Join(", ", labels));
        BridgesLine.Visibility = Visibility.Visible;
    }

    /// <summary>The home-nest toggle was pressed — the flipped choice (what the
    /// composer should now stage through <c>set_new_thread_home_nest</c>).</summary>
    public event EventHandler<bool>? HomeNestToggled;

    /// <summary>
    /// <c>recipient-picker-home-nest-toggle</c>: paint the composer's home-nest
    /// choice, read off the picker state's shared <c>include_home_nest</c>.
    /// <c>null</c> collapses the control (absent): every picker but the
    /// new-thread composer's.
    /// </summary>
    public void SetHomeNest(bool? include)
    {
        if (include is not { } on)
        {
            HomeNestToggle.Visibility = Visibility.Collapsed;
            return;
        }
        HomeNestToggle.Content = S.Get(on
            ? "conversations/unified/room_home_nest_yes"
            : "conversations/unified/room_home_nest_no");
        // Programmatic IsChecked writes raise no Click, so this cannot loop.
        HomeNestToggle.IsChecked = on;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            HomeNestToggle, on ? "true" : "false");
        HomeNestToggle.Visibility = Visibility.Visible;
    }

    private void OnHomeNestClicked(object sender, RoutedEventArgs e)
    {
        // The toggle flipped itself on the click; hand the page the NEW value,
        // which the snapshot tick then confirms back through SetHomeNest.
        HomeNestToggled?.Invoke(this, HomeNestToggle.IsChecked == true);
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
    /// <c>recipient-picker-chip[i]</c>: paint the committed chips, each reading
    /// <c>"{address} ×"</c> — a chip activates to its ×, the one gesture a committed
    /// chip has (tui's and linux's chip). <paramref name="addresses"/> are the chips'
    /// display spellings, in order.
    /// </summary>
    public void SetChips(System.Collections.Generic.IReadOnlyList<string> addresses)
    {
        ChipsList.ItemsSource = addresses.Select((a, i) => new ChipRow(a, i)).ToList();
    }

    private string _suggestionsPainted = "";

    /// <summary>
    /// <c>recipient-picker-suggestion[i]</c>: paint each suggestion as the finished
    /// label the manager derived (<c>suggestion_displays[i]</c>), with the address
    /// beside it when the two differ (<c>conversations.md</c> § Where logic lives →
    /// <i>Recipient suggestions</i>) — the shape tui, linux and android paint.
    /// <paramref name="addresses"/> are the suggestions' display spellings,
    /// index-parallel with <paramref name="labels"/>. Empty collapses the list. An
    /// unchanged list is left alone so a snapshot tick neither rebuilds the rows nor
    /// re-raises selection.
    /// </summary>
    public void SetSuggestions(
        System.Collections.Generic.IReadOnlyList<string> addresses,
        System.Collections.Generic.IReadOnlyList<string> labels)
    {
        var texts = addresses.Select((address, i) =>
        {
            var label = i < labels.Count ? labels[i] : "";
            return !string.IsNullOrEmpty(label) && label != address
                ? $"{label} · {address}"
                : address;
        }).ToList();
        var key = string.Join("\u001f", texts);
        if (key == _suggestionsPainted) return;
        _suggestionsPainted = key;
        SuggestionsList.ItemsSource = texts
            .Select((t, i) => new SuggestionItem(t, i)).ToList();
        SuggestionsList.Visibility = texts.Count == 0
            ? Visibility.Collapsed
            : Visibility.Visible;
    }
}
