using System.ComponentModel;
using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_conversations;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit tests for the conversations SEND path (Slice 2 of the
/// windows FaunaMls wiring). The windows FlaUI e2e
/// (<c>test_*.py --client windows</c>) flakes solo on win-arm64, so this
/// real-FFI VM test is the authoritative gate: it drives the shared
/// <see cref="ConversationsManager"/> (with the test-helpers mock backend) and
/// asserts that <see cref="ConversationsViewModel.Send"/> issues the manager
/// send — the same <c>dm-send-button → manager.send(thread_id)</c> route the
/// goal doc names (<c>conversations.md</c> § User actions). The observable
/// post-condition of a successful mock-backend send (<c>backends::mock::send</c>
/// returns <c>Ok</c>) is that the manager appends the sent message to the
/// thread's <c>messages</c> and clears the compose body draft
/// (<c>manager.rs::send</c>).
/// </summary>
public class ConversationsSendTests
{
    /// <summary>
    /// Mirrors the production observer contract the VM depends on: the manager
    /// calls <c>OnChanged</c> after a mutation, the observer raises
    /// <see cref="INotifyPropertyChanged"/>, and the VM clears its cached
    /// snapshot in response (so a post-send read sees fresh state). Identical
    /// to <see cref="ConversationsViewModelTests"/>'s observer.
    /// </summary>
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

    [Fact]
    public async Task Send_appends_the_sent_message_to_the_thread()
    {
        var m = NewManager();
        var threadId = m.CreateMlsGroup(new TypedAddress[]
        {
            new TypedAddress.Email(@emailAddress: "bob@self-nest.test"),
        });
        var vm = new ConversationsViewModel(m, new SyncObserver());

        // A freshly-created thread has no messages.
        Assert.Empty(m.ThreadDetail(threadId)!.messages);

        // The user selects the thread and types a body, then clicks Send
        // (dm-send-button → manager.send(thread_id)).
        vm.OpenThread(threadId);
        vm.SetComposeBody(threadId, "hello bob");
        await vm.Send(threadId);

        // A successful mock-backend send appends the sent message and clears
        // the compose body draft (manager.rs::send Ok branch).
        var detail = m.ThreadDetail(threadId)!;
        Assert.Single(detail.messages);
        Assert.Equal("hello bob", detail.messages[0].body);
        Assert.Equal("", detail.compose.bodyDraft);
    }

    /// <summary>The app's observer: it enqueues its PropertyChanged on the dispatcher
    /// (<c>ConversationsNotifyObserver</c>), so the VM's cache is cleared only after the
    /// continuation that awaited a mutation has run. This one never delivers, which
    /// is that continuation's whole view of it.</summary>
    private sealed class DeferredObserver : SnapshotObserver, INotifyPropertyChanged
    {
        public event PropertyChangedEventHandler? PropertyChanged;
        public void OnChanged() { }
    }

    [Fact]
    public async Task A_read_right_after_an_awaited_mutator_sees_the_mutation_not_the_cached_snapshot()
    {
        var m = NewManager();
        var threadId = m.CreateMlsGroup(new TypedAddress[]
        {
            new TypedAddress.Email(@emailAddress: "bob@self-nest.test"),
        });
        var vm = new ConversationsViewModel(m, new DeferredObserver());
        vm.OpenThread(threadId);
        vm.SetComposeBody(threadId, "hello bob");
        // The page paints before the send: the detail is cached, with no message yet.
        Assert.Empty(vm.SelectedDetail!.messages);

        await vm.Send(threadId);

        // ConversationsPage repaints in this same continuation (`await …; Refresh();`),
        // before any observer tick could clear the cache.
        var painted = Assert.Single(vm.SelectedDetail!.messages);
        Assert.Equal("hello bob", painted.body);
    }
}
