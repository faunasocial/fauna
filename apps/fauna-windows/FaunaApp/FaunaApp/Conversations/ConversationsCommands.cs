// testing.md convention 15 — automation surface, compiled out of release
// artifacts. Reached ONLY from Testing/TestAgent's command table (verified: no
// other caller in the tree), and every handler here calls a `*ForTest` UniFFI
// seam, which the production FFI flavor does not export. Gated with the agent.
#if DEBUG || FAUNA_E2E_AGENT
using System;
using System.Collections.Generic;
using System.Text.Json;
using uniffi.fauna_conversations;

namespace FaunaApp.Conversations;

/// <summary>
/// E2E bridge command handlers for the unified conversations page.
/// Mirrors the OnboardingMachine call_machine_method route: the test
/// process posts JSON commands to the bridge, the running app polls
/// them via TestAgent, and dispatches to the live
/// <c>ConversationsManager</c> singleton via these helpers.
///
/// Only available when both the <c>uniffi</c> and <c>test-helpers</c>
/// features are on (gated by the windows-ffi build), since
/// <see cref="ConversationsManager.InjectInboundForTest"/> and
/// <see cref="ConversationsManager.CreateMlsGroup"/> are themselves
/// feature-gated. Production builds drop both.
/// </summary>
internal static class ConversationsCommands
{
    /// <summary>
    /// <c>conversations_inject_inbound</c>: hand the payload JSON whole to the shared
    /// parser (<c>ConversationsManager::inject_inbound_from_test_json</c>), never a
    /// key-by-key re-parse here. One parser is one seam: <c>recipients</c>, the mail
    /// rail's real-self resolution, attachments, labels, <c>is_own</c> and
    /// <c>force_subject_change</c> behave as on tui, linux, web and apple, and a key
    /// added to the payload reaches windows with them. A payload the parser refuses
    /// throws, and the agent reports the command failed (convention 11).
    /// </summary>
    public static void InjectInbound(Dictionary<string, object?> command) =>
        ConversationsManagerHost.Instance.InjectInboundFromTestJson(JsonSerializer.Serialize(command));

    /// <summary>
    /// <c>conversations_evict_attachment</c> <c>{thread_id, filename}</c>: drop this
    /// device's cached bytes of every attachment named <c>filename</c> in the thread,
    /// exactly as the store's budget eviction drops one entry (the shared
    /// <c>evict_thread_attachments_for_test</c>, which redraws). Evicting nothing
    /// THROWS, so the agent reports the command failed rather than acking a no-op a
    /// render would then be asserted over (convention 11). Twin of linux's
    /// <c>handle_conversations_evict_attachment</c>.
    /// </summary>
    public static void EvictAttachment(Dictionary<string, object?> command)
    {
        var threadId = RequireThreadId(command);
        var filename = GetString(command, "filename");
        if (string.IsNullOrEmpty(filename))
        {
            throw new ArgumentException("the payload carries no `filename`");
        }
        var evicted = ConversationsManagerHost.Instance.EvictThreadAttachmentsForTest(threadId, filename);
        if (evicted == 0)
        {
            throw new InvalidOperationException(
                $"no resident attachment named \"{filename}\" in thread \"{threadId}\"");
        }
    }

    public static void CreateMlsGroup(Dictionary<string, object?> command)
    {
        var participants = new List<TypedAddress>();
        if (command.TryGetValue("participants", out var raw) && raw is JsonElement arr
            && arr.ValueKind == JsonValueKind.Array)
        {
            foreach (var p in arr.EnumerateArray())
            {
                var s = p.GetString();
                if (string.IsNullOrEmpty(s)) continue;
                participants.Add(AddressForRail(Rail.FaunaMls, s));
            }
        }
        ConversationsManagerHost.Instance.CreateMlsGroup(participants.ToArray());
    }

    /// <summary>
    /// Stamp <c>thread_id</c>'s compose into <c>send_state = Failed { reason }</c>
    /// and select it — the same observable state a backend send error leaves —
    /// so the e2e can assert the page-level <c>error-message</c> surfaces the
    /// reason. There is no product path that fails a send on demand (a mail-OFF
    /// nest queues and returns Ok), so the deterministic-failure test drives this
    /// <c>test-helpers</c> injection seam. Mirrors linux
    /// <c>handle_conversations_inject_send_failure</c>; payload <c>{thread_id, reason}</c>.
    /// </summary>
    public static void InjectSendFailure(Dictionary<string, object?> command)
    {
        var threadId = RequireThreadId(command);
        var reason = GetString(command, "reason");
        if (string.IsNullOrEmpty(reason)) reason = "nest rejected fauna.email.send";
        ConversationsManagerHost.Instance.InjectSendFailureForTest(threadId, reason);
    }

