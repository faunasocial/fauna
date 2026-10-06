using System;
using System.Threading;
using Microsoft.UI.Xaml;
using FaunaApp.Core.Services;

namespace FaunaApp.Services;

/// <summary>
/// Win32 single-instance enforcement (apps/windows.md § App Lifecycle,
/// principle 1: a 2nd launch <b>activates</b> the running instance and exits
/// itself — it never kills the primary, which may be minimized to tray holding
/// unsaved compose state).
///
/// <para>Mechanism: a per-session named <see cref="Mutex"/> distinguishes the
/// primary (first to acquire) from a secondary (mutex already held). A secondary
/// signals a named <see cref="EventWaitHandle"/> the primary waits on, then bows
/// out; the primary's listener marshals to the UI thread and surfaces its window
/// via <see cref="TrayIconService.ShowMainWindow"/>.</para>
///
/// <para>The policy (when to redirect) is the pure, unit-tested
/// <see cref="SingleInstanceGate"/>; this class is only the platform mechanics.
/// Both are <b>disabled under the E2E bridge</b> (<c>FAUNA_E2E_BRIDGE</c> set) so
/// the harness can run concurrent app processes — the Windows twin of linux's
/// <c>gio NON_UNIQUE</c>-in-e2e gate.</para>
///
/// <para><b>It is also the launch-collision chooser's forwarding channel</b>
/// (account-scoping.md § Concurrent instances). The chooser's two non-pick exits
/// need to reach the process that already serves the colliding account and make it
/// do something, then leave — exactly what this class's named events already are,
/// so nothing new is invented and there is no second IPC surface to keep in sync.
/// linux reaches the same two exits over <c>org.freedesktop.Application</c>'s
/// <c>Activate</c> / <c>ActivateAction("add-account")</c>; windows has no payload
/// channel, so the add-account intent gets its <b>own named event</b> beside the
/// activate one rather than an argument on it.</para>
///
/// <para><b>It also owns the per-(OS login, account) raise channel</b>
/// (account-scoping.md § Concurrent instances → <i>The per-(OS login, account)
/// raise channel</i>, ratified 2026-07-23). The app-wide names above are owned by
/// whichever instance launched <i>plainly</i>, and a <b>bound</b> instance
/// deliberately owns none — so "focus the instance serving account X" used to have
/// nothing to call whenever X's server was a bound sibling, and the chooser could
/// only report a dead end. Every serving instance — plain <i>and</i> bound — now
/// additionally claims <c>Local\FaunaApp-Activate-&lt;token&gt;</c>, and
/// focus-existing targets that name <b>uniformly</b>. Nothing is registered and no
/// rendezvous file is written: the name is derivable by any would-be raiser, and
/// liveness is handle ownership itself, which dies with the process exactly as the
/// instance lock's file lock does.</para>
///
/// <para><b>Reachability is still not guaranteed, and callers must handle that</b>
/// — hence <c>bool</c> returns rather than pretending. What changed is the meaning
/// of <c>false</c>: it is no longer "you are stuck", because the caller re-probes
/// the lock and distinguishes a sibling that exited (continue as a plain launch)
/// from one that is alive but endpoint-less (surface it). That fork is
/// <see cref="Core.Services.LaunchCollisionGate.ResolveFocusExisting"/>.</para>
///
/// <para><b>The add-account forward stays app-wide and must NOT be re-keyed</b> —
/// the onboarding scratchpad belongs to the primary, and only a plain instance owns
/// the app-wide name (goal doc, same section).</para>
/// </summary>
internal static class SingleInstanceManager
{
    // Local\ => per logon-session (one primary per signed-in user, matching the
    // "3 instances in one Task Manager" bug this fixes — not machine-wide).
    private const string MutexName = @"Local\FaunaApp-SingleInstance";
    private const string ActivateEventName = @"Local\FaunaApp-Activate";
    // The chooser's "log in as a new user" exit. A SECOND event rather than a flag
    // on the first, because an auto-reset EventWaitHandle carries no payload — the
    // primary must be able to tell "raise" from "raise into the add-account wizard".
    private const string AddAccountEventName = @"Local\FaunaApp-AddAccount";

    // Held for the whole process lifetime so the OS keeps the single-instance
    // claim until exit; never disposed (process exit releases it).
    private static Mutex? _mutex;
    private static EventWaitHandle? _activateEvent;
    private static EventWaitHandle? _addAccountEvent;
    // Held for the process lifetime, like the events above.
    private static RouteHandoffEndpoint? _routeEndpoint;
    private static Window? _window;

