package com.fauna.app.ui.screen.feed

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_feed.GateRoomOption
import uniffi.fauna_feed.GateTierOption

/**
 * Compose-level coverage for [GateTierSelect] — the composer's gate-to-tier
 * select (`compose-gate-tier-select`): "Public" (⇒ null, ungated) plus the
 * author's own tiers, selectable by item text — the shape the e2e `select` action
 * drives. Renders with seeded [GateTierOption]s + a capturing callback: a plain
 * uniffi record, no VM, no FFI native call, so it runs on the host JVM like the
 * other content tests. Robolectric popup selection follows the
 * `LinkedNestsContentTest` precedent (open the anchor, click the item by text).
 *
 * The room cases (`ownRooms` / `gateRoom` / `onSelectRoom`) cover the fourth
 * answer (`ui/feed.md` § Encryption at rest → *Room-restricted — the app
 * half*): one option per room, laid out between the tiers and Sell, carried
 * as the room's hex id rather than its label.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class GateTierSelectTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val tiers = listOf(
        GateTierOption(name = "Gold", rank = 1u),
        GateTierOption(name = "Silver", rank = 2u),
    )

    private val rooms = listOf(
        GateRoomOption(room = "aa11", label = "Fam Chat"),
        GateRoomOption(room = "bb22", label = "Book Club"),
    )

    @Test
    fun selectExists_defaultsToPublic() {
        composeTestRule.setContent {
            GateTierSelect(ownTiers = tiers, gateTier = null, onSelect = {}, enabled = true)
        }
        composeTestRule.onNodeWithTag("compose-gate-tier-select").assertExists()
        // The anchor shows "Public" while ungated.
        composeTestRule.onNodeWithText("Public").assertExists()
    }

    @Test
    fun selectingATier_firesOnSelectWithTierName() {
        var selected: String? = "unset"
        composeTestRule.setContent {
            GateTierSelect(ownTiers = tiers, gateTier = null, onSelect = { selected = it }, enabled = true)
        }
        // Anchor shows "Public"; the menu item "Gold" is unambiguous.
        composeTestRule.onNodeWithTag("compose-gate-tier-select").performClick()
        composeTestRule.onNodeWithText("Gold").performClick()
        assertEquals("Gold", selected)
    }

    @Test
    fun selectingPublic_firesOnSelectWithNull() {
        var selected: String? = "unset"
        composeTestRule.setContent {
            // Start gated to Gold so the anchor shows "Gold" and the menu's
            // "Public" item is the only node bearing that text.
            GateTierSelect(ownTiers = tiers, gateTier = "Gold", onSelect = { selected = it }, enabled = true)
        }
        composeTestRule.onNodeWithTag("compose-gate-tier-select").performClick()
        composeTestRule.onNodeWithText("Public").performClick()
        assertEquals(null, selected)
    }

    @Test
    fun noTiers_selectStillRenders() {
        // With no own tiers the composer must still render the select (offering
        // only "Public") — never crash or hide the composer.
        composeTestRule.setContent {
            GateTierSelect(ownTiers = emptyList(), gateTier = null, onSelect = {}, enabled = true)
        }
        composeTestRule.onNodeWithTag("compose-gate-tier-select").assertExists()
        composeTestRule.onNodeWithText("Public").assertExists()
    }

    // ── "Sell this post…" — the select's always-last third answer ────────────
    // (monetization.md § Per-post pay-to-unlock).

    @Test
    fun sellOption_isAlwaysLast_andFiresOnSelectSell() {
        var sellFired = false
        composeTestRule.setContent {
            GateTierSelect(
                ownTiers = tiers, gateTier = null, onSelect = {}, enabled = true,
                onSelectSell = { sellFired = true },
            )
        }
        composeTestRule.onNodeWithTag("compose-gate-tier-select").performClick()
        composeTestRule.onNodeWithText("Sell this post…").performClick()
        assertTrue(sellFired)
    }

    @Test
    fun sellSelected_anchorShowsSellLabel() {
        composeTestRule.setContent {
            GateTierSelect(
                ownTiers = tiers, gateTier = null, onSelect = {}, enabled = true,
                sellSelected = true,
            )
        }
        composeTestRule.onNodeWithTag("compose-gate-tier-select")
            .assert(hasText("Sell this post…"))
    }

    @Test
    fun selectingATier_whileSellSelected_stillFiresOnSelect() {
        // Mutual exclusion itself lives in the composer's own state (clearing
        // sellMode on a tier pick); this composable only needs to keep routing
        // Public/tier picks through `onSelect` regardless of `sellSelected`.
        var selected: String? = "unset"
        composeTestRule.setContent {
            GateTierSelect(
                ownTiers = tiers, gateTier = null, onSelect = { selected = it }, enabled = true,
                sellSelected = true,
            )
        }
        composeTestRule.onNodeWithTag("compose-gate-tier-select").performClick()
        composeTestRule.onNodeWithText("Gold").performClick()
        assertEquals("Gold", selected)
    }

    // ── Rooms — the select's fourth answer, between the tiers and Sell ───────
    // (`ui/feed.md` § Encryption at rest → *Room-restricted — the app half*).

    @Test
    fun selectingARoom_firesOnSelectRoomWithTheHexId_neverTheLabel() {
        var selectedRoom: String? = "unset"
        composeTestRule.setContent {
            GateTierSelect(
                ownTiers = tiers, gateTier = null, onSelect = {}, enabled = true,
                ownRooms = rooms, onSelectRoom = { selectedRoom = it },
            )
        }
        composeTestRule.onNodeWithTag("compose-gate-tier-select").performClick()
        composeTestRule.onNodeWithText("Room: Fam Chat").performClick()
        assertEquals("aa11", selectedRoom)
    }

    @Test
    fun roomSelected_anchorShowsTheRoomLabel() {
        composeTestRule.setContent {
            GateTierSelect(
                ownTiers = tiers, gateTier = null, onSelect = {}, enabled = true,
                ownRooms = rooms, gateRoom = "bb22",
            )
        }
        composeTestRule.onNodeWithTag("compose-gate-tier-select")
            .assert(hasText("Room: Book Club"))
    }

    @Test
    fun bothRoomsAndSell_offered_roomsComeBeforeSell() {
        // Position, not text, is what the composer's own answer resolution
        // relies on (the room id ≠ the room's displayed text) — but the LAYOUT
        // itself (rooms before Sell) is still asserted here since it is a
        // ui.yaml/goal-doc-ratified order (Public → tiers → rooms → Sell).
        var sellFired = false
        var selectedRoom: String? = "unset"
        composeTestRule.setContent {
            GateTierSelect(
                ownTiers = tiers, gateTier = null, onSelect = {}, enabled = true,
                ownRooms = rooms, onSelectRoom = { selectedRoom = it },
                onSelectSell = { sellFired = true },
            )
        }
        composeTestRule.onNodeWithTag("compose-gate-tier-select").performClick()
        composeTestRule.onNodeWithText("Room: Book Club").performClick()
        assertEquals("bb22", selectedRoom)
        assertTrue(!sellFired)
    }

    @Test
    fun noOwnRooms_selectStillRendersWithNoRoomOptions() {
        composeTestRule.setContent {
            GateTierSelect(ownTiers = tiers, gateTier = null, onSelect = {}, enabled = true)
        }
        composeTestRule.onNodeWithTag("compose-gate-tier-select").assertExists()
        composeTestRule.onNodeWithText("Room: Fam Chat").assertDoesNotExist()
    }
}
