package com.fauna.app.ui.components

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.ffi.FfiAccountEntry
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [AccountSwitcherSection] (Account
 * settings → Accounts, `long-term-store.md` § Multi-account evolution).
 * [FfiAccountEntry] is a plain record — constructible with no host `.so` — so this
 * test is FFI-free, no Hilt, no VM (mirrors `FoldersContentTest`). Covers Stage 1
 * (switch / remove / add) plus the Stage-2 `account-require-confirm-toggle` render
 * + callback; the biometric gate the toggle guards is driven at the caller and
 * proven by the (host-gated) cross-app e2e.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AccountSwitcherSectionTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun entry(actorId: String, handle: String? = null, requireConfirm: Boolean = false) =
        FfiAccountEntry(
            actorId = actorId,
            handle = handle,
            domain = null,
            tier = null,
            requireConfirmToActivate = requireConfirm,
        )

    @Test
    fun `active row shows the indicator and no remove button, non-active rows show remove`() {
        val a = entry("aaaa", "alice")
        val b = entry("bbbb", "bob")
        composeTestRule.setContent {
            AccountSwitcherSection(
                accounts = listOf(a, b),
                activeActorId = "aaaa",
                label = { it.handle ?: it.actorId },
                onSwitch = {},
                onRemove = {},
                onRequireConfirmToggle = { _, _ -> },
                onAddAccount = {},
            )
        }

        composeTestRule.onNodeWithTag("account-switcher-list").assertExists()
        composeTestRule.onNodeWithTag("account-switcher-item[0]").assertExists()
        composeTestRule.onNodeWithTag("account-item-handle[0]").assertTextEquals("alice")
        composeTestRule.onNodeWithTag("account-item-active-indicator[0]").assertExists()
        composeTestRule.onNodeWithTag("account-remove-button[0]").assertDoesNotExist()

        // Row 1 is non-active → clickable, which merges its descendants' semantics
        // into the row node (the same reason apple's Section doc warns off putting
        // ids on a bare merging container) — query the unmerged tree for its children.
        composeTestRule.onNodeWithTag("account-item-handle[1]", useUnmergedTree = true).assertTextEquals("bob")
        composeTestRule.onNodeWithTag("account-item-active-indicator[1]", useUnmergedTree = true).assertDoesNotExist()
        composeTestRule.onNodeWithTag("account-remove-button[1]", useUnmergedTree = true).assertExists()
    }

    @Test
    fun `tapping a non-active row switches, tapping the active row does nothing`() {
        val a = entry("aaaa")
        val b = entry("bbbb")
        var switchedTo: String? = null
        composeTestRule.setContent {
            AccountSwitcherSection(
                accounts = listOf(a, b),
                activeActorId = "aaaa",
                label = { it.actorId },
                onSwitch = { switchedTo = it },
                onRemove = {},
                onRequireConfirmToggle = { _, _ -> },
                onAddAccount = {},
            )
        }

        // The active row (index 0) has no clickable modifier attached — a click on
        // it must not register (there's no click semantics action to perform).
        composeTestRule.onNodeWithTag("account-switcher-item[1]").performClick()
        assertEquals("bbbb", switchedTo)
    }

    @Test
    fun `remove fires the callback with the tapped row's actor id`() {
        val a = entry("aaaa")
        val b = entry("bbbb")
        var removed: String? = null
        composeTestRule.setContent {
            AccountSwitcherSection(
                accounts = listOf(a, b),
                activeActorId = "aaaa",
                label = { it.actorId },
                onSwitch = {},
                onRemove = { removed = it },
                onRequireConfirmToggle = { _, _ -> },
                onAddAccount = {},
            )
        }

        composeTestRule.onNodeWithTag("account-remove-button[1]", useUnmergedTree = true).performClick()
        assertEquals("bbbb", removed)
    }

    @Test
    fun `add account row always renders and fires its callback`() {
        var addTapped = false
        composeTestRule.setContent {
            AccountSwitcherSection(
                accounts = listOf(entry("aaaa")),
                activeActorId = "aaaa",
                label = { it.actorId },
                onSwitch = {},
                onRemove = {},
                onRequireConfirmToggle = { _, _ -> },
                onAddAccount = { addTapped = true },
            )
        }

        composeTestRule.onNodeWithTag("account-add-button").performClick()
        assertEquals(true, addTapped)
    }

    @Test
    fun `require-confirm toggle renders on every row incl active and fires with actor id and value`() {
        val a = entry("aaaa", "alice") // active
        val b = entry("bbbb", "bob")
        var toggled: Pair<String, Boolean>? = null
        composeTestRule.setContent {
            AccountSwitcherSection(
                accounts = listOf(a, b),
                activeActorId = "aaaa",
                label = { it.handle ?: it.actorId },
                onSwitch = {},
                onRemove = {},
                onRequireConfirmToggle = { actorId, require -> toggled = actorId to require },
                onAddAccount = {},
            )
        }

        // On EVERY row, including the active one (row 0) — the natural admin target
        // is usually the account you are already on (long-term-store.md § Per-account
        // re-auth).
        val toggles = composeTestRule.onAllNodesWithTag("account-require-confirm-toggle", useUnmergedTree = true)
        toggles.assertCountEquals(2)

        // The active row's toggle is live too, and each carries its own actor id.
        toggles[0].performClick()
        assertEquals("aaaa" to true, toggled)
        toggles[1].performClick()
        assertEquals("bbbb" to true, toggled)
    }

    @Test
    fun `require-confirm toggle reflects the entry flag`() {
        composeTestRule.setContent {
            AccountSwitcherSection(
                accounts = listOf(entry("aaaa", "alice", requireConfirm = true)),
                activeActorId = "aaaa",
                label = { it.handle ?: it.actorId },
                onSwitch = {},
                onRemove = {},
                onRequireConfirmToggle = { _, _ -> },
                onAddAccount = {},
            )
        }

        composeTestRule.onNodeWithTag("account-require-confirm-toggle", useUnmergedTree = true).assertIsOn()
    }
}
