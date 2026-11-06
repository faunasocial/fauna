using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using uniffi.fauna_devices_machine;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>One painted <c>session-card</c>: the face's row with its three
/// localized texts resolved. <see cref="Index"/> is the card's position — the
/// e2e driver's <c>session-card[i]</c>.</summary>
internal sealed record SessionCard(
    int Index, string TokenId, string Kind, string? Mark, string Detail, bool CanRevoke);

/// <summary>
/// Settings → Sessions (<c>docs/goal/ui/sessions.md</c>): where the account is
/// signed in right now, as one escalation ladder — the list (revoke one), sign out
/// everywhere else, the 24-hour lock. The windows twin of android's
/// <c>SessionsVM</c> and FaunaKit's <c>SessionsView</c>.
///
/// <para><b>It decides nothing.</b> Every load and act is one call into the
/// UniFFI face (<c>libs/fauna-ffi/src/sessions.rs</c>), which runs the shared
/// <c>SessionsClient::pass</c> and answers the whole page — rows already folded,
/// sorted, named and marked, the page error, and whether the lock landed. The VM
/// paints that verbatim and hands the face the page it painted last, so a failed
/// re-read keeps the rows (the face's rule, not this one). The two confirm states
/// — the armed sign-out-everywhere-else pair and the typed lock word — are the
/// only state this app owns (§ Where logic lives: "app glue is the render and the
/// two confirm states").</para>
///
/// <para>A landed lock leaves the shell: every bearer is revoked and the nest
/// mints no new one, so <see cref="Locked"/> re-enters launch, whose re-run
/// challenge lands the locked notice (leg 2).</para>
/// </summary>
internal partial class SessionsViewModel : ViewModelBase
{
    private readonly INestRpcClient _rpc;
    private readonly Func<Task<DevicesMachine?>> _devices;
    private readonly Func<string?> _ownDeviceId;
    private readonly string _confirmWord;

    /// <summary>
    /// The painted token-id list, published for the e2e agent's <c>settings</c>
    /// state key (<c>session_token_ids</c> — <c>actions/sessions.py::painted_token_ids</c>):
    /// <c>null</c> before the first hydrate of a visit, the card order after it.
    /// Process-global like android's <c>SessionsTestState</c>; <see cref="Leave"/>
    /// clears it so a later visit's wait cannot pass on an earlier visit's rows.
    /// </summary>
    internal static IReadOnlyList<string>? PaintedTokenIdsForTest { get; set; }

    /// <summary>The page the face answered last — <c>null</c> until the first
    /// hydrate lands, so <c>sessions-empty</c> never paints before it.</summary>
    [ObservableProperty] private FfiSessionsSnapshot? _snapshot;

    /// <summary>The cards to paint, in the face's order.</summary>
    [ObservableProperty] private IReadOnlyList<SessionCard> _cards = Array.Empty<SessionCard>();

    /// <summary>The two-press inline confirm's second press is showing.</summary>
    [ObservableProperty] private bool _revokeOthersArmed;

    /// <summary><c>sessions-lockout-confirm-field</c>, verbatim.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(LockEnabled))]
    private string _lockWord = string.Empty;

    /// <summary>A pass is in flight — every act waits for it.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(LockEnabled))]
    private bool _busy;

    /// <summary>A pass that could not start at all (no connection) — the one
    /// error the face cannot answer, held until the next pass lands.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(PageError))]
    private string? _startError;

    /// <summary>Re-enter launch after a landed lock — the page wires the app's
    /// escalation. Invoked at most once per landed lock.</summary>
    public Func<Task>? Locked { get; set; }

    public SessionsViewModel(
        INestRpcClient rpc,
        Func<Task<DevicesMachine?>> devices,
        Func<string?> ownDeviceId,
        string? confirmWord = null)
    {
        _rpc = rpc;
        _devices = devices;
        _ownDeviceId = ownDeviceId;
        // For the render's enable check only: the face re-checks what was typed.
        _confirmWord = confirmWord ?? FaunaFfiMethods.SessionsLockoutConfirmWord();
    }

