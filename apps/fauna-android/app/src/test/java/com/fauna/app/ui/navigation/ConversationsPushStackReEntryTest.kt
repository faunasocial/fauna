package com.fauna.app.ui.navigation

import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.navigation.NavHostController
import androidx.navigation.NavType
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.navigation.navArgument
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * Measures android's **push-stack page re-entry** — `docs/goal/ui/README.md`
 * § Navigation model → *A nav to a page shows that page's own primary
 * surface (push-stack pages; ratified 2026-08-28)*: a nav patch targeting a
 * single-pane push-stack page must return that page's own stack to its
 * root, so a covered detail from a previous visit cannot hide the page.
 *
 * android's `conversations` list and `conversation/{threadId}` detail are a
 * flat sibling pair in the shipped graph (`FaunaNavHost.kt`) — the detail is
 * pushed with a plain `navigate("conversation/$threadId")`
 * (`ConversationListScreen.kt`), not nested under `conversations`, so the
 * drawer's multi-stack `popUpTo(start) { saveState } + restoreState` idiom
 * ([DrawerShellReEntryTest]'s subject) can restore the covered detail on
 * re-entry exactly the way it can restore a shell's stale sub-page. This is
 * a **different** rule from the shell one — it targets an ordinary page, not
 * a shell — and this test drives it the same headless way: the real
 * [navigateToDrawerRoute] edge, real route strings, real start destination,
 * stand-in [Text] bodies for the ~80 screens the shipped graph would
 * otherwise pull Hilt/FFI/Room into.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ConversationsPushStackReEntryTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    /** The real start destination of the shipped graph (`FaunaNavHost.kt`). */
    private val start = "conversations"

    private lateinit var nav: NavHostController

    private fun render() {
        composeTestRule.setContent {
            nav = rememberNavController()
            NavHost(nav, start) {
                composable(start) { Body(start) }
                composable("contacts") { Body("contacts") }
                composable("settings") { Body("settings") }
                composable(
                    "conversation/{threadId}",
                    arguments = listOf(navArgument("threadId") { type = NavType.StringType }),
                ) { entry -> Body("conversation/${entry.arguments?.getString("threadId")}") }
            }
        }
        composeTestRule.waitForIdle()
    }

    @Composable
    private fun Body(route: String) = Text(route)

    /** Drive the real drawer edge, then settle. */
    private fun drawerTo(route: String) {
        composeTestRule.runOnUiThread { nav.navigateToDrawerRoute(route) }
        composeTestRule.waitForIdle()
    }

    /** Drive an in-page detail push (a plain `navigate`, as `ConversationListScreen` does). */
    private fun openThread(threadId: String) {
        composeTestRule.runOnUiThread { nav.navigate("conversation/$threadId") }
        composeTestRule.waitForIdle()
    }

    private fun landing(): String? =
        composeTestRule.runOnIdle { nav.currentBackStackEntry?.destination?.route }

    @Test
    fun `re-entering conversations from another drawer destination lands on the list, not a covered thread`() {
        render()

        openThread("123")
        assertEquals("conversation/{threadId}", landing())

        // Leave conversations for a sibling drawer destination — this is what
        // saves the covered detail into conversations' multi-stack slot.
        drawerTo("contacts")
        assertEquals("contacts", landing())

        drawerTo(start)
        assertEquals(
            "re-entering conversations must show its own primary surface — the " +
                "list — not the thread a previous visit left open (ui/README.md " +
                "§ Navigation model)",
            start,
            landing(),
        )
    }

    @Test
    fun `a fresh visit to conversations still opens a thread normally`() {
        render()

        openThread("456")
        assertEquals(
            "the push-stack reset must not block an ordinary in-page navigate",
            "conversation/{threadId}",
            landing(),
        )
    }
}
