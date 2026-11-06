using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

// A session whose domain does not resolve must still get its one client.
//
// `ConnectedAsync` is the door every `NestRpcClient` call goes through, and the
// launch path walks through it before it mounts the main app. It used to await
// the UniFFI `Connect()`, which waits up to 30 s for the socket and then throws
// with its supervisor still running. So a dark domain — the account a reach hint
// opened, whose launch challenge succeeded over the hint while the session dial
// still goes to the domain — cost every caller a full wait in turn behind
// `_connectGate`, each one leaving another supervisor dialling a host that will
// never answer, and the mount arrived minutes late (`test_reach_hint_dial_journey`
// red on windows, 2026-10-31).
//
// This is the real FFI on purpose: the host is RFC 6761 `.invalid`, so it can
// never resolve, nothing listens, and the behaviour under test is exactly what
// the client does with that. Nothing here reads a clock — a client that waits for
// the socket throws out of the first call, whatever the machine's load.
public class NestRpcClientDarkDomainTests
{
    [Fact]
    public async Task ADarkDomainStillYieldsTheSessionsOneClient()
    {
        var rpc = new NestRpcClient(
            "http://reach-hint-domain.invalid:1", new MockCryptoService(), new FakeAgentSyncNudge());
        try
        {
            // Both calls are ones the launch path awaits before it mounts, and
            // neither needs the socket to be up: they only build a handle over
            // the client. A wait for the socket here is a wait in front of the app.
            Assert.NotNull(await rpc.BuildDraftsSyncAsync("posts"));
            Assert.NotNull(await rpc.BuildEventDraftsSyncAsync());
        }
        finally
        {
            // Stops the supervisor that is still retrying the dark domain.
            await rpc.DisposeAsync();
        }
    }
}
