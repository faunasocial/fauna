using System;
using FaunaApp.Core.Logs;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Where this seat's sign-out erases, and how it reaches its credential
/// registry — what the residue face needs to ask the sign-out's own questions
/// again. <paramref name="BaseDir"/> is the install base
/// (<see cref="AccountStateDir.Base"/>), where the record is kept and where a
/// sibling window holds its instance lock; <paramref name="StoreContainerDir"/>
/// is what the seat passes to <c>AccountStateEraseAllScopes</c> and
/// <c>SignOutBlocked</c> (<c>null</c> on windows — the shared per-user store
/// root); <paramref name="Registry"/> opens a registry view over the one
/// credential store (<c>CredentialStore.Registry</c>, which lives in the WinUI
/// project this assembly cannot reference).
/// </summary>
internal sealed record ResidueSeat(
    string BaseDir, string? StoreContainerDir, Func<FfiAccountRegistry> Registry);

/// <summary>
/// The state of <c>identity_choice</c>'s <c>sign-out-residue</c> view while a
/// sign-out's residue still owes work: its one line, and Remove Again
/// (<c>account-scoping.md</c> § Erasure follows scope → <i>the residue
/// surface</i>). An interface so <c>OnboardingViewModel</c> is driven by a fake
/// in unit tests, and so nothing XAML binds is a UniFFI type.
/// </summary>
internal interface ISignOutResidueSurface
{
    /// <summary>
    /// <c>sign-out-residue-message</c>, already localized — the shared
    /// <c>Rendered</c> copy, or the retry's own refusal while another window
    /// serves one of the residue's accounts.
    /// </summary>
    string Line { get; }

    /// <summary>
    /// <c>sign-out-residue-retry-button</c>: re-sweep exactly what this residue
    /// recorded and return what is left — <c>null</c> when the device is now
    /// clean, which closes the view. Blocking file I/O: call it off the UI
    /// thread.
    /// </summary>
    ISignOutResidueSurface? Retry();
}

/// <summary>
/// Windows' credential erase — the one its sign-out runs
/// (<c>App.ClearCredentialNamespace</c> → the registry's <c>ClearAll()</c>, which
/// reads back what it deleted), handed to shared Rust as the residue retry's
/// <c>FfiResidueCredentialEraser</c> so the retry re-runs exactly what the
/// sign-out ran. Windows has one credential store and no wholesale platform
/// reset behind it (android's <c>SignOutCredentialEraser</c> has both), so this
/// is the whole sequence. Shared Rust calls it only when the recorded credential
/// half is not clean, and reads the RECORDED keys back on top of its answer.
/// </summary>
internal sealed class SignOutCredentialEraser : FfiResidueCredentialEraser
{
    private readonly Func<FfiAccountRegistry> _registry;

    public SignOutCredentialEraser(Func<FfiAccountRegistry> registry) => _registry = registry;

    public FfiCredentialSweep EraseCredentials()
    {
        using var registry = _registry();
        return registry.ClearAll();
    }
}

/// <summary>
/// The windows seat of the sign-out residue surface, over the <c>fauna-ffi</c>
/// residue face (<c>libs/fauna-ffi/src/sign_out_residue.rs</c>) android uses.
/// The record, the four re-sweep rules and every word of the line are shared
/// Rust; this only carries the seat's bases across and localizes the line.
/// Twin of android's <c>AccountStores.recordResidue</c> /
/// <c>retryResidue</c> / <c>recheckResidueAtLaunch</c>.
/// </summary>
internal sealed class SignOutResidueSurface : ISignOutResidueSurface
{
    private readonly ResidueSeat _seat;
    private readonly FfiSignOutResidue _residue;

    private SignOutResidueSurface(ResidueSeat seat, FfiSignOutResidue residue)
    {
        _seat = seat;
        _residue = residue;
        Line = Strings.Resolve(residue.Line());
    }

    public string Line { get; }

    /// <summary>
    /// The sign-out's half: record what its two erases left — the
    /// <see cref="AccountStateDir.EraseAll"/> sweep and the registry's
    /// <c>ClearAll()</c> read-back — under the install base, so it outlives this
    /// process, and return the view to paint. <c>null</c> is the clean outcome,
    /// and the only one; a clean erase says nothing and keeps no record.
    ///
    /// <para><paramref name="sweep"/> is <c>null</c> when the filesystem erase
    /// failed outright. That half is then unknown — there are no paths to
    /// record — and the credential half still owes its line, which is why a
    /// missing sweep is recorded as an empty one rather than returning early.</para>
    ///
    /// <para>The survivor paths were logged by <see cref="AccountStateDir.EraseAll"/>;
    /// the credential keys are logged here. Neither reaches the line.</para>
    /// </summary>
    internal static ISignOutResidueSurface? Record(
        ResidueSeat seat, FfiEraseSweep? sweep, FfiCredentialSweep credentials)
    {
        if (credentials.survivors.Length > 0 || credentials.wipeFailed)
        {
            ShellLog.Warn("SignOutResidue",
                $"sign-in credentials SURVIVED the erase (wipe failed: {credentials.wipeFailed}); still readable: "
                + string.Join(", ", credentials.survivors));
        }
        return Painted(seat,
            FaunaFfiMethods.SignOutResidueRecord(seat.BaseDir, sweep ?? NothingSwept, credentials));
    }

    /// <summary>
    /// The signed-out launch's silent re-check: a record a previous sign-out
    /// left is re-swept FIRST, and the view comes back only if something is
    /// still left. Shared Rust leaves the record alone while the registry holds
    /// an account — a signed-in launch is not the user the residue was reported
    /// to. Blocking file I/O: call it off the UI thread.
    ///
    /// <para>A re-check that throws paints nothing and is logged: a launch must
    /// not die on it, and the record is still on disk for the next one.</para>
    /// </summary>
    internal static ISignOutResidueSurface? RecheckAtLaunch(ResidueSeat seat)
    {
        try
        {
            using var registry = seat.Registry();
            return Painted(seat, FaunaFfiMethods.SignOutResidueRecheckAtLaunch(
                registry, seat.BaseDir, seat.StoreContainerDir, null,
                new SignOutCredentialEraser(seat.Registry)));
        }
        catch (Exception ex)
        {
            ShellLog.Warn("SignOutResidue", $"launch re-check failed: {ex.Message}");
            return null;
        }
    }

    public ISignOutResidueSurface? Retry()
    {
        using var registry = _seat.Registry();
        // `ownLock: null` — windows serves through the process-global holder
        // (SessionInstance), which the shared probe puts down itself; the same
        // arguments SignOutBlocked is asked with.
        return Painted(_seat, _residue.Retry(
            registry, _seat.BaseDir, _seat.StoreContainerDir, null,
            new SignOutCredentialEraser(_seat.Registry)));
    }

    private static ISignOutResidueSurface? Painted(ResidueSeat seat, FfiSignOutResidue? residue)
        => residue is null ? null : new SignOutResidueSurface(seat, residue);

    private static FfiEraseSweep NothingSwept => new(
        erased: 0,
        survivors: Array.Empty<string>(),
        residue: new FfiEraseResidueView(survivors: 0, credentialsSurvived: false, owesWork: false));
}
