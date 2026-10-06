using System;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// <see cref="SyncAgentSession"/> reaches the nest only through the login's own live
/// <see cref="INestRpcClient"/> — never a private <c>FfiNestClient</c> of its own.
///
/// <para>The private one-shot client it used to build its provisioner on was never
/// <c>Connect()</c>ed, so the renewal-grant mint's <c>fauna.sync.register</c> waited out its
/// deadline and the machine's named device row never landed. This pins the build's
/// best-effort failure contract over the injected client; the provisioner build itself needs
/// a live <c>FfiNestClient</c> and is witnessed end to end by
/// <c>test_relaunch_device_accrual.py --app windows</c>.</para>
/// </summary>
public class SyncAgentSessionClientTests
{
    [Fact]
    public async Task A_failed_provisioner_build_is_a_null_session_not_a_throw()
    {
        // The mock cannot build a provisioner (no live FfiNestClient) and throws — which is
        // exactly the "nest unreachable at login" shape: best-effort, never disrupts login.
        var rpc = new MockNestRpcClient();

        var session = await SyncAgentSession.CreateAsync(
            rpc, "00", "device", Array.Empty<byte[]>(), Array.Empty<byte[]>(), () => null);

        Assert.Null(session);
        Assert.Contains("BuildSyncAgentProvisioner", rpc.Calls);
    }
}
