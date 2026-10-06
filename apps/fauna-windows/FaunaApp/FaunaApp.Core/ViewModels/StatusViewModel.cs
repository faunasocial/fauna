using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// Displays service status: identity/quota/bridges from the nest HTTP+WS-RPC
/// APIs, plus the real per-device sync state read from the local sync-agent over the
/// shared FFI surface (see <see cref="IAgentStatusProbe"/> — DirectNestClient's own Sync
/// sub-object is a nest-side placeholder it has no way to answer for real).
/// </summary>
public partial class StatusViewModel : ViewModelBase
{
    // ── Nest status ──
    [ObservableProperty] private string? _actorId;
    [ObservableProperty] private string? _handle;
    [ObservableProperty] private bool _nestAvailable;

    // ── Sync status ──
    [ObservableProperty] private string? _syncVersion;
    [ObservableProperty] private ulong _syncUptimeSecs;
    [ObservableProperty] private ConnectionState _syncConnectionState;
    [ObservableProperty] private bool _syncConnected;
    [ObservableProperty] private bool _syncing;
    [ObservableProperty] private ulong _filesPending;
    [ObservableProperty] private ulong _bytesPending;
    [ObservableProperty] private ulong? _lastSync;

    // ── Bridge status ──
    [ObservableProperty] private bool _bridgeAvailable;
    [ObservableProperty] private bool _bridgeConnected;
    [ObservableProperty] private uint _bridgeAccountsActive;

    // ── Quota (fauna.quota.get) — the Status sub-page carries the live quota
    // breakdown the e2e (`quota-inbox`/`quota-storage`/`quota-devices`) reads
    // without navigating, settings.md § Navigation model. Pre-formatted
    // "used / max" strings so the dumb view binds them straight to TextBlocks
    // (mirrors the android Account screen + the windows SettingsViewModel storage
    // fetch). ── ──
    [ObservableProperty] private string _quotaInbox = "--";
    [ObservableProperty] private string _quotaStorage = "--";
    [ObservableProperty] private string _quotaDevices = "--";

    // ── Feature limits (fauna.features.status via FfiFeaturesClient.Rows) —
    // the controversial-class feature plane's transparency read
    // (dynamic-features.md § Transparency & auditability), placed directly
    // after Quota as its sibling "what bounds me" surface (settings.md §
    // Layout & flow item 2b). Every judgement (bounds, headroom, per-cell
    // tier attribution, available/disabled/hidden) is shared-Rust output —
    // this VM does no folding, only the hidden-row filter every other app
    // also applies at its own render boundary (never in Rust). `null` until
    // the read resolves (or fails), distinguishing "not loaded yet" from "a
    // nest with zero gated features" (an empty-but-non-null list) — the page
    // uses this to gate FeatureLimitsSection's visibility.
    //
    // Plain property, NOT [ObservableProperty]: StatusViewModel is public but
    // FfiFeatureRow is UniFFI-internal (CS0053 — a public generated property
    // can't expose an internal type), and this page reads it synchronously
    // from UpdateDisplay() right after LoadCommand completes anyway, exactly
    // like the Quota fields above, which take the same direct-read path
    // despite ALSO being [ObservableProperty] — no PropertyChanged subscriber
    // needs this one specifically. ──
    internal IReadOnlyList<FfiFeatureRow>? FeatureLimitRows { get; private set; }

    // ── General ──
    [ObservableProperty] private bool _isLoading;

    private readonly INestHttpClient _nest;
    // Identity rides the WS-RPC façade (`fauna.account.get`); the rest of the
    // status panel still polls the HTTP surfaces.
    private readonly INestRpcClient _rpc;
    // The LOCAL per-device sync agent (mirrors MainViewModel's established
    // sync-agent-status pattern) — DirectNestClient's Sync sub-object is a
    // nest-side placeholder with no visibility into this device's real sync
    // state, so Connected/Syncing/FilesPending/BytesPending below come from
    // here, not from `_nest.GetServiceStatusAsync()`.
    private readonly IAgentStatusProbe _agentStatus;

