using System.Collections.ObjectModel;
using System.Linq;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// Root ViewModel that owns navigation state and connection status.
/// Uses NestHttpClient for identity queries.
/// </summary>
public partial class MainViewModel : ObservableObject
{
    [ObservableProperty] private string _currentPage = "Status";
    [ObservableProperty] private bool _isConnected;
    [ObservableProperty] private bool _needsOnboarding;
    [ObservableProperty] private string? _actorId;
    [ObservableProperty] private string? _handle;

    // Global connection-status indicator (transport.md § Connection-status
    // indicator): the live transport ConnectionState mapped to a localized label,
    // bound by MainPage's pane-header `connection-status` element. Defaults to
    // "Connecting…" until the pump delivers the current state.
    [ObservableProperty] private string _connectionStatus = Strings.Get("common/connecting");

    // Global sync-agent-status indicator (sync-agent.md § Local agent health): the
    // LOCAL fauna-sync-agent process's own health, sibling to connection-status but
    // a distinct concept (that one is the nest WS-RPC link; this one is whether the
    // per-user background sync helper is alive). Defaults to "Not running" until the
    // first poll lands — matches the real pre-poll state (no agent contacted yet).
    [ObservableProperty] private string _syncAgentStatusText = Strings.Get("status/sync_agent/not_running");
    [ObservableProperty] private string _syncAgentStatusVersionText = string.Empty;
    [ObservableProperty] private string _syncAgentStatusUptimeText = string.Empty;

    // Global critical-alerts banner (critical-alerts.md § Rendering contract):
    // one resolved, joined-lines string per active alert row, bound by
    // MainPage's `critical-alerts`/`critical-alert` ItemsControl. Empty ⇒ the
    // banner is absent — the whole presence rule, mirrored from
    // CriticalAlertsHost.Active() with no second signal.
    [ObservableProperty] private ObservableCollection<string> _activeCriticalAlerts = new();

    // Global unread-notification count (`notifications.md` § Layout & flow; the
    // `data.notifications.unread_count` state field ui.yaml declares). APP-GLOBAL,
    // not page-scoped, and that is the whole point: a `fauna.notification` push has
    // to move this while the user is on ANY page — which is what linux keeps in
    // `app.notifications_unread_count`, updated from its push arm regardless of the
    // visible page. Windows had the snapshot setter (`AppDataSnapshot.SetUnreadCount`)
    // from the state-protocol Phase 1 commit onward and NO caller for it, so the
    // declared state field answered a hard-coded 0 forever — a dishonest surface that
    // read as "the push never arrived" . The page's own
    // `NotificationsViewModel.UnreadCount` is unchanged and still drives the page
    // badge; this is the shell-lifetime twin that outlives any one page.
    [ObservableProperty] private long _unreadNotificationCount;

    // Identity now rides the WS-RPC façade (`fauna.account.get`); the HTTP
    // `/api/v1/account` twin was deleted nest-side.
    private readonly INestRpcClient _rpc;
    private readonly IAgentStatusProbe _agentHealth;

