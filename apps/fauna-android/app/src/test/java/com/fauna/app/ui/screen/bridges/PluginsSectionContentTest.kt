package com.fauna.app.ui.screen.bridges

import android.content.Context
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiInstalledPlugin
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText

/**
 * The render half of the admin's hosted plugins on the Bridges page
 * (`bridges.md` § Hosted plugins on this page; tui's `bridges.rs::plugin_elements`):
 * one `bridge-plugin-item` per installed plugin with its name, publisher and
 * status leaves, Uninstall behind an inline confirm, and the install entry. The
 * rows' texts are composed in shared Rust (`installed_plugin_text`); this seeds
 * them directly, so it pins the paint and the arm-then-confirm gesture only.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class PluginsSectionContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val ctx get() = ApplicationProvider.getApplicationContext<Context>()

    private fun plugin(name: String = "Hello Plugin", idHex: String = "0a0b", state: String = "running") =
        FfiInstalledPlugin(
            principalIdHex = idHex,
            name = name,
            clientId = "https://plugins.example/hello.json",
            state = state,
            publisher = LocalizedText("bridges.plugin_publisher_development", emptyMap()),
            status = listOf(LocalizedText("bridges.plugin_state_running", emptyMap())),
        )

    private fun render(
        plugins: List<FfiInstalledPlugin>? = emptyList(),
        installOpened: Boolean = false,
        onInstall: (String) -> Unit = {},
        onUninstall: (String) -> Unit = {},
    ) {
        composeTestRule.setContent {
            PluginsSection(
                plugins = plugins,
                installOpened = installOpened,
                onInstall = onInstall,
                onUninstall = onUninstall,
            )
        }
    }

    /** Nothing is painted for the list until the read returned: no rows, no empty line. */
    @Test
    fun beforeTheReadReturnedNeitherRowsNorTheEmptyLineShow() {
        render(plugins = null)
        composeTestRule.onNodeWithTag("bridge-plugin-item").assertDoesNotExist()
        composeTestRule.onNodeWithText(ctx.getString(R.string.bridges_plugins_empty)).assertDoesNotExist()
        composeTestRule.onNodeWithTag("bridge-plugin-install-url").assertExists()
    }

    @Test
    fun anEmptyListPaintsTheEmptyLine() {
        render(plugins = emptyList())
        composeTestRule.onNodeWithText(ctx.getString(R.string.bridges_plugins_empty)).assertExists()
    }

    /** The name carries the document URL, the status carries the runner state — attrs a test reads, not words. */
    @Test
    fun aRowPaintsItsNamePublisherAndStatusWithTheirAttrs() {
        render(plugins = listOf(plugin()))
        composeTestRule.onNodeWithTag("bridge-plugin-item").assertExists()
        composeTestRule.onNodeWithTag("bridge-plugin-name")
            .assertTextEquals("Hello Plugin")
            .assert(
                SemanticsMatcher.expectValue(
                    SemanticsProperties.StateDescription,
                    "https://plugins.example/hello.json",
                ),
            )
        composeTestRule.onNodeWithTag("bridge-plugin-publisher")
            .assertTextEquals(ctx.getString(R.string.bridges_plugin_publisher_development))
        composeTestRule.onNodeWithTag("bridge-plugin-status")
            .assertTextEquals(ctx.getString(R.string.bridges_plugin_state_running))
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, "running"))
    }

    /** The first press arms the confirm and uninstalls nothing; the armed press uninstalls THAT row. */
    @Test
    fun uninstallArmsFirstThenUninstallsTheArmedRow() {
        val uninstalled = mutableListOf<String>()
        render(plugins = listOf(plugin("One", "01"), plugin("Two", "02")), onUninstall = { uninstalled += it })
        val buttons = composeTestRule.onAllNodesWithTag("bridge-plugin-uninstall")
        buttons[1].performClick()
        assertEquals(emptyList<String>(), uninstalled)
        composeTestRule.onNodeWithText(ctx.getString(R.string.bridges_plugin_uninstall_confirm)).assertExists()
        composeTestRule.onNodeWithText(ctx.getString(R.string.bridges_plugin_uninstall_cancel)).assertExists()
        composeTestRule.onNodeWithText(ctx.getString(R.string.bridges_plugin_uninstall_confirm)).performClick()
        assertEquals(listOf("02"), uninstalled)
    }

    @Test
    fun keepDisarmsWithoutUninstalling() {
        val uninstalled = mutableListOf<String>()
        render(plugins = listOf(plugin()), onUninstall = { uninstalled += it })
        composeTestRule.onNodeWithTag("bridge-plugin-uninstall").performClick()
        composeTestRule.onNodeWithText(ctx.getString(R.string.bridges_plugin_uninstall_cancel)).performClick()
        composeTestRule.onNodeWithText(ctx.getString(R.string.bridges_plugin_uninstall_confirm)).assertDoesNotExist()
        assertEquals(emptyList<String>(), uninstalled)
    }

    /** Install is disabled while the URL is blank, and hands the typed URL over. */
    @Test
    fun installIsDisabledWhileBlankAndSendsTheTypedUrl() {
        val installed = mutableListOf<String>()
        render(onInstall = { installed += it })
        composeTestRule.onNodeWithTag("bridge-plugin-install-submit").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("bridge-plugin-install-url")
            .performTextInput("https://plugins.example/hello.json")
        composeTestRule.onNodeWithTag("bridge-plugin-install-submit").assertIsEnabled().performClick()
        assertEquals(listOf("https://plugins.example/hello.json"), installed)
    }

    @Test
    fun theOpenedHintPointsAtConnectedApps() {
        render(installOpened = true)
        composeTestRule.onNodeWithText(ctx.getString(R.string.bridges_plugin_install_opened)).assertExists()
    }
}