    /// <summary><c>sessions-lockout-button</c>'s enablement: the literal word,
    /// never case-folded, and no pass in flight.</summary>
    public bool LockEnabled => !Busy && string.Equals(LockWord, _confirmWord, StringComparison.Ordinal);

    /// <summary><c>error-message</c>: the face's page error, else a pass that
    /// could not start.</summary>
    public string? PageError => Snapshot?.error is { } err ? Strings.Resolve(err) : StartError;

    /// <summary><c>sessions-empty</c> — the fail-safe for a reply carrying no
    /// session; never before the first hydrate.</summary>
    public bool ShowEmpty => Snapshot is { } s && s.rows.Length == 0;

    /// <summary>The nav-edge hydrate.</summary>
    public Task LoadAsync() =>
        RunAsync((d, own, prev) => _rpc.SessionsLoadAsync(d, own, prev));

    /// <summary><c>session-revoke-button[i]</c> — no confirm (recoverable by
    /// re-auth).</summary>
    public Task RevokeAsync(string tokenId) =>
        RunAsync((d, own, prev) => _rpc.SessionsRevokeAsync(d, own, prev, tokenId));

    /// <summary><c>sessions-revoke-others-button</c> — the first press only arms
    /// the confirm.</summary>
    public void ArmRevokeOthers() => RevokeOthersArmed = true;

    /// <summary><c>sessions-revoke-others-cancel-button</c> — nothing sent.</summary>
    public void CancelRevokeOthers() => RevokeOthersArmed = false;

    /// <summary><c>sessions-revoke-others-confirm-button</c> — the second press.</summary>
    public Task ConfirmRevokeOthersAsync()
    {
        RevokeOthersArmed = false;
        return RunAsync((d, own, prev) => _rpc.SessionsRevokeOthersAsync(d, own, prev));
    }

    /// <summary><c>sessions-lockout-button</c> — the typed word goes to the face
    /// verbatim, which refuses a wrong one on <c>error-message</c> and reaches no
    /// nest.</summary>
    public Task LockAsync()
    {
        var word = LockWord;
        return RunAsync((d, own, prev) => _rpc.SessionsLockoutAsync(d, own, prev, word));
    }

    /// <summary>The page left: unpublish the painted ids.</summary>
    public void Leave() => PaintedTokenIdsForTest = null;

    private async Task RunAsync(
        Func<DevicesMachine?, string?, FfiSessionsSnapshot?, Task<FfiSessionsPage>> call)
    {
        if (Busy) return;
        Busy = true;
        FfiSessionsPage? page = null;
        try
        {
            page = await call(await _devices(), _ownDeviceId(), Snapshot);
            StartError = null;
        }
        catch (Exception ex)
        {
            // Only a pass that could not start reaches here — every pass that ran
            // answers a page. Said out loud, the painted rows kept (e2e
            // convention 11: an act's error is never dropped).
            StartError = Strings.Error(ex);
            ShellLog.Warn(nameof(SessionsViewModel), $"the sessions pass could not start: {ex.Message}");
        }
        finally
        {
            Busy = false;
        }
        if (page is null) return;

        Paint(page.snapshot);
        if (page.locked)
        {
            ShellLog.Warn(nameof(SessionsViewModel), "the account lock landed — leaving for the launch surface");
            LockWord = string.Empty;
            if (Locked is { } leave) await leave();
        }
    }

    private void Paint(FfiSessionsSnapshot snapshot)
    {
        Snapshot = snapshot;
        Cards = snapshot.rows
            .Select((r, i) => new SessionCard(
                i,
                r.tokenId,
                Strings.Resolve(r.kind),
                r.mark is { } mark ? Strings.Resolve(mark) : null,
                Strings.Resolve(r.detail),
                r.canRevoke))
            .ToList();
        OnPropertyChanged(nameof(PageError));
        OnPropertyChanged(nameof(ShowEmpty));
        PaintedTokenIdsForTest = snapshot.rows.Select(r => r.tokenId).ToList();
    }
}
