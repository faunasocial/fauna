package com.fauna.app.ui.screen.profile

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.core.ApiClient
import com.fauna.app.core.AppMessages
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.viewmodel.ProfileOffersVM
import kotlinx.coroutines.runBlocking
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.times
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config

/**
 * The **Tiers-tab re-read door** on android (`monetization.md` § Pillar 1 →
 * *The Tiers-tab re-read door*): every activation of `profile-tiers-tab`
 * re-reads the OTHER-profile offers section, which [ProfileScreen] expresses by
 * bumping a reload token that [ProfileOffersSection] carries as a
 * `LaunchedEffect` key.
 *
 * **Why this needs its own test rather than riding [ProfileOffersContentTest].**
 * The bug this pins is invisible to a Content-level test: the section rendered
 * correctly the whole time, it just never re-asked. Keyed on the actor alone,
 * the effect re-fired only when the branch re-composed — i.e. after a detour
 * through the Posts tab — so a re-click while already on Tiers, which is exactly
 * how a subscriber asks "did the author approve me yet?", read nothing. There is
 * no push kind for a subscribe grant to answer it instead.
 *
 * Drives the real [ProfileOffersSection] over a mocked [ApiClient] (the
 * [com.fauna.app.ui.viewmodel.DevicesVMTest] shape) and counts round trips. The
 * offers list is deliberately empty so no row renders and the FFI-backed
 * `offerStatus` lambda is never invoked — this stays a JVM test with no native
 * library, like every other Robolectric suite here.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ProfileOffersRereadTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val author = "aa".repeat(32)

    private fun api(): ApiClient = mock(ApiClient::class.java).also { api ->
        runBlocking {
            whenever(api.subscriptionOffersList(author)).thenReturn(emptyList())
        }
    }

    @Test
    fun `every tiers-tab activation re-reads, not only the first composition`() {
        val api = api()
        var token by mutableIntStateOf(0)

        composeTestRule.setContent {
            CompositionLocalProvider(LocalAppMessages provides AppMessages()) {
                ProfileOffersSection(
                    targetActorIdHex = author,
                    reloadToken = token,
                    vm = ProfileOffersVM(api),
                )
            }
        }
        composeTestRule.waitForIdle()
        runBlocking { verify(api, times(1)).subscriptionOffersList(author) }

        // The tab is re-activated while already showing — `tab` does not change,
        // so the branch does not re-compose and the token is the only signal.
        token++
        composeTestRule.waitForIdle()
        runBlocking { verify(api, times(2)).subscriptionOffersList(author) }

        token++
        composeTestRule.waitForIdle()
        runBlocking { verify(api, times(3)).subscriptionOffersList(author) }
    }

    @Test
    fun `a recomposition that changes nothing does not re-read`() {
        val api = api()
        var unrelated by mutableIntStateOf(0)

        composeTestRule.setContent {
            CompositionLocalProvider(LocalAppMessages provides AppMessages()) {
                // Read the unrelated state here so bumping it recomposes this
                // scope without touching either `LaunchedEffect` key.
                @Suppress("UNUSED_EXPRESSION") unrelated
                ProfileOffersSection(
                    targetActorIdHex = author,
                    reloadToken = 0,
                    vm = ProfileOffersVM(api),
                )
            }
        }
        composeTestRule.waitForIdle()
        unrelated++
        composeTestRule.waitForIdle()

        // The door is tab activation, not recomposition: keying the effect on
        // something that changes on every frame would turn a render into a
        // network round trip.
        runBlocking { verify(api, times(1)).subscriptionOffersList(author) }
    }
}
