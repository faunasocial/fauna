using System;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using uniffi.fauna_launch_machine;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The locked launch surface — ui.yaml page <c>launch_account_locked</c> — and the
/// onboarding step it opens, <c>identity_stolen_entry</c>
/// (<c>docs/goal/ui/sessions.md</c> § Layout &amp; flow → <i>The locked surface
/// (leg 2)</i>; behavior <c>devices.md</c> § The locked state). The windows twin of
/// FaunaKit's <c>LockedLaunchModel</c>, android's <c>LaunchAccountLockedScreen</c>
/// state and tui's <c>locked.rs</c>.
///
/// <para>The standing notice — that the account is locked, when the lock ends, and
/// that a lock the owner did not set means somebody holds the secret key — and its
/// ONE action, the stolen-identity ceremony. No retry (nothing clears the lock
/// before its time; the launch machine's own refresh at that time signs back in)
/// and no fallthrough ("use a different nest" would misstate the problem).</para>
///
/// <para><b>It decides nothing.</b> Whether the notice still stands on a later
/// launch snapshot is the shared rule (<see cref="NoticeHolds"/> →
/// <c>launch_lock_notice_holds</c>); the ceremony is the session-less shared
/// composition (<c>succession_succeed_stolen_locked</c>, reached through
/// <see cref="_ceremony"/>), riding an anonymous connection with the seed this
/// device still holds — a locked app has no session (<c>devices.md</c> § The two
/// panic buttons, point 5).</para>
/// </summary>
internal partial class LockedLaunchViewModel : ObservableObject
{
    private readonly Func<string, Task<FfiStolenOutcome>> _ceremony;
    private readonly string? _predecessorActorIdHex;
    private readonly Func<long, string> _formatTime;

    /// <summary><c>LaunchSnapshot.lockedUntilSecs</c> of the lock this surface
    /// shows.</summary>
    public ulong LockedUntilSecs { get; }

    /// <summary>The surface is on its stolen-identity step.</summary>
    [ObservableProperty] private bool _inStolenEntry;

    /// <summary><c>recovery-entry-phrase-field</c> — the kit in hand.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(Armed))]
    private string _phrase = string.Empty;

    /// <summary><c>identity-stolen-confirm-field</c>.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(Armed))]
    private string _confirm = string.Empty;

    /// <summary>The ceremony is in flight — a second press cannot start a second
    /// irreversible ceremony.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(Armed))]
    private bool _busy;

    /// <summary><c>error-message</c> on the step. Never logged: one arm carries
    /// the successor's only secret key.</summary>
    [ObservableProperty] private string? _error;

    /// <param name="lockedUntilSecs">The lock this surface shows.</param>
    /// <param name="ceremony">The session-less stolen-identity ceremony over the
    /// kit input — the shell wires <c>succession_succeed_stolen_locked</c> with
    /// this device's held seed and nest.</param>
    /// <param name="predecessorActorIdHex">The locked identity, recorded with the
    /// succession's handoff.</param>
    /// <param name="formatTime">The unlock time's render; the shared
    /// <c>format_unix_local</c> by default.</param>
    public LockedLaunchViewModel(
        ulong lockedUntilSecs,
        Func<string, Task<FfiStolenOutcome>> ceremony,
        string? predecessorActorIdHex,
        Func<long, string>? formatTime = null)
    {
        LockedUntilSecs = lockedUntilSecs;
        _ceremony = ceremony;
        _predecessorActorIdHex = predecessorActorIdHex;
        _formatTime = formatTime ?? FaunaFfiMethods.FormatUnixLocal;
    }

    /// <summary><c>launch-account-locked-notice</c>: the three sentences in the
    /// ratified order, one element, the unlock time through the shared
    /// formatter.</summary>
    public string NoticeText => string.Join(" ",
        Strings.Get("onboarding/launch/account_locked"),
        Strings.Format("onboarding/launch/account_locked_until",
            _formatTime(LockedUntilSecs > long.MaxValue ? long.MaxValue : (long)LockedUntilSecs)),
        Strings.Get("onboarding/launch/account_locked_not_yours"));

