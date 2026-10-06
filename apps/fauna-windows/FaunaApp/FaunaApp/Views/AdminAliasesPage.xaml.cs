using System;
using System.ComponentModel;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// Admin page: external mail forwarders (<c>admin-aliases</c>, admin.md § 4 /
/// mail-aliases.md § Kind 7). A dumb renderer of the shared <c>ForwarderMachine</c>
/// (libs/fauna-client-mail-settings, exposed over UniFFI by
/// <c>libs/fauna-ffi/src/mail_admin.rs</c> <c>build_forwarders_machine</c>) through
/// the testable <see cref="AdminAliasesViewModel"/> (FaunaApp.Core). The page builds
/// the machine over the shared, already-connected WS-RPC client (the
/// <see cref="INestRpcClient"/> seam) and hands it to the VM, then binds the VM's
/// snapshot rows / picker domains and forwards the add/delete gestures. The VM owns
/// everything else (priority #2): the WS-RPC
/// (<c>fauna.bridges.{list,create,delete}_forwarder</c> + <c>list_local_domains</c>),
/// the <c>&lt;pattern&gt;@&lt;domain&gt;</c> render, the hex alias-id round-trip, and
/// the refresh-then-act sequencing. Catch-all designation (Kind 4) is deferred — it
/// rides <c>admin-dns</c>, not this page. No <c>ConfigureAwait(false)</c> in the VM
/// or these handlers (off-thread bound-state mutation throws a silent COMException).
/// Prior art: <see cref="AdminUsersPage"/> (VM-driven indexed list) +
/// <see cref="AdminDnsPage"/> (machine build over the shared client) +
/// apps/fauna-linux/src/views/admin.rs (build_admin_aliases_page).
/// </summary>
public sealed partial class AdminAliasesPage : Page
{
    private ServiceClients? _clients;
    private AdminAliasesViewModel? _vm;

    public AdminAliasesPage()
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
            UpdateForwardersEmpty();
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

    /// <summary>Build the forwarder machine over the shared, already-connected,
    /// auto-reconnecting WS-RPC client (the <see cref="INestRpcClient"/> seam,
    /// mirrors AdminMailPage) and wrap it in the VM. Riding the shared client —
    /// rather than a one-shot per-page <c>FfiNestClient.Connect()</c> — means a
    /// transient WS blip recovers instead of pinning the page in an error.</summary>
    private async Task EnsureViewModelAsync()
    {
        if (_vm is not null || _clients?.Rpc is null) return;
        var machine = await _clients.Rpc.BuildForwardersMachineAsync();
        _vm = new AdminAliasesViewModel(machine);
        _vm.PropertyChanged += ViewModel_PropertyChanged;
        _vm.Forwarders.CollectionChanged += (_, _) => UpdateForwardersEmpty();
        ForwardersList.ItemsSource = _vm.Forwarders;
        DomainSelect.ItemsSource = _vm.Domains;
    }

    private void ViewModel_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (_vm is null) return;
        switch (e.PropertyName)
        {
            case nameof(AdminAliasesViewModel.IsLoading):
                LoadProgress.IsActive = _vm.IsLoading;
                LoadProgress.Visibility = _vm.IsLoading ? Visibility.Visible : Visibility.Collapsed;
                break;
            case nameof(AdminAliasesViewModel.Error):
                RenderError(_vm.Error);
                break;
        }
    }

    private void DeleteForwarderButton_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is Button btn) btn.Content = S.Get("admin/aliases_page/delete_forwarder");
    }

    /// <summary>Create the forwarder from the add form (picked hosted domain +
    /// local-part + external target). The VM no-ops a blank field; on success the
    /// machine re-reads so the new row appears, and the inputs clear (a guard no-op /
    /// rejection leaves them so the user can fix).</summary>
    private async void AddForwarderSubmit_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        var domain = DomainSelect.SelectedItem as string;
        var pattern = PatternInput.Text;
        var target = TargetInput.Text;
        await _vm.CreateAsync(domain, pattern, target);
        if (_vm.Error is null
            && !string.IsNullOrWhiteSpace(domain)
            && !string.IsNullOrWhiteSpace(pattern)
            && !string.IsNullOrWhiteSpace(target))
        {
            PatternInput.Text = string.Empty;
            TargetInput.Text = string.Empty;
        }
        UpdateForwardersEmpty();
    }

    private async void DeleteForwarder_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button { DataContext: ForwarderRowVm row }) return;
        await _vm.DeleteAsync(row.AliasIdHex);
        UpdateForwardersEmpty();
    }

    /// <summary>Render the VM's last action error onto the dedicated
    /// <c>admin-aliases-action-error</c> element (admin.md § 4) and the app-wide
    /// error surface (the state-protocol <c>messages.error</c> read).</summary>
    private void RenderError(string? msg)
    {
        var empty = string.IsNullOrEmpty(msg);
        ActionError.Text = msg ?? string.Empty;
        ActionError.Visibility = empty ? Visibility.Collapsed : Visibility.Visible;
        App.CurrentErrorMessage = empty ? null : msg;
    }

    private void UpdateForwardersEmpty()
    {
        if (_vm is null) return;
        NoForwardersText.Visibility =
            _vm.Forwarders.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }
}
