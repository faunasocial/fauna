import SwiftUI

/// The contents of an open thread — `thread-header` strip, scrollable bubble
/// stream with `subject-divider`s interleaved, an `error-message` banner (when a
/// send failed), and the inline `dm-compose-bar`. Renders entirely off the
/// `ThreadDetail` snapshot; every mutation goes back through `ConversationsVM` —
/// except mark-as-spam, which is orthogonal to conversation state (a
/// `MailSettingsMachine` write, not a `ConversationsManager` one) and dispatches
/// straight to `client.api.markMessageAsSpam`. Hosts the rename, add-participant
/// and room-settings `.sheet` overlays. Shared by the macOS + iOS conversations views — macOS
/// embeds it in the detail pane of the `NavigationSplitView`, iOS in a pushed
/// `NavigationStack` screen; the body is identical.
public struct ThreadDetailView: View {
    public let detail: ThreadDetail
    /// Whether this view carries the PAGE-level error (`ConversationsVM.pageError`
    /// — a refused add-participant, a failed rename) as well as the send error.
    /// Only a shell whose list pane is off screen while a thread is open passes
    /// `true`: iOS's pushed screen, where the list's own banner is hidden under
    /// the push and a refusal stamped while the thread is open would otherwise
    /// reach no visible `error-message` at all. macOS keeps the list pane and its
    /// banner beside the thread, so it passes `false` — two banners would be two
    /// `error-message` elements on one page.
    public let showsPageError: Bool

    public init(detail: ThreadDetail, showsPageError: Bool = false) {
        self.detail = detail
        self.showsPageError = showsPageError
    }

    @Environment(ConversationsVM.self) private var vm
    @Environment(FaunaClient.self) private var client: FaunaClient?
    /// The app-scoped content-policy cache (family-safety.md § Content policy),
    /// injected at each shell's app root. One cache serves the feed and this
    /// surface, so the two can never drift on how a floor is enforced. Optional
    /// so previews and view tests render without it (⇒ enforce nothing).
    @Environment(ContentPolicyStore.self) private var contentPolicy: ContentPolicyStore?
    @State private var renameRequest: RenameRequest?
    /// The open room policy editor, seeded at the moment its door was pressed
    /// (`roomSettingsSeed` — `nil` on a policy-less room or a non-room, where the
    /// door is greyed anyway, so nothing opens).
    @State private var roomSettingsRequest: RoomSettingsRequest?
    /// The viewer's muted-keyword list (moderation.md § Muted keywords), fetched
    /// fresh whenever a DIFFERENT thread opens (`.task(id: detail.threadId)`) —
    /// mirrors web's reload-on-enter (linux instead keeps a persistent
    /// process-wide cache updated at mute time; either shape satisfies the
    /// shared contract). Threaded down to every bubble so the collapse check
    /// stays a dumb render, not a per-bubble fetch.
    @State private var mutedKeywords: [MutedKeyword] = []

    private struct RenameRequest: Identifiable { let id: ThreadId; let current: String }
    private struct RoomSettingsRequest: Identifiable {
        let id: ThreadId
        let detail: ThreadDetail
        let seed: RoomSettingsDraft
    }

    private func openRoomSettings() {
        guard let seed = roomSettingsSeed(detail: detail) else { return }
        roomSettingsRequest = RoomSettingsRequest(id: detail.threadId, detail: detail, seed: seed)
    }

    /// `error-message` text — the manager's send failure, falling back to a
    /// client-glue error the snapshot cannot carry (a failed attach read).
    /// Mirrors `FeedVM.errorMessage`'s snapshot-then-client-glue precedence.
    private var sendError: String {
        composeSendOrClientError(detail.compose.sendState, clientError: vm.clientErrorMessage)
    }

    /// The one `error-message` this view paints: the send error, else — only
    /// where `showsPageError` — the page's own error. One banner, never two.
    private var bannerError: String {
        if !sendError.isEmpty || !showsPageError { return sendError }
        return vm.pageError
    }

    /// `dm-reply-preview` — the shared `reply_preview` record rendered as
    /// "{sender}: {excerpt}", the text tui, linux and web paint. Apps render the
    /// record, never derive it (`conversations.md` § Where logic lives → *Reply
    /// preview*): `nil` when no reply is armed or the answered message is outside
    /// the fetched window. Re-read on every render the snapshot drives, since
    /// `detail.compose.replyTo` changing is what re-renders this view.
    private var replyPreviewText: String? {
        guard detail.compose.replyTo != nil,
              let preview = vm.replyPreview(detail.threadId) else { return nil }
        return "\(preview.senderDisplay): \(preview.excerpt)"
    }

    /// The post-succession review marks for this thread's chips
    /// (`thread-member-unattested-mark`/`-keep-button`,
    /// `succession-aftermath.md` § Propagation), off the app-wide CACHED
    /// roster (`client.memberReviewRoster` — never a fresh read per repaint,
    /// a thread header paints far more often than the ledger changes). `[]`
    /// before `client` is up, which paints no marks — the honest state before
    /// the roster has ever loaded.
    private var memberReviewMarks: [Data?] {
        guard let client else { return [] }
        return (try? client.api.memberReviewMarksForThread(
            manager: vm.manager, threadId: detail.threadId, roster: client.memberReviewRoster))
            ?? []
    }

    /// `thread-member-keep-button` — orthogonal to conversation state, so it
    /// dispatches straight to `client.api`, the same shape `onMarkAsSpam`
    /// above uses for its own orthogonal write. Re-reads the shared roster
    /// after, so every other painted mark (this thread's other chips, the
    /// contacts badge) clears in step.
    private func keepMemberReview(person: Data) {
        guard let client else { return }
        Task {
            _ = try? await client.api.memberReviewKeep(person: person)
            await client.reloadMemberReviewRoster()
        }
    }

