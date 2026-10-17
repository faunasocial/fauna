package com.fauna.app.ui.screen.conversations

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_conversations.ComposeState
import uniffi.fauna_conversations.RecipientPickerState
import uniffi.fauna_conversations.ResolveState
import uniffi.fauna_conversations.SendState
import uniffi.fauna_conversations.TypedAddress
import uniffi.fauna_core.LocalizedText

/**
 * Compose-level coverage for the stateless [NewThreadComposeContent] (the
 * new-thread compose screen, `docs/goal/ui/conversations.md` §"New-thread
 * compose lives in the detail pane"). Renders with seeded [ComposeState] /
 * [RecipientPickerState] records — no Hilt, no VM, no FFI — so the recipient
 * picker, group-conversation hint, compose bar, and `error-message` surface are
 * exercised on the JVM. (Android E2E `test_recipient_picker.py` /
 * `test_conversations_compose_error.py --client android` is the standing gate
 * once the host emulator lands.)
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class NewThreadComposeContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun picker(
        rawInput: String = "",
        chips: List<TypedAddress> = emptyList(),
        suggestions: List<TypedAddress> = emptyList(),
        suggestionDisplays: List<String> = emptyList(),
        resolveState: ResolveState = ResolveState.IDLE,
        resolved: TypedAddress? = null,
    ) = RecipientPickerState(
        rawInput = rawInput,
        chips = chips,
        suggestions = suggestions,
        suggestionDisplays = suggestionDisplays,
        resolveState = resolveState,
        resolved = resolved,
    )

    private fun compose(
        bodyDraft: String = "",
        subjectDraft: String? = null,
        recipientPicker: RecipientPickerState? = picker(),
        sendState: SendState = SendState.Idle,
    ) = ComposeState(
        bodyDraft = bodyDraft,
        subjectDraft = subjectDraft,
        attachments = emptyList(),
        replyTo = null,
        replyRecipients = emptyList(),
        recipientPicker = recipientPicker,
        sendState = sendState,
    )

    private fun email(addr: String): TypedAddress = TypedAddress.Email(addr)

    // The shape `SendState::failed` produces in shared Rust: one key for every
    // send failure, the backend's own detail as `{message}`.
    private fun sendFailure(detail: String) = LocalizedText(
        key = "conversations.unified.error_send",
        args = mapOf("message" to detail),
    )

    private fun render(
        compose: ComposeState?,
        onBack: () -> Unit = {},
        onDiscard: () -> Unit = {},
        onRecipientInputChange: (String) -> Unit = {},
        onAcceptRecipient: () -> Unit = {},
        onAcceptSuggestion: (Int) -> Unit = {},
        onRemoveRecipientChip: (Int) -> Unit = {},
        onBodyChange: (String) -> Unit = {},
        onSubjectChange: (String) -> Unit = {},
        onTopicToggle: () -> Unit = {},
        onSend: () -> Unit = {},
        onAttach: () -> Unit = {},
        onReplyCancel: () -> Unit = {},
        onHomeNestToggle: (Boolean) -> Unit = {},
    ) {
        composeTestRule.setContent {
            NewThreadComposeContent(
                compose = compose,
                onBack = onBack,
                onDiscard = onDiscard,
                onRecipientInputChange = onRecipientInputChange,
                onAcceptRecipient = onAcceptRecipient,
                onAcceptSuggestion = onAcceptSuggestion,
                onRemoveRecipientChip = onRemoveRecipientChip,
                onBodyChange = onBodyChange,
                onSubjectChange = onSubjectChange,
                onTopicToggle = onTopicToggle,
                onSend = onSend,
                onAttach = onAttach,
                onReplyCancel = onReplyCancel,
                onHomeNestToggle = onHomeNestToggle,
            )
        }
    }

    @Test
    fun homeNestTogglePaintsBeforeAnyChipAndReportsTheValueAsked() {
        val asked = mutableListOf<Boolean>()
        render(compose(), onHomeNestToggle = { asked += it })
        composeTestRule.onNodeWithTag("recipient-picker-home-nest-toggle").assertExists()
        assertEquals(
            "false",
            composeTestRule.onNodeWithTag("recipient-picker-home-nest-toggle").fetchSemanticsNode()
                .config[androidx.compose.ui.semantics.SemanticsProperties.StateDescription],
        )
        composeTestRule.onNodeWithTag("recipient-picker-home-nest-toggle").performClick()
        assertEquals(listOf(true), asked)
    }

    @Test
    fun recipientPickerAndComposeBarRender() {
        render(compose())
        composeTestRule.onNodeWithTag("recipient-picker-input").assertExists()
        composeTestRule.onNodeWithTag("recipient-resolve-status").assertExists()
        composeTestRule.onNodeWithTag("dm-text-field").assertExists()
        composeTestRule.onNodeWithTag("dm-send-button").assertExists()
        composeTestRule.onNodeWithTag("topic-toggle-button").assertExists()
        composeTestRule.onNodeWithTag("attachment-button").assertExists()
    }

    @Test
    fun chipsRenderPerRecipient() {
        render(compose(recipientPicker = picker(chips = listOf(email("a@x.test"), email("b@x.test")))))
        assertEquals(
            2,
            composeTestRule.onAllNodesWithTag("recipient-picker-chip").fetchSemanticsNodes().size,
        )
    }

    @Test
    fun groupHintShownAtTwoOrMoreChips() {
        render(compose(recipientPicker = picker(chips = listOf(email("a@x.test"), email("b@x.test")))))
        composeTestRule.onNodeWithTag("group-conversation-hint").assertExists()
    }

    @Test
    fun groupHintHiddenWithOneChip() {
        render(compose(recipientPicker = picker(chips = listOf(email("a@x.test")))))
        composeTestRule.onAllNodesWithTag("group-conversation-hint").assertCountEquals(0)
    }

    @Test
    fun suggestionsRenderIndexed() {
        render(compose(recipientPicker = picker(suggestions = listOf(email("s1@x.test"), email("s2@x.test")))))
        assertEquals(
            2,
            composeTestRule.onAllNodesWithTag("recipient-picker-suggestion").fetchSemanticsNodes().size,
        )
    }

    @Test
    fun suggestionClickAcceptsThatRowNotTheTypedText() {
        val accepted = mutableListOf<Int>()
        var typedAccepts = 0
        render(
            compose(
                recipientPicker = picker(
                    suggestions = listOf(email("alice@x.test"), email("bob@x.test")),
                    suggestionDisplays = listOf("Alice", "bob@x.test"),
                ),
            ),
            onAcceptRecipient = { typedAccepts++ },
            onAcceptSuggestion = { accepted += it },
        )
        // The row reads the manager-derived label first, the address beside it
        // (the harness's default `displayAddress` is `toString`, so only the
        // label half is pinned here).
        composeTestRule.onAllNodesWithTag("recipient-picker-suggestion")[0]
            .assertTextContains("Alice \u00b7 ", substring = true)
        composeTestRule.onAllNodesWithTag("recipient-picker-suggestion")[1].performClick()
        assertEquals(listOf(1), accepted)
        assertEquals(0, typedAccepts)
    }

    @Test
    fun chipClickRemovesThatChip() {
        val removed = mutableListOf<Int>()
        render(
            compose(recipientPicker = picker(chips = listOf(email("a@x.test"), email("b@x.test")))),
            onRemoveRecipientChip = { removed += it },
        )
        composeTestRule.onAllNodesWithTag("recipient-picker-chip")[0].assertTextContains("a@x.test", substring = true)
        composeTestRule.onAllNodesWithTag("recipient-picker-chip")[0].assertTextContains(" ×", substring = true)
        composeTestRule.onAllNodesWithTag("recipient-picker-chip")[1].performClick()
        assertEquals(listOf(1), removed)
    }

    @Test
    fun subjectInputHiddenUntilTopicActive() {
        render(compose(subjectDraft = null))
        composeTestRule.onAllNodesWithTag("subject-input").assertCountEquals(0)
    }

    @Test
    fun subjectInputShownWhenTopicActive() {
        render(compose(subjectDraft = ""))
        composeTestRule.onNodeWithTag("subject-input").assertExists()
    }

    @Test
    fun errorMessageHiddenWhenNotFailed() {
        render(compose(sendState = SendState.Idle))
        composeTestRule.onAllNodesWithTag("error-message").assertCountEquals(0)
    }

    // The reason is a shared LocalizedText (key + `{message}`), so the screen
    // must resolve it: the backend detail reaches the user AND the key does not
    // survive to the surface. A verbatim-equality assertion could not tell a
    // resolved template from a painted raw key.
    @Test
    fun errorMessageRendersFailedReason() {
        render(compose(sendState = SendState.Failed(sendFailure("mail not provisioned"))))
        composeTestRule.onNodeWithTag("error-message")
            .assertExists()
            .assertTextContains("mail not provisioned", substring = true)
        composeTestRule.onNodeWithTag("error-message")
            .assertTextContains("Could not send", substring = true)
    }

    @Test
    fun recipientInputForwardsKeystrokes() {
        var typed: String? = null
        render(compose(), onRecipientInputChange = { typed = it })
        composeTestRule.onNodeWithTag("recipient-picker-input").performTextInput("alice@x.test")
        assertTrue(typed?.isNotEmpty() == true)
    }

    @Test
    fun sendButtonFiresCallback() {
        var sent = false
        render(compose(), onSend = { sent = true })
        composeTestRule.onNodeWithTag("dm-send-button").performClick()
        assertTrue(sent)
    }

    @Test
    fun nullComposeShowsHintNotPicker() {
        render(compose = null)
        composeTestRule.onAllNodesWithTag("recipient-picker-input").assertCountEquals(0)
        composeTestRule.onAllNodesWithTag("dm-send-button").assertCountEquals(0)
    }

    @Test
    fun cancelButtonRendersAndFiresDiscard() {
        // The explicit discard affordance (conversations.md § Persistence) — the
        // one path, alongside a successful send, that clears the new-thread draft.
        var discarded = false
        render(compose(bodyDraft = "scratch"), onDiscard = { discarded = true })
        composeTestRule.onNodeWithTag("new-conversation-cancel").assertExists().performClick()
        assertTrue(discarded)
    }
}
