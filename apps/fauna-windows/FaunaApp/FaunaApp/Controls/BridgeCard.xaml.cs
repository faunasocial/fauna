using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.System;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Controls;

/// <summary>
/// One bridge's card — the shared <c>bridge-card</c> component, embeddable by any
/// page. Extracted 2026-07-31 out of the inline <c>DataTemplate</c> in
/// <c>BridgesPage.xaml</c> so the AT Protocol page's Linked-account panel can render the
/// identical widget tree with zero new element IDs (<c>docs/goal/ui/atproto.md</c>
/// § Layout &amp; flow item 3, § Element IDs).
///
/// <para>Every sibling app made the same extraction before windows and the concept is
/// deliberately identical on all seven (priority #1/#3): linux
/// <c>build_bridge_detail_content</c>, apple <c>BridgeCardContent</c>, web
/// <c>BridgeCard.svelte</c>, android's <c>internal</c> <c>BridgeCard</c>, tui
/// <c>embed_bridge_card</c>.</para>
///
/// <para><b>The card decides nothing.</b> It renders a <see cref="BridgeInfo"/> and
/// raises <see cref="ActionClick"/>; link/unlink policy, the dialog and the nest calls
/// stay with the host page, exactly as before the extraction.</para>
///
/// <para>⚠ <b><see cref="ActionButton"/> gates on <see cref="BridgeInfo.Linked"/>, never
/// on declared link modes.</b> It is unconditional here — only its LABEL flips
/// Link/Unlink. web's extraction surfaced this as a real gap in its own Bridges page:
/// hiding the button when a provider declares no link modes makes the AT Protocol page's
/// synthetic pre-fetch card vanish rather than render an honest surface
/// (<c>ui/atproto.md</c>:42). windows was already correct; keep it so.</para>
///
/// <para>Text, name and label are assigned in <see cref="OnBridgeChanged"/> rather than
/// by <c>x:Bind</c>: a UserControl inside a <c>DataTemplate</c> binds against ITSELF, not
/// the item, and the explicit <c>AutomationProperties.Name</c> write is what keeps FlaUI
/// counting the rows (without a Name a DataTemplate row reports count/is_visible 0 —
/// memory <c>reference_winui_flaui_datatemplate_name</c>).</para>
/// </summary>
public sealed partial class BridgeCard : UserControl
{
    public BridgeCard()
    {
        this.InitializeComponent();
        // The ward's asks move off-card (a status read lands, a link/follow is
        // refused on the page's view model): repaint just the ask rows. The static
        // event is app-lifetime, so it is held only while the card is in the tree.
        Loaded += (_, _) =>
        {
            WardAsksCache.Changed += OnWardAsksChanged;
            RenderSourceAsks(Bridge);
        };
        Unloaded += (_, _) => WardAsksCache.Changed -= OnWardAsksChanged;
    }

    private void OnWardAsksChanged() => DispatcherQueue.TryEnqueue(() => RenderSourceAsks(Bridge));

    /// <summary>The ward pressed <c>bridge-source-request-button</c> for ONE refused
    /// triple. Carries that row's own triple and the bridge's display name (display-only
    /// label — the one still available once the add-follow buffers are cleared); the host
    /// page dispatches it over <c>BridgesViewModel.AskSourceAsync</c>.</summary>
    public event EventHandler<SourceAskEventArgs>? SourceAskClick;

    /// <summary>The bridge this card renders. A host embedding a single card for a
    /// named provider (the AT Protocol page) may pass a SYNTHETIC unlinked
    /// <see cref="BridgeInfo"/> when the provider has no row in
    /// <c>fauna.bridges.list</c> — the same honest-empty-surface case tui's
    /// <c>embed_bridge_card</c> and web's pre-fetch panel render.</summary>
    public BridgeInfo? Bridge
    {
        get => (BridgeInfo?)GetValue(BridgeProperty);
        set => SetValue(BridgeProperty, value);
    }

