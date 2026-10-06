import SwiftUI

/// Per-message cache for `dm-message-text`'s document-derived plaintext read
/// (render-model.md § D1; `message.rs:74-86`'s "unified by swapping apple's
/// `automationValue` closures to a document-derived plaintext" contract — the
/// read must stay `RenderDocument::to_plaintext`, never the raw `body`).
/// Without this, a large body's cross-UniFFI-boundary render repeats on
/// EVERY automation touch (`dm-message-text`'s closure runs once per read,
/// not once per message) — one driver of the full-app hang diagnosed in
/// `mail-message-size.md` § Implementation status today. Safe to key on
/// `messageId` alone: a message's document text never changes post-creation
/// (no edit feature; revealing a remote image only flips a block's
/// `revealed` bit, which `to_plaintext`'s block walk doesn't read).
/// `NSCache` is thread-safe and evicts under memory pressure for free.
private let dmMessagePlaintextCache = NSCache<NSString, NSString>()

private func cachedDocumentPlaintext(for message: MessageSnapshot) -> String {
    let key = message.messageId as NSString
    if let cached = dmMessagePlaintextCache.object(forKey: key) {
        return cached as String
    }
    let text = renderDocumentToPlaintext(document: message.document)
    dmMessagePlaintextCache.setObject(text as NSString, forKey: key)
    return text
}

/// One message bubble in a thread's detail view. Single visual shape across
/// every rail (spec §"Bubble layout"); the only capability-derived bits are the
/// per-message reply button's `disabled` state and the per-bubble ⋯ overflow's
/// reaction/delete affordances — never a `rail` branch. Shared by the macOS + iOS
/// conversations detail views.
///
/// Element IDs (ui.yaml `dm-message-bubble` + page): `dm-sender` (indexed),
/// `encrypted-badge`, `signed-badge`, `verified-badge`,
/// `content-label-badge`, `dm-message-text` (indexed), `dm-attachment-image`
/// (indexed), `dm-attachment-file` (indexed), `c2pa-badge` (per attachment,
/// beside the `dm-attachment-image`/`-file` it vouches for — never a
/// whole-message flag; `conversations.md` § Attachments "C2PA on-device"),
/// `dm-reply-button` (indexed),
/// `dm-reply-all-button` (indexed — mail only), `dm-message-actions-button`
/// (indexed) → `dm-message-actions-menu` (`dm-reaction-option` ×6 +
/// `dm-reaction-more-button` + own-only `dm-message-delete-button` →
/// `dm-message-delete-confirm-button` + received-only
/// `dm-message-mark-as-spam-button`), `dm-reaction-pill` (indexed),
/// `dm-message-deleted` (tombstone).
public struct DmMessageBubble: View {
    public let message: MessageSnapshot
    public let capabilities: ThreadCapabilities
    /// The viewer's sealed, client-only muted-keyword list (moderation.md §
    /// Muted keywords; content-moderation-and-ranking.md § Q3) — threaded down
    /// from the thread host (`ThreadDetailView`). A hide/collapse verb, NOT a
    /// moderation-queue flag: matched at render, client-side, post-decrypt.
    /// Empty by default, matching the shared `matches_muted_keywords`
    /// contract ("false for an empty list — no mutes, nothing collapses").
    public var mutedKeywords: [MutedKeyword]
    /// The content-policy render-engine inputs (the guardian floor + the
    /// viewer's own spam/phishing thresholds), threaded down from the thread
    /// host exactly like `mutedKeywords` above. Defaults to "nothing set", which
    /// enforces nothing and never touches the FFI — so previews and the
    /// VM-free view tests keep working unchanged.
    public var contentPolicy: ContentPolicyInputs
    /// The thread's currently-selected message
    /// (`ThreadDetail.selectedMessageId` — `conversations.md` § The selected
    /// message; a mail-search hit's target), threaded down from
    /// `ThreadDetailView`. `nil` in the ordinary (nothing selected) case.
    /// Drives both the bubble tint and the timestamp's `selected` observable
    /// — see `isSelected`/`timestampLabel`.
    public var selectedMessageId: MessageId?
    /// Seed a reply on this message. `replyAll == false` = `dm-reply-button`
    /// (sender-only); `true` = `dm-reply-all-button` (every participant but
    /// self). Both route to `manager.start_reply`.
    public var onReply: (MessageId, Bool) -> Void
    /// Resolve an attachment's `blobHash` to its plaintext bytes — the shared
    /// `ConversationsManager.attachment_bytes(blob_hash)` loader, threaded down
    /// from `ConversationsVM` (conversations.md § Attachments). `nil` for an
    /// unknown / not-yet-fetched hash; the bubble then falls back to an icon so
    /// the `dm-attachment-image` element still announces the attachment.
    public var loadAttachment: (String) -> Data?
    /// Whether each attachment's bytes are resident, in document order
    /// (`ConversationsVM.attachmentResidency`). Read by nothing in the body: it is
    /// the bubble's repaint KEY. Evicting or refetching bytes changes no message, so
    /// without it SwiftUI saw an unchanged bubble and kept a dropped picture painted
    /// (measured on iOS) — a changed value here is what re-runs `loadAttachment`.
    public var attachmentResidency: [Bool]
    /// Reveal this message's blocked remote images — the shared
    /// `ConversationsManager.reveal_remote_images(message_id)` dispatch, threaded down from
    /// `ConversationsVM` (D3 — render-model.md § D3). The manager flips its in-memory reveal set
    /// and re-emits the snapshot with `RemoteImage.revealed = true`; the bubble holds **no** reveal
    /// state of its own and repaints off the fresh `message.document`.
    public var onRevealRemoteImages: (MessageId) -> Void
    /// Toggle `emoji` on this message — the shared
    /// `ConversationsManager.toggle_reaction(thread, msg, emoji)`, threaded down from
    /// `ConversationsVM` (conversations.md § Reactions & message delete). The manager
    /// resolves Add vs Remove against self's current state, optimistically updates the
    /// aggregate, posts a `Reaction` channel message, and re-emits — the bubble holds
    /// no reaction state of its own and repaints off `message.reactions`. Gated on
    /// `capabilities.supportsReactions` (FaunaMls-only).
    public var onToggleReaction: (MessageId, String) -> Void
    /// Delete this (own) message — the sender-only
    /// `ConversationsManager.delete_message(thread, msg)`. Optimistically marks the
    /// target `deleted` (manager rejects a non-own target; the security floor is also
    /// enforced on ingest), and the bubble repaints to the `dm-message-deleted`
    /// tombstone off `message.deleted`. Gated on
    /// `capabilities.supportsMessageDelete && message.isOwn` (FaunaMls-only).
    public var onDeleteMessage: (MessageId) -> Void
    /// Mark this (received) message as spam — the live `Insert` consumer
    /// (mail-spam.md § Encrypted-mode interaction + § Wire shapes `put_spam_model`
    /// `history_op`), threaded down from `ThreadDetailView` to
    /// `APIClient.markMessageAsSpam` (over the shared `MailSettingsMachine`, NOT
    /// the `ConversationsManager` — orthogonal to reactions/delete/reply). Gated
    /// `!message.isOwn` (a received message; you flag others' content, not your
    /// own). Fire-and-forget: a silent no-op when mail isn't enabled / the nest
    /// lacks `spam-model-sealed-at-rest`, exactly like a local moderation
    /// correction — there is no server-train fallback for client-only content.
    public var onMarkAsSpam: (MessageId) -> Void
    /// Resolve a link-preview og:image `image_hash` to its public nest blob URL — threaded down from
    /// `ConversationsVM.linkPreviewImageURL` (D4 — render-model.md § D4). Reveal-gated by the caller
    /// (consulted only when the block's `revealed` is true); `nil` ⇒ the card paints
    /// title/description/domain but no image. The og:image is the user's OWN nest content-addressed
    /// blob, **not** a sealed attachment, so it loads via a URL (`AsyncImage`) — unlike
    /// `loadAttachment`'s sealed in-memory bytes.
    public var linkPreviewImageURL: (String) -> URL?
    /// Fire the one-shot resolve for a still-`Resolving` link-preview url — the shared
    /// `ConversationsManager.resolveLinkPreview` (cached, folds the terminal state onto the message's
    /// document). The bubble fires it once per resolving url on appear; the manager re-emits and the
    /// bubble repaints with the `Resolved` card. The conversations twin of the feed card's
    /// `.task`-driven `vm.resolveLinkPreview`.
    public var onResolveLinkPreview: (String) -> Void

