package com.fauna.app.ui.screen.settings

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_client_mail_settings.ImportMailboxProgress
import uniffi.fauna_client_mail_settings.ImportPauseReason
import uniffi.fauna_client_mail_settings.ImportSessionState
import uniffi.fauna_client_mail_settings.ImportSourceKind
import uniffi.fauna_client_mail_settings.ImportStatus
import uniffi.fauna_client_mail_settings.ImportStep
import uniffi.fauna_client_mail_settings.ImportTlsMode
import uniffi.fauna_client_mail_settings.MailImportSnapshot
import uniffi.fauna_client_mail_settings.MicrosoftSignInState
import uniffi.fauna_client_mail_settings.MicrosoftSignInView
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
        // The session's clock; 0 = no session read yet (the untimed summaries).
        startedAt: Long = 0L,
        lastProgressAt: Long = 0L,
        mailboxProgress: List<ImportMailboxProgress> = emptyList(),
        errorLog: List<String> = emptyList(),
        resumeNeedsCredentials: Boolean = false,
        microsoftSigninAvailable: Boolean = false,
        microsoftSignin: MicrosoftSignInView? = null,
        sessionState: ImportSessionState? = null,
        pauseReason: ImportPauseReason? = null,
        rebuiltMailboxes: List<String> = emptyList(),
        sourceRemembered: Boolean = false,
        importDuplicates: Boolean = false,
        username: String = "",
    ) = MailImportSnapshot(
        step = step,
        sourceKind = sourceKind,
        host = host,
        port = 993u,
        tlsMode = ImportTlsMode.IMPLICIT,
        username = username,
        password = "",
        mailboxes = mailboxes,
        dateFrom = "",
        maxSizeBytes = 52428800uL,
        sessionState = sessionState,
        pauseReason = pauseReason,
        importedCount = importedCount,
        skippedCount = 0u,
        erroredCount = 0u,
        totalCount = totalCount,
        errorLog = errorLog,
        mailboxProgress = mailboxProgress,
        startedAt = startedAt,
        lastProgressAt = lastProgressAt,
        status = ImportStatus.IDLE,
        error = null,
        resumeNeedsCredentials = resumeNeedsCredentials,
        microsoftSigninAvailable = microsoftSigninAvailable,
        microsoftSignin = microsoftSignin,
        rebuiltMailboxes = rebuiltMailboxes,
        sourceRemembered = sourceRemembered,
        importDuplicates = importDuplicates,
    )

    private fun render(
        snapshot: MailImportSnapshot,
        onConnect: (ImportSourceKind, String, String, String, String) -> Unit = { _, _, _, _, _ -> },
        onScopeNext: (String, String) -> Unit = { _, _ -> },
        onStart: () -> Unit = {},
        onResume: (String, String) -> Unit = { _, _ -> },
        onSignInWithMicrosoft: (String) -> Unit = {},
        onRestart: () -> Unit = {},
        onDismiss: () -> Unit = {},
        onForgetSource: () -> Unit = {},
        onSetImportDuplicates: (Boolean) -> Unit = {},
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
                onResume = onResume,
                onCancel = {},
                onSkipRest = {},
                onRestart = onRestart,
                onDismiss = onDismiss,
                onForgetSource = onForgetSource,
                onSetImportDuplicates = onSetImportDuplicates,
                onViewImported = {},
                onSignInWithMicrosoft = onSignInWithMicrosoft,
            )
        }
    }

    /**
     * Review skipped (§ Wizard steps step 6, outcomes 11 and 33): the skip log
     * stays out of the Done card until the button is pressed, lists every line
     * of the snapshot's `errorLog` once it is, and a second press hides it.
     */
    @Test
    fun reviewSkippedRevealsAndHidesTheSkipLogOnTheDoneScreen() {
        render(
            snap(
                ImportStep.DONE,
                errorLog = listOf("INBOX uid 2: too large", "Archive uid 1: mailbox gone"),
            ),
        )
        composeTestRule.onNodeWithTag("mail-import-error-log").assertDoesNotExist()

        composeTestRule.onNodeWithTag("mail-import-review-skipped-button").performClick()
        composeTestRule.onNodeWithTag("mail-import-error-log").assertExists()
        composeTestRule.onNodeWithText("INBOX uid 2: too large").assertExists()
        composeTestRule.onNodeWithText("Archive uid 1: mailbox gone").assertExists()
        composeTestRule.onNodeWithTag("mail-import-done-summary").assertExists()

        composeTestRule.onNodeWithTag("mail-import-review-skipped-button").performClick()
        composeTestRule.onNodeWithTag("mail-import-error-log").assertDoesNotExist()
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

    /**
     * Outlook shows the address and Sign in with Microsoft — no password, no
     * host, no Connect: the IMAP fallback is withdrawn (§ Microsoft sign-in).
     */
    @Test
    fun outlookShowsTheAddressAndSignInWithMicrosoftAlone() {
        assertSourceTable(ImportSourceKind.OUTLOOK, setOf("mail-import-source-oauth-button"))
        composeTestRule.onNodeWithTag("mail-import-connect-button").assertDoesNotExist()
    }

    /** No registration: the button is inert beside the unavailable sentence. */
    @Test
    fun signInWithMicrosoftIsInertWhereTheRegistrationIsMissing() {
        render(snap(ImportStep.SOURCE, sourceKind = ImportSourceKind.OUTLOOK))
        composeTestRule.onNodeWithTag("mail-import-source-oauth-button").assertIsNotEnabled()
        composeTestRule.onNodeWithText("Microsoft sign-in is not available yet.").assertExists()
    }

    /** While Microsoft's code is out it paints with its attributes and the button waits. */
    @Test
    fun theMicrosoftCodePaintsWithItsAttributesWhileTheButtonWaits() {
        render(
            snap(
                ImportStep.SOURCE,
                sourceKind = ImportSourceKind.OUTLOOK,
                microsoftSigninAvailable = true,
                microsoftSignin = MicrosoftSignInView(
                    userCode = "FAUNA-42",
                    verificationUri = "https://microsoft.example/devicelogin",
                    expiresAtMs = 0uL,
                    state = MicrosoftSignInState.WAITING,
                ),
            ),
        )
        // In flight (the code is out): the button waits.
        composeTestRule.onNodeWithTag("mail-import-source-oauth-button").assertIsNotEnabled()
        val code = composeTestRule.onNodeWithTag("mail-import-source-oauth-code")
        code.assertExists()
        assertEquals(
            "code=FAUNA-42;url=https://microsoft.example/devicelogin",
            code.fetchSemanticsNode().config.getOrNull(SemanticsProperties.StateDescription),
        )
        composeTestRule.onNodeWithText("Open in browser").assertExists()
    }

    @Test
    fun signInWithMicrosoftPressCarriesTheTypedAddressOut() {
        var seen: String? = null
        render(
            snap(
                ImportStep.SOURCE,
                sourceKind = ImportSourceKind.OUTLOOK,
                microsoftSigninAvailable = true,
            ),
            onSignInWithMicrosoft = { seen = it },
        )
        composeTestRule.onNodeWithTag("mail-import-source-username").performScrollTo()
            .performTextInput("person@outlook.example")
        composeTestRule.onNodeWithTag("mail-import-source-oauth-button").performScrollTo().performClick()
        assert(seen == "person@outlook.example") { "the press must carry the address; got $seen" }
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

    /**
     * The cross-app journeys read two attributes off these nodes
     * (`tests/e2e-unified/tests/test_mail_import.py`): the progress bar's
     * `fraction` and the app-password field's `help`. Android serves both as the
     * node's bare `stateDescription`, the one value the bridge answers any name from.
     */
    @Test
    fun progressBarPublishesItsFractionAndAppPasswordFieldItsHelp() {
        render(snap(ImportStep.PROGRESS, totalCount = 10uL, importedCount = 3uL))
        composeTestRule.onNodeWithTag("mail-import-progress-bar")
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, "0.3000"))
    }

    /**
     * A session adopted after a restart (`resumeNeedsCredentials`) asks for the
     * Generic source's port (seeded from the session), connection security and
     * password beside Resume, and Resume carries the port and the typed
     * password out (`mailbox-migration.md` § Resume protocol step 2).
     */
    @Test
    fun anAdoptedSessionAsksForTheGenericCredentialBesideResume() {
        var resumed: Pair<String, String>? = null
        render(
            snap(ImportStep.PROGRESS, resumeNeedsCredentials = true),
            onResume = { port, password -> resumed = port to password },
        )
        composeTestRule.onNodeWithTag("mail-import-source-port").assertTextContains("993")
        composeTestRule.onNodeWithTag("mail-import-source-tls-mode").assertExists()
        composeTestRule.onNodeWithTag("mail-import-source-password").performScrollTo().performTextInput("hunter2")
        composeTestRule.onNodeWithTag("mail-import-resume-button").performScrollTo().performClick()
        assertEquals("993" to "hunter2", resumed)
    }

    /** Without the flag the Progress step paints no credential field. */
    @Test
    fun aSessionThisAppStartedPaintsNoResumeCredential() {
        render(snap(ImportStep.PROGRESS))
        composeTestRule.onNodeWithTag("mail-import-source-password").assertDoesNotExist()
        composeTestRule.onNodeWithTag("mail-import-source-port").assertDoesNotExist()
    }

    /** One row per mailbox the run takes, worded from the machine's own counts. */
    @Test
    fun progressListPaintsEachMailboxFromTheMachinesCounts() {
        render(
            snap(
                ImportStep.PROGRESS,
                totalCount = 5uL,
                mailboxProgress = listOf(
                    ImportMailboxProgress(name = "INBOX", imported = 2uL, skipped = 1uL, errored = 0uL, total = 3uL),
                    ImportMailboxProgress(name = "Archive", imported = 0uL, skipped = 0uL, errored = 0uL, total = 2uL),
                ),
            ),
        )
        composeTestRule.onAllNodesWithTag("mail-import-mailbox-progress-list-item").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("mail-import-mailbox-progress-list-item-progress")[0]
            .assertTextContains("2 of 3 · 1 skipped · 0 errored")
        composeTestRule.onAllNodesWithTag("mail-import-mailbox-progress-list-item-progress")[1]
            .assertTextContains("0 of 2 · 0 skipped · 0 errored")
    }

    @Test
    fun appPasswordFieldCarriesTheProvidersExplanationAsItsHelp() {
        render(snap(ImportStep.SOURCE, sourceKind = ImportSourceKind.GMAIL))
        composeTestRule.onNodeWithTag("mail-import-source-app-password")
            .assert(
                SemanticsMatcher("help names the provider's app-password page") {
                    it.config.getOrNull(SemanticsProperties.StateDescription)
                        ?.contains("myaccount.google.com/apppasswords") == true
                },
            )
    }

    /**
     * A finished session (not running, so its clock is frozen at the last
     * transition) reports how long it took on the Done card, and the progress
     * summary carries the elapsed time — both from the shared `transfer_timing`.
     */
    @Test
    fun doneAndProgressSummariesReportTheElapsedTime() {
        render(snap(ImportStep.DONE, importedCount = 12uL, startedAt = 1_000L, lastProgressAt = 1_090L))
        composeTestRule.onNodeWithTag("mail-import-done-summary")
            .assertTextContains("took", substring = true)
    }

    @Test
    fun progressSummaryAddsElapsedTime() {
        render(
            snap(
                ImportStep.PROGRESS, totalCount = 10uL, importedCount = 3uL,
                startedAt = 1_000L, lastProgressAt = 1_090L,
            ),
        )
        composeTestRule.onNodeWithTag("mail-import-progress-summary")
            .assertTextContains("elapsed", substring = true)
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
     * `SelectSourceKind` dispatch just wrote, so picking a kind left the host
     * field blank and Connect dialed nothing. Generic is the one
     * kind that shows the host field now.
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
                        sourceKind = ImportSourceKind.GENERIC,
                        host = "imap.example.org",
                    )
                },
                onSetTlsMode = {},
                onConnect = { _, _, _, _, _ -> },
                onToggleMailbox = {},
                onScopeNext = { _, _ -> },
                onPrev = {},
                onStart = {},
                onPause = {},
                onResume = { _, _ -> },
                onCancel = {},
                onSkipRest = {},
                onRestart = {},
                onDismiss = {},
                onForgetSource = {},
                onSetImportDuplicates = {},
                onViewImported = {},
                onSignInWithMicrosoft = {},
            )
        }

        // `SourceOption`'s Row does not merge its `RadioButton`'s click action with
        // the label `Text` beside it, and neither Row nor the enclosing Column emits
        // its own semantics node, so the four (RadioButton, Text) pairs flatten into
        // one sibling list — `onNodeWithText("OUTLOOK")`'s nearest semantics parent is
        // ambiguous over all four RadioButtons, not just its own row's. Select by
        // position instead: Generic is the fourth declared option (index 3, the same
        // order `SourceOption` is called in above).
        composeTestRule.onAllNodes(isSelectable())[3].performClick()

        composeTestRule.onNodeWithTag("mail-import-source-host")
            .assertTextContains("imap.example.org", substring = true)
        composeTestRule.onNodeWithTag("mail-import-source-port")
            .assertTextContains("993", substring = true)
    }
    /**
     * A failed import is a record, not a run (§ A failed import is a record the
     * wizard shows until the user moves on): Dismiss alone, none of Pause /
     * Resume / Restart / Cancel; a live import keeps them.
     */
    @Test
    fun aFailedImportOffersDismissAndNoneOfTheControls() {
        var dismissed = 0
        render(
            snap(
                ImportStep.PROGRESS,
                sessionState = ImportSessionState.ERRORED,
                rebuiltMailboxes = listOf("INBOX"),
            ),
            onDismiss = { dismissed++ },
        )
        composeTestRule.onNodeWithTag("mail-import-dismiss-button").assertExists().performClick()
        assertEquals(1, dismissed)
        for (id in listOf(
            "mail-import-pause-button",
            "mail-import-resume-button",
            "mail-import-restart-button",
            "mail-import-cancel-button",
        )) {
            composeTestRule.onNodeWithTag(id).assertDoesNotExist()
        }
    }

    /** A live import keeps its controls and no Dismiss. */
    @Test
    fun aLiveImportKeepsItsControlsAndNoDismiss() {
        render(snap(ImportStep.PROGRESS, sessionState = ImportSessionState.RUNNING))
        composeTestRule.onNodeWithTag("mail-import-dismiss-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("mail-import-pause-button").assertExists()
        composeTestRule.onNodeWithTag("mail-import-restart-button").assertDoesNotExist()
    }

    /** The restart is offered only while a rebuilt source mailbox is pending. */
    @Test
    fun theRestartIsOfferedWhileARebuiltMailboxIsPending() {
        var restarted = 0
        render(
            snap(
                ImportStep.PROGRESS,
                sessionState = ImportSessionState.PAUSED,
                rebuiltMailboxes = listOf("INBOX"),
            ),
            onRestart = { restarted++ },
        )
        composeTestRule.onNodeWithTag("mail-import-restart-button").assertExists().performClick()
        assertEquals(1, restarted)
    }

    /** The forget control paints for every provider while a source is remembered. */
    @Test
    fun theForgetButtonPaintsOnlyWhileASourceIsRemembered() {
        var forgotten = 0
        render(
            snap(ImportStep.SOURCE, sourceKind = ImportSourceKind.GMAIL, sourceRemembered = true),
            onForgetSource = { forgotten++ },
        )
        composeTestRule.onNodeWithTag("mail-import-source-forget-button").assertExists().performScrollTo().performClick()
        assertEquals(1, forgotten)
    }

    @Test
    fun theForgetButtonIsAbsentWhenNothingIsRemembered() {
        render(snap(ImportStep.SOURCE, sourceKind = ImportSourceKind.GMAIL))
        composeTestRule.onNodeWithTag("mail-import-source-forget-button").assertDoesNotExist()
    }

    /** The remembered source fills the Source drafts once the snapshot says so. */
    @Test
    fun theRememberedSourceFillsTheUsernameDraft() {
        render(
            snap(
                ImportStep.SOURCE,
                sourceKind = ImportSourceKind.GENERIC,
                host = "imap.old.example",
                sourceRemembered = true,
                username = "me@old.example",
            ),
        )
        composeTestRule.onNodeWithTag("mail-import-source-username").assertTextContains("me@old.example")
        composeTestRule.onNodeWithTag("mail-import-source-host").assertTextContains("imap.old.example")
    }

    /** The import-duplicates toggle reads the snapshot (off by default) and carries its warning. */
    @Test
    fun theImportDuplicatesToggleIsOffByDefaultAndCarriesItsWarning() {
        val seen = mutableListOf<Boolean>()
        render(snap(ImportStep.SCOPE), onSetImportDuplicates = { seen += it })
        val toggle = composeTestRule.onNodeWithTag("mail-import-scope-import-duplicates-toggle")
        toggle.assertExists()
        assertEquals(
            "off",
            toggle.fetchSemanticsNode().config.getOrNull(SemanticsProperties.StateDescription),
        )
        toggle.performClick()
        assertEquals(listOf(true), seen)
    }

    @Test
    fun theImportDuplicatesToggleReadsOnWhenTheSnapshotSaysSo() {
        render(snap(ImportStep.SCOPE, importDuplicates = true))
        assertEquals(
            "on",
            composeTestRule.onNodeWithTag("mail-import-scope-import-duplicates-toggle")
                .fetchSemanticsNode().config.getOrNull(SemanticsProperties.StateDescription),
        )
    }

    /**
     * The full-mailbox notice (`mailbox-migration.md` § Failure handling →
     * *Nest-side errors*): a session paused on `MailboxFull` paints the notice
     * and Skip the rest beside Resume and Cancel; any other pause reason, or a
     * running session, paints neither.
     */
    @Test
    fun aMailboxFullPauseOffersTheNoticeAndSkipTheRest() {
        var snapshot by mutableStateOf(
            snap(
                ImportStep.PROGRESS,
                sessionState = ImportSessionState.PAUSED,
                pauseReason = ImportPauseReason.CONNECTION_LOST,
            ),
        )
        composeTestRule.setContent {
            MailImportContent(
                snapshot = snapshot,
                sourceKindLabel = { it.name },
                tlsModeLabel = { it.name },
                onBack = {},
                onSelectSourceKind = {},
                onSetTlsMode = {},
                onConnect = { _, _, _, _, _ -> },
                onToggleMailbox = {},
                onScopeNext = { _, _ -> },
                onPrev = {},
                onStart = {},
                onPause = {},
                onResume = { _, _ -> },
                onCancel = {},
                onSkipRest = {},
                onRestart = {},
                onDismiss = {},
                onForgetSource = {},
                onSetImportDuplicates = {},
                onViewImported = {},
                onSignInWithMicrosoft = {},
            )
        }
        composeTestRule.onNodeWithTag("mail-import-quota-notice").assertDoesNotExist()
        composeTestRule.onNodeWithTag("mail-import-quota-skip-rest-button").assertDoesNotExist()

        snapshot = snap(
            ImportStep.PROGRESS,
            sessionState = ImportSessionState.PAUSED,
            pauseReason = ImportPauseReason.MAILBOX_FULL,
        )
        composeTestRule.onNodeWithTag("mail-import-quota-notice").assertExists()
        composeTestRule.onNodeWithTag("mail-import-quota-skip-rest-button").assertExists()
        composeTestRule.onNodeWithTag("mail-import-resume-button").assertExists()
        composeTestRule.onNodeWithTag("mail-import-cancel-button").assertExists()
    }
}
