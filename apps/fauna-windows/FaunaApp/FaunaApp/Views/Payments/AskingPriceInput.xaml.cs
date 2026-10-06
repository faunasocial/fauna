using Microsoft.UI.Xaml.Controls;

namespace FaunaApp.Views.Payments;

/// <summary>
/// The gated asking-price TextBox — see the .xaml header for why this is its
/// own removable-build-item control rather than a bare TextBox at either call
/// site. A dumb text holder: callers own parsing (empty-or-unparseable means
/// no machine price, matching tui's `profile/mod.rs`/`feed/mod.rs` reference
/// legs — never an error at this layer).
/// </summary>
/// <remarks>⚠ Nothing may name this type from markup — see the .xaml header.</remarks>
public sealed partial class AskingPriceInput : UserControl
{
    public AskingPriceInput()
    {
        InitializeComponent();
    }

    public string Text
    {
        get => Box.Text;
        set => Box.Text = value;
    }

    /// <summary>The inner box's own TextChanged — what lets a host forward each edit as it
    /// is made (the feed composer stages a sale's asking price on the draft as it is typed).</summary>
    public event TextChangedEventHandler TextChanged
    {
        add => Box.TextChanged += value;
        remove => Box.TextChanged -= value;
    }

    public object Header
    {
        get => Box.Header;
        set => Box.Header = value;
    }

    public string PlaceholderText
    {
        get => Box.PlaceholderText;
        set => Box.PlaceholderText = value;
    }

    public string AutomationId
    {
        set => Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(Box, value);
    }
}
