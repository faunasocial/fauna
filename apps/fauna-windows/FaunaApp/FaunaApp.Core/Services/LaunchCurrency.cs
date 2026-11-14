using System.Collections.Generic;
using System.Threading;
using FaunaApp.Core.Logs;
using uniffi.fauna_launch_machine;

namespace FaunaApp.Core.Services;

/// <summary>
/// Which launch is the current one — windows' twin of tui's <c>launch::deliver</c> /
/// <c>launch::retire_in_flight</c>, linux's <c>launch_currency</c>, android's
/// <c>LaunchCurrency</c> and apple's <c>LaunchMachineBox</c> (<c>onboarding.md</c>
/// § App-launch routing → <i>A verdict renders only while its own launch is still the
/// current one</i>; multiple-accounts outcome 17).
///
/// <para>A launch verdict is decided from a store read taken when the launch started
/// and rendered a round trip later. Every asynchronous launch drive (the cold start,
/// the post-wizard landing, the account switch and the teardown re-entry it shares
/// with the remove-account promote, the abandoned-append restore, and Retry / Trust
/// on the machine already on screen) passes <c>App.DriveLaunchAsync</c>, which
/// <see cref="Begin"/>s its machine here before it awaits and asks
/// <see cref="Deliver"/> when the await returns. A newer drive replaces the held
/// machine; the paths that supersede a launch without starting one — the e2e session
/// patch, sign-out, the factory-reset re-onboard, the switch's teardown — clear it
/// (<see cref="RetireInFlight"/>). A verdict from a machine that is no longer held is
/// dropped whole.</para>
///
/// <para>It is an OWNERSHIP test, never an "is a session present?" one: the held
/// launch's verdict still routes in full over a live session, which the
/// <c>IdentityChanged</c> teardown depends on. The held machine is the IN-FLIGHT one
/// only: <see cref="Deliver"/> lets go of it when the verdict routes, so a settled
/// launch is never retired (its machine lives on as the session's bearer source, held
/// by the app, not here).</para>
///
/// <para><b>Always on, unlike <see cref="E2eSessionCounters"/>.</b> The drop is product
/// behaviour, and the counters are two increments beside it; only the agent that
/// publishes them (<c>fauna_e2e_agent::LAUNCH_VERDICTS_KEY</c>) is compiled out of a
/// Release build. <b>A lock, not the UI thread alone:</b> the drives begin and deliver
/// on the UI thread, but the e2e session patch retires from the agent's command
/// thread.</para>
/// </summary>
internal sealed class LaunchCurrency
{
    /// <summary>The one instance the app reads; tests build their own.</summary>
    internal static LaunchCurrency Shared { get; } = new();

    private readonly object _gate = new();

    /// <summary>The launch whose verdict the app is waiting for.</summary>
    private ILaunchMachine? _inFlight;

    private long _routed;
    private long _superseded;

    /// <summary>
    /// <paramref name="machine"/> is now the launch in flight: any earlier one still
    /// waiting is superseded, and its verdict will be dropped when it lands.
    /// </summary>
    internal void Begin(ILaunchMachine machine)
    {
        lock (_gate)
        {
            _inFlight = machine;
        }
    }

    /// <summary>
    /// May <paramref name="machine"/>'s delivered verdict be rendered? <c>true</c> only
    /// while it is still the launch in flight — which it then stops being (the verdict
    /// settles it). Counts the verdict either way, before anything is rendered or
    /// dropped.
    /// </summary>
    internal bool Deliver(ILaunchMachine machine)
    {
        bool current;
        lock (_gate)
        {
            current = ReferenceEquals(_inFlight, machine);
            if (current)
            {
                _inFlight = null;
            }
        }
        if (current)
        {
            Interlocked.Increment(ref _routed);
        }
        else
        {
            Interlocked.Increment(ref _superseded);
            ShellLog.Info("LaunchCurrency", "[launch] verdict superseded before it rendered — dropping it");
        }
        return current;
    }

    /// <summary>
    /// Retire a launch still in flight because something newer now owns the routing (a
    /// session built without a launch, a sign-out, a re-onboard, a switch's teardown).
    /// A settled launch holds nothing here, so this is a no-op for it. Returns whether
    /// a launch was retired.
    /// </summary>
    internal bool RetireInFlight()
    {
        lock (_gate)
        {
            if (_inFlight is null)
            {
                return false;
            }
            _inFlight = null;
        }
        ShellLog.Info("LaunchCurrency", "[launch] a newer session retired the launch still in flight");
        return true;
    }

    /// <summary>Verdicts that rendered.</summary>
    internal long Routed => Interlocked.Read(ref _routed);

    /// <summary>Verdicts dropped because their launch was no longer the current one.</summary>
    internal long Superseded => Interlocked.Read(ref _superseded);

    /// <summary><c>fauna_e2e_agent::LAUNCH_VERDICTS_KEY</c>'s value.</summary>
    internal Dictionary<string, long> Observable => new()
    {
        ["routed"] = Routed,
        ["superseded"] = Superseded,
    };
}
