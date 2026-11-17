using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using uniffi.fauna_conversations;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// Displays and manages user settings: nest URL, handle, identity info.
/// Uses NestHttpClient for identity and configuration operations.
/// </summary>
public partial class SettingsViewModel : ViewModelBase
{
    [ObservableProperty] private string? _nestUrl;
    [ObservableProperty] private string? _handle;
    [ObservableProperty] private string? _actorId;
    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string _newNestUrl = string.Empty;
    [ObservableProperty] private string _newHandle = string.Empty;
    // Unknown is a distinct state from any real mode — never seed a default
    // (settings.md item 7, linux's original fix). LoadAsync's InboxModeGetAsync() fetch is the
    // only writer for the load path; a client that instead seeded "open" here
    // would paint a mode the user never chose whenever the fetch is slow, or
    // silently mask a fetch failure (the caught-and-swallowed catch block
    // below) as a plausible-looking answer instead of an honest unknown.
    [ObservableProperty] private string _inboxMode = "";
    [ObservableProperty] private bool _isSavingInboxMode;
    [ObservableProperty] private long _storageUsedBytes;
    [ObservableProperty] private long _storageTotalBytes;
    [ObservableProperty] private double _storagePercent;
    [ObservableProperty] private bool _isDeleting;
    // null = not yet hydrated (the section's honest bare-title state); empty =
    // hydrated with nothing scheduled; non-empty = the counted rows
    // (settings.md § Pending actions — the same three-state distinction every
    // other app's reference implementation carries).
    [ObservableProperty] private IReadOnlyList<PendingActionRowVm>? _pendingActions;
    // The STANDING unsent-changes section (account-offline-mutation.md § The
    // offline-mutation contract → The unsent-changes surface, point 1) —
    // pending actions' sibling and three-state shape: null = not read yet, empty =
    // read with nothing unsent, rows = the device outbox's undrained intents.
    [ObservableProperty] private IReadOnlyList<UnsentChangeRowVm>? _unsentChanges;
    [ObservableProperty] private bool _isAutoStartEnabled;
    [ObservableProperty] private bool _isCloseToTrayEnabled;
    [ObservableProperty] private int _keyPackageCount;
    [ObservableProperty] private bool _isMlsAvailable;
    // The Encryption page's read-only low flag: true once a count has been read
    // AND the shared threshold (fauna_conversations::keypackages_low, fewer
    // than five — direct-messages.md § Key Package Management) calls it low.
    // Never a local literal, and never true before hydration.
    [ObservableProperty] private bool _isKeyPackageLow;
    [ObservableProperty] private bool _isRefreshingKeyPackages;

    /// The key-package pool target a manual Refresh tops up to (the same 20
    /// the login-time replenish aims for).
    internal const ulong KeyPackageRefreshTarget = 20;

    // Persisted "Close to tray" preference (apps/windows.md § App Lifecycle).
    // A local file under %LocalAppData%\Fauna, read/written here and by the
    // TrayIconService close handler — no session needed, so it loads in the ctor.
    private readonly AppSettingsStore _appSettings = new();

    partial void OnIsCloseToTrayEnabledChanged(bool value)
        => _appSettings.CloseToTray = value;

    // A real user flip of the auto-start toggle: persist it as the EXPLICIT
    // tri-state choice (AutoStartGate never overrides an explicit opt-out) and
    // apply the Run-key change via AutoStartService (the one owner of the
    // registry mechanics — apps/windows.md § App Lifecycle → Auto-start at
    // sign-in). Load-time state is seeded through the backing field (LoadAsync),
    // so this only fires on genuine changes, never as a load echo that would
    // silently convert the default into a fake explicit choice.
    partial void OnIsAutoStartEnabledChanged(bool value)
    {
        _appSettings.AutoStartChoice = value;
        if (value)
        {
            AutoStartService.Register();
        }
        else
        {
            AutoStartService.Unregister();
        }
    }

    private readonly INestHttpClient _nest;
    // Account identity / quota / deletion ride the WS-RPC façade
    // (`fauna.account.get`, `fauna.quota.get`, `fauna.account.delete`); the
    // HTTP `/api/v1/account` twin was deleted nest-side.
    private readonly INestRpcClient _rpc;
    // The Export My Data save step (settings.md § Data export) — the SAME
    // seam single-file restore uses (native FileSavePicker in production, a
    // fixed e2e directory). Null on the three sibling settings sub-pages
    // (Privacy/General/Encryption) that construct this VM but never wire the
    // export button; ExportAccountDataAsync no-ops rather than fetching bytes
    // it cannot save.
    private readonly ISnapshotFileSaver? _fileSaver;
    // The shared session's manager — the Encryption page's Refresh rides its
    // durable ensure_keypackages (mints on the session's MlsEngine and notifies
    // the autosave observer; a raw RPC upload would strand the init keys).
    // Null on every other settings sub-page: RefreshKeyPackagesAsync no-ops.
    private readonly IConversationsManager? _keyPackageManager;

