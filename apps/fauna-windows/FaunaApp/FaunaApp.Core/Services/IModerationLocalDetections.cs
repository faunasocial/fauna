using System.Collections.Generic;
using uniffi.fauna_client_moderation;
using uniffi.fauna_conversations;

namespace FaunaApp.Core.Services;

/// <summary>
/// The <b>client half</b> of the moderation queue: the conversations session's
/// retained post-decrypt <b>local detections</b> (the encrypted-mode social-content
/// moderation signal the nest cannot produce — <c>moderation.md</c> § Layout &amp;
/// flow) plus the client-side removal a <c>train-correction-button</c> applies to a
/// local row.
///
/// <para>A thin hand-written seam over the shared-Rust
/// <see cref="ConversationsSession"/> — mirroring how <see cref="INestRpcClient"/>
/// seams the nest WS-RPC — so <c>ModerationViewModel</c>'s server∪local union and
/// train-routing are unit-testable without a live MLS session. A <c>null</c> seam on
/// the VM means "no session" → the queue is the server rows alone (the windows twin of
/// linux's <c>active_session()</c> returning <c>None</c>).</para>
/// </summary>
internal interface IModerationLocalDetections
{
    /// <summary>The session's retained post-decrypt local detections, newest-first
    /// (<c>ConversationsSession::moderation_local_detections</c>). Empty until the
    /// receive loop classifies an incoming spam message post-decrypt.</summary>
    IReadOnlyList<LocalDetection> Snapshot();

    /// <summary>The retained decrypted body text for a local detection's content
    /// (<c>ConversationsSession::moderation_message_body</c>), read <b>before</b> the
    /// train correction removes the flag so the client can train its spam model on the
    /// text (the local half of the 1d sealed-write path — <c>mail-spam.md</c> §
    /// Encrypted-mode interaction). <c>null</c> once the message ages out of the session
    /// store (→ the correction just removes the flag, no train).</summary>
    string? Body(string contentId);

    /// <summary>Drop the local detection for <paramref name="contentId"/> after the
    /// user trains a correction on that queue row
    /// (<c>ConversationsSession::moderation_remove_local_detection</c>). No-op if it is
    /// a server row (not in this store) or already gone.</summary>
    void Remove(string contentId);
}

/// <summary>
/// Production <see cref="IModerationLocalDetections"/> over the shared-Rust
/// conversations session built at login (carried on <c>ServiceClients.ConvSession</c>,
/// the same session <c>ConversationsPage</c> renders off). The reader + remover are
/// the UniFFI-exported <see cref="ConversationsSession"/> methods; the shared
/// post-decrypt classify writer already fires on this session, so reading the store
/// is all the windows queue needs (no per-app writer).
/// </summary>
internal sealed class SessionLocalDetections : IModerationLocalDetections
{
    private readonly ConversationsSession _session;

    public SessionLocalDetections(ConversationsSession session) => _session = session;

    public IReadOnlyList<LocalDetection> Snapshot() => _session.ModerationLocalDetections();

    public string? Body(string contentId) => _session.ModerationMessageBody(contentId);

    public void Remove(string contentId) => _session.ModerationRemoveLocalDetection(contentId);
}
