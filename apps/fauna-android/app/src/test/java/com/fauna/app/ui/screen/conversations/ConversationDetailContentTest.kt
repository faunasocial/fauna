package com.fauna.app.ui.screen.conversations

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.semantics.SemanticsProperties
import android.graphics.Bitmap
import java.io.ByteArrayOutputStream
import com.fauna.app.core.ContentPolicyInputs
import com.fauna.app.ui.util.LocalConnectionState
import com.fauna.ffi.FfiConnectionState
import com.fauna.ffi.FfiContentPolicy
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_conversations.AddParticipantState
import uniffi.fauna_conversations.AttachmentDraft
import uniffi.fauna_conversations.AttachmentSnapshot
import uniffi.fauna_conversations.ComposeState
import uniffi.fauna_conversations.DeliveryMode
import uniffi.fauna_conversations.MessageBadges
import uniffi.fauna_conversations.MessageSnapshot
import uniffi.fauna_conversations.Rail
import uniffi.fauna_conversations.ReactionGroup
import uniffi.fauna_conversations.RecipientPickerState
import uniffi.fauna_conversations.ReplyPreview
import uniffi.fauna_conversations.quicksetEmojis
import uniffi.fauna_conversations.railGlyph
import uniffi.fauna_conversations.ResolveState
import uniffi.fauna_conversations.SendState
import uniffi.fauna_conversations.ThreadCapabilities
import uniffi.fauna_conversations.ThreadDetail
import uniffi.fauna_conversations.ThreadEncryption
import uniffi.fauna_conversations.ThreadFlavor
import uniffi.fauna_conversations.TypedAddress
import uniffi.fauna_core.ContentLabelEntry
import uniffi.fauna_core.Inline
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.RenderBlock
import uniffi.fauna_core.RenderDocument

/**
 * Compose-level coverage for the stateless [ConversationDetailContent] (the
 * conversation detail pane, `docs/goal/ui/conversations.md`). Renders with a
 * seeded [ThreadDetail] — no Hilt, no VM, no FFI native calls (the snapshot
 * records are plain Kotlin data classes) — so the thread-header, member chips,
 * capability-gated affordances, message bubbles + badges, subject dividers, and
 * the gesture callbacks are exercised on the JVM. (Android E2E
 * `test_subject_divider.py` / `test_capability_gating.py --client android` is
 * the standing gate once the host emulator lands.)
 */