    internal MainViewModel(INestRpcClient rpc, IAgentStatusProbe agentHealth)
    {
        _rpc = rpc;
        _agentHealth = agentHealth;
        // Global connection-status indicator (transport.md § Connection-status
        // indicator). Subscribe BEFORE starting the idempotent pump so the first
        // raise — which carries the current state — isn't missed. The pump marshals
        // ConnectionStateChanged onto the UI thread, so the bound-state mutation is
        // thread-safe. This is the app-lifetime shell VM (constructed once per login
        // in MainPage.OnNavigatedTo), so no unsubscribe is needed.
        _rpc.ConnectionStateChanged += OnConnectionStateChanged;
        _rpc.StartConnectionStatePump();

        // Global critical-alerts banner. CriticalAlertsHost.Instance is a
        // process-wide singleton built once and reused across every login/
        // account-switch (never re-subscribed per MainViewModel construction),
        // so this only adds a listener for THIS shell VM's lifetime — same
        // no-unsubscribe posture as ConnectionStateChanged above. Read the
        // CURRENT state immediately: an alert may already be active (e.g. the
        // session-start sweep posted before this VM was built).
        // Latch the UI context FIRST. The host may already have been built on a
        // pool thread (the e2e state provider's sweep-pass read, or ActorScope's
        // ClearAll — both run before any MainViewModel exists), in which case it
        // captured no context in its own ctor and would otherwise invoke the
        // repaint on whatever thread `CriticalAlerts::post` calls it from. This
        // ctor runs on the UI thread, which makes it the right place to install
        // it.
        CriticalAlertsHost.CaptureUiContext();
        CriticalAlertsHost.Instance.Changed += OnCriticalAlertsChanged;
        RefreshCriticalAlerts();

        // Re-read the unread-notification count on every reconnect AND on every
        // `fauna.notification` / `ResyncRequired` push: windows funnels both onto
        // this one `Reconnected` fan-out (`NestRpcClient.DispatchPush`), so this is
        // the single arm that keeps the count live between page visits. Same
        // app-lifetime, no-unsubscribe posture as ConnectionStateChanged above.
        _rpc.Reconnected += OnReconnectedRefreshUnread;
    }

    private void OnReconnectedRefreshUnread() => _ = RefreshUnreadNotificationCountAsync();

    /// <summary>
    /// Re-read `fauna.notifications.count` into <see cref="UnreadNotificationCount"/>.
    /// Fully swallowed: the count is an indicator, and a failed read must never
    /// surface as an error banner over whatever page happens to be visible — the
    /// next push or reconnect re-reads it, and the shell's own load is the cold-start
    /// backstop.
    /// </summary>
    /// <remarks>No ConfigureAwait(false): the property mutation must land back on the
    /// UI thread or WinUI throws a silent COMException.</remarks>
    public async Task RefreshUnreadNotificationCountAsync()
    {
        try
        {
            UnreadNotificationCount = await _rpc.NotificationsCountAsync();
        }
        catch (Exception ex)
        {
            ShellLog.Debug("MainViewModel",
                $"unread notification count refresh failed: {ex.Message}");
        }
    }

    /// Re-read the registry and republish resolved, joined-lines rows
    /// (critical-alerts.md § Rendering contract: each row's lines render
    /// verbatim, client-joined). A NEW collection each time — simpler and
    /// thread-safer than in-place Add/Remove under the FFI callback's
    /// marshaled-but-still-async delivery.
    private void RefreshCriticalAlerts() =>
        ActiveCriticalAlerts = new ObservableCollection<string>(
            CriticalAlertsHost.Instance.Active()
                .Select(row => string.Join(" ", row.lines.Select(Strings.Resolve))));

    private void OnCriticalAlertsChanged() => RefreshCriticalAlerts();

