using System;
using FaunaApp.Core.Logs;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Where this seat's sign-out erases, and how it reaches its credential
/// registry — what the one sign-out routine needs to ask the sign-out's own
/// questions. <paramref name="BaseDir"/> is the install base
/// (<see cref="AccountStateDir.Base"/>), home of both the sign-out record and the
/// residue record, and where a sibling window holds its instance lock;
/// <paramref name="StoreContainerDir"/> is what the seat passes to
/// <c>AccountStateEraseAllScopes</c> and <c>SignOutBlocked</c> (<c>null</c> on
/// windows — the shared per-user store root); <paramref name="Registry"/> opens a
/// registry view over the one credential store (<c>CredentialStore.Registry</c>,
/// which lives in the WinUI project this assembly cannot reference).
/// </summary>
internal sealed record ResidueSeat(
    string BaseDir, string? StoreContainerDir, Func<FfiAccountRegistry> Registry);

/// <summary>
/// The state of <c>identity_choice</c>'s <c>sign-out-residue</c> view while a
/// sign-out still owes work: its one line, and Remove Again
/// (<c>account-scoping.md</c> § Erasure follows scope → <i>the residue
/// surface</i>). An interface so <c>OnboardingViewModel</c> is driven by a fake
/// in unit tests, and so nothing XAML binds is a UniFFI type. Twin of apple's
/// <c>SignOutResidueSurfacing</c>.
/// </summary>
internal interface ISignOutResidueSurface
{
    /// <summary>
    /// <c>sign-out-residue-message</c>, already localized — the shared
    /// <c>Rendered</c> copy, or the routine's own refusal while another window
    /// serves one of the accounts it would reach.
    /// </summary>
    string Line { get; }

    /// <summary>
    /// <c>sign-out-residue-retry-button</c>: run the one sign-out routine again
    /// as Remove Again and return what is left — <c>null</c> when the device is
    /// now clean, which closes the view. Blocking file I/O and up to the sync
    /// agent's budget per recorded account: call it off the UI thread.
    /// </summary>
    ISignOutResidueSurface? Retry();
}

/// <summary>
/// What one run of the sign-out routine answers the seat
/// (<see cref="SignOutResidueSurface.Finish"/>, the FFI <c>FfiSignOutFinish</c>).
/// Twin of apple's <c>SignOutFinishOutcome</c>.
/// </summary>
/// <param name="Residue">The <c>sign-out-residue</c> view to paint — <c>null</c>
/// when the run left nothing.</param>
/// <param name="FailsClosed">The launch must not route over the registry: another
/// instance serves an account the run would reach and the sign-out record still
/// owes its wipe (<c>account-scoping.md</c> § Erasure follows scope → <i>The
/// sign-out record is on all 7 apps</i>, decision 4). The launch lands on
/// <c>identity_choice</c> painting <paramref name="Residue"/> (the refusal line),
/// whose Remove Again re-runs the routine.</param>
internal sealed record SignOutFinishOutcome(ISignOutResidueSurface? Residue, bool FailsClosed)
{
    /// <summary>A run that found nothing to do.</summary>
    internal static readonly SignOutFinishOutcome Nothing = new(null, false);
}

/// <summary>
/// Windows' own credential acts inside the sign-out routine — handed to shared
/// Rust as its <c>FfiSignOutHost</c>.
/// <list type="bullet">
/// <item><see cref="WipeOwnStore"/> — nothing more to do: shared Rust has already
/// run the registry's own wipe (<c>wipe_own</c>: the index and every per-actor
/// slot of this app), and windows has one credential store with no wholesale
/// platform reset behind it (android's <c>SecureStorage.clear()</c> is that).
/// The install-scoped rows the store also holds (<c>install/device_secret</c>)
/// are not a sign-out's to take. The read-back shared Rust runs after this is
/// the witness.</item>
/// <item><see cref="EraseCredentials"/> — the residue re-sweep's credential erase:
/// the registry's <c>ClearAll()</c>, which reads back what it deleted. Shared
/// Rust reads the RECORDED keys back on top of this answer.</item>
/// </list>
/// Twin of apple's <c>SignOutHost</c>.
/// </summary>
internal sealed class SignOutHost : FfiSignOutHost
{
    private readonly Func<FfiAccountRegistry> _registry;

    public SignOutHost(Func<FfiAccountRegistry> registry) => _registry = registry;

    public bool WipeOwnStore() => true;

    public FfiCredentialSweep EraseCredentials()
    {
        using var registry = _registry();
        return registry.ClearAll();
    }
}

/// <summary>
/// The windows seat of <b>the sign-out record</b> and the residue surface, over the
/// <c>fauna-ffi</c> face (<c>libs/fauna-ffi/src/sign_out_residue.rs</c>) android and
/// the Apple apps use (<c>account-scoping.md</c> § Erasure follows scope → <i>The
/// sign-out record is on all 7 apps</i>). The record, the routine and every word of
/// the line are shared Rust; this only carries the seat's bases across and localizes
/// the line. Twin of apple's <c>SignOutResidueSurface</c> and android's
/// <c>AccountStores.recordSignOut</c> / <c>finishSignOut</c>.
/// </summary>
internal sealed class SignOutResidueSurface : ISignOutResidueSurface
{
    private readonly ResidueSeat _seat;

