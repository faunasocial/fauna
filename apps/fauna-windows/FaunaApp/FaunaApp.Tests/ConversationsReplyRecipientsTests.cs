using System;
using System.ComponentModel;
using System.Linq;
using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_conversations;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic real-FFI tests for the reply-recipient editable "To" line
/// (#2b; <c>conversations.md</c> § Participants vs. reply recipients). The
/// windows FlaUI e2e (<c>test_conversations_reply_recipients.py --client
/// windows</c>) flakes solo on win-arm64, so these VM-over-real-manager tests
/// are the authoritative windows gate: they prove the C# binding round-trips the
/// shared <c>start_reply</c> / <c>add_reply_recipient</c> /
/// <c>remove_reply_recipient</c> calls + the <c>supports_recipient_selection</c>
/// capability + the FFI-exported <c>try_parse_typed_address</c> recognizer
/// (priority #2/#4 — windows consumes the shared parser, no per-app
/// duplicate). The seeding / minus-self / dedup *logic* itself is covered by
/// shared-Rust unit tests (<c>fauna-conversations</c> manager + smtp backend).
/// </summary>
public class ConversationsReplyRecipientsTests
{
    private sealed class SyncObserver : SnapshotObserver, INotifyPropertyChanged
    {
        public event PropertyChangedEventHandler? PropertyChanged;
        public void OnChanged() =>
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(string.Empty));
    }

    private static ConversationsManager NewManager()
    {
        var m = new ConversationsManager();
        m.InstallMockBackendsForTest();
        return m;
    }

    /// <summary>Inject one inbound mail (alice → me) and return the ids.</summary>
    private static (ConversationsManager m, string tid, string mid) MailThread()
    {
        var m = NewManager();
        var msg = new RailInboundMessage(
            @rail: Rail.Smtp,
            @sender: new TypedAddress.Email(@emailAddress: "alice@host.test"),
            @recipients: new TypedAddress[]
            {
                new TypedAddress.Email(@emailAddress: "me@self-nest.test"),
            },
            @subject: "Lunch",
            @body: "hi",
            @bodyFormat: BodyFormat.PlainText,
            @timestampMs: 0,
            @messageId: "msg-1",
            @inReplyTo: null,
            @attachments: Array.Empty<AttachmentSnapshot>(),
            @badges: new MessageBadges(
                @encrypted: false, @signed: false, @verified: false,
                @contentWarning: null),
            @legalTakedownRef: null,
            @planeRef: null);
        m.InjectInboundForTest(msg);
        var tid = m.Snapshot().threads[0].threadId;
        var mid = m.ThreadDetail(tid)!.messages[0].messageId;
        return (m, tid, mid);
    }

    // Display a TypedAddress via the FFI-exported shared switch — no per-app
    // duplicate (priority #2/#4; mirrors the production ConversationsPage swap).
    private static string AddrDisplay(TypedAddress a) =>
        FaunaConversationsMethods.TypedAddressDisplay(a);

    [Fact]
    public void StartReply_seeds_to_line_with_sender_then_remove_clears_it()
    {
        var (m, tid, mid) = MailThread();
        var vm = new ConversationsViewModel(m, new SyncObserver());
        vm.OpenThread(tid);

        // Mail threads offer recipient selection (the To line is shown).
        Assert.True(vm.SelectedDetail!.capabilities.supportsRecipientSelection);

        vm.StartReply(tid, mid, replyAll: false);
        var recips = vm.SelectedDetail!.compose.replyRecipients;
        Assert.Single(recips);
        Assert.Equal("alice@host.test", AddrDisplay(recips[0]));

        // × drops the recipient from this reply only.
        vm.RemoveReplyRecipient(tid, recips[0]);
        Assert.Empty(vm.SelectedDetail!.compose.replyRecipients);
        // Thread membership untouched (alice + me).
        Assert.Equal(2, vm.SelectedDetail!.participants.Length);
    }

    [Fact]
    public void AddReplyRecipient_appends_a_parsed_address()
    {
        var (m, tid, mid) = MailThread();
        var vm = new ConversationsViewModel(m, new SyncObserver());
        vm.OpenThread(tid);
        vm.StartReply(tid, mid, replyAll: false); // seeds [alice]

        var dave = FaunaConversationsMethods.TryParseTypedAddress("dave@host.test");
        Assert.NotNull(dave);
        vm.AddReplyRecipient(tid, dave!);

        var displays = vm.SelectedDetail!.compose.replyRecipients
            .Select(AddrDisplay).OrderBy(s => s).ToArray();
        Assert.Equal(new[] { "alice@host.test", "dave@host.test" }, displays);
    }

    [Fact]
    public void FaunaMls_thread_hides_to_line_and_start_reply_seeds_nothing()
    {
        var m = NewManager();
        var tid = m.CreateMlsGroup(new TypedAddress[]
        {
            new TypedAddress.Email(@emailAddress: "bob@self-nest.test"),
        });
        var vm = new ConversationsViewModel(m, new SyncObserver());
        vm.OpenThread(tid);

        // Recipients ARE the group on FaunaMls — no editable To line.
        Assert.False(vm.SelectedDetail!.capabilities.supportsRecipientSelection);
        // start_reply on a non-selection rail leaves the To line empty.
        vm.StartReply(tid, "nonexistent-msg", replyAll: true);
        Assert.Empty(vm.SelectedDetail!.compose.replyRecipients);
    }

    [Fact]
    public void TryParseTypedAddress_recognizes_email_and_rejects_plain_string()
    {
        Assert.IsType<TypedAddress.Email>(
            FaunaConversationsMethods.TryParseTypedAddress("alice@host.test"));
        Assert.Null(FaunaConversationsMethods.TryParseTypedAddress("plain-string"));
    }

    /// <summary>The FFI-exported <c>typed_address_display</c> renders each rail's
    /// canonical display field, so windows (and the other native apps) consume
    /// one shared switch instead of a per-app duplicate (priority #2/#4). Runs
    /// against the real native dll, so it gates Rust↔C# conformance.</summary>
    [Fact]
    public void TypedAddressDisplay_renders_each_rail_canonical_field()
    {
        Assert.Equal("alice@nest.example", FaunaConversationsMethods.TypedAddressDisplay(
            new TypedAddress.Fauna(@handle: "alice@nest.example", @actorId: new byte[32])));
        Assert.Equal("bob@example.com", FaunaConversationsMethods.TypedAddressDisplay(
            new TypedAddress.Email(@emailAddress: "bob@example.com")));
        // A bridged address shows the far network's own spelling, not the bridge.
        Assert.Equal("@dave:example.org", FaunaConversationsMethods.TypedAddressDisplay(
            new TypedAddress.Bridged(@bridgeId: "matrix", @address: "@dave:example.org")));
    }
}
