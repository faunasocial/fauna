using FaunaApp.Core.Helpers;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The per-key load cache behind every post picture (<c>BlobImageLoader</c>): a value is
/// loaded once per key, and a caller that asks while that load is still running shares it.
///
/// <para><b>The defect this pins.</b> The loader used to cache a picture only once its load
/// had finished. A feed that re-renders while its pictures load — every snapshot tick of a
/// hydrating feed re-runs the post-card sync, which asks for every image row's C2PA verdict
/// until it lands, and a content-changed row re-binds its image — therefore started another
/// full fetch, open, C2PA parse and decode for the same blob on every tick. On a feed with a
/// dozen pictures that was hundreds of redundant loads a second, enough UI-thread work to
/// stall the app past the e2e bridge's UIA budget (measured 2026-11-02: ~60 C2PA parses a
/// second for 4 s over three provenance-asserting images, beside a 7.3 s UI-thread stall).</para>
/// </summary>
public class SingleFlightCacheTests
{
    private sealed class Probe
    {
        public int Loads;
        public readonly List<TaskCompletionSource<string?>> Pending = new();

        public Func<Task<string?>> Load() => () =>
        {
            Loads++;
            var tcs = new TaskCompletionSource<string?>();
            Pending.Add(tcs);
            return tcs.Task;
        };
    }

    private static SingleFlightCache<string?> NewCache() => new(v => v is not null);

    [Fact]
    public async Task Callers_asking_while_a_load_runs_share_that_one_load()
    {
        var cache = NewCache();
        var probe = new Probe();

        var first = cache.GetOrLoadAsync("blob", probe.Load());
        var second = cache.GetOrLoadAsync("blob", probe.Load());
        var third = cache.GetOrLoadAsync("blob", probe.Load());

        // The mutation this rejects: a cache that only remembers FINISHED loads starts a
        // second and a third load here, one per caller.
        Assert.Equal(1, probe.Loads);

        probe.Pending[0].SetResult("bitmap");
        Assert.Equal("bitmap", await first);
        Assert.Equal("bitmap", await second);
        Assert.Equal("bitmap", await third);
    }

    [Fact]
    public async Task A_finished_value_is_served_without_loading_again()
    {
        var cache = NewCache();
        var probe = new Probe();

        var first = cache.GetOrLoadAsync("blob", probe.Load());
        probe.Pending[0].SetResult("bitmap");
        await first;

        Assert.Equal("bitmap", await cache.GetOrLoadAsync("blob", probe.Load()));
        Assert.Equal(1, probe.Loads);
    }

    [Fact]
    public async Task A_value_not_worth_keeping_is_loaded_again_by_the_next_caller()
    {
        // A sealed attachment that did not open yet, or a decode failure, answers null —
        // and the render after the post's unlock must retry and paint.
        var cache = NewCache();
        var probe = new Probe();

        var first = cache.GetOrLoadAsync("blob", probe.Load());
        probe.Pending[0].SetResult(null);
        Assert.Null(await first);

        var retry = cache.GetOrLoadAsync("blob", probe.Load());
        Assert.Equal(2, probe.Loads);
        probe.Pending[1].SetResult("bitmap");
        Assert.Equal("bitmap", await retry);
    }

    [Fact]
    public async Task A_load_that_throws_is_not_shared_with_the_next_caller()
    {
        var cache = NewCache();
        var probe = new Probe();

        var first = cache.GetOrLoadAsync("blob", probe.Load());
        probe.Pending[0].SetException(new InvalidOperationException("transport"));
        await Assert.ThrowsAsync<InvalidOperationException>(() => first);

        var retry = cache.GetOrLoadAsync("blob", probe.Load());
        Assert.Equal(2, probe.Loads);
        probe.Pending[1].SetResult("bitmap");
        Assert.Equal("bitmap", await retry);
    }

    [Fact]
    public async Task After_a_clear_a_caller_never_receives_a_load_begun_before_it()
    {
        // Clear is the actor boundary: a picture opened with one actor's per-post key must
        // not reach the next actor's session — neither through the finished cache nor
        // through a load that was still running when the boundary was crossed.
        var cache = NewCache();
        var probe = new Probe();

        var before = cache.GetOrLoadAsync("blob", probe.Load());
        cache.Clear();
        var after = cache.GetOrLoadAsync("blob", probe.Load());
        Assert.Equal(2, probe.Loads);

        // The old load finishing late must not populate the cache for the new session.
        probe.Pending[0].SetResult("old-actor");
        Assert.Equal("old-actor", await before);
        probe.Pending[1].SetResult("new-actor");
        Assert.Equal("new-actor", await after);
        Assert.Equal("new-actor", await cache.GetOrLoadAsync("blob", probe.Load()));
        Assert.Equal(2, probe.Loads);
    }

    [Fact]
    public async Task A_load_finishing_after_a_clear_does_not_fill_the_cleared_cache()
    {
        var cache = NewCache();
        var probe = new Probe();

        var before = cache.GetOrLoadAsync("blob", probe.Load());
        cache.Clear();
        probe.Pending[0].SetResult("old-actor");
        await before;

        var next = cache.GetOrLoadAsync("blob", probe.Load());
        Assert.Equal(2, probe.Loads);
        probe.Pending[1].SetResult("new-actor");
        Assert.Equal("new-actor", await next);
    }

    [Fact]
    public async Task Different_keys_load_independently()
    {
        var cache = NewCache();
        var probe = new Probe();

        var a = cache.GetOrLoadAsync("a", probe.Load());
        var b = cache.GetOrLoadAsync("b", probe.Load());
        Assert.Equal(2, probe.Loads);

        probe.Pending[1].SetResult("bee");
        probe.Pending[0].SetResult("ay");
        Assert.Equal("ay", await a);
        Assert.Equal("bee", await b);
    }

    [Fact]
    public async Task A_load_that_completes_synchronously_is_cached_and_not_left_in_flight()
    {
        var cache = NewCache();
        var loads = 0;
        Func<Task<string?>> sync = () => { loads++; return Task.FromResult<string?>("bitmap"); };

        Assert.Equal("bitmap", await cache.GetOrLoadAsync("blob", sync));
        Assert.Equal("bitmap", await cache.GetOrLoadAsync("blob", sync));
        Assert.Equal(1, loads);

        Func<Task<string?>> syncNull = () => { loads++; return Task.FromResult<string?>(null); };
        Assert.Null(await cache.GetOrLoadAsync("other", syncNull));
        Assert.Null(await cache.GetOrLoadAsync("other", syncNull));
        Assert.Equal(3, loads);
    }
}