    /// <summary><c>identity-stolen-button</c>'s enablement: the literal
    /// <c>SUCCEED</c>, never localized, and no ceremony in flight.</summary>
    public bool Armed =>
        !Busy && string.Equals(Confirm, RecoveryKitViewModel.StolenConfirmWord, StringComparison.Ordinal);

    /// <summary>Whether the surface still stands on <paramref name="snapshot"/>:
    /// the shared <c>LaunchSnapshot::lock_notice_holds</c> rule (true while the
    /// machine stays parked on this lock or is mid-flight on its own refresh), and
    /// unconditionally while the user is on the stolen-identity step,
    /// mid-ceremony.</summary>
    public bool NoticeHolds(LaunchSnapshot snapshot) =>
        InStolenEntry || FaunaLaunchMachineMethods.LaunchLockNoticeHolds(snapshot, LockedUntilSecs);

    /// <summary><c>launch-account-locked-stolen-button</c>.</summary>
    public void OpenStolenEntry()
    {
        InStolenEntry = true;
        Error = null;
    }

    /// <summary><c>recovery-entry-back-button</c>: nothing typed on the step
    /// outlives it. The shell re-reads the machine afterwards — the notice stood
    /// unconditionally while the step was up.</summary>
    public void CloseStolenEntry()
    {
        if (Busy) return;
        InStolenEntry = false;
        Phrase = string.Empty;
        Confirm = string.Empty;
        Error = null;
    }

    /// <summary>
    /// <c>identity-stolen-button</c> — the irreversible succession, with no session.
    /// Both gates are re-checked here, not only in the render.
    ///
    /// <para>The succession is recorded before the switch
    /// (<see cref="SuccessionHandoff.Record"/>, as the Settings ceremony does): it
    /// landed either way, so what it owes is owed either way. On the persisted
    /// landed arm <paramref name="onSwitch"/> activates the successor and re-runs
    /// launch, landing it signed in. Every other arm speaks on <c>error-message</c>
    /// and switches nothing — the persist-failure arm carries the only copy of the
    /// successor's key, so it goes on screen.</para>
    /// </summary>
    public async Task SucceedAsync(Func<string, Task> onSwitch)
    {
        if (Busy) return;
        if (!string.Equals(Confirm, RecoveryKitViewModel.StolenConfirmWord, StringComparison.Ordinal))
        {
            Error = Strings.Get("settings/recovery_kit/stolen_confirm_placeholder");
            return;
        }
        if (string.IsNullOrEmpty(Phrase))
        {
            Error = Strings.Get("settings/recovery_kit/kit_phrase_required");
            return;
        }
        Busy = true;
        FfiStolenOutcome outcome;
        try
        {
            outcome = await _ceremony(Phrase);
        }
        catch (Exception ex)
        {
            // Only a failure BEFORE the ceremony could start reaches here — every
            // ceremony that ran is an outcome.
            Error = Strings.Error(ex);
            ShellLog.Error(nameof(LockedLaunchViewModel), $"the locked ceremony could not start: {ex.GetType().Name}");
            Busy = false;
            return;
        }
        Busy = false;

        if (outcome.landed is not { } landed)
        {
            Error = outcome.message is { } message ? Strings.Resolve(message) : outcome.kind;
            ShellLog.Warn(nameof(LockedLaunchViewModel), $"the locked ceremony did not land: {outcome.kind}");
            return;
        }
        SuccessionHandoff.Record(landed, _predecessorActorIdHex);
        if (!landed.persisted)
        {
            Error = Strings.Format("settings/recovery_kit/stolen_persist_failed", landed.successorSecretHex);
            ShellLog.Error(nameof(LockedLaunchViewModel),
                "the locked ceremony parked the successor's only secret on error-message");
            return;
        }
        Phrase = string.Empty;
        Confirm = string.Empty;
        Error = null;
        await onSwitch(landed.newActorIdHex);
    }
}