    public init(
        message: MessageSnapshot,
        capabilities: ThreadCapabilities,
        mutedKeywords: [MutedKeyword] = [],
        contentPolicy: ContentPolicyInputs = ContentPolicyInputs(),
        selectedMessageId: MessageId? = nil,
        onReply: @escaping (MessageId, Bool) -> Void = { _, _ in },
        loadAttachment: @escaping (String) -> Data? = { _ in nil },
        attachmentResidency: [Bool] = [],
        onRevealRemoteImages: @escaping (MessageId) -> Void = { _ in },
        onToggleReaction: @escaping (MessageId, String) -> Void = { _, _ in },
        onDeleteMessage: @escaping (MessageId) -> Void = { _ in },
        onMarkAsSpam: @escaping (MessageId) -> Void = { _ in },
        linkPreviewImageURL: @escaping (String) -> URL? = { _ in nil },
        onResolveLinkPreview: @escaping (String) -> Void = { _ in }
    ) {
        self.message = message
        self.capabilities = capabilities
        self.mutedKeywords = mutedKeywords
        self.contentPolicy = contentPolicy
        self.selectedMessageId = selectedMessageId
        self.onReply = onReply
        self.loadAttachment = loadAttachment
        self.attachmentResidency = attachmentResidency
        self.onRevealRemoteImages = onRevealRemoteImages
        self.onToggleReaction = onToggleReaction
        self.onDeleteMessage = onDeleteMessage
        self.onMarkAsSpam = onMarkAsSpam
        self.linkPreviewImageURL = linkPreviewImageURL
        self.onResolveLinkPreview = onResolveLinkPreview
    }

