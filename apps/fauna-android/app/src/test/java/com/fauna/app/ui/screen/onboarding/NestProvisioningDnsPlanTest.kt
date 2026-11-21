package com.fauna.app.ui.screen.onboarding

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.onboarding.DnsPlanRowView
import com.fauna.ffi.onboarding.DnsPlanSnapshot
import com.fauna.ffi.provisioning.DnsPlanAction
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import social.fauna.generated.Ids
import uniffi.fauna_core.LocalizedText

/**
 * `docs/goal/behavior/onboarding-provisioning.md` § 6 *The DNS plan* on
 * android's nest_provisioning page: [ProvisioningDnsPlan] paints the shared
 * `dns_plan()` snapshot — nothing while it is not `visible`, otherwise
 * `provisioning-dns-plan` with one `provisioning-dns-plan-row[<name>|<type>]`
 * per record, each sentence resolved from the snapshot's own text.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class NestProvisioningDnsPlanTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun text(key: String, vararg args: Pair<String, String>) =
        LocalizedText("onboarding.nest_provisioning.$key", mapOf(*args))

    @Test
    fun aHiddenPlanPaintsNothing() {
        composeTestRule.setContent {
            ProvisioningDnsPlan(
                DnsPlanSnapshot(
                    visible = false,
                    heading = LocalizedText("", emptyMap()),
                    rows = emptyList(),
                ),
            )
        }
        composeTestRule.onNodeWithTag(Ids.PROVISIONING_DNS_PLAN).assertDoesNotExist()
        composeTestRule.onNodeWithTag("${Ids.PROVISIONING_DNS_PLAN_ROW}[@|A]").assertDoesNotExist()
    }

    @Test
    fun aReadyPlanPaintsTheHeadingAndOneKeyedRowPerRecord() {
        val plan = DnsPlanSnapshot(
            visible = true,
            heading = text("dns_plan_heading", "domain" to "example.org"),
            rows = listOf(
                DnsPlanRowView(
                    key = "@|A",
                    action = DnsPlanAction.CHANGE,
                    text = text(
                        "dns_plan_row_change",
                        "name" to "@",
                        "record_type" to "A",
                        "old" to "9.9.9.9",
                        "value" to "1.2.3.4",
                    ),
                ),
                DnsPlanRowView(
                    key = "@|MX",
                    action = DnsPlanAction.ADD,
                    text = text(
                        "dns_plan_row_add",
                        "name" to "@",
                        "record_type" to "MX",
                        "value" to "10 mail.example.org",
                    ),
                ),
            ),
        )
        composeTestRule.setContent { ProvisioningDnsPlan(plan) }

        composeTestRule.onNodeWithTag(Ids.PROVISIONING_DNS_PLAN).assertIsDisplayed()
        composeTestRule.onNodeWithTag("${Ids.PROVISIONING_DNS_PLAN_ROW}[@|A]", useUnmergedTree = true)
            .assertIsDisplayed()
            .assertTextContains("9.9.9.9 → 1.2.3.4", substring = true)
        composeTestRule.onNodeWithTag("${Ids.PROVISIONING_DNS_PLAN_ROW}[@|MX]", useUnmergedTree = true)
            .assertIsDisplayed()
            .assertTextContains("10 mail.example.org", substring = true)
    }
}
