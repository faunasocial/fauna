using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_moderation;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// The windows moderation-queue is the UNION of the server <c>fauna.moderation.actions</c>
/// obligation rows and the client's own post-decrypt LOCAL detections
/// (<c>moderation.md</c> § Layout &amp; flow — slice 4). These drive the union +
/// train-routing over the REAL native <c>FaunaFfiMethods.ModerationQueue</c> merge
/// (dedupe by <c>content_id</c>, server wins, newest-first) — a cross-language
/// conformance check of the shared <c>merge_queue</c> — with a fake local-detection
/// seam standing in for the live MLS session's store. linux reader parity:
/// <c>client.rs::{fetch_moderation_actions, correct_moderation_row}</c>.
/// The tests exercise the real FFI dll (reference: windows dotnet tests load the
/// native FFI).
/// </summary>
public class ModerationViewModelLocalDetectionTests
{
    // FfiObligationAction(id, content_type, content_id, category, confidence_per_mille, action, timestamp)
    private static FfiObligationAction ServerRow(string contentId, string category, byte action, ushort perMille, long ts) =>
        new(0L, "post", contentId, category, perMille, action, ts);

    // LocalDetection(content_id, content_type, category, confidence_per_mille, timestamp)
    private static LocalDetection LocalRow(string contentId, string category, ushort perMille, long ts) =>
        new(contentId, "message", category, perMille, ts);

    /// <summary>A fake local-detection seam: an in-memory store the tests seed and
    /// observe removals on, standing in for the live <c>ConversationsSession</c> store.</summary>
    private sealed class FakeLocal : IModerationLocalDetections
    {
        private readonly List<LocalDetection> _items;
        public List<string> Removed { get; } = new();
        public FakeLocal(params LocalDetection[] items) => _items = items.ToList();
        public IReadOnlyList<LocalDetection> Snapshot() => _items;
        public string? Body(string contentId) => null;   // these tests don't exercise the client-write body path
        public void Remove(string contentId)
        {
            Removed.Add(contentId);
            _items.RemoveAll(d => d.contentId == contentId);
        }
    }

    [Fact]
    public async Task Load_UnionsServerActionsWithLocalDetections_LocalRowHasBlankAction()
    {
        var rpc = new MockNestRpcClient
        {
            NextModerationActions = new List<FfiObligationAction>
            {
                ServerRow("aa", "phishing", action: 2, perMille: 880, ts: 200),
            },
        };
        var local = new FakeLocal(LocalRow("bb", "spam", perMille: 720, ts: 100));
        var vm = new ModerationViewModel(rpc, local);

        await vm.LoadCommand.ExecuteAsync(null);

        // The union of both sources, newest-first (server ts=200 before local ts=100).
        Assert.Equal(2, vm.Actions.Count);

        var server = vm.Actions[0];
        Assert.Equal("aa", server.ContentId);
        Assert.False(server.IsLocal);
        Assert.False(string.IsNullOrEmpty(server.ActionLabel));   // server row carries its enforcement action

        var localRow = vm.Actions[1];
        Assert.Equal("bb", localRow.ContentId);
        Assert.True(localRow.IsLocal);
        Assert.Equal(string.Empty, localRow.ActionLabel);          // blank action column (§ Don't fabricate)
        Assert.False(string.IsNullOrEmpty(localRow.CategoryLabel)); // badge still resolved (spam) via the shared map
        Assert.Equal(72, localRow.ConfidencePercent);              // (720 + 5) / 10, shared half-up rounding
    }

    [Fact]
    public async Task Load_DedupesByContentId_ServerRowWins()
    {
        var rpc = new MockNestRpcClient
        {
            NextModerationActions = new List<FfiObligationAction>
            {
                ServerRow("dup", "spam", action: 1, perMille: 900, ts: 100),
            },
        };
        var local = new FakeLocal(LocalRow("dup", "spam", perMille: 720, ts: 500));
        var vm = new ModerationViewModel(rpc, local);

        await vm.LoadCommand.ExecuteAsync(null);

        // Same content_id on both sources → one row, the server row (carries the action).
        Assert.Single(vm.Actions);
        Assert.False(vm.Actions[0].IsLocal);
        Assert.False(string.IsNullOrEmpty(vm.Actions[0].ActionLabel));
    }

    [Fact]
    public async Task Train_LocalRow_RemovesFromSessionStore_WithoutRpcTrain()
    {
        var rpc = new MockNestRpcClient();  // no server rows
        var local = new FakeLocal(LocalRow("bb", "spam", perMille: 720, ts: 100));
        var vm = new ModerationViewModel(rpc, local);
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Single(vm.Actions);          // the sole local detection surfaces
        var localRow = vm.Actions[0];
        Assert.True(localRow.IsLocal);

        await vm.TrainCommand.ExecuteAsync(localRow);

        // A local detection has no nest obligation to train — the correction removes the
        // client-side false-positive flag and repaints (moderation.md § State: held client-side).
        Assert.Contains("bb", local.Removed);
        Assert.Null(rpc.LastTrain);
        Assert.Empty(vm.Actions);           // refreshed → the removed flag is gone
    }

    [Fact]
    public async Task Train_ServerRow_SubmitsHamVerdict_EvenWithLocalSeam()
    {
        var rpc = new MockNestRpcClient
        {
            NextModerationActions = new List<FfiObligationAction>
            {
                ServerRow("srv", "spam", action: 1, perMille: 900, ts: 100),
            },
        };
        var local = new FakeLocal();  // empty local store
        var vm = new ModerationViewModel(rpc, local);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.TrainCommand.ExecuteAsync(vm.Actions[0]);

        // A server row still trains the caller's Bayesian model via the nest.
        Assert.NotNull(rpc.LastTrain);
        Assert.Equal("srv", rpc.LastTrain!.Value.ContentId);
        Assert.Equal("ham", rpc.LastTrain!.Value.Verdict);
        Assert.Empty(local.Removed);
    }
}
