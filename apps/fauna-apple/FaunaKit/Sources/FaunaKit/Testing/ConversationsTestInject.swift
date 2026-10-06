import Foundation

// Compiled out of release artifacts (testing.md § convention 15): the inject
// and evict arms below call `test-helpers` UniFFI seams
// (`injectInboundFromTestJson`, `evictThreadAttachmentsForTest`) the production
// FFI flavor does not export. Every caller — both app shells' TestAgent arms
// and the FaunaKit tests — is already debug-only, so the gate costs nothing and
// matches the rest of `FaunaKit/Testing/`.
#if DEBUG

/// The refusal sentence `conversations_accept_recipient` reports when the accept
/// commits no chip — apple's hand-carried copy of shared Rust's
/// `fauna_conversations::manager::ACCEPT_RECIPIENT_NO_CHIP_REASON`.
///
/// Carried by hand rather than exported over UniFFI **on that constant's own
/// instruction**: "Rust consumers (tui, linux) use this constant directly; web's
/// TypeScript arm and the still-owed windows/apple arms carry the same sentence
/// by hand, since no FFI export is warranted for a debug-only string." web's
/// TypeScript arm carries it the same way. One copy here, shared by both apple
/// targets, so macOS and iOS cannot drift from each other either.
///
/// ⚠ It must stay byte-identical to the Rust constant: the cross-app pin
/// `tests/e2e-unified/tests/test_agent_refuses_declining_arm.py` asserts text
/// every app agrees on, so a re-wording on one side unassertable-ifies it.
public let acceptRecipientNoChipReason =
    "nothing committed — the active picker had no resolvable recipient "
    + "(empty input, or an address that resolved to no chip)"

public enum ConversationsTestInject {
    /// `conversations_inject_inbound` — seed a synthetic inbound message
    /// directly into the shared `ConversationsManager`, bypassing the real
    /// MLS/SMTP receive path so an e2e test can drive the thread render
    /// without a live cross-member send. One implementation for both app
    /// shells (macOS's `FaunaMacApp`, iOS's `FaunaApp`).
    ///
    /// The payload goes to the shared parser WHOLE
    /// (`ConversationsManager::inject_inbound_from_test_json`) — the one seam
    /// tui, linux and web inject through — so `recipients`, the mail rail's
    /// real-self resolution, attachments, labels, `is_own` and
    /// `force_subject_change` behave the same on every app, and a key added to
    /// the payload reaches apple with no Swift change. This file used to parse
    /// the payload key by key and silently dropped `recipients`
    /// (`conversations.md` § Participants vs. reply recipients).
    ///
    /// Failures are reported loudly via `AppMessages.reportRefusedAgentCommand`,
    /// never `.debug`: `POST /app/commands` acks 200 before the handler runs,
    /// so the app's own error surface is the only channel the harness can
    /// observe. The parser throws `NotSupported` when the rail has no
    /// registered backend — a swallowed catch here turned into a phantom "the
    /// thread never rendered".
    @MainActor
    public static func injectInbound(
        _ command: [String: Any],
        into manager: ConversationsManager
    ) {
        do {
            let json = try JSONSerialization.data(withJSONObject: command)
            try manager.injectInboundFromTestJson(
                payloadJson: String(decoding: json, as: UTF8.self))
        } catch {
            AppMessages.reportRefusedAgentCommand("conversations_inject_inbound failed: \(String(describing: error))")
        }
    }

    /// `conversations_evict_attachment` — drop this device's cached bytes of
    /// every attachment named `filename` in `thread_id`, exactly as the store's
    /// budget eviction drops one entry (the shared
    /// `evict_thread_attachments_for_test`, which redraws). Lets an e2e reach
    /// the re-fetch of an evicted attachment and the declared placeholder of one
    /// with nowhere to be fetched from, without filling the 128 MiB store
    /// (`conversations.md` § Attachments → *Retention*). Evicting nothing is a
    /// FAILED command, never an ack (convention 11): a render asserted after a
    /// no-op evict witnesses nothing. The linux twin is
    /// `handle_conversations_evict_attachment`, tui's its `automation.rs` arm.
    @MainActor
    public static func evictAttachment(
        _ command: [String: Any],
        in manager: ConversationsManager
    ) {
        let threadId = command["thread_id"] as? String ?? ""
        let filename = command["filename"] as? String ?? ""
        let evicted = manager.evictThreadAttachmentsForTest(
            id: threadId, filename: filename)
        if evicted == 0 {
            AppMessages.reportRefusedAgentCommand(
                "conversations_evict_attachment: no resident attachment named "
                + "\(filename.debugDescription) in thread \(threadId.debugDescription)")
        }
    }

    /// `conversations_seed_resolved_link_preview` — record a **pre-resolved** D4
    /// link preview for `url`, so a bubble whose body is the standalone
    /// `[url](url)` paragraph folds its `RenderBlock::LinkPreview` block
    /// `Resolved` and paints the `link-preview-card` (`DmMessageBubble` builds
    /// the same `LinkPreviewCard` leaf the feed card does). Flat payload:
    /// `{url, title?, description?, image_hash?}`.
    ///
    /// Why a seam and not the real resolve: `fauna.linkpreview.resolve` needs the
    /// nest to fetch a live OpenGraph page (SSRF-guarded — `render-model.md` § D4),
    /// so the cross-app e2e seeds the terminal state deterministically. The
    /// conversations twin of the feed's `feed_inject_posts` `link_preview` spec,
    /// and the apple twin of linux's
    /// `handle_conversations_seed_resolved_link_preview` / tui's
    /// `conversations::seed_resolved_link_preview` — all four drive the ONE shared
    /// seam `ConversationsManager::seed_resolved_link_preview_for_test`.
    ///
    /// `revealed: false` at seed (the manager's own posture), so the card's
    /// og:image stays blocked until the message's `load-remote-content-button` is
    /// tapped — precisely the transition the e2e asserts.
    ///
    /// Honours the command or fails loudly (`testing.md` point 11): a missing or
    /// empty `url` is refused through `AppMessages.reportRefusedAgentCommand`,
    /// never silently dropped — `POST /app/commands` acks 200 before the handler
    /// runs, so the app's own error surface is the only channel the harness can
    /// observe. An empty `image_hash` string means *no og:image*, not a
    /// zero-length hash — the same normalisation linux and tui apply, so
    /// `image_hash: null` and `""` agree across all four apps.
    ///
    /// One implementation for macOS + iOS, like `injectInbound` above.
    @MainActor
    public static func seedResolvedLinkPreview(
        _ command: [String: Any],
        into manager: ConversationsManager
    ) {
        guard let url = command["url"] as? String, !url.isEmpty else {
            AppMessages.reportRefusedAgentCommand(
                "conversations_seed_resolved_link_preview: missing `url`")
            return
        }
        manager.seedResolvedLinkPreviewForTest(
            url: url,
            title: command["title"] as? String ?? "",
            description: command["description"] as? String ?? "",
            imageHash: (command["image_hash"] as? String).flatMap { $0.isEmpty ? nil : $0 }
        )
    }
}

#endif
