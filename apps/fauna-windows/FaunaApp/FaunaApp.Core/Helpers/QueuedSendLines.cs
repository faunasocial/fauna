using FaunaApp.Core.Services;
using uniffi.fauna_conversations;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// The queued own copy's lines (<c>devices.md</c> § Offline compose → <i>What
/// "resurfaced as drafts" means</i>; <c>account-offline-mutation.md</c> § The
/// offline-mutation contract → <i>The unsent-changes surface</i>, point 2), from the
/// shared <c>MessageSnapshot.queued_send</c> the conversations manager projects on every
/// thread read: <c>dm-message-queued</c> (<i>Waiting to send</i>) under a copy still in the
/// device outbox, <c>dm-message-unsent</c> (<i>Not sent — &lt;reason&gt;</i>, the reason the
/// shared Rust rendered from the stored code) under one the drain parked. Only the unsent
/// copy offers <c>dm-message-retry-button</c> and <c>dm-message-discard-button</c>. Nothing
/// is decided here but which string carries the shared state.
/// </summary>
internal static class QueuedSendLines
{
    /// <summary>The <c>dm-message-queued</c> line, or <c>null</c> when the copy is not waiting.</summary>
    internal static string? Queued(QueuedSendState? state) =>
        state is QueuedSendState.Queued ? Strings.Get("conversations/unified/dm_message_queued") : null;

    /// <summary>The <c>dm-message-unsent</c> line, or <c>null</c> when the copy is not parked.</summary>
    internal static string? Unsent(QueuedSendState? state) =>
        state is QueuedSendState.Unsent unsent
            ? Strings.Resolve(new uniffi.fauna_core.LocalizedText(
                "conversations.unified.dm_message_unsent",
                new Dictionary<string, string> { ["reason"] = Strings.Resolve(unsent.@reason) }))
            : null;
}