@RunWith(FaunaRobolectricTestRunner::class)
// Tall screen so the message LazyColumn (in a weight(1f) Box above the compose
// bar) renders all seeded messages without virtualization dropping the last one —
// the compose bar grew a markdown-toolbar row, shrinking the default-screen
// viewport. These tests verify per-message rendering, not viewport packing.
@Config(sdk = [34], qualifiers = "h1280dp")
class ConversationDetailContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun badges(
        encrypted: Boolean = false,
        signed: Boolean = false,
        verified: Boolean = false,
        contentWarning: String? = null,
    ) = MessageBadges(
        encrypted = encrypted,
        signed = signed,
        verified = verified,
        contentWarning = contentWarning,
    )

    /** A plaintext body's render document — one paragraph of [body], then one `Attachment`
     *  block per attachment, exactly what the manager's `append_message` fold produces
     *  (render-model.md § D2). The bubble now renders attachments from the document, so the
     *  fixture must carry them there, not only in the sibling `attachments` field. */
    private fun plainDoc(body: String, attachments: List<AttachmentSnapshot> = emptyList()) =
        RenderDocument(
            listOf<RenderBlock>(RenderBlock.Paragraph(listOf(Inline.Text(body)))) +
                attachments.map { att ->
                    RenderBlock.Attachment(
                        blobHash = att.blobHash,
                        filename = att.filename,
                        mimeType = att.mimeType,
                        sizeBytes = att.sizeBytes,
                        isImage = att.isImage,
                        c2pa = att.c2pa,
                    )
                },
        )

    private fun msg(
        id: String,
        sender: String = "Alice",
        body: String = "hi there",
        subjectLine: String? = null,
        badges: MessageBadges = badges(),
        attachments: List<AttachmentSnapshot> = emptyList(),
        document: RenderDocument = plainDoc(body, attachments),
        reactions: List<ReactionGroup> = emptyList(),
        deleted: Boolean = false,
        isOwn: Boolean = false,
        labels: List<ContentLabelEntry> = emptyList(),
    ) = MessageSnapshot(
        messageId = id,
        sender = TypedAddress.Email("$sender@example.com"),
        senderDisplay = sender,
        body = body,
        document = document,
        timestampMs = 1_700_000_000_000L,
        subjectLine = subjectLine,
        badges = badges,
        replyTo = null,
        reactions = reactions,
        deleted = deleted,
        isOwn = isOwn,
        legalTakedownRef = null,
        // Post-decrypt content-policy labels (moderation.md § Per-row badge
        // data path) — drives `content-label-badge` via the shared
        // primaryContentLabel pick (contentLabelBadgeRendersHighestConfidenceLabel
        // below); empty elsewhere since no other case in this file exercises it.
        labels = labels,
        // The account-data-plane identity a T1 observation reports
        // (account-data-plane.md § The replica boundary → T1). Absent here:
        // this file tests the bubble's RENDER, and android has not built the
        // reporting leg yet (tui leads).
        planeRef = null,
    )

    private fun caps(
        membershipChange: Boolean = false,
        rename: Boolean = false,
        attachments: Boolean = true,
        subject: Boolean = false,
        markdown: Boolean = true,
        recipientSelection: Boolean = false,
        reactions: Boolean = false,
        messageDelete: Boolean = false,
    ) = ThreadCapabilities(
        supportsAttachments = attachments,
        supportsMarkdown = markdown,
        supportsReactions = reactions,
        supportsPerMessageReply = true,
        supportsMembershipChange = membershipChange,
        supportsRecipientSelection = recipientSelection,
        supportsRename = rename,
        supportsSubject = subject,
        supportsMessageDelete = messageDelete,
        deliveryMode = DeliveryMode.REALTIME,
        encryption = ThreadEncryption.E2E,
    )

    private fun attachmentDraft(
        filename: String = "photo.png",
        sizeBytes: ULong = 512UL,
        isImage: Boolean = true,
        mimeType: String = "image/png",
        blobHash: String = "hash-$filename",
    ) = AttachmentDraft(
        blobHash = blobHash,
        filename = filename,
        mimeType = mimeType,
        sizeBytes = sizeBytes,
        isImage = isImage,
    )

    private fun composeState(
        bodyDraft: String = "",
        subjectDraft: String? = null,
        replyTo: String? = null,
        sendState: SendState = SendState.Idle,
        replyRecipients: List<TypedAddress> = emptyList(),
        attachments: List<AttachmentDraft> = emptyList(),
    ) = ComposeState(
        bodyDraft = bodyDraft,
        subjectDraft = subjectDraft,
        attachments = attachments,
        replyTo = replyTo,
        replyRecipients = replyRecipients,
        recipientPicker = null,
        sendState = sendState,
    )

    private fun detail(
        label: String = "Alice",
        rail: Rail = Rail.FAUNA_MLS,
        flavor: ThreadFlavor = ThreadFlavor.OneToOne,
        participantDisplays: List<String> = listOf("Alice", "Bob"),
        capabilities: ThreadCapabilities = caps(),
        messages: List<MessageSnapshot> = emptyList(),
        compose: ComposeState = composeState(),
        // The search-hit selection (`conversations.md` § The selected message) —
        // `SearchNav::Mail`'s second half. `null` unless a test opts a specific
        // message in, mirroring the read-time-resolve: the producer already
        // guarantees `None` unless the id names a message in `messages`.
        selectedMessageId: String? = null,
    ) = ThreadDetail(
        threadId = "t1",
        rail = rail,
        glyph = railGlyph(rail),
        flavor = flavor,
        label = label,
        participants = participantDisplays.map { TypedAddress.Email("$it@example.com") },
        participantDisplays = participantDisplays,
        capabilities = capabilities,
        messages = messages,
        selectedMessageId = selectedMessageId,
        compose = compose,
        // `null` = a rail that models no room yet, which is what every fixture
        // here is (`conversation-rooms.md` § The room). Added when `ThreadDetail`
        // grew the field; nothing on any merge path
        // compiles this source set except `android-unit-test-compile-check`,
        // which is exactly the gate this omission was waiting for.
        room = null,
    )

    private fun addParticipantState(
        targetThreadId: String = "t1",
        rawInput: String = "",
        chips: List<TypedAddress> = emptyList(),
        inPlaceMlsGroup: Boolean = false,
    ) = AddParticipantState(
        targetThreadId = targetThreadId,
        picker = RecipientPickerState(
            rawInput = rawInput,
            chips = chips,
            suggestions = emptyList(),
            resolveState = ResolveState.IDLE,
            resolved = null,
        ),
        inPlaceMlsGroup = inPlaceMlsGroup,
    )

    private fun render(
        detail: ThreadDetail?,
        addParticipant: AddParticipantState? = null,
        onBack: () -> Unit = {},
        onReply: (String) -> Unit = {},
        onAddParticipant: () -> Unit = {},
        onRename: (String) -> Unit = {},
        onReplyAll: (String) -> Unit = {},
        onAddReplyRecipient: (String) -> Unit = {},
        onRemoveReplyRecipient: (TypedAddress) -> Unit = {},
        onRemoveAttachment: (Int) -> Unit = {},
        // Non-FFI stand-in — the real default calls the shared byte_size FFI,
        // which can't load under Robolectric (the .so). `resolveLocalized`
        // returns the key verbatim when no matching string resource exists, so
        // this renders as a deterministic, assertable "<n>B" stub.
        byteSize: (ULong) -> LocalizedText = { LocalizedText(key = "${it}B", args = emptyMap()) },
        onAddParticipantInputChange: (String) -> Unit = {},
        onAddParticipantAccept: () -> Unit = {},
        onAddParticipantConfirm: () -> Unit = {},
        onAddParticipantCancel: () -> Unit = {},
        onBodyChange: (String) -> Unit = {},
        onSubjectChange: (String) -> Unit = {},
        onTopicToggle: () -> Unit = {},
        onSend: () -> Unit = {},
        onAttach: () -> Unit = {},
        onReplyCancel: () -> Unit = {},
        onToggleReaction: (String, String) -> Unit = { _, _ -> },
        onDeleteMessage: (String) -> Unit = {},
        onMoreReaction: (String) -> Unit = {},
        onMarkSpam: (MessageSnapshot) -> Unit = {},
        contentPolicyInputs: ContentPolicyInputs = ContentPolicyInputs(),
        memberMarks: List<ByteArray?> = emptyList(),
        onKeepMember: (ByteArray) -> Unit = {},
        onRemoveParticipant: (TypedAddress) -> Unit = {},
        pageError: LocalizedText? = null,
        receiveStopped: Boolean = false,
        unopenableMail: UInt = 0u,
        // `null` is UNKNOWN, not "connecting" — the shared rule leaves a
        // control live on a state word it does not recognise, so every case
        // that does not opt in sees exactly the ungated UI it saw before the
        // offline gate reached this page.
        connectionState: FfiConnectionState? = null,
        replyPreview: ReplyPreview? = null,
        loadAttachmentBytes: (String) -> ByteArray? = { null },
        attachmentResident: (String) -> Boolean = { false },
    ) {
        composeTestRule.setContent {
            CompositionLocalProvider(LocalConnectionState provides connectionState) {
            ConversationDetailContent(
                detail = detail,
                addParticipant = addParticipant,
                contentPolicyInputs = contentPolicyInputs,
                memberMarks = memberMarks,
                onKeepMember = onKeepMember,
                onRemoveParticipant = onRemoveParticipant,
                pageError = pageError,
                receiveStopped = receiveStopped,
                unopenableMail = unopenableMail,
                onBack = onBack,
                onReply = onReply,
                onAddParticipant = onAddParticipant,
                onRename = onRename,
                onReplyAll = onReplyAll,
                onAddReplyRecipient = onAddReplyRecipient,
                onRemoveReplyRecipient = onRemoveReplyRecipient,
                onRemoveAttachment = onRemoveAttachment,
                byteSize = byteSize,
                onAddParticipantInputChange = onAddParticipantInputChange,
                onAddParticipantAccept = onAddParticipantAccept,
                onAddParticipantConfirm = onAddParticipantConfirm,
                onAddParticipantCancel = onAddParticipantCancel,
                onBodyChange = onBodyChange,
                onSubjectChange = onSubjectChange,
                onTopicToggle = onTopicToggle,
                onSend = onSend,
                onAttach = onAttach,
                onReplyCancel = onReplyCancel,
                onToggleReaction = onToggleReaction,
                onDeleteMessage = onDeleteMessage,
                onMoreReaction = onMoreReaction,
                onMarkSpam = onMarkSpam,
                replyPreview = replyPreview,
                loadAttachmentBytes = loadAttachmentBytes,
                attachmentResident = attachmentResident,
            )
            }
        }
    }

    @Test
    fun headerRendersWithProtocolIcon() {
        render(detail(label = "Alice", messages = listOf(msg("m1"))))
        composeTestRule.onNodeWithTag("thread-header").assertExists()
        // protocol-icon appears in the header (+ none elsewhere on the detail).
        assertEquals(
            1,
            composeTestRule.onAllNodesWithTag("protocol-icon").fetchSemanticsNodes().size,
        )
    }

    // ── Content-policy render enforcement (Slice C, family-safety.md § Content ──
    // policy). The verdict is a real shared-Rust call, so these run under
    // android-host-test (host JNA). The cross-app tier_3 proof
    // (test_family.py) is host-emulator-gated on android, so these Robolectric
    // arms are the locally-runnable proof the block mechanism actually gates.

    @Test
    fun guardianBlockFloorCollapsesBubbleToNoticeAndHidesBody() {
        // A guardian `block` floor over an nsfw-labeled message (700‰ > the 500‰
        // guardian trigger) collapses the bubble to `content-policy-blocked-notice`
        // in place of the body — no `dm-message-text` reaches the tree.
        val labeled = msg("m1", body = "flagged", labels = listOf(ContentLabelEntry("nsfw", 700u)))
        render(
            detail(messages = listOf(labeled)),
            contentPolicyInputs = ContentPolicyInputs(
                contentPolicy = FfiContentPolicy("block", "inherit", "inherit", "inherit"),
            ),
        )
        composeTestRule.onNodeWithTag("content-policy-blocked-notice").assertExists()
        composeTestRule.onNodeWithTag("dm-message-text").assertDoesNotExist()
    }

    @Test
    fun cleanMessageUnderABlockFloorStillRendersItsBody() {
        // The gate is label-specific (not always-on): an UNLABELED message under
        // the same block floor renders its body normally, no false-positive
        // collapse and no notice.
        val clean = msg("m1", body = "hello there")
        render(
            detail(messages = listOf(clean)),
            contentPolicyInputs = ContentPolicyInputs(
                contentPolicy = FfiContentPolicy("block", "inherit", "inherit", "inherit"),
            ),
        )
        composeTestRule.onNodeWithTag("dm-message-text").assertExists()
        composeTestRule.onNodeWithTag("content-policy-blocked-notice").assertDoesNotExist()
    }

    @Test
    fun memberChipsRenderPerParticipant() {
        render(detail(participantDisplays = listOf("Alice", "Bob", "Carol")))
        assertEquals(
            3,
            composeTestRule.onAllNodesWithTag("thread-member-chip").fetchSemanticsNodes().size,
        )
    }

    @Test
    fun memberChipsRenderTheFullSetPastThree() {
        // The FULL participant set, one chip each — never a "first 3 + +N"
        // truncation (conversations.md § Detail pane, ratified 2026-08-28; this
        // app was the one place the cap half was ever built, and it hid a
        // flagged member's review pair behind the overflow). Both halves
        // pinned: every chip present, and the overflow text gone.
        render(detail(participantDisplays = listOf("A", "B", "C", "D", "E")))
        assertEquals(
            5,
            composeTestRule.onAllNodesWithTag("thread-member-chip").fetchSemanticsNodes().size,
        )
        composeTestRule.onNodeWithText("+2 more").assertDoesNotExist()
    }

    // ── Post-succession member review pair. Mirrors
    // linux's `build_member_chip` unit pins: the mark + Keep button render
    // only on the chip whose index carries a non-null mark, present and
    // absent are both asserted (never infer absence from a single positive
    // case), and Keep is wired to the RIGHT person, not just any person.

    @Test
    fun memberUnattestedMarkAndKeepButtonRenderOnlyOnTheFlaggedParticipant() {
        val bob = byteArrayOf(1, 2, 3)
        render(
            detail(participantDisplays = listOf("Alice", "Bob", "Carol")),
            memberMarks = listOf(null, bob, null),
        )
        // 3 chips still render — the pair scopes INSIDE the existing chip,
        // never a 4th flat element indexing separately over it.
        assertEquals(
            3,
            composeTestRule.onAllNodesWithTag("thread-member-chip").fetchSemanticsNodes().size,
        )
        // AssistChip merges its label's semantics into one accessible node
        // (the same class AttendeeRowContentTest hit),
        // so the nested mark/Keep testTags are only reachable unmerged.
        composeTestRule.onAllNodesWithTag("thread-member-unattested-mark", useUnmergedTree = true)
            .assertCountEquals(1)
        composeTestRule.onAllNodesWithTag("thread-member-keep-button", useUnmergedTree = true)
            .assertCountEquals(1)
    }

    @Test
    fun memberUnattestedMarkRendersOnAParticipantPastTheThird() {
        // The defect the first-3 cap caused, pinned as a positive: a flagged
        // member at index 3+ renders their mark + Keep exactly like index 0-2
        // (the surface's job is to show the owner the whole roster —
        // succession-aftermath.md § Propagation → *MLS groups*).
        val eve = byteArrayOf(4, 5, 6)
        render(
            detail(participantDisplays = listOf("A", "B", "C", "D", "Eve")),
            memberMarks = listOf(null, null, null, null, eve),
        )
        composeTestRule.onAllNodesWithTag("thread-member-unattested-mark", useUnmergedTree = true)
            .assertCountEquals(1)
        composeTestRule.onAllNodesWithTag("thread-member-keep-button", useUnmergedTree = true)
            .assertCountEquals(1)
    }

    @Test
    fun memberUnattestedMarkAbsentWhenNoOneIsFlagged() {
        render(
            detail(participantDisplays = listOf("Alice", "Bob")),
            memberMarks = listOf(null, null),
        )
        composeTestRule.onAllNodesWithTag("thread-member-unattested-mark", useUnmergedTree = true)
            .assertCountEquals(0)
        composeTestRule.onAllNodesWithTag("thread-member-keep-button", useUnmergedTree = true)
            .assertCountEquals(0)
    }

    @Test
    fun keepButtonInvokesTheCallbackWithTheFlaggedPersonsId() {
        val bob = byteArrayOf(9, 8, 7)
        var kept: ByteArray? = null
        render(
            detail(participantDisplays = listOf("Alice", "Bob")),
            memberMarks = listOf(null, bob),
            onKeepMember = { kept = it },
        )
        composeTestRule.onNodeWithTag("thread-member-keep-button", useUnmergedTree = true)
            .performClick()
        assertEquals(true, bob.contentEquals(kept))
    }

    // ── Member chip IS Remove (conversations.md § the ─────────────────
    // `thread-member-chip[i]` row) — bound at PAINT, gated on
    // `supports_membership_change`, and must never double-fire with the
    // nested Keep control (the inverse of the verdict pressed).

    @Test
    fun memberChipRemovesTheRightParticipantOnAMembershipCapableThread() {
        var removed: TypedAddress? = null
        render(
            detail(
                participantDisplays = listOf("Alice", "Bob", "Carol"),
                capabilities = caps(membershipChange = true),
            ),
            onRemoveParticipant = { removed = it },
        )
        // Index 1 — "Bob" — never index 0, so a wrong-index bug cannot pass.
        composeTestRule.onAllNodesWithTag("thread-member-chip")[1].performClick()
        assertEquals(TypedAddress.Email("Bob@example.com"), removed)
    }

    @Test
    fun memberChipDoesNothingOnANonMembershipCapableThread() {
        // Mail: the historical From/To/Cc set is informational, not mutable
        // (conversations.md § Participants vs. reply recipients) — the chip
        // must not read as a control at all.
        var removed: TypedAddress? = null
        render(
            detail(
                participantDisplays = listOf("Alice", "Bob"),
                capabilities = caps(membershipChange = false),
            ),
            onRemoveParticipant = { removed = it },
        )
        composeTestRule.onAllNodesWithTag("thread-member-chip")[0].performClick()
        assertEquals(null, removed)
    }

    @Test
    fun keepButtonDoesNotAlsoTriggerTheChipsRemove() {
        // The inner Keep `.clickable` must consume its own tap — pressing Keep
        // and evicting the person instead would be the exact inverse of the
        // verdict pressed. Asserted, not assumed (the item's own wording).
        val bob = byteArrayOf(9, 8, 7)
        var kept: ByteArray? = null
        var removed: TypedAddress? = null
        render(
            detail(
                participantDisplays = listOf("Alice", "Bob"),
                capabilities = caps(membershipChange = true),
            ),
            memberMarks = listOf(null, bob),
            onKeepMember = { kept = it },
            onRemoveParticipant = { removed = it },
        )
        composeTestRule.onNodeWithTag("thread-member-keep-button", useUnmergedTree = true)
            .performClick()
        assertEquals(true, bob.contentEquals(kept))
        assertEquals(null, removed)
    }

    // ── `error-message` precedence (linux's `page_error_text` twin) ───────────

    @Test
    fun pageErrorRendersInErrorMessage() {
        render(
            detail(compose = composeState(sendState = SendState.Idle)),
            pageError = LocalizedText(
                key = "conversations.unified.error_remove_participant",
                args = mapOf("message" to "nest refused the removal"),
            ),
        )
        composeTestRule.onNodeWithTag("error-message")
            .assertExists()
            .assertTextContains("nest refused the removal", substring = true)
            .assertTextContains("Could not remove them", substring = true)
    }

    @Test
    fun pageErrorOutranksAFailedComposeSend() {
        // Every `pageError` producer clears it on entry, so it is always the
        // more recent of the two by construction — linux's own precedence
        // (`page_error_text`), mirrored here.
        render(
            detail(
                compose = composeState(
                    sendState = SendState.Failed(
                        LocalizedText(
                            key = "conversations.unified.error_send",
                            args = mapOf("message" to "send failed"),
                        ),
                    ),
                ),
            ),
            pageError = LocalizedText(
                key = "conversations.unified.error_remove_participant",
                args = mapOf("message" to "removal failed"),
            ),
        )
        composeTestRule.onNodeWithTag("error-message")
            .assertTextContains("removal failed", substring = true)
        composeTestRule.onAllNodesWithTag("error-message").assertCountEquals(1)
    }

    @Test
    fun receiveStoppedOutranksAPageErrorAndAFailedComposeSend() {
        // The dead-receive-rail notice (conversations.md § Errors & edge
        // cases, the fourth truth) is a STANDING condition set by the shared
        // receive loop's supervisor, never by a page producer — it must not
        // be masked by either of the other two truths. Android has no
        // served-elsewhere arm, so receiveStopped is TOP precedence here
        // (apple/windows read served-elsewhere first, then receiveStopped).
        render(
            detail(
                compose = composeState(
                    sendState = SendState.Failed(
                        LocalizedText(
                            key = "conversations.unified.error_send",
                            args = mapOf("message" to "send failed"),
                        ),
                    ),
                ),
            ),
            pageError = LocalizedText(
                key = "conversations.unified.error_remove_participant",
                args = mapOf("message" to "removal failed"),
            ),
            receiveStopped = true,
        )
        composeTestRule.onNodeWithTag("error-message")
            .assertTextContains("stopped arriving", substring = true)
        composeTestRule.onAllNodesWithTag("error-message").assertCountEquals(1)
    }

    @Test
    fun receiveStoppedIsShownWithNoOtherErrorPresent() {
        // A success clears `pageError` and an idle compose carries no send
        // failure, but the dead-receive-rail notice is not cleared by either
        // — it stands until a newer receive loop over the same manager
        // retires it (no gesture clears it).
        render(
            detail(compose = composeState(sendState = SendState.Idle)),
            pageError = null,
            receiveStopped = true,
        )
        composeTestRule.onNodeWithTag("error-message")
            .assertExists()
            .assertTextContains("stopped arriving", substring = true)
    }

    // The floor of the stack (`ui/conversations.md` § Errors & edge cases →
    // *A fifth truth*, 2026-09-15): received mail this run that could not
    // open under the account's key set. Mirrors linux's
    // `unopenable_mail_shows_only_when_nothing_else_present` /
    // `a_failed_send_outranks_unopenable_mail`.

    @Test
    fun unopenableMailShowsWithNoOtherErrorPresent() {
        render(
            detail(compose = composeState(sendState = SendState.Idle)),
            unopenableMail = 2u,
        )
        composeTestRule.onNodeWithTag("error-message")
            .assertExists()
            .assertTextContains("could not be opened", substring = true)
            .assertTextContains("2 ", substring = true)
    }

    @Test
    fun aFailedComposeSendOutranksUnopenableMail() {
        render(
            detail(
                compose = composeState(
                    sendState = SendState.Failed(
                        LocalizedText(
                            key = "conversations.unified.error_send",
                            args = mapOf("message" to "send failed"),
                        ),
                    ),
                ),
            ),
            unopenableMail = 2u,
        )
        composeTestRule.onNodeWithTag("error-message")
            .assertTextContains("send failed", substring = true)
        composeTestRule.onAllNodesWithTag("error-message").assertCountEquals(1)
    }

    @Test
    fun receiveStoppedOutranksUnopenableMail() {
        render(
            detail(compose = composeState(sendState = SendState.Idle)),
            receiveStopped = true,
            unopenableMail = 2u,
        )
        composeTestRule.onNodeWithTag("error-message")
            .assertTextContains("stopped arriving", substring = true)
        composeTestRule.onAllNodesWithTag("error-message").assertCountEquals(1)
    }

    @Test
    fun renameButtonHiddenWhenUnsupported() {
        render(detail(capabilities = caps(rename = false)))
        composeTestRule.onAllNodesWithTag("thread-rename-button").assertCountEquals(0)
    }

    @Test
    fun renameButtonShownWhenSupported() {
        render(detail(capabilities = caps(rename = true)))
        composeTestRule.onNodeWithTag("thread-rename-button").assertExists()
    }

    @Test
    fun addParticipantHiddenWhenUnsupported() {
        render(detail(capabilities = caps(membershipChange = false)))
        composeTestRule.onAllNodesWithTag("thread-add-participant-button").assertCountEquals(0)
    }

    @Test
    fun addParticipantShownWhenSupported() {
        render(detail(capabilities = caps(membershipChange = true)))
        composeTestRule.onNodeWithTag("thread-add-participant-button").assertExists()
    }

    @Test
    fun messageBubblesRenderBodyAndSender() {
        render(detail(messages = listOf(msg("m1", body = "first"), msg("m2", body = "second"))))
        assertEquals(
            2,
            composeTestRule.onAllNodesWithTag("dm-sender").fetchSemanticsNodes().size,
        )
        assertEquals(
            2,
            composeTestRule.onAllNodesWithTag("dm-message-text").fetchSemanticsNodes().size,
        )
        assertEquals(
            2,
            composeTestRule.onAllNodesWithTag("dm-reply-button").fetchSemanticsNodes().size,
        )
    }

    @Test
    fun badgesRenderPerFlags() {
        render(
            detail(
                messages = listOf(
                    msg("m1", badges = badges(encrypted = true, signed = true, verified = true)),
                ),
            ),
        )
        composeTestRule.onNodeWithTag("encrypted-badge").assertExists()
        composeTestRule.onNodeWithTag("signed-badge").assertExists()
        composeTestRule.onNodeWithTag("verified-badge").assertExists()
    }

    /** `c2pa-badge` is the per-ATTACHMENT verdict (`AttachmentSnapshot.c2pa`): one badge
     *  beside the signed picture, none for the unsigned one beside it and none on the
     *  message header (conversation-attachments.md § Attachments "C2PA on-device"). */
    @Test
    fun c2paBadgeRendersPerSignedAttachment() {
        val unsigned = AttachmentSnapshot(
            blobHash = "cc01", filename = "plain.png", mimeType = "image/png",
            sizeBytes = 10u, isImage = true, c2pa = false,
        )
        val signed = unsigned.copy(blobHash = "cc02", filename = "signed.png", c2pa = true)
        render(detail(messages = listOf(msg("m1", attachments = listOf(unsigned, signed)))))
        composeTestRule.onAllNodesWithTag("dm-attachment-image").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("c2pa-badge").assertCountEquals(1)
    }

    @Test
    fun contentLabelBadgeRendersHighestConfidenceLabel() {
        // Two entries — the shared com.fauna.ffi.primaryContentLabel pick (not a
        // local reduce) must resolve to "spam" (850) over "phishing" (400).
        render(
            detail(
                messages = listOf(
                    msg(
                        "m1",
                        labels = listOf(
                            ContentLabelEntry("phishing", 400u),
                            ContentLabelEntry("spam", 850u),
                        ),
                    ),
                ),
            ),
        )
        composeTestRule.onNodeWithTag("content-label-badge").assertExists()
        composeTestRule.onNodeWithText("Spam").assertExists()
    }

    @Test
    fun contentLabelBadgeAbsentWhenUnlabelled() {
        render(detail(messages = listOf(msg("m1"))))
        composeTestRule.onNodeWithTag("content-label-badge").assertDoesNotExist()
    }

    @Test
    fun subjectDividerRendersOnSubjectChange() {
        // The snapshot sets subject_line only on a subject change; the first
        // message has none, the second carries a new subject → one divider.
        render(
            detail(
                messages = listOf(
                    msg("m1", subjectLine = null),
                    msg("m2", subjectLine = "Re: lunch"),
                ),
            ),
        )
        assertEquals(
            1,
            composeTestRule.onAllNodesWithTag("subject-divider").fetchSemanticsNodes().size,
        )
    }

    @Test
    fun attachmentTagsByKind() {
        render(
            detail(
                messages = listOf(
                    msg(
                        "m1",
                        attachments = listOf(
                            AttachmentSnapshot(
                                blobHash = "aa01",
                                filename = "pic.png",
                                mimeType = "image/png",
                                sizeBytes = 10u,
                                isImage = true,
                                c2pa = false,
                            ),
                            AttachmentSnapshot(
                                blobHash = "bb02",
                                filename = "doc.pdf",
                                mimeType = "application/pdf",
                                sizeBytes = 20u,
                                isImage = false,
                                c2pa = false,
                            ),
                        ),
                    ),
                ),
            ),
        )
        composeTestRule.onNodeWithTag("dm-attachment-image").assertExists()
        composeTestRule.onNodeWithTag("dm-attachment-file").assertExists()
    }

    private val picture = AttachmentSnapshot(
        blobHash = "aa01",
        filename = "pic.png",
        mimeType = "image/png",
        sizeBytes = 10u,
        isImage = true,
        c2pa = false,
    )

    private fun pngBytes(): ByteArray = ByteArrayOutputStream().also { out ->
        Bitmap.createBitmap(4, 4, Bitmap.Config.ARGB_8888).compress(Bitmap.CompressFormat.PNG, 100, out)
    }.toByteArray()

    private fun attachmentState(): String? =
        composeTestRule.onNodeWithTag("dm-attachment-image")
            .fetchSemanticsNode().config.getOrElseNullable(SemanticsProperties.StateDescription) { null }

    /** A picture and a file with no resident bytes both show their DECLARED name and
     *  size (conversations.md § Attachments → *Retention*), and the picture's `state`
     *  says `placeholder`. */
    @Test
    fun attachmentsWithoutBytesShowTheirNameAndSize() {
        val file = picture.copy(blobHash = "bb02", filename = "notes.txt", mimeType = "text/plain", sizeBytes = 12u, isImage = false)
        render(detail(messages = listOf(msg("m1", attachments = listOf(picture, file)))))
        composeTestRule.onNodeWithTag("dm-attachment-image").assertTextEquals("pic.png (10B)")
        composeTestRule.onNodeWithTag("dm-attachment-file").assertTextEquals("notes.txt (12B)")
        assertEquals("placeholder", attachmentState())
    }

    /** The picture follows RESIDENCY, not only its hash: resident bytes paint, and an
     *  evict — which changes no message — repaints the declared placeholder. A bubble
     *  keyed on the hash alone would keep painting the dropped picture. */
    @Test
    fun theAttachmentPictureRebuildsWhenItsBytesLeave() {
        val png = pngBytes()
        var held by mutableStateOf(true)
        render(
            detail(messages = listOf(msg("m1", attachments = listOf(picture)))),
            loadAttachmentBytes = { if (held) png else null },
            attachmentResident = { held },
        )
        assertEquals("painted", attachmentState())
        held = false
        composeTestRule.waitForIdle()
        assertEquals("placeholder", attachmentState())
        composeTestRule.onNodeWithTag("dm-attachment-image").assertTextEquals("pic.png (10B)")
    }

    /** The compose bar renders the shared `reply_preview` record — sender and excerpt —
     *  never the bare answered-message id it painted before. */
    @Test
    fun replyPreviewRendersTheSharedRecord() {
        render(
            detail(compose = composeState(replyTo = "msg-7")),
            replyPreview = ReplyPreview(senderDisplay = "Alice", excerpt = "see you at noon"),
        )
        composeTestRule.onNodeWithTag("dm-reply-preview").assertTextEquals("Alice: see you at noon")
    }

    /** No record (the answered message is outside the fetched window) is an empty
     *  preview — never the raw message id. */
    @Test
    fun replyPreviewWithoutARecordPaintsNoId() {
        render(detail(compose = composeState(replyTo = "msg-7")))
        composeTestRule.onNodeWithTag("dm-reply-preview").assertTextEquals("")
    }

    @Test
    fun replyButtonPassesMessageId() {
        var replied: String? = null
        render(detail(messages = listOf(msg("msg-7"))), onReply = { replied = it })
        composeTestRule.onNodeWithTag("dm-reply-button").performClick()
        assertEquals("msg-7", replied)
    }

    @Test
    fun nullDetailShowsHintNotHeader() {
        render(detail = null)
        composeTestRule.onAllNodesWithTag("thread-header").assertCountEquals(0)
    }

    // ── Phase 3: compose bar + error surface ─────────────────────────

    @Test
    fun composeBarRendersOnOpenThread() {
        render(detail(messages = listOf(msg("m1"))))
        composeTestRule.onNodeWithTag("dm-text-field").assertExists()
        composeTestRule.onNodeWithTag("dm-send-button").assertExists()
        composeTestRule.onNodeWithTag("topic-toggle-button").assertExists()
        composeTestRule.onNodeWithTag("attachment-button").assertExists()
    }

    @Test
    fun emptyThreadStillShowsComposeBar() {
        // conversations.md §"Empty states": header + compose bar visible, bubble
        // area blank.
        render(detail(messages = emptyList()))
        composeTestRule.onNodeWithTag("thread-header").assertExists()
        composeTestRule.onNodeWithTag("dm-send-button").assertExists()
    }

    @Test
    fun subjectInputHiddenUntilTopicActive() {
        render(detail(compose = composeState(subjectDraft = null)))
        composeTestRule.onAllNodesWithTag("subject-input").assertCountEquals(0)
    }

    @Test
    fun subjectInputShownWhenTopicActive() {
        render(detail(compose = composeState(subjectDraft = "Re: lunch")))
        composeTestRule.onNodeWithTag("subject-input").assertExists()
    }

    @Test
    fun replyPreviewShownWhenReplying() {
        render(detail(compose = composeState(replyTo = "msg-7")))
        composeTestRule.onNodeWithTag("dm-reply-preview").assertExists()
        composeTestRule.onNodeWithTag("dm-reply-cancel").assertExists()
    }

    @Test
    fun replyPreviewHiddenWhenNotReplying() {
        render(detail(compose = composeState(replyTo = null)))
        composeTestRule.onAllNodesWithTag("dm-reply-preview").assertCountEquals(0)
    }

    @Test
    fun attachmentButtonDisabledWhenUnsupported() {
        render(detail(capabilities = caps(attachments = false)))
        composeTestRule.onNodeWithTag("attachment-button").assertIsNotEnabled()
    }

    @Test
    fun attachmentButtonEnabledWhenSupported() {
        render(detail(capabilities = caps(attachments = true)))
        composeTestRule.onNodeWithTag("attachment-button").assertIsEnabled()
    }

    @Test
    fun markdownToolbarEnabledWhenSupported() {
        // html-mail.md § Composition: the markdown toolbar is wired into the compose
        // bar and enabled on a markdown-capable thread (mail / FaunaMls).
        render(detail(capabilities = caps(markdown = true)))
        composeTestRule.onNodeWithTag("markdown-toolbar").assertExists()
        composeTestRule.onNodeWithTag("markdown-bold-button").assertIsEnabled()
        composeTestRule.onNodeWithTag("markdown-link-button").assertIsEnabled()
    }

    @Test
    fun markdownToolbarDisabledWhenUnsupported() {
        // Capability-gated (rule #5): disabled, not hidden, on a non-markdown rail.
        render(detail(capabilities = caps(markdown = false)))
        composeTestRule.onNodeWithTag("markdown-bold-button").assertIsNotEnabled()
    }

    @Test
    fun markdownBoldInsertsMarkers() {
        // The toolbar wraps the selection with markdown markers (client-glue text
        // wrapping) and forwards the new text via onBodyChange. With an empty draft
        // (cursor at 0, empty selection) bold inserts the bare `****` pair.
        var body: String? = null
        render(detail(compose = composeState(bodyDraft = "")), onBodyChange = { body = it })
        composeTestRule.onNodeWithTag("markdown-bold-button").performClick()
        assertEquals("****", body)
    }

    @Test
    fun errorMessageHiddenWhenSendIdle() {
        render(detail(compose = composeState(sendState = SendState.Idle)))
        composeTestRule.onAllNodesWithTag("error-message").assertCountEquals(0)
    }

    // The reason is a shared LocalizedText (key + `{message}`, the shape
    // `SendState::failed` produces), so the screen must resolve it through the
    // app's string table: the backend detail reaches the user AND the key never
    // survives to the surface. Verbatim equality could not tell those apart.
    @Test
    fun errorMessageRendersFailedReason() {
        render(
            detail(
                compose = composeState(
                    sendState = SendState.Failed(
                        LocalizedText(
                            key = "conversations.unified.error_send",
                            args = mapOf("message" to "nest rejected send"),
                        ),
                    ),
                ),
            ),
        )
        composeTestRule.onNodeWithTag("error-message")
            .assertExists()
            .assertTextContains("nest rejected send", substring = true)
        composeTestRule.onNodeWithTag("error-message")
            .assertTextContains("Could not send", substring = true)
    }

    @Test
    fun sendButtonFiresCallback() {
        var sent = false
        render(detail(messages = listOf(msg("m1"))), onSend = { sent = true })
        composeTestRule.onNodeWithTag("dm-send-button").performClick()
        assertEquals(true, sent)
    }

    // ── Phase 4: rename + add-participant overlays ───────────────────

    @Test
    fun renameDialogHiddenUntilButtonClicked() {
        render(detail(capabilities = caps(rename = true)))
        composeTestRule.onAllNodesWithTag("thread-rename-field").assertCountEquals(0)
    }

    @Test
    fun renameButtonOpensDialog() {
        render(detail(capabilities = caps(rename = true)))
        composeTestRule.onNodeWithTag("thread-rename-button").performClick()
        composeTestRule.onNodeWithTag("thread-rename-field").assertExists()
        composeTestRule.onNodeWithTag("thread-rename-confirm").assertExists()
    }

    @Test
    fun renameFieldPrefillsCurrentLabel() {
        render(detail(label = "Project X", capabilities = caps(rename = true)))
        composeTestRule.onNodeWithTag("thread-rename-button").performClick()
        composeTestRule.onNodeWithTag("thread-rename-field").assertTextContains("Project X")
    }

    @Test
    fun renameConfirmPassesNewLabel() {
        var renamed: String? = null
        render(detail(capabilities = caps(rename = true)), onRename = { renamed = it })
        composeTestRule.onNodeWithTag("thread-rename-button").performClick()
        composeTestRule.onNodeWithTag("thread-rename-field").performTextReplacement("Team chat")
        composeTestRule.onNodeWithTag("thread-rename-confirm").performClick()
        assertEquals("Team chat", renamed)
    }

    @Test
    fun renameConfirmDisabledWhenBlank() {
        render(detail(label = "Alice", capabilities = caps(rename = true)))
        composeTestRule.onNodeWithTag("thread-rename-button").performClick()
        composeTestRule.onNodeWithTag("thread-rename-field").performTextReplacement("   ")
        composeTestRule.onNodeWithTag("thread-rename-confirm").assertIsNotEnabled()
    }

    @Test
    fun addParticipantDialogHiddenWhenStateNull() {
        render(detail(capabilities = caps(membershipChange = true)), addParticipant = null)
        composeTestRule.onAllNodesWithTag("add-participant-confirm").assertCountEquals(0)
    }

    @Test
    fun addParticipantDialogShownWhenStatePresent() {
        render(
            detail(capabilities = caps(membershipChange = true)),
            addParticipant = addParticipantState(),
        )
        composeTestRule.onNodeWithTag("add-participant-confirm").assertExists()
        // Reuses the shared recipient picker.
        composeTestRule.onNodeWithTag("recipient-picker-input").assertExists()
    }

    @Test
    fun addParticipantConfirmFiresCallback() {
        var confirmed = false
        render(
            detail(capabilities = caps(membershipChange = true)),
            addParticipant = addParticipantState(),
            onAddParticipantConfirm = { confirmed = true },
        )
        composeTestRule.onNodeWithTag("add-participant-confirm").performClick()
        assertEquals(true, confirmed)
    }

    @Test
    fun addParticipantInputForwardsKeystrokes() {
        var typed: String? = null
        render(
            detail(capabilities = caps(membershipChange = true)),
            addParticipant = addParticipantState(),
            onAddParticipantInputChange = { typed = it },
        )
        composeTestRule.onNodeWithTag("recipient-picker-input").performTextInput("bob@x.test")
        assertEquals(true, typed?.isNotEmpty())
    }

    // --- Reply "To" line + reply-all (conversations.md § Participants vs.
    // reply recipients; mail only — gated on supportsRecipientSelection) ---

    @Test
    fun replyToLineRendersChipsAndAddOnRecipientSelectionRail() {
        render(
            detail(
                capabilities = caps(recipientSelection = true),
                compose = composeState(
                    replyRecipients = listOf(
                        TypedAddress.Email("alice@example.com"),
                        TypedAddress.Email("bob@example.com"),
                    ),
                ),
            ),
        )
        assertEquals(
            2,
            composeTestRule.onAllNodesWithTag("dm-reply-recipient-chip").fetchSemanticsNodes().size,
        )
        composeTestRule.onNodeWithTag("dm-reply-recipient-add").assertExists()
    }

    @Test
    fun replyToLineHiddenWithoutRecipientSelection() {
        // FaunaMls (default caps): recipients are the group, no editable To line.
        render(
            detail(
                compose = composeState(
                    replyRecipients = listOf(TypedAddress.Email("alice@example.com")),
                ),
            ),
        )
        composeTestRule.onNodeWithTag("dm-reply-recipient-add").assertDoesNotExist()
        composeTestRule.onAllNodesWithTag("dm-reply-recipient-chip").assertCountEquals(0)
    }

    @Test
    fun removeReplyRecipientFiresWithThatAddress() {
        var removed: TypedAddress? = null
        render(
            detail(
                capabilities = caps(recipientSelection = true),
                compose = composeState(
                    replyRecipients = listOf(
                        TypedAddress.Email("alice@example.com"),
                        TypedAddress.Email("bob@example.com"),
                    ),
                ),
            ),
            onRemoveReplyRecipient = { removed = it },
        )
        composeTestRule.onAllNodesWithTag("dm-reply-recipient-remove").onFirst().performClick()
        assertEquals(TypedAddress.Email("alice@example.com"), removed)
    }

    @Test
    fun addReplyRecipientFiresRawTextOnDone() {
        var added: String? = null
        render(
            detail(capabilities = caps(recipientSelection = true)),
            onAddReplyRecipient = { added = it },
        )
        composeTestRule.onNodeWithTag("dm-reply-recipient-add").performTextInput("carol@x.test")
        composeTestRule.onNodeWithTag("dm-reply-recipient-add").performImeAction()
        assertEquals("carol@x.test", added)
    }

    @Test
    fun stagedAttachmentChipRendersFilenameAndSize() {
        render(
            detail(
                compose = composeState(
                    attachments = listOf(attachmentDraft(filename = "vacation.png", sizeBytes = 2048UL)),
                ),
            ),
        )
        val chip = composeTestRule.onNodeWithTag("dm-compose-attachment-chip")
        chip.assertExists()
        chip.assertTextContains("vacation.png", substring = true)
        chip.assertTextContains("2048B", substring = true)
    }

    @Test
    fun noStagedAttachmentsRendersNoChip() {
        render(detail(compose = composeState(attachments = emptyList())))
        composeTestRule.onAllNodesWithTag("dm-compose-attachment-chip").assertCountEquals(0)
    }

    @Test
    fun removeAttachmentFiresWithThatIndex() {
        var removedIndex: Int? = null
        render(
            detail(
                compose = composeState(
                    attachments = listOf(
                        attachmentDraft(filename = "one.png"),
                        attachmentDraft(filename = "two.png"),
                    ),
                ),
            ),
            onRemoveAttachment = { removedIndex = it },
        )
        composeTestRule.onAllNodesWithTag("dm-compose-attachment-remove")[1].performClick()
        assertEquals(1, removedIndex)
    }

    @Test
    fun replyAllButtonShownOnRecipientSelectionRail() {
        render(detail(capabilities = caps(recipientSelection = true), messages = listOf(msg("m1"))))
        composeTestRule.onNodeWithTag("dm-reply-all-button").assertExists()
    }

    @Test
    fun replyAllButtonHiddenWithoutRecipientSelection() {
        // FaunaMls default caps — reply-all collapses to plain reply.
        render(detail(messages = listOf(msg("m1"))))
        composeTestRule.onAllNodesWithTag("dm-reply-all-button").assertCountEquals(0)
    }

    @Test
    fun replyAllButtonFiresWithMessageId() {
        var repliedAll: String? = null
        render(
            detail(capabilities = caps(recipientSelection = true), messages = listOf(msg("m1"))),
            onReplyAll = { repliedAll = it },
        )
        composeTestRule.onNodeWithTag("dm-reply-all-button").performClick()
        assertEquals("m1", repliedAll)
    }

    // ── Reactions + message delete (conversations.md § Reactions & message
    // delete). Capability-gated, never rail-branched; mirrors the windows
    // ConversationsReactionsTests + linux message_bubble render legs. ──────────

    @Test
    fun actionsButtonShownWhenReactionsSupported() {
        // FaunaMls (supports_reactions): the ⋯ dm-message-actions-button shows.
        render(detail(capabilities = caps(reactions = true), messages = listOf(msg("m1"))))
        composeTestRule.onNodeWithTag("dm-message-actions-button").assertExists()
    }

    @Test
    fun actionsButtonHiddenWhenNoActions() {
        // SMTP-like thread: no reactions, no delete → ⋯ button absent entirely,
        // even on an own message (mail bubbles stay clean).
        render(
            detail(
                capabilities = caps(reactions = false, messageDelete = false),
                messages = listOf(msg("m1", isOwn = true)),
            ),
        )
        composeTestRule.onAllNodesWithTag("dm-message-actions-button").assertCountEquals(0)
    }

    @Test
    fun actionsButtonShownForOwnDeletableMessageWithoutReactions() {
        // delete-only capability + own message → the ⋯ button still appears
        // (the gate is supports_reactions OR (supports_message_delete && is_own)).
        render(
            detail(
                capabilities = caps(reactions = false, messageDelete = true),
                messages = listOf(msg("m1", isOwn = true)),
            ),
        )
        composeTestRule.onNodeWithTag("dm-message-actions-button").assertExists()
    }

    @Test
    fun actionsMenuShowsSixQuickSetOptions() {
        render(detail(capabilities = caps(reactions = true), messages = listOf(msg("m1"))))
        composeTestRule.onNodeWithTag("dm-message-actions-button").performClick()
        composeTestRule.onAllNodesWithTag("dm-reaction-option").assertCountEquals(6)
        composeTestRule.onNodeWithTag("dm-reaction-more-button").assertExists()
    }

    @Test
    fun quickSetRowIsTheSharedFaceInOrder() {
        // The quick-set is ONE definition — `fauna_conversations::QUICKSET_EMOJIS`, read here
        // through its `quicksetEmojis()` UniFFI face. This asserts the rendered row IS that
        // list in that order, rather than a re-typed copy that merely agrees today (the copy
        // this row deleted). `dm-reaction-option` is an indexed ui.yaml element, so an e2e
        // tapping index i is asserting index i OF THIS LIST on all 7 apps.
        val shared = quicksetEmojis()
        render(detail(capabilities = caps(reactions = true), messages = listOf(msg("m1"))))
        composeTestRule.onNodeWithTag("dm-message-actions-button").performClick()
        val options = composeTestRule.onAllNodesWithTag("dm-reaction-option")
        assertEquals(shared.size, options.fetchSemanticsNodes().size)
        shared.forEachIndexed { i, emoji -> options[i].assertTextEquals(emoji) }
    }

    @Test
    fun quickSetCrossesTheFfiByteIdentically() {
        // What the Rust-side test cannot see: the emoji survive the binding hop unchanged.
        // "❤️" is U+2764 followed by variation selector U+FE0F — a lossy hop drops the
        // selector and still *looks* right in a diff, while painting a monochrome heart and
        // comparing unequal against the emoji a peer actually reacted with. Asserted as code
        // units rather than a literal so the pin cannot be "fixed" by pasting the bad value.
        val heart = quicksetEmojis()[1]
        assertEquals(2, heart.length)
        assertEquals(0x2764, heart[0].code)
        assertEquals(0xFE0F, heart[1].code)
    }

    @Test
    fun quickSetOptionFiresToggleReaction() {
        // The first quick-set emoji is 👍 (the fixed shared order).
        var toggled: Pair<String, String>? = null
        render(
            detail(capabilities = caps(reactions = true), messages = listOf(msg("react-me"))),
            onToggleReaction = { id, emoji -> toggled = id to emoji },
        )
        composeTestRule.onNodeWithTag("dm-message-actions-button").performClick()
        composeTestRule.onAllNodesWithTag("dm-reaction-option").onFirst().performClick()
        assertEquals("react-me" to "👍", toggled)
    }

    @Test
    fun moreReactionButtonFiresWithMessageId() {
        var moreFor: String? = null
        render(
            detail(capabilities = caps(reactions = true), messages = listOf(msg("more-me"))),
            onMoreReaction = { moreFor = it },
        )
        composeTestRule.onNodeWithTag("dm-message-actions-button").performClick()
        composeTestRule.onNodeWithTag("dm-reaction-more-button").performClick()
        assertEquals("more-me", moreFor)
    }

    @Test
    fun deleteButtonShownOnOwnMessage() {
        render(
            detail(
                capabilities = caps(reactions = true, messageDelete = true),
                messages = listOf(msg("m1", isOwn = true)),
            ),
        )
        composeTestRule.onNodeWithTag("dm-message-actions-button").performClick()
        composeTestRule.onNodeWithTag("dm-message-delete-button").assertExists()
    }

    @Test
    fun deleteButtonAbsentOnPeerMessage() {
        // is_own = false → the ⋯ button still shows (supports_reactions), but the
        // delete option must be absent (sender-only).
        render(
            detail(
                capabilities = caps(reactions = true, messageDelete = true),
                messages = listOf(msg("m1", isOwn = false)),
            ),
        )
        composeTestRule.onNodeWithTag("dm-message-actions-button").performClick()
        composeTestRule.onAllNodesWithTag("dm-reaction-option").assertCountEquals(6)
        composeTestRule.onAllNodesWithTag("dm-message-delete-button").assertCountEquals(0)
    }

    @Test
    fun deleteConfirmShownAfterDeleteClick() {
        // Two-step destructive delete inside the flyout: delete → confirm.
        render(
            detail(
                capabilities = caps(reactions = true, messageDelete = true),
                messages = listOf(msg("m1", isOwn = true)),
            ),
        )
        composeTestRule.onNodeWithTag("dm-message-actions-button").performClick()
        composeTestRule.onNodeWithTag("dm-message-delete-button").performClick()
        composeTestRule.onNodeWithTag("dm-message-delete-confirm-button").assertExists()
        composeTestRule.onAllNodesWithTag("dm-message-delete-button").assertCountEquals(0)
    }

    @Test
    fun deleteConfirmFiresCallback() {
        var deleted: String? = null
        render(
            detail(
                capabilities = caps(reactions = true, messageDelete = true),
                messages = listOf(msg("del-me", isOwn = true)),
            ),
            onDeleteMessage = { deleted = it },
        )
        composeTestRule.onNodeWithTag("dm-message-actions-button").performClick()
        composeTestRule.onNodeWithTag("dm-message-delete-button").performClick()
        composeTestRule.onNodeWithTag("dm-message-delete-confirm-button").performClick()
        assertEquals("del-me", deleted)
    }

    @Test
    fun markAsSpamShownOnReceivedMessage() {
        // The received-only mark-as-spam gesture (!is_own) — the live `Insert`
        // consumer (mail-spam.md § Wire shapes). It appears even on a clean thread
        // with no reactions/delete, since it silently no-ops when mail isn't
        // enabled; so the ⋯ button itself now shows for a received message.
        render(
            detail(
                capabilities = caps(reactions = false, messageDelete = false),
                messages = listOf(msg("m1", isOwn = false)),
            ),
        )
        composeTestRule.onNodeWithTag("dm-message-actions-button").performClick()
        composeTestRule.onNodeWithTag("dm-message-mark-as-spam-button").assertExists()
    }

    @Test
    fun markAsSpamAbsentOnOwnMessage() {
        // You flag others' content as spam, not your own — absent on is_own.
        render(
            detail(
                capabilities = caps(reactions = true, messageDelete = true),
                messages = listOf(msg("m1", isOwn = true)),
            ),
        )
        composeTestRule.onNodeWithTag("dm-message-actions-button").performClick()
        composeTestRule.onAllNodesWithTag("dm-message-mark-as-spam-button").assertCountEquals(0)
    }

    @Test
    fun markAsSpamFiresCallbackWithMessage() {
        // The whole MessageSnapshot lifts to the VM (it needs the retained decrypted
        // body + subject + opaque message id for train_spam_model_client_mail).
        var flagged: MessageSnapshot? = null
        render(
            detail(
                capabilities = caps(reactions = false, messageDelete = false),
                messages = listOf(msg("spam-me", isOwn = false, body = "cheap pills")),
            ),
            onMarkSpam = { flagged = it },
        )
        composeTestRule.onNodeWithTag("dm-message-actions-button").performClick()
        composeTestRule.onNodeWithTag("dm-message-mark-as-spam-button").performClick()
        assertEquals("spam-me", flagged?.messageId)
        assertEquals("cheap pills", flagged?.body)
    }

    @Test
    fun reactionPillsRenderFromSnapshot() {
        render(
            detail(
                capabilities = caps(reactions = true),
                messages = listOf(
                    msg(
                        "m1",
                        reactions = listOf(
                            ReactionGroup(emoji = "👍", count = 2u, reactedByMe = true),
                            ReactionGroup(emoji = "❤️", count = 1u, reactedByMe = false),
                        ),
                    ),
                ),
            ),
        )
        assertEquals(
            2,
            composeTestRule.onAllNodesWithTag("dm-reaction-pill").fetchSemanticsNodes().size,
        )
    }

    @Test
    fun reactionPillsAbsentWhenNoReactions() {
        render(detail(capabilities = caps(reactions = true), messages = listOf(msg("m1"))))
        composeTestRule.onAllNodesWithTag("dm-reaction-pill").assertCountEquals(0)
    }

    @Test
    fun reactionPillTapFiresToggle() {
        var toggled: Pair<String, String>? = null
        render(
            detail(
                capabilities = caps(reactions = true),
                messages = listOf(
                    msg("pill-me", reactions = listOf(ReactionGroup(emoji = "🙏", count = 1u, reactedByMe = true))),
                ),
            ),
            onToggleReaction = { id, emoji -> toggled = id to emoji },
        )
        composeTestRule.onNodeWithTag("dm-reaction-pill").performClick()
        assertEquals("pill-me" to "🙏", toggled)
    }

    @Test
    fun deletedMessageShowsTombstoneAndStripsBody() {
        // A deleted message renders only the dm-message-deleted placeholder —
        // body / actions / reactions all stripped (conversations.md tombstone).
        render(
            detail(
                capabilities = caps(reactions = true, messageDelete = true),
                messages = listOf(msg("m1", isOwn = true, deleted = true)),
            ),
        )
        composeTestRule.onNodeWithTag("dm-message-deleted").assertExists()
        composeTestRule.onAllNodesWithTag("dm-message-text").assertCountEquals(0)
        composeTestRule.onAllNodesWithTag("dm-message-actions-button").assertCountEquals(0)
    }

    // ── The selected message (conversations.md § The selected message —
    // `SearchNav::Mail`'s second half: "open the thread AND select this message
    // in it"). The automation observable is a `selected` boolean on
    // `dm-message-timestamp` — Compose's own semantics property — always
    // present (never omitted when false, testing.md point 6) and painted on
    // EVERY render arm, tombstones included, mirroring linux's
    // `message_timestamp_label` / tui's `message_timestamp_element`. ──────────

    @Test
    fun selectedMessageTimestampCarriesSelectedTrue() {
        render(detail(messages = listOf(msg("m1")), selectedMessageId = "m1"))
        composeTestRule.onNodeWithTag("dm-message-timestamp").assertIsSelected()
    }

    @Test
    fun nonSelectedMessageTimestampCarriesSelectedFalse() {
        // selectedMessageId names a DIFFERENT message than the one rendered — the
        // attribute must be explicitly "false", never simply absent.
        render(detail(messages = listOf(msg("m1")), selectedMessageId = "some-other-message"))
        composeTestRule.onNodeWithTag("dm-message-timestamp").assertIsNotSelected()
    }

    @Test
    fun noSelectionLeavesEveryTimestampUnselected() {
        // The ordinary-navigation case (no search hit involved): selectedMessageId
        // is null, so every message's timestamp reads selected=false.
        render(detail(messages = listOf(msg("m1"), msg("m2")), selectedMessageId = null))
        val timestamps = composeTestRule.onAllNodesWithTag("dm-message-timestamp")
        timestamps.assertCountEquals(2)
        timestamps[0].assertIsNotSelected()
        timestamps[1].assertIsNotSelected()
    }

    @Test
    fun onlyTheNamedMessageIsSelectedAmongSiblings() {
        render(
            detail(
                messages = listOf(msg("m1"), msg("m2")),
                selectedMessageId = "m2",
            ),
        )
        val timestamps = composeTestRule.onAllNodesWithTag("dm-message-timestamp")
        timestamps[0].assertIsNotSelected()
        timestamps[1].assertIsSelected()
    }

    @Test
    fun deletedMessageStillCarriesSelectedTimestamp() {
        // The gap linux just fixed (conversations.md § The selected message): a
        // tombstone arm previously painted NO timestamp at all, so a mail search
        // hit landing on a since-deleted message had nowhere to mark. Proves the
        // fix — dm-message-timestamp exists (selected=true) even though
        // dm-message-text is absent from a deleted bubble.
        render(detail(messages = listOf(msg("m1", deleted = true)), selectedMessageId = "m1"))
        composeTestRule.onNodeWithTag("dm-message-deleted").assertExists()
        composeTestRule.onAllNodesWithTag("dm-message-text").assertCountEquals(0)
        composeTestRule.onNodeWithTag("dm-message-timestamp").assertIsSelected()
    }

    @Test
    fun deletedNonSelectedMessageTimestampCarriesSelectedFalse() {
        render(detail(messages = listOf(msg("m1", deleted = true)), selectedMessageId = null))
        composeTestRule.onNodeWithTag("dm-message-timestamp").assertIsNotSelected()
    }

    // -- the offline gate's two conversation-plane commits ----------------
    //
    // Placed here rather than in OfflineGateTest because this file already owns
    // the ThreadDetail/AddParticipantState fixtures these need (the same reason
    // batch 10 put the folders gate cases in FoldersContentTest). A red-verify
    // of the gate must therefore name THIS class alongside OfflineGateTest and
    // FoldersContentTest, or these cases silently drop out of the split.

    private fun needsNest(): String =
        androidx.test.core.app.ApplicationProvider
            .getApplicationContext<android.content.Context>()
            .getString(com.fauna.app.R.string.common_needs_nest)

    @Test
    fun markAsSpamGatesWithNoNest_whileItsOverflowOpenerStaysLive() {
        // Training the sealed tier-1 model PUTs it to the mail bridge
        // (`fauna.bridges.put_spam_model`) and has deliberately no server-train
        // fallback -- the nest cannot read conversation content -- so with no
        // nest there is nothing this gesture can do.
        render(
            detail(
                capabilities = caps(reactions = false, messageDelete = false),
                messages = listOf(msg("spam-me", isOwn = false, body = "cheap pills")),
            ),
            connectionState = FfiConnectionState.DISCONNECTED,
        )
        // The OPENER stays live: a gated opener would make the item
        // unreachable rather than merely dead, which proves nothing.
        composeTestRule.onNodeWithTag("dm-message-actions-button")
            .assertIsEnabled().performClick()
        composeTestRule.onNodeWithTag("dm-message-mark-as-spam-button").assertIsNotEnabled()
    }

    @Test
    fun markAsSpamIsLiveWhenConnected() {
        render(
            detail(
                capabilities = caps(reactions = false, messageDelete = false),
                messages = listOf(msg("spam-me", isOwn = false, body = "cheap pills")),
            ),
            connectionState = FfiConnectionState.CONNECTED,
        )
        composeTestRule.onNodeWithTag("dm-message-actions-button").performClick()
        composeTestRule.onNodeWithTag("dm-message-mark-as-spam-button").assertIsEnabled()
    }

    @Test
    fun theAddParticipantConfirmGatesForAnInPlaceGroupAdd_whileItsCancelStaysLive() {
        // The in-place arm opens its add commit by FETCHING the newcomer's key
        // package (`fauna.conversations.keypackage.fetch`), so it cannot start
        // without a nest.
        render(
            detail(flavor = ThreadFlavor.MlsGroup),
            addParticipant = addParticipantState(inPlaceMlsGroup = true),
            connectionState = FfiConnectionState.DISCONNECTED,
        )
        composeTestRule.onNodeWithTag("add-participant-confirm").assertIsNotEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertIsDisplayed()
    }

    @Test
    fun theAddParticipantConfirmStaysLiveForAForkEvenWithNoNest() {
        // The converse half, and the one that discriminates: a FaunaMls 1:1
        // FORKS a new group (its first send bootstraps it) and a non-FaunaMls
        // rail has no wire membership op at all, so confirming issues NOTHING
        // and must survive the outage. A gate applied to this arm too would be
        // the over-claim the contract forbids -- and would read as coverage.
        render(
            detail(),
            addParticipant = addParticipantState(inPlaceMlsGroup = false),
            connectionState = FfiConnectionState.DISCONNECTED,
        )
        composeTestRule.onNodeWithTag("add-participant-confirm").assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Test
    fun theAddParticipantConfirmIsLiveWhenConnected() {
        render(
            detail(flavor = ThreadFlavor.MlsGroup),
            addParticipant = addParticipantState(inPlaceMlsGroup = true),
            connectionState = FfiConnectionState.CONNECTED,
        )
        composeTestRule.onNodeWithTag("add-participant-confirm").assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }
}
