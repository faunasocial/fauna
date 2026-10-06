using System.Linq;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_conversations;

namespace FaunaApp.Tests;

/// <summary>
/// The windows conversation-message <c>dm-message-mark-as-spam-button</c> — the
/// <b>live <c>Insert</c> consumer</b> (mail-spam.md § Wire shapes / § Encrypted-mode
/// interaction — the follow-on to the moderation-queue 1d
/// switch which left <c>history_op: None</c>). Drives
/// <see cref="ConversationsViewModel.MarkMessageSpamAsync"/> over a fakeable
/// <see cref="ISpamModelClientWrite"/> seam (the real <c>MailSettingsMachine</c> is an
/// unfakeable sealed UniFFI object — mirrors <c>ModerationViewModelSealedTrainTests</c>).
/// The method never touches the <c>ConversationsManager</c>, so a bare mock-backed
/// manager is enough to construct the VM.
/// </summary>
public class ConversationsViewModelMarkAsSpamTests
{
    private sealed class NoopObserver : SnapshotObserver
    {
        public void OnChanged() { }
    }

    private static ConversationsViewModel NewVm(ISpamModelClientWrite? spamWrite)
    {
        var m = new ConversationsManager();
        m.InstallMockBackendsForTest();
        return new ConversationsViewModel(m, new NoopObserver(), spamWrite);
    }

    /// <summary>A fake sealed-write seam standing in for the real (unfakeable)
    /// <c>MailSettingsMachine</c>: a settable feature-presence flag + a canned write
    /// outcome, recording the trained mail-train args.</summary>
    private sealed class FakeSpamWrite : ISpamModelClientWrite
    {
        public bool SealedAvailable { get; set; } = true;
        public SpamWriteResult NextResult { get; set; } = new(Sealed: true, SampleCount: 1);
        public (string Text, bool IsSpam, byte[] MessageId, string Mailbox, string Subject)? LastMailTrain
        {
            get; private set;
        }
        public int MailTrainCalls { get; private set; }

        public Task<bool> SealedSpamWriteAvailableAsync() => Task.FromResult(SealedAvailable);

        // Not exercised here — the moderation-queue's history_op: None train is
        // ModerationViewModelSealedTrainTests's job.
        public Task<SpamWriteResult> TrainSpamModelClientAsync(string text, bool isSpam) =>
            throw new System.NotImplementedException();

        public Task<SpamWriteResult> TrainSpamModelClientMailAsync(
            string text, bool isSpam, byte[] messageId, string mailbox, string subject)
        {
            MailTrainCalls++;
            LastMailTrain = (text, isSpam, messageId, mailbox, subject);
            return Task.FromResult(NextResult);
        }
    }

    // 1. Received message, sealed-write available, no subject line → sealed mail-train
    // fires with the right args (body, isSpam:true, message_id bytes, INBOX, snippet subject).
    [Fact]
    public async Task MarkMessageSpamAsync_ReceivedMessage_SealedAvailable_WritesSealedMailTrain()
    {
        var spam = new FakeSpamWrite { SealedAvailable = true };
        var vm = NewVm(spam);

        await vm.MarkMessageSpamAsync("msg-1", "buy cheap pills now", subjectLine: null, isOwn: false);

        Assert.Equal(1, spam.MailTrainCalls);
        var (text, isSpam, messageId, mailbox, subject) = spam.LastMailTrain!.Value;
        Assert.Equal("buy cheap pills now", text);
        Assert.True(isSpam);
        Assert.Equal("msg-1", System.Text.Encoding.UTF8.GetString(messageId));
        Assert.Equal("INBOX", mailbox);
        // No subject line ⇒ the VM passes it through raw (empty) — the shared
        // machine derives the body-snippet fallback server-side, not the VM.
        Assert.Equal("", subject);
    }

    // 2. Own message → gated off entirely, regardless of sealed availability (defense
    // in depth — the menu never offers this gesture on an own message either).
    [Fact]
    public async Task MarkMessageSpamAsync_OwnMessage_GatedOff_NoTrainCall()
    {
        var spam = new FakeSpamWrite { SealedAvailable = true };
        var vm = NewVm(spam);

        await vm.MarkMessageSpamAsync("msg-2", "hello there", subjectLine: null, isOwn: true);

        Assert.Equal(0, spam.MailTrainCalls);
    }

    // 3. Sealed write unavailable → silent no-op. Unlike the moderation-queue server-row
    // correction, a conversation message has NO server-train fallback to degrade to.
    [Fact]
    public async Task MarkMessageSpamAsync_SealedUnavailable_SilentNoOp_NoServerFallback()
    {
        var spam = new FakeSpamWrite { SealedAvailable = false };
        var vm = NewVm(spam);

        await vm.MarkMessageSpamAsync("msg-3", "buy cheap pills now", subjectLine: null, isOwn: false);

        Assert.Equal(0, spam.MailTrainCalls);
    }

    // 4. No seam at all (mail not enabled / no session) → silent no-op, no throw.
    [Fact]
    public async Task MarkMessageSpamAsync_NoSeam_SilentNoOp()
    {
        var vm = NewVm(spamWrite: null);

        await vm.MarkMessageSpamAsync("msg-4", "buy cheap pills now", subjectLine: null, isOwn: false);
    }

    // 5. Subject line present → sealed verbatim, no snippet fallback.
    [Fact]
    public async Task MarkMessageSpamAsync_SubjectPresent_UsesItVerbatim()
    {
        var spam = new FakeSpamWrite { SealedAvailable = true };
        var vm = NewVm(spam);

        await vm.MarkMessageSpamAsync("msg-5", "body text", subjectLine: "Re: hello", isOwn: false);

        Assert.Equal("Re: hello", spam.LastMailTrain!.Value.Subject);
    }

    // 6. Long body, no subject → the VM still passes the raw (empty) subject
    // through; the snippet-with-ellipsis derivation is the shared machine's job
    // now (machine.rs spam_subject_snippet), not asserted here.
    [Fact]
    public async Task MarkMessageSpamAsync_LongBodyNoSubject_PassesEmptySubjectThrough()
    {
        var spam = new FakeSpamWrite { SealedAvailable = true };
        var vm = NewVm(spam);
        var longBody = string.Concat(Enumerable.Repeat("word ", 30));

        await vm.MarkMessageSpamAsync("msg-6", longBody, subjectLine: null, isOwn: false);

        Assert.Equal("", spam.LastMailTrain!.Value.Subject);
    }

    // 7. Empty/whitespace-only body → silent no-op (nothing meaningful to train on).
    [Fact]
    public async Task MarkMessageSpamAsync_EmptyBody_SilentNoOp()
    {
        var spam = new FakeSpamWrite { SealedAvailable = true };
        var vm = NewVm(spam);

        await vm.MarkMessageSpamAsync("msg-7", "   ", subjectLine: null, isOwn: false);

        Assert.Equal(0, spam.MailTrainCalls);
    }
}