    // Per-bubble transient menu state. Inline + `@State`-driven (NOT a system
    // `Menu`/`.confirmationDialog`) so every id attaches to a real, in-process-
    // registered element — the same pattern `MailSettingsView`'s disable-confirm
    // overlay uses, and the uniform cross-app shape the windows/linux reference
    // legs render (priority #1; a system menu's items don't `.onAppear`-register in
    // the in-process driver). Each is scoped to one bubble's view identity.
    @State private var showActions = false
    @State private var confirmDelete = false
    @State private var showMorePicker = false
    @State private var moreEmoji = ""
    /// Session-local reveal for THIS message instance only — the mute stays;
    /// revealing just un-collapses this one bubble for the rest of the
    /// session (never persisted, never written back). Resets naturally on a
    /// fresh app launch since the bubble's `@State` doesn't survive it.
    @State private var mutedRevealed = false
    /// Session-local reveal for a content-policy **collapse** on THIS bubble —
    /// the same shape as `mutedRevealed`, and likewise never persisted. A
    /// `block` has no reveal at all, so this can only ever un-collapse.
    @State private var contentRevealed = false

    /// The device's region content plane (`region-blocking.md`), injected at
    /// the app root beside `ContentPolicyStore`; `nil` in a preview.
    @Environment(RegionStore.self) private var region: RegionStore?

    /// This message's render decision (family-safety.md § Content policy;
    /// region-blocking.md § Where it composes) — the strictest-wins compose of
    /// the region content policy, the guardian floor and the viewer's own
    /// thresholds, resolved entirely in shared Rust, post-decrypt. Also records
    /// this message's Guardian Notify enforcement — see
    /// `ContentPolicyInputs.recordedRender`'s doc for the full rationale.
    private var decision: RegionRenderDecision {
        contentPolicy.recordedRender(
            itemId: message.messageId, labels: message.labels, region: region,
            subject: .message(id: message.messageId, text: message.body))
    }

    private var witnessKey: String { "conversation:\(message.messageId)" }

    /// The fixed shared quick-set order (conversations.md § Reactions & message
    /// delete): 👍 ❤️ 😂 😮 😢 🙏 — byte-identical on every app; `dm-reaction-option[i]`
    /// indexes into it, so the ORDER is part of the cross-app contract, not just the
    /// membership.
    ///
    /// Read from shared Rust (`fauna_conversations::quickset_emojis`, the FFI face of
    /// the `QUICKSET_EMOJIS` const linux and tui use directly) rather than re-typed
    /// here — priority #2/#4. The hand-kept Swift copy this replaces was one of four
    /// (android/windows/web hold the others), and nothing would have caught them
    /// drifting apart.
    private static let quickSetEmojis = quicksetEmojis()

    /// Capability + ownership gates. `canReact`/`canDelete` derive ONLY from the
    /// thread capabilities + `message.isOwn` (never a `rail` branch — § Architectural
    /// rules); `hasActions` decides whether the ⋯ overflow shows at all (keeps mail
    /// bubbles clean when neither action is available).
    private var canReact: Bool { capabilities.supportsReactions }
    private var canDelete: Bool { capabilities.supportsMessageDelete && message.isOwn }
    /// Mark-as-spam is offered on any received message (`!isOwn`) — you flag
    /// others' content as spam, not your own; unlike `canReact`/`canDelete` this
    /// is NOT capability-gated (mail-spam.md § Encrypted-mode interaction; mirrors
    /// linux's `can_flag_spam = !msg.is_own`).
    private var canMarkSpam: Bool { !message.isOwn }
    private var hasActions: Bool { canReact || canDelete || canMarkSpam }

    private var senderName: String {
        message.senderDisplay.isEmpty ? ConversationsUI.display(message.sender) : message.senderDisplay
    }

    /// Whether THIS message is the thread's currently-selected one
    /// (`conversations.md` § The selected message) — a mail-search hit's
    /// target.
    private var isSelected: Bool { selectedMessageId != nil && selectedMessageId == message.messageId }

    /// The shared `dm-message-timestamp` — the ONE bubble child painted on
    /// EVERY render arm (deleted, legal-takedown, content-blocked,
    /// content-collapsed, muted, and normal alike), which is why the
    /// `selected` observable rides it rather than a new element (mirrors
    /// tui's `message_timestamp_element` / linux's `message_timestamp_label`
    /// — a mail-search hit can point at a since-deleted/muted/blocked
    /// message, exactly the case most needing the mark). `selected` is
    /// ALWAYS present (`"true"`/`"false"`), never omitted when false, so a
    /// test can tell "not selected" from "no signal at all".
    ///
    /// Apple's `/element/attr` has no per-attribute-name map (unlike
    /// tui/linux's `.attr("selected", …)`) — it falls through to
    /// `value ?? text` for any unrecognized attr name
    /// (`InProcessAutomationServer.swift`'s `/element/attr` handler), so the
    /// `selected` read rides `automationValue`'s `value` closure while `text`
    /// keeps answering the rendered clock (mirrors `RecipientPicker`'s
    /// `recipient-resolve-status` state-on-an-attribute precedent).
    private var timestampLabel: some View {
        Text(ValueFormat.conversationTimestamp(thenMs: message.timestampMs))
            .font(.caption2)
            .foregroundStyle(.secondary)
            .accessibilityIdentifier(Ids.dmMessageTimestamp)
            .automationValue(
                Ids.dmMessageTimestamp,
                text: { ValueFormat.conversationTimestamp(thenMs: message.timestampMs) },
                value: { isSelected ? "true" : "false" })
    }

