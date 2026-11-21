using FaunaApp.Core.Helpers;
using FaunaApp.Core.Services;
using uniffi.fauna_conversations;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The queued own copy's lines on windows (<c>devices.md</c> § Offline compose → <i>What
/// "resurfaced as drafts" means</i>; <c>account-offline-mutation.md</c> § The
/// offline-mutation contract → <i>The unsent-changes surface</i>, point 2): from the
/// shared <c>MessageSnapshot.queued_send</c>, a waiting copy carries the
/// <c>dm-message-queued</c> line and no <c>dm-message-unsent</c>; a parked one the
/// <c>dm-message-unsent</c> line with the shared reason and no queued line; a sent message
/// neither. The bubble offers retry and discard exactly when the unsent line is present.
/// </summary>
[Collection("StringsGlobal")]
public class ConversationsQueuedSendTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["conversations/unified/dm_message_queued"] = "Waiting to send",
            ["conversations/unified/dm_message_unsent"] = "Not sent — {reason}",
            ["settings/unsent_changes/reason_mls_not_a_member"] = "You are no longer a member of this conversation",
        };

        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public ConversationsQueuedSendTests() => Strings.Initialize(new FakeLocalizer());

    [Fact]
    public void AWaitingCopyIsMarkedQueued_AndNotUnsent()
    {
        var state = new QueuedSendState.Queued();

        Assert.Equal("Waiting to send", QueuedSendLines.Queued(state));
        Assert.Null(QueuedSendLines.Unsent(state));
    }

    [Fact]
    public void AParkedCopyIsMarkedUnsentWithTheSharedReason_AndNotQueued()
    {
        var state = new QueuedSendState.Unsent(
            "mls_not_a_member",
            new uniffi.fauna_core.LocalizedText(
                "settings.unsent_changes.reason_mls_not_a_member", new Dictionary<string, string>()));

        Assert.Null(QueuedSendLines.Queued(state));
        Assert.Equal(
            "Not sent — You are no longer a member of this conversation",
            QueuedSendLines.Unsent(state));
    }

    [Fact]
    public void ASentMessageCarriesNeitherLine()
    {
        Assert.Null(QueuedSendLines.Queued(null));
        Assert.Null(QueuedSendLines.Unsent(null));
    }
}
