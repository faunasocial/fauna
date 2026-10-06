package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.ffi.FfiAdminMembershipTier
import com.fauna.ffi.FfiAdminTier
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [AdminSettingsContent] (the
 * `admin-settings` page, nav-labelled "Tiers" — admin.md § 3): the tier-definition
 * rows with in-place cap editing. The factory-reset danger zone moved to
 * `admin-nest` (admin.md § N Nest), covered by [AdminNestContentTest]; the
 * read-only `nest-mode-indicator` that also lived there is retired outright
 * (storage-modes.md). Renders with seeded state — no Hilt, no VM, no FFI
 * native calls (the `FfiAdminTier` is a pure data class). The cross-app
 * `test_admin.py --client android` is the standing gate once the host emulator
 * lands.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AdminSettingsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun tier(
        name: String,
        inbox: Long = 100,
        storage: Long = 200,
        devices: Long = 3,
        blob: Long = 50,
        feeds: Long = 10,
    ) = FfiAdminTier(
        name = name,
        maxInboxBytes = inbox,
        maxStorageBytes = storage,
        maxDevices = devices,
        maxBlobSize = blob,
        maxFeeds = feeds,
    )

    private fun membershipTier(
        tierName: String,
        adminTier: String = "personal",
        lapseTier: String = "free",
        createdAt: Long = 0,
    ) = FfiAdminMembershipTier(
        tierName = tierName,
        adminTier = adminTier,
        lapseTier = lapseTier,
        createdAt = createdAt,
    )

    private fun render(
        tiers: List<FfiAdminTier> = listOf(tier("free")),
        ownMembershipTierNames: List<String> = emptyList(),
        membershipTiers: List<FfiAdminMembershipTier> = emptyList(),
        onSaveTier: (String, Long, Long, Long, Long, Long) -> Unit = { _, _, _, _, _, _ -> },
        onSaveMembershipTier: (String, String, String) -> Unit = { _, _, _ -> },
        onClearMembershipTier: (String) -> Unit = {},
        // FFI-free stub mirroring the shared `parse_cap` (trim, parse i64, clamp ≥0)
        // so Robolectric stays off the native path — no host JNA needed.
        parseCap: (String) -> Long? = { it.trim().toLongOrNull()?.coerceAtLeast(0) },
    ) {
        composeTestRule.setContent {
            AdminSettingsContent(
                tiers = tiers,
                ownMembershipTierNames = ownMembershipTierNames,
                membershipTiers = membershipTiers,
                onBack = {},
                onSaveTier = onSaveTier,
                onSaveMembershipTier = onSaveMembershipTier,
                onClearMembershipTier = onClearMembershipTier,
                parseCap = parseCap,
            )
        }
    }

    @Test
    fun rendersHeadingSectionAndNavBack() {
        render()
        composeTestRule.onNodeWithTag("admin-settings-heading").assertExists()
        composeTestRule.onNodeWithTag("admin-nav-back").assertExists()
        composeTestRule.onNodeWithTag("admin-settings-tiers-section").assertExists()
    }

    @Test
    fun tierRowsRenderWithCapFieldsAndSaveButton() {
        render(tiers = listOf(tier("free"), tier("personal")))
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-settings-tier-item").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-settings-tier-cap-inbox").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-settings-tier-cap-feeds").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-settings-tier-save-button").fetchSemanticsNodes().size)
    }

    @Test
    fun saveTierFiresWithEditedCaps() {
        var saved: List<Any>? = null
        render(
            tiers = listOf(tier("free", inbox = 100, storage = 200, devices = 3, blob = 50, feeds = 10)),
            onSaveTier = { n, i, s, d, b, f -> saved = listOf(n, i, s, d, b, f) },
        )
        composeTestRule.onNodeWithTag("admin-settings-tier-cap-inbox").performScrollTo()
            .performTextReplacement("999")
        composeTestRule.onNodeWithTag("admin-settings-tier-save-button").performScrollTo().performClick()
        // Edited inbox; the untouched caps carry their persisted values.
        assertEquals(listOf<Any>("free", 999L, 200L, 3L, 50L, 10L), saved)
    }

    @Test
    fun saveTierFallsBackToPersistedOnEmptyCap() {
        var saved: List<Any>? = null
        render(
            tiers = listOf(tier("free", inbox = 100)),
            onSaveTier = { n, i, s, d, b, f -> saved = listOf(n, i, s, d, b, f) },
        )
        // Clearing a cap must not silently zero it — parseCap falls back to 100.
        composeTestRule.onNodeWithTag("admin-settings-tier-cap-inbox").performScrollTo()
            .performTextClearance()
        composeTestRule.onNodeWithTag("admin-settings-tier-save-button").performScrollTo().performClick()
        assertEquals(100L, (saved as List<*>)[1])
    }
    // The raw parse-and-clamp semantics now live in shared Rust
    // (`fauna_core::format::parse_cap`, Rust-tested) and are consumed via the
    // injected `parseCap`; `saveTierFallsBackToPersistedOnEmptyCap` above covers
    // the android-side `?: prev` fallback wiring, so no android-side semantic
    // duplicate is kept.

    // ── Membership designations (monetization.md § Pillar 4) ───────────────

    @Test
    fun membershipSectionRendersEmptyStateWithNoOwnedTiers() {
        render(ownMembershipTierNames = emptyList())
        composeTestRule.onNodeWithTag("admin-settings-membership-section").assertExists()
        composeTestRule.onAllNodesWithTag("admin-settings-membership-item")
            .assertCountEquals(0)
    }

    @Test
    fun membershipRowsRenderOnePerOwnedTier() {
        render(
            tiers = listOf(tier("free"), tier("personal")),
            ownMembershipTierNames = listOf("paid-a", "paid-b"),
            membershipTiers = listOf(membershipTier("paid-a", adminTier = "personal", lapseTier = "free")),
        )
        assertEquals(
            2,
            composeTestRule.onAllNodesWithTag("admin-settings-membership-item").fetchSemanticsNodes().size,
        )
    }

    @Test
    fun undesignatedRowDefaultsLapseTierToSharedDefault() {
        render(
            tiers = listOf(tier("free"), tier("personal")),
            ownMembershipTierNames = listOf("paid-a"),
            membershipTiers = emptyList(),
        )
        // The row's lapse-tier select must default to the shared
        // DEFAULT_LAPSE_TIER ("free") even before the row is ever designated.
        // assertTextContains (not assertTextEquals) — the node's merged text
        // also carries the field's floating label ("Lapses to").
        composeTestRule.onNodeWithTag("admin-settings-membership-lapse-tier-select")
            .assertTextContains("free")
    }

    @Test
    fun designatedRowShowsExistingAdminAndLapseTiers() {
        render(
            tiers = listOf(tier("free"), tier("personal"), tier("community")),
            ownMembershipTierNames = listOf("paid-a"),
            membershipTiers = listOf(membershipTier("paid-a", adminTier = "personal", lapseTier = "community")),
        )
        composeTestRule.onNodeWithTag("admin-settings-membership-admin-tier-select")
            .assertTextContains("personal")
        composeTestRule.onNodeWithTag("admin-settings-membership-lapse-tier-select")
            .assertTextContains("community")
    }

    @Test
    fun clearButtonDisabledWhenRowUndesignated() {
        render(
            tiers = listOf(tier("free")),
            ownMembershipTierNames = listOf("paid-a"),
            membershipTiers = emptyList(),
        )
        composeTestRule.onNodeWithTag("admin-settings-membership-clear-button").assertIsNotEnabled()
    }

    @Test
    fun clearButtonEnabledWhenRowDesignated() {
        render(
            tiers = listOf(tier("free"), tier("personal")),
            ownMembershipTierNames = listOf("paid-a"),
            membershipTiers = listOf(membershipTier("paid-a")),
        )
        composeTestRule.onNodeWithTag("admin-settings-membership-clear-button").assertIsEnabled()
    }

    @Test
    fun saveMembershipTierFiresWithPickedQuotaTiers() {
        var saved: Triple<String, String, String>? = null
        render(
            tiers = listOf(tier("free"), tier("personal"), tier("community")),
            ownMembershipTierNames = listOf("paid-a"),
            membershipTiers = emptyList(),
            onSaveMembershipTier = { name, admit, lapse -> saved = Triple(name, admit, lapse) },
        )
        composeTestRule.onNodeWithTag("admin-settings-membership-admin-tier-select")
            .performScrollTo().performClick()
        composeTestRule.onAllNodesWithText("personal").onLast().performClick()
        composeTestRule.onNodeWithTag("admin-settings-membership-lapse-tier-select")
            .performScrollTo().performClick()
        composeTestRule.onAllNodesWithText("community").onLast().performClick()
        composeTestRule.onNodeWithTag("admin-settings-membership-save-button")
            .performScrollTo().performClick()
        assertEquals(Triple("paid-a", "personal", "community"), saved)
    }

    @Test
    fun saveMembershipTierNoOpsWithoutAnAdminTierPicked() {
        var saveCalled = false
        render(
            tiers = listOf(tier("free")),
            ownMembershipTierNames = listOf("paid-a"),
            membershipTiers = emptyList(),
            onSaveMembershipTier = { _, _, _ -> saveCalled = true },
        )
        // No admin-tier has been picked yet (blank by default on an
        // undesignated row) — Save must not fire a request the nest would
        // refuse with fauna.admin.invalid_params.
        composeTestRule.onNodeWithTag("admin-settings-membership-save-button")
            .performScrollTo().performClick()
        assertFalse(saveCalled)
    }

    @Test
    fun clearMembershipTierFiresWithRowIdentity() {
        var cleared: String? = null
        render(
            tiers = listOf(tier("free"), tier("personal")),
            ownMembershipTierNames = listOf("paid-a"),
            membershipTiers = listOf(membershipTier("paid-a")),
            onClearMembershipTier = { cleared = it },
        )
        composeTestRule.onNodeWithTag("admin-settings-membership-clear-button")
            .performScrollTo().performClick()
        assertEquals("paid-a", cleared)
    }
}
