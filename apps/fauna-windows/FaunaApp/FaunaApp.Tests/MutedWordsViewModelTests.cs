using System;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The Settings "Muted words" sub-page VM (moderation.md § Muted keywords):
/// add/list/remove a single user-global sealed keyword list over
/// <c>muted_keywords_{list,set}</c> (INestRpcClient.MutedKeywords{List,Set}Async).
/// Every load/mutation also repopulates <see cref="MutedKeywordsCache"/> — the
/// load-bearing step that keeps the conversation bubble collapse in sync with
/// every edit (mirrors linux's <c>apply()</c> updating
/// <c>crate::conversations::set_muted_keywords_cache</c>).
///
/// <para><b>Serialized against <see cref="MutedKeywordsCacheTests"/>.</b>
/// <c>MutedKeywordsCache</c> is process-global static state and BOTH classes reset it
/// in their ctor/Dispose. xUnit parallelizes by class, so without a shared collection
/// the sibling's <c>Reset()</c> lands between this class's
/// <c>SetKeywords</c> and its cache assertion — the test then fails with an empty
/// <c>MutedKeywordsCache.Words</c> while passing in isolation. Same reason and same
/// mechanism as <c>StringsGlobalCollection</c>.</para>
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class MutedWordsViewModelTests : IDisposable
{
    public MutedWordsViewModelTests() => MutedKeywordsCache.Reset();
    public void Dispose() => MutedKeywordsCache.Reset();

    [Fact]
    public async Task LoadAsync_PopulatesWords_AndCache()
    {
        var rpc = new MockNestRpcClient { NextMutedKeywords = new[] { "spam", "lottery" } };
        var vm = new MutedWordsViewModel(rpc);

        await vm.LoadAsync();

        Assert.Contains("MutedKeywordsList", rpc.Calls);
        Assert.Equal(new[] { "spam", "lottery" }, vm.Words);
        Assert.Equal(new[] { "spam", "lottery" }, MutedKeywordsCache.Words);
        Assert.Null(vm.ErrorMessage);
    }

    /// <summary>The add gesture sends only the DELTA — never the page's list
    /// wholesale: the shared seam re-reads the stored list
    /// inside its own CAS update, so a term another device stored since this
    /// page loaded survives. The VM re-renders from the seam's returned
    /// stored list.</summary>
    [Fact]
    public async Task AddAsync_SendsOnlyTheDelta_AndRerendersFromTheStoredList()
    {
        var rpc = new MockNestRpcClient { NextMutedKeywords = new[] { "spam" } };
        var vm = new MutedWordsViewModel(rpc);
        await vm.LoadAsync();

        rpc.NextMutedKeywords = new[] { "spam", "lottery" };
        var ok = await vm.AddAsync("lottery");

        Assert.True(ok);
        Assert.Contains("MutedKeywordsAdd", rpc.Calls);
        Assert.DoesNotContain("MutedKeywordsSet", rpc.Calls);
        Assert.Equal("lottery", rpc.LastMutedKeywordAdded);
        Assert.Equal(new[] { "spam", "lottery" }, vm.Words);
        Assert.Equal(new[] { "spam", "lottery" }, MutedKeywordsCache.Words);
    }

    [Fact]
    public async Task AddAsync_BlankTerm_NoRpcCall()
    {
        var rpc = new MockNestRpcClient();
        var vm = new MutedWordsViewModel(rpc);

        var ok = await vm.AddAsync("   ");

        Assert.False(ok);
        Assert.DoesNotContain("MutedKeywordsAdd", rpc.Calls);
    }

    [Fact]
    public async Task RemoveAsync_SendsOnlyTheTerm_AndRerendersFromTheStoredList()
    {
        var rpc = new MockNestRpcClient { NextMutedKeywords = new[] { "spam", "lottery" } };
        var vm = new MutedWordsViewModel(rpc);
        await vm.LoadAsync();

        rpc.NextMutedKeywords = new[] { "lottery" };
        var ok = await vm.RemoveAsync("spam");

        Assert.True(ok);
        Assert.Contains("MutedKeywordsRemove", rpc.Calls);
        Assert.Equal("spam", rpc.LastMutedKeywordRemoved);
        Assert.Equal(new[] { "lottery" }, vm.Words);
    }

    /// <summary>
    /// The loading-is-not-empty rule's deterministic half
    /// (<c>docs/goal/ui/README.md</c> § <i>List pages: loading is not empty</i>),
    /// pinned on the VM because the page reads <c>Loaded</c> to decide whether
    /// <c>muted-word-empty</c> may paint. Three states, and the two that both
    /// show zero words must stay distinguishable: never read, and read-and-empty.
    /// </summary>
    [Fact]
    public async Task Loaded_IsFalseUntilAReadReturns()
    {
        var rpc = new MockNestRpcClient { NextMutedKeywords = Array.Empty<string>() };
        var vm = new MutedWordsViewModel(rpc);

        Assert.False(vm.Loaded, "a VM that has not read yet must not report a loaded page");
        Assert.Empty(vm.Words);

        await vm.LoadAsync();

        Assert.True(vm.Loaded, "a read that found nothing IS the genuine empty state");
        Assert.Empty(vm.Words);
    }

    /// <summary>
    /// A FIRST read that fails leaves the page unloaded, so the page shows its
    /// error rather than a false "you haven't muted any words yet" beside it.
    /// This is the arm a start-state-only fix (XAML <c>Visibility="Collapsed"</c>)
    /// would miss, since the render runs after the failed load.
    /// </summary>
    [Fact]
    public async Task Loaded_StaysFalseWhenTheFirstReadFails()
    {
        var rpc = new MockNestRpcClient { NextError = "boom" };
        var vm = new MutedWordsViewModel(rpc);

        await vm.LoadAsync();

        Assert.False(vm.Loaded);
        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
    }

    [Fact]
    public async Task AddAsync_Failure_RoutesToError_AndCacheUntouched()
    {
        var rpc = new MockNestRpcClient { NextError = "boom" };
        var vm = new MutedWordsViewModel(rpc);

        var ok = await vm.AddAsync("spam");

        Assert.False(ok);
        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
        Assert.Empty(MutedKeywordsCache.Words);
    }
}