    /// True iff this decrypted message's body matches the viewer's muted-keyword
    /// list — the shared `matches_muted_keywords` free fn (case-insensitive
    /// substring, OR across terms; `false` for an empty list). Checks
    /// `mutedKeywords.isEmpty` FIRST (the overwhelmingly common case — most
    /// viewers set none) and reads the raw retained `message.body`, matching
    /// web/windows/android (`+page.svelte`'s `mutedKeywords.length === 0`
    /// guard; `ConversationsPage.xaml.cs` / `ConversationDetailScreen.kt` do
    /// the same) — NOT `message.document`, which is a `RenderDocument` value
    /// only cheap for `dm-message-text`'s own read below because that read is
    /// cached. Evaluating `body` was previously an eagerly-evaluated function
    /// argument (Swift has no argument laziness) calling
    /// `renderDocumentToPlaintext` — a full UniFFI-boundary document render —
    /// on EVERY SwiftUI body pass of this view regardless of whether any
    /// keyword was even set; for a large (~multi-MiB) mail body this froze the
    /// whole app (`mail-message-size.md` § Implementation status today).
    private var isMutedMatch: Bool {
        guard !mutedKeywords.isEmpty else { return false }
        return matchesMutedKeywords(body: message.body, mutedKeywords: mutedKeywords)
    }

    public var body: some View {
        let decision = decision
        let contentVerdict = decision.verdict
        VStack(alignment: .leading, spacing: 4) {
            if message.deleted {
                deletedPlaceholder
            } else if let reference = message.legalTakedownRef {
                legalTakedownPlaceholder(reference)
            } else if let withheld = decision.withheld(revealed: contentRevealed) {
                // The region arm, AHEAD of the family arm (same verb, better
                // attributed — region-blocking.md § The blocked render); its
                // collapse reveal shares the family reveal state.
                RegionPlaceholderView(placeholder: withheld, witnessKey: witnessKey) {
                    contentRevealed = true
                }
                timestampLabel
            } else if contentVerdict == "block" {
                // AHEAD of the muted arm on purpose (the linux/web/android
                // ordering): a guardian `block` must never be reachable through
                // the muted-keyword reveal.
                ContentPolicyBlockedNotice()
                timestampLabel
            } else if contentVerdict == "collapse" && !contentRevealed {
                ContentPolicyCollapsedPlaceholder { contentRevealed = true }
                timestampLabel
            } else if isMutedMatch && !mutedRevealed {
                mutedPlaceholder
            } else {
                messageContent
            }
        }
        .padding(8)
        .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
        // The selected-message mark (`conversations.md` § The selected
        // message) — a background/border tint, the GUI counterpart of
        // linux's `.message-selected { box-shadow: inset 0 0 0 2px #f59e0b }`
        // / android's amber ring (cross-app visual consistency; priority #1).
        .overlay {
            if isSelected {
                RoundedRectangle(cornerRadius: 8).stroke(Color(hex: "F59E0B"), lineWidth: 2)
            }
        }
        .accessibilityElement(children: .contain)
        // Convention 17's verdict side: a region block on a message with a body
        // to withhold (a tombstone or a takedown replaces the body already).
        .regionBlockWitness(
            witnessKey,
            blocked: !message.deleted && message.legalTakedownRef == nil && decision.isRegionBlocked)
        // Fire-once resolve for each still-`Resolving` link preview in this bubble (render-model.md
        // § D4) → the manager folds `Resolved`/`Failed` onto the block + re-emits → the card paints.
        // Keyed on the resolving-url set so it fires once and settles (the feed card's fire-once
        // pattern). A deleted message carries no document blocks, so this is a no-op there.
        .task(id: resolvingLinkPreviewUrls(message.document)) {
            for url in resolvingLinkPreviewUrls(message.document) {
                onResolveLinkPreview(url)
            }
        }
    }

