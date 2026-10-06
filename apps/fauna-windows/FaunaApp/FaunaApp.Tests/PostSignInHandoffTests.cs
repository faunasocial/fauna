using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Pins <see cref="PostSignInHandoff"/>: the onboarding wizard's sign-in
/// follow-ups run on the session's own client (transport.md: one authenticated
/// WebSocket per actor), only for the actor they were captured for, once.
/// </summary>
[Collection("PostSignInHandoff")]
public class PostSignInHandoffTests : IDisposable
{
    private const string Alice = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    private const string Bob = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    public PostSignInHandoffTests() => PostSignInHandoff.Clear();
    public void Dispose() => PostSignInHandoff.Clear();

    [Fact]
    public async Task FollowUpsRunOnTheSessionClientInOrderAndOnlyOnce()
    {
        var rpc = new MockNestRpcClient();
        PostSignInHandoff.Enqueue(Alice, "kit", r => r.RecoveryRegisterDeferredKitAsync("k1"));
        PostSignInHandoff.Enqueue(Alice, "second", r => r.RecoveryRegisterDeferredKitAsync("k2"));

        await PostSignInHandoff.RunForAsync(Alice, rpc);
        await PostSignInHandoff.RunForAsync(Alice, rpc);

        Assert.Equal(
            new[] { "RecoveryRegisterDeferredKit(k1)", "RecoveryRegisterDeferredKit(k2)" },
            rpc.Calls);
    }

    [Fact]
    public async Task AnotherActorsSessionNeverRunsThem()
    {
        var rpc = new MockNestRpcClient();
        PostSignInHandoff.Enqueue(Alice, "kit", r => r.RecoveryRegisterDeferredKitAsync("k1"));

        await PostSignInHandoff.RunForAsync(Bob, rpc);
        await PostSignInHandoff.RunForAsync(null, rpc);

        Assert.Empty(rpc.Calls);
        await PostSignInHandoff.RunForAsync(Alice.ToUpperInvariant(), rpc);
        Assert.Equal(new[] { "RecoveryRegisterDeferredKit(k1)" }, rpc.Calls);
    }

    [Fact]
    public async Task AFailedFollowUpDoesNotStopTheNext()
    {
        var rpc = new MockNestRpcClient();
        PostSignInHandoff.Enqueue(Alice, "throws", _ => throw new InvalidOperationException("boom"));
        PostSignInHandoff.Enqueue(Alice, "kit", r => r.RecoveryRegisterDeferredKitAsync("k1"));

        await PostSignInHandoff.RunForAsync(Alice, rpc);

        Assert.Equal(new[] { "RecoveryRegisterDeferredKit(k1)" }, rpc.Calls);
    }

    [Fact]
    public async Task TheCredentialWipeDropsThem()
    {
        var rpc = new MockNestRpcClient();
        PostSignInHandoff.Enqueue(Alice, "kit", r => r.RecoveryRegisterDeferredKitAsync("k1"));

        PostSignInHandoff.Clear();
        await PostSignInHandoff.RunForAsync(Alice, rpc);

        Assert.Empty(rpc.Calls);
    }
}