    /// <summary>
    /// Stamp a PRE-RESOLVED link preview for <c>url</c> via
    /// <c>ConversationsManager::seed_resolved_link_preview_for_test</c>, so an
    /// injected bubble whose body is the standalone <c>[url](url)</c> paragraph
    /// folds its <c>LinkPreview</c> block <c>Resolved</c> and paints the
    /// <c>link-preview-card</c> (render-model.md § D4). A *real* resolve needs a
    /// live nest fetch of an OpenGraph page, so the cross-app e2e seeds the card
    /// deterministically instead of driving <c>fauna.linkpreview.resolve</c>.
    ///
    /// The card itself has been built on windows since <c>a141c238c5</c>; this
    /// seam is the only reason the shared witness could not yet speak for this
    /// column. Mirrors linux <c>handle_conversations_seed_resolved_link_preview</c>
    /// and apple <c>ConversationsTestInject.seedResolvedLinkPreview</c>; payload
    /// <c>{url, title, description, image_hash}</c>.
    /// </summary>
    public static void SeedResolvedLinkPreview(Dictionary<string, object?> command)
    {
        var url = GetString(command, "url");
        if (string.IsNullOrEmpty(url))
        {
            throw new ArgumentException("the payload carries no `url`");
        }
        var imageHash = GetString(command, "image_hash");
        // An empty string is not a hash — every sibling app normalizes it to
        // null, and `Some("")` would fold a card with a broken image slot.
        if (string.IsNullOrEmpty(imageHash)) imageHash = null;
        ConversationsManagerHost.Instance.SeedResolvedLinkPreviewForTest(
            url,
            GetString(command, "title") ?? string.Empty,
            GetString(command, "description") ?? string.Empty,
            imageHash);
    }

    /// <summary>
    /// Drive <c>ConversationsManager::select_thread_and_message</c> — the same
    /// call <c>SearchResultsPage</c>'s <c>Mail</c> row activation makes through
    /// <c>ConversationsViewModel.OpenThreadAndMessage</c> — so the e2e can prove
    /// the downstream half of the contract (the named message is marked and
    /// scrolled into view, conversations.md § The selected message) without
    /// building a real local content index to search.
    ///
    /// Mirrors linux <c>handle_conversations_select_message</c> and apple
    /// <c>ConversationsSendTestCommand.selectMessage</c>; payload
    /// <c>{thread_id, message_id}</c>.
    /// </summary>
    public static void SelectMessage(Dictionary<string, object?> command)
    {
        var threadId = RequireThreadId(command);
        var messageId = GetString(command, "message_id");
        if (string.IsNullOrEmpty(messageId))
        {
            throw new ArgumentException("the payload carries no `message_id`");
        }
        ConversationsManagerHost.Instance.SelectThreadAndMessage(threadId, messageId);
    }

    /// <summary>
    /// Stamp <c>ConversationsSnapshot.error</c> via
    /// <c>ConversationsManager::inject_page_error_for_test</c> — the observable
    /// state a failed membership/label wire op leaves — so the e2e can assert
    /// the page-level <c>error-message</c> surface (conversations.md § Errors &amp;
    /// edge cases). The membership twin of <see cref="InjectSendFailure"/>, for
    /// the same reason: no product path fails one of those ops on demand.
    /// Mirrors linux's <c>handle_conversations_inject_page_error</c>; payload
    /// <c>{key, message}</c>. See <c>actions/conversations.py::inject_page_error_for_test</c>.
    /// </summary>
    public static void InjectPageError(Dictionary<string, object?> command)
    {
        var message = GetString(command, "message");
        if (string.IsNullOrEmpty(message)) return;
        var key = GetString(command, "key");
        if (string.IsNullOrEmpty(key)) key = "conversations.unified.error_add_participant";
        ConversationsManagerHost.Instance.InjectPageErrorForTest(
            new uniffi.fauna_core.LocalizedText(key, new Dictionary<string, string> { ["message"] = message }));
    }