    // The device outbox's unsent-changes doors (account-offline-mutation.md § The
    // unsent-changes surface): the process outbox in production, a fake in a test.
    private readonly IUnsentChangesDoors _unsentDoors;

    internal SettingsViewModel(
        INestHttpClient nest,
        INestRpcClient rpc,
        ISnapshotFileSaver? fileSaver = null,
        IConversationsManager? keyPackageManager = null,
        IUnsentChangesDoors? unsentDoors = null)
    {
        _nest = nest;
        _rpc = rpc;
        _fileSaver = fileSaver;
        _keyPackageManager = keyPackageManager;
        _unsentDoors = unsentDoors ?? new DeviceUnsentChangesDoors();
        // Seed the backing field (not the property) so the initial read doesn't
        // re-persist; a local file, available without an authed session.
        _isCloseToTrayEnabled = _appSettings.CloseToTray;
        // Start-at-login shows the user's CHOICE (tri-state, default ON), from the same
        // local file — never the Run key's existence, which an e2e run never writes and
        // a fresh install has not written yet (AutoStartGate.ChoiceIsOn).
        _isAutoStartEnabled = AutoStartGate.ChoiceIsOn(_appSettings.AutoStartChoice);
    }

    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        ErrorMessage = null;
        try
        {
            var identity = await _rpc.GetIdentityAsync();
            if (identity is not null)
            {
                ActorId = identity.ActorId;
                Handle = identity.Handle;
                NestUrl = identity.NestUrl;
                NewNestUrl = identity.NestUrl ?? string.Empty;

                // Load inbox mode (fauna.inbox.mode.get over WS-RPC)
                try
                {
                    InboxMode = await _rpc.InboxModeGetAsync();
                }
                catch { /* inbox mode API may not be available */ }

                // Load quota (fauna.quota.get over WS-RPC)
                try
                {
                    var quota = await _rpc.QuotaGetAsync();
                    StorageUsedBytes = quota.storage.usedBytes;
                    StorageTotalBytes = quota.storage.maxBytes;
                    StoragePercent = FaunaFfiMethods.QuotaPercent(StorageUsedBytes, StorageTotalBytes);
                }
                catch { /* quota may not be available */ }

                // Auto-start is seeded in the constructor from the persisted choice
                // (a local file, no session needed) — not read back from the Run key
                // here. Re-announce it so a page bound before this load repaints.
                OnPropertyChanged(nameof(IsAutoStartEnabled));

                // Load MLS key package count
                try
                {
                    KeyPackageCount = await _rpc.KeypackageCountAsync();
                    IsMlsAvailable = true;
                    IsKeyPackageLow = FaunaConversationsMethods.KeypackagesLow((ulong)Math.Max(0, KeyPackageCount));
                }
                catch { IsMlsAvailable = false; }
            }
            else
            {
                SetError(Strings.Get("errors/no_identity"));
            }
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// Settings → Encryption "Refresh Keys": top the pool up through the shared
    /// session's <c>ensure_keypackages</c>, then re-read the count (and the low
    /// flag) from the nest. Manual only — the page never mints on load.
    [RelayCommand]
    private async Task RefreshKeyPackagesAsync()
    {
        if (_keyPackageManager is null) return;
        IsRefreshingKeyPackages = true;
        ErrorMessage = null;
        try
        {
            await _keyPackageManager.EnsureKeypackages(KeyPackageRefreshTarget);
            KeyPackageCount = await _rpc.KeypackageCountAsync();
            IsMlsAvailable = true;
            IsKeyPackageLow = FaunaConversationsMethods.KeypackagesLow((ulong)Math.Max(0, KeyPackageCount));
        }
        catch (Exception ex) { ShowError(ex); }
        finally { IsRefreshingKeyPackages = false; }
    }

    [RelayCommand]
    private async Task ConfigureAsync()
    {
        if (string.IsNullOrWhiteSpace(NewNestUrl))
            return;

        IsLoading = true;
        ErrorMessage = null;
        try
        {
            await _nest.ConfigureAsync(NewNestUrl);
            NestUrl = NewNestUrl;
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// Set the inbox-acceptance mode over WS-RPC (<c>fauna.inbox.mode.set</c>).
    /// <paramref name="mode"/> is one of the canonical tags the Settings UI emits
    /// (<c>open</c> / <c>allow_knock</c> / <c>contacts_only</c> / <c>closed</c>);
    /// the nest rejects anything else as invalid-params.
    [RelayCommand]
    private async Task SetInboxModeByNameAsync(string mode)
    {
        IsSavingInboxMode = true;
        ErrorMessage = null;
        try
        {
            await _rpc.InboxModeSetAsync(mode);
            InboxMode = mode;
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
        finally
        {
            IsSavingInboxMode = false;
        }
    }

    [RelayCommand]
    private async Task DeleteAccountAsync()
    {
        IsDeleting = true;
        ErrorMessage = null;
        try
        {
            await _rpc.AccountDeleteAsync();
            // Only SCHEDULES a 14-day cancellable pending action (settings.md §
            // Where logic lives → Account deletion) — no sign-out, no
            // credential/store erase, no navigation here. The re-listed
            // pending-actions row is the receipt; its cancel button is the way
            // back within the window.
            await LoadPendingActionsAsync();
        }
        catch (Exception ex) { ShowError(ex); }
        finally { IsDeleting = false; }
    }

    /// Settings → Account "change handle". Rides <c>fauna.profile.handle.change</c>
    /// (the authenticated bearer kind) — a queued, cancellable pending action, so
    /// the displayed <see cref="Handle"/> is NOT updated here; it refreshes on the
    /// next account load once the pending action executes (matching linux /
    /// apple). Replaces the prior abuse of the anonymous <c>register</c> route.
    [RelayCommand]
    private async Task ChangeHandleAsync()
    {
        var handle = NewHandle.Trim();
        if (string.IsNullOrEmpty(handle)) return;

        // Client-side format validation via the shared canonical validator — the
        // SAME rules the nest enforces (fauna_protocol::handle::validate_handle,
        // via the UniFFI face), so feedback is instant and identical across every
        // app (settings.md § Where logic lives → Handle change; mirrors linux
        // settings/account.rs). A *taken* handle stays server-authoritative and
        // surfaces from the change RPC rejection below.
        var formatError = uniffi.fauna_ffi.FaunaFfiMethods.ValidateHandle(handle);
        if (formatError is not null)
        {
            ErrorMessage = formatError;
            return;
        }

        ErrorMessage = null;
        try
        {
            await _rpc.ChangeHandleAsync(handle);
            NewHandle = string.Empty;
            // The reply's echoed handle is NOT applied here — it names a
            // queued, cancellable pending action (settings.md § Pending
            // actions), never a local cache; Handle refreshes on the next
            // account load once the pending action executes. The scheduled
            // change is surfaced by the pending-actions section instead.
            await LoadPendingActionsAsync();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    /// Settings → Account's STANDING pending-actions section (settings.md §
    /// Pending actions) — always present, empty when nothing is scheduled.
    /// Narrowed to still-`pending` rows by <see cref="INestRpcClient.PendingActionsListAsync"/>
    /// already; this just projects each row into its display shape (the verb+
    /// target sentence via the shared <c>describe_pending_action</c> renderer,
    /// the execute-after time via the shared <c>format_unix_local</c>).
    [RelayCommand]
    private async Task LoadPendingActionsAsync()
    {
        try
        {
            var rows = await _rpc.PendingActionsListAsync();
            PendingActions = rows
                .Select(r => new PendingActionRowVm(
                    r.id,
                    FaunaFfiMethods.DescribePendingAction(r.actionType, r.target),
                    Strings.Format("settings/pending_actions/applies", FaunaFfiMethods.FormatUnixLocal(r.executeAfter))))
                .ToList();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    /// One click, no confirm — cancelling is the safe direction (settings.md §
    /// Pending actions). Always re-lists rather than splicing the cancelled row
    /// out locally, matching every other app's reference implementation.
    [RelayCommand]
    private async Task CancelPendingActionAsync(long id)
    {
        try
        {
            await _rpc.PendingActionCancelAsync(id);
            await LoadPendingActionsAsync();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    // ── Unsent changes (account-offline-mutation.md § The offline-mutation
    //    contract → The unsent-changes surface, point 1). Read from the
    //    device outbox, never the nest, so it answers with the nest down; re-read
    //    every UnsentRefresh.Interval while the page shows and after a retry or a
    //    discard. Retry and discard are offered on a NOT-SENT row only, and
    //    discard arms a confirm on that row first. tui's
    //    SettingsState::unsent_changes and android's AccountSettingsVM are the twins.

    // The last read, and the intent whose discard is armed
    // (unsent-change-discard-button pressed, its confirm showing on that row).
    private IReadOnlyList<uniffi.fauna_core.UnsentChange>? _unsentRead;
    private string? _unsentDiscardArmed;

    /// One read of the unsent changes. A failed read keeps what the last read
    /// showed (logged, never painted: the next tick reads again). An armed discard
    /// whose row is no longer a not-sent change is disarmed.
    public async Task LoadUnsentChangesAsync()
    {
        IReadOnlyList<uniffi.fauna_core.UnsentChange> read;
        try
        {
            read = await _unsentDoors.ListAsync();
        }
        catch (Exception ex)
        {
            ShellLog.Debug(nameof(SettingsViewModel), $"unsent changes not re-read: {ex.Message}");
            return;
        }
        _unsentRead = read;
        if (_unsentDiscardArmed is { } armed
            && !read.Any(c => c.@intentId == armed && c.@state is uniffi.fauna_core.UnsentState.Unsent))
        {
            _unsentDiscardArmed = null;
        }
        ProjectUnsentChanges();
    }

    /// <c>unsent-change-retry-button</c>: re-arm the not-sent change; it waits again
    /// at its place in its scope and the drain starts.
    public async Task RetryUnsentChangeAsync(string intentId)
    {
        _unsentDiscardArmed = null;
        try { await _unsentDoors.RetryAsync(intentId); }
        catch (Exception ex) { ShowError(ex); }
        await LoadUnsentChangesAsync();
    }

    /// <c>unsent-change-discard-button</c>: arm the confirm on that row — nothing is
    /// deleted yet.
    public void ArmUnsentDiscard(string intentId)
    {
        _unsentDiscardArmed = intentId;
        ProjectUnsentChanges();
    }

    /// <c>unsent-change-discard-confirm-button</c>: delete the not-sent change — the
    /// one user-initiated deletion of a pending write.
    public async Task ConfirmUnsentDiscardAsync(string intentId)
    {
        _unsentDiscardArmed = null;
        try { await _unsentDoors.DiscardAsync(intentId); }
        catch (Exception ex) { ShowError(ex); }
        await LoadUnsentChangesAsync();
    }

    /// The rows from the last read and the armed discard. Assigned only when they
    /// changed, so the two-second re-read repaints nothing while nothing moves.
    private void ProjectUnsentChanges()
    {
        if (_unsentRead is null) return;
        var rows = _unsentRead
            .Select(c =>
            {
                var notSent = c.@state is uniffi.fauna_core.UnsentState.Unsent;
                return new UnsentChangeRowVm(
                    c.@intentId,
                    UnsentChangeText.Describe(c),
                    UnsentChangeText.Status(c),
                    notSent,
                    notSent && c.@intentId == _unsentDiscardArmed);
            })
            .ToList();
        if (UnsentChanges is { } shown && shown.SequenceEqual(rows)) return;
        UnsentChanges = rows;
    }

    /// Settings → Account "Export My Data" (settings.md § Data export). Fetches
    /// the full archive (payload bytes included — no toggle,
    /// account-data-plane.md § Nest-side requirements item 1, Payload stores
    /// decision (5)) and saves it through the same <see cref="ISnapshotFileSaver"/>
    /// seam single-file restore uses. A cancelled save dialog is not an error
    /// (mirrors BackupsViewModel.DownloadSnapshotFileAsync).
    [RelayCommand]
    private async Task ExportAccountDataAsync()
    {
        if (_fileSaver is null) return;
        ErrorMessage = null;
        try
        {
            var data = await _nest.ExportAccountDataAsync();
            await _fileSaver.SaveAsync("fauna-export.zip", data);
        }
        catch (Exception ex) { ShowError(ex); }
    }
}

/// <summary>
/// One <c>pending-action-item</c> row: the id the cancel gesture round-trips,
/// the verb+target sentence (<c>pending-action-description</c>, already
/// rendered by the shared <c>describe_pending_action</c>), and the finished
/// execute-after display text (<c>pending-action-execute-after</c>, already
/// composed through the shared <c>format_unix_local</c> + the
/// <c>settings/pending_actions/applies</c> template). A record, so the
/// re-listed rows compare by value.
/// </summary>
public sealed record PendingActionRowVm(long Id, string Description, string ExecuteAfterText);

/// <summary>
/// One <c>unsent-change-item</c> row: the intent id retry and discard round-trip, the
/// kind and target in words (<c>unsent-change-description</c>), the state line
/// (<c>unsent-change-status</c>, the reason appended on a not-sent row), whether it is a
/// not-sent row (the only one offering <c>unsent-change-retry-button</c> and
/// <c>unsent-change-discard-button</c>), and whether its discard is armed (the
/// <c>unsent-change-discard-confirm-button</c> showing in the discard's place). Every
/// word comes from the shared describer and state text. A record, so a re-read compares
/// by value.
/// </summary>
public sealed record UnsentChangeRowVm(
    string IntentId, string Description, string Status, bool NotSent, bool DiscardArmed)
{
    /// <summary>Retry shows on a not-sent row.</summary>
    public bool ShowsRetry => NotSent;

    /// <summary>Discard shows on a not-sent row until it is armed.</summary>
    public bool ShowsDiscard => NotSent && !DiscardArmed;

    /// <summary>The confirm shows in the discard's place once armed.</summary>
    public bool ShowsDiscardConfirm => DiscardArmed;
}
