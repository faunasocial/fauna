using System;
using System.Threading;
using System.Threading.Tasks;
using uniffi.fauna_feed;
using uniffi.fauna_ffi;

namespace FaunaApp.Core;

/// <summary>
/// The ONE <see cref="FfiFeedManager"/> for the current session — the feed's
/// twin of <c>FaunaApp.Conversations.ConversationsManagerHost</c>, and windows'
/// analogue of linux's <c>apps/fauna-linux/src/feed/host.rs</c>.
///
/// <para><b>Lifetime: per authenticated session, NOT per Feed-page load.</b>
/// <c>feed.md</c> § State &amp; data shape makes <c>FeedManager</c> "the direct
/// analogue of <c>fauna_conversations::ConversationsManager</c> … it mirrors
/// that shape exactly", and the ratified rebuild boundary is re-auth /
/// account-switch ("linux's (like tui's) is rebuilt per re-auth/account-switch
/// rather than an eternal singleton", <c>feed.md</c> § Implementation status
/// today). Windows was the one app that rebuilt it on every
/// <c>FeedPage.Page_Loaded</c> instead, which is a genuine per-app divergence
/// (priority #1/#3) and not merely a test artifact: a navigation away and back
/// discarded the loaded page, the selected feed, the composer state and the
/// engagement-cue tracking, and re-fetched the whole timeline from scratch.</para>
///
/// <para><b>What that cost, concretely.</b> Two bugs so far. The first was the
/// draft-persistence race — an untouched, freshly-rebuilt manager wrote a
/// spurious empty snapshot over a real pending save (<c>ui-actual-windows.yaml</c>,
/// the 2026-08-26 entry: "the manager rebuilds per page load"). The second is
/// the one this host closes: the shared reconnect-rehydrate barrier reads the
/// manager's own monotonic reload generation
/// (<c>fauna_e2e_agent::FEED_RELOADS_KEY</c>), so a mid-test rebuild makes the
/// counter go BACKWARDS and puts the release condition out of reach by
/// construction — <c>test_nest_flip_feed_rehydrate[windows]</c> measured
/// baseline 4 → <c>started=2</c>.</para>
///
/// <para><b>Rebuild is keyed on the transport, so it cannot be forgotten.</b>
/// <see cref="GetOrBuildAsync"/> takes the <c>INestRpcClient</c> the manager
/// would ride and rebuilds whenever that object differs from the one the cached
/// manager was built over. Re-auth installs a fresh <c>NestRpcClient</c>, so a
/// re-auth rebuilds automatically — no new call site has to remember to reset
/// the host, which is the failure mode a plain "reset me at every login" seam
/// would have. <see cref="ResetForActorChange"/> stays as the explicit actor
/// boundary (<c>App.DropActorScopedState</c>), for the same reason its
/// conversations twin has one: an actor change must drop the outgoing
/// identity's state even where the transport object happens to be reused.</para>
///
/// <para>Lives in <c>FaunaApp.Core</c>, a layer BELOW both <c>Services</c> and
/// <c>ViewModels</c>, so neither has to depend on the other to reach the
/// manager — the reason its predecessor <c>ActiveFeedManagerHolder</c> was put
/// here. <c>Services.NestRpcClient</c>'s signal-share wrappers need the exact
/// instance the Feed page observes (the manager caches that opt-in for its own
/// signal producer), and <c>ViewModels</c> already depends on <c>Services</c>
/// via <c>FeedViewModel</c>'s <c>INestRpcClient</c> parameter, so reading
/// <c>FeedViewModel.Current</c> from <c>Services</c> would close a namespace
/// cycle.</para>
/// </summary>
internal static class FeedManagerHost
{
    private static readonly SemaphoreSlim Gate = new(1, 1);

    private static FfiFeedManager? _current;

    /// <summary>The transport the cached manager was built over. Held strongly:
    /// it is replaced on the next rebuild and cleared at an actor change, so at
    /// most one already-disposed client is ever pinned.</summary>
    private static object? _builtOver;

    /// <summary>Unsubscribes the session-level reconnect re-pull (see
    /// <see cref="GetOrBuildAsync"/>); replaced on rebuild, cleared at an actor change.</summary>
    private static Action? _offPageReconnectUnsub;

    /// <summary>The cached manager's cue hydrate (<see cref="HydrateCuesOnceAsync"/>),
    /// once started; dropped with the manager.</summary>
    private static readonly OnceTask CueHydrate = new();

    /// <summary>Guards <see cref="_cueTracker"/> and <see cref="_cueTrackerManager"/>:
    /// the capture reads them on the UI thread, a player's reading and the test
    /// agent may arrive from elsewhere.</summary>
    private static readonly object CueTrackerLock = new();

    /// <summary>The engagement-cue capture tracker of <see cref="_cueTrackerManager"/>
    /// (<see cref="CueTrackerFor"/>).</summary>
    private static FfiCueTracker? _cueTracker;
    private static FfiFeedManager? _cueTrackerManager;

