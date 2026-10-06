using System;
using System.ComponentModel;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views;

/// <summary>
/// The flat <c>admin-contacts</c> page (admin.md § Contacts; carddav-server.md
/// § Independent enablement; ui.yaml <c>admin-contacts</c>) — the deployment-wide
/// CardDAV-enable toggle, the contacts sibling of <see cref="AdminCalendarPage"/>'s
/// CalDAV-enable toggle. A dumb renderer of the shared
/// <c>fauna_client_mail_settings::CarddavPolicyMachine</c> through the testable
/// <see cref="AdminContactsViewModel"/> (FaunaApp.Core), consumed over the UniFFI
/// <c>ICarddavPolicyMachine</c> seam. The page builds the real machine over the
/// shared, already-connected + auto-reconnecting WS-RPC client
/// (<see cref="INestRpcClient.BuildCarddavPolicyMachineAsync"/>, the
/// <see cref="AdminCalendarPage"/> pattern), hands it to the VM, then reflects the VM's
/// <c>CarddavEnabled</c> flag into the named toggle and forwards the toggle gesture.
/// All projection / WS-RPC sequencing lives in shared Rust (priority #2): the
/// <c>get_mail_config</c> hydrate (its <c>carddav_enabled</c>) + the
/// <c>set_carddav_enabled</c> write + the re-read-after-write. No
/// <c>ConfigureAwait(false)</c> in these handlers (off-thread bound-state mutation
/// throws a silent COMException). The <c>admin-nav-back</c> affordance lives in the
/// shared AdminShell header, NOT here. No port field — CardDAV rides the shared DAV
/// listener admin-calendar's port field governs. Mirrors AdminCalendarPage (machine
/// build over the shared client + the <c>_syncing</c> render guard), minus the port
/// StackPanel.
/// </summary>
public sealed partial class AdminContactsPage : Page
{
    private ServiceClients? _clients;
    private AdminContactsViewModel? _vm;

    // Set while RenderState programmatically updates the toggle (whose change
    // handler dispatches), so reflecting persisted state never echoes back as an
    // action — mirrors AdminCalendarPage's toggle guard.
    private bool _syncing;

    public AdminContactsPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _clients = clients;
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_clients is null) return;

        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            if (_vm is null)
            {
                await EnsureViewModelAsync();
            }
            await _vm!.LoadCommand.ExecuteAsync(null);
            RenderState();
        }
        catch (Exception ex)
        {
            RenderError(Strings.Error(ex));
        }
        finally
        {
            LoadProgress.IsActive = false;
            LoadProgress.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>Build the CardDAV-policy machine over the SHARED, already-connected +
    /// auto-reconnecting WS-RPC client (the <see cref="AdminCalendarPage"/> pattern) and
    /// wrap it in the VM. The <c>get_mail_config</c> read + <c>set_carddav_enabled</c>
    /// write kinds are Admin-class; the shared connection authenticates as the
    /// logged-in actor, which on this (am_i_admin-gated) page is the admin. Reusing
    /// the shared client avoids a per-page one-shot FfiNestClient whose transient
    /// connect failure would surface as a page error while other pages recover.</summary>
    private async Task EnsureViewModelAsync()
    {
        if (_vm is not null || _clients?.Rpc is null) return;
        var machine = await _clients.Rpc.BuildCarddavPolicyMachineAsync();
        _vm = new AdminContactsViewModel(machine);
        _vm.PropertyChanged += ViewModel_PropertyChanged;
    }

    private void ViewModel_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (_vm is null) return;
        switch (e.PropertyName)
        {
            case nameof(AdminContactsViewModel.IsLoading):
                LoadProgress.IsActive = _vm.IsLoading;
                LoadProgress.Visibility = _vm.IsLoading ? Visibility.Visible : Visibility.Collapsed;
                break;
            case nameof(AdminContactsViewModel.Error):
                RenderError(_vm.Error);
                break;
        }
    }

    /// <summary>Reflect the VM's projected snapshot into the toggle. Called after the
    /// load + every toggle. The toggle is wrapped in the <c>_syncing</c> guard so
    /// reflecting persisted state never echoes back a dispatch.</summary>
    private void RenderState()
    {
        if (_vm is null) return;

        RenderError(_vm.Error);

        _syncing = true;
        EnabledToggle.IsOn = _vm.CarddavEnabled;
        _syncing = false;
    }

    /// <summary>Render the VM's last action error onto the app-wide
    /// <c>error-message</c> InfoBar + the state-protocol error surface.</summary>
    private void RenderError(string? msg)
    {
        var empty = string.IsNullOrEmpty(msg);
        ErrorBar.Message = msg ?? string.Empty;
        ErrorBar.IsOpen = !empty;
        ErrorTextMirror.Text = msg ?? " ";
        App.CurrentErrorMessage = empty ? null : msg;
    }

    // ── CardDAV-enable master toggle (set_carddav_enabled) ──
    private async void EnabledToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_syncing || _vm is null || sender is not ToggleSwitch sw) return;
        await _vm.SetCarddavEnabledAsync(sw.IsOn);
        RenderState();
    }
}
