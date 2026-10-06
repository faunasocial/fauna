package com.fauna.app.ui.screen.settings

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_client_mail_settings.ImportSourceKind
import uniffi.fauna_client_mail_settings.ImportStatus
import uniffi.fauna_client_mail_settings.ImportStep
import uniffi.fauna_client_mail_settings.ImportTlsMode
import uniffi.fauna_client_mail_settings.MailImportSnapshot
import uniffi.fauna_client_mail_settings.SourceMailboxOption

/**
 * Compose-level coverage for the stateless [MailImportContent] (the `mail-import`
 * wizard, mailbox-migration.md): the five conditional steps share one element
 * set, shown by `snapshot.step`. Seeded state — no Hilt, no VM.
 *
 * Beyond the export twin's step-by-step existence checks, this pins the two
 * things that are genuinely this page's own and would otherwise be believed
 * rather than known: the per-provider Source-field table, and that the Confirm
 * summary counts only the mailboxes still in scope.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class MailImportContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun snap(
        step: ImportStep,
        sourceKind: ImportSourceKind = ImportSourceKind.GENERIC,
        host: String = "",
        mailboxes: List<SourceMailboxOption> = emptyList(),
        // u64 on the snapshot, unlike the export twin's u32 counts.
        totalCount: ULong = 0uL,
        importedCount: ULong = 0uL,
    ) = MailImportSnapshot(
        step = step,
        sourceKind = sourceKind,
        host = host,
        port = 993u,
        tlsMode = ImportTlsMode.IMPLICIT,
        username = "",
        password = "",
        mailboxes = mailboxes,
        dateFrom = "",
        maxSizeBytes = 52428800uL,
        sessionState = null,
        importedCount = importedCount,
        skippedCount = 0u,
        erroredCount = 0u,
        totalCount = totalCount,
        errorLog = emptyList(),
        status = ImportStatus.IDLE,
        error = null,
    )

    private fun render(
        snapshot: MailImportSnapshot,
        onConnect: (ImportSourceKind, String, String, String, String) -> Unit = { _, _, _, _, _ -> },
        onScopeNext: (String, String) -> Unit = { _, _ -> },
        onStart: () -> Unit = {},
    ) {
        composeTestRule.setContent {
            MailImportContent(
                snapshot = snapshot,
                // Pure FFI-free stubs for the two shared label maps.
                sourceKindLabel = { it.name },
                tlsModeLabel = { it.name },
                onBack = {},
                onSelectSourceKind = {},
                onSetTlsMode = {},
                onConnect = onConnect,
                onToggleMailbox = {},
                onScopeNext = onScopeNext,
                onPrev = {},
                onStart = onStart,
                onPause = {},
                onResume = {},
                onCancel = {},
                onViewImported = {},
                onReviewSkipped = {},
            )
        }
    }

    @Test
    fun sourceStepShowsPickerAndConnect() {
        render(snap(ImportStep.SOURCE))
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("mail-import-source-picker").assertExists()
        composeTestRule.onNodeWithTag("mail-import-connect-button").assertExists()
    }

    /**
     * Every Source-step field id, so a per-provider assertion can state the
     * WHOLE table — what the provider shows and, just as importantly, what it
     * must not. `source-username` is excluded: it is the one field painted for
     * every kind (nothing in ui.yaml scopes it the way the other four are
     * annotated), so it is asserted separately below.
     */
    private val conditionalSourceFields = listOf(
        "mail-import-source-app-password",
        "mail-import-source-oauth-button",
        "mail-import-source-host",
        "mail-import-source-port",
        "mail-import-source-tls-mode",
        "mail-import-source-password",
    )

    /**
     * Assert the per-provider field table for one kind (`mailbox-migration.md`
     * § Wizard steps step 1, as transcribed by the tui lead).
     *
     * One provider per test, because `setContent` may be called only ONCE per
     * Compose test — a single test looping over the four kinds throws
     * IllegalStateException on its second render, which is exactly how the
     * first version of this file failed.
     */
    private fun assertSourceTable(kind: ImportSourceKind, shown: Set<String>) {
        render(snap(ImportStep.SOURCE, sourceKind = kind))
        composeTestRule.onNodeWithTag("mail-import-source-username").assertExists()
        for (id in conditionalSourceFields) {
            if (id in shown) {
                composeTestRule.onNodeWithTag(id).assertExists()
            } else {
                composeTestRule.onNodeWithTag(id).assertDoesNotExist()
            }
        }
    }

    private val imapFallback = setOf(
        "mail-import-source-host",
        "mail-import-source-port",
        "mail-import-source-tls-mode",
        "mail-import-source-password",
    )

    @Test
    fun gmailShowsTheAppPasswordAndNoImapFields() {
        assertSourceTable(ImportSourceKind.GMAIL, setOf("mail-import-source-app-password"))
    }

    @Test
    fun icloudShowsTheAppPasswordAndNoImapFields() {
        assertSourceTable(ImportSourceKind.I_CLOUD, setOf("mail-import-source-app-password"))
    }

    /** Outlook is the one kind with BOTH: the OAuth button and the IMAP fallback. */
    @Test
    fun outlookShowsTheOauthButtonAndTheImapFallback() {
        assertSourceTable(
            ImportSourceKind.OUTLOOK,
            imapFallback + "mail-import-source-oauth-button",
        )
    }

    @Test
    fun genericShowsTheImapFieldsAlone() {
        assertSourceTable(ImportSourceKind.GENERIC, imapFallback)
    }

    /**
     * The Source form commits at the transition, not per keystroke — so the tap
     * must carry the typed values out. Typing then tapping Connect is exactly
     * the sequence a user (and `test_mail_import.py`) performs.
     */
    @Test
    fun connectCarriesTheTypedSourceFormOut() {
        var seen: List<String>? = null
        render(
            snap(ImportStep.SOURCE, sourceKind = ImportSourceKind.GENERIC),
            onConnect = { kind, host, port, user, pass ->
                seen = listOf(kind.name, host, port, user, pass)
            },
        )
        composeTestRule.onNodeWithTag("mail-import-source-host").performScrollTo()
            .performTextInput("imap.example.org")
        composeTestRule.onNodeWithTag("mail-import-source-port").performScrollTo()
            .performTextInput("993")
        composeTestRule.onNodeWithTag("mail-import-source-username").performScrollTo()
            .performTextInput("someone")
        composeTestRule.onNodeWithTag("mail-import-source-password").performScrollTo()
            .performTextInput("hunter2")
        composeTestRule.onNodeWithTag("mail-import-connect-button").performScrollTo().performClick()

        assert(seen == listOf("GENERIC", "imap.example.org", "993", "someone", "hunter2")) {
            "Connect must carry the whole typed form out in one call; got $seen"
        }
    }

    @Test
    fun scopeStepShowsControlsAndTheSharedWizardNav() {
        render(
            snap(
                ImportStep.SCOPE,
                mailboxes = listOf(
                    SourceMailboxOption(name = "INBOX", selected = true, messageCount = 7u),
                ),
            ),
        )
        composeTestRule.onNodeWithTag("mail-import-scope-mailboxes").assertExists()
        composeTestRule.onNodeWithTag("mail-import-scope-mailbox-item").assertExists()
        composeTestRule.onNodeWithTag("mail-import-scope-date-from").assertExists()
        composeTestRule.onNodeWithTag("mail-import-scope-max-size").assertExists()
        composeTestRule.onNodeWithTag("mail-import-scope-mailbox-mapping").assertExists()
        // The SHARED wizard nav ids, not a mail-import-* pair of its own.
        composeTestRule.onNodeWithTag("wizard-back-button").assertExists()
        composeTestRule.onNodeWithTag("wizard-next-button").assertExists()
    }

    /**
     * The Confirm summary counts only what is still in scope. Asserted with a
     * deselected mailbox present, because "count the selected ones" and "count
     * them all" agree on every all-selected fixture — which is exactly how a
     * summary that silently ignores the user's deselection would pass.
     */
    @Test
    fun confirmSummaryCountsOnlyTheSelectedMailboxes() {
        var started = false
        render(
            snap(
                ImportStep.CONFIRM,
                mailboxes = listOf(
                    SourceMailboxOption(name = "INBOX", selected = true, messageCount = 7u),
                    SourceMailboxOption(name = "Archive", selected = false, messageCount = 99u),
                ),
            ),
            onStart = { started = true },
        )
        composeTestRule.onNodeWithTag("mail-import-confirm-summary")
            .assertExists()
            .assertTextContains("1 mailbox(es)", substring = true)
        composeTestRule.onNodeWithTag("mail-import-confirm-summary")
            .assertTextContains("7 messages", substring = true)
        composeTestRule.onNodeWithTag("mail-import-start-button").performScrollTo().performClick()
        assert(started)
    }

    @Test
    fun progressStepShowsBarAndControls() {
        render(snap(ImportStep.PROGRESS, totalCount = 10uL, importedCount = 3uL))
        composeTestRule.onNodeWithTag("mail-import-progress-summary").assertExists()
        composeTestRule.onNodeWithTag("mail-import-progress-bar").assertExists()
        composeTestRule.onNodeWithTag("mail-import-error-log").assertExists()
        composeTestRule.onNodeWithTag("mail-import-pause-button").assertExists()
        composeTestRule.onNodeWithTag("mail-import-resume-button").assertExists()
        composeTestRule.onNodeWithTag("mail-import-cancel-button").assertExists()
    }

    @Test
    fun doneStepShowsSummaryAndBothDeepLinks() {
        render(snap(ImportStep.DONE, importedCount = 12uL))
        composeTestRule.onNodeWithTag("mail-import-done-summary").assertExists()
        composeTestRule.onNodeWithTag("mail-import-view-imported-button").assertExists()
        composeTestRule.onNodeWithTag("mail-import-review-skipped-button").assertExists()
    }

    /**
     * Nothing re-seeded the Source-step host/port drafts from the snapshot a
     * `SelectSourceKind` dispatch just wrote, so picking Outlook left the host
     * field blank and Connect dialed nothing.
     *
     * `setContent` runs once per test (this file's own rule), so the picked
     * kind's re-published snapshot is faked here exactly the way
     * [MailImportVM.selectSourceKind] really produces it: `onSelectSourceKind`
     * flips a local `mutableStateOf` snapshot to one already carrying the
     * provider preset's host/port (`apply_client_action`'s `SelectSourceKind`
     * arm, shared Rust — never re-derived on this side).
     */
    @Test
    fun selectingASourceKindSeedsTheHostAndPortDrafts() {
        var snapshot by mutableStateOf(snap(ImportStep.SOURCE, sourceKind = ImportSourceKind.GMAIL))
        composeTestRule.setContent {
            MailImportContent(
                snapshot = snapshot,
                sourceKindLabel = { it.name },
                tlsModeLabel = { it.name },
                onBack = {},
                onSelectSourceKind = {
                    snapshot = snap(
                        ImportStep.SOURCE,
                        sourceKind = ImportSourceKind.OUTLOOK,
                        host = "outlook.office365.com",
                    )
                },
                onSetTlsMode = {},
                onConnect = { _, _, _, _, _ -> },
                onToggleMailbox = {},
                onScopeNext = { _, _ -> },
                onPrev = {},
                onStart = {},
                onPause = {},
                onResume = {},
                onCancel = {},
                onViewImported = {},
                onReviewSkipped = {},
            )
        }

        // `SourceOption`'s Row does not merge its `RadioButton`'s click action with
        // the label `Text` beside it, and neither Row nor the enclosing Column emits
        // its own semantics node, so the four (RadioButton, Text) pairs flatten into
        // one sibling list — `onNodeWithText("OUTLOOK")`'s nearest semantics parent is
        // ambiguous over all four RadioButtons, not just its own row's. Select by
        // position instead: Outlook is the second declared option (index 1, the same
        // order `SourceOption` is called in above).
        composeTestRule.onAllNodes(isSelectable())[1].performClick()

        composeTestRule.onNodeWithTag("mail-import-source-host")
            .assertTextContains("outlook.office365.com", substring = true)
        composeTestRule.onNodeWithTag("mail-import-source-port")
            .assertTextContains("993", substring = true)
    }
}
