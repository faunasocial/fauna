using System.Runtime.CompilerServices;
using Xunit;

namespace FaunaApp.Tests;

// sync-agent-credentials.md § Credential model: the agent is sent the paired
// predecessor keys (`SyncCapability.predecessor_keys_by_actor`) so a row a
// retired identity signed is offered that predecessor's root. Shared Rust
// resolves the pairs off the account registry the app hands the provisioner
// (ruling (8)(c) of writer-signed-change-records.md); the app hands the
// registry OBJECT, never ids or key bytes zipped app-side. Without it the
// agent note-skips the predecessor-signed row.
//
// `BuildSyncAgentProvisionerAsync` binds a real `FfiNestClient` (a live WS-RPC
// socket, no injectable seam), so the hand-in is pinned STRUCTURALLY over that
// method's slice of `NestRpcClient.cs` — the split
// `NestRpcClientMediaPredecessorChainPinTests` makes.
public class NestRpcClientSyncAgentProvisionerRegistryPinTests
{
    private static readonly string SourcePath = ResolveSourcePath();

    private static string ResolveSourcePath([CallerFilePath] string testFilePath = "") =>
        Path.GetFullPath(Path.Combine(
            Path.GetDirectoryName(testFilePath)!, // FaunaApp.Tests
            "..", "FaunaApp.Core", "Services", "NestRpcClient.cs"));

    private static string BuildSyncAgentProvisionerBody()
    {
        var source = File.ReadAllText(SourcePath);
        const string startAnchor = "public async Task<FfiSyncAgentProvisioner> BuildSyncAgentProvisionerAsync(";
        const string endAnchor = "public async Task<IFfiPushRegistration> BuildPushRegistrationAsync(";
        var start = source.IndexOf(startAnchor, StringComparison.Ordinal);
        Assert.True(start >= 0, $"`{startAnchor}` moved or was renamed — update this test's anchor");
        var end = source.IndexOf(endAnchor, start, StringComparison.Ordinal);
        Assert.True(end >= 0, $"`{endAnchor}` no longer follows `{startAnchor}` — update this test's anchor");
        return source[start..end];
    }

    [Fact]
    public void BuildSyncAgentProvisionerHandsTheProvisionerTheAccountRegistry()
    {
        var body = BuildSyncAgentProvisionerBody();

        var call = body.IndexOf("nest.SyncAgentProvisioner(", StringComparison.Ordinal);
        Assert.True(call >= 0, "the `nest.SyncAgentProvisioner(` call moved — update this test's anchor");
        var registry = body.IndexOf("ResolveAccountRegistry()", call, StringComparison.Ordinal);
        Assert.True(registry >= 0,
            "BuildSyncAgentProvisionerAsync no longer hands the provisioner the account registry — the agent is sent no paired predecessor keys and note-skips a row a retired identity signed");
    }
}