    /// <summary>
    /// The session's live manager, or <c>null</c> before the Feed page has built
    /// one (a state-protocol deep link straight to Settings can reach the e2e
    /// state serializer first) and after an actor change. Readers only — the
    /// build goes through <see cref="GetOrBuildAsync"/>.
    /// </summary>
    public static FfiFeedManager? Current => _current;

    /// <summary>
    /// Whether <paramref name="manager"/> is the session's live one: false for a
    /// manager an actor change dropped (<see cref="ResetForActorChange"/>) or a
    /// re-auth replaced. Such a manager can still tick — its last reload landing,
    /// its transport closing — between the reset and the teardown of the page that
    /// observes it, and anything that page does on that tick belongs to the outgoing
    /// identity. The one rule behind the cue hydrate, the cue tracker and the Feed
    /// page's repaint.
    /// </summary>
    public static bool Holds(FfiFeedManager manager) => ReferenceEquals(manager, _current);

    /// <summary>
    /// The session's manager, built on first use over <paramref name="transport"/>
    /// and reused by every later Feed-page load. Rebuilt when
    /// <paramref name="transport"/> is a different object than the cached
    /// manager was built over (i.e. on re-auth).
    /// </summary>
    /// <param name="transport">The <c>INestRpcClient</c> the manager rides —
    /// the rebuild key, compared by reference.</param>
    /// <param name="build">Builds a fresh manager over that transport.</param>
    public static async Task<FfiFeedManager> GetOrBuildAsync(
        object transport, Func<Task<FfiFeedManager>> build)
    {
        if (_current is { } live && ReferenceEquals(_builtOver, transport)) return live;

        // Page_Loaded is `async void` on the UI thread, so two loads can be in
        // flight at once (a repeat nav while the first build's await is
        // outstanding). Without the gate both would build, and the loser's
        // manager would be the one the page observes while the holder published
        // the winner's — the split-instance bug this whole host exists to
        // prevent, reintroduced one level down.
        await Gate.WaitAsync();
        try
        {
            if (_current is { } raced && ReferenceEquals(_builtOver, transport)) return raced;
            var built = await build();
            _offPageReconnectUnsub?.Invoke();
            _offPageReconnectUnsub = null;
            CueHydrate.Reset();
            _current = built;
            _builtOver = transport;
#if DEBUG || FAUNA_E2E_AGENT
            // Armed before this manager existed (a seat signed in off the feed):
            // held from birth, so its first hydrate parks. After `_current` is set,
            // so an arm racing this build reaches the manager one way or the other.
            if (_cueHydrationHoldForTest) built.SetCueHydrationHoldForTest(true);
#endif
            if (transport is Services.INestRpcClient rpc)
            {
                void OnReconnected()
                {
                    // The Feed page re-pulls on its own while it is open
                    // (FeedViewModel's RefreshOnReconnect); this covers the session
                    // being on ANY other page, where nothing else would re-query the
                    // feed (transport.md § Push events — observers re-pull on
                    // reconnect; the manager is session-scoped, so its re-pull is
                    // too). Raised on the UI thread.
                    if (ViewModels.FeedViewModel.Current is { IsReconnectWired: true }) return;
                    if (!ReferenceEquals(_current, built)) return;
                    _ = RepullAsync(built);
                }
                rpc.Reconnected += OnReconnected;
                _offPageReconnectUnsub = () => rpc.Reconnected -= OnReconnected;
            }
            return built;
        }
        finally
        {
            Gate.Release();
        }
    }

    /// <summary>
    /// Fetch-on-session-start for the sealed <c>cues:v1</c> rollup
    /// (<c>engagement-cues.md</c> § At rest) — ONCE per manager, the twin of web's
    /// <c>hydrateCuesOnce</c> (linux and tui get the same by construction: their
    /// hydrate runs where the manager is built). <c>HydrateCues</c> replaces the
    /// live engine, so a second call would discard every verdict folded since the
    /// first and not yet put; the Feed page loading again must not re-run it. A
    /// failed hydrate is forgotten, so the next load retries, and throws: the
    /// caller surfaces it (an unopenable rollup is a reported error, never a
    /// silent fresh start).
    /// </summary>
    public static Task HydrateCuesOnceAsync(FfiFeedManager manager)
        => Holds(manager)
            ? CueHydrate.RunAsync(() => manager.HydrateCues())
            // A manager this host no longer holds (an actor change mid-load) is
            // hydrated where it stands and cached nowhere.
            : manager.HydrateCues();