    // ── real-wire (FaunaMls) e2e commands ─────────────────────────
    //
    // The native twins of linux `conv_backend::{request_e2e_activation,
    // e2e_resolve_send_new, e2e_send}` (apps/fauna-linux/src/conversations/
    // conv_backend.rs) and web `conversations.ts`'s three `conversations_real_*`
    // cases. They drive the SAME `ConversationsManager` the real receive loop is
    // wired to, so a tier_3 test can exercise a fauna-native MLS conversation
    // end-to-end instead of the mock rail (priority #1/#4 — windows was the
    // deviant client here, so this is a lift, not an invention).
    //
    // Blocking note: `ProcessCommand` runs on a thread-pool thread (TestAgent's
    // `Task.Run(PollLoopAsync)`), never the UI thread, and none of these manager
    // calls touch WinUI bound state — so `.GetAwaiter().GetResult()` is the direct
    // mirror of linux's `block_on_e2e` and cannot deadlock the dispatcher. Do NOT
    // move these into a `RunOnUiThread` postAction.

    /// <summary>
    /// Ensure/report that the real wire-backed FaunaMls manager is live. On windows
    /// the real manager is registered unconditionally at login under
    /// <c>FAUNA_E2E_REAL_CONVERSATIONS</c> (<c>App.StartE2eRealConversationsAsync</c>),
    /// so — exactly like linux's <c>request_e2e_activation</c>, a no-op probe over
    /// the already-wired session — there is nothing to install here. The Python
    /// action ignores the command result and polls
    /// <c>data.conv_real_backend_active</c> instead.
    /// </summary>
    public static void EnableRealFaunaMls(Dictionary<string, object?> command)
    {
        _ = command;
        // Intentionally empty: readiness is observed via
        // ConversationsManagerHost.IsRealManagerRegistered.
    }

    /// <summary>
    /// Start a new conversation, resolve <c>recipient</c> to a chip, and send
    /// <c>body</c> over the REAL FaunaMls rail. Mirrors linux
    /// <c>e2e_resolve_send_new</c> step for step; payload <c>{recipient, body}</c>.
    /// Throws when the recipient does not resolve, so the TestAgent surfaces a
    /// harness fault rather than letting the test read it as an empty thread.
    /// </summary>
    public static void RealResolveSendNew(Dictionary<string, object?> command)
    {
        var recipient = GetString(command, "recipient") ?? "";
        var body = GetString(command, "body") ?? "";
        var m = ConversationsManagerHost.Instance;

        m.StartNewConversation();
        m.SetNewThreadRecipientInput(recipient);
        m.ResolveRecipient().GetAwaiter().GetResult();
        if (!m.AcceptCurrentRecipientChip())
        {
            throw new InvalidOperationException(
                $"recipient '{recipient}' did not resolve to a chip (not a reachable Fauna actor?)");
        }
        m.SetNewThreadBody(body);
        // `send_new_thread` returns the new ThreadId (a `string` alias in the C#
        // bindings); the action layer re-reads the thread list, so we drop it.
        m.SendNewThread().GetAwaiter().GetResult();
    }

    /// <summary>
    /// Send <c>body</c> into the existing thread <c>thread_id</c> over the REAL
    /// FaunaMls rail. Mirrors linux <c>e2e_send</c>; payload <c>{thread_id, body}</c>.
    /// </summary>
    public static void RealSend(Dictionary<string, object?> command)
    {
        var threadId = RequireThreadId(command);
        var body = GetString(command, "body") ?? "";
        var m = ConversationsManagerHost.Instance;

        m.SetComposeBody(threadId, body);
        m.Send(threadId).GetAwaiter().GetResult();
    }

    /// <summary>
    /// Add the peer (<c>peer_actor_id_hex</c> injected, <c>peer_handle</c> for
    /// display/chip-matching) to <c>thread_id</c> over the REAL FaunaMls rail —
    /// on a bound group this posts the MLS Commit + Welcome; on a 1:1 it forks a
    /// fresh group (snapshot-only until its first <see cref="RealSend"/>). Mirrors
    /// linux <c>e2e_add</c> step for step; payload
    /// <c>{thread_id, peer_actor_id_hex, peer_handle}</c>.
    /// </summary>
    public static void RealAdd(Dictionary<string, object?> command)
    {
        var threadId = RequireThreadId(command);
        var actorIdHex = GetString(command, "peer_actor_id_hex") ?? "";
        var peerHandle = GetString(command, "peer_handle") ?? "";
        var m = ConversationsManagerHost.Instance;

        m.OpenAddParticipant(threadId);
        m.SetAddParticipantRecipientInput(peerHandle);
        m.AcceptAddParticipantChip(new TypedAddress.Fauna(@handle: peerHandle, @actorId: Convert.FromHexString(actorIdHex)));
        m.ConfirmAddParticipant().GetAwaiter().GetResult();
        ThrowOnPageError(m.PageErrorDiagnostic());
    }