    public static readonly DependencyProperty BridgeProperty = DependencyProperty.Register(
        nameof(Bridge), typeof(BridgeInfo), typeof(BridgeCard),
        new PropertyMetadata(null, OnBridgeChanged));

    /// <summary>The card's action button was pressed. Carries the rendered
    /// <see cref="BridgeInfo"/> so the host needs no lookup — which is what lets a host
    /// act on a synthetic row that is in no collection.</summary>
    public event EventHandler<BridgeInfo>? ActionClick;

    /// <summary>A metadata-driven settings row committed a new value (bridges.md
    /// § Bridge settings). The card decides nothing here
    /// either — the host page dispatches <see cref="BridgeSettingChangedEventArgs.Value"/>
    /// over <c>INestRpcClient.BridgesSetSettingsAsync</c> and re-reads.</summary>
    public event EventHandler<BridgeSettingChangedEventArgs>? SettingChanged;

    private static void OnBridgeChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
        => ((BridgeCard)d).Render(e.NewValue as BridgeInfo);

    private void Render(BridgeInfo? bridge)
    {
        if (bridge is null)
        {
            // Nothing to render yet (the host has not assigned a bridge). Collapse
            // rather than paint a nameless card — an unnamed row is invisible to
            // FlaUI anyway, so a half-painted one would only be misleading.
            Root.Visibility = Visibility.Collapsed;
            return;
        }

        Root.Visibility = Visibility.Visible;
        AutomationProperties.SetName(Root, bridge.DisplayName);
        NameText.Text = bridge.DisplayName;
        IdentityRun.Text = bridge.Identity ?? string.Empty;
        ModeRun.Text = bridge.Mode ?? string.Empty;
        RenderSettings(bridge);
        RenderSourceAsks(bridge);
        ActionText.Text = bridge.Linked
            ? S.Get("bridges/unlink_action")
            : S.Get("bridges/link_action");

        // Why linking is unavailable, from the SHARED Rust rule (null = it is
        // available), so windows cannot drift from the other six apps. The card is
        // where this belongs: windows already rendered the identical sentence, but
        // only inside the link ContentDialog — which a user cannot open once the
        // button is disabled, so the nest's explanation was effectively unreachable
        // and `bridge-link-blocked-reason` never existed on the card at all.
        var block = uniffi.fauna_ffi.FaunaFfiMethods.BridgeLinkBlock(
            bridge.Linked, bridge.Error, (uint)BridgeLinkForm.ModesForPlatform(bridge).Count);
        if (block is not null)
        {
            LinkBlockedReason.Text = block switch
            {
                uniffi.fauna_ffi.FfiLinkBlock.ProviderError pe => pe.@message,
                _ => S.Get("bridges/no_link_method"),
            };
            LinkBlockedReason.Visibility = Visibility.Visible;
        }
        else
        {
            LinkBlockedReason.Text = string.Empty;
            LinkBlockedReason.Visibility = Visibility.Collapsed;
        }

        // DISABLED, never hidden. The ⚠ note above forbids *hiding* the button when
        // a provider declares no link modes (it would make the AT Protocol page's
        // synthetic pre-fetch card vanish instead of rendering an honest surface);
        // it does not forbid disabling it, and "stays rendered but DISABLED" is the
        // ratified shape — a live-looking control that silently swallows the tap is
        // the exact bug several sibling apps shipped first.
        ActionButton.IsEnabled = block is null;
    }

    private void ActionButton_Click(object sender, RoutedEventArgs e)
    {
        if (Bridge is { } bridge) ActionClick?.Invoke(this, bridge);
    }

