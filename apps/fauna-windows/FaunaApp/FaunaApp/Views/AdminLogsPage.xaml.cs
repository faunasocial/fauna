using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Helpers;

namespace FaunaApp.Views;

/// <summary>
/// The admin Logs page (observability.md § Surfaces) — a dumb renderer of the shared
/// <see cref="AdminLogsViewModel"/> (FaunaApp.Core, over the
/// <see cref="INestRpcClient"/> WS-RPC seam → <c>FfiAdminClient.Logs</c> /
/// <c>fauna.admin.logs</c>). Fetches the nest's <c>fauna-log</c> ring once on load and
/// renders it with the SAME <see cref="LogsFormat"/> / <see cref="LogRow"/> shape as
/// the client's own Settings → Logs page; the severity filter narrows the held source
/// in memory (no refetch). NO clear — there is no admin RPC to wipe the nest ring.
/// admin-nav-back is the shared shell PaneHeader button. Page-level failures route to
/// <c>error-message</c>. Mirrors linux <c>views/admin.rs build_admin_logs_page</c>.
/// </summary>
public sealed partial class AdminLogsPage : Page
{
    private AdminLogsViewModel? _vm;

    public AdminLogsPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _vm ??= new AdminLogsViewModel(clients.Rpc!);
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        LogList.ItemsSource = _vm.Entries;

        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            await _vm.LoadAsync();
            RenderError(_vm.Error);
        }
        catch (Exception ex)
        {
            RenderError(Strings.Error(ex));
        }
        finally
        {
            LoadProgress.IsActive = false;
            LoadProgress.Visibility = Visibility.Collapsed;
            UpdateEmpty();
        }
    }

    private void FilterCombo_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        // Fires during XAML init (SelectedIndex=0) before the VM exists — guard it.
        if (_vm is null) return;
        _vm.SetFilter(FilterCombo.SelectedIndex);
        UpdateEmpty();
    }

    private void CopyButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        ClipboardHelper.CopyText(_vm.CopyText());
    }

    private void UpdateEmpty()
    {
        if (_vm is null) return;
        EmptyPlaceholder.Visibility = _vm.Entries.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private void RenderError(string? msg)
    {
        if (string.IsNullOrEmpty(msg))
        {
            ErrorBar.IsOpen = false;
        }
        else
        {
            ErrorBar.Message = msg;
            ErrorBar.IsOpen = true;
        }
    }
}
