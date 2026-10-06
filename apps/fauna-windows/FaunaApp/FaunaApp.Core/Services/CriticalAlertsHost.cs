using uniffi.fauna_client_alerts;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Process-wide owner of the cross-page critical-alerts registry
/// (`docs/goal/behavior/critical-alerts.md`) — windows' twin of android's
/// <c>CriticalAlertsHost.kt</c> and linux's/tui's process- or app-scoped
/// registry holder. <see cref="FaunaFfiMethods.CriticalAlertsRegistry"/> hands
/// back the SAME process-wide handle every call — one native process = one
/// registry — so every FFI-exposed machine that hosts a feeder (today:
/// <c>AtprotoSettingsMachine</c>'s S4-C custody check, via
/// <c>build_atproto_settings_machine</c>) posts here with no wiring on this
/// class's part.
/// <para>
/// A lazily-built singleton constructed ONCE for the app's lifetime — not
/// re-subscribed per login/account-switch — the same "build once, reuse
/// across navigation" shape android's <c>AtprotoSettingsHost</c> uses to avoid
/// the per-visit-rebuild bug (a fresh subscription per re-entry would still
/// see every alert, since the registry itself is untouched, but would leak
/// one abandoned observer per switch).
/// </para>
/// </summary>
internal sealed class CriticalAlertsHost
{
    public static readonly CriticalAlertsHost Instance = new();

    private readonly CriticalAlerts _registry;
    private readonly ObserverImpl _observer;
    // Where OnChanged marshals back to, so the FFI callback thread never touches
    // bound state directly — the same pattern NestRpcClient.RaiseConnectionStateOnUi
    // uses for ConnectionStateChanged.
    //
    // ⚠ NOT captured in the constructor any more, and that was a real defect rather
    // than a style point. The old comment asserted "the UI thread — this singleton is
    // first touched from MainViewModel's ctor". That assertion was FALSE: the e2e
    // state provider reads SweepPassesStarted/Completed from the PushState pool
    // thread (App.xaml.cs, the convention-14 barrier pair added 2026-08-15) and
    // ActorScope.DropActorScopedState() calls ClearAll from the cmd-session pool
    // thread — both before any MainViewModel exists. Whichever ran first built this
    // singleton on a POOL thread, where SynchronizationContext.Current is null, and
    // a null context made RaiseChangedOnUi invoke handlers INLINE on the Rust FFI
    // callback thread. CriticalAlerts::post calls its observers synchronously
    // (fauna-client-alerts, notify()), so the repaint then ran inside the feeder's
    // own call stack and the session-start sweep never returned from the alert it had
    // just posted: the alarm sat in the registry, correct and complete, while the
    // banner it should have raised never repainted. Hence: settable,
    // latched from the UI thread, and never invoked inline.
    private SynchronizationContext? _uiContext;

    /// <summary>
    /// Raised on the captured UI <see cref="SynchronizationContext"/> whenever
    /// the active alert set changes — safe to touch bound state from directly.
    /// </summary>
    public event Action? Changed;

    private CriticalAlertsHost()
    {
        // Best effort only — this may well be a pool thread (see _uiContext).
        // CaptureUiContext() is what actually guarantees it.
        _uiContext = SynchronizationContext.Current;
        _registry = FaunaFfiMethods.CriticalAlertsRegistry();
        // Held as a field so the FFI callback vtable stays alive for the
        // process's lifetime (mirrors FeedViewModel's own observer-keepalive
        // comment for FfiFeedManager.AddObserver).
        _observer = new ObserverImpl(this);
        _registry.Subscribe(_observer);
    }

    /// <summary>
    /// Latch the UI <see cref="SynchronizationContext"/> that repaints marshal onto.
    /// Call from the UI thread; idempotent, and a no-op off the UI thread, so a
    /// pool-thread caller can never install its own (absent) context.
    /// </summary>
    public static void CaptureUiContext()
    {
        if (SynchronizationContext.Current is { } ctx)
            Instance._uiContext = ctx;
    }

    private void RaiseChangedOnUi()
    {
        var handler = Changed;
        if (handler is null) return;
        var ctx = _uiContext;
        if (ctx is not null)
        {
            ctx.Post(_ => handler(), null);
            return;
        }
        // No UI context latched yet. Hand the repaint to the thread pool rather than
        // running it here: this method is called synchronously by
        // CriticalAlerts::post, i.e. from INSIDE the posting feeder's own call stack
        // on the Rust FFI callback thread. A handler that touches bound WinUI state
        // off the UI thread does not merely fail — it takes the whole sweep pass down
        // with it, so the alarm never finishes being raised. Returning immediately
        // keeps the feeder whole either way; the repaint is then re-driven by
        // MainViewModel's own RefreshCriticalAlerts() once a context exists.
        ThreadPool.QueueUserWorkItem(_ => handler());
    }

    /// <summary>
    /// All active alerts, deterministic order. Empty ⇒ the `critical-alerts`
    /// banner is absent — the whole presence rule; callers never test the
    /// registry a second way.
    /// </summary>
    public CriticalAlertRow[] Active() => _registry.Active();

    /// <summary>
    /// Drop every active alert — the identity-teardown boundary (sign-out,
    /// account switch, factory reset; `critical-alerts.md` § Mechanism →
    /// *Lifetime*). A standing alert that survives an account switch would
    /// accuse the INCOMING account with the outgoing one's finding.
    /// </summary>
    public void ClearAll() => _registry.ClearAll();

    /// <summary>
    /// Count of sweep passes started so far — convention 14's causal-barrier
    /// pair with <see cref="SweepPassesCompleted"/>
    /// (<c>fauna_e2e_agent::ALERT_SWEEP_PASSES_KEY</c> owns the cross-app
    /// contract; <c>CriticalAlerts::sweep_passes_started</c> the reasoning).
    /// Monotonic and process-wide — never a windows-side counter.
    /// </summary>
    public ulong SweepPassesStarted() => _registry.SweepPassesStarted();

    /// <summary>The barrier's other half — see <see cref="SweepPassesStarted"/>.</summary>
    public ulong SweepPassesCompleted() => _registry.SweepPassesCompleted();

    private sealed class ObserverImpl : CriticalAlertsObserver
    {
        private readonly CriticalAlertsHost _host;
        public ObserverImpl(CriticalAlertsHost host) => _host = host;
        public void OnChanged()
        {
            // The alert set's history, one line per change (e2e trace only): what a
            // banner that is up — or unexpectedly down — is checked against.
            if (Logs.E2eTrace.Enabled)
            {
                var keys = string.Join(",", System.Linq.Enumerable.Select(_host._registry.Active(), r => r.key));
                Logs.E2eTrace.Write($"[critical-alerts] changed: [{keys}]");
            }
            _host.RaiseChangedOnUi();
        }
    }
}
