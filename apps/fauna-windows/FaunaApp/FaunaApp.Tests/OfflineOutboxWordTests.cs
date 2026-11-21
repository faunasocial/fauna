using System;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// windows' two calls into the device's offline outbox
/// (<c>account-offline-mutation.md</c> § The offline-mutation contract): the
/// shell's connection handler hands every word it gives the offline gate to the
/// process-level <c>SetDeviceConnectionWord</c>, and <see cref="FeedManagerHost"/>
/// hands a manager it builds the gate's word once, which lends the outbox to a
/// manager built before the first word. With both, a like tapped with no nest is
/// queued instead of being sent into the dead connection.
///
/// <para>The discriminator needs no nest and no account runtime: a queued like goes
/// to the outbox, which reads this process's account runtime at the enqueue and,
/// with none running here, refuses with the shared <c>RUNTIME_ABSENT</c> sentence
/// (<c>fauna_account_plane::account_driver::handle_source</c>) — or, if a runtime is
/// up, takes it and the like succeeds. A like with no outbox goes to the nest
/// client instead, whose url (<c>wss://127.0.0.1:0/ws</c>) nothing answers, so it
/// fails some other way or never settles. The deadline below only bounds that
/// wrong branch; the right one settles without touching the network.</para>
///
/// <para>In <c>ActorScopedStaticsGlobal</c> (no parallel collection): the outbox
/// and its word are one per process, the shared gate is too, and the shell tests
/// raise connection words of their own.</para>
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class OfflineOutboxWordTests
{
    /// <summary><c>fauna_account_plane::account_driver::handle_source::RUNTIME_ABSENT</c>.</summary>
    private const string RuntimeAbsent = "the account runtime is not running";

    /// <summary>A post id the manager holds no row for (32 bytes, hex).</summary>
    private static readonly string PostId = new string('0', 62) + "aa";

    /// <summary>Like a post the manager holds no row for, and say why the like was
    /// not queued — <c>null</c> when it was.</summary>
    private static async Task<string?> WhyNotQueued(FfiFeedManager manager)
    {
        var like = manager.Like(PostId);
        if (await Task.WhenAny(like, Task.Delay(TimeSpan.FromSeconds(30))) != like)
            return "the like never settled: it went to the nest client, not the outbox";
        try
        {
            await like;
            return null;
        }
        catch (FfiException e) when (e.Message.Contains(RuntimeAbsent))
        {
            return null;
        }
        catch (Exception e)
        {
            return $"the like was sent, not queued: {e.GetType().Name}: {e.Message}";
        }
    }

    [Fact]
    public async Task TheShellsConnectionWordReachesTheDeviceOutbox_SoAnOfflineLikeQueues()
    {
        var rpc = new MockNestRpcClient();
        var vm = new MainViewModel(rpc, new FakeAgentStatusProbe());
        using var nest = new FfiNestClient("wss://127.0.0.1:0/ws", new byte[32]);
        try
        {
            // Wire the outbox online first, so only the shell's own word can turn
            // it offline: a like then queues because the handler fed the word.
            FaunaFfiMethods.SetDeviceConnectionWord("connected");
            rpc.RaiseConnectionState(FfiConnectionState.Disconnected);

            // Built after the word, so it borrows the wired outbox at its build.
            using var manager = nest.FeedManager(new byte[32]);
            Assert.Null(await WhyNotQueued(manager));
        }
        finally
        {
            vm.Detach();
            LeaveTheProcessOnline();
        }
    }

    [Fact]
    public async Task AManagerTheHostBuildsTakesTheGatesWord_SoAnOfflineLikeQueues()
    {
        using var nest = new FfiNestClient("wss://127.0.0.1:0/ws", new byte[32]);
        using var manager = nest.FeedManager(new byte[32]);
        try
        {
            // Online as far as the device knows; the gate reads offline. The host's
            // build hands the manager the gate's word, so the two agree.
            FaunaFfiMethods.SetDeviceConnectionWord("connected");
            OfflineGate.Shared.ResetForTest("disconnected");

            await FeedManagerHost.GetOrBuildAsync(new object(), () => Task.FromResult(manager));
            Assert.Null(await WhyNotQueued(manager));
        }
        finally
        {
            FeedManagerHost.ResetForActorChange();
            LeaveTheProcessOnline();
        }
    }

    /// <summary>The outbox, once wired, stays wired for the process: leave it on the
    /// word that sends every gesture, as an unwired one does, and the gate on its
    /// starting word, so no later test inherits an offline device.</summary>
    private static void LeaveTheProcessOnline()
    {
        FaunaFfiMethods.SetDeviceConnectionWord("connected");
        OfflineGate.Shared.ResetForTest("disconnected");
    }
}
