using System;
using System.Collections.Generic;
using System.Linq;
using FaunaApp.Core.Logs;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// What <see cref="SessionInstance.Become"/> decided — the shared holder's four
/// cases, flattened for C# call sites
/// (<c>fauna_client_accounts::SessionInstanceOutcome</c>).
/// Three of the five mean <b>proceed</b>; the two refusals are terminal.
/// </summary>
public enum SessionInstanceStatus
{
    /// <summary>The account's lock was taken — a fresh acquire.</summary>
    Acquired,

    /// <summary>
    /// This process already held this account's lock. A same-account session
    /// rebuild reuses it: re-acquiring would make the process refuse <i>itself</i>,
    /// because the kernel treats a second open as a competing owner.
    /// </summary>
    Reused,

    /// <summary>
    /// No guard is held (no resolvable state base, or the lock file could not be
    /// opened) — proceed unguarded. A filesystem hiccup must never become a client
    /// that refuses to launch.
    /// </summary>
    Degraded,

    /// <summary>Another live process already serves this account.</summary>
    RefusedAlreadyServed,

    /// <summary>
    /// This process is bound to one account but the session resolved another —
    /// "launch bound or refuse", never a plain-launch fallback.
    /// </summary>
    RefusedBoundMismatch,
}

/// <summary>
/// The result of a <see cref="SessionInstance.Become"/> call: the status plus the
/// shared platform-neutral one-line reason, which crosses the FFI seam so all seven
/// apps log the same sentence rather than each inventing one.
/// </summary>
public readonly record struct SessionInstanceResult(SessionInstanceStatus Status, string Reason)
{
    /// <summary>
    /// Whether this launch (or session rebuild) may go on. <c>false</c> is terminal:
    /// the caller refuses — never a fallback onto a different account.
    /// </summary>
    public bool MayProceed =>
        Status is SessionInstanceStatus.Acquired
               or SessionInstanceStatus.Reused
               or SessionInstanceStatus.Degraded;
}

/// <summary>
/// Windows' leg of the <b>(OS login, account) single-instance guard</b>
/// (<c>docs/goal/architecture/apps/account-scoping.md</c> § Concurrent
/// instances): at most one instance per account, any number of accounts
/// concurrently.
///
/// <para>Everything that decides anything here is shared Rust. The lock is
/// <c>fauna_client_accounts::AccountInstanceLock</c> — exclusive-or-die (never
/// queued: a second same-account instance has nothing coherent to do), crash-safe
/// by construction (a kernel file lock the OS releases when the holder dies, so
/// there is no stale-lock reconciliation at boot), and degrading <b>open</b> on I/O
/// failure. The four behaviours around it — bound-or-refuse, reuse on a
/// same-account rebuild, swap-by-replacement on a cross-account switch, and
/// degrade-open — are <c>SessionInstanceHolder</c>, reached over UniFFI. This class
/// adds only the two duties the goal doc assigns the platform: <b>resolve the
/// install-scoped state base</b> and <b>log the degrade</b>. It is the C# twin of
/// linux's <c>account_scope::become_session_instance</c>.</para>
///
/// <para><b>This is NOT <see cref="SingleInstanceManager"/>'s replacement.</b> The
/// two layers answer different questions and never disagree: the mutex-plus-activate
/// event stays in force as the plain-launch <i>raise</i> layer (per OS login, per
/// app), while this lock decides whether a <i>second same-account session</i> may
/// run. A bound launch opts out of the mutex entirely and is guarded only here.</para>
///
/// <para><b>Read <see cref="LaunchBinding"/>, never <c>RequestedBoundAccount</c>.</b>
/// A binding can arise <i>after</i> launch: the launch-collision chooser's pick makes
/// this process the chosen account's bound instance, with no third process and no
/// environment left to re-read. linux records "a client that re-read
/// <c>FAUNA_BOUND_ACCOUNT</c> would silently ignore its own chooser" as the second of
/// the three mistakes that bite chooser platforms in order.</para>
/// </summary>
public static class SessionInstance
{
    private const string LogSource = "SessionInstance";

    /// <summary>
    /// The account this process runs as when it is not the plain/primary instance —
    /// the environment's <c>FAUNA_BOUND_ACCOUNT</c> <i>or</i> the chooser's pick,
    /// whichever this process has. <c>null</c> is an ordinary primary launch.
    /// </summary>
    public static string? LaunchBinding => FaunaFfiMethods.SessionLaunchBinding();

