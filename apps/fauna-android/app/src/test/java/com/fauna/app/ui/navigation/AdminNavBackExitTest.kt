package com.fauna.app.ui.navigation

import androidx.compose.material3.Text
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
 * Measures android's **`admin-nav-back` exit** — `docs/goal/behavior/admin.md`
 * § Navigation model: leaving admin lands on the primary view (Conversations),
 * not on the page the user reached admin from.
 *
 * The admin pages are flat `settings/admin*` siblings stacked over Settings, so
 * a one-entry pop (what every admin screen's back used to do) returns a user
 * who came in through Settings to Settings. This drives the production exit,
 * [exitAdminToPrimary], over the same flat graph shape as
 * [DrawerShellReEntryTest]; the destination bodies are bare [Text]s for the
 * reasons given there. The e2e harness cannot grade this on android: its
 * state-protocol entry pushes `settings/admin` straight over the primary view,
 * so a pop lands on the primary view by artifact.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AdminNavBackExitTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val start = "conversations"

    private val routes = listOf(
        start,
        "settings",
        "settings/admin",
        "settings/admin-users",
        "settings/admin-nest",
    )

    private lateinit var nav: NavHostController

    private fun render() {
        composeTestRule.setContent {
            nav = rememberNavController()
            NavHost(nav, start) {
                routes.forEach { route -> composable(route) { Text(route) } }
            }
        }
        composeTestRule.waitForIdle()
    }

    private fun go(route: String) {
        composeTestRule.runOnUiThread { nav.navigate(route) }
        composeTestRule.waitForIdle()
    }

    private fun exitAdmin() {
        composeTestRule.runOnUiThread { nav.exitAdminToPrimary() }
        composeTestRule.waitForIdle()
    }

    private fun landing(): String? =
        composeTestRule.runOnIdle { nav.currentBackStackEntry?.destination?.route }

    @Test
    fun `admin back from a page entered through Settings lands on the primary view`() {
        render()

        // The user's real path: Conversations → Settings → Admin → an admin page.
        go("settings")
        go("settings/admin")
        go("settings/admin-users")
        assertEquals("settings/admin-users", landing())

        exitAdmin()
        assertEquals(
            "admin-nav-back must leave admin for the primary view, not pop " +
                "one entry back into Settings (admin.md § Navigation model)",
            start,
            landing(),
        )
    }

    @Test
    fun `admin back from the admin dashboard lands on the primary view`() {
        render()

        go("settings")
        go("settings/admin")

        exitAdmin()
        assertEquals(start, landing())
    }

    @Test
    fun `admin back leaves nothing of admin or settings to pop back into`() {
        render()

        go("settings")
        go("settings/admin")
        go("settings/admin-nest")
        exitAdmin()

        // System back from the primary view is the app's own exit, not a
        // return into the admin or Settings pages just left.
        val popped = composeTestRule.runOnUiThread { nav.popBackStack() }
        assertEquals("the primary view is the stack's root", false, popped)
    }
}
