using System.ComponentModel;
using Microsoft.UI.Dispatching;
using uniffi.fauna_onboarding_machine;

namespace FaunaApp.Onboarding;

/// <summary>
/// Bridge between the UniFFI <see cref="OnboardingObserver"/> trait (called from
/// arbitrary Tokio threads on the Rust side) and WinUI's <see cref="INotifyPropertyChanged"/>
/// (which must fire on the UI thread).
///
/// Lives in the FaunaApp WinUI project — <see cref="DispatcherQueue"/> is part
/// of WindowsAppSDK and only compiles under the windows10.0 TFM.
/// </summary>
internal sealed class NotifyObserver : OnboardingObserver, INotifyPropertyChanged
{
    private readonly DispatcherQueue? _dispatcher;

    public event PropertyChangedEventHandler? PropertyChanged;

    // ── UI-thread saturation instrument ──────────────────────────────────────
    // This callback re-evaluates EVERY binding on the onboarding page (empty
    // property name), and each OneWay source is an FFI call taking the machine's
    // state lock — so its cost is (notification rate x page size), and when that
    // approaches the whole thread the app stops being drivable rather than
    // stopping working. The decision about what counts as pathological lives in
    // FaunaApp.Core.Helpers.UiThreadBudget, where it is unit-tested and shared
    // with the other observers of the same shape; all this file does is time the
    // invoke and print what comes back. A healthy page prints nothing.
    private static readonly global::FaunaApp.Core.Helpers.UiThreadBudget _budget = new();

    // Where the notifications COME FROM, which is the one thing the budget above
    // cannot say. A storm sustained at "as fast as the thread can go" has two very
    // different explanations with identical symptoms: the re-evaluation itself
    // provokes the next notification (a 1:1 feedback loop through the binding
    // graph), or an independent producer is notifying that fast on its own. Those
    // live in different codebases — the app's bindings versus the machine's spawned
    // tasks — so guessing costs a whole diagnostic cycle either way.
    //
    // `_invoking` is set on the UI thread around the re-evaluation and read from
    // whatever Tokio thread calls in. That read is deliberately unsynchronised: an
    // exact count is not the question, the RATIO is, and a lock here would perturb
    // the very contention being measured.
    private static volatile bool _invoking;
    private static int _reentrant;   // arrived while a re-evaluation was running
    private static int _external;    // arrived while the thread was otherwise idle

    public NotifyObserver()
    {
        // Capture the UI dispatcher at construction time. The VM is built on
        // the UI thread when the OnboardingPage is constructed, so this picks
        // up the right queue.
        _dispatcher = DispatcherQueue.GetForCurrentThread();
    }

    public void OnChanged()
    {
        if (_invoking) global::System.Threading.Interlocked.Increment(ref _reentrant);
        else global::System.Threading.Interlocked.Increment(ref _external);

        // Empty property name = WinUI re-evaluates every binding. UniFFI may
        // call us from a Tokio worker, so marshal back to the UI thread before
        // raising INotifyPropertyChanged.
        if (_dispatcher is null)
        {
            // Already on a thread without a dispatcher (test, or shutdown). Fire
            // synchronously — caller is responsible for thread affinity.
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(string.Empty));
            return;
        }

        _dispatcher.TryEnqueue(() =>
        {
            // ⚠ The try/catch is load-bearing, not defensive habit. An exception
            // thrown inside a DispatcherQueue callback unwinds into the WinRT
            // dispatcher, which cannot propagate it: the runtime STOWS it and
            // fail-fasts the process with 0xC000027B — and no managed hook sees it
            // (measured 2026-08-31: Application.UnhandledException,
            // AppDomain.CurrentDomain.UnhandledException and
            // TaskScheduler.UnobservedTaskException were all installed and ALL
            // stayed silent through the crash).
            //
            // This callback is an unusually wide blast radius for that: the empty
            // property name tells WinUI to re-evaluate EVERY binding on the page,
            // so any one throwing getter anywhere in the onboarding tree takes the
            // whole process down, with no stack and no log. Catching here converts
            // that into a line naming the culprit, on the one channel that outlives
            // the app (the e2e bridge inherits stderr).
            var t0 = global::System.Diagnostics.Stopwatch.GetTimestamp();
            _invoking = true;
            try
            {
                PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(string.Empty));
            }
            catch (global::System.Exception ex)
            {
                global::System.Console.Error.WriteLine(
                    "[fauna] FATAL (onboarding binding re-evaluation) — a bound "
                    + "property threw while WinUI refreshed the onboarding page; "
                    + "unhandled, this stows and fail-fasts the process: " + ex);
                global::System.Console.Error.Flush();
            }
            _invoking = false;
            ReportIfSaturated(t0);
        });
    }

    /// <summary>Hand the completed re-evaluation to the budget and print only what
    /// it decides is pathological. Runs on the UI thread, inside the dispatched
    /// callback, so the single shared budget needs no synchronisation.</summary>
    private static void ReportIfSaturated(long invokeStartedAt)
    {
        var freq = (double)global::System.Diagnostics.Stopwatch.Frequency;
        var now = global::System.Diagnostics.Stopwatch.GetTimestamp();
        var report = _budget.Record(
            nowSeconds: now / freq,
            invokeMs: (now - invokeStartedAt) / freq * 1000.0);
        if (report is null) return;

        // ShellLog, NOT Console.Error. This app is a GUI-subsystem process and the
        // bridge starts it with UseShellExecute=false and NO redirection, so .NET
        // passes bInheritHandles=false and the child gets no std handles at all:
        // every Console.Error.WriteLine from this process is written to an invalid
        // handle and silently discarded. That matters more than usual here, because
        // this instrument's whole value is that its SILENCE is evidence — and
        // silence on a dead channel is evidence of nothing. ShellLog feeds the
        // native ring that lands in <data_dir>/logs/, which `app_log_text` attaches
        // to every failing test, and it is proven to arrive (the `open-url:
        // suppressed under the e2e harness` line reaches the report this way).
        var reentrant = global::System.Threading.Interlocked.Exchange(ref _reentrant, 0);
        var external = global::System.Threading.Interlocked.Exchange(ref _external, 0);
        global::FaunaApp.Core.Logs.ShellLog.Warn(
            "Onboarding",
            report.Value.ToString()
            + $" Arrivals this window: {reentrant} re-entrant (during a re-evaluation), "
            + $"{external} external (thread otherwise idle) — re-entrant-dominant means "
            + "the binding graph is provoking its own next notification, external-dominant "
            + "means an independent producer is notifying this fast on its own.");
    }
}
