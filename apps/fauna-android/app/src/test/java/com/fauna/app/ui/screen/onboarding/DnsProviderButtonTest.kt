package com.fauna.app.ui.screen.onboarding

import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for [DnsProviderButton] — the stateless per-row unit
 * `dns_config`'s provider grid renders (`DnsConfigScreen.kt`), split out so it
 * is directly testable without an `OnboardingHost`/FFI machine, mirroring how
 * [com.fauna.app.ui.screen.settings.MailAliasesContent] stays FFI-free (no
 * Hilt, no VM, no native calls). Exercises the two rule-5 halves
 * (`ui/README.md` § Copy comprehensibility rule 5) `DnsConfigScreen` pairs per
 * provider: `dnsProviderEligible` (enabled state) and
 * `dnsProviderIneligibleReason` (the reason text) — [DnsProviderButton] takes
 * both already resolved as plain `enabled`/`reason` parameters.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class DnsProviderButtonTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Test
    fun eligibleProviderIsEnabledWithNoReasonText() {
        composeTestRule.setContent {
            DnsProviderButton(
                label = "Hetzner",
                selected = false,
                enabled = true,
                reason = null,
                tag = "dns-provider-row[0]",
                onClick = {},
            )
        }
        composeTestRule.onNodeWithTag("dns-provider-row[0]").assertIsEnabled()
        composeTestRule.onNodeWithTag("dns-provider-row[0]").assertTextEquals("Hetzner")
    }

    @Test
    fun ineligibleProviderIsDisabledAndShowsItsReason() {
        composeTestRule.setContent {
            DnsProviderButton(
                label = "Cloudflare",
                selected = false,
                enabled = false,
                reason = "Doesn't sell VPS servers — untick the same-provider box above to pick it.",
                tag = "dns-provider-row[1]",
                onClick = {},
            )
        }
        composeTestRule.onNodeWithTag("dns-provider-row[1]").assertIsNotEnabled()
        composeTestRule.onNodeWithText("Doesn't sell VPS servers — untick the same-provider box above to pick it.")
            .assertTextEquals("Doesn't sell VPS servers — untick the same-provider box above to pick it.")
    }

    @Test
    fun clickFiresOnClickWithNoOtherState() {
        var clicked = 0
        composeTestRule.setContent {
            DnsProviderButton(
                label = "Porkbun",
                selected = false,
                enabled = true,
                reason = null,
                tag = "dns-provider-row[2]",
                onClick = { clicked++ },
            )
        }
        composeTestRule.onNodeWithTag("dns-provider-row[2]").performClick()
        assertEquals(1, clicked)
    }
}