    /// <summary>
    /// The ONE engagement-cue capture tracker of <paramref name="manager"/> — one per
    /// auth, never one per Feed-page mount, so what the tracker holds (its frozen test
    /// clock included, <c>fauna_feed::CueTracker</c> § The test clock) outlives a trip
    /// off the page, as on tui, linux, web (<c>cueTrackerFor</c>), android
    /// (<c>FeedManagerHost.cueTracker</c>) and apple (<c>FeedVM.cueTracker()</c>). A
    /// rebuilt manager (a re-auth or an account switch) gets a fresh tracker, and
    /// <see cref="ResetForActorChange"/> drops it with the manager. The page's
    /// <c>Feed.CueViewportObserver</c> drains it when the page is left, so a later
    /// mount still starts with nothing tracked.
    ///
    /// <para>Only the host's <see cref="Current"/> manager has a kept tracker, the rule
    /// <see cref="HydrateCuesOnceAsync"/> follows: a manager this host no longer holds
    /// (a page left after an actor change, whose teardown runs after the reset) gets a
    /// throwaway one, empty, so it records nothing for the outgoing identity and keeps
    /// no reference to its manager.</para>
    ///
    /// <para>windows' <c>PostsList</c> is a non-virtualizing <c>StackPanel</c>: every
    /// card stays realized wherever the list is scrolled, so a card the sample could
    /// not read is held, not ended — <see cref="LeaveModel.HoldUnmeasured"/>, linux's
    /// and apple's model; android's virtualized list takes the opposite one.</para>
    /// </summary>
    public static FfiCueTracker CueTrackerFor(FfiFeedManager manager)
    {
        lock (CueTrackerLock)
        {
            if (!Holds(manager)) return new FfiCueTracker(LeaveModel.HoldUnmeasured);
            if (_cueTracker is { } live && ReferenceEquals(_cueTrackerManager, manager)) return live;
            // The replaced tracker is not disposed here: a sample or a player's
            // reading may still hold it, and the binding's finalizer frees it once
            // nothing does.
            _cueTracker = new FfiCueTracker(LeaveModel.HoldUnmeasured);
            _cueTrackerManager = manager;
            return _cueTracker;
        }
    }

#if DEBUG || FAUNA_E2E_AGENT
    /// <summary>The e2e cue-hydration hold as last armed (<see cref="SetCueHydrationHoldForTest"/>),
    /// kept for a manager not built yet.</summary>
    private static volatile bool _cueHydrationHoldForTest;

    /// <summary>
    /// Set or clear the e2e cue-hydration hold (<c>feed_cue_hydration_hold</c>; the
    /// shared <c>fauna_feed::FeedManager::set_cue_hydration_hold_for_test</c>): set, the
    /// manager's rollup fetch parks inside <c>HydrateCues</c> until it is cleared, so a
    /// test can drive a second device before it has fetched (engagement-cues.md § At
    /// rest). The in-memory lever web and apple use too — the windows driver builds the
    /// app's launch environment from scratch, so the hold file tui and linux take never
    /// reaches this process. windows builds its manager only when the Feed page first
    /// loads, so a seat that signs in off the feed arms the hold here and the build
    /// applies it (<see cref="GetOrBuildAsync"/>); a live manager takes it at once, and
    /// a parked hydrate goes on at its next poll once it is cleared. Compiled out of
    /// release artifacts with the agent that calls it (convention 15).
    /// </summary>
    public static void SetCueHydrationHoldForTest(bool held)
    {
        _cueHydrationHoldForTest = held;
        _current?.SetCueHydrationHoldForTest(held);
    }
#endif

    /// <summary>The page-less twin of <c>FeedViewModel.ReloadAsync</c>: the same
    /// shared seams, in the same order. A fault is logged, never thrown into the
    /// UI-thread event raise.</summary>
    private static async Task RepullAsync(FfiFeedManager manager)
    {
        try
        {
            await manager.RefreshFeeds();
            await manager.RefreshBridgeFeeds();
            await manager.RefreshAvailableBridges();
            await manager.RefreshCurrentFeed();
        }
        catch (Exception ex)
        {
            Logs.ShellLog.Warn("FeedManagerHost", $"off-page reconnect re-pull failed: {ex.GetType().Name}: {ex.Message}");
        }
    }

    /// <summary>
    /// End the outgoing identity's feed state at an actor change — a switch, a
    /// sign-out or a factory-reset re-onboard. The twin of
    /// <c>ConversationsManagerHost.ResetForActorChange</c> and of linux's
    /// <c>feed::host</c> drop; called only from <c>App.DropActorScopedState</c>.
    /// </summary>
    public static void ResetForActorChange()
    {
        _offPageReconnectUnsub?.Invoke();
        _offPageReconnectUnsub = null;
        CueHydrate.Reset();
        _current = null;
        _builtOver = null;
        lock (CueTrackerLock)
        {
            _cueTracker = null;
            _cueTrackerManager = null;
        }
    }
}

/// <summary>
/// A task run at most once until it succeeds: the first <see cref="RunAsync"/>
/// starts it and every later call shares that same run, in flight or done; a run
/// that faulted or was cancelled is replaced by the next call's fresh one.
/// <see cref="Reset"/> forgets a run outright (the thing it ran for was
/// replaced). Callers are on the UI thread, so no lock guards the slot.
/// </summary>
internal sealed class OnceTask
{
    private Task? _run;

    public Task RunAsync(Func<Task> start)
    {
        if (_run is { IsFaulted: false, IsCanceled: false } running) return running;
        return _run = start();
    }

    public void Reset() => _run = null;
}
