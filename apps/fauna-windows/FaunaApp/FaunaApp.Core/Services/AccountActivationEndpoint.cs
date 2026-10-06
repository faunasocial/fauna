using System;
using System.Threading;
using FaunaApp.Core.Logs;

namespace FaunaApp.Core.Services;

/// <summary>
/// Windows' leg of the <b>per-(OS login, account) raise channel</b>
/// (<c>docs/goal/architecture/apps/account-scoping.md</c> § Concurrent instances
/// → <i>The per-(OS login, account) raise channel</i>, ratified 2026-07-23): a named
/// activation endpoint, <c>Local\FaunaApp-Activate-&lt;token&gt;</c>, that
/// <b>every serving instance claims — plain and bound alike</b>.
///
/// <para><b>Why it exists.</b> The app-wide
/// <c>Local\FaunaApp-Activate</c> is owned only by whichever instance launched
/// <i>plainly</i>; a bound instance deliberately owns none. So "focus the instance
/// serving account X" had nothing to call whenever X's server was a bound sibling,
/// and the launch-collision chooser could only report a dead end. Keying the
/// endpoint on the account instead of the install removes that hole
/// structurally.</para>
///
/// <para><b>Nothing is registered and no rendezvous file is written.</b> The name is
/// <i>derivable</i> by any would-be raiser from the account alone, and liveness is
/// handle ownership itself — it dies with the process exactly as the instance
/// lock's file lock does, so there is no staleness to reconcile at boot. That is
/// the whole reason the goal doc rejects a rendezvous file beside the lock.</para>
///
/// <para><b>The token is never derived here.</b> It comes from shared Rust
/// (<see cref="SessionInstance.InstanceToken"/> →
/// <c>fauna_client_accounts::account_instance_token</c>) — the same normalized actor
/// spelling <c>instance-&lt;token&gt;.lock</c> uses, so an endpoint can never
/// address an account the lock file keyed differently. A second
/// <c>ToLowerInvariant</c> in C# is precisely the drift the shared derivation
/// exists to prevent.</para>
///
/// <para><b>Best-effort throughout.</b> An endpoint that cannot be claimed leaves
/// this instance unreachable over the per-account channel, which a raiser reports
/// honestly by re-probing the lock
/// (<see cref="LaunchCollisionGate.ResolveFocusExisting"/>). A guard fault must
/// never break a launch.</para>
///
/// <para>Lives in Core, not beside the WinUI single-instance mechanics, because all
/// of it — claim, raise, switch-release — is headless <c>System.Threading</c> that
/// tests can drive directly; only "raise the window" needs the UI layer, and that
/// arrives as a callback.</para>
/// </summary>
public sealed class AccountActivationEndpoint : IDisposable
{
    private const string LogSource = "AccountActivationEndpoint";

    /// <summary>
    /// The endpoint-name prefix. <c>Local\</c> scopes it to the OS logon session,
    /// which is exactly the "(OS login, account)" unit the goal doc names — a
    /// second signed-in user's instances are a different unit entirely.
    /// </summary>
    public const string NamePrefix = @"Local\FaunaApp-Activate-";

    /// <summary>The endpoint name for a <b>key token</b> (not a raw actor id).</summary>
    public static string NameFor(string keyToken) => NamePrefix + keyToken;

    private readonly Action _onRaise;
    private readonly object _gate = new();
    private EventWaitHandle? _handle;
    private string? _token;

    /// <param name="onRaise">
    /// Invoked on the listener thread when an activation arrives. The caller
    /// marshals to the UI thread — that hop is the UI layer's business, not this
    /// class's.
    /// </param>
    public AccountActivationEndpoint(Action onRaise) =>
        _onRaise = onRaise ?? throw new ArgumentNullException(nameof(onRaise));

    /// <summary>
    /// The key token this endpoint currently serves, or <c>null</c> if none — for
    /// tests and diagnostics.
    /// </summary>
    public string? ServedToken
    {
        get { lock (_gate) { return _token; } }
    }

