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
import uniffi.fauna_client_mail_settings.AliasPolicyView
import uniffi.fauna_client_mail_settings.AuthPolicyView
import uniffi.fauna_client_mail_settings.BaselinePublishView
import uniffi.fauna_client_mail_settings.MailPolicyConfirm
import uniffi.fauna_client_mail_settings.BaselineStateView
import uniffi.fauna_client_mail_settings.DmarcPolicyView
import uniffi.fauna_client_mail_settings.ImapPolicyView
import uniffi.fauna_client_mail_settings.MicrosoftRegistrationView
import uniffi.fauna_client_mail_settings.OutboundPolicyView
import uniffi.fauna_client_mail_settings.SpamPolicyView
import uniffi.fauna_client_mail_settings.SubmissionPolicyView

/**
 * Compose-level coverage for the stateless [AdminMailContent] (the flat
 * `admin-mail` policy page, admin.md § 6 / mail-policy-config.md § Tier 2): the
 * mail-enable toggle and all six policy groups (spam / auth / submission / imap /
 * outbound / alias), each a full-PUT save. Renders with seeded state — no Hilt,
 * no VM, no FFI native calls. Verifies the widgets render and a save gathers the
 * whole edited sub-struct.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AdminMailContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun spam() = SpamPolicyView(
        maxScoreBeforeSpamFolder = 5u,
        maxScoreBeforeReject = 0u,
        dnsblServers = listOf("zen.spamhaus.org"),
        rejectNoRdns = false,
        greylistEnabled = false,
        greylistDelaySecs = 60u,
        maxConnPerMin = 30u,
        fcrdnsMode = "score_signal",
        heloIdentityRequired = false,
        rejectFcrdnsFail = false,
        maxMessageBytes = 26_214_400u,
        bayesianWeightMilli = 700u,
        bayesianMinSamples = 50u,
        bayesianFullConfidenceSamples = 200u,
        trainingHistoryRetentionDays = 30u,
        unlistedRecipientPenalty = 0u,
        baselineStandingPublish = false,
    )

    private fun auth() = AuthPolicyView(
        enforceDmarc = true,
        enforceDmarcQuarantine = false,
        enforceSpfHardfail = false,
        enforceDkim = false,
        logOnly = false,
        maxAuthFailuresPerMinute = 30u,
        maxConnPerIp = 0u,
    )

    private fun submission() = SubmissionPolicyView(maxPerDay = 1000u, maxRecipientsPerMessage = 100u)

    private fun imap() = ImapPolicyView(
        idleTimeoutSecs = 1740u,
        tombstoneRetentionDays = 30u,
        deleteNonempty = "forbidden",
        bodystructureCacheMax = 256u,
        storageBytesDefault = 1uL shl 30,
        messageCountDefault = 50_000u,
    )

    private fun outbound() = OutboundPolicyView(
        retryScheduleSeconds = listOf(0uL, 300uL, 900uL),
        permanentFailureTimeoutHours = 120u,
        delayWarningAtHours = 4u,
        ndrRateLimitDays = 1u,
        suppressNdrSpfHardfail = true,
        suppressNdrDmarcReject = true,
        postmasterCcBounces = false,
        tlsrptSendReports = false,
        ipv6Enabled = false,
        treat5xxAsTransient = emptyList(),
    )

    private fun alias() = AliasPolicyView(
        exactAliasesMax = 20u,
        reservedLocalParts = listOf("postmaster", "abuse"),
        subaddressingEnabled = true,
        wildcardPrefixEnabled = true,
    )

    private fun dmarc() = DmarcPolicyView(
        policyMode = "quarantine",
        subdomainPolicyMode = "reject",
        pct = 50u.toUByte(),
        adkimMode = "r",
        aspfMode = "s",
        ruaDestination = "mailto:reports@example.test",
        rufPublish = false,
        rufDestination = null,
        foMode = "0",
        riSeconds = 86400u,
        aggregateRetentionDays = 90u,
        forensicRetentionDays = 30u,
    )

    private fun microsoft() = MicrosoftRegistrationView(clientId = "", tenant = "", isDefault = true)

    private fun render(
        mailEnabled: Boolean = true,
        mailUnavailableHere: Boolean = false,
        working: Boolean = false,
        onSetMailEnabled: (Boolean) -> Unit = {},
        onSaveSpam: (SpamPolicyView) -> Unit = {},
        onSaveAuth: (AuthPolicyView) -> Unit = {},
        onSaveSubmission: (SubmissionPolicyView) -> Unit = {},
        onSaveImap: (ImapPolicyView) -> Unit = {},
        onSaveOutbound: (OutboundPolicyView) -> Unit = {},
        onSaveAlias: (AliasPolicyView) -> Unit = {},
        onSaveDmarc: (DmarcPolicyView) -> Unit = {},
        onSaveMicrosoft: (String, String) -> Unit = { _, _ -> },
        microsoft: MicrosoftRegistrationView = microsoft(),
        baselinePublishResult: BaselinePublishView? = null,
        armed: MailPolicyConfirm? = null,
        onPress: (MailPolicyConfirm) -> Unit = {},
        baselineState: BaselineStateView? = null,
        onSetBaselineStanding: (Boolean) -> Unit = {},
        // FFI-free stubs mirroring the shared parse_count / parse_count_u64 (trim +
        // parse) so Robolectric stays off the native path — no host JNA needed.
        parseCount: (String) -> UInt? = { it.trim().toUIntOrNull() },
        parseCountU64: (String) -> ULong? = { it.trim().toULongOrNull() },
    ) {
        composeTestRule.setContent {
            AdminMailContent(
                mailEnabled = mailEnabled,
                mailUnavailableHere = mailUnavailableHere,
                spam = spam(),
                auth = auth(),
                submission = submission(),
                imap = imap(),
                outbound = outbound(),
                alias = alias(),
                dmarc = dmarc(),
                microsoft = microsoft,
                working = working,
                onBack = {},
                onSetMailEnabled = onSetMailEnabled,
                onSaveSpam = onSaveSpam,
                onSaveAuth = onSaveAuth,
                onSaveSubmission = onSaveSubmission,
                onSaveImap = onSaveImap,
                onSaveOutbound = onSaveOutbound,
                onSaveAlias = onSaveAlias,
                onSaveDmarc = onSaveDmarc,
                onSaveMicrosoft = onSaveMicrosoft,
                baselinePublishResult = baselinePublishResult,
                armed = armed,
                onPress = onPress,
                baselineState = baselineState,
                onSetBaselineStanding = onSetBaselineStanding,
                formatDate = { "2026-10-10" },
                parseCount = parseCount,
                parseCountU64 = parseCountU64,
                // The shared `dmarc_forensic_retention_warns` threshold (30), FFI-free.
                dmarcRetentionWarns = { it > 30u },
            )
        }
    }

    @Test
    fun rendersHeadingAndEnableToggle() {
        render()
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("admin-nav-back").assertExists()
        composeTestRule.onNodeWithTag("admin-mail-enabled-toggle").assertExists()
    }

    @Test
    fun allSixGroupSaveButtonsRender() {
        render()
        for (tag in listOf(
            "admin-mail-spam-save-button",
            "admin-mail-auth-save-button",
            "admin-mail-submission-save-button",
            "admin-mail-imap-save-button",
            "admin-mail-outbound-save-button",
            "admin-mail-alias-save-button",
        )) {
            composeTestRule.onNodeWithTag(tag).assertExists()
        }
    }

    @Test
    fun selectsRender() {
        render()
        composeTestRule.onNodeWithTag("admin-mail-fcrdns-mode-select").assertExists()
        composeTestRule.onNodeWithTag("admin-mail-imap-delete-nonempty-select").assertExists()
    }

    @Test
    fun enableToggleFires() {
        var set: Boolean? = null
        render(mailEnabled = true, onSetMailEnabled = { set = it })
        composeTestRule.onNodeWithTag("admin-mail-enabled-toggle").performClick()
        assertEquals(false, set)
    }

    private fun stateIs(expected: String) =
        SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, expected)

    @Test
    fun unavailableBoxDisablesTheMasterToggleAndShowsTheNotice() {
        // A box that cannot enable mail (shared `mail_unavailable_here`): the master
        // toggle reads `unavailable`, is disabled, the notice beneath it.
        var set: Boolean? = null
        render(mailEnabled = false, mailUnavailableHere = true, onSetMailEnabled = { set = it })
        composeTestRule.onNodeWithTag("admin-mail-enabled-toggle")
            .assertIsNotEnabled()
            .assert(stateIs("unavailable"))
        composeTestRule.onNodeWithTag("admin-mail-unavailable-notice")
            .assertTextEquals(RuntimeEnvironment.getApplication().getString(R.string.mail_settings_unavailable_notice))
        assertEquals(null, set)
    }

    @Test
    fun mailAlreadyOnKeepsALiveMasterToggleOnAnUnavailableBox() {
        render(mailEnabled = true, mailUnavailableHere = true)
        composeTestRule.onNodeWithTag("admin-mail-enabled-toggle")
            .assertIsEnabled()
            .assert(stateIs("on"))
        composeTestRule.onNodeWithTag("admin-mail-unavailable-notice").assertDoesNotExist()
    }

    @Test
    fun saveSpamGathersEditedThreshold() {
        var saved: SpamPolicyView? = null
        render(onSaveSpam = { saved = it })
        composeTestRule.onNodeWithTag("admin-mail-spam-threshold-junk").performTextReplacement("6")
        composeTestRule.onNodeWithTag("admin-mail-spam-save-button").performScrollTo().performClick()
        assertEquals(6u, saved?.maxScoreBeforeSpamFolder)
        // The unedited fields ride along in the full PUT.
        assertEquals("score_signal", saved?.fcrdnsMode)
        assertEquals(listOf("zen.spamhaus.org"), saved?.dnsblServers)
    }

    @Test
    fun saveSpamGathersEditedBayesianWeight() {
        var saved: SpamPolicyView? = null
        render(onSaveSpam = { saved = it })
        composeTestRule.onNodeWithTag("admin-mail-spam-bayesian-weight")
            .performScrollTo().performTextReplacement("500")
        composeTestRule.onNodeWithTag("admin-mail-spam-save-button").performScrollTo().performClick()
        assertEquals(500u, saved?.bayesianWeightMilli)
        // The unedited Tier-2 knobs ride along in the full PUT.
        assertEquals(50u, saved?.bayesianMinSamples)
        assertEquals(200u, saved?.bayesianFullConfidenceSamples)
        assertEquals(30u, saved?.trainingHistoryRetentionDays)
    }

    @Test
    fun saveSpamGathersEditedUnlistedRecipientPenalty() {
        var saved: SpamPolicyView? = null
        render(onSaveSpam = { saved = it })
        composeTestRule.onNodeWithTag("admin-mail-unlisted-recipient-penalty")
            .performScrollTo().performTextReplacement("1000")
        composeTestRule.onNodeWithTag("admin-mail-spam-save-button").performScrollTo().performClick()
        assertEquals(1000u, saved?.unlistedRecipientPenalty)
        // The unedited fields ride along in the full PUT.
        assertEquals(30u, saved?.trainingHistoryRetentionDays)
    }

    @Test
    fun publishBaselineButtonPresses() {
        var pressed: MailPolicyConfirm? = null
        render(onPress = { pressed = it })
        composeTestRule.onNodeWithTag("admin-mail-publish-spam-baseline-button")
            .performScrollTo().performClick()
        assertEquals(MailPolicyConfirm.PUBLISH_SPAM_BASELINE, pressed)
    }

    @Test
    fun armedPublishBaselineButtonSaysWhoseTrainingItReflects() {
        // mail-spam.md § Cold start Path 2 step 1: the first press arms the button,
        // which then reads the opted-in-training sentence; the second press publishes.
        render(armed = MailPolicyConfirm.PUBLISH_SPAM_BASELINE)
        composeTestRule.onNodeWithTag("admin-mail-publish-spam-baseline-button")
            .performScrollTo()
            .assertTextContains("Publish the baseline? It reflects the spam training of the members who opted in", substring = true)
    }

    @Test
    fun publishBaselineResultRendersPublished() {
        render(
            baselinePublishResult = BaselinePublishView(
                published = true, contributors = 3u, sampleCount = 42u, skippedContributors = 0u, deferred = false,
            ),
        )
        composeTestRule.onNodeWithTag("admin-mail-publish-spam-baseline-result")
            .performScrollTo().assertTextContains("3 contributors", substring = true)
    }

    @Test
    fun publishBaselineResultRendersWithheld() {
        render(
            baselinePublishResult = BaselinePublishView(
                published = false, contributors = 2u, sampleCount = 0u, skippedContributors = 0u, deferred = false,
            ),
        )
        composeTestRule.onNodeWithTag("admin-mail-publish-spam-baseline-result")
            .performScrollTo().assertTextContains("too few contributors", substring = true)
    }

    @Test
    fun publishBaselineResultRendersSkippedContributors() {
        // The holder-side erosion count (silent-erosion fix, mail-spam.md
        // § Encrypted-mode interaction) must surface beside the published/
        // withheld message whenever the last run skipped anyone.
        render(
            baselinePublishResult = BaselinePublishView(
                published = true, contributors = 3u, sampleCount = 42u, skippedContributors = 2u, deferred = false,
            ),
        )
        composeTestRule.onNodeWithTag("admin-mail-publish-spam-baseline-result")
            .performScrollTo().assertTextContains("2 opted-in contributor(s)", substring = true)
    }

    @Test
    fun publishBaselineResultRendersDeferredAsWaiting() {
        // A delta-floor deferral also carries `published = false`; it must read
        // "Waiting for more contributor activity.", never "too few contributors".
        render(
            baselinePublishResult = BaselinePublishView(
                published = false, contributors = 2u, sampleCount = 0u, skippedContributors = 0u, deferred = true,
            ),
        )
        composeTestRule.onNodeWithTag("admin-mail-publish-spam-baseline-result")
            .performScrollTo().assertTextContains("Waiting for more contributor activity.")
    }

    @Test
    fun standingBaselineToggleFiresItsOwnGesture() {
        var flipped: Boolean? = null
        render(onSetBaselineStanding = { flipped = it })
        composeTestRule.onNodeWithTag("admin-mail-spam-baseline-standing-toggle")
            .performScrollTo().performClick()
        // The spam() fixture carries standing off, so the click turns it on.
        assertEquals(true, flipped)
    }

    @Test
    fun baselineStateRendersPublishedNoneAndWaiting() {
        render(
            baselineState = BaselineStateView(
                published = true, contributors = 3u, sampleCount = 42u,
                publishedAtMs = 1_760_000_000_000L, skippedContributors = 0u, deferred = true, standing = true,
            ),
        )
        composeTestRule.onNodeWithTag("admin-mail-spam-baseline-state").performScrollTo()
            .assertTextContains(
                "Published over 3 contributors on 2026-10-10. Waiting for more contributor activity.",
            )
    }

    @Test
    fun baselineStateRendersNoneWhenNothingIsServed() {
        render(
            baselineState = BaselineStateView(
                published = false, contributors = 0u, sampleCount = 0u,
                publishedAtMs = null, skippedContributors = 0u, deferred = false, standing = false,
            ),
        )
        composeTestRule.onNodeWithTag("admin-mail-spam-baseline-state").performScrollTo()
            .assertTextContains("No baseline published.")
    }

    @Test
    fun saveAuthGathersToggles() {
        var saved: AuthPolicyView? = null
        render(onSaveAuth = { saved = it })
        composeTestRule.onNodeWithTag("admin-mail-auth-enforce-dkim-toggle").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-mail-auth-save-button").performScrollTo().performClick()
        assertEquals(true, saved?.enforceDkim)
        assertEquals(true, saved?.enforceDmarc) // unedited, rides along
    }

    @Test
    fun saveAuthGathersMaxConnPerIp() {
        var saved: AuthPolicyView? = null
        render(onSaveAuth = { saved = it })
        composeTestRule.onNodeWithTag("admin-mail-auth-max-conn-per-ip-input")
            .performScrollTo().performTextReplacement("128")
        composeTestRule.onNodeWithTag("admin-mail-auth-save-button").performScrollTo().performClick()
        assertEquals(128u, saved?.maxConnPerIp)
        assertEquals(30u, saved?.maxAuthFailuresPerMinute) // unedited, rides along
    }

    @Test
    fun dmarcGroupRendersItsSixControls() {
        render()
        for (tag in listOf(
            "admin-mail-dmarc-ruf-publish-toggle",
            "admin-mail-dmarc-ruf-warning",
            "admin-mail-dmarc-fo-mode-select",
            "admin-mail-dmarc-aggregate-retention-input",
            "admin-mail-dmarc-forensic-retention-input",
            "admin-mail-dmarc-save-button",
        )) {
            composeTestRule.onNodeWithTag(tag).performScrollTo().assertExists()
        }
    }

    @Test
    fun saveDmarcGathersTheFourEditsAndCarriesTheRest() {
        var saved: DmarcPolicyView? = null
        render(onSaveDmarc = { saved = it })
        composeTestRule.onNodeWithTag("admin-mail-dmarc-ruf-publish-toggle").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-mail-dmarc-aggregate-retention-input")
            .performScrollTo().performTextReplacement("120")
        composeTestRule.onNodeWithTag("admin-mail-dmarc-forensic-retention-input")
            .performScrollTo().performTextReplacement("60")
        composeTestRule.onNodeWithTag("admin-mail-dmarc-save-button").performScrollTo().performClick()
        assertEquals(true, saved?.rufPublish)
        assertEquals(120u, saved?.aggregateRetentionDays)
        assertEquals(60u, saved?.forensicRetentionDays)
        // Every record tag without a control rides along unchanged.
        assertEquals("quarantine", saved?.policyMode)
        assertEquals("mailto:reports@example.test", saved?.ruaDestination)
        assertEquals(50u.toUByte(), saved?.pct)
    }

    @Test
    fun forensicRetentionWarningShowsPastTheDefault() {
        render()
        composeTestRule.onNodeWithText("keeping them longer than 30 days", substring = true).assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-mail-dmarc-forensic-retention-input")
            .performScrollTo().performTextReplacement("60")
        composeTestRule.onNodeWithText("keeping them longer than 30 days", substring = true).assertExists()
    }

    @Test
    fun saveMicrosoftSendsTheDraftsAsTyped() {
        var sent: Pair<String, String>? = null
        render(onSaveMicrosoft = { id, tenant -> sent = id to tenant })
        composeTestRule.onNodeWithTag("admin-mail-microsoft-client-id-input")
            .performScrollTo().performTextReplacement("00000000-0000-0000-0000-000000000000")
        composeTestRule.onNodeWithTag("admin-mail-microsoft-tenant-input")
            .performScrollTo().performTextReplacement("organizations")
        composeTestRule.onNodeWithTag("admin-mail-microsoft-save-button").performScrollTo().performClick()
        assertEquals("00000000-0000-0000-0000-000000000000" to "organizations", sent)
    }

    @Test
    fun microsoftFieldsSeedFromTheStoredRegistration() {
        render(
            microsoft = MicrosoftRegistrationView(
                clientId = "00000000-0000-0000-0000-000000000000", tenant = "common", isDefault = false,
            ),
        )
        composeTestRule.onNodeWithTag("admin-mail-microsoft-client-id-input")
            .performScrollTo().assertTextContains("00000000-0000-0000-0000-000000000000")
        composeTestRule.onNodeWithText("Using your organisation", substring = true).assertExists()
    }

    @Test
    fun outboundPostmasterCcIsReadOnly() {
        render()
        composeTestRule.onNodeWithTag("admin-mail-outbound-postmaster-cc-toggle").assertIsNotEnabled()
    }
}
