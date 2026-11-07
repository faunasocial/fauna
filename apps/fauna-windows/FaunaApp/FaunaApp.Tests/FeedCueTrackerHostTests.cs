using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// The engagement-cue capture tracker is the auth's, never one per Feed-page mount
/// (<c>e2e-latency-independent-assertions.md</c> § Implementation status today, the
/// frozen engagement-cue clock bullet): what it holds — the frozen test clock
/// included — must outlive a trip off the page, and a rebuilt manager or an actor
/// change must start a fresh one. windows built a new tracker on every
/// <c>Page_Loaded</c> until <see cref="FeedManagerHost.CueTrackerFor"/>; android's
/// <c>FeedManagerHost.cueTracker</c> is the twin. Builds fully offline (mirrors
/// <see cref="FeedViewModelAttachmentTests"/>): <c>NestClient::new</c> opens no socket.
///
/// <para>In <c>ActorScopedStaticsGlobal</c>: it installs managers into the host's
/// process-wide slot and resets it.</para>
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class FeedCueTrackerHostTests
{
    private static bool Frozen(FfiCueTracker tracker)
        => tracker.ProbeForTest().Contains("\"frozen\":true");

    /// <summary>Make <paramref name="manager"/> the host's current one, as the Feed
    /// page's build does — a fresh transport object is a re-auth, so it always
    /// installs.</summary>
    private static Task<FfiFeedManager> Install(FfiFeedManager manager)
        => FeedManagerHost.GetOrBuildAsync(new object(), () => Task.FromResult(manager));

    [Fact]
    public async Task OneManagerKeepsOneTracker_SoAFrozenClockOutlivesAPageMount()
    {
        using var nest = new FfiNestClient("wss://127.0.0.1:0/ws", new byte[32]);
        using var manager = nest.FeedManager(new byte[32]);
        try
        {
            await Install(manager);

            var firstMount = FeedManagerHost.CueTrackerFor(manager);
            firstMount.FreezeClockForTest(1_000UL, 2_000UL);
            var nextMount = FeedManagerHost.CueTrackerFor(manager);

            Assert.Same(firstMount, nextMount);
            Assert.True(Frozen(nextMount));
        }
        finally
        {
            FeedManagerHost.ResetForActorChange();
        }
    }

    [Fact]
    public async Task ARebuiltManagerGetsAFreshTracker()
    {
        using var nest = new FfiNestClient("wss://127.0.0.1:0/ws", new byte[32]);
        using var before = nest.FeedManager(new byte[32]);
        using var after = nest.FeedManager(new byte[32]);
        try
        {
            await Install(before);
            var old = FeedManagerHost.CueTrackerFor(before);
            old.FreezeClockForTest(1_000UL, 2_000UL);

            await Install(after);
            var fresh = FeedManagerHost.CueTrackerFor(after);

            Assert.NotSame(old, fresh);
            Assert.False(Frozen(fresh));
        }
        finally
        {
            FeedManagerHost.ResetForActorChange();
        }
    }

    [Fact]
    public async Task AnActorChangeDropsTheTracker_AndTheOutgoingManagerKeepsNone()
    {
        using var nest = new FfiNestClient("wss://127.0.0.1:0/ws", new byte[32]);
        using var manager = nest.FeedManager(new byte[32]);
        try
        {
            await Install(manager);
            var outgoing = FeedManagerHost.CueTrackerFor(manager);
            outgoing.FreezeClockForTest(1_000UL, 2_000UL);

            FeedManagerHost.ResetForActorChange();
            // The left page's teardown runs after the reset: it gets an empty,
            // unkept tracker, never the outgoing identity's frozen one.
            var teardown = FeedManagerHost.CueTrackerFor(manager);
            var again = FeedManagerHost.CueTrackerFor(manager);

            Assert.NotSame(outgoing, teardown);
            Assert.NotSame(teardown, again);
            Assert.False(Frozen(teardown));
        }
        finally
        {
            FeedManagerHost.ResetForActorChange();
        }
    }

    /// <summary>The rule a left Feed page paints by: between the actor change's reset
    /// and the page's own teardown, the outgoing manager still ticks, and a page that
    /// repainted on it re-published the outgoing identity's window to the e2e state
    /// after the reset had cleared it (the windows archive-import interaction red: the
    /// follower's tap read the owner's ♥ and the owner's card order).</summary>
    [Fact]
    public async Task OnlyTheInstalledManagerIsHeld_NeverOneAnActorChangeOrARebuildLeftBehind()
    {
        using var nest = new FfiNestClient("wss://127.0.0.1:0/ws", new byte[32]);
        using var outgoing = nest.FeedManager(new byte[32]);
        using var incoming = nest.FeedManager(new byte[32]);
        try
        {
            await Install(outgoing);
            Assert.True(FeedManagerHost.Holds(outgoing));

            FeedManagerHost.ResetForActorChange();
            Assert.False(FeedManagerHost.Holds(outgoing));

            await Install(incoming);
            Assert.True(FeedManagerHost.Holds(incoming));
            Assert.False(FeedManagerHost.Holds(outgoing));
        }
        finally
        {
            FeedManagerHost.ResetForActorChange();
        }
    }
}