    /// <summary>
    /// Bind this process to <paramref name="actorIdHex"/> — the chooser's pick.
    /// The chooser is the one writer that <i>creates</i> a binding after launch, and
    /// it runs only on a <b>plain</b> launch (a bound launch that collides is
    /// terminally refused and never renders the chooser).
    /// <c>IFfiAccountRegistry.BindAccount</c> remains the gate that decides whether
    /// the pick is <i>allowed</i>; this records the answer for the rest of the process.
    ///
    /// <para>A binding also <i>moves</i> without any call from this app: a succession
    /// re-points the account it names, and the shared
    /// <c>IFfiAccountRegistry.RecordSuccession</c> (which the FFI ceremony runs before
    /// <c>SwitchAccountHandler</c> is invoked with the successor) re-points the binding
    /// with it, so the switch's <see cref="Become"/> on the successor passes
    /// bound-or-refuse exactly as the predecessor's did
    /// (<c>account-scoping.md</c> § Concurrent instances → <i>The binding follows the
    /// account</i>). Never re-bind by hand here.</para>
    /// </summary>
    public static void BindLaunchTo(string actorIdHex) =>
        FaunaFfiMethods.BindSessionLaunchTo(actorIdHex);

    /// <summary>
    /// The per-account <b>instance token</b> for <paramref name="actorIdHex"/> —
    /// <c>null</c> when it is not a well-formed actor id.
    ///
    /// <para>The single normalized spelling every per-account artifact keys off,
    /// derived in shared Rust (<c>fauna_client_accounts::account_instance_token</c>)
    /// so this client's per-account activation endpoint
    /// (<c>Local\FaunaApp-Activate-&lt;token&gt;</c>) and the shared lock file
    /// (<c>instance-&lt;token&gt;.lock</c>) can never key differently
    /// (<c>account-scoping.md</c> § Concurrent instances → <i>The per-(OS login,
    /// account) raise channel</i>). Never re-derive it here: a second
    /// <c>ToLowerInvariant</c> in C# is exactly the drift the shared derivation
    /// exists to prevent.</para>
    ///
    /// <para><c>null</c> is a <b>degrade, not a refusal</b>: an account with no
    /// token is simply unreachable over the per-account channel, and the caller
    /// falls back on the lock re-probe.</para>
    /// </summary>
    public static string? InstanceToken(string actorIdHex)
    {
        if (string.IsNullOrWhiteSpace(actorIdHex))
        {
            return null;
        }
        try
        {
            return FaunaFfiMethods.AccountInstanceToken(actorIdHex);
        }
        catch (Exception ex)
        {
            ShellLog.Warn(LogSource, $"[instance-endpoint] token derivation threw ({ex.Message}) — no endpoint");
            return null;
        }
    }

    /// <summary>
    /// Become (or remain) this process's session account, under the install-scoped
    /// base (<see cref="AccountStateDir.Base"/>).
    ///
    /// <para><b>Call it at the point the session account resolves — before opening
    /// any of that account's scoped state</b>, which on windows means immediately
    /// after the secret loads into <c>ICryptoService</c> and before the MLS store,
    /// drafts or backup-coordinator paths are derived from
    /// <c>ActorIdHex</c>. Call it again on every account switch: one call site
    /// covers acquire, same-account reuse and cross-account swap, because the
    /// holder distinguishes them.</para>
    /// </summary>
    public static SessionInstanceResult Become(string actorIdHex) =>
        BecomeUnder(AccountStateDir.Base, actorIdHex, storeContainerDir: null);

