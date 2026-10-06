using System;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Helpers;
using FaunaApp.UiIds;
using uniffi.fauna_atproto_settings_machine;
using uniffi.fauna_client_connected_apps;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// Settings → Connected apps (<c>docs/goal/ui/connected-apps.md</c>; ui.yaml page
/// <c>connected-apps</c>; rail slot directly after Task delegation — <c>settings.md</c> §
/// Navigation model). A dumb renderer of <see cref="ConnectedAppsViewModel"/> over the shared
/// <c>ConnectedAppsMachine</c>: the roster's composition, the scope words, the class badge key,
/// <i>lasts-until</i> and which verb revokes a row are all the machine's — a row's key is opaque
/// here. Reference painter: <c>apps/fauna-tui/src/settings/connected_apps.rs</c>.
///
/// <para><b>The lift.</b> The AT Protocol page's consent cards and connected-app rows, the Nostr
/// page's bunker rows and the Mail &amp; Calendar page's app-password rows render HERE and no
/// longer on their old pages — a row moves, it is never shown twice (<c>connected-apps.md</c> §
/// Architectural rules).</para>
///
/// <para><b>Row shapes (the e2e contract).</b> Every indexed row is a Grid root carrying the
/// item's AutomationId and a non-empty <c>AutomationProperties.Name</c> (without one FlaUI counts
/// a code-built row as zero — reference_winui_flaui_datatemplate_name), with its leaves scoped
/// inside it. The item's own text is the joined description; a mail row adds its login, kind and
/// secret controls. The hidden secret's <c>Text</c> stays EMPTY until revealed — the driver polls
/// it until it turns non-empty and returns that AS the secret, so a mask would pass the reveal
/// test without the on-demand read ever running — and it is dense (never collapsed): a collapsed
/// element is pruned from the UIA tree, which would desync this leaf's index from the reveal
/// button's. A burned mail row carries <c>revoked="true"</c> as its HelpText (the windows bridge
/// maps any non-name attr to HelpText).</para>
///
/// <para><b>Capture suppression</b> (<c>security.md</c> § On-screen secret exposure, rule 2): a
/// mail app password is a <em>minted, revocable</em> secret, so the window's capture is
/// suppressed while, and only while, one is painted. ONE hold for the whole page, synced to
/// <em>any row revealed</em>; navigating away releases it.</para>
///
/// <para>No <c>ConfigureAwait(false)</c> anywhere here or in the VM: off-thread mutation of bound
/// state throws a silent <c>COMException</c> in WinUI.</para>
/// </summary>
public sealed partial class ConnectedAppsPage : Page, IAsyncLoadedPage
{
    private ConnectedAppsViewModel? _vm;

    private TaskCompletionSource _loadComplete =
        new(TaskCreationOptions.RunContinuationsAsynchronously);

    /// <inheritdoc />
    public Task LoadComplete => _loadComplete.Task;

    private readonly ScreenCaptureHold _captureHold =
        FaunaApp.Helpers.SuppressScreenCapture.ForMainWindow();

    private ServiceClients? _clients;

