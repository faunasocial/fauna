using System.Runtime.CompilerServices;
using Xunit;

namespace FaunaApp.Tests;

// Ruling (8)(b)/(8)(c) of writer-signed-change-records.md: a successor's Media
// machine opens a row signed under a retired identity only when it is handed
// the account's attested predecessor ids AND the paired id/key chain. A key
// handed in bare (`SetPredecessorBackupKeys`) opens what the current identity
// signed, never a predecessor's row, so the inherited corpus would list but
// not open.
//
// `BuildMediaMachineAsync` binds a real `FfiNestClient` (a live WS-RPC socket,
// no injectable seam), so the two hand-ins are pinned STRUCTURALLY over that
// method's slice of `NestRpcClient.cs` — the same split
// `NestRpcClientConnectDisposeRaceTests` makes. windows' twin of android's
// `MediaVMPredecessorChainPinTest` and tui's `media::init`.
public class NestRpcClientMediaPredecessorChainPinTests
{
    private static readonly string SourcePath = ResolveSourcePath();

    private static string ResolveSourcePath([CallerFilePath] string testFilePath = "") =>
        Path.GetFullPath(Path.Combine(
            Path.GetDirectoryName(testFilePath)!, // FaunaApp.Tests
            "..", "FaunaApp.Core", "Services", "NestRpcClient.cs"));

    private static string BuildMediaMachineBody()
    {
        var source = File.ReadAllText(SourcePath);
        const string startAnchor = "public async Task<MediaMachine> BuildMediaMachineAsync(MediaObserver observer)";
        const string endAnchor = "public async Task WireMediaFollowedFoldersAsync(";
        var start = source.IndexOf(startAnchor, StringComparison.Ordinal);
        Assert.True(start >= 0, $"`{startAnchor}` moved or was renamed — update this test's anchor");
        var end = source.IndexOf(endAnchor, start, StringComparison.Ordinal);
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
    public void BuildMediaMachineHandsTheMachineTheRegistrysAttestedPredecessorIds()
    {
        var body = BuildMediaMachineBody();

        var keys = RequireIndex(body, "machine.SetPredecessorBackupKeys(", 0,
            "the bare backup-key injection moved — update this test's anchor");
        var read = RequireIndex(body, "AttestedPredecessorActorIds(_crypto.ActorIdHex)", keys,
            "BuildMediaMachineAsync no longer reads the registry's attested predecessor ids for the session actor — a successor's Media machine would judge a predecessor-signed row by the statement walk only");
        RequireIndex(body, "machine.SetPredecessorActorIds(", read,
            "BuildMediaMachineAsync reads the attested predecessor ids but never hands them to the machine");
    }

    [Fact]
    public void BuildMediaMachineHandsTheMachineThePairedPredecessorChain()
    {
        var body = BuildMediaMachineBody();

        var keys = RequireIndex(body, "machine.SetPredecessorBackupKeys(", 0,
            "the bare backup-key injection moved — update this test's anchor");
        // The read itself is the shared `PredecessorChainOrEmpty`, pinned to the
        // registry's walk for the session actor by
        // `NestRpcClientDevicesPredecessorChainPinTests`.
        var read = RequireIndex(body, "PredecessorChainOrEmpty(", keys,
            "BuildMediaMachineAsync no longer reads the registry's paired predecessor chain — a bare key never opens a predecessor-signed row, so the inherited corpus stays unopenable");
        RequireIndex(body, "machine.SetPredecessorChain(", read,
            "BuildMediaMachineAsync reads the paired chain but never hands it to the machine");
    }
}