    // ── The ward's feed-source ask (family-safety.md § Feed-source approvals → App
    //    affordance; tui's bridges.rs `source_ask_rows`, android's `sourceAskRows`).
    //    Durable rows first, then session-only refusals; a refused triple with a
    //    durable row is skipped by the shared fold, so an answered ask shows its
    //    verdict instead of offering the button again. An APPROVED row is a LABEL (the
    //    "try again" prompt), never a button — the ward redeems the single-use grant by
    //    repeating the ORIGINAL link / follow (rule (e)).

    private void RenderSourceAsks(BridgeInfo? bridge)
    {
        SourceAskPanel.Children.Clear();
        if (bridge is null)
        {
            SourceAskPanel.Visibility = Visibility.Collapsed;
            return;
        }

        var rows = WardAskFolds.SourceAskRowsFor(
            WardAsksCache.FeedRequests, WardAsksCache.RefusedFeed, bridge.Id);
        foreach (var state in rows.States)
        {
            // AutomationId ONLY, no static Name: a get_text target — a hardcoded Name
            // would shadow the label text (reference_windows_flaui_gettext_dynamic_label_no_name).
            var label = new TextBlock
            {
                Text = S.Get(state == uniffi.fauna_ffi.FfiFeedRequestState.Approved
                    ? "bridges/source_request_approved"
                    : "bridges/source_request_pending"),
                Opacity = 0.8,
                TextWrapping = TextWrapping.Wrap,
            };
            AutomationProperties.SetAutomationId(label, FaunaApp.UiIds.Ids.BridgeSourceRequestState);
            SourceAskPanel.Children.Add(label);
        }
        foreach (var triple in rows.Asks)
        {
            // Each button captures ITS OWN triple (rule (d)/(f)): keyed on the ask data,
            // never on the add-follow form's buffers, which dispatch clears.
            var button = new Button
            {
                Content = S.Get("bridges/source_request_button"),
                HorizontalAlignment = HorizontalAlignment.Left,
            };
            AutomationProperties.SetAutomationId(button, FaunaApp.UiIds.Ids.BridgeSourceRequestButton);
            var asked = triple;
            button.Click += (_, _) => SourceAskClick?.Invoke(
                this, new SourceAskEventArgs(asked, bridge.DisplayName));
            SourceAskPanel.Children.Add(button);
        }
        SourceAskPanel.Visibility = rows.IsEmpty ? Visibility.Collapsed : Visibility.Visible;
    }

    // ── Metadata-driven settings (bridges.md § Bridge settings) — built imperatively, one row per BridgeSetting, mirroring
    //    linux's build_settings_group / tui's setting_elements / web's
    //    BridgeCard.svelte / android's SettingRow arm-for-arm (priority #1).

    private void RenderSettings(BridgeInfo bridge)
    {
        SettingsPanel.Children.Clear();
        foreach (var setting in bridge.Settings)
        {
            SettingsPanel.Children.Add(BuildSettingRow(bridge.Id, setting));
        }
    }

    private FrameworkElement BuildSettingRow(string bridgeId, BridgeSetting setting)
    {
        // Hedges the same three spellings linux's build_settings_group does —
        // providers do not agree on one string (activitypub emits "boolean",
        // the shared default emits "bool").
        switch (setting.SettingType)
        {
            case "bool":
            case "boolean":
            case "toggle":
                return BuildToggleRow(bridgeId, setting);
            case "number":
                return BuildTextRow(bridgeId, setting, isNumber: true);
            case "text":
                return BuildTextRow(bridgeId, setting, isNumber: false);
            default:
                // Forward-compat fallback: a setting_type this build predates
                // (a newer nest) — a read-only row, never a dropped setting.
                // Mirrors tui's Element::chrome fallback / web's/android's
                // read-only "{label}: {value}" line.
                var value = setting.BoolValue?.ToString()
                    ?? setting.NumberValue?.ToString()
                    ?? setting.TextValue
                    ?? string.Empty;
                return new TextBlock { Text = $"{setting.Label}: {value}", Opacity = 0.6 };
        }
    }

