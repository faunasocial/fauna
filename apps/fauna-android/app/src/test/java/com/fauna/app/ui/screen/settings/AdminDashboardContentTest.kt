package com.fauna.app.ui.screen.settings

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiAdminDashboardSnapshot
import com.fauna.ffi.FfiStatCard
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText

/**
 * Compose-level coverage for the stateless [AdminDashboardContent]: the page
 * paints the shared card set (`admin_dashboard_snapshot()`, admin.md § 1.
 * Dashboard → *The card set*) and nothing of its own — every card an indexed
 * `admin-stat-card` with its `-label` / `-value`, in the fold's order, each
 * [LocalizedText] resolved against the generated resources — and a failed
 * read rides `error-message` while the other reads' cards still paint. The
 * snapshot is seeded, so no Hilt, VM or native FFI. (Android E2E
 * `test_admin.py`'s dashboard witnesses are the device gate once the run
 * venue lands.)
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AdminDashboardContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun verbatim(text: String) =
        LocalizedText("admin.dashboard.verbatim", mapOf("text" to text))

    private fun card(labelKey: String, value: String) =
        FfiStatCard(LocalizedText(labelKey, emptyMap()), verbatim(value))

    private val cards = listOf(
        card("admin.view.registered_users", "3"),
        card("admin.dashboard.suspended", "0"),
        FfiStatCard(verbatim("free"), verbatim("3")),
        card("admin.dashboard.connections", "5"),
        card("admin.dashboard.version", "1.2.3"),
    )

    private fun render(snapshot: FfiAdminDashboardSnapshot?, error: String? = snapshot?.error) {
        composeTestRule.setContent {
            AdminDashboardContent(
                snapshot = snapshot,
                isLoading = false,
                error = error,
                onBack = {},
                onRefresh = {},
                onNavigate = {},
            )
        }
    }

    private fun texts(tag: String): List<String> =
        composeTestRule.onAllNodesWithTag(tag, useUnmergedTree = true)
            .fetchSemanticsNodes()
            .map { node ->
                node.config.getOrElse(SemanticsProperties.Text) { emptyList() }
                    .joinToString("") { it.text }
            }

    @Test
    fun paintsEverySharedCardTaggedInOrder() {
        render(FfiAdminDashboardSnapshot(stats = cards, error = null))
        assertEquals(
            cards.size,
            composeTestRule.onAllNodesWithTag("admin-stat-card", useUnmergedTree = true)
                .fetchSemanticsNodes().size,
        )
        assertEquals(
            listOf("Registered Users", "Suspended", "free", "Connections", "Version"),
            texts("admin-stat-card-label"),
        )
        assertEquals(listOf("3", "0", "3", "5", "1.2.3"), texts("admin-stat-card-value"))
        assertEquals(
            0,
            composeTestRule.onAllNodesWithTag("error-message").fetchSemanticsNodes().size,
        )
    }

    @Test
    fun aFailedReadRidesErrorMessageAndTheOtherCardsStillPaint() {
        render(FfiAdminDashboardSnapshot(stats = cards.take(2), error = "status: boom"))
        assertEquals(
            listOf("Registered Users", "Suspended"),
            texts("admin-stat-card-label"),
        )
        assertEquals(listOf("Failed to load admin stats: status: boom"), texts("error-message"))
    }

    @Test
    fun everyAdminEntryIsAKeyedRailRowThatOpensItsPage() {
        // ui.yaml navigation.sub_page_nav_rows: admin-nav-row[<page key>], the
        // key being the page the entry opens (the route is `settings/<key>`).
        val keys = listOf(
            "admin-users", "admin-settings", "admin-nest", "admin-mail", "admin-calendar",
            "admin-contacts", "admin-files", "admin-aliases", "admin-web",
            "admin-bridges-pending", "admin-dns", "admin-custody-hosting", "admin-logs",
        )
        val routes = mutableListOf<String>()
        composeTestRule.setContent {
            AdminDashboardContent(
                snapshot = null,
                isLoading = false,
                error = null,
                onBack = {},
                onRefresh = {},
                onNavigate = { routes += it },
            )
        }
        keys.forEach { key ->
            composeTestRule.onNodeWithTag("admin-nav-row[$key]").performScrollTo().performClick()
        }
        assertEquals(keys.map { "settings/$it" }, routes)
    }

    @Test
    fun beforeTheSnapshotLoadsNoCardIsPainted() {
        render(null)
        composeTestRule.onNodeWithTag("admin-dashboard-heading").assertExists()
        assertEquals(
            0,
            composeTestRule.onAllNodesWithTag("admin-stat-card", useUnmergedTree = true)
                .fetchSemanticsNodes().size,
        )
    }
}