    /// <summary>
    /// Remove the peer (<c>peer_actor_id_hex</c> injected; <c>peer_handle</c> must
    /// match the one used at <see cref="RealAdd"/> so the snapshot removal — which
    /// keys on <c>TypedAddress</c> display — drops the right chip) from the bound
    /// FaunaMls group <c>thread_id</c> (posts the MLS Commit; no Welcome). Mirrors
    /// linux <c>e2e_remove</c>; payload
    /// <c>{thread_id, peer_actor_id_hex, peer_handle}</c>.
    /// </summary>
    public static void RealRemove(Dictionary<string, object?> command)
    {
        var threadId = RequireThreadId(command);
        var actorIdHex = GetString(command, "peer_actor_id_hex") ?? "";
        var peerHandle = GetString(command, "peer_handle") ?? "";
        var m = ConversationsManagerHost.Instance;

        var addr = new TypedAddress.Fauna(@handle: peerHandle, @actorId: Convert.FromHexString(actorIdHex));
        m.RemoveParticipant(threadId, addr).GetAwaiter().GetResult();
        ThrowOnPageError(m.PageErrorDiagnostic());
    }

    /// <summary>
    /// Rename the bound FaunaMls group <c>thread_id</c> (posts the encrypted
    /// <c>GroupMeta::NameChanged</c> Application envelope). Mirrors linux
    /// <c>e2e_rename</c>; payload <c>{thread_id, label}</c>.
    /// </summary>
    public static void RealRename(Dictionary<string, object?> command)
    {
        var threadId = RequireThreadId(command);
        var label = GetString(command, "label") ?? "";
        var m = ConversationsManagerHost.Instance;

        m.RenameThread(threadId, label).GetAwaiter().GetResult();
        ThrowOnPageError(m.PageErrorDiagnostic());
    }

    // ── helpers ───────────────────────────────────────────────────

    /// <summary>
    /// Turn a page error the gesture just stamped into the exception the TestAgent
    /// reports. <c>ConfirmAddParticipant</c>, <c>RemoveParticipant</c> and
    /// <c>RenameThread</c> are UI gestures: they signal a failed wire op ONLY through
    /// the manager's page-error slot and return nothing a caller can check. Each
    /// clears that slot on entry, so what it holds now is this gesture's own
    /// outcome. Without this read-back the agent acks success for an op the nest
    /// refused, which is <c>e2e-conventions.md</c> § convention 11's swallow. Twin of
    /// linux's and tui's <c>conv_backend.rs::page_error</c>.
    /// </summary>
    private static void ThrowOnPageError(string? diagnostic)
    {
        if (diagnostic is not null)
        {
            throw new InvalidOperationException(diagnostic);
        }
    }

    /// <summary>
    /// The payload's <c>thread_id</c>, or a throw naming its absence. A missing id
    /// used to be a bare <c>return</c>, which acked green for a command that did
    /// nothing: the bad-payload corner of convention 11.
    /// </summary>
    private static string RequireThreadId(Dictionary<string, object?> command)
    {
        var threadId = GetString(command, "thread_id");
        if (string.IsNullOrEmpty(threadId))
        {
            throw new ArgumentException("the payload carries no `thread_id`");
        }
        return threadId;
    }

    private static string? GetString(Dictionary<string, object?> d, string key)
    {
        if (!d.TryGetValue(key, out var v) || v is null) return null;
        return v switch
        {
            JsonElement je when je.ValueKind == JsonValueKind.String => je.GetString(),
            JsonElement je when je.ValueKind == JsonValueKind.Null => null,
            JsonElement je => je.GetRawText(),
            string s => s,
            _ => v.ToString(),
        };
    }

    /// <summary>
    /// Produce a <see cref="TypedAddress"/> from a raw string, picking the
    /// variant that matches the <paramref name="rail"/>. The bridge always
    /// names the rail explicitly, so we don't need to probe — this just
    /// maps "alice@host" → Email when rail is Smtp, etc. A bridged address
    /// names its bridge beside the far spelling, which a bare string cannot,
    /// so <see cref="Rail.Bridged"/> refuses (the shared parser's
    /// <c>bridge_id</c> key carries it).
    /// </summary>
    private static TypedAddress AddressForRail(Rail rail, string raw) => rail switch
    {
        Rail.FaunaMls => new TypedAddress.Fauna(@handle: raw, @actorId: ZeroActor),
        Rail.Smtp => new TypedAddress.Email(@emailAddress: raw),
        _ => throw new ArgumentException($"a {rail} address cannot be built from a bare string", nameof(rail)),
    };

    private static readonly byte[] ZeroActor = new byte[32];
}
#endif
