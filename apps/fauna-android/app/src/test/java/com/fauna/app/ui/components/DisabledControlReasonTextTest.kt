package com.fauna.app.ui.components

import androidx.compose.foundation.layout.Box
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onChildren
import androidx.compose.ui.test.onNodeWithTag
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for [DisabledControlReasonText] — the shared un-id'd
 * hint (`ui/README.md` § Copy comprehensibility rule 5) rendered beside every
 * onboarding disabled-control site this session wired up (dns_config's
 * per-provider rows via [DnsProviderButtonTest], vps_config's and
 * nest_provisioning's Continue buttons). A pure leaf taking an
 * already-`localized()`-resolved `String?`, so it runs on the host JVM with no
 * FFI seam / `.so`, exactly like [GatedPostBadgeTest].
 *
 * [DisabledControlReasonText] is deliberately un-id'd chrome (no `.testTag`),
 * so "nothing rendered" is asserted by wrapping it in a tagged, otherwise-empty
 * container and checking its child count rather than querying for the text
 * itself.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class DisabledControlReasonTextTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Test
    fun rendersReasonWhenPresent() {
        composeTestRule.setContent {
            Box(modifier = Modifier.testTag("container")) {
                DisabledControlReasonText(reason = "Choose a plan for your server to continue.")
            }
        }
        composeTestRule.onNodeWithTag("container").onChildren().assertCountEquals(1)
        composeTestRule.onNodeWithTag("container").onChildren()[0]
            .assertTextEquals("Choose a plan for your server to continue.")
    }

    @Test
    fun rendersNothingWhenNull() {
        composeTestRule.setContent {
            Box(modifier = Modifier.testTag("container")) {
                DisabledControlReasonText(reason = null)
            }
        }
        composeTestRule.onNodeWithTag("container").onChildren().assertCountEquals(0)
    }

    @Test
    fun rendersNothingWhenEmpty() {
        composeTestRule.setContent {
            Box(modifier = Modifier.testTag("container")) {
                DisabledControlReasonText(reason = "")
            }
        }
        composeTestRule.onNodeWithTag("container").onChildren().assertCountEquals(0)
    }
}