    public ConnectedAppsPage()
    {
        this.InitializeComponent();
        // Typing a code is the lookup call; the page's other gated controls are declared as
        // their rows are built (BuildRequestCard / BuildRow).
        ConnectBtn.FaunaGate("fauna.oauth.consent.lookup_code");
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _clients = clients;
        }
        // Re-arm for a reused (cached) page instance so a second navigation waits on this
        // load, not the previous one's already-completed barrier.
        if (_loadComplete.Task.IsCompleted)
            _loadComplete = new(TaskCreationOptions.RunContinuationsAsynchronously);
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        // The reveal's only end besides hiding: walking away lifts the suppression. Skipping
        // this is the stuck-on failure — an app that has silently stopped taking screenshots.
        _captureHold.Sync(false);
        base.OnNavigatedFrom(e);
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        try
        {
            await LoadAsync();
        }
        finally
        {
            // ALWAYS complete, so the agent's bounded await never hangs a navigation.
            _loadComplete.TrySetResult();
        }
    }

    private async Task LoadAsync()
    {
        if (_clients?.Rpc is null) return;
        try
        {
            // A visit starts unread: a fresh VM (and so a fresh machine) per visit, painting
            // neither rows nor the empty state until its own read has returned.
            _vm = new ConnectedAppsViewModel(_clients.Rpc, _clients.Account.Handle ?? string.Empty)
            {
                HandoffSource = new AppHandoffSource(),
            };
            _vm.Changed += Render;
            LoadProgress.IsActive = true;
            LoadProgress.Visibility = Visibility.Visible;
            Render();
            await _vm.VisitAsync();
        }
        catch (Exception ex)
        {
            ShowError(S.Error(ex));
        }
        finally
        {
            LoadProgress.IsActive = false;
            LoadProgress.Visibility = Visibility.Collapsed;
        }
        Render();
    }

    /// <summary>The app's route door as the VM's staged-handoff source.</summary>
    private sealed class AppHandoffSource : IConsentHandoffSource
    {
        public string? Pending => App.PendingConsentHandoff;

        public void Finish(string requestUri) => App.FinishConsentHandoff(requestUri);
    }

    private void ShowError(string? message)
    {
        if (message is not null)
        {
            ErrorBar.Message = message;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = message;
        }
        else
        {
            ErrorBar.IsOpen = false;
            App.CurrentErrorMessage = null;
        }
    }

    // ── Render ──

    /// <summary>Project the VM onto the tree. Called after the visit's reads land and after every
    /// gesture — the page is non-optimistic, so what is shown is what the nest persisted.</summary>
    private void Render()
    {
        if (_vm is null) return;
        ShowError(_vm.PageError);

        var snap = _vm.Snapshot;

        // 1. Requests — only while a request is live.
        RequestsHost.Children.Clear();
        var requests = snap?.@requests ?? Array.Empty<ConsentCardRow>();
        RequestsSection.Visibility = requests.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
        foreach (var request in requests)
        {
            RequestsHost.Children.Add(BuildRequestCard(request));
        }

        // 3. The roster — the three-state list: nothing until the read has returned, then
        //    either rows or the empty state.
        RowsHost.Children.Clear();
        var principals = _vm.Loaded ? snap!.@principals : Array.Empty<ConnectedAppRow>();
        EmptyText.Visibility = _vm.Loaded && principals.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
        foreach (var row in principals)
        {
            RowsHost.Children.Add(BuildRow(row));
        }

        // 4. Blocked apps — only while something is blocked.
        BlockedHost.Children.Clear();
        var blocked = snap?.@blocked ?? Array.Empty<BlockedAppRow>();
        BlockedSection.Visibility = blocked.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
        foreach (var b in blocked)
        {
            BlockedHost.Children.Add(BuildBlockedRow(b));
        }

        // Every gesture re-renders through here, so this is the one place that sees a reveal
        // land or end. Idempotent: N re-renders with a secret showing take one hold.
        _captureHold.Sync(_vm.Revealed.Count > 0);
    }

    private static Grid NewCard(string automationId, string name)
    {
        var grid = new Grid
        {
            Padding = new Thickness(12),
            RowSpacing = 6,
            BorderBrush = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"],
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(6),
        };
        AutomationProperties.SetAutomationId(grid, automationId);
        AutomationProperties.SetName(grid, name);
        return grid;
    }

    private static void AddRow(Grid grid, FrameworkElement child)
    {
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        Grid.SetRow(child, grid.RowDefinitions.Count - 1);
        grid.Children.Add(child);
    }

    private static Button NewButton(string automationId, string label, RoutedEventHandler onClick, string? name = null)
    {
        var button = new Button { Content = label };
        AutomationProperties.SetAutomationId(button, automationId);
        if (name is not null) AutomationProperties.SetName(button, name);
        button.Click += onClick;
        return button;
    }

    // ── A request card ──

    /// <summary>One <c>connected-apps-request-card</c>: a third-party app is asking to act for
    /// this account and is waiting for the answer — the built consent card, moved here from the
    /// AT Protocol page with its wording unchanged. Every request the nest lists is painted;
    /// the binding code is the user's check. ⚠ Approve and Decline render unconditionally —
    /// <b>never gated on how close the request is to expiring</b>: a resolution is reported
    /// even past <c>expires_at</c>, so "this request just expired" may only ever come from the
    /// resolve reply.</summary>
    private FrameworkElement BuildRequestCard(ConsentCardRow request)
    {
        var card = NewCard(Ids.ConnectedAppsRequestCard, "consent request");
        AddRow(card, new TextBlock
        {
            Text = ConnectedAppsViewModel.RequestText(request),
            TextWrapping = TextWrapping.Wrap,
        });

        // The code's text is localized prose; the e2e reads the raw value off the `code` attr
        // (mapped from HelpText), never the sentence.
        var code = new TextBlock { Text = ConnectedAppsViewModel.RequestCodeText(request) };
        AutomationProperties.SetAutomationId(code, Ids.ConnectedAppsRequestCode);
        AutomationProperties.SetHelpText(code, request.@code);
        AddRow(card, code);

        AddRow(card, new TextBlock
        {
            Text = S.Get("atproto_settings/consent_code_hint"),
            Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
            TextWrapping = TextWrapping.Wrap,
        });

        // Each button is declared to the offline gate as it is built: the rows are rebuilt on
        // every render, so a declaration at construction time would never see these.
        var buttons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        var approve = NewButton(Ids.ConnectedAppsRequestApprove,
            S.Get("atproto_settings/consent_approve_button"), Approve_Click);
        approve.Style = (Style)Application.Current.Resources["AccentButtonStyle"];
        approve.Tag = request.@consentIdHex;
        buttons.Children.Add(approve);
        var decline = Tagged(NewButton(Ids.ConnectedAppsRequestDecline,
            S.Get("atproto_settings/consent_deny_button"), Decline_Click), request.@consentIdHex);
        buttons.Children.Add(decline);
        var block = Tagged(NewButton(Ids.ConnectedAppsRequestBlock,
            S.Get("connected_apps/block"), Block_Click), request.@consentIdHex);
        buttons.Children.Add(block);
        AddRow(card, buttons);
        // Approve and Decline are both the resolve call; Block is the per-client block.
        approve.FaunaGate("fauna.bridges.atproto.resolve_consent");
        decline.FaunaGate("fauna.bridges.atproto.resolve_consent");
        block.FaunaGate("fauna.oauth.consent.block_client");
        return card;
    }

    private static Button Tagged(Button button, string tag)
    {
        button.Tag = tag;
        return button;
    }

    // ── A roster row ──

    /// <summary>One <c>connected-apps-item</c>: the joined description as the item's own text
    /// (ui.yaml mints no per-field leaves for the columns every row has), a mail app password's
    /// own leaves, then Revoke, or the armed confirm pair.</summary>
    private FrameworkElement BuildRow(ConnectedAppRow row)
    {
        var item = NewCard(Ids.ConnectedAppsItem, "connected app");
        var burned = row.@mail is { @revoked: true };
        if (burned)
        {
            // The burned state as a value a reader can count, beside the words.
            AutomationProperties.SetHelpText(item, "true");
        }
        AddRow(item, new TextBlock
        {
            Text = ConnectedAppsViewModel.RowText(row),
            TextWrapping = TextWrapping.Wrap,
        });

        if (row.@mail is { } mail)
        {
            AddRow(item, MailLeaf(Ids.ConnectedAppsItemType, Strings.Resolve(mail.@kind)));
            AddRow(item, MailLeaf(Ids.ConnectedAppsItemUsername, _vm!.MuaUsername(mail), selectable: true));

            // ⚠ The hidden secret's text MUST stay EMPTY (class remarks). AutomationId only, no
            // static Name: a get_text target with a static Name returns the wrong value.
            var revealed = _vm.Revealed.TryGetValue(row.@key, out var secretText);
            var secret = new TextBlock
            {
                Text = revealed ? secretText! : string.Empty,
                IsTextSelectionEnabled = true,
                FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Consolas"),
                FontSize = 12,
            };
            AutomationProperties.SetAutomationId(secret, Ids.ConnectedAppsItemSecret);
            AddRow(item, secret);

            var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
            actions.Children.Add(Tagged(NewButton(Ids.ConnectedAppsItemCopyUsername,
                S.Get("settings/mail/copy_username"), CopyUsername_Click), _vm.MuaUsername(mail)));
            actions.Children.Add(Tagged(NewButton(Ids.ConnectedAppsItemRevealSecret,
                S.Get(revealed ? "settings/mail/hide_secret" : "settings/mail/reveal_secret"),
                Reveal_Click), row.@key));
            // Copy is independent of the reveal toggle: the secret reaches the clipboard without
            // being painted on a screen someone else can read.
            actions.Children.Add(Tagged(NewButton(Ids.ConnectedAppsItemCopySecret,
                S.Get("settings/mail/copy_secret"), CopySecret_Click), row.@key));
            AddRow(item, actions);
        }

        if (_vm!.RevokeArmed == row.@key)
        {
            AddRow(item, new TextBlock
            {
                Text = S.Get("connected_apps/revoke_prompt").Replace("{name}", S.Resolve(row.@name)),
                TextWrapping = TextWrapping.Wrap,
            });
            var confirm = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
            var confirmButton = Tagged(NewButton(Ids.ConnectedAppsItemRevokeConfirm,
                S.Get("connected_apps/revoke_confirm"), RevokeConfirm_Click), row.@key);
            confirm.Children.Add(confirmButton);
            confirm.Children.Add(Tagged(NewButton(Ids.ConnectedAppsItemRevokeCancel,
                S.Get("connected_apps/revoke_cancel"), RevokeCancel_Click), row.@key));
            AddRow(item, confirm);
            // The confirm is the commit. A mail app password's revoke is a write to this client's
            // own config (offline-safe — the ruling the Mail & Calendar page's revoke carried), so
            // only the nest-side verbs the machine picks for every other row are gated.
            if (row.@mail is null) confirmButton.FaunaGate("fauna.principals.revoke");
        }
        else
        {
            AddRow(item, Tagged(NewButton(Ids.ConnectedAppsItemRevoke,
                S.Get("connected_apps/revoke"), RevokeArm_Click), row.@key));
        }
        return item;
    }

    private static TextBlock MailLeaf(string automationId, string text, bool selectable = false)
    {
        var leaf = new TextBlock
        {
            Text = text,
            Opacity = 0.7,
            FontSize = 12,
            IsTextSelectionEnabled = selectable,
        };
        AutomationProperties.SetAutomationId(leaf, automationId);
        return leaf;
    }

    // ── A blocked row ──

    private FrameworkElement BuildBlockedRow(BlockedAppRow blocked)
    {
        var item = NewCard(Ids.ConnectedAppsBlockedItem, "blocked app");
        // The client id verbatim, as the request card showed it — nothing here parses it into a
        // host or a name.
        AddRow(item, new TextBlock
        {
            Text = ConnectedAppsViewModel.BlockedText(blocked),
            TextWrapping = TextWrapping.Wrap,
        });
        AddRow(item, Tagged(NewButton(Ids.ConnectedAppsBlockedItemUnblock,
            S.Get("connected_apps/unblock"), Unblock_Click), blocked.@clientId));
        return item;
    }

    // ── Gestures ──

    private void CodeBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_vm is null) return;
        _vm.Code = CodeBox.Text;
        ConnectBtn.IsEnabled = CodeBox.Text.Trim().Length > 0;
    }

    private async void Connect_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.SubmitCodeAsync();
        CodeBox.Text = string.Empty;
        Render();
    }

    private async void Approve_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || (sender as FrameworkElement)?.Tag is not string id) return;
        await _vm.ResolveRequestAsync(id, approved: true);
        Render();
    }

    private async void Decline_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || (sender as FrameworkElement)?.Tag is not string id) return;
        await _vm.ResolveRequestAsync(id, approved: false);
        Render();
    }

    private async void Block_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || (sender as FrameworkElement)?.Tag is not string id) return;
        await _vm.BlockRequestAsync(id);
        Render();
    }

    private async void Unblock_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || (sender as FrameworkElement)?.Tag is not string clientId) return;
        await _vm.UnblockAsync(clientId);
        Render();
    }

    private void RevokeArm_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || (sender as FrameworkElement)?.Tag is not string key) return;
        _vm.ArmRevoke(key);
        Render();
    }

    private void RevokeCancel_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.CancelRevoke();
        Render();
    }

    private async void RevokeConfirm_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || (sender as FrameworkElement)?.Tag is not string key) return;
        await _vm.ConfirmRevokeAsync(key);
        Render();
    }

    private async void Reveal_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || (sender as FrameworkElement)?.Tag is not string key) return;
        await _vm.ToggleRevealAsync(key);
        Render();
    }

    private void CopyUsername_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: string username } button || username.Length == 0) return;
        FaunaApp.Helpers.ClipboardHelper.CopyText(username);
        button.Content = S.Get("settings/mail/copied");
    }

    /// <summary>Read the secret on demand and put it on the clipboard without painting it.</summary>
    private async void CopySecret_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button { Tag: string key } button) return;
        var secret = await _vm.ReadSecretAsync(key);
        if (secret is not null)
        {
            FaunaApp.Helpers.ClipboardHelper.CopyText(secret);
            button.Content = S.Get("settings/mail/copied");
        }
        else
        {
            // A failed read leaves the machine's own error on the snapshot.
            Render();
        }
    }
}
