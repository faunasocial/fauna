using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The session-local muted-keyword cache + reveal set (moderation.md §
/// Muted keywords; content-moderation-and-ranking.md § Q3) — the
/// DmMessageBubble collapse's one source of truth for "is this message
/// muted". Calls the REAL FaunaFfiMethods.MatchesMutedKeywords export
/// (native dll loads in the test host —
/// reference_windows_dotnet_test_loads_native_ffi).
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class MutedKeywordsCacheTests : IDisposable
{
    // The cache is process-lifetime static state (the native twin of linux's
    // MUTED_KEYWORDS/REVEALED_MUTED thread-locals) — reset around each test
    // so cases don't leak into each other.
    //
    // Resetting is NOT sufficient on its own: xUnit parallelizes by class, and
    // MutedWordsViewModelTests resets the same static, so the two classes must
    // also be SERIALIZED against each other — hence the shared collection above
    // (defined in ActorScopeTests.cs, whose header explains the merge). Without it, one
    // class's reset lands mid-assertion in the other and the failure looks like
    // a product bug: cache empty in a full run, green in isolation.
    public MutedKeywordsCacheTests() => MutedKeywordsCache.Reset();
    public void Dispose() => MutedKeywordsCache.Reset();

    [Fact]
    public void IsMuted_EmptyList_NeverMutes()
    {
        Assert.False(MutedKeywordsCache.IsMuted("congrats you won the lottery", "msg-1"));
    }

    [Fact]
    public void IsMuted_MatchingBody_TrueUntilRevealed()
    {
        MutedKeywordsCache.SetKeywords(new[] { new uniffi.fauna_core.MutedKeyword("lottery", -1000) });

        Assert.True(MutedKeywordsCache.IsMuted("you WON the LOTTERY!", "msg-1"));

        MutedKeywordsCache.Reveal("msg-1");

        Assert.False(MutedKeywordsCache.IsMuted("you WON the LOTTERY!", "msg-1"));
    }

    [Fact]
    public void IsMuted_NonMatchingBody_False()
    {
        MutedKeywordsCache.SetKeywords(new[] { new uniffi.fauna_core.MutedKeyword("lottery", -1000) });

        Assert.False(MutedKeywordsCache.IsMuted("lunch tomorrow?", "msg-2"));
    }

    [Fact]
    public void Reveal_OnlyAffectsItsOwnMessageId()
    {
        MutedKeywordsCache.SetKeywords(new[] { new uniffi.fauna_core.MutedKeyword("lottery", -1000) });
        MutedKeywordsCache.Reveal("msg-1");

        Assert.True(MutedKeywordsCache.IsMuted("won the lottery", "msg-2"));
    }
}
