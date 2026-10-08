using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
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
    [ObservableProperty] private bool _isAutoStartEnabled;
    [ObservableProperty] private bool _isCloseToTrayEnabled;
    [ObservableProperty] private int _keyPackageCount;
    [ObservableProperty] private bool _isMlsAvailable;

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

    internal SettingsViewModel(INestHttpClient nest, INestRpcClient rpc, ISnapshotFileSaver? fileSaver = null)
    {
        _nest = nest;
        _rpc = rpc;
        _fileSaver = fileSaver;
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
