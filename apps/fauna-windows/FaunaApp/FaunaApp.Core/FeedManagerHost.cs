using System;
using System.Threading;
using System.Threading.Tasks;
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

    /// <summary>
    /// The session's live manager, or <c>null</c> before the Feed page has built
    /// one (a state-protocol deep link straight to Settings can reach the e2e
    /// state serializer first) and after an actor change. Readers only — the
    /// build goes through <see cref="GetOrBuildAsync"/>.
    /// </summary>
    public static FfiFeedManager? Current => _current;

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
            _current = built;
            _builtOver = transport;
            return built;
        }
        finally
        {
            Gate.Release();
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
        _current = null;
        _builtOver = null;
    }
}