    public var body: some View {
        VStack(spacing: 0) {
            ThreadHeader(
                detail: detail,
                marks: memberReviewMarks,
                onAddParticipant: { vm.openAddParticipant(detail.threadId) },
                onRename: { renameRequest = RenameRequest(id: detail.threadId, current: detail.label) },
                onRoomSettings: { openRoomSettings() },
                onKeepMember: { keepMemberReview(person: $0) },
                // `thread-member-chip[i]` → `manager.remove_participant` on a
                // membership-change-capable thread. Async (a FaunaMls group
                // posts an MLS Commit), driven off the same `vm` seam every
                // other header mutation uses; a failure surfaces on
                // `error-message` through the manager snapshot, which is where
                // `conversations.md` § the element table puts it.
                onRemoveMember: { addr in
                    Task { await vm.removeParticipant(detail.threadId, addr) }
                }
            )

            // `ScrollViewReader` is what makes the selected-message deep link worth
            // having (`conversations.md` § The selected message) — the mark alone
            // is silent if the matched row is scrolled off-screen. `LazyVStack`
            // supports `scrollTo` for a not-yet-realized row (Apple's documented
            // use case), so no artificial delay is needed.
            ScrollViewReader { scrollProxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 8) {
                        ForEach(detail.messages, id: \.messageId) { msg in
                            if let subject = msg.subjectLine {
                                SubjectDivider(subject: subject)
                            }
                            DmMessageBubble(
                                message: msg,
                                capabilities: detail.capabilities,
                                mutedKeywords: mutedKeywords,
                                contentPolicy: contentPolicy?.inputs ?? ContentPolicyInputs(),
                                selectedMessageId: detail.selectedMessageId,
                                onReply: { vm.startReply(detail.threadId, $0, replyAll: $1) },
                                loadAttachment: { vm.attachmentBytes($0) },
                                attachmentResidency: vm.attachmentResidency(msg),
                                onRevealRemoteImages: { vm.revealRemoteImages($0) },
                                onToggleReaction: { mid, emoji in
                                    Task { await vm.toggleReaction(detail.threadId, mid, emoji) }
                                },
                                onDeleteMessage: { mid in
                                    Task { await vm.deleteMessage(detail.threadId, mid) }
                                },
                                onMarkAsSpam: { mid in
                                    guard let client else { return }
                                    Task {
                                        await client.api.markMessageAsSpam(
                                            messageId: mid, body: msg.body, subject: msg.subjectLine)
                                    }
                                },
                                linkPreviewImageURL: { vm.linkPreviewImageURL($0) },
                                onResolveLinkPreview: { url in Task { await vm.resolveLinkPreview(url) } }
                            )
                            // Explicit `.id` for `scrollTo` below, which addresses views by
                            // their `.id()` modifier — separate from the ForEach's own
                            // `id: \.messageId` key, which only drives SwiftUI's row identity.
                            .id(msg.messageId)
                        }
                    }
                    .padding(12)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                // Fires on first appearance AND whenever the selection changes (a
                // fresh mail-search hit while the thread is already open) — the
                // `.task(id:)` shape used throughout this codebase for exactly
                // that "on-appear-and-on-change" pairing.
                .task(id: detail.selectedMessageId) {
                    guard let target = detail.selectedMessageId else { return }
                    withAnimation {
                        scrollProxy.scrollTo(target, anchor: .center)
                    }
                }
            }

            if !bannerError.isEmpty {
                ErrorBanner(message: bannerError)
                    .padding(.horizontal, 12)
            }

            Divider()

            DmComposeBar(
                compose: detail.compose,
                capabilities: detail.capabilities,
                replyPreviewText: replyPreviewText,
                onBodyChange: { vm.setComposeBody(detail.threadId, $0) },
                onSubjectChange: { vm.setComposeSubject(detail.threadId, $0) },
                onToggleTopic: { vm.toggleTopic(detail.threadId) },
                onCancelReply: { vm.clearReplyTo(detail.threadId) },
                onAddReplyRecipient: { vm.addReplyRecipient(detail.threadId, $0) },
                onRemoveReplyRecipient: { vm.removeReplyRecipient(detail.threadId, $0) },
                onAttachFile: { try vm.attachFile(detail.threadId, at: $0) },
                onAttachError: { vm.clientErrorMessage = $0 },
                onRemoveAttachment: { vm.removeAttachment(detail.threadId, at: $0) },
                onSend: { Task { await vm.send(detail.threadId) } }
            )
            .padding(12)
        }
        .sheet(item: $renameRequest) { req in
            RenameSheet(threadId: req.id, current: req.current)
        }
        // The room policy editor — `room_settings`. The sheet is bound to state
        // this view owns, and only the editor's own verdict clears it: `onClose`
        // fires once every staged commit landed (`RoomSettingsSheet.save`), or
        // on Cancel; a refusal keeps it open.
        .sheet(item: $roomSettingsRequest) { req in
            RoomSettingsSheet(detail: req.detail, seed: req.seed, onClose: { roomSettingsRequest = nil })
        }
        .sheet(isPresented: addParticipantPresented) {
            if let ap = vm.addParticipant { AddParticipantSheet(state: ap) }
        }
        .task(id: detail.threadId) {
            guard let client else { return }
            mutedKeywords = (try? await client.api.mutedKeywordsList())?.keywords ?? []
        }
    }

    private var addParticipantPresented: Binding<Bool> {
        Binding(
            get: { vm.addParticipant != nil },
            set: { if !$0 { vm.cancelAddParticipant() } }
        )
    }
}