    /// <summary>
    /// Render the tri-state sync-agent-status indicator (Running / Restart pending /
    /// Not running) from the agent's own health probe (`sync-agent.md` § Local agent
    /// health).
    ///
    /// <para>The derivation is <b>shared Rust</b> — `fauna_client_sync::agent::
    /// agent_health_state`, reached through `FfiSyncAgentProvisioner.AgentHealth` —
    /// so this method only maps a state to a label. It used to compare versions
    /// here, in a hand-written twin of linux's `update_sync_agent_status_indicator`;
    /// that is exactly the per-app divergence the cutover retires. The version passed
    /// in is still `fauna_ffi_build_version()`: the linked `fauna-ffi` crate's own
    /// `CARGO_PKG_VERSION`, which shares the workspace version with
    /// `fauna-sync-agent` — NOT the .NET assembly version, which is MSBuild-derived
    /// and unrelated to the Cargo workspace.</para>
    ///
    /// <para>Called by the View's ~10s DispatcherTimer (VMs in FaunaApp.Core can't
    /// reference DispatcherTimer directly — mirrors EventsViewModel's poll split).</para>
    /// No ConfigureAwait(false): the property mutations below must land back on
    /// the UI thread or WinUI throws a silent COMException.
    /// </summary>
    public async Task PollSyncAgentStatusAsync()
    {
        var status = await _agentHealth.ProbeAsync(FaunaFfiMethods.FaunaFfiBuildVersion());

        if (status.@state == FfiAgentHealthState.NotRunning)
        {
            SyncAgentStatusText = Strings.Get("status/sync_agent/not_running");
            SyncAgentStatusVersionText = string.Empty;
            SyncAgentStatusUptimeText = string.Empty;
            return;
        }

        SyncAgentStatusText = status.@state switch
        {
            FfiAgentHealthState.Running => Strings.Get("status/sync_agent/running"),
            FfiAgentHealthState.KeysPending => Strings.Get("status/sync_agent/keys_pending"),
            FfiAgentHealthState.NotEnrolled => Strings.Get("status/sync_agent/not_enrolled"),
            _ => Strings.Get("status/sync_agent/restart_pending"),
        };
        SyncAgentStatusVersionText = status.@version;
        SyncAgentStatusUptimeText = ValueFormat.DurationSecs(status.@uptimeSecs);
    }

    /// <summary>
    /// Drain-if-due and send the batched Guardian Notify report
    /// (family-safety.md § Guardian Notify), via <see
    /// cref="GuardianNotifyCache.CheckNowAsync"/> — a no-op when nothing is
    /// pending or the ≤hourly interval has not elapsed. Called by the View's
    /// ~10s DispatcherTimer (VMs in FaunaApp.Core can't reference
    /// DispatcherTimer directly — mirrors <see cref="PollSyncAgentStatusAsync"/>'s
    /// split); the short tick interval is safe because the real cadence gate is
    /// the accumulator's own <c>notify_report_min_interval_secs</c> check, not
    /// this tick.
    /// </summary>
    public Task CheckGuardianNotifyAsync() => GuardianNotifyCache.CheckNowAsync(_rpc);

    /// Map the live transport state to the localized indicator label through the
    /// single shared owner, `connectionStateLabel` — never a per-app switch.
    /// "Connecting…" (a transient swap / reconnect) is deliberately a normal state,
    /// never an error banner — transport.md § Connection-status indicator.
    /// <para>This is also the ONE place the offline-affordance gate learns the
    /// transport state (W4 (account-data-plane.md § Workstreams) phase 4, <c>account-data-plane.md</c> § The
    /// offline-mutation contract). Driving both from the same event is what makes
    /// it impossible for the indicator a user reads and the gate that greys their
    /// controls to disagree — and the word comes from the shared
    /// <c>ConnectionStateWord</c> export, never a C# switch over the enum.</para>
    private void OnConnectionStateChanged(FfiConnectionState state)
    {
        // Every value the indicator takes, repeats included — the
        // `connection_reports` stickiness counter (a no-op outside e2e builds).
        E2eLoudSurfaces.ObserveConnectionReport(state);
        ConnectionStatus = Strings.Resolve(FaunaFfiMethods.ConnectionStateLabel(state));
        OfflineGate.Shared.SetConnectionState(FaunaFfiMethods.ConnectionStateWord(state));
    }

    [RelayCommand]
    private async Task LoadAsync()
    {
        var identity = await _rpc.GetIdentityAsync();
        if (identity is not null)
        {
            ActorId = identity.ActorId;
            Handle = identity.Handle;
            NeedsOnboarding = false;
        }
        else
        {
            NeedsOnboarding = true;
        }

        // Cold-start read of the app-global unread count, so the surface is honest
        // before any push arrives (the push/reconnect arm above keeps it live after).
        await RefreshUnreadNotificationCountAsync();
    }

    [RelayCommand]
    private void NavigateTo(string page) => CurrentPage = page;
}
