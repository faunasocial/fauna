using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Helpers;
using uniffi.fauna_ffi;

namespace FaunaApp.Views;

/// <summary>
/// The standalone Nostr settings page (docs/goal/ui/nostr.md § Page structure /
/// § Layout &amp; flow), closing the last "windows absent" row in that doc's
/// implementation-status table. A dumb renderer of the testable
/// <see cref="NostrViewModel"/> over the unified <c>fauna.bridges.*</c> seam keyed
/// <c>bridge_id:"nostr"</c> — no new FFI, no new Rust (priority #2).
///
/// <para>The rendering contract this file must not break, in both directions:
/// <see cref="NostrViewModel.Registered"/> gates the unavailable notice while
/// <c>Available</c> gates nothing (the web/apple bootstrap bug of 2026-07-14), and
/// the linked gate genuinely swaps the account surface (the linux bug of
/// 2026-07-19 — without it <c>nostr-pubkey-copy-btn</c>, the e2e's
/// <c>is_linked()</c> signal, is meaningless).</para>
///
/// <para>No <c>ConfigureAwait(false)</c> anywhere here or in the VM: off-thread
/// mutation of bound state throws a silent <c>COMException</c> in WinUI.</para>
/// </summary>
public sealed partial class NostrPage : Page, IAsyncLoadedPage
{
    private ServiceClients? _clients;
    private NostrViewModel? _vm;
    private bool _suppressToggles;

    /// <summary>The five content-toggle switches, keyed by their wire settings key —
    /// built once in <see cref="BuildContentToggles"/> from the shared catalog, so
    /// <see cref="RenderState"/> can still paint them by key without re-spelling any
    /// of the five names.</summary>
    private readonly Dictionary<string, ToggleSwitch> _toggleSwitchesByKey = new();

#if PAYMENTS
    /// <summary>The § 6 Zap signers surface, built in code and dropped into
    /// <c>ZapSignersSectionHost</c>. Not named from markup: its type is removed
    /// from the store-safe build (FaunaApp.csproj drops Views\Payments\**), and
    /// a XAML element naming it would fail to compile there.</summary>
    private Views.Payments.NostrZapSignersSection? _zapSigners;
#endif

    /// <summary>Capture suppression while the one-time <c>bunker://…</c> connect string is
    /// painted — rule 2 of <c>docs/goal/architecture/security.md</c> § On-screen secret
    /// exposure. The string carries a single-use secret with a hard TTL and is re-mintable,
    /// which is precisely rule 2's <em>minted, revocable</em> class.
    ///
    /// <para>The QR beside it is the same secret in another encoding, so it is covered by
    /// the same hold rather than needing one of its own — the affinity is a property of the
    /// window, not of a control, and both are painted and dropped together.</para></summary>
    private readonly ScreenCaptureHold _captureHold =
        FaunaApp.Helpers.SuppressScreenCapture.ForMainWindow();

    public NostrPage()
    {
        this.InitializeComponent();
        BuildLinkModes();
        BuildContentToggles();
    }

    private TaskCompletionSource _loadComplete =
        new(TaskCreationOptions.RunContinuationsAsynchronously);