    // This process's per-account activation endpoint. The mechanism is Core's
    // (headless, and therefore actually gated by a build that runs tests); this
    // layer supplies only the UI-thread hop the raise needs.
    private static readonly AccountActivationEndpoint _accountEndpoint = new(RaiseOnUiThread);

    // Via E2eEnv so the read is compiled out of release builds (convention 15);
    // the production twin returns null, so IsE2E is false exactly as before and
    // single-instance enforcement stays on in the shipped app.
    private static bool IsE2E => E2eEnv.Bridge is not null;

    /// <summary>
    /// Claim the single-instance slot at startup. Returns <c>true</c> if this
    /// process should continue starting (it is the primary, or we are under E2E);
    /// <c>false</c> if another instance is already running — in which case it has
    /// been signaled to surface its window and the caller must exit without
    /// building a second UI. Best-effort: any failure falls back to starting
    /// normally (a guard fault must never block launch).
    /// </summary>
    public static bool TryClaim()
    {
        try
        {
            // Always take/own the handle so the claim lives for the process.
            _mutex = new Mutex(initiallyOwned: true, MutexName, out bool createdNew);
            bool anotherInstanceRunning = !createdNew;

            var decision = SingleInstanceGate.Decide(IsE2E, anotherInstanceRunning);
            if (decision == SingleInstanceDecision.RedirectAndExit)
            {
                // A launch carrying a fauna:// route (the Explorer Share leaf's
                // hand-off) gives the route to the running instance, which raises
                // itself and applies it; with no route listener yet (a primary
                // mid-startup) it falls back to the plain raise.
                if (App.LaunchRoute is not { } route
                    || !RouteHandoffEndpoint.TryForward(
                        RouteHandoffEndpoint.DefaultEventName, RouteHandoffEndpoint.DefaultPendingPath, route))
                {
                    TryRaiseRunningInstance();
                }
                return false;
            }
            return true;
        }
        catch
        {
            // If the mutex can't be created, fail open — better a possible
            // duplicate than a launch that can't start at all.
            return true;
        }
    }

    /// <summary>
    /// Primary-only: begin listening for a secondary's activation signal. No-op
    /// under E2E. Call after the main window exists and <see cref="TrayIconService"/>
    /// is initialized (so <see cref="TrayIconService.ShowMainWindow"/> can surface it).
    /// </summary>
    public static void StartActivationListener(Window window)
    {
        _window = window;
        if (IsE2E)
        {
            return;
        }
        try
        {
            _activateEvent = new EventWaitHandle(
                initialState: false, EventResetMode.AutoReset, ActivateEventName, out _);
            var thread = new Thread(ActivationLoop)
            {
                IsBackground = true,
                Name = "FaunaActivateListener",
            };
            thread.Start();
        }
        catch
        {
            // Activation listening is best-effort; without it a 2nd launch still
            // safely exits, it just won't surface the existing window.
        }

        try
        {
            _addAccountEvent = new EventWaitHandle(
                initialState: false, EventResetMode.AutoReset, AddAccountEventName, out _);
            var thread = new Thread(AddAccountLoop)
            {
                IsBackground = true,
                Name = "FaunaAddAccountListener",
            };
            thread.Start();
        }
        catch
        {
            // Same best-effort contract: without it the chooser's add-account
            // button reports that it couldn't reach us, which is honest.
        }

        // The route channel (windows.md § Shell Extension → The Share hand-off,
        // step 3): a second launch's fauna:// route, raised into this window and
        // then applied. App-wide like the activate event, and off under E2E for
        // the same reason (concurrent harness processes would contend for it).
        _routeEndpoint = new RouteHandoffEndpoint(
            RouteHandoffEndpoint.DefaultEventName,
            RouteHandoffEndpoint.DefaultPendingPath,
            uri => window.DispatcherQueue.TryEnqueue(() =>
            {
                TrayIconService.ShowMainWindow();
                App.ApplyRoute(uri);
            }));
        _routeEndpoint.Start();
    }

    /// <summary>
    /// Ask the running instance to raise its window. <c>true</c> if the signal was
    /// delivered — the caller then exits, having handed the user off.
    ///
    /// <para>Two callers, one channel: the single-instance redirect
    /// (<see cref="TryClaim"/>, which ignores the result — it must not run a second
    /// instance either way) and the chooser's <b>focus-existing</b> exit, which
    /// needs the answer because "nobody was listening" is a state the user has to
    /// be told about rather than silently quit into.</para>
    /// </summary>
    public static bool TryRaiseRunningInstance() => TrySignal(ActivateEventName);

