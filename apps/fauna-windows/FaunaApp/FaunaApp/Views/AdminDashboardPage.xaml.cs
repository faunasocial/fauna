using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Views;

/// <summary>
/// Admin dashboard page showing nest statistics: user count, storage, inbox
/// messages, sessions. Only accessible to nest administrators.
/// </summary>
public sealed partial class AdminDashboardPage : Page
{
    private INestRpcClient? _rpc;

    public AdminDashboardPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _rpc = clients.Rpc;
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        await LoadStatsAsync();
    }

    private async Task LoadStatsAsync()
    {
        if (_rpc is null) return;

        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;

        try
        {
            // fauna.admin.stats over WS-RPC (replaces the deleted GET /admin/api/stats).
            var stats = await _rpc.AdminStatsAsync();
            PopulateStats(stats);
            // Version from fauna.admin.status — the WS-RPC source for the running
            // nest version (replaces the deleted GET /api/v1/node-info HTTP twin).
            var status = await _rpc.AdminStatusAsync();
            VersionValue.Text = status.version;
            ErrorBar.IsOpen = false;
            App.CurrentErrorMessage = null;
        }
        catch (Exception ex)
        {
            var msg = Strings.Error(ex);
            ErrorBar.Message = msg;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = msg;
        }
        finally
        {
            LoadProgress.IsActive = false;
            LoadProgress.Visibility = Visibility.Collapsed;
        }
    }

    // Map the typed `fauna.admin.stats` reply to the stat tiles, mirroring the
    // linux dashboard (apps/fauna-linux/src/views/admin.rs). Inbox stays the
    // "--" placeholder: the reply carries `total_inbox_bytes` (a size), not a
    // message COUNT, and we don't show bytes under a "messages" label. Byte sizes
    // defer to the shared Rust formatter (ValueFormat over fauna_core byte_size).
    private void PopulateStats(FfiAdminStats stats)
    {
        UsersValue.Text = stats.totalUsers.ToString();
        StorageValue.Text = ValueFormat.ByteSize((ulong)stats.totalStorageBytes);
        SessionsValue.Text = stats.wsConnections.ToString();
    }
}