    internal StatusViewModel(INestHttpClient nest, INestRpcClient rpc, IAgentStatusProbe agentStatus)
    {
        _nest = nest;
        _rpc = rpc;
        _agentStatus = agentStatus;
    }

    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        ErrorMessage = null;
        try
        {
            // Query nest via HTTP
            try
            {
                NestAvailable = await _nest.IsAvailableAsync();
                if (NestAvailable)
                {
                    var identity = await _rpc.GetIdentityAsync();
                    ActorId = identity?.ActorId;
                    Handle = identity?.Handle;
                }
            }
            catch
            {
                NestAvailable = false;
            }

            // Query service status from nest for version/uptime/connection.
            try
            {
                var status = await _nest.GetServiceStatusAsync();
                SyncVersion = status.Version;
                SyncUptimeSecs = status.UptimeSecs;
                SyncConnectionState = status.Connection;
            }
            catch
            {
                // Service status not available — leave defaults
            }

            // Query the LOCAL sync agent directly for the real per-device
            // Connected/Syncing/FilesPending/BytesPending signal (best-effort —
            // null when the agent isn't running, leaving the defaults above).
            // FilesPending/BytesPending ride the shared fauna-sync-agent's own
            // per-set queue-depth projection (aggregate_backlog,
            // bins/fauna-sync-agent/src/pipe_server.rs) — real numbers, not
            // windows-invented ones.
            var agentSync = await _agentStatus.SyncStatusAsync();
            if (agentSync is not null)
            {
                SyncConnected = agentSync.@connected;
                Syncing = agentSync.@syncing;
                FilesPending = agentSync.@filesPending;
                BytesPending = agentSync.@bytesPending;
                LastSync = agentSync.@lastSync;
            }

            // Query bridge status from nest via `fauna.bridges.list`. A linked
            // bridge is an active account (the WS-RPC roster has no separate
            // `connected` flag — `linked` is the closest semantic).
            try
            {
                var bridges = await _rpc.BridgesListAsync();
                BridgeAvailable = true;
                uint activeCount = 0;
                foreach (var bridge in bridges)
                {
                    if (bridge.Linked)
                        activeCount++;
                }
                BridgeConnected = activeCount > 0;
                BridgeAccountsActive = activeCount;
            }
            catch
            {
                BridgeAvailable = false;
            }

            // Query quota over WS-RPC (`fauna.quota.get`) — the tier-aware
            // usage breakdown the Status sub-page's quota-section displays
            // (inbox / storage / devices). Mirrors the SettingsViewModel storage
            // fetch + the android Account screen's "used / max" cells.
            try
            {
                var quota = await _rpc.QuotaGetAsync();
                QuotaInbox = $"{ValueFormat.ByteSize((ulong)quota.inbox.usedBytes)} / {ValueFormat.ByteSize((ulong)quota.inbox.maxBytes)}";
                QuotaStorage = $"{ValueFormat.ByteSize((ulong)quota.storage.usedBytes)} / {ValueFormat.ByteSize((ulong)quota.storage.maxBytes)}";
                QuotaDevices = $"{quota.devices.used} / {quota.devices.max}";
            }
            catch
            {
                // Quota may not be available — leave defaults ("--").
            }

            // Feature limits (dynamic-features.md § Transparency & auditability):
            // the whole transparency-read surface in one call, already folded
            // into rows. Filter `hidden` here (client-side, never in Rust) — a
            // member whose capability token this nest build doesn't advertise
            // at all, matching every other app's render boundary (linux
            // `update_features`, tui `root.rs`).
            try
            {
                var rows = await _rpc.FeaturesRowsAsync();
                FeatureLimitRows = rows.Where(r => r.@affordance != "hidden").ToList();
            }
            catch
            {
                // Feature-limits read failed (e.g. a transport error) —
                // leave FeatureLimitRows null so the section stays hidden rather
                // than painting an empty/stale surface.
            }
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// <summary>Pure last-sync → i18n text mapping (<c>status/sync/last_sync</c> row).
    /// null (the agent has never finished a transfer/clean pass) ⇒ the shared "Never"
    /// baseline; a real reading (unix seconds) renders through the shared
    /// <see cref="ValueFormat.RelativeTime"/> (epoch ms — hence the ×1000). Static +
    /// pure so unit tests pin the mapping without a live FFI read (<paramref
    /// name="nowMs"/> injected for determinism). Mirrors
    /// <c>BackupsViewModel.LastUploadText</c>/<c>LastAuditText</c>'s shape.</summary>
    internal static string LastSyncText(ulong? lastSync, long nowMs) =>
        lastSync is { } secs
            ? ValueFormat.RelativeTime(nowMs, (long)secs * 1000)
            : Strings.Get("common/never");
}
