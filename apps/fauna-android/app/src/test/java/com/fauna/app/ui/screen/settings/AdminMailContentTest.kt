package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_client_mail_settings.AliasPolicyView
import uniffi.fauna_client_mail_settings.AuthPolicyView
import uniffi.fauna_client_mail_settings.BaselinePublishView
import uniffi.fauna_client_mail_settings.ImapPolicyView
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

    private fun render(
        mailEnabled: Boolean = true,
        working: Boolean = false,
        onSetMailEnabled: (Boolean) -> Unit = {},
        onSaveSpam: (SpamPolicyView) -> Unit = {},
        onSaveAuth: (AuthPolicyView) -> Unit = {},
        onSaveSubmission: (SubmissionPolicyView) -> Unit = {},
        onSaveImap: (ImapPolicyView) -> Unit = {},
        onSaveOutbound: (OutboundPolicyView) -> Unit = {},
        onSaveAlias: (AliasPolicyView) -> Unit = {},
        baselinePublishResult: BaselinePublishView? = null,
        onPublishBaseline: () -> Unit = {},
        // FFI-free stubs mirroring the shared parse_count / parse_count_u64 (trim +
        // parse) so Robolectric stays off the native path — no host JNA needed.
        parseCount: (String) -> UInt? = { it.trim().toUIntOrNull() },
        parseCountU64: (String) -> ULong? = { it.trim().toULongOrNull() },
    ) {
        composeTestRule.setContent {
            AdminMailContent(
                mailEnabled = mailEnabled,
                spam = spam(),
                auth = auth(),
                submission = submission(),
                imap = imap(),
                outbound = outbound(),
                alias = alias(),
                working = working,
                onBack = {},
                onSetMailEnabled = onSetMailEnabled,
                onSaveSpam = onSaveSpam,
                onSaveAuth = onSaveAuth,
                onSaveSubmission = onSaveSubmission,
                onSaveImap = onSaveImap,
                onSaveOutbound = onSaveOutbound,
                onSaveAlias = onSaveAlias,
                baselinePublishResult = baselinePublishResult,
                onPublishBaseline = onPublishBaseline,
                parseCount = parseCount,
                parseCountU64 = parseCountU64,
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
    fun publishBaselineButtonFires() {
        var clicked = false
        render(onPublishBaseline = { clicked = true })
        composeTestRule.onNodeWithTag("admin-mail-publish-spam-baseline-button")
            .performScrollTo().performClick()
        assertEquals(true, clicked)
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
    fun outboundPostmasterCcIsReadOnly() {
        render()
        composeTestRule.onNodeWithTag("admin-mail-outbound-postmaster-cc-toggle").assertIsNotEnabled()
    }
}