    /// Tombstone for a deleted message (conversations.md § Reactions & message
    /// delete): the localized placeholder REPLACES body/attachments/reactions/actions
    /// (the bubble collapses to just this), driven off `MessageSnapshot.deleted` —
    /// matching the windows + linux reference legs. `automationText` registers the id
    /// + the painted string so the in-process driver counts/reads it (a bare
    /// `.accessibilityIdentifier` is invisible to the registry).
    private var deletedPlaceholder: some View {
        HStack(spacing: 6) {
            automationText(Ids.dmMessageDeleted, L.conversations.detail.messageDeleted)
                .font(.caption)
                .italic()
                .foregroundStyle(.secondary)
            Spacer(minLength: 4)
            timestampLabel
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// A message taken down under a legal obligation (`moderation.md` § Categories &
    /// enforcement item 1): the nest withholds the sealed envelope and carries only the
    /// reference, so shared Rust hands us a tombstone `MessageSnapshot` with an empty
    /// body and no decrypt. Collapses the bubble like `deletedPlaceholder` — sender,
    /// timestamp, body and actions are all hidden — rather than painting an empty or
    /// failed-decrypt bubble. No dedicated ui.yaml id (presentation inside the existing
    /// bubble scope, like the quoted-post tombstone). Mirrors web / linux / android.
    private func legalTakedownPlaceholder(_ reference: String) -> some View {
        HStack(spacing: 6) {
            Text(renderLocalizedText(legalTakedownTombstone(reference: reference)))
                .font(.caption)
                .italic()
                .foregroundStyle(.secondary)
            Spacer(minLength: 4)
            timestampLabel
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// A decrypted message whose body matches the viewer's muted-keyword list
    /// (moderation.md § Muted keywords; content-moderation-and-ranking.md § Q3)
    /// — collapses the bubble like `deletedPlaceholder`, but with a per-message
    /// reveal that un-collapses just this instance for the rest of the session.
    /// The mute itself is unaffected — revealing doesn't un-mute the term.
    private var mutedPlaceholder: some View {
        HStack(spacing: 8) {
            automationText(Ids.dmMessageMuted, L.conversations.detail.mutedWord)
                .font(.caption)
                .italic()
                .foregroundStyle(.secondary)
            Spacer(minLength: 4)
            timestampLabel
            Button(L.conversations.detail.mutedReveal) {
                mutedRevealed = true
            }
            .buttonStyle(.borderless)
            .font(.caption2)
            .accessibilityIdentifier(Ids.dmMessageMutedRevealButton)
            .automationActivate(Ids.dmMessageMutedRevealButton) {
                mutedRevealed = true
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// The full content of a live (non-deleted) bubble.
    @ViewBuilder private var messageContent: some View {
        let hasBlockedRemote = hasBlockedRemoteImages(message.document)
        let attachmentBlocks = documentAttachments(message.document)
        // Sender + badges row
        HStack(spacing: 6) {
            automationText(Ids.dmSender, senderName)
                .font(.caption.weight(.semibold))
            if message.badges.encrypted {
                badge("lock.fill", "encrypted-badge", L.conversations.compose.encrypted)
            }
            if message.badges.signed {
                badge("signature", "signed-badge", L.conversations.message.signed)
            }
            if message.badges.verified {
                badge("checkmark.seal.fill", "verified-badge", L.common.verified)
            }
            // `message.badges.contentWarning` is a different, unrelated wire
            // field nothing in `libs/fauna-conversations` ever sets — the real
            // classifier data path is `labels: [ContentLabelEntry]`, resolved
            // via the shared `primaryContentLabel` (moderation.md § Per-row
            // badge data path; mirrors linux `message_bubble.rs` / android
            // `ConversationDetailScreen.kt`). `ContentLabelBadge` renders +
            // self-registers `content-label-badge`.
            if let label = primaryContentLabel(labels: message.labels) {
                ContentLabelBadge(category: label.category)
            }
            Spacer(minLength: 4)
            // Per-message contextual timestamp — the shared bucketer
            // `conversation_timestamp_display` (value-formatting.md § Conversation
            // timestamp), plus the `selected` observable — see `timestampLabel`.
            timestampLabel
        }

        // In-bubble reply-quote (D2b — render-model.md § D2 QuotedMessage). The manager folds a
        // `.quotedMessage` block (parent author + ≤2-line snippet) to the FRONT of the document
        // when this message replies AND the parent is loaded in the thread; we paint it as its
        // own card ABOVE the body (the body walker skips the block). Absent / parent-not-loaded
        // → no card, the bubble renders normally.
        if let quote = documentQuotedMessage(message.document) {
            quoteCard(author: quote.author, snippet: quote.snippet)
        }

        // Body — the shared document walk (render-model.md § D1). One model for every
        // format: the manager already chose the markdown/plaintext/html producer when it
        // built `document`, so the bubble no longer branches on `body_format` or re-parses.
        DocumentBodyView(document: message.document)
            .font(.body)
            .accessibilityIdentifier(Ids.dmMessageText)
            // Complex body view — keep the id, register the painted *document*
            // plaintext as the indexed read (the text e2e reads back), uniform
            // with linux/web/android/windows (render-model.md P1 read-uniformity).
            // Cached per messageId — see `cachedDocumentPlaintext` above — so a
            // large body's FFI render doesn't repeat on every automation touch.
            .automationValue(Ids.dmMessageText, text: { cachedDocumentPlaintext(for: message) })

        // Per-message remote-image reveal (html-mail Slice 3 / D3): shown only while this
        // body has ≥1 still-blocked remote image. Tapping dispatches to the manager, which
        // flips its reveal set + re-emits with `revealed = true` (the sole inbound image
        // request follows on the repaint) — no client-side flag.
        if hasBlockedRemote {
            Button {
                onRevealRemoteImages(message.messageId)
            } label: {
                Label(L.conversations.detail.loadRemoteContent, systemImage: "photo")
                    .font(.caption2)
            }
            .buttonStyle(.borderless)
            .accessibilityIdentifier(Ids.loadRemoteContentButton)
            .automationActivate(Ids.loadRemoteContentButton) { onRevealRemoteImages(message.messageId) }
        }

        // Attachments — rendered from the document's `.attachment` blocks (render-model.md
        // § D2a), in body order, replacing the former sibling `message.attachments` loop.
        ForEach(Array(attachmentBlocks.enumerated()), id: \.offset) { _, block in
            attachment(block)
        }

        // Link previews (render-model.md § D4) — the document's folded `LinkPreview` blocks, painted
        // below the body off the text flow (the SAME `LinkPreviewCard` the feed card paints — build
        // once, reuse). One card per Resolved block; Resolving/Failed → no card (the inline body link
        // already shows). The og:image is reveal-gated: `linkPreviewImageURL` is consulted only when
        // `revealed`, so the bubble's `load-remote-content-button` above (now counting the un-revealed
        // og:image via the shared `hasBlockedRemoteImages`) reveals it — no new toggle.
        ForEach(Array(documentResolvedLinkPreviews(message.document).enumerated()), id: \.offset) { index, preview in
            LinkPreviewCard(
                url: preview.url, title: preview.title, description: preview.description,
                imageURL: (preview.revealed ? preview.imageHash : nil).flatMap { linkPreviewImageURL($0) },
                index: index)
        }

        // Aggregated reactions under the bubble (conversations.md § Reactions &
        // message delete): one `dm-reaction-pill` per emoji group (emoji + count),
        // own-reacted highlighted, tap toggles. Present only when the message HAS
        // reactions (ui.yaml `dm-reaction-pill`) — groups only ever populate on a
        // reactions-capable rail, so no extra capability gate is needed here.
        if !message.reactions.isEmpty {
            reactionPills
        }

        // Per-message reply affordances + the ⋯ overflow. Reply is *disabled* (not
        // hidden) per capabilities so the e2e `disabled` attribute reads correctly;
        // reply-all is *hidden* off mail (a "Reply All" with no editable To line is
        // meaningless on a group rail). The ⋯ overflow is *hidden* unless ≥1 action
        // is available. None is rail-branched.
        HStack(spacing: 8) {
            Button {
                onReply(message.messageId, false)
            } label: {
                Label(L.common.reply, systemImage: "arrowshape.turn.up.left")
                    .font(.caption2)
            }
            .buttonStyle(.borderless)
            .disabled(!capabilities.supportsPerMessageReply)
            .accessibilityIdentifier(Ids.dmReplyButton)
            .automationActivate(Ids.dmReplyButton, isEnabled: { capabilities.supportsPerMessageReply }) {
                onReply(message.messageId, false)
            }

            if capabilities.supportsRecipientSelection {
                Button {
                    onReply(message.messageId, true)
                } label: {
                    Label(L.common.replyAll, systemImage: "arrowshape.turn.up.left.2")
                        .font(.caption2)
                }
                .buttonStyle(.borderless)
                .disabled(!capabilities.supportsPerMessageReply)
                .accessibilityIdentifier(Ids.dmReplyAllButton)
                .automationActivate(Ids.dmReplyAllButton, isEnabled: { capabilities.supportsPerMessageReply }) {
                    onReply(message.messageId, true)
                }
            }

            if hasActions {
                Button {
                    showActions.toggle()
                } label: {
                    Label(L.conversations.detail.messageActions, systemImage: "ellipsis")
                        .font(.caption2)
                        .labelStyle(.iconOnly)
                }
                .buttonStyle(.borderless)
                .help(L.conversations.detail.messageActions)
                .accessibilityIdentifier(Ids.dmMessageActionsButton)
                // Indexed per bubble; toggles the inline menu open/closed (the
                // driver's single click on `dm-message-actions-button[i]` opens it).
                .automationActivate(Ids.dmMessageActionsButton) { showActions.toggle() }
            }
        }

        // The inline ⋯ actions menu — mounts (and its children `.onAppear`-register)
        // only while open, so the driver opens the overflow first, then drives the
        // options/delete by id.
        if hasActions, showActions {
            actionsMenu
        }
    }

    /// The under-bubble reaction-pill row. Each `dm-reaction-pill` is an
    /// `automationActivate` (one Entry) carrying both the toggle action and the
    /// `"emoji count"` read the e2e asserts.
    private var reactionPills: some View {
        HStack(spacing: 4) {
            ForEach(Array(message.reactions.enumerated()), id: \.offset) { _, group in
                reactionPill(group)
            }
            Spacer(minLength: 0)
        }
    }

    private func reactionPill(_ group: ReactionGroup) -> some View {
        let label = "\(group.emoji) \(group.count)"
        return Button {
            toggle(group.emoji)
        } label: {
            Text(label).font(.caption2)
        }
        .buttonStyle(.borderless)
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .background(
            group.reactedByMe ? Color.accentColor.opacity(0.25) : Color.secondary.opacity(0.12),
            in: Capsule()
        )
        .accessibilityIdentifier(Ids.dmReactionPill)
        // Indexed pill — activation toggles self's reaction; value reads "emoji count".
        .automationActivate(Ids.dmReactionPill, value: { label }) { toggle(group.emoji) }
    }

    /// The inline ⋯ actions menu (conversations.md § Reactions & message delete): a
    /// quick-set reaction row (6 fixed emojis), a "more" entry into the native emoji
    /// picker, and — for own messages — a delete action with an inline confirm step.
    /// `dm-message-actions-menu` carries a presence read so the driver can confirm it
    /// opened; `.accessibilityElement(children: .contain)` keeps the container id from
    /// clobbering the child ids on the XCUITest/a11y path.
    @ViewBuilder private var actionsMenu: some View {
        VStack(alignment: .leading, spacing: 6) {
            if canReact {
                HStack(spacing: 4) {
                    ForEach(Self.quickSetEmojis, id: \.self) { emoji in
                        Button {
                            toggle(emoji)
                        } label: {
                            Text(emoji).font(.body)
                        }
                        .buttonStyle(.borderless)
                        .accessibilityIdentifier(Ids.dmReactionOption)
                        .automationActivate(Ids.dmReactionOption) { toggle(emoji) }
                    }
                }

                Button {
                    showMorePicker.toggle()
                } label: {
                    Label(L.conversations.detail.moreReactions, systemImage: "face.smiling")
                        .font(.caption)
                }
                .buttonStyle(.borderless)
                .accessibilityIdentifier(Ids.dmReactionMoreButton)
                .automationActivate(
                    Ids.dmReactionMoreButton,
                    enterText: { moreEmoji = $0 },
                    typeRefusal: {
                        showMorePicker
                            ? nil : "the emoji field is not open — click dm-reaction-more-button first"
                    }
                ) { showMorePicker.toggle() }

                // The "more" picker — the ONLY sanctioned per-platform divergence
                // (conversations.md § Rendering / picker glue): a free-entry
                // single-emoji field, which an OS emoji panel (the iOS emoji keyboard,
                // the macOS character palette) fills as readily as typing or pasting.
                // The first grapheme entered toggles that reaction and closes the field.
                // Entry mode: while open, the field is reachable as
                // `dm-reaction-more-button` itself — the button's registration above
                // writes `moreEmoji` (refused while closed), so this `onChange` runs
                // exactly as for a human entry, and the id keeps one index slot.
                if showMorePicker {
                    TextField("🙂", text: $moreEmoji)
                        .frame(width: 64)
                        .textFieldStyle(.roundedBorder)
                        .onChange(of: moreEmoji) { _, newValue in
                            if let pick = newValue.first {
                                toggle(String(pick))
                                moreEmoji = ""
                                showMorePicker = false
                            }
                        }
                }
            }

            if canDelete {
                if canReact { Divider() }
                if confirmDelete {
                    // Inline confirm step — replaces the delete button (the driver
                    // clicks delete, then this `dm-message-delete-confirm-button`).
                    HStack(spacing: 8) {
                        Text(L.conversations.detail.deleteMessageConfirmTitle)
                            .font(.caption)
                        Button(L.conversations.detail.deleteMessageConfirm, role: .destructive) {
                            performDelete()
                        }
                        .buttonStyle(.borderless)
                        .accessibilityIdentifier(Ids.dmMessageDeleteConfirmButton)
                        .automationActivate(Ids.dmMessageDeleteConfirmButton) { performDelete() }
                        Button(L.common.cancel, role: .cancel) { confirmDelete = false }
                            .buttonStyle(.borderless)
                    }
                } else {
                    Button(role: .destructive) {
                        confirmDelete = true
                    } label: {
                        Label(L.conversations.detail.deleteMessage, systemImage: "trash")
                            .font(.caption)
                    }
                    .buttonStyle(.borderless)
                    .accessibilityIdentifier(Ids.dmMessageDeleteButton)
                    .automationActivate(Ids.dmMessageDeleteButton) { confirmDelete = true }
                }
            }

            if canMarkSpam {
                // The live `Insert` consumer (mail-spam.md § Wire shapes) — trains the
                // sealed tier-1 model over the retained decrypted body AND writes a
                // sealed training-history row the `mail-spam` page renders + undoes
                // (`APIClient.markMessageAsSpam`). Fire-and-forget, silent on any
                // failure — matching `dm-message-delete-button`'s divider placement.
                if canReact || canDelete { Divider() }
                Button {
                    performMarkAsSpam()
                } label: {
                    Label(L.conversations.detail.markAsSpam, systemImage: "exclamationmark.bubble")
                        .font(.caption2)
                }
                .buttonStyle(.borderless)
                .accessibilityIdentifier(Ids.dmMessageMarkAsSpamButton)
                .automationActivate(Ids.dmMessageMarkAsSpamButton) { performMarkAsSpam() }
            }
        }
        .padding(8)
        .background(Color.secondary.opacity(0.12), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.dmMessageActionsMenu)
        // Presence anchor so the in-process driver can confirm the menu opened
        // (is_visible / count) — a bare `.accessibilityIdentifier` is invisible to
        // the registry; the read is empty (presence only).
        .automationValue(Ids.dmMessageActionsMenu, text: { "" })
    }

    /// Toggle `emoji` on this message and dismiss the overflow (the manager owns the
    /// optimistic aggregate; the bubble repaints off the re-emitted snapshot).
    private func toggle(_ emoji: String) {
        onToggleReaction(message.messageId, emoji)
        showMorePicker = false
        showActions = false
    }

    /// Confirm-step delete: dispatch the sender-only delete and dismiss the overflow
    /// (the manager optimistically marks `deleted`; the bubble repaints to the
    /// tombstone off the re-emitted snapshot).
    private func performDelete() {
        onDeleteMessage(message.messageId)
        confirmDelete = false
        showActions = false
    }

    /// Dispatch the mark-as-spam gesture and dismiss the overflow — no confirm
    /// step (unlike delete): the correction is reversible from the `mail-spam`
    /// page's per-row undo, matching linux/windows (no in-bubble confirm there
    /// either).
    private func performMarkAsSpam() {
        onMarkAsSpam(message.messageId)
        showActions = false
    }

    /// The in-bubble reply-quote card (D2b — render-model.md § D2 QuotedMessage): the parent
    /// message's author over a ≤2-line truncated snippet of its body, in an accent-bar card above
    /// the bubble body. Rendered only when the manager folded a `.quotedMessage` block (this message
    /// replies AND its parent is loaded). The `automationValue` carries `author: snippet` so the
    /// in-process driver can count + read it (the e2e asserts the parent snippet is present).
    private func quoteCard(author: String, snippet: String) -> some View {
        HStack(spacing: 6) {
            RoundedRectangle(cornerRadius: 1).fill(Color.accentColor).frame(width: 3)
            VStack(alignment: .leading, spacing: 1) {
                Text(author)
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(.tint)
                Text(snippet)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                    .truncationMode(.tail)
            }
            Spacer(minLength: 0)
        }
        .padding(6)
        .background(Color.secondary.opacity(0.10), in: RoundedRectangle(cornerRadius: 6))
        .accessibilityIdentifier(Ids.dmMessageQuote)
        .automationValue(Ids.dmMessageQuote, text: { "\(author): \(snippet)" })
    }

    /// One attachment row, from a document `.attachment` embed block (render-model.md § D2a).
    /// Image attachments render the **real** decoded image (loaded via the shared
    /// `attachment_bytes(blob_hash)` loader, threaded down as `loadAttachment` — a miss is what
    /// asks the receive loop to fetch the bytes again). A picture without decodable bytes — not
    /// yet fetched, or evicted with nowhere to refill from — degrades to its DECLARED
    /// placeholder under the same id: filename and size, never a bare icon (`conversations.md`
    /// § Attachments → *Retention*); every other type is a file row carrying the same name and
    /// size. `dm-attachment-image` answers `get_attr(.., "state")` with `painted` (a decoded
    /// image on screen) or `placeholder` — the strings linux's and web's agents answer.
    /// Non-`.attachment` blocks no-op (the caller only passes the document's attachment blocks).
    ///
    /// Re-read on every render, and `attachmentResidency` is what forces one when bytes are
    /// evicted or fetched again — neither changes the message.
    ///
    /// The receiver's own per-attachment verdict — shared Rust probed the decrypted bytes
    /// (`AttachmentSnapshot.c2pa`) — paints `c2pa-badge` beside the attachment it vouches for,
    /// never the whole message (windows' `BuildImageAttachment` is the reference;
    /// `conversations.md` § Attachments "C2PA on-device").
    @ViewBuilder private func attachment(_ block: RenderBlock) -> some View {
        if case let .attachment(blobHash, filename, _, sizeBytes, isImage, c2pa) = block {
            let declared = "\(filename) (\(ValueFormat.byteSize(sizeBytes)))"
            VStack(alignment: .leading, spacing: 2) {
                if isImage, let data = loadAttachment(blobHash), let img = FaunaImage.decode(data) {
                    Image(platformImage: img)
                        .resizable()
                        .scaledToFit()
                        .frame(maxWidth: 240, maxHeight: 240, alignment: .leading)
                        .help(declared)
                        .accessibilityIdentifier(Ids.dmAttachmentImage)
                        .accessibilityLabel(declared)
                        .automationValue(Ids.dmAttachmentImage, text: { declared }, value: { "painted" })
                } else if isImage {
                    Label(declared, systemImage: "photo")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier(Ids.dmAttachmentImage)
                        .automationValue(Ids.dmAttachmentImage, text: { declared }, value: { "placeholder" })
                } else {
                    Label(declared, systemImage: "doc")
                        .font(.caption)
                        .accessibilityIdentifier(Ids.dmAttachmentFile)
                        .automationValue(Ids.dmAttachmentFile, text: { declared })
                }
                if c2pa {
                    badge("c.circle.fill", "c2pa-badge", "C2PA")
                }
            }
        }
    }

    /// `.automationValue` is what makes the badge discoverable to the in-process
    /// driver's `/element/count` (`AutomationRegistry` — a bare `.accessibilityIdentifier`
    /// never registers a slot; see `ContentLabelBadge`'s `automationText` for the same
    /// requirement on a `Text`-backed element).
    private func badge(_ symbol: String, _ id: String, _ label: String) -> some View {
        Image(systemName: symbol)
            .font(.caption2)
            .foregroundStyle(.secondary)
            .help(label)
            .accessibilityIdentifier(id)
            .accessibilityLabel(label)
            .automationValue(id, text: { label })
    }
}
