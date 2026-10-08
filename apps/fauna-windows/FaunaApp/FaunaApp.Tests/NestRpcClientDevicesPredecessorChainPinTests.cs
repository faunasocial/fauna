using System.Runtime.CompilerServices;
using Xunit;

namespace FaunaApp.Tests;

// succession-aftermath.md § Re-key scope, the `BackupKey` corpus row: a
// succession re-points an owned set to the successor and re-seals nothing, so
// an owner-only set's name still rests under the predecessor's owner root. The
// successor's Folders page lists it only when its Devices machine is handed the
// registry's paired predecessor chain (`DevicesMachine::set_predecessor_chain`,
// red-first in `fauna-devices-machine`'s
// `a_successors_inherited_set_lists_once_the_predecessor_chain_is_wired`).
// Measured unwired on windows 2026-10-07: the successor's Folders page listed
// nothing after the ceremony.
//
// `BuildDevicesMachineAsync` binds a real `FfiNestClient` (a live WS-RPC socket,
// no injectable seam), so the hand-in is pinned STRUCTURALLY over that method's
// slice of `NestRpcClient.cs`, the split `NestRpcClientMediaPredecessorChainPinTests`
// makes. tui's twin is `settings::devices::label_custody`.
public class NestRpcClientDevicesPredecessorChainPinTests
{
    private static readonly string SourcePath = ResolveSourcePath();

    private static string ResolveSourcePath([CallerFilePath] string testFilePath = "") =>
        Path.GetFullPath(Path.Combine(
            Path.GetDirectoryName(testFilePath)!, // FaunaApp.Tests
            "..", "FaunaApp.Core", "Services", "NestRpcClient.cs"));

    private static string Slice(string startAnchor, string endAnchor)
    {
        var source = File.ReadAllText(SourcePath);
        var start = source.IndexOf(startAnchor, StringComparison.Ordinal);
        Assert.True(start >= 0, $"`{startAnchor}` moved or was renamed — update this test's anchor");
        var end = source.IndexOf(endAnchor, start + startAnchor.Length, StringComparison.Ordinal);
        Assert.True(end >= 0, $"`{endAnchor}` no longer follows `{startAnchor}` — update this test's anchor");
        return source[start..end];
    }

    private static int RequireIndex(string body, string anchor, int from, string message)
    {
        var idx = body.IndexOf(anchor, from, StringComparison.Ordinal);
        Assert.True(idx >= 0, message);
        return idx;
    }

    [Fact]
    public void BuildDevicesMachineHandsTheMachineThePairedPredecessorChainAfterTheBuild()
    {
        var body = Slice(
            "public async Task<DevicesMachine> BuildDevicesMachineAsync(DevicesObserver observer)",
            "private (byte[][] actorIds, byte[][] keys) PredecessorChainOrEmpty(");

        var build = RequireIndex(body, "FaunaFfiMethods.BuildDevicesMachine(", 0,
            "the Devices machine build moved — update this test's anchor");
        var read = RequireIndex(body, "PredecessorChainOrEmpty(", build,
            "BuildDevicesMachineAsync no longer reads the registry's paired predecessor chain — a successor's Folders page drops every set it inherited");
        RequireIndex(body, "machine.SetPredecessorChain(", read,
            "BuildDevicesMachineAsync reads the paired chain but never hands it to the machine");
    }

    [Fact]
    public void ThePredecessorChainReadIsTheRegistrysWalkForTheSessionActor()
    {
        var body = Slice(
            "private (byte[][] actorIds, byte[][] keys) PredecessorChainOrEmpty(",
            "public async Task<");

        RequireIndex(body, "PredecessorChain(_crypto.ActorIdHex)", 0,
            "PredecessorChainOrEmpty no longer reads the registry's own chain walk for the session's actor — every reader seam it feeds (Media, Devices) would be handed someone else's chain or none");
    }
}