    /// <summary>
    /// The base-passed half of <see cref="Become"/>, so the guard is testable
    /// against a temp directory without mutating process-global environment — the
    /// same split linux uses for its <c>_under</c> helpers.
    ///
    /// <para><paramref name="storeContainerDir"/> is where the instance declares
    /// itself at the shared account-store root (the serving lock a sibling app's
    /// sign-out asks about) — the same argument <c>AccountStateEraseAllScopes</c>
    /// takes. Production passes <c>null</c> (the per-user platform root); a test
    /// passes a temp dir, or it would write lock files into the real
    /// <c>%LocalAppData%\Fauna\sync</c>.</para>
    /// </summary>
    public static SessionInstanceResult BecomeUnder(string? stateBase, string actorIdHex, string? storeContainerDir)
    {
        FfiSessionInstanceOutcome outcome;
        try
        {
            // `Concurrent` — windows' W5.6 (account-data-plane.md § Workstreams) retirement leg (2026-08-24,
            // `account-scoping.md` § Concurrent instances). Serving takes a
            // SHARED per-account lock, so a second same-account instance
            // coexists instead of being refused; exclusivity survives only in
            // the three genuinely exclusive critical sections, each carrying
            // its own lock. `RefusedAlreadyServed` below is not dead code —
            // it still fires against an *exclusive* holder (the sign-out
            // probe's brief exclusive acquire), and `RefusedBoundMismatch` is
            // mode-independent.
            outcome = FaunaFfiMethods.BecomeProcessSessionInstance(
                stateBase, actorIdHex, FfiServingMode.Concurrent, storeContainerDir);
        }
        catch (Exception ex)
        {
            // The guard narrows a race; it must not widen a failure into a client
            // that cannot launch. An FFI fault degrades open, like an I/O one.
            ShellLog.Warn(LogSource, $"[instance-lock] acquire threw ({ex.Message}) — proceeding unguarded");
            return new SessionInstanceResult(SessionInstanceStatus.Degraded, ex.Message);
        }

        switch (outcome)
        {
            case FfiSessionInstanceOutcome.Acquired:
                return new SessionInstanceResult(SessionInstanceStatus.Acquired, string.Empty);

            case FfiSessionInstanceOutcome.Reused:
                return new SessionInstanceResult(SessionInstanceStatus.Reused, string.Empty);

            case FfiSessionInstanceOutcome.Degraded degraded:
                // The goal doc assigns this log to the platform, which is why the
                // shared crate reports the degrade upward instead of logging itself.
                ShellLog.Warn(
                    LogSource,
                    $"[instance-lock] degraded acquire for {actorIdHex} ({degraded.@cause}) — proceeding unguarded");
                return new SessionInstanceResult(SessionInstanceStatus.Degraded, degraded.@cause.ToString());

            case FfiSessionInstanceOutcome.Refused refused:
                var status = refused.@refusal is FfiInstanceRefusal.BoundMismatch
                    ? SessionInstanceStatus.RefusedBoundMismatch
                    : SessionInstanceStatus.RefusedAlreadyServed;
                return new SessionInstanceResult(status, refused.@reason);

            default:
                // Unreachable: the shared enum is closed. Degrade open rather than
                // throw — a new variant must not strand a launch.
                return new SessionInstanceResult(SessionInstanceStatus.Degraded, string.Empty);
        }
    }

    /// <summary>
    /// Those of <paramref name="actorIds"/> that <b>no live instance currently
    /// serves</b>, order preserved — the launch-collision chooser's list.
    ///
    /// <para><b>Display-only.</b> An account free at probe time can be taken before
    /// the human clicks, so arbitration stays at <see cref="Become"/>: the pick must
    /// still go through <c>BindAccount</c> + acquire and handle a refusal. Failures
    /// read as "not served", so a hiccup narrows the list rather than leaving the
    /// user with no way in.</para>
    /// </summary>
    public static IReadOnlyList<string> NotCurrentlyServed(IEnumerable<string> actorIds) =>
        NotCurrentlyServedUnder(AccountStateDir.Base, actorIds);

    /// <summary>The base-passed half of <see cref="NotCurrentlyServed"/>.</summary>
    public static IReadOnlyList<string> NotCurrentlyServedUnder(string? stateBase, IEnumerable<string> actorIds)
    {
        if (stateBase is null)
        {
            // No resolvable base means no lock files to probe and no way to tell
            // free from served. Offer nothing rather than offer everything — the
            // chooser's other two exits still work, so the user is never stranded.
            return Array.Empty<string>();
        }
        var ids = actorIds?.ToArray() ?? Array.Empty<string>();
        if (ids.Length == 0)
        {
            return Array.Empty<string>();
        }
        try
        {
            return FaunaFfiMethods.AccountsNotCurrentlyServed(stateBase, ids);
        }
        catch (Exception ex)
        {
            ShellLog.Warn(LogSource, $"[instance-lock] probe failed ({ex.Message}) — offering nothing");
            return Array.Empty<string>();
        }
    }

    /// <summary>
    /// Whether a live instance currently serves <paramref name="actorIdHex"/> — the
    /// launch-collision check, over the same display-only probe. Answers
    /// <c>false</c> on every failure, so a probe hiccup falls through to the ordinary
    /// launch rather than stranding the user in a chooser.
    /// </summary>
    public static bool IsServed(string actorIdHex) => IsServedUnder(AccountStateDir.Base, actorIdHex);

    /// <summary>The base-passed half of <see cref="IsServed"/>.</summary>
    public static bool IsServedUnder(string? stateBase, string actorIdHex)
    {
        if (stateBase is null || string.IsNullOrWhiteSpace(actorIdHex))
        {
            return false;
        }
        var free = NotCurrentlyServedUnder(stateBase, new[] { actorIdHex });
        return !free.Any(id => string.Equals(id, actorIdHex, StringComparison.OrdinalIgnoreCase));
    }
}
