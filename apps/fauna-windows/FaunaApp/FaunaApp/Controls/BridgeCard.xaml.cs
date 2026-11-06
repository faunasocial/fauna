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
        FollowsHeading.Text = S.Get("bridges/follows");
        FollowIdInput.PlaceholderText = S.Get("bridges/id_to_follow");
        FollowPetnameInput.PlaceholderText = S.Get("bridges/petname_optional");
        AddFollowButton.Content = S.Get("bridges/add_follow");
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

    /// <summary><c>bridge-follow-request-approve</c> / <c>-refuse</c> was pressed on one
    /// row (bridges.md § Follow requests). The card decides nothing: the host page
    /// answers it over <c>BridgesViewModel.AnswerFollowRequestAsync</c>, which re-reads.</summary>
    public event EventHandler<FollowRequestAnswerEventArgs>? FollowRequestAnswer;

    /// <summary><c>bridge-add-follow-button</c> was pressed with a non-blank id (bridges.md
    /// § User actions). The host adds over <c>BridgesViewModel.AddFollowAsync</c>, which
    /// re-reads.</summary>
    public event EventHandler<FollowAddEventArgs>? FollowAdd;

    /// <summary><c>bridge-follow-remove</c> was pressed on one follow row. The host
    /// removes over <c>BridgesViewModel.RemoveFollowAsync</c>, which re-reads.</summary>
    public event EventHandler<FollowRemoveEventArgs>? FollowRemove;

    /// <summary>The bridge the add row's buffers were typed for: a recycled card handed a
    /// different bridge starts with an empty add row.</summary>
    private string? _renderedBridgeId;

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
        var identity = bridge.Linked ? bridge.IdentityLine : null;
        IdentityText.Text = identity ?? string.Empty;
        IdentityText.Visibility = identity is null ? Visibility.Collapsed : Visibility.Visible;
        ModeText.Text = bridge.Mode ?? string.Empty;
        RenderSettings(bridge);
        RenderFollows(bridge);
        RenderFollowRequests(bridge);
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

    // ── Follows (bridges.md § Follows; apple's BridgeCardContent `followsContent`,
    //    tui's bridges.rs). Rendered for a LINKED card whose provider supports
    //    follows. A row paints the shared follow_display (the nickname, else the id).

    private void RenderFollows(BridgeInfo bridge)
    {
        if (_renderedBridgeId != bridge.Id)
        {
            FollowIdInput.Text = string.Empty;
            FollowPetnameInput.Text = string.Empty;
            _renderedBridgeId = bridge.Id;
        }
        FollowRows.Children.Clear();
        if (!bridge.Linked || !bridge.SupportsFollows)
        {
            FollowsPanel.Visibility = Visibility.Collapsed;
            return;
        }
        foreach (var follow in bridge.Follows ?? Array.Empty<BridgeFollow>())
        {
            FollowRows.Children.Add(BuildFollowRow(bridge.Id, follow));
        }
        FollowsPanel.Visibility = Visibility.Visible;
    }

    private Grid BuildFollowRow(string bridgeId, BridgeFollow follow)
    {
        var display = uniffi.fauna_ffi.FaunaFfiMethods.FollowDisplay(follow.Id, follow.Petname);
        var row = new Grid { ColumnSpacing = 8 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        AutomationProperties.SetAutomationId(row, FaunaApp.UiIds.Ids.BridgeFollowItem);
        // A bare Grid carrying only an AutomationId is UIA-pruned; the Name keeps it.
        AutomationProperties.SetName(row, display);

        var text = new TextBlock
        {
            Text = display,
            VerticalAlignment = VerticalAlignment.Center,
            TextWrapping = TextWrapping.Wrap,
        };
        Grid.SetColumn(text, 0);

        // A glyph, so the row's get_text reads the follow alone (FlaUI's GetText skips
        // single-character icon glyphs); the label is the button's accessible name.
        var remove = new Button
        {
            Content = "\uE711",
            FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Segoe MDL2 Assets"),
            FontSize = 10,
            Padding = new Thickness(4, 2, 4, 2),
        };
        AutomationProperties.SetAutomationId(remove, FaunaApp.UiIds.Ids.BridgeFollowRemove);
        AutomationProperties.SetName(remove, S.Get("bridges/remove_follow"));
        Grid.SetColumn(remove, 1);
        var followId = follow.Id;
        remove.Click += (_, _) => FollowRemove?.Invoke(this, new FollowRemoveEventArgs(bridgeId, followId));

        row.Children.Add(text);
        row.Children.Add(remove);
        return row;
    }

    private void FollowIdInput_TextChanged(object sender, TextChangedEventArgs e)
        => AddFollowButton.IsEnabled = !string.IsNullOrWhiteSpace(FollowIdInput.Text);

    private void AddFollowButton_Click(object sender, RoutedEventArgs e)
    {
        if (Bridge is not { } bridge) return;
        var id = FollowIdInput.Text.Trim();
        if (id.Length == 0) return;
        var petname = FollowPetnameInput.Text.Trim();
        // Dispatch clears the buffers; a refused follow's ask keys on its own triple,
        // never on these (family-safety.md § Feed-source approvals, rule (d)).
        FollowIdInput.Text = string.Empty;
        FollowPetnameInput.Text = string.Empty;
        FollowAdd?.Invoke(this, new FollowAddEventArgs(bridge.Id, id, petname.Length == 0 ? null : petname));
    }

    // ── Follow requests (bridges.md § Follow requests; apple's BridgeCardContent
    //    `followRequestsContent`, tui's bridges.rs). Rendered for a LINKED bridge whose
    //    provider declares the capability: the heading with the count (kept at zero),
    //    one row per request painting the shared row model's `display`, Approve /
    //    Refuse forwarding the row's id — no confirm, and the host re-reads.

    private void RenderFollowRequests(BridgeInfo bridge)
    {
        FollowRequestRows.Children.Clear();
        if (!bridge.Linked || !bridge.SupportsFollowRequests)
        {
            FollowRequestsPanel.Visibility = Visibility.Collapsed;
            return;
        }

        var requests = bridge.FollowRequests ?? Array.Empty<BridgeFollowRequest>();
        FollowRequestsHeading.Text = S.Format("bridges/follow_requests", requests.Count.ToString());
        foreach (var request in requests)
        {
            FollowRequestRows.Children.Add(BuildFollowRequestRow(bridge.Id, request));
        }
        FollowRequestsPanel.Visibility = Visibility.Visible;
    }

    private Grid BuildFollowRequestRow(string bridgeId, BridgeFollowRequest request)
    {
        var row = new Grid { ColumnSpacing = 8 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        AutomationProperties.SetAutomationId(row, FaunaApp.UiIds.Ids.BridgeFollowRequestItem);
        // A bare Grid carrying only an AutomationId is UIA-pruned; the Name is what
        // materializes the row (memory reference_winui_flaui_datatemplate_name).
        AutomationProperties.SetName(row, request.Display);

        var text = new TextBlock
        {
            Text = request.Display,
            VerticalAlignment = VerticalAlignment.Center,
            TextWrapping = TextWrapping.Wrap,
        };
        Grid.SetColumn(text, 0);

        var approveButton = new Button { Content = S.Get("bridges/approve_follow_request") };
        AutomationProperties.SetAutomationId(approveButton, FaunaApp.UiIds.Ids.BridgeFollowRequestApprove);
        Grid.SetColumn(approveButton, 1);

        var refuseButton = new Button { Content = S.Get("bridges/refuse_follow_request") };
        AutomationProperties.SetAutomationId(refuseButton, FaunaApp.UiIds.Ids.BridgeFollowRequestRefuse);
        Grid.SetColumn(refuseButton, 2);

        // Each button captures ITS OWN row's id, never a position the re-read can move.
        var id = request.Id;
        approveButton.Click += (_, _) => FollowRequestAnswer?.Invoke(
            this, new FollowRequestAnswerEventArgs(bridgeId, id, approve: true));
        refuseButton.Click += (_, _) => FollowRequestAnswer?.Invoke(
            this, new FollowRequestAnswerEventArgs(bridgeId, id, approve: false));

        row.Children.Add(text);
        row.Children.Add(approveButton);
        row.Children.Add(refuseButton);
        return row;
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

    // ── Metadata-driven settings (bridges.md § Bridge settings) — the one settings renderer, BridgeSettingRows, shared with a
    //    hosted plugin's settings card on the Bridges page.

    private void RenderSettings(BridgeInfo bridge)
    {
        SettingsPanel.Children.Clear();
        foreach (var setting in bridge.Settings)
        {
            var bridgeId = bridge.Id;
            SettingsPanel.Children.Add(BridgeSettingRows.Build(setting, value =>
                SettingChanged?.Invoke(this, new BridgeSettingChangedEventArgs(bridgeId, value))));
        }
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

/// <summary>Payload for <see cref="BridgeCard.FollowAdd"/>: the bridge, the external id
/// and the optional nickname, ready for <c>BridgesViewModel.AddFollowAsync</c>.</summary>
public sealed class FollowAddEventArgs : EventArgs
{
    public string BridgeId { get; }
    public string Id { get; }
    public string? Petname { get; }

    public FollowAddEventArgs(string bridgeId, string id, string? petname)
    {
        BridgeId = bridgeId;
        Id = id;
        Petname = petname;
    }
}

/// <summary>Payload for <see cref="BridgeCard.FollowRemove"/>: the bridge and that row's
/// own follow id.</summary>
public sealed class FollowRemoveEventArgs : EventArgs
{
    public string BridgeId { get; }
    public string FollowId { get; }

    public FollowRemoveEventArgs(string bridgeId, string followId)
    {
        BridgeId = bridgeId;
        FollowId = followId;
    }
}

/// <summary>Payload for <see cref="BridgeCard.FollowRequestAnswer"/> — the bridge, the
/// row's own request id and the verdict, ready for
/// <c>BridgesViewModel.AnswerFollowRequestAsync</c>.</summary>
public sealed class FollowRequestAnswerEventArgs : EventArgs
{
    public string BridgeId { get; }
    public string Id { get; }
    public bool Approve { get; }

    public FollowRequestAnswerEventArgs(string bridgeId, string id, bool approve)
    {
        BridgeId = bridgeId;
        Id = id;
        Approve = approve;
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