    /// <summary>
    /// Ask the running instance to open its add-account wizard. <c>true</c> if the
    /// signal was delivered. The wizard runs <b>there</b>, which is the point: the
    /// onboarding scratchpad belongs to the primary, so a colliding process never
    /// runs it (the same ownership rule as the bound-wizard refusal).
    /// </summary>
    public static bool TryForwardAddAccount() => TrySignal(AddAccountEventName);

    /// <summary>
    /// Claim (or retarget) this process's <b>per-account activation endpoint</b> —
    /// <c>Local\FaunaApp-Activate-&lt;token&gt;</c> — for the account it now serves.
    ///
    /// <para>Called from <c>App.EnsureSessionInstance</c>, the single funnel every
    /// session account resolves through, so <b>every serving instance claims one:
    /// plain and bound alike</b>, on a fresh acquire, a same-account rebuild, an
    /// account switch, and even a degraded (unguarded) acquire — a degraded
    /// instance is still serving, and a raiser must still be able to reach it. A
    /// switch releases the outgoing endpoint only <i>after</i> claiming the new
    /// one, mirroring the shared holder's acquire-new-then-release-old swap.</para>
    ///
    /// <para><b>Deliberately live under E2E</b>, unlike
    /// <see cref="StartActivationListener"/>. The app-wide listener is disabled
    /// there because its <i>claim</i> half redirects the harness's concurrent
    /// processes; this endpoint is keyed per account, so concurrent test instances
    /// of different accounts never contend — and the two-driver collision e2e is
    /// the only place the channel is provable at all.</para>
    ///
    /// <para>Best-effort throughout: an endpoint that cannot be claimed leaves this
    /// instance unreachable over the per-account channel, which the raiser's lock
    /// re-probe reports honestly. A guard fault must never break a launch.</para>
    /// </summary>
    public static void ServeAccount(string actorIdHex) => _accountEndpoint.Serve(actorIdHex);

    /// <summary>
    /// Ask the instance serving <paramref name="actorIdHex"/> to raise its window.
    /// <c>true</c> if the activation was delivered.
    ///
    /// <para>The chooser's focus-existing exit targets this <b>uniformly</b> —
    /// plain and bound servers alike — which is the whole point of the per-account
    /// channel: the app-wide name is owned only by a plain instance, so raising a
    /// bound sibling over it was structurally impossible.</para>
    ///
    /// <para><c>false</c> means the endpoint is unowned, which alone does NOT mean
    /// the user is stuck — the caller re-probes the lock to tell "it exited" from
    /// "it's alive but endpoint-less"
    /// (<see cref="Core.Services.LaunchCollisionGate.ResolveFocusExisting"/>).</para>
    /// </summary>
    public static bool TryRaiseAccountInstance(string actorIdHex) =>
        AccountActivationEndpoint.TryRaise(actorIdHex);

    private static bool TrySignal(string eventName)
    {
        try
        {
            // TryOpenExisting, never `new EventWaitHandle`: creating the event here
            // would succeed against a primary that does not exist, and the caller
            // would report a delivery that nobody received.
            if (!EventWaitHandle.TryOpenExisting(eventName, out var ev))
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

    private static void ActivationLoop() => SignalLoop(_activateEvent, () => TrayIconService.ShowMainWindow());

    private static void AddAccountLoop() => SignalLoop(_addAccountEvent, () =>
    {
        // Raise first, then open the wizard: an add-account intent that lands in a
        // tray-minimized window is indistinguishable from one that was dropped.
        TrayIconService.ShowMainWindow();
        App.AddAccountHandler?.Invoke();
    });

    /// <summary>
    /// The UI-thread hop the per-account endpoint's listener needs — the only part
    /// of the raise channel that is genuinely WinUI's. Called from the listener
    /// thread; SW_RESTORE + SetForegroundWindow must run on the UI thread.
    /// </summary>
    private static void RaiseOnUiThread()
    {
        // App.MainWindow is assigned as soon as the window is constructed, well
        // before StartActivationListener runs at the end of the launch — and the
        // per-account endpoint is claimed in between, so preferring the field with
        // a fallback is what stops a mid-startup raise from being dropped.
        var w = _window ?? App.MainWindow;
        w?.DispatcherQueue.TryEnqueue(() => TrayIconService.ShowMainWindow());
    }

    private static void SignalLoop(EventWaitHandle? handle, Action onSignal)
    {
        while (true)
        {
            try
            {
                handle!.WaitOne();
                var w = _window;
                if (w is null)
                {
                    continue;
                }
                // Marshal to the UI thread; SW_RESTORE + SetForegroundWindow and any
                // navigation must run there.
                w.DispatcherQueue.TryEnqueue(() => onSignal());
            }
            catch
            {
                break; // handle disposed / process tearing down — stop listening.
            }
        }
    }
}
