using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;

namespace FaunaApp.Views;

/// <summary>
/// Admin → Web page (web-content-hosting.md § Admin apex hosting). Builds its own WS-RPC
/// <see cref="FfiNestClient"/> from the page's secrets (like AdminMailPage / AdminDnsPage),
/// wraps it in the shared <c>FfiWebClient</c>, and renders the testable
/// <see cref="AdminWebViewModel"/>: the apex-actor picker. The actor list is every account
/// on the nest (<c>fauna_client_admin::users_list_all</c> over FfiNestClient.Admin), reused
/// by the VM via an injected fetch — the same source the AdminDnsPage catch-all picker
/// uses.
/// </summary>
public sealed partial class AdminWebPage : Page
{
    private ServiceClients? _clients;
    private AdminWebViewModel? _vm;
    private bool _suppressSelection;

    public AdminWebPage()
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

    private async void Page_Loaded(object sender, RoutedEventArgs e) => await LoadAsync();

    private async Task LoadAsync()
    {
        if (_clients is null) return;
        try
        {
            await EnsureViewModelAsync();
            await _vm!.LoadAsync();
            RenderState();
        }
        catch (Exception ex)
        {
            ShowError(Strings.Error(ex));
        }
    }

    private async Task EnsureViewModelAsync()
    {
        if (_vm is not null || _clients?.Rpc is null) return;
        var web = await _clients.Rpc.BuildWebClientAsync();
        var domain = _clients.Account.Domain ?? string.Empty;
        var rpc = _clients.Rpc;
        _vm = new AdminWebViewModel(web, () => LoadActorsAsync(rpc), domain);
    }

    /// <summary>Project every account on the nest into the apex picker's actor options
    /// (id + label) — the same call + shape the AdminDnsPage catch-all picker uses, over
    /// the shared WS-RPC client (the <see cref="INestRpcClient"/> seam).</summary>
    private static async Task<IReadOnlyList<ApexActorOption>> LoadActorsAsync(INestRpcClient rpc)
    {
        var users = await rpc.AdminUsersListAllAsync();
        var list = new List<ApexActorOption>(users.Length);
        foreach (var u in users)
        {
            list.Add(new ApexActorOption(u.actorId, AdminActorOptions.Label(u)));
        }
        return list;
    }

    private void RenderState()
    {
        if (_vm is null) return;
        // Guard suppresses the SelectionChanged re-fire from the programmatic ItemsSource +
        // SelectedIndex set (the render-time echo).
        _suppressSelection = true;
        ApexSelect.ItemsSource = _vm.ApexOptions;
        ApexSelect.SelectedIndex = _vm.ApexSelectedIndex;
        _suppressSelection = false;
        ApexInfo.Text = _vm.ApexInfoText;
        ShowError(_vm.Error);
    }

    private async void ApexSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_suppressSelection || _vm is null) return;
        var idx = ApexSelect.SelectedIndex;
        if (idx < 0 || idx == _vm.ApexSelectedIndex) return; // render-time echo
        await _vm.SelectApexAsync(idx);
        RenderState();
    }

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
