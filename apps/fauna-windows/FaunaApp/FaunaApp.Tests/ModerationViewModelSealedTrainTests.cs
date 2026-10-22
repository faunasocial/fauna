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
/// The windows moderation-queue <c>train-correction-button</c>: a <b>server</b> row runs the
/// one shared <c>MailSettingsMachine::train_moderation_correction</c> flow (sealed model
/// write, then <c>fauna.moderation.train</c> always; <c>moderation.md</c> § Training
/// correction submission) — the C# obligation is dispatching it once per row, surfacing its
/// error and re-reading the queue; the sealed-then-nest ordering is pinned in Rust
/// (<c>correction_trains_sealed_then_reports_to_the_nest</c>). A <b>local</b> row trains
/// "not spam" on its retained body and removes the flag. These drive the dispatch over a
/// fakeable <see cref="ISpamModelClientWrite"/> seam (the real <c>MailSettingsMachine</c> is
/// an unfakeable sealed UniFFI object) + a fake local-detection seam + MockNestRpcClient.
/// </summary>
public class ModerationViewModelSealedTrainTests
{
    // FfiObligationAction(id, content_type, content_id, category, confidence_per_mille, action, timestamp, issuer_nest)
    private static FfiObligationAction ServerRow(string contentId, string category, byte action, ushort perMille, long ts) =>
        new(0L, "post", contentId, category, perMille, action, ts, null);

    // LocalDetection(content_id, content_type, category, confidence_per_mille, timestamp)
    private static LocalDetection LocalRow(string contentId, string category, ushort perMille, long ts) =>
        new(contentId, "message", category, perMille, ts);

    /// <summary>A fake local-detection seam with a retained-body map (the windows twin of
    /// the session's <c>moderation_message_body</c> read) — records removals.</summary>
    private sealed class FakeLocal : IModerationLocalDetections
    {
        private readonly List<LocalDetection> _items;
        private readonly Dictionary<string, string> _bodies = new();
        public List<string> Removed { get; } = new();
        public FakeLocal(params LocalDetection[] items) => _items = items.ToList();
        public FakeLocal WithBody(string contentId, string body) { _bodies[contentId] = body; return this; }
        public IReadOnlyList<LocalDetection> Snapshot() => _items;
        public string? Body(string contentId) => _bodies.TryGetValue(contentId, out var b) ? b : null;
        public void Remove(string contentId)
        {
            Removed.Add(contentId);
            _items.RemoveAll(d => d.contentId == contentId);
        }
    }

    /// <summary>A fake seam standing in for the real (unfakeable)
    /// <c>MailSettingsMachine</c>: records the local-row client writes and the shared
    /// server-row correction flow's calls.</summary>
    private sealed class FakeSpamWrite : ISpamModelClientWrite
    {
        public bool SealedAvailable { get; set; }
        public SpamWriteResult NextResult { get; set; } = new(Sealed: true, SampleCount: 1);
        public (string Text, bool IsSpam)? LastTrain { get; private set; }
        public int TrainCalls { get; private set; }
        public List<(string ContentId, bool IsSpam)> Corrections { get; } = new();
        public System.Exception? CorrectionFailure { get; set; }
        public System.Action? OnCorrection { get; set; }
        public Task<bool> SealedSpamWriteAvailableAsync() => Task.FromResult(SealedAvailable);
        public Task<SpamWriteResult> TrainSpamModelClientAsync(string text, bool isSpam)
        {
            TrainCalls++;
            LastTrain = (text, isSpam);
            return Task.FromResult(NextResult);
        }

        public Task TrainModerationCorrectionAsync(string contentId, bool isSpam)
        {
            Corrections.Add((contentId, isSpam));
            OnCorrection?.Invoke();
            return CorrectionFailure is null ? Task.CompletedTask : Task.FromException(CorrectionFailure);
        }

        // Not exercised by the moderation-queue tests (history_op: None) — the mail-train
        // Insert consumer is FaunaApp.Tests/ConversationsViewModelMarkAsSpamTests.cs's
        // FakeSpamWrite. Present only so this class satisfies ISpamModelClientWrite.
        public Task<SpamWriteResult> TrainSpamModelClientMailAsync(
            string text, bool isSpam, byte[] messageId, string mailbox, string subject) =>
            throw new System.NotImplementedException();
    }

