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
/// The windows moderation-queue <c>train-correction-button</c> writes the caller's
/// tier-1 spam model via the shared <b>sealed client-write path</b> when the nest
/// advertises <c>spam-model-sealed-at-rest</c>, and <b>degrades</b> to
/// <c>fauna.moderation.train</c> otherwise (<c>mail-spam.md</c> § Encrypted-mode
/// interaction; the co-design 1d surface switch, mirroring linux
/// <c>client.rs::{correct_moderation_row, train_moderation_flow}</c>). The capability
/// gate is load-bearing: a blind sealed write against a v-older nest double-seals →
/// the user's model becomes unreadable → cold-start reset = user-data loss
/// (<c>version-compatibility.md</c> I1/I2). These drive the dispatch over a fakeable
/// <see cref="ISpamModelClientWrite"/> seam (the real <c>MailSettingsMachine</c> is an
/// unfakeable sealed UniFFI object) + a fake local-detection seam + MockNestRpcClient.
/// </summary>
public class ModerationViewModelSealedTrainTests
{
    // FfiObligationAction(id, content_type, content_id, category, confidence_per_mille, action, timestamp)
    private static FfiObligationAction ServerRow(string contentId, string category, byte action, ushort perMille, long ts) =>
        new(0L, "post", contentId, category, perMille, action, ts);

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

    /// <summary>A fake sealed-write seam standing in for the real (unfakeable)
    /// <c>MailSettingsMachine</c>: a settable feature-presence flag + a canned write
    /// outcome, recording the trained (text, isSpam).</summary>
    private sealed class FakeSpamWrite : ISpamModelClientWrite
    {
        public bool SealedAvailable { get; set; }
        public SpamWriteResult NextResult { get; set; } = new(Sealed: true, SampleCount: 1);
        public (string Text, bool IsSpam)? LastTrain { get; private set; }
        public int TrainCalls { get; private set; }
        public Task<bool> SealedSpamWriteAvailableAsync() => Task.FromResult(SealedAvailable);
        public Task<SpamWriteResult> TrainSpamModelClientAsync(string text, bool isSpam)
        {
            TrainCalls++;
            LastTrain = (text, isSpam);
            return Task.FromResult(NextResult);
        }

        // Not exercised by the moderation-queue tests (history_op: None) — the mail-train
        // Insert consumer is FaunaApp.Tests/ConversationsViewModelMarkAsSpamTests.cs's
        // FakeSpamWrite. Present only so this class satisfies ISpamModelClientWrite.
        public Task<SpamWriteResult> TrainSpamModelClientMailAsync(
            string text, bool isSpam, byte[] messageId, string mailbox, string subject) =>
            throw new System.NotImplementedException();
    }

    // 1. Server row, nest sealed-at-rest available + body present → sealed client write, NO nest train.
    [Fact]
    public async Task Train_ServerRow_SealedAvailable_WritesSealedModel_NoNestTrain()
    {
        var rpc = new MockNestRpcClient
        {
            NextModerationActions = new List<FfiObligationAction> { ServerRow("srv", "spam", 1, 900, 100) },
            NextPostBody = new Dictionary<string, string> { ["srv"] = "buy cheap pills now" },
        };
        var spam = new FakeSpamWrite { SealedAvailable = true, NextResult = new(Sealed: true, SampleCount: 7) };
        var vm = new ModerationViewModel(rpc, local: null, spamWrite: spam);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.TrainCommand.ExecuteAsync(vm.Actions[0]);

        // A "not spam" correction (verdict "ham" → is_spam=false) on the fetched post body,
        // written through the sealed client path — the nest train never fires.
        Assert.Equal(("buy cheap pills now", false), spam.LastTrain);
        Assert.Null(rpc.LastTrain);
    }

    // 2. Server row, nest NOT sealed-at-rest → degrade to fauna.moderation.train (ham), NO client write.
    [Fact]
    public async Task Train_ServerRow_SealedUnavailable_DegradesToNestTrain()
    {
        var rpc = new MockNestRpcClient
        {
            NextModerationActions = new List<FfiObligationAction> { ServerRow("srv", "spam", 1, 900, 100) },
            NextPostBody = new Dictionary<string, string> { ["srv"] = "should not be read" },
        };
        var spam = new FakeSpamWrite { SealedAvailable = false };
        var vm = new ModerationViewModel(rpc, local: null, spamWrite: spam);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.TrainCommand.ExecuteAsync(vm.Actions[0]);

        // Pre-check false ⇒ never pay for the body fetch, never client-write; degrade to the nest.
        Assert.Equal(0, spam.TrainCalls);
        Assert.NotNull(rpc.LastTrain);
        Assert.Equal("srv", rpc.LastTrain!.Value.ContentId);
        Assert.Equal("ham", rpc.LastTrain!.Value.Verdict);
    }

    // 3. Server row, sealed available but the machine reports ServerPath (Sealed==false) → fall through to nest train.
    [Fact]
    public async Task Train_ServerRow_MachineServerPath_FallsThroughToNestTrain()
    {
        var rpc = new MockNestRpcClient
        {
            NextModerationActions = new List<FfiObligationAction> { ServerRow("srv", "spam", 1, 900, 100) },
            NextPostBody = new Dictionary<string, string> { ["srv"] = "body text" },
        };
        var spam = new FakeSpamWrite { SealedAvailable = true, NextResult = new(Sealed: false, SampleCount: 0) };
        var vm = new ModerationViewModel(rpc, local: null, spamWrite: spam);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.TrainCommand.ExecuteAsync(vm.Actions[0]);

        // The client write was attempted on the fetched body…
        Assert.Equal(("body text", false), spam.LastTrain);
        // …but the machine took the server path, so the correction falls through to the nest train.
        Assert.NotNull(rpc.LastTrain);
        Assert.Equal("ham", rpc.LastTrain!.Value.Verdict);
    }

    // 4. Server row, sealed available but the post body is empty/absent → degrade to nest train.
    [Fact]
    public async Task Train_ServerRow_EmptyBody_DegradesToNestTrain()
    {
        var rpc = new MockNestRpcClient
        {
            NextModerationActions = new List<FfiObligationAction> { ServerRow("srv", "spam", 1, 900, 100) },
            NextPostBody = new Dictionary<string, string>(),   // no body for "srv"
        };
        var spam = new FakeSpamWrite { SealedAvailable = true };
        var vm = new ModerationViewModel(rpc, local: null, spamWrite: spam);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.TrainCommand.ExecuteAsync(vm.Actions[0]);

        Assert.Equal(0, spam.TrainCalls);   // no body ⇒ no client write
        Assert.NotNull(rpc.LastTrain);       // degrade to the nest
        Assert.Equal("ham", rpc.LastTrain!.Value.Verdict);
    }

    // 5. Local row, retained decrypted body present → client write (not-spam), removed, NO nest train.
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
        // then the client-side flag is removed. The nest train never fires for a local row.
        Assert.Equal(("spam message body", false), spam.LastTrain);
        Assert.Contains("bb", local.Removed);
        Assert.Null(rpc.LastTrain);
        Assert.Empty(vm.Actions);
    }

    // 6. Local row, body aged out of the session store (null) → just remove; no client write, no nest train.
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
        Assert.Null(rpc.LastTrain);
    }
}
