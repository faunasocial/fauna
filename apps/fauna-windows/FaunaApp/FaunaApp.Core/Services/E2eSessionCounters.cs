using System.Threading;

namespace FaunaApp.Core.Services;

/// <summary>
/// Convention 14's two negative-assert counters for windows —
/// <c>state.session_generation</c> (<c>fauna_e2e_agent::SESSION_GENERATION_KEY</c>)
/// and <c>state.activation_gestures</c>
/// (<c>fauna_e2e_agent::ACTIVATION_GESTURES_KEY</c>), which own the cross-app
/// contracts. The windows twin of linux's <c>automation::link</c> counters and
/// tui's <c>App::session_generation</c> field.
///
/// <para><b>Why the counters live in Core rather than beside the agent in
/// <c>App.xaml.cs</c>.</b> Their two writers sit on opposite sides of the
/// assembly line: the three session teardowns are app-layer
/// (<c>SwitchAccountHandler</c>, <c>SignOutHandler</c>,
/// <c>FactoryResetReonboardHandler</c> — they dispose WinUI-bound clients), while
/// the activation gesture completes in <c>AccountSwitcherViewModel</c>, in Core.
/// Core is the assembly both can reach (<c>InternalsVisibleTo("FaunaApp")</c>),
/// and putting them together keeps the two halves of one negative assert
/// readable as one thing.</para>
///
/// <para><b>The convention-15 shape is <see cref="E2eEnv"/>'s</b> — a gated real
/// plus a <i>same-signature</i> production twin — and here the twin is doing more
/// work than it looks. Every writer below is a <b>production</b> call site
/// (a real sign-out, a real switch, a real row tap), so the alternative would be
/// sprinkling <c>#if</c> over five statements in three files and re-deciding the
/// gate at each future teardown path someone adds. With the twin, the call sites
/// compile unconditionally and read as ordinary code; a Release build gets a
/// no-op whose counters are unobservable because the agent that would publish
/// them is itself compiled out.</para>
///
/// <para><b>Interlocked, not <c>volatile</c> +
/// <c>++</c>.</b> Unlike <c>App</c>'s barrier probes (single writer, the UI
/// thread), these are incremented from the UI thread and read from the agent's
/// poll thread, and the switch path can be re-entered from a second row click
/// before the first awaits out. A lost update here would under-count a teardown
/// — the one direction that turns a negative assert into a false pass.</para>
/// </summary>
internal static class E2eSessionCounters
{
#if DEBUG || FAUNA_E2E_AGENT

    private static long _sessionGeneration;
    private static long _activationGestures;

    /// <summary>
    /// Count one <b>initiated</b> authenticated-session teardown.
    ///
    /// <para>⚠ Call this <b>synchronously in the handler that decides to tear
    /// down</b>, never from inside a <c>DispatcherQueue.TryEnqueue</c>
    /// continuation. Windows' teardown handlers hand their UI work to the
    /// dispatcher (<c>SignOutHandler</c> and <c>FactoryResetReonboardHandler</c>
    /// enqueue their re-root; the switch awaits), and <c>barrier</c> is itself a
    /// <c>DispatcherQueue</c> round trip on the agent's post-action rail — so a
    /// bump made inside an enqueued continuation lands <i>behind</i> the very
    /// barrier meant to observe it, and the negative assert silently reverts to
    /// the race it replaced. Bumping at the decision is convention 14's own
    /// corollary asked of the product rather than the test.</para>
    /// </summary>
    internal static void RecordSessionTeardown() => Interlocked.Increment(ref _sessionGeneration);

    /// <summary>The value for <c>state.session_generation</c>.</summary>
    internal static long SessionGeneration => Interlocked.Read(ref _sessionGeneration);

    /// <summary>
    /// Count one <b>completed</b> account-activation gesture — a tap on an
    /// <c>account-switcher-item</c> row, counted once its handler has returned
    /// whatever it decided.
    ///
    /// <para>Bump this in a <c>finally</c>, so a refusal or a thrown error counts
    /// too: a gesture that failed still finished, and a completion observable
    /// that only fires on the happy path would make a waiting test hang rather
    /// than fail.</para>
    /// </summary>
    internal static void RecordActivationGesture() => Interlocked.Increment(ref _activationGestures);

    /// <summary>The value for <c>state.activation_gestures</c>.</summary>
    internal static long ActivationGestures => Interlocked.Read(ref _activationGestures);

#else

    // Production twins. Same signatures, no-ops — the call sites are ordinary
    // product code and stay unchanged by construction.
    internal static void RecordSessionTeardown() { }
    internal static long SessionGeneration => 0;
    internal static void RecordActivationGesture() { }
    internal static long ActivationGestures => 0;

#endif
}