    // 1. Server row → exactly one shared-flow call carrying the row's content id and the
    //    not-spam verdict; the VM runs no per-app sealed write beside it (the
    //    sealed-then-nest ordering is the Rust test correction_trains_sealed_then_reports_to_the_nest).
    [Fact]
    public async Task Train_ServerRow_RunsTheSharedCorrectionFlowOnce()
    {
        var rpc = new MockNestRpcClient
        {
            NextModerationActions = new List<FfiObligationAction> { ServerRow("srv", "spam", 1, 900, 100) },
        };
        var spam = new FakeSpamWrite();
        var vm = new ModerationViewModel(rpc, local: null, spamWrite: spam);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.TrainCommand.ExecuteAsync(vm.Actions[0]);

        Assert.Equal(new[] { ("srv", false) }, spam.Corrections);
        Assert.Equal(0, spam.TrainCalls);
        Assert.Null(vm.ErrorMessage);
    }

    // 2. The flow's error is the surface's error: shown, and the row stays.
    [Fact]
    public async Task Train_ServerRow_FlowFailure_ShowsTheError()
    {
        var rpc = new MockNestRpcClient
        {
            NextModerationActions = new List<FfiObligationAction> { ServerRow("srv", "spam", 1, 900, 100) },
        };
        var spam = new FakeSpamWrite { CorrectionFailure = new System.InvalidOperationException("nest half failed") };
        var vm = new ModerationViewModel(rpc, local: null, spamWrite: spam);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.TrainCommand.ExecuteAsync(vm.Actions[0]);

        Assert.Single(spam.Corrections);
        Assert.NotNull(vm.ErrorMessage);
        Assert.Single(vm.Actions);
    }

    // 3. A successful correction re-reads the queue (the server no longer lists the row).
    [Fact]
    public async Task Train_ServerRow_Success_RefreshesTheQueue()
    {
        var rpc = new MockNestRpcClient
        {
            NextModerationActions = new List<FfiObligationAction> { ServerRow("srv", "spam", 1, 900, 100) },
        };
        var spam = new FakeSpamWrite { OnCorrection = () => rpc.NextModerationActions = new List<FfiObligationAction>() };
        var vm = new ModerationViewModel(rpc, local: null, spamWrite: spam);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.TrainCommand.ExecuteAsync(vm.Actions[0]);

        Assert.Empty(vm.Actions);
    }

    // 4. No seam at all → the correction cannot run: surfaced, never a silent no-op.
    [Fact]
    public async Task Train_ServerRow_NoSeam_ShowsAnError()
    {
        var rpc = new MockNestRpcClient
        {
            NextModerationActions = new List<FfiObligationAction> { ServerRow("srv", "spam", 1, 900, 100) },
        };
        var vm = new ModerationViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.TrainCommand.ExecuteAsync(vm.Actions[0]);

        Assert.NotNull(vm.ErrorMessage);
    }

    // 5. Local row, retained decrypted body present → client write (not-spam), removed, no shared flow.
    [Fact]
    public async Task Train_LocalRow_WithBody_WritesSealedModel_AndRemoves()
    {
        var rpc = new MockNestRpcClient();
        var local = new FakeLocal(LocalRow("bb", "spam", 720, 100)).WithBody("bb", "spam message body");
        var spam = new FakeSpamWrite { SealedAvailable = true, NextResult = new(Sealed: true, SampleCount: 3) };
        var vm = new ModerationViewModel(rpc, local, spam);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.TrainCommand.ExecuteAsync(vm.Actions[0]);

        // Local content trains "not spam" on the retained body (no server obligation exists),
        // then the client-side flag is removed. The shared correction flow never runs for a local row.
        Assert.Equal(("spam message body", false), spam.LastTrain);
        Assert.Contains("bb", local.Removed);
        Assert.Empty(spam.Corrections);
        Assert.Empty(vm.Actions);
    }

    // 6. Local row, body aged out of the session store (null) → just remove; no client write, no shared flow.
    [Fact]
    public async Task Train_LocalRow_NoBody_JustRemoves()
    {
        var rpc = new MockNestRpcClient();
        var local = new FakeLocal(LocalRow("bb", "spam", 720, 100)); // no retained body
        var spam = new FakeSpamWrite { SealedAvailable = true };
        var vm = new ModerationViewModel(rpc, local, spam);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.TrainCommand.ExecuteAsync(vm.Actions[0]);

        Assert.Equal(0, spam.TrainCalls);
        Assert.Contains("bb", local.Removed);
        Assert.Empty(spam.Corrections);
    }
}