    private SignOutResidueSurface(ResidueSeat seat, string line)
    {
        _seat = seat;
        Line = line;
    }

    public string Line { get; }

    /// <summary>
    /// <b>The sign-out's decision</b>, written before anything else — after the
    /// serving question passed, before the teardown is counted and before the
    /// runtime's stop is awaited (decision 1): <c>sign-out-record.json</c> under the
    /// install base names every account the registry lists plus
    /// <paramref name="servedActor"/> (the account this window serves, which an
    /// unreadable index would not name) and owes the credential wipe, so a process
    /// killed anywhere after this is finished by the next launch. The
    /// unreadable-index start-over passes <c>null</c>: a wipe owed with no accounts,
    /// whose finish sweeps every scope under the bases.
    ///
    /// <para>Returns the accounts recorded, which the gesture hands to its own
    /// <see cref="Finish"/> as <c>written</c>, so a record the install base refused
    /// still finishes in this process. Synchronous and quick (one small file
    /// write): it runs on the gesture's own turn, before its first await.</para>
    ///
    /// <para>A record that throws is logged and answers an EMPTY list, never
    /// <c>null</c>: handed to <see cref="Finish"/> as <c>written</c>, it stands in for a
    /// record that owes the wipe with no accounts named, and a wipe owed reaches every
    /// account the registry lists — so the gesture still erases in this process, as it
    /// did before the record existed. Only a crash before that finish goes
    /// unrecorded.</para>
    /// </summary>
    internal static string[] RecordSignOut(ResidueSeat seat, string? servedActor)
    {
        try
        {
            using var registry = seat.Registry();
            return FaunaFfiMethods.SignOutRecordBegin(registry, seat.BaseDir, servedActor);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("SignOutResidue",
                $"[sign-out] the record could not be written ({ex.Message}) — this process still owes the wipe");
            return Array.Empty<string>();
        }
    }

    /// <summary>
    /// <b>The one routine</b> the sign-out's tail, the launch and Remove Again run —
    /// shared Rust's finish over this seat's install base and store root: the serving
    /// question, the sync agent dismissed (for every <paramref name="caller"/> but the
    /// gesture, whose own teardown already did it), the wipe, each recorded account's
    /// scopes and auxiliary slots, the residue re-sweep, the residue record written
    /// before the sign-out record clears (decisions 2–6).
    ///
    /// <para><c>ownLock: null</c> — windows serves through the process-global holder
    /// (<see cref="SessionInstance"/>), which the shared probe puts down itself;
    /// <paramref name="written"/> is the gesture's <see cref="RecordSignOut"/>
    /// answer, <c>null</c> elsewhere. While the registry names somebody only Remove
    /// Again re-sweeps the residue record, its recorded paths only, the credential
    /// half held as recorded (decision 3, the registry gate).</para>
    ///
    /// <para>Blocking file I/O and, but for the gesture, up to the agent budget per
    /// recorded account: call it off the UI thread. It does not catch: a run that
    /// throws has said nothing about the disk, and each caller decides what that
    /// means for its own paint — Remove Again keeps the line up, the launch and the
    /// gesture log it and go on, the records still standing for the next run.</para>
    /// </summary>
    internal static SignOutFinishOutcome Finish(
        ResidueSeat seat, FfiFinishCaller caller, string[]? written)
    {
        FfiSignOutFinish outcome;
        using (var registry = seat.Registry())
        {
            outcome = FaunaFfiMethods.SignOutFinish(
                registry, new[] { seat.BaseDir }, seat.StoreContainerDir, null,
                new SignOutHost(seat.Registry), caller, written);
        }
        if (outcome.refused)
        {
            ShellLog.Warn("SignOutResidue",
                $"[sign-out] finish ({caller}) REFUSED — another instance serves an account it "
                + $"reaches (fails closed: {outcome.failsClosed})");
        }
        else if (outcome.residue is not null)
        {
            ShellLog.Warn("SignOutResidue",
                $"[sign-out] finish ({caller}) left this user's data on the device");
        }
        ISignOutResidueSurface? surface = null;
        if (outcome.residue is { } residue)
        {
            // The line is all the view needs; the record behind it is on disk, and
            // Remove Again re-runs the routine over that, never over this object.
            using (residue)
            {
                surface = new SignOutResidueSurface(seat, Strings.Resolve(residue.Line()));
            }
        }
        return new SignOutFinishOutcome(surface, outcome.failsClosed);
    }

    public ISignOutResidueSurface? Retry() => Finish(_seat, FfiFinishCaller.RemoveAgain, null).Residue;
}