    /// <inheritdoc />
    public Task LoadComplete => _loadComplete.Task;

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _clients = clients;
        }
        // Re-arm for a reused (cached) page instance so a second navigation waits on
        // this load, not the previous one's already-completed barrier.
        if (_loadComplete.Task.IsCompleted)
            _loadComplete = new(TaskCreationOptions.RunContinuationsAsynchronously);
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        base.OnNavigatedFrom(e);
        _vm?.CleanupReconnect();
        // Rule 2's "or navigate-away" arm, and the release of last resort: walking off the
        // page with the connect string on screen must not strand the hold and leave the
        // whole app uncapturable.
        _captureHold.Sync(false);
    }

    /// <summary>The 3 native link-request modes — generate / import / remote. NIP-07
    /// is a browser-extension boundary and is web-only (nostr.md § Architectural rules
    /// #4), matching apple/android/linux/tui. Each item's automation Name is the stable
    /// wire mode, never the localized label: the windows driver's <c>select</c> matches
    /// <c>ComboBoxItem.Name</c> EXACTLY (reference_windows_flaui_select_exact_name).
    ///
    /// <para>The label itself comes from the shared
    /// <c>fauna_client_bridges::nostr_link_mode_label</c> map (nostr.md § Account
    /// linking) — the same pattern <see cref="NostrViewModel.SigningModeLabel"/> uses
    /// for the *stored*-mode label — rather than three hand-typed i18n keys; windows
    /// was the last app carrying its own copy of this table.</para></summary>
    private void BuildLinkModes()
    {
        foreach (var mode in new[] { NostrViewModel.ModeGenerate, NostrViewModel.ModeImport, NostrViewModel.ModeRemote })
        {
            var item = new ComboBoxItem
            {
                Content = Strings.Resolve(FaunaFfiMethods.NostrLinkModeLabel(mode)),
                Tag = mode,
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, mode);
            LinkModeCombo.Items.Add(item);
        }
        LinkModeCombo.SelectedIndex = 0;
    }

    /// <summary>Build the five content-toggle rows from the shared catalog
    /// (<c>fauna_client_bridges::nostr_settings::nostr_content_toggle_options</c>) —
    /// wire key, element id, label and optional subtitle all come from there, none
    /// re-spelled here (nostr.md § Where logic lives → *The content-toggle
    /// catalog*). Mirrors linux's/android's/apple's per-row dynamic construction
    /// rather than a WinUI DataTemplate, so <see cref="RenderToggle"/>'s existing
    /// suppression-guarded imperative <c>IsOn</c> set (needed to keep a
    /// programmatic repaint from re-entering <see cref="ContentToggle_Toggled"/>)
    /// stays exactly as it was for the five static switches.</summary>
    private void BuildContentToggles()
    {
        foreach (var opt in FaunaFfiMethods.NostrContentToggleOptions())
        {
            var grid = new Grid { ColumnSpacing = 8 };
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

            var label = Strings.Resolve(opt.@label);
            var textColumn = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
            textColumn.Children.Add(new TextBlock { Text = label, TextWrapping = TextWrapping.NoWrap });
            if (opt.@subtitle is { } subtitle)
            {
                textColumn.Children.Add(new TextBlock
                {
                    Text = Strings.Resolve(subtitle),
                    Opacity = 0.7,
                    FontSize = 12,
                    TextWrapping = TextWrapping.Wrap,
                });
            }
            Grid.SetColumn(textColumn, 0);
            grid.Children.Add(textColumn);

            var toggle = new ToggleSwitch
            {
                OnContent = string.Empty,
                OffContent = string.Empty,
                MinWidth = 0,
                Tag = opt.@key,
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(toggle, opt.@uiId);
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(toggle, label);
            toggle.Toggled += ContentToggle_Toggled;
            Grid.SetColumn(toggle, 1);
            grid.Children.Add(toggle);

            ContentTogglesHost.Children.Add(grid);
            _toggleSwitchesByKey[opt.@key] = toggle;
        }
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
        _vm ??= new NostrViewModel(_clients.Rpc);
#if PAYMENTS
        // §6 lives in its own removable build item — bind it the same way,
        // one seam further out (dynamic-features.md § Platform-family surface
        // excision), mirroring ProfilePage's PaymentsSectionsHost.
        if (_zapSigners is null)
        {
            _zapSigners = new Views.Payments.NostrZapSignersSection();
            ZapSignersSectionHost.Content = _zapSigners;
            _zapSigners.Attach(_vm, RenderState);
        }
#endif
        await _vm.LoadAsync();
        RenderState();
    }

    // ── Render ──

    private void RenderState()
    {
        if (_vm is null) return;

        // ONLY `Registered` may gate this notice. See the class remarks.
        UnavailableBar.IsOpen = !_vm.Registered;

        var linked = _vm.Linked;
        LinkForm.Visibility = linked ? Visibility.Collapsed : Visibility.Visible;
        LinkedAccount.Visibility = linked ? Visibility.Visible : Visibility.Collapsed;
        ContentSettings.Visibility = linked ? Visibility.Visible : Visibility.Collapsed;
        RelaysSection.Visibility = linked ? Visibility.Visible : Visibility.Collapsed;
        FollowsSection.Visibility = linked ? Visibility.Visible : Visibility.Collapsed;

        RenderLinkModeFields();

        PubkeyText.Text = _vm.IdentityDisplay ?? string.Empty;
        SigningModeText.Text = _vm.SigningModeLabel;

        NpubConfirmBanner.Visibility = linked && _vm.NpubConfirmationOwed
            ? Visibility.Visible : Visibility.Collapsed;
        NpubConfirmBannerText.Text = _vm.NpubConfirmBannerText;

        _suppressToggles = true;
        foreach (var (key, toggle) in _toggleSwitchesByKey)
            RenderToggle(toggle, key);
        _suppressToggles = false;

        RelaysList.ItemsSource = _vm.Relays.ToList();
        RelaysEmpty.Visibility = _vm.Relays.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        FollowsList.ItemsSource = _vm.Follows.ToList();
        FollowsEmpty.Visibility = _vm.Follows.Count == 0 ? Visibility.Visible : Visibility.Collapsed;

        // Order matters: the VM's error is published FIRST so RenderConnectedApps can
        // override it with a QR-paint failure. The reverse order silently swallowed
        // that failure — ShowError(_vm.ErrorMessage) would clear it a line later.
        ShowError(_vm.ErrorMessage);
        RenderConnectedApps();
#if PAYMENTS
        _zapSigners?.Render();
#endif
    }

    /// <summary>Paint the Connected apps section: its own custodial gate (NOT the
    /// plain linked gate the sections above share), the roster, and the one-time
    /// connect-string reveal.</summary>
    private void RenderConnectedApps()
    {
        var show = _vm!.ShowConnectedApps;
        ConnectedAppsSection.Visibility = show ? Visibility.Visible : Visibility.Collapsed;

        var connect = _vm.ConnectString;
        // The single sync point for the reveal: this method is the only writer of
        // ConnectReveal's visibility, so both arms below are covered by one idempotent call.
        _captureHold.Sync(!string.IsNullOrEmpty(connect));
        if (string.IsNullOrEmpty(connect))
        {
            // Drop the painted matrix with the reveal: the connect string carries a
            // live one-time secret, so it should not outlive its own display in the
            // visual tree (the identity-export QR's rule, same reasoning).
            QrPainter.Clear(ConnectQrCanvas);
            ConnectStringText.Text = string.Empty;
            ConnectReveal.Visibility = Visibility.Collapsed;
            return;
        }

        ConnectStringText.Text = connect;
        ConnectReveal.Visibility = Visibility.Visible;
        try
        {
            QrPainter.Paint(ConnectQrCanvas, connect!);
        }
        catch (Exception ex)
        {
            // Surfaced, never silently swallowed: the copyable string above is still
            // usable, so a QR-encode failure degrades rather than blocks — but a blank
            // canvas with zero feedback is undebuggable in production and in e2e.
            QrPainter.Clear(ConnectQrCanvas);
            ShowError(Strings.Error(ex));
        }
    }

    /// <summary>Paint one content toggle and mirror its on/off into HelpText — the
    /// cross-app <c>get_attr(id, "state")</c> contract
    /// (reference_windows_flaui_state_attr_helptext).</summary>
    private void RenderToggle(ToggleSwitch toggle, string key)
    {
        var on = _vm!.Toggle(key);
        toggle.IsOn = on;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(toggle, on ? "on" : "off");
    }

    /// <summary>The nsec box shows in import mode, the bunker box in remote mode —
    /// both only while unlinked (the mode picker itself is hidden once linked, so its
    /// selection is moot then). Mirrors linux's <c>NostrLinkGate</c>.</summary>
    private void RenderLinkModeFields()
    {
        var unlinked = _vm is not null && !_vm.Linked;
        var mode = (LinkModeCombo.SelectedItem as ComboBoxItem)?.Tag as string
                   ?? NostrViewModel.ModeGenerate;
        NsecInput.Visibility = unlinked && mode == NostrViewModel.ModeImport
            ? Visibility.Visible : Visibility.Collapsed;
        BunkerInput.Visibility = unlinked && mode == NostrViewModel.ModeRemote
            ? Visibility.Visible : Visibility.Collapsed;
    }

    // ── Handlers ──

    private void LinkMode_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_vm is not null)
        {
            _vm.LinkMode = (LinkModeCombo.SelectedItem as ComboBoxItem)?.Tag as string
                           ?? NostrViewModel.ModeGenerate;
        }
        RenderLinkModeFields();
    }

    private async void Link_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.LinkMode = (LinkModeCombo.SelectedItem as ComboBoxItem)?.Tag as string
                       ?? NostrViewModel.ModeGenerate;
        var secret = _vm.LinkMode switch
        {
            NostrViewModel.ModeImport => NsecInput.Password,
            NostrViewModel.ModeRemote => BunkerInput.Text,
            _ => null,
        };
        await _vm.LinkAsync(secret);
        NsecInput.Password = string.Empty;
        RenderState();
    }

    private async void Unlink_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.UnlinkAsync();
        RenderState();
    }

    private async void NpubConfirmYes_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.ConfirmNpubAsync();
        RenderState();
    }

    // Both copy affordances route through the one shared ClipboardHelper rather than
    // the hand-rolled DataPackage idiom this page shipped with (priority #4 — resolve
    // drift, don't match it).
    private void PubkeyCopy_Click(object sender, RoutedEventArgs e) =>
        ClipboardHelper.CopyText(_vm?.IdentityDisplay);

    private async void ContentToggle_Toggled(object sender, RoutedEventArgs e)
    {
        // Guarded against the programmatic IsOn set in RenderState.
        if (_suppressToggles || _vm is null) return;
        if (sender is not ToggleSwitch sw || sw.Tag is not string key) return;
        await _vm.SetToggleAsync(key, sw.IsOn);
        RenderState();
    }

    private async void AddRelay_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.AddRelayAsync(RelayInput.Text);
        if (string.IsNullOrEmpty(_vm.ErrorMessage)) RelayInput.Text = string.Empty;
        RenderState();
    }

    private async void RemoveRelay_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button btn || btn.Tag is not string url) return;
        // Resolve by value, not by a captured index: the list re-projects on every
        // refresh, so a stale index could remove the wrong row.
        var index = _vm.Relays.ToList().FindIndex(r => r.Url == url);
        await _vm.RemoveRelayAsync(index);
        RenderState();
    }

    private async void AddFollow_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.AddFollowAsync(FollowPubkeyInput.Text, FollowPetnameInput.Text);
        if (string.IsNullOrEmpty(_vm.ErrorMessage))
        {
            FollowPubkeyInput.Text = string.Empty;
            FollowPetnameInput.Text = string.Empty;
        }
        RenderState();
    }

    private async void RemoveFollow_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button btn || btn.Tag is not string id) return;
        var index = _vm.Follows.ToList().FindIndex(f => f.Id == id);
        await _vm.RemoveFollowAsync(index);
        RenderState();
    }

    // ── Connected apps (Nostr Connect / NIP-46 bunker) ──

    private async void ConnectApp_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.ConnectAppAsync();
        RenderState();
    }

    private void ConnectStringCopy_Click(object sender, RoutedEventArgs e) =>
        ClipboardHelper.CopyText(_vm?.ConnectString);

    private void ShowError(string? message)
    {
        if (string.IsNullOrEmpty(message))
        {
            ErrorBar.IsOpen = false;
            App.CurrentErrorMessage = null;
        }
        else
        {
            ErrorBar.Message = message;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = message;
        }
    }
}
