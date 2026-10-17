package com.fauna.app.core

import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.components.RegionBlockWitness
import com.fauna.app.ui.components.RegionPlaceholder
import com.fauna.app.ui.screen.settings.RegionSettingsRows
import com.fauna.ffi.FfiContentPolicy
import com.fauna.ffi.FfiDeclaredRegion
import com.fauna.ffi.FfiRegionPlaceholder
import com.fauna.ffi.FfiRegionPolicyRow
import com.fauna.ffi.FfiRegionSource
import com.fauna.ffi.FfiRegionView
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import social.fauna.generated.Ids
import uniffi.fauna_core.ContentLabelEntry

/**
 * The android leg of the region content plane (`region-blocking.md` § The
 * content plane): the leaf's answer reaches the shared `FfiRegionPlane` and its
 * view, an unopened plane leaves every surface on the family arms, the
 * placeholder paints the `region-blocked-*` ids with the convention-17 witness
 * sets in step, and the settings rows paint the shared view. The plane's verify
 * and fold are shared Rust, tested there; this proves the android wiring.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class RegionPlaneHostTest {
    @get:Rule val compose = createComposeRule()
    @get:Rule val dir = TemporaryFolder()

    @After fun reset() = RegionPlaneHost.resetForTest()

    private fun nsfw() = listOf(ContentLabelEntry(category = "nsfw", confidencePerMille = 900u))

    private val subject = RegionSubject("ab".repeat(32), null, "hello", emptyList(), false)

    @Test
    fun `an unopened plane leaves the family verdict and paints no region placeholder`() {
        val floor = FfiContentPolicy(nsfw = "block", spam = "inherit", phishing = "inherit", commercial = "inherit")
        val d = RegionPlaneHost.render(nsfw(), ContentPolicyInputs(contentPolicy = floor), subject)
        assertEquals("block", d.verdict)
        assertNull(d.placeholder)
        assertNull(RegionPlaneHost.view())
    }

    @Test
    fun `the leaf's answer is the declared region with its source, and nothing binds without a policy`() {
        RegionPlaneHost.open("NO", FfiRegionSource.SYSTEM_REGION, dir.root.absolutePath)
        val declared = RegionPlaneHost.view()?.declared
        assertEquals("NO", declared?.code)
        assertEquals(FfiRegionSource.SYSTEM_REGION, declared?.source)
        assertEquals("region.source_system_region", declared?.sourceLabelKey)
        assertTrue(RegionPlaneHost.view()!!.policies.isEmpty())

        val d = RegionPlaneHost.render(nsfw(), ContentPolicyInputs(), subject)
        // No rule fires: a label alone is at most a badge, which renders normally.
        assertTrue(d.verdict, d.verdict == "show" || d.verdict == "badge")
        assertNull(d.placeholder)
    }

    @Test
    fun `a code that is no region declares nothing`() {
        RegionPlaneHost.open(null, FfiRegionSource.SYSTEM_REGION, dir.root.absolutePath)
        assertNull(RegionPlaneHost.view()?.declared)
    }

    @Test
    fun `a block placeholder paints the region ids and keeps the witness sets in step`() {
        val p = FfiRegionPlaceholder(verb = "block", region = "NO", authorityName = "Authority", reason = "Withheld.")
        compose.setContent {
            RegionBlockWitness("feed:x", isBlocked = true)
            RegionPlaceholder(p, witnessKey = "feed:x", onReveal = {})
        }
        compose.onNodeWithTag(Ids.REGION_BLOCKED_NOTICE)
            .assertTextEquals("Not shown in NO — blocked under the policy of Authority")
        compose.onNodeWithTag(Ids.REGION_BLOCKED_AUTHORITY).assertTextEquals("Authority")
        compose.onNodeWithTag(Ids.REGION_BLOCKED_REASON).assertTextEquals("Withheld.")
        compose.onAllNodesWithTag(Ids.REGION_COLLAPSED_REVEAL_BUTTON).assertCountEquals(0)
        compose.waitForIdle()
        assertEquals(1 to 1, RegionPlaneHost.blockRenderState())
    }

    @Test
    fun `a collapse placeholder reveals and is not counted as a block`() {
        val p = FfiRegionPlaceholder(verb = "collapse", region = "NO", authorityName = "Authority", reason = "Hidden.")
        var revealed = false
        compose.setContent { RegionPlaceholder(p, witnessKey = "feed:y", onReveal = { revealed = true }) }
        compose.onNodeWithTag(Ids.REGION_BLOCKED_NOTICE)
            .assertTextEquals("Hidden in NO under the policy of Authority — select to show")
        compose.onNodeWithTag(Ids.REGION_COLLAPSED_REVEAL_BUTTON).performClick()
        assertTrue(revealed)
        assertEquals(0 to 0, RegionPlaneHost.blockRenderState())
    }

    @Test
    fun `the settings rows paint the shared view`() {
        val view = FfiRegionView(
            declared = FfiDeclaredRegion("NO", FfiRegionSource.SYSTEM_REGION, "region.source_system_region"),
            policies = listOf(
                FfiRegionPolicyRow("NO", "Authority", 3u, 0u, "inert", 9u),
            ),
            lastCheckedAt = null,
            stale = true,
        )
        compose.setContent { RegionSettingsRows(view) }
        compose.onNodeWithTag(Ids.SETTINGS_REGION_DECLARED).assertTextEquals("Your region: NO")
        compose.onNodeWithTag(Ids.SETTINGS_REGION_SOURCE)
            .assertTextEquals("From your system region setting — change it in your system settings")
        compose.onAllNodesWithTag(Ids.SETTINGS_REGION_POLICY_ITEM).assertCountEquals(1)
        compose.onNodeWithTag(Ids.SETTINGS_REGION_POLICY_AUTHORITY, useUnmergedTree = true)
            .assertTextEquals("NO: Authority")
        compose.onNodeWithTag(Ids.SETTINGS_REGION_INERT_NOTICE, useUnmergedTree = true).assertExists()
        compose.onAllNodesWithTag(Ids.SETTINGS_REGION_LAST_CHECKED).assertCountEquals(0)
        compose.onNodeWithTag(Ids.SETTINGS_REGION_STALE_WARNING).assertExists()
    }

    @Test
    fun `no view reads as no region declared`() {
        compose.setContent { RegionSettingsRows(null) }
        compose.onNodeWithTag(Ids.SETTINGS_REGION_DECLARED).assertTextEquals("No region is declared on this device")
        compose.onAllNodesWithTag(Ids.SETTINGS_REGION_SOURCE).assertCountEquals(0)
    }
}
