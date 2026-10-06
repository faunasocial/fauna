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
/// The flat <c>admin-calendar</c> page (admin.md § 8 Calendar; caldav-server.md
/// § Independent enablement; ui.yaml <c>admin-calendar</c>) — the deployment-wide
/// CalDAV-enable toggle, the sibling of <see cref="AdminMailPage"/>'s mail-enable
/// toggle. A dumb renderer of the shared
/// <c>fauna_client_mail_settings::CaldavPolicyMachine</c> through the testable
/// <see cref="AdminCalendarViewModel"/> (FaunaApp.Core), consumed over the UniFFI
/// <c>ICaldavPolicyMachine</c> seam. The page builds the real machine over the
/// shared, already-connected + auto-reconnecting WS-RPC client
/// (<see cref="INestRpcClient.BuildCaldavPolicyMachineAsync"/>, the
/// <see cref="AdminMailPage"/> pattern), hands it to the VM, then reflects the VM's
/// <c>CaldavEnabled</c> flag into the named toggle and forwards the toggle gesture.
/// All projection / WS-RPC sequencing lives in shared Rust (priority #2): the
/// <c>get_mail_config</c> hydrate (its <c>caldav_enabled</c>) + the
/// <c>set_caldav_enabled</c> write + the re-read-after-write. No
/// <c>ConfigureAwait(false)</c> in these handlers (off-thread bound-state mutation
/// throws a silent COMException). The <c>admin-nav-back</c> affordance lives in the
/// shared AdminShell header, NOT here. Lifts the flat linux reference
/// apps/fauna-linux/src/settings/admin_calendar.rs; mirrors AdminMailPage (machine
/// build over the shared client + the <c>_syncing</c> render guard).
/// </summary>
public sealed partial class AdminCalendarPage : Page
{
    private ServiceClients? _clients;
    private AdminCalendarViewModel? _vm;

    // Set while RenderState programmatically updates the toggle (whose change
    // handler dispatches), so reflecting persisted state never echoes back as an
    // action — mirrors AdminMailPage's mail-enable guard.
    private bool _syncing;

    public AdminCalendarPage()
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

    /// <summary>Build the CalDAV-policy machine over the SHARED, already-connected +
    /// auto-reconnecting WS-RPC client (the <see cref="AdminMailPage"/> pattern) and
    /// wrap it in the VM. The <c>get_mail_config</c> read + <c>set_caldav_enabled</c>
    /// write kinds are Admin-class; the shared connection authenticates as the
    /// logged-in actor, which on this (am_i_admin-gated) page is the admin. Reusing
    /// the shared client avoids a per-page one-shot FfiNestClient whose transient
    /// connect failure would surface as a page error while other pages recover.</summary>
    private async Task EnsureViewModelAsync()
    {
        if (_vm is not null || _clients?.Rpc is null) return;
        var machine = await _clients.Rpc.BuildCaldavPolicyMachineAsync();
        _vm = new AdminCalendarViewModel(machine);
        _vm.PropertyChanged += ViewModel_PropertyChanged;
    }

    private void ViewModel_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (_vm is null) return;
        switch (e.PropertyName)
        {
            case nameof(AdminCalendarViewModel.IsLoading):
                LoadProgress.IsActive = _vm.IsLoading;
                LoadProgress.Visibility = _vm.IsLoading ? Visibility.Visible : Visibility.Collapsed;
                break;
            case nameof(AdminCalendarViewModel.Error):
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
        EnabledToggle.IsOn = _vm.CaldavEnabled;
        // Reflect the persisted port (or, after an invalid save, the typed value the
        // VM left in place). The TextBox doesn't dispatch on edit, so it's outside the
        // guard's concern — kept inside for symmetry with the toggle.
        CaldavPortBox.Text = _vm.CaldavPort;
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

    // ── CalDAV-enable master toggle (set_caldav_enabled) ──
    private async void EnabledToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_syncing || _vm is null || sender is not ToggleSwitch sw) return;
        await _vm.SetCaldavEnabledAsync(sw.IsOn);
        RenderState();
    }

    // ── Admin-set CalDAV port (set_caldav_port) ──
    private async void SaveCaldavPortButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.CaldavPort = CaldavPortBox.Text.Trim();
        await _vm.SaveCaldavPortAsync();
        RenderState();
    }
}
