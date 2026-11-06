using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_devices_machine;

namespace FaunaApp.Views;

/// <summary>
/// Settings → Sessions sub-page (<c>docs/goal/ui/sessions.md</c>; ui.yaml
/// § sessions): where the account is signed in right now, one escalation ladder —
/// the list (revoke one), sign out everywhere else, the 24-hour lock. The windows
/// render of the shared fold, twin of android's <c>SessionsScreen</c>, FaunaKit's
/// <c>SessionsView</c> and linux's <c>settings/sessions.rs</c>.
///
/// <para>A pure render over <see cref="SessionsViewModel"/>: it hydrates on the
/// nav edge and re-paints after each act, painting whatever page the face answers
/// (no fold, sort or keep-rows rule here). A landed lock leaves the shell through
/// the app's escalation — the same teardown and re-run launch a mid-session
/// session-ending verdict takes — whose re-run challenge meets the lock and lands
/// the locked notice (leg 2).</para>
///
/// <para>⚠ No <c>ConfigureAwait(false)</c> — a WinUI page that mutates bound state
/// off the UI thread throws a silent <c>COMException</c>.</para>
/// </summary>
public sealed partial class SettingsSessionsPage : Page, IAsyncLoadedPage
{
    private SessionsViewModel? _vm;
    private IReadOnlyList<SessionCard>? _paintedCards;

    // e2e nav-readiness barrier (IAsyncLoadedPage): completes once the first
    // hydrate has painted, success or handled error.
    private readonly TaskCompletionSource _loadComplete =
        new(TaskCreationOptions.RunContinuationsAsynchronously);
    Task IAsyncLoadedPage.LoadComplete => _loadComplete.Task;

    public SettingsSessionsPage()
    {
        InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        // A visit starts unpainted: the e2e wait on the painted ids must not pass
        // on an earlier visit's rows.
        SessionsViewModel.PaintedTokenIdsForTest = null;
        if (e.Parameter is not ServiceClients { Rpc: { } rpc } clients) return;
        var convSession = clients.ConvSession;
        var account = clients.Account;
        _vm = new SessionsViewModel(
            rpc,
            // The session's ONE Devices machine (Core.DevicesMachineHost): it names
            // device-minted sessions and owns the sealed-label render; the face
            // refreshes it inside each pass. A failed build is no reason to hide the
            // sessions — the fold then names a device key it cannot match neutrally.
            devices: async () =>
            {
                try
                {
                    return await DevicesMachineHost.GetOrBuildAsync(rpc, convSession);
                }
                catch (Exception ex)
                {
                    ShellLog.Warn(nameof(SettingsSessionsPage), $"[sessions] devices machine unavailable: {ex.Message}");
                    return null;
                }
            },
            ownDeviceId: () => account.ResolveDeviceId())
        {
            // The lock landed: every bearer is revoked and the nest mints no new
            // one, so leave the shell for launch (ui/sessions.md § User actions).
            Locked = () => App.EscalateToLaunchSurfaceHandler?.Invoke() ?? Task.CompletedTask,
        };
        _vm.PropertyChanged += Vm_PropertyChanged;
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        if (_vm is not null)
        {
            _vm.PropertyChanged -= Vm_PropertyChanged;
            _vm.Leave();
        }
        base.OnNavigatedFrom(e);
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        try
        {
            if (_vm is not null)
            {
                Render();
                await _vm.LoadAsync();
            }
        }
        finally
        {
            _loadComplete.TrySetResult();
        }
    }

    private void Vm_PropertyChanged(object? sender, PropertyChangedEventArgs e) => Render();

    private void Render()
    {
        if (_vm is not { } vm) return;

        if (!ReferenceEquals(_paintedCards, vm.Cards))
        {
            _paintedCards = vm.Cards;
            CardsList.ItemsSource = vm.Cards.Select(c => new SessionCardRow(c)).ToList();
        }
        EmptyText.Visibility = vm.ShowEmpty ? Visibility.Visible : Visibility.Collapsed;

        RevokeOthersConfirmPair.Visibility = vm.RevokeOthersArmed ? Visibility.Visible : Visibility.Collapsed;
        RevokeOthersButton.IsEnabled = !vm.Busy;

        if (LockWordBox.Text != vm.LockWord) LockWordBox.Text = vm.LockWord;
        LockButton.IsEnabled = vm.LockEnabled;

        if (vm.PageError is { Length: > 0 } error)
        {
            ErrorBar.Message = error;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = error;
        }
        else
        {
            ErrorBar.IsOpen = false;
            App.CurrentErrorMessage = null;
        }
    }

    private async void Revoke_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is { } vm && sender is FrameworkElement { Tag: SessionCardRow row })
            await vm.RevokeAsync(row.TokenId);
    }

    private void RevokeOthers_Click(object sender, RoutedEventArgs e) => _vm?.ArmRevokeOthers();

    private void RevokeOthersCancel_Click(object sender, RoutedEventArgs e) => _vm?.CancelRevokeOthers();

    private async void RevokeOthersConfirm_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is { } vm) await vm.ConfirmRevokeOthersAsync();
    }

    private void LockWordBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_vm is { } vm) vm.LockWord = LockWordBox.Text;
    }

    /// <summary>The face re-checks the word (the action arm), so a press that
    /// reached here past a disabled render still cannot lock on a wrong one.</summary>
    private async void Lock_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is { } vm) await vm.LockAsync();
    }

    /// <summary>One card's binding row — the VM's card plus the two visibilities
    /// a classic <c>{Binding}</c> cannot convert from a bool or a null.</summary>
    private sealed record SessionCardRow(SessionCard Card)
    {
        public string TokenId => Card.TokenId;
        public string Kind => Card.Kind;
        public string Mark => Card.Mark ?? string.Empty;
        public string Detail => Card.Detail;
        public Visibility MarkVisibility => Card.Mark is null ? Visibility.Collapsed : Visibility.Visible;
        public Visibility RevokeVisibility => Card.CanRevoke ? Visibility.Visible : Visibility.Collapsed;
    }
}