    /// <summary>
    /// Claim (or retarget to) <paramref name="actorIdHex"/>'s endpoint.
    ///
    /// <para>Idempotent for the account already served, so it is safe on every
    /// session rebuild. On a <b>switch</b> it claims the incoming endpoint
    /// <i>before</i> releasing the outgoing one, mirroring the shared holder's
    /// acquire-new-then-release-old swap — the account being switched to is never
    /// momentarily unreachable.</para>
    /// </summary>
    public void Serve(string actorIdHex)
    {
        var token = SessionInstance.InstanceToken(actorIdHex);
        if (token is null)
        {
            ShellLog.Warn(
                LogSource,
                $"[instance-endpoint] no key token for {actorIdHex} — unreachable over the per-account channel");
            return;
        }

        lock (_gate)
        {
            if (string.Equals(_token, token, StringComparison.Ordinal))
            {
                return;   // already serving this account — a same-account rebuild.
            }

            var outgoing = _handle;
            try
            {
                var ev = new EventWaitHandle(
                    initialState: false, EventResetMode.AutoReset, NameFor(token), out bool createdNew);
                if (!createdNew)
                {
                    // The instance lock is supposed to make this impossible: we hold
                    // this account's lock, so nobody else should own its endpoint.
                    // Log rather than bail — we are the legitimate holder, and two
                    // waiters on one auto-reset event would otherwise silently
                    // deliver a raise to the wrong window.
                    ShellLog.Warn(
                        LogSource,
                        $"[instance-endpoint] {NameFor(token)} was already owned — lock and endpoint disagree");
                }
                _handle = ev;
                _token = token;
                var thread = new Thread(() => Listen(ev))
                {
                    IsBackground = true,
                    Name = "FaunaAccountActivateListener",
                };
                thread.Start();
            }
            catch (Exception ex)
            {
                _handle = null;
                _token = null;
                ShellLog.Warn(
                    LogSource,
                    $"[instance-endpoint] could not claim {NameFor(token)} ({ex.Message}) — unreachable over the per-account channel");
            }

            Release(outgoing);
        }
    }

    /// <summary>
    /// Stop serving: release the endpoint so the name is free for whoever serves
    /// that account next.
    /// </summary>
    public void Dispose()
    {
        lock (_gate)
        {
            var outgoing = _handle;
            _handle = null;
            _token = null;
            Release(outgoing);
        }
    }

    /// <summary>
    /// Ask the instance serving <paramref name="actorIdHex"/> to raise its window.
    /// <c>true</c> if the activation was delivered.
    ///
    /// <para>Static, and deliberately so: the raiser is a <i>different process</i>
    /// from the server, and it addresses the endpoint by deriving its name — there
    /// is nothing to look up and no instance to hold.</para>
    ///
    /// <para><c>false</c> means the endpoint is unowned, which alone does NOT mean
    /// the user is stuck: the caller re-probes the lock to tell "it exited" from
    /// "it's alive but endpoint-less"
    /// (<see cref="LaunchCollisionGate.ResolveFocusExisting"/>).</para>
    /// </summary>
    public static bool TryRaise(string actorIdHex)
    {
        var token = SessionInstance.InstanceToken(actorIdHex);
        if (token is null)
        {
            return false;
        }
        try
        {
            // TryOpenExisting, never `new EventWaitHandle`: creating it here would
            // succeed against a server that does not exist, and we would report a
            // delivery nobody received.
            if (!EventWaitHandle.TryOpenExisting(NameFor(token), out var ev))
            {
                return false;
            }
            using (ev)
            {
                ev.Set();
            }
            return true;
        }
        catch
        {
            return false;
        }
    }

    /// <summary>
    /// Wake the outgoing listener so it closes its own handle.
    ///
    /// <para>⚠ <b>Signal it; do not <c>Dispose()</c> it here.</b> <c>WaitOne</c>
    /// holds a <c>DangerousAddRef</c> on the handle for the duration of the wait, so
    /// disposing against an infinitely-waiting listener does <i>not</i> close the OS
    /// handle — this process would keep OWNING the old account's endpoint for its
    /// whole lifetime, and a raiser aiming at that account would get a
    /// delivered-looking raise into a window that no longer serves it. Waking the
    /// listener is what actually frees the name.</para>
    /// </summary>
    private static void Release(EventWaitHandle? outgoing)
    {
        try
        {
            outgoing?.Set();
        }
        catch
        {
            // Already torn down; its listener will have closed it.
        }
    }

    /// <summary>
    /// The listener. Owns its handle for the endpoint's whole life and closes it on
    /// the way out — that close is the only thing that releases the name.
    ///
    /// <para>Takes the handle <b>by argument</b> rather than reading the field: a
    /// switch replaces the field, and a loop that re-read it would start waiting on
    /// the incoming account's endpoint — two threads on one auto-reset event, with a
    /// raise delivered to whichever woke first.</para>
    /// </summary>
    private void Listen(EventWaitHandle handle)
    {
        try
        {
            while (true)
            {
                handle.WaitOne();

                lock (_gate)
                {
                    if (!ReferenceEquals(handle, _handle))
                    {
                        // Retargeted (or released): this wake-up is the switch's
                        // "you may stop" signal, not a raise.
                        return;
                    }
                    // Logged on the RECEIVING side on purpose. A raiser only learns
                    // that the endpoint existed; this line is the only evidence the
                    // serving instance actually got the activation — which is what
                    // makes the e2e assertion causal rather than "the other process
                    // happened to exit".
                    ShellLog.Info(LogSource, $"[instance-endpoint] raised for {_token}");
                }

                _onRaise();
            }
        }
        catch
        {
            // Process tearing down — stop listening.
        }
        finally
        {
            handle.Dispose();
        }
    }
}
