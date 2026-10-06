using System;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// What this device's stolen-identity ceremony holds while it owns the Account
/// page (<c>docs/goal/ui/settings.md</c> § Recovery kit → <i>The persist-failure
/// message survives the page</i>): the page's other error writers, and the
/// supersession escalation the ceremony itself causes. The windows twin of
/// FaunaKit's <c>StolenCeremonyHold</c>; tui's <c>App::defer_own_supersession</c>
/// is the model all three follow.
///
/// <para><b>The writers.</b> When the successor's seed could not be stored, the
/// ceremony parks the new key on Account's <c>error-message</c> — the only copy in
/// existence. <see cref="Admits"/> is the gate the page's one render funnel asks
/// before any other write while <see cref="MessagePending"/> is set.</para>
///
/// <para><b>The escalation.</b> The ceremony is what supersedes the identity, so
/// the running session's next re-mint or token refresh is refused the moment the
/// nest commits — typically before the ceremony's own result is handled.
/// Escalating then (teardown + relaunch) would take the page, and the ceremony's
/// result, down with it. So while the ceremony runs, while its non-adopting
/// outcome is what Account shows, or while its persist-failure message is
/// pending, the escalation is recorded rather than performed, and it runs once the
/// user leaves Account — or at once, when the ceremony ends while the user is
/// elsewhere and no message is parked.</para>
///
/// <para>One per process (<see cref="Shared"/>), because its reporters live at
/// three altitudes — the session-ending sources on <c>App</c>, the ceremony
/// (<c>RecoveryKitViewModel</c>) and the Account page's navigation edges
/// (<c>SettingsAccountPage</c>) — and the page and its view model are rebuilt per
/// visit. UI-thread only, like every one of those reporters. The constructor is
/// open so a unit test owns a fresh one.</para>
/// </summary>
internal sealed class StolenCeremonyHold
{
    public static StolenCeremonyHold Shared { get; } = new();

    /// <summary>True from the stolen ceremony's dispatch until its result is handled.</summary>
    public bool CeremonyInFlight { get; private set; }

    /// <summary>True while the ceremony's persist-failure message is parked on Account.</summary>
    public bool MessagePending { get; private set; }

    /// <summary>True when the ceremony ended WITHOUT adopting a successor while the
    /// user was on Account, so its outcome is what the page shows — until the user
    /// leaves. The refusal the ceremony causes has no fixed order against its result
    /// (tui measured one landing ~30 ms after an undecidable result), and escalating
    /// then would replace the message the user needs with an import screen for a key
    /// they were never shown.</summary>
    public bool OutcomeOnScreen { get; private set; }

    /// <summary>How many Account pages are mounted. A count, not a flag: a re-entered
    /// page can mount before the previous one's leave edge, which is then not the
    /// user leaving Account (measured on iOS, 2026-09-26).</summary>
    private int _accountMounts;

    /// <summary>The escalation held back, if one is owed.</summary>
    private Func<Task>? _owed;

    /// <summary>True while an Account page is on screen.</summary>
    public bool OnAccount => _accountMounts > 0;

    /// <summary>Whether an escalation is owed — for tests and the diagnostic log.</summary>
    public bool IsOwed => _owed is not null;

    /// <summary>
    /// Whether another Account-page writer may put <paramref name="text"/> on the
    /// shared <c>error-message</c>: only while no persist-failure message is pending.
    /// A clear is refused too, unlike apple's gate: windows paints the message on the
    /// one InfoBar every writer shares, so a clear closes it and hides the key.
    /// </summary>
    public bool Admits(string? text) => !MessagePending;

    /// <summary>
    /// Route a supersession escalation: performed now, unless the ceremony owns the
    /// Account page, in which case it is recorded (latest wins — every arrival is the
    /// same teardown).
    /// </summary>
    public async Task EscalateAsync(Func<Task> perform)
    {
        if (CeremonyInFlight || OutcomeOnScreen || MessagePending)
        {
            ShellLog.Info("StolenCeremonyHold",
                "[supersession] held back: this device's own ceremony owns Account");
            _owed = perform;
            return;
        }
        await perform();
    }

    /// <summary><c>identity-stolen-button</c> dispatched the ceremony.</summary>
    public void CeremonyStarted() => CeremonyInFlight = true;

    /// <summary>
    /// The ceremony's result was handled. <paramref name="adopted"/>: the device
    /// switched to the successor — the switch is itself the full relaunch, so an owed
    /// escalation is spent, not performed. <paramref name="messageParked"/>: the
    /// persist-failure message now sits on Account and holds the escalation until the
    /// user leaves it, wherever they are. Otherwise an owed escalation runs now if the
    /// user is off Account; on Account the result stays on screen and the leave edge
    /// performs the escalation, after the user has read it.
    /// </summary>
    public async Task CeremonyEndedAsync(bool adopted, bool messageParked)
    {
        CeremonyInFlight = false;
        MessagePending = messageParked;
        if (adopted)
        {
            _owed = null;
            return;
        }
        OutcomeOnScreen = OnAccount;
        if (!OnAccount && !MessagePending)
        {
            await PerformOwedAsync();
        }
    }

    /// <summary>The Account page appeared.</summary>
    public void AccountAppeared() => _accountMounts++;

    /// <summary>
    /// An Account page left the screen. When it was the last one, the user left the
    /// Account page — the persist-failure message's one acknowledgment gesture — so a
    /// parked message is discharged here and the held-back escalation, if any, goes
    /// the ordinary way.
    /// </summary>
    public async Task AccountLeftAsync()
    {
        _accountMounts = Math.Max(0, _accountMounts - 1);
        if (_accountMounts != 0) return;
        MessagePending = false;
        OutcomeOnScreen = false;
        if (CeremonyInFlight) return;
        await PerformOwedAsync();
    }

    private async Task PerformOwedAsync()
    {
        if (_owed is not { } perform) return;
        _owed = null;
        ShellLog.Info("StolenCeremonyHold", "[supersession] performing the held-back escalation");
        await perform();
    }
}

/// <summary>
/// Route a session-ending verdict (<c>security.md</c> § Post-auth surfacing) to the
/// launch surface — the one door both of windows' mid-session sources go through
/// (the connection supervisor's stop, read off <c>FfiNestClient.SessionEndingVerdict</c>,
/// and the TTL loop's refused refresh), so the two cannot disagree. Re-entering the
/// real launch flow is what lands the right screen: the re-run challenge earns the
/// same refusal, and launch routing takes it from there — the identity-import route
/// for a supersession (<c>identity-succession.md</c> § Propagation → <i>Own device
/// fleet</i>). The windows twin of FaunaKit's <c>escalateSessionEnding</c>.
/// </summary>
internal static class SessionEndingRoute
{
    /// <summary>A supersession goes through <paramref name="hold"/>, which holds it
    /// back while this device's own stolen-identity ceremony owns the Account page;
    /// a nest-identity change or a refused sign-in is never the ceremony's doing and
    /// escalates at once.</summary>
    public static async Task EscalateAsync(
        FfiSessionEndingVerdict verdict, StolenCeremonyHold hold, Func<Task> escalate)
    {
        ShellLog.Warn("SessionEndingRoute", $"[post-auth] session-ending verdict {verdict} — re-entering launch");
        if (verdict == FfiSessionEndingVerdict.Superseded)
        {
            await hold.EscalateAsync(escalate);
            return;
        }
        await escalate();
    }
}
