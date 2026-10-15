using System.Runtime.CompilerServices;
using Xunit;

namespace FaunaApp.Tests;

// Ruling (8)(b) source (ii) of writer-signed-change-records.md: the content-index
// reader, the launch folder resume and the served-set walk admit a row a retired
// identity signed with no `fauna.recovery.succession.lookup` round trip only when
// handed the account's attested predecessor ids. The FFI faces take the registry
// OBJECT and resolve the ids in Rust off the session's own actor, so the pin is
// that windows hands the registry across both faces.
//
// Both faces bind a real `FfiNestClient` (a live WS-RPC socket, no injectable
// seam), so the hand-ins are pinned STRUCTURALLY over `NestRpcClient.cs` — the
// split `NestRpcClientMediaPredecessorChainPinTests` makes.
public class NestRpcClientFileSeatsPredecessorRegistryPinTests
{
    private static readonly string SourcePath = ResolveSourcePath();

    private static string ResolveSourcePath([CallerFilePath] string testFilePath = "") =>
        Path.GetFullPath(Path.Combine(
            Path.GetDirectoryName(testFilePath)!, // FaunaApp.Tests
            "..", "FaunaApp.Core", "Services", "NestRpcClient.cs"));

    [Fact]
    public void TheConversationsSessionIsHandedTheAccountRegistry()
    {
        var source = File.ReadAllText(SourcePath);
        var call = source.IndexOf("nest.ConversationsSessionOverManager(", StringComparison.Ordinal);
        Assert.True(call >= 0, "`ConversationsSessionOverManager` call moved — update this test's anchor");
        var end = source.IndexOf(");", call, StringComparison.Ordinal);
        Assert.Contains("accounts: ResolveAccountRegistry()", source[call..end]);
    }

    [Fact]
    public void TheServeSetWalkIsHandedTheAccountRegistry()
    {
        var source = File.ReadAllText(SourcePath);
        var call = source.IndexOf("FaunaFfiMethods.FoldersServeSet(", StringComparison.Ordinal);
        Assert.True(call >= 0, "`FoldersServeSet` call moved — update this test's anchor");
        var end = source.IndexOf(");", call, StringComparison.Ordinal);
        Assert.Contains("accounts: ResolveAccountRegistry()", source[call..end]);
    }

    // webdav-server.md § Key model (c): the served-set walk (the flip's re-seal of
    // pre-serve files, and its app-start resume) runs only when the face is
    // handed the app's recording device; a missing hand-in silently runs no walk.
    [Fact]
    public void TheServeSetFaceAndTheSessionFactoryNameTheRecordingDevice()
    {
        var source = File.ReadAllText(SourcePath);
        var serve = source.IndexOf("FaunaFfiMethods.FoldersServeSet(", StringComparison.Ordinal);
        Assert.True(serve >= 0, "`FoldersServeSet` call moved — update this test's anchor");
        Assert.Contains("deviceId: RecordingDevice(deviceIdHex)",
            source[serve..source.IndexOf(");", serve, StringComparison.Ordinal)]);

        var factory = source.IndexOf("nest.ConversationsSessionOverManager(", StringComparison.Ordinal);
        Assert.True(factory >= 0, "`ConversationsSessionOverManager` call moved — update this test's anchor");
        Assert.Contains("recordingDevice: RecordingDevice(deviceIdHex)",
            source[factory..source.IndexOf(");", factory, StringComparison.Ordinal)]);
    }
}
