package com.fauna.app.ui.screen.settings

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.R
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import uniffi.fauna_client_mail_settings.CredentialKind
import uniffi.fauna_client_mail_settings.MailCredentialSummary
import uniffi.fauna_client_mail_settings.MuaInstructions
import uniffi.fauna_client_mail_settings.PendingRotationStatus
import uniffi.fauna_client_mail_settings.SettingsStatus

/**
 * Compose-level coverage for the stateless [MailSettingsContent] (the
 * `mail-settings` hub, mail-settings.md): the enabled toggle, credentials list,
 * add-credential form, rotate-keys form, MUA instructions, and pending-rotation
 * banner. Seeded state — no Hilt, no VM, no FFI native calls (the native
 * password/token generation lives in the VM).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class MailSettingsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val mua = MuaInstructions(
        imapHost = "mail.example.com",
        imapPort = 993u,
        smtpHost = "mail.example.com",
        smtpPort = 465u,
        caldavHost = "mail.example.com",
        caldavPort = 443u,
        webdavUrl = "https://mail.example.com/webdav/",
        domain = "example.com",
        usernameFormat = "{handle}@{domain}",
        authMechanism = "PLAIN",
    )

    private fun cred(id: String = "c1", name: String = "iPhone Mail", revoked: Boolean = false) = MailCredentialSummary(
        credentialId = id,
        displayName = name,
        kind = CredentialKind.PLAIN,
        createdAt = 1_700_000_000u,
        muaUsername = "{handle}+$id@example.com",
        revoked = revoked,
    )

    private fun render(
        enabled: Boolean = true,
        mailUnavailableHere: Boolean = false,
        caldavEnabled: Boolean = false,
        carddavEnabled: Boolean = false,
        servesWebdavSet: Boolean = false,
        // Mirrors the shared `MailSettingsSnapshot::credential_management_reachable`
        // predicate; defaults to the same disjunction the shared machine computes
        // so existing call sites don't need updating, but can be overridden
        // (e.g. a WebDAV-only mailbox with all of enabled/caldavEnabled/
        // carddavEnabled false).
        credentialManagementReachable: Boolean = enabled || caldavEnabled || carddavEnabled || servesWebdavSet,
        servingEnabled: Boolean = true,
        credentials: List<MailCredentialSummary> = emptyList(),
        pendingRotation: PendingRotationStatus? = null,
        nestEncrypted: Boolean = true,
        lastToken: String? = null,
        onEnable: (String, CredentialKind, Boolean, String) -> Unit = { _, _, _, _ -> },
        onAddCredential: (String, CredentialKind, Boolean, String) -> Unit = { _, _, _, _ -> },
        onDisableMail: () -> Unit = {},
        onSetServingEnabled: (Boolean) -> Unit = {},
        // Pure stub for the FFI-backed strength readout, mirroring shared
        // `password_strength_label`'s canonical <8 Weak / <16 Fair / ≥16 Strong.
        strengthLabel: (String) -> String = {
            when {
                it.isEmpty() -> ""
                it.length < 8 -> "Weak"
                it.length < 16 -> "Fair"
                else -> "Strong"
            }
        },
        // Pure stub for the FFI-backed status indicator, mirroring shared
        // `settings_status_label`'s gating: an Idle mailbox reads "up to date"
        // only when enabled, else "disabled" (the bug-fix this guards).
        statusLabel: (SettingsStatus, Boolean) -> String = { status, isEnabled ->
            when (status) {
                SettingsStatus.Idle -> if (isEnabled) "All up to date" else "Mail is disabled"
                SettingsStatus.Syncing -> "Syncing"
                is SettingsStatus.RotationInProgress -> "Rotating"
            }
        },
        onStartRotation: (List<String>, () -> Unit) -> Unit = { _, _ -> },
        onResumeRotation: () -> Unit = {},
        onNavAliases: () -> Unit = {},
        receiveEnabled: Boolean? = null,
        sendEnabled: Boolean? = null,
        onToggleReceive: () -> Unit = {},
        onToggleSend: () -> Unit = {},
        autorespondEnabled: Boolean? = null,
        autorespondMessage: String = "",
        onToggleAutorespond: () -> Unit = {},
        onCommitAutorespondMessage: () -> Unit = {},
        quotaText: String? = null,
        submissionQuotaText: String? = null,
        overQuotaNotice: String? = null,
    ) {
        composeTestRule.setContent {
            MailSettingsContent(
                enabled = enabled,
                mailUnavailableHere = mailUnavailableHere,
                caldavEnabled = caldavEnabled,
                carddavEnabled = carddavEnabled,
                servesWebdavSet = servesWebdavSet,
                credentialManagementReachable = credentialManagementReachable,
                servingEnabled = servingEnabled,
                credentials = credentials,
                mua = mua,
                pendingRotation = pendingRotation,
                status = SettingsStatus.Idle,
                nestEncrypted = nestEncrypted,
                generatedPassword = "Generated-Strong-Pw-1234", // gitleaks:allow
                lastToken = lastToken,
                onBack = {},
                onNavAliases = onNavAliases,
                onNavLists = {},
                onNavExport = {},
                onNavImport = {},
                onNavSpam = {},
                onEnable = onEnable,
                onAddCredential = onAddCredential,
                onDisableMail = onDisableMail,
                onSetServingEnabled = onSetServingEnabled,
                strengthLabel = strengthLabel,
                statusLabel = statusLabel,
                onStartRotation = onStartRotation,
                onResumeRotation = onResumeRotation,
                onRegeneratePassword = {},
                onClearToken = {},
                receiveEnabled = receiveEnabled,
                sendEnabled = sendEnabled,
                onToggleReceive = onToggleReceive,
                onToggleSend = onToggleSend,
                autorespondEnabled = autorespondEnabled,
                autorespondMessage = autorespondMessage,
                onToggleAutorespond = onToggleAutorespond,
                onCommitAutorespondMessage = onCommitAutorespondMessage,
                quotaText = quotaText,
                submissionQuotaText = submissionQuotaText,
                overQuotaNotice = overQuotaNotice,
            )
        }
    }

    @Test
    fun receiveAndSendSwitchesAreAbsentUntilTheFirstRead() {
        render(enabled = true)
        composeTestRule.onAllNodesWithTag("mail-settings-receive-toggle").assertCountEquals(0)
        composeTestRule.onAllNodesWithTag("mail-settings-send-toggle").assertCountEquals(0)
    }

    @Test
    fun receiveAndSendSwitchesReflectTheReadAndDispatchOnTap() {
        var receiveTaps = 0
        var sendTaps = 0
        render(
            enabled = true,
            receiveEnabled = true,
            sendEnabled = false,
            onToggleReceive = { receiveTaps++ },
            onToggleSend = { sendTaps++ },
        )
        composeTestRule.onNodeWithTag("mail-settings-receive-toggle").performScrollTo().assertIsOn()
        composeTestRule.onNodeWithTag("mail-settings-send-toggle").performScrollTo().assertIsOff()
        composeTestRule.onNodeWithTag("mail-settings-receive-toggle").performClick()
        composeTestRule.onNodeWithTag("mail-settings-send-toggle").performClick()
        assertEquals(1, receiveTaps)
        assertEquals(1, sendTaps)
        // Non-optimistic: a tap alone repaints nothing — only a new read does.
        composeTestRule.onNodeWithTag("mail-settings-receive-toggle").assertIsOn()
        composeTestRule.onNodeWithTag("mail-settings-send-toggle").assertIsOff()
    }

    @Test
    fun autorespondToggleIsAbsentUntilReadButTheMessageInputIsAlwaysThere() {
        render(enabled = true)
        composeTestRule.onAllNodesWithTag("mail-settings-autorespond-toggle").assertCountEquals(0)
        composeTestRule.onNodeWithTag("mail-settings-autorespond-message-input").performScrollTo().assertExists()
    }

    @Test
    fun autorespondToggleShowsTheStateAndDispatches() {
        var taps = 0
        render(enabled = true, autorespondEnabled = true, autorespondMessage = "Away", onToggleAutorespond = { taps++ })
        composeTestRule.onNodeWithTag("mail-settings-autorespond-toggle").performScrollTo().assertIsOn()
        composeTestRule.onNodeWithTag("mail-settings-autorespond-toggle").performClick()
        assertEquals(1, taps)
    }

    @Test
    fun quotaReadoutAndNoticeRenderTheSharedTextAndAreAbsentBeforeTheRead() {
        render(enabled = true)
        composeTestRule.onAllNodesWithTag("mail-settings-mailbox-quota").assertCountEquals(0)
        composeTestRule.onAllNodesWithTag("mail-settings-submission-quota").assertCountEquals(0)
        composeTestRule.onAllNodesWithTag("mail-settings-over-quota-notice").assertCountEquals(0)
    }

    @Test
    fun submissionQuotaReadoutShowsTheSharedTextWhenGiven() {
        render(enabled = true, submissionQuotaText = "3 of 100 recipients today")
        composeTestRule.onNodeWithTag("mail-settings-submission-quota").performScrollTo()
            .assertTextEquals("3 of 100 recipients today")
    }

    @Test
    fun quotaReadoutShowsTheTextAndTheNoticeOnlyWhenOneIsGiven() {
        render(enabled = true, quotaText = "1 MB of 2 MB · 3 of 10 messages")
        composeTestRule.onNodeWithTag("mail-settings-mailbox-quota").performScrollTo()
            .assertTextEquals("1 MB of 2 MB · 3 of 10 messages")
        composeTestRule.onAllNodesWithTag("mail-settings-over-quota-notice").assertCountEquals(0)
    }

    @Test
    fun overQuotaNoticeRendersWhenGiven() {
        render(enabled = true, quotaText = "full", overQuotaNotice = "Your mailbox is full.")
        composeTestRule.onNodeWithTag("mail-settings-over-quota-notice").performScrollTo()
            .assertTextEquals("Your mailbox is full.")
    }

    @Test
    fun accountControlsAreHiddenWhenMailIsDisabled() {
        render(enabled = false, receiveEnabled = true, sendEnabled = true, autorespondEnabled = false, quotaText = "x")
        composeTestRule.onAllNodesWithTag("mail-settings-receive-toggle").assertCountEquals(0)
        composeTestRule.onAllNodesWithTag("mail-settings-mailbox-quota").assertCountEquals(0)
    }

    private fun stateIs(expected: String) =
        SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, expected)

    @Test
    fun unavailableBoxDisablesTheToggleAndShowsTheNotice() {
        // A box that cannot enable mail (shared `mail_unavailable_here`): the toggle
        // reads `unavailable`, is disabled, and the notice replaces the subtitle.
        render(enabled = false, mailUnavailableHere = true)
        composeTestRule.onNodeWithTag("mail-settings-enabled-toggle")
            .assertIsNotEnabled()
            .assert(stateIs("unavailable"))
        composeTestRule.onNodeWithTag("mail-settings-unavailable-notice")
            .assertTextEquals(RuntimeEnvironment.getApplication().getString(R.string.mail_settings_unavailable_notice))
    }

    @Test
    fun mailAlreadyOnKeepsALiveToggleOnAnUnavailableBox() {
        render(enabled = true, mailUnavailableHere = true)
        composeTestRule.onNodeWithTag("mail-settings-enabled-toggle")
            .assertIsEnabled()
            .assert(stateIs("on"))
        composeTestRule.onNodeWithTag("mail-settings-unavailable-notice").assertDoesNotExist()
    }

    @Test
    fun availableBoxReadsOffWithoutANotice() {
        render(enabled = false, mailUnavailableHere = false)
        composeTestRule.onNodeWithTag("mail-settings-enabled-toggle")
            .assertIsEnabled()
            .assert(stateIs("off"))
        composeTestRule.onNodeWithTag("mail-settings-unavailable-notice").assertDoesNotExist()
    }

    @Test
    fun enabledShowsCoreElements() {
        render(enabled = true, credentials = listOf(cred()))
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("mail-settings-enabled-toggle").assertExists()
        composeTestRule.onNodeWithTag("mail-settings-add-credential-button").assertExists()
        composeTestRule.onNodeWithTag("mail-settings-rotate-keys-button").assertExists()
        composeTestRule.onNodeWithTag("mail-settings-keys-info").assertExists()
        composeTestRule.onNodeWithTag("mail-settings-mua-instructions").assertExists()
    }

    /** The app passwords are rows of the Connected apps page
     *  (mail-settings.md § Where the credential rows render): this page paints
     *  none of them, only the pointer, Add password and Rotate keys. */
    @Test
    fun credentialRowsMovedToTheConnectedAppsPage() {
        render(credentials = listOf(cred("a"), cred("b", "Thunderbird")))
        composeTestRule.onNodeWithTag("mail-settings-credentials-list").assertDoesNotExist()
        composeTestRule.onNodeWithTag("mail-settings-credential-item").assertDoesNotExist()
        composeTestRule.onNodeWithTag("mail-settings-add-credential-button").assertExists()
        composeTestRule.onNodeWithTag("mail-settings-rotate-keys-button").assertExists()
        composeTestRule.onNodeWithText("Connected apps", substring = true).assertExists()
    }

    @Test
    fun muaFieldsRender() {
        render()
        composeTestRule.onNodeWithTag("mail-settings-mua-imap-host").assertExists()
        composeTestRule.onNodeWithTag("mail-settings-mua-smtp-port").assertExists()
        composeTestRule.onNodeWithTag("mail-settings-mua-username-format").assertExists()
    }

    /** The kind selector defaults to OAUTHBEARER (ui.yaml
     *  `mail-add-credential-type-selector`; mail-credentials.md § KDF choice —
     *  "OAUTHBEARER is preferred … as the default"), so an untouched form mints a
     *  bearer token and paints no password field. */
    @Test
    fun addCredentialFormDefaultsToBearerToken() {
        var submitted: List<Any?>? = null
        render(onAddCredential = { n, k, a, p -> submitted = listOf(n, k, a, p) })
        composeTestRule.onNodeWithTag("mail-settings-add-credential-button").performClick()
        composeTestRule.onNodeWithTag("mail-add-credential-password-input").assertDoesNotExist()
        composeTestRule.onNodeWithTag("mail-add-credential-name-input").performScrollTo().performTextInput("iPad")
        composeTestRule.onNodeWithTag("mail-add-credential-submit-button").performScrollTo().performClick()
        assertEquals(listOf<Any?>("iPad", CredentialKind.O_AUTH_BEARER, true, ""), submitted)
    }

    @Test
    fun addCredentialFormSubmitsAutogeneratePlain() {
        var submitted: List<Any?>? = null
        render(onAddCredential = { n, k, a, p -> submitted = listOf(n, k, a, p) })
        composeTestRule.onNodeWithTag("mail-settings-add-credential-button").performClick()
        composeTestRule.onNodeWithTag("mail-add-credential-name-input").performScrollTo().performTextInput("iPad")
        composeTestRule.onNodeWithTag("mail-add-credential-type-selector").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("mail-add-credential-submit-button").performScrollTo().performClick()
        // PLAIN selected, auto-generate on by default → (name, PLAIN, true, "")
        assertEquals(listOf<Any?>("iPad", CredentialKind.PLAIN, true, ""), submitted)
    }

    /** One click on each tagged row's centre flips it — the gesture the e2e
     *  `enable_mail_plain` action makes (UiAutomator clicks a node's centre, which
     *  on a labelled row is the label): the whole row is the toggle, not just
     *  the box at its start. */
    @Test
    fun clickingTheTaggedRowsSelectsPlainWithAManualPassword() {
        var submitted: List<Any?>? = null
        render(onAddCredential = { n, k, a, p -> submitted = listOf(n, k, a, p) })
        composeTestRule.onNodeWithTag("mail-settings-add-credential-button").performClick()
        composeTestRule.onNodeWithTag("mail-add-credential-name-input").performScrollTo().performTextInput("Default")
        composeTestRule.onNodeWithTag("mail-add-credential-type-selector").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("mail-add-credential-autogenerate-toggle").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("mail-add-credential-password-input").performScrollTo().performTextInput("chosen-password-1")
        composeTestRule.onNodeWithTag("mail-add-credential-submit-button").performScrollTo().performClick()
        assertEquals(listOf<Any?>("Default", CredentialKind.PLAIN, false, "chosen-password-1"), submitted)
    }

    @Test
    fun rotateKeysFormConfirms() {
        var excluded: List<String>? = null
        render(credentials = listOf(cred("k1")), onStartRotation = { e, _ -> excluded = e })
        composeTestRule.onNodeWithTag("mail-settings-rotate-keys-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("mail-rotate-keys-warning-text").assertExists()
        composeTestRule.onNodeWithTag("mail-rotate-keys-confirm-button").performScrollTo().performClick()
        assertEquals(emptyList<String>(), excluded)
    }

    /** Confirm holds the form open — progress painted, both controls disabled —
     *  until the rotation's dispatch returns; its return closes the form
     *  (mail-settings.md § Element visibility). Closing on dispatch left
     *  nothing on screen while the rotation ran. */
    @Test
    fun aConfirmedRotationHoldsTheFormOpenUntilItReturns() {
        var finish: (() -> Unit)? = null
        render(credentials = listOf(cred("k1"), cred("k2")), onStartRotation = { _, done -> finish = done })
        composeTestRule.onNodeWithTag("mail-settings-rotate-keys-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("mail-rotate-keys-confirm-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("mail-rotate-keys-confirm-button").assertExists().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("mail-rotate-keys-cancel-button").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("mail-rotate-keys-progress-indicator").assertTextEquals("Rotating")
        composeTestRule.runOnIdle { finish!!() }
        composeTestRule.onNodeWithTag("mail-rotate-keys-confirm-button").assertDoesNotExist()
    }

    @Test
    fun pendingRotationBannerShowsResume() {
        var resumed = false
        render(pendingRotation = PendingRotationStatus(credentialsRemaining = listOf("a", "b")), onResumeRotation = { resumed = true })
        composeTestRule.onNodeWithTag("mail-settings-pending-rotation-banner").assertExists()
        composeTestRule.onNodeWithTag("mail-settings-pending-rotation-resume-button").performScrollTo().performClick()
        assert(resumed)
    }

    @Test
    fun disableMailRequiresConfirm() {
        // Flipping the enabled-toggle off opens the destructive confirm dialog;
        // DisableMail fires only on the confirm button — mail-settings.md § Disable mail.
        var disabled = false
        render(enabled = true, onDisableMail = { disabled = true })
        composeTestRule.onAllNodesWithTag("mail-settings-disable-confirm").assertCountEquals(0)
        composeTestRule.onNodeWithTag("mail-settings-enabled-toggle").performClick()
        composeTestRule.onNodeWithTag("mail-settings-disable-confirm").assertExists()
        assertEquals(false, disabled)
        composeTestRule.onNodeWithTag("mail-settings-disable-confirm-button").performClick()
        assertEquals(true, disabled)
    }

    @Test
    fun disableMailCancelLeavesMailEnabled() {
        // Cancel dismisses the dialog without dispatching DisableMail; the toggle
        // stays on (snapshot `enabled` unchanged).
        var disabled = false
        render(enabled = true, onDisableMail = { disabled = true })
        composeTestRule.onNodeWithTag("mail-settings-enabled-toggle").performClick()
        composeTestRule.onNodeWithText("Cancel").performClick()
        assertEquals(false, disabled)
        composeTestRule.onAllNodesWithTag("mail-settings-disable-confirm").assertCountEquals(0)
    }

    @Test
    fun disabledHidesManagementControls() {
        render(enabled = false)
        composeTestRule.onNodeWithTag("mail-settings-enabled-toggle").assertExists()
        composeTestRule.onAllNodesWithTag("mail-settings-add-credential-button").assertCountEquals(0)
    }

    @Test
    fun disabledMailboxStatusReadsDisabledNotUpToDate() {
        // Regression guard: the Content must thread `enabled` into the shared
        // settings_status_label, so a disabled (Idle) mailbox reads "disabled",
        // NOT "up to date" (the bug the old enabled-blind statusText had).
        render(enabled = false)
        composeTestRule.onNodeWithTag("mail-settings-status-indicator", useUnmergedTree = true)
            .assertTextEquals("Mail is disabled")
    }

    @Test
    fun enabledMailboxStatusReadsUpToDate() {
        render(enabled = true)
        composeTestRule.onNodeWithTag("mail-settings-status-indicator", useUnmergedTree = true)
            .assertTextEquals("All up to date")
    }

    @Test
    fun serveHereToggleVisibleAndCheckedWhenEnabled() {
        render(enabled = true, servingEnabled = true)
        composeTestRule.onNodeWithTag("mail-settings-serve-here-toggle").performScrollTo().assertExists().assertIsOn()
    }

    @Test
    fun serveHereToggleHiddenWhenMailDisabled() {
        render(enabled = false)
        composeTestRule.onAllNodesWithTag("mail-settings-serve-here-toggle").assertCountEquals(0)
    }

    @Test
    fun caldavOnlyMailbox_showsCredentialMgmtAndServeHere_butNotImapSmtp() {
        // A CalDAV-only mailbox (email off, calendar on) shares the one bridge
        // credential serving IMAP+SMTP+CalDAV, so credential-management + the
        // serve-here toggle render for it; the IMAP/SMTP connection rows are
        // email-only and stay hidden. The MUA-instructions block itself renders
        // (mailbox provisioned) with the CalDAV host+port rows + the shared
        // username/auth rows. mail-settings.md § CalDAV-only mailbox; mirrors the
        // linux lead (apps/fauna-linux/src/settings/mail.rs:1179).
        render(enabled = false, caldavEnabled = true, credentials = listOf(cred("c1")))
        composeTestRule.onNodeWithTag("mail-settings-add-credential-button").assertExists()
        composeTestRule.onNodeWithTag("mail-settings-serve-here-toggle").performScrollTo().assertExists()
        composeTestRule.onNodeWithTag("mail-settings-mua-instructions").performScrollTo().assertExists()
        composeTestRule.onNodeWithTag("mail-settings-mua-caldav-host").performScrollTo().assertExists()
        composeTestRule.onNodeWithTag("mail-settings-mua-caldav-port").performScrollTo().assertExists()
        composeTestRule.onNodeWithTag("mail-settings-mua-username-format").performScrollTo().assertExists()
        composeTestRule.onAllNodesWithTag("mail-settings-mua-imap-host").assertCountEquals(0)
        composeTestRule.onAllNodesWithTag("mail-settings-mua-smtp-port").assertCountEquals(0)
    }

    @Test
    fun muaInstructionsPerProtocolGating() {
        // The one mail-settings-mua-instructions block is per-protocol: IMAP/SMTP
        // rows gate on `enabled`, the 2 CalDAV rows gate on `caldavEnabled`, and
        // username/auth show whenever a mailbox is provisioned. mail-settings.md
        // § CalDAV-only mailbox; linux lead apps/fauna-linux/src/settings/mail.rs:1179.

        // Email-only: IMAP/SMTP present, CalDAV rows hidden.
        render(enabled = true, caldavEnabled = false)
        composeTestRule.onNodeWithTag("mail-settings-mua-imap-host").assertExists()
        composeTestRule.onNodeWithTag("mail-settings-mua-username-format").assertExists()
        composeTestRule.onAllNodesWithTag("mail-settings-mua-caldav-host").assertCountEquals(0)
    }

    @Test
    fun muaInstructions_emailAndCalendar_showsAllRows() {
        // Both protocols on: IMAP/SMTP and CalDAV rows both render.
        render(enabled = true, caldavEnabled = true)
        composeTestRule.onNodeWithTag("mail-settings-mua-imap-host").performScrollTo().assertExists()
        composeTestRule.onNodeWithTag("mail-settings-mua-caldav-host").performScrollTo().assertExists()
        composeTestRule.onNodeWithTag("mail-settings-mua-caldav-port").performScrollTo().assertExists()
    }

    @Test
    fun webdavOnlyMailbox_showsCredentialMgmtAndMuaUrlRow_butNoImapSmtpOrCaldav() {
        // A WebDAV-only mailbox (email/CalDAV/CardDAV all off, this actor serves
        // >=1 folder over WebDAV) shares the one bridge credential, so
        // credential-management + the serve-here toggle render for it, and the
        // MUA-instructions block shows exactly one connection-detail row: the
        // WebDAV collection-root URL (a full URL, not host+port — no SRV
        // autodiscovery exists for WebDAV). mail-settings.md § CalDAV-only
        // mailbox; webdav-server.md § Independent enablement pt 1; mirrors the
        // linux lead (apps/fauna-linux/src/settings/mail.rs:1205).
        render(enabled = false, caldavEnabled = false, servesWebdavSet = true, credentials = listOf(cred("c1")))
        composeTestRule.onNodeWithTag("mail-settings-add-credential-button").assertExists()
        composeTestRule.onNodeWithTag("mail-settings-serve-here-toggle").performScrollTo().assertExists()
        composeTestRule.onNodeWithTag("mail-settings-mua-instructions").performScrollTo().assertExists()
        composeTestRule.onNodeWithTag("mail-settings-mua-webdav-url")
            .performScrollTo()
            .assertExists()
            .assertTextEquals("https://mail.example.com/webdav/")
        composeTestRule.onAllNodesWithTag("mail-settings-mua-imap-host").assertCountEquals(0)
        composeTestRule.onAllNodesWithTag("mail-settings-mua-caldav-host").assertCountEquals(0)
    }

    @Test
    fun webdavUrlRowHiddenWhenActorServesNoWebdavSet() {
        render(enabled = true, servesWebdavSet = false)
        composeTestRule.onNodeWithTag("mail-settings-mua-instructions").assertExists()
        composeTestRule.onAllNodesWithTag("mail-settings-mua-webdav-url").assertCountEquals(0)
    }

    @Test
    fun serveHereToggleFlipDispatches() {
        var set: Boolean? = null
        render(enabled = true, servingEnabled = true, onSetServingEnabled = { set = it })
        composeTestRule.onNodeWithTag("mail-settings-serve-here-toggle").performScrollTo().performClick()
        assertEquals(false, set)
    }
}
