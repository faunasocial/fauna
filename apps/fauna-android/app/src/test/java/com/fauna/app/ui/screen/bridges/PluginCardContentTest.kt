package com.fauna.app.ui.screen.bridges

import android.content.Context
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.util.AutomationAttributePack
import com.fauna.ffi.FfiBridgeSetting
import com.fauna.ffi.FfiCborValue
import com.fauna.ffi.FfiPluginCard
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText

/**
 * The render half of a hosted plugin's settings card on the Bridges page
 * (`third-party.md` § The principal model → *Hosted principals*; tui's
 * `bridges.rs::plugin_card_elements`): one `bridge-card` per installed plugin
 * whose `bridge-action-button` reads Connect while unbound and Disconnect while
 * bound, carries the plugin's `client_id` and a `bound` value, and — once bound —
 * paints the plugin's settings through the one settings renderer. The card's
 * texts are composed in shared Rust (`plugin_card_text`); this seeds them
 * directly, so it pins the paint and the gestures only.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class PluginCardContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val ctx get() = ApplicationProvider.getApplicationContext<Context>()

    private val clientId = "https://plugins.example/hello.json"

    private fun card(
        bound: Boolean = false,
        settings: List<FfiBridgeSetting> = emptyList(),
    ) = FfiPluginCard(
        principalIdHex = "0a0b",
        name = "Hello Plugin",
        clientId = clientId,
        badge = LocalizedText("bridges.plugin_card_badge", mapOf("form" to "wasm")),
        publisher = LocalizedText("bridges.plugin_publisher_development", emptyMap()),
        bound = bound,
        bindingPrincipalIdHex = if (bound) "0c0d" else null,
        settings = settings,
    )

    private fun stringSetting(settingType: String) = FfiBridgeSetting(
        key = "greeting",
        label = "Greeting",
        settingType = settingType,
        value = FfiCborValue.Text("hi"),
        options = null,
    )

    private fun render(
        card: FfiPluginCard,
        onPress: () -> Unit = {},
        onUpdateSetting: (String, FfiCborValue) -> Unit = { _, _ -> },
    ) {
        composeTestRule.setContent {
            PluginCard(card = card, onPress = onPress, onUpdateSetting = onUpdateSetting)
        }
    }

    /** The button names which plugin's card it is and whether it is connected — attrs a test reads. */
    @Test
    fun anUnboundCardOffersConnectAndNamesItsPlugin() {
        render(card(bound = false))
        composeTestRule.onNodeWithTag("bridge-card").assertExists()
        composeTestRule.onNodeWithTag("bridge-action-button")
            .assertTextEquals(ctx.getString(R.string.bridges_plugin_connect))
            .assert(
                SemanticsMatcher.expectValue(
                    SemanticsProperties.StateDescription,
                    AutomationAttributePack.pack("client_id" to clientId, "bound" to "false"),
                ),
            )
    }

    @Test
    fun aBoundCardOffersDisconnectAndReadsBound() {
        render(card(bound = true))
        composeTestRule.onNodeWithTag("bridge-action-button")
            .assertTextEquals(ctx.getString(R.string.bridges_plugin_disconnect))
            .assert(
                SemanticsMatcher.expectValue(
                    SemanticsProperties.StateDescription,
                    AutomationAttributePack.pack("client_id" to clientId, "bound" to "true"),
                ),
            )
    }

    @Test
    fun theButtonRunsTheCardsVerb() {
        var pressed = 0
        render(card(), onPress = { pressed++ })
        composeTestRule.onNodeWithTag("bridge-action-button").performClick()
        assertEquals(1, pressed)
    }

    /** The plugin manifest spells a free-text setting `string`: it is an editable committing input, not a read-only label. */
    @Test
    fun aStringSettingIsEditableAndCommitsItsOwnKey() {
        val updates = mutableListOf<Pair<String, FfiCborValue>>()
        render(card(bound = true, settings = listOf(stringSetting("string"))), onUpdateSetting = { k, v -> updates += k to v })
        composeTestRule.onNodeWithText("Greeting").performTextReplacement("hello")
        composeTestRule.onNodeWithText(ctx.getString(R.string.common_save)).performClick()
        assertEquals(listOf<Pair<String, FfiCborValue>>("greeting" to FfiCborValue.Text("hello")), updates)
    }
}