    private ToggleSwitch BuildToggleRow(string bridgeId, BridgeSetting setting)
    {
        var toggle = new ToggleSwitch
        {
            Header = setting.Label,
            OnContent = string.Empty,
            OffContent = string.Empty,
            IsOn = setting.BoolValue ?? false,
        };
        toggle.Toggled += (_, _) => SettingChanged?.Invoke(this,
            new BridgeSettingChangedEventArgs(bridgeId, BridgeSettingValue.Bool(setting.Key, toggle.IsOn)));
        return toggle;
    }

    /// <summary>A text/number setting row. Commits on BOTH blur and Enter (the
    /// mail-spam-threshold-override-input / FoldersPage capInput idiom) — Enter
    /// alone doesn't move focus off a single-line TextBox, so the e2e's
    /// click-to-commit gesture needs the LostFocus arm too. An unparseable
    /// number commits nothing (never a client error — nest-side clamping is the
    /// guard, this is a UX nicety per the row's own scope note).</summary>
    private Grid BuildTextRow(string bridgeId, BridgeSetting setting, bool isNumber)
    {
        var row = new Grid { ColumnSpacing = 8 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var label = new TextBlock
        {
            Text = setting.Label,
            VerticalAlignment = VerticalAlignment.Center,
            Opacity = 0.7,
        };
        Grid.SetColumn(label, 0);

        var textBox = new TextBox
        {
            Width = 120,
            Text = isNumber ? setting.NumberValue?.ToString() ?? string.Empty : setting.TextValue ?? string.Empty,
        };
        Grid.SetColumn(textBox, 1);

        void Commit()
        {
            if (isNumber)
            {
                if (uniffi.fauna_ffi.FaunaFfiMethods.ParseCountI64(textBox.Text) is long n)
                {
                    SettingChanged?.Invoke(this,
                        new BridgeSettingChangedEventArgs(bridgeId, BridgeSettingValue.Number(setting.Key, n)));
                }
            }
            else
            {
                SettingChanged?.Invoke(this,
                    new BridgeSettingChangedEventArgs(bridgeId, BridgeSettingValue.Text(setting.Key, textBox.Text)));
            }
        }

        textBox.LostFocus += (_, _) => Commit();
        textBox.KeyDown += (_, e) =>
        {
            if (e.Key == VirtualKey.Enter) Commit();
        };

        row.Children.Add(label);
        row.Children.Add(textBox);
        return row;
    }
}

/// <summary>Payload for <see cref="BridgeCard.SourceAskClick"/> — the refused
/// <c>(bridge_id, operation, target)</c> triple the pressed button carries, plus the
/// display-only label the ask is filed under.</summary>
public sealed class SourceAskEventArgs : EventArgs
{
    public string BridgeId { get; }
    /// <summary>The wire spelling of the refused operation (<c>link</c> / <c>follow</c>).</summary>
    public string Operation { get; }
    /// <summary>Empty for a <c>link</c>; the follow id otherwise.</summary>
    public string Target { get; }
    public string Label { get; }

    internal FeedTriple Triple => new(BridgeId, Operation, Target);

    internal SourceAskEventArgs(FeedTriple triple, string label)
    {
        BridgeId = triple.BridgeId;
        Operation = triple.Operation;
        Target = triple.Target;
        Label = label;
    }
}

/// <summary>Payload for <see cref="BridgeCard.SettingChanged"/> — the bridge id
/// (the card carries no ambient identity of its own beyond <see cref="BridgeCard.Bridge"/>)
/// plus the committed value, ready to pass straight to
/// <c>INestRpcClient.BridgesSetSettingsAsync</c>.</summary>
public sealed class BridgeSettingChangedEventArgs : EventArgs
{
    public string BridgeId { get; }
    public BridgeSettingValue Value { get; }

    public BridgeSettingChangedEventArgs(string bridgeId, BridgeSettingValue value)
    {
        BridgeId = bridgeId;
        Value = value;
    }
}
