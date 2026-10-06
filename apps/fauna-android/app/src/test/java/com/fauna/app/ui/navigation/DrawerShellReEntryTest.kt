package com.fauna.app.ui.navigation

import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * Measures android's **shell re-entry landing** — `docs/goal/ui/README.md`
 * § Navigation model → *Entering a shell lands on its canonical entry*
 * (ratified 2026-08-13): crossing into a shell must land on the shell's
 * canonical entry, never on the sub-page a previous visit left open.
 *
 * ## Why this test exists, and why it is headless
 *
 * android was the one app the 2026-08-13 seven-app sweep could not classify
 * from source. Every other app's landing is readable (a URL, a fresh page
 * instance, an explicit reset, a persisted property); android's is not — the
 * drawer navigates with [navigateToDrawerRoute]'s
 * `popUpTo(start) { saveState = true }` + `restoreState = true` over a **flat**
 * graph, where `settings` and `settings/account` are siblings rather than a
 * nested graph and its child. Whether Navigation-Compose's saved-back-stack
 * machinery then restores `settings/account` as the visible destination, or
 * lands on the `settings` root, is a property of that machinery, not of
 * anything we wrote. So it had to be **driven**, not read.
 *
 * It does **not** need the emulator. The rule of thumb is to name the exact
 * observable that would need a human eye before accepting that a check cannot
 * be automated — and here the observable is a `NavController` destination, not
 * a pixel, so no eye is required.
 *
 * ## What is real here and what is a stand-in
 *
 * Real: [navigateToDrawerRoute] — the production nav options, called by the
 * drawer's own `onClick`; the flat graph shape; the real route strings; the
 * real start destination (`conversations`).
 *
 * Stood in for: the destination *bodies*. The shipped graph builds ~80
 * screens, each pulling Hilt view models, the FFI and Room — none of which the
 * saved-back-stack question depends on, and two of which are known local
 * hazards (Robolectric cannot open SQLite on ARM64; heavily-composed screens
 * flake with `AppNotIdleException`). The destinations here are bare [Text]s.
 *
 * ⚠ This grades the **nav edge**, which is where the rule lives and where a
 * fix would go. It does not grade what a human sees after that edge — the
 * qualification-4 blind spot that let linux pass every e2e assertion while the
 * sidebar-clicking user still hit the stale sub-page. An emulator walk through
 * the real drawer stays owed; this test is what makes the *mechanism* honest
 * in the meantime.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class DrawerShellReEntryTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    /** The real start destination of the shipped graph (`FaunaNavHost.kt`). */
    private val start = "conversations"

    /**
     * The shipped graph's shape, minus the screen bodies: every route a flat
     * sibling of every other, exactly as `FaunaNavHost.kt` declares them.
     */
    private val routes = listOf(
        start,
        "contacts",
        "settings",
        "settings/account",
        "settings/privacy",
        "settings/admin",
        "settings/admin-users",
    )

    private lateinit var nav: NavHostController

    private fun render() {
        composeTestRule.setContent {
            nav = rememberNavController()
            NavHost(nav, start) {
                routes.forEach { route -> composable(route) { Body(route) } }
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

    /** Drive an in-shell sub-page tap (a plain `navigate`, as the screens do). */
    private fun subPageTo(route: String) {
        composeTestRule.runOnUiThread { nav.navigate(route) }
        composeTestRule.waitForIdle()
    }

    private fun landing(): String? =
        composeTestRule.runOnIdle { nav.currentBackStackEntry?.destination?.route }

    @Test
    fun `re-entering settings from another shell lands on the settings root`() {
        render()

        drawerTo("settings")
        assertEquals("settings", landing())

        subPageTo("settings/account")
        assertEquals("settings/account", landing())

        // Leave the shell entirely, then come back through the drawer.
        drawerTo(start)
        assertEquals(start, landing())

        drawerTo("settings")
        assertEquals(
            "re-entering Settings must land on its canonical entry, not the " +
                "sub-page the previous visit left open (ui/README.md " +
                "§ Navigation model)",
            "settings",
            landing(),
        )
    }

    @Test
    fun `re-entering settings from a sibling drawer destination lands on the settings root`() {
        render()

        drawerTo("settings")
        subPageTo("settings/admin")
        subPageTo("settings/admin-users")
        assertEquals("settings/admin-users", landing())

        drawerTo("contacts")
        drawerTo("settings")
        assertEquals(
            "a deep sub-page stack must not be restored on shell re-entry",
            "settings",
            landing(),
        )
    }

    @Test
    fun `re-selecting the shell you are already in does not evict you from the sub-page`() {
        render()

        drawerTo("settings")
        subPageTo("settings/privacy")
        assertEquals("settings/privacy", landing())

        // Qualification 1: navigating to the shell you are ALREADY in is a
        // reload, not an edge — it must not throw the user out of the sub-page
        // they are on. An implementation that resets on every nav actuation
        // rather than on the edge is wrong in a way the rule does not license.
        drawerTo("settings")
        assertEquals(
            "re-selecting the current shell is a reload, not an entry edge",
            "settings/privacy",
            landing(),
        )
    }
}
