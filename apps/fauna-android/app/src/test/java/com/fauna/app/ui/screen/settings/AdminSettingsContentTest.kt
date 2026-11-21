package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.ffi.FfiAdminMembershipTier
import com.fauna.ffi.FfiAdminTier
import uniffi.fauna_core.Possibility
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.R
import org.robolectric.RuntimeEnvironment
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

    /** A cap input inside a tier row (not the add-a-tier form's same-id inputs). */
    private fun rowNode(tag: String) = composeTestRule.onNode(
        hasTestTag(tag) and hasAnyAncestor(hasTestTag("admin-settings-tier-item")),
    )

    /** A cap input / control inside the add-a-tier form. */
    private fun addNode(tag: String) = composeTestRule.onNode(
        hasTestTag(tag) and hasAnyAncestor(hasTestTag("admin-settings-tier-add-section")),
    )

    private fun render(
        tiers: List<FfiAdminTier> = listOf(tier("free")),
        ownMembershipTierNames: List<String> = emptyList(),
        membershipTiers: List<FfiAdminMembershipTier> = emptyList(),
        onSaveTier: (String, Long, Long, Long, Long, Long) -> Unit = { _, _, _, _, _, _ -> },
        onDeleteTier: (String) -> Unit = {},
        onAddTier: (String, Long, Long, Long, Long, Long) -> Unit = { _, _, _, _, _, _ -> },
        onRefused: (String) -> Unit = {},
        onSaveMembershipTier: (String, String, String) -> Unit = { _, _, _ -> },
        onClearMembershipTier: (String) -> Unit = {},
        // FFI-free stub mirroring the shared `parse_cap` (trim, parse i64, clamp ≥0)
        // so Robolectric stays off the native path — no host JNA needed.
        parseCap: (String) -> Long? = { it.trim().toLongOrNull()?.coerceAtLeast(0) },
        // FFI-free stub of `tier_delete_possibility`: absent on `free`.
        tierDeletePossibility: (String) -> Possibility = {
            if (it == "free") Possibility.Absent else Possibility.Possible
        },
        addTierAnswer: Possibility = Possibility.Possible,
    ) {
        composeTestRule.setContent {
            AdminSettingsContent(
                tiers = tiers,
                ownMembershipTierNames = ownMembershipTierNames,
                membershipTiers = membershipTiers,
                onBack = {},
                onSaveTier = onSaveTier,
                onDeleteTier = onDeleteTier,
                onAddTier = onAddTier,
                onRefused = onRefused,
                onSaveMembershipTier = onSaveMembershipTier,
                onClearMembershipTier = onClearMembershipTier,
                parseCap = parseCap,
                tierDeletePossibility = tierDeletePossibility,
                // FFI-free stand-ins for the shared draft answers.
                drafts = AdminSettingsDrafts(
                    tierCaps = AdminDraftFakes.tierCaps,
                    tierCreate = AdminDraftFakes.tierCreate,
                    membershipSave = AdminDraftFakes.membershipSave,
                ),
                addTierAnswer = addTierAnswer,
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
        // Each cap id also appears once in the add-a-tier form (3 = 2 rows + the form).
        assertEquals(3, composeTestRule.onAllNodesWithTag("admin-settings-tier-cap-inbox").fetchSemanticsNodes().size)
        assertEquals(3, composeTestRule.onAllNodesWithTag("admin-settings-tier-cap-feeds").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-settings-tier-save-button").fetchSemanticsNodes().size)
    }

    @Test
    fun aDeletableTierRowCarriesADeleteButtonThatFiresWithItsName() {
        val deleted = mutableListOf<String>()
        render(
            tiers = listOf(tier("free"), tier("personal")),
            onDeleteTier = { deleted += it },
        )
        // `free` is never removable (`tier_delete_possibility` answers absent,
        // admin.md § 3 → Removing a tier), so only `personal`'s row offers it.
        assertEquals(1, composeTestRule.onAllNodesWithTag("admin-settings-tier-delete-button").fetchSemanticsNodes().size)
        composeTestRule.onAllNodesWithTag("admin-settings-tier-delete-button")[0].performScrollTo().performClick()
        assertEquals(listOf("personal"), deleted)
    }

    @Test
    fun saveTierFiresWithEditedCaps() {
        var saved: List<Any>? = null
        render(
            tiers = listOf(tier("free", inbox = 100, storage = 200, devices = 3, blob = 50, feeds = 10)),
            onSaveTier = { n, i, s, d, b, f -> saved = listOf(n, i, s, d, b, f) },
        )
        rowNode("admin-settings-tier-cap-inbox").performScrollTo()
            .performTextReplacement("999")
        composeTestRule.onNodeWithTag("admin-settings-tier-save-button").performScrollTo().performClick()
        // Edited inbox; the untouched caps carry their persisted values.
        assertEquals(listOf<Any>("free", 999L, 200L, 3L, 50L, 10L), saved)
    }

    private fun string(id: Int) = RuntimeEnvironment.getApplication().getString(id)

    @Test
    fun aBlankCapBlocksTheTierSaveAndNeverSendsThePersistedValue() {
        var saved: List<Any>? = null
        render(
            tiers = listOf(tier("free", inbox = 100)),
            onSaveTier = { n, i, s, d, b, f -> saved = listOf(n, i, s, d, b, f) },
        )
        // Clearing a cap blocks the save with the shared reason — it is
        // neither zeroed nor silently replaced by the persisted 100.
        rowNode("admin-settings-tier-cap-inbox").performScrollTo()
            .performTextClearance()
        composeTestRule.onNodeWithTag("admin-settings-tier-save-button").performScrollTo()
            .assertIsNotEnabled().performClick()
        composeTestRule.onNodeWithText(string(R.string.admin_settings_page_save_tier_error_invalid_cap)).assertExists()
        assertEquals(null, saved)
        rowNode("admin-settings-tier-cap-inbox").performScrollTo().performTextReplacement("5")
        composeTestRule.onNodeWithTag("admin-settings-tier-save-button").performScrollTo().performClick()
        assertEquals(5L, (saved as List<*>)[1])
    }
    private fun fillAddForm(name: String, cap: String = "1") {
        addNode("admin-settings-tier-add-name-input").performScrollTo().performTextReplacement(name)
        for (id in listOf("inbox", "storage", "devices", "blob-size", "feeds")) {
            addNode("admin-settings-tier-cap-$id").performScrollTo().performTextReplacement(cap)
        }
    }

    @Test
    fun addTierFormRendersEvenWithNoTiers() {
        render(tiers = emptyList())
        composeTestRule.onNodeWithTag("admin-settings-tier-add-section").assertExists()
        composeTestRule.onNodeWithTag("admin-settings-tier-add-name-input").assertExists()
        composeTestRule.onNodeWithTag("admin-settings-tier-add-button").assertExists()
    }

    @Test
    fun addTierFiresWithTrimmedNameAndParsedCaps() {
        var added: List<Any>? = null
        render(onAddTier = { n, i, s, d, b, f -> added = listOf(n, i, s, d, b, f) })
        fillAddForm("  gold ", cap = "7")
        addNode("admin-settings-tier-add-button").performScrollTo().performClick()
        assertEquals(listOf<Any>("gold", 7L, 7L, 7L, 7L, 7L), added)
    }

    @Test
    fun addTierIsBlockedWithTheSharedReasonForAnEmptyNameOrABlankCap() {
        var added = false
        render(onAddTier = { _, _, _, _, _, _ -> added = true })
        fillAddForm("")
        addNode("admin-settings-tier-add-button").performScrollTo().assertIsNotEnabled().performClick()
        composeTestRule.onNodeWithText(string(R.string.admin_settings_page_add_tier_error_empty_name)).assertExists()
        fillAddForm("gold")
        addNode("admin-settings-tier-cap-feeds").performScrollTo().performTextClearance()
        addNode("admin-settings-tier-add-button").performScrollTo().assertIsNotEnabled().performClick()
        composeTestRule.onNodeWithText(string(R.string.admin_settings_page_add_tier_error_invalid_cap)).assertExists()
        assertFalse("a blocked form must not reach the wire", added)
        // The drafts stay: the name is still there.
        addNode("admin-settings-tier-add-name-input").assertTextContains("gold")
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
            .performScrollTo().assertIsNotEnabled().performClick()
        composeTestRule.onNodeWithText(
            string(R.string.admin_settings_page_save_membership_tier_error_no_tier),
        ).assertExists()
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


    // ── Controls wait for their read (admin.md § Errors & edge cases) ──

    @Test
    fun tierAddFormWaitsForTheTiersRead() {
        var added = false
        render(addTierAnswer = ReadGateFakes.stillLoading, onAddTier = { _, _, _, _, _, _ -> added = true })
        composeTestRule.onNodeWithTag("admin-settings-tier-add-name-input")
            .performScrollTo()
            .assertIsNotEnabled()
            .assertPaintedReason(ReadGateFakes.stillLoadingText)
        // The read's reason, not the empty-name draft complaint.
        composeTestRule.onNodeWithTag("admin-settings-tier-add-button")
            .performScrollTo()
            .assertIsNotEnabled()
            .assertPaintedReason(ReadGateFakes.stillLoadingText)
            .performClick()
        assertEquals(false, added)
    }

    @Test
    fun tierAddFormSaysFailedAfterAFailedRead() {
        render(addTierAnswer = ReadGateFakes.loadFailed)
        composeTestRule.onNodeWithTag("admin-settings-tier-add-button")
            .performScrollTo()
            .assertIsNotEnabled()
            .assertPaintedReason(ReadGateFakes.loadFailedText)
    }

    @Test
    fun tierAddNameInputLiveOnceRead() {
        render(addTierAnswer = Possibility.Possible)
        composeTestRule.onNodeWithTag("admin-settings-tier-add-name-input")
            .performScrollTo()
            .assertIsEnabled()
            .assertNoPaintedReason()
    }
}
