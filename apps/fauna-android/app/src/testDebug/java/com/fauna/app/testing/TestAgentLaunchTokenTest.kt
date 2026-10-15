package com.fauna.app.testing

import com.fauna.app.core.AppState
import com.fauna.app.core.SecureStorage
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.robolectric.annotation.Config

/**
 * The wrong-clock refresh witness's two android seams (launch-routing smoke
 * case M, `login.md` § E2E test login): the top-level `launch_token` state key
 * (`fauna_e2e_agent::LAUNCH_TOKEN_KEY`) and the `launch_refresh_token` command
 * (`fauna_e2e_agent::LAUNCH_REFRESH_TOKEN`), both over `FfiNestClient`'s
 * `test-helpers` exports.
 *
 * A host test has no connected nest, so what is pinned here is the
 * convention-11 face — the key is present and JSON `null` (the reader's
 * refusal, never an absent key or a hand-spelled zero) and the command refuses
 * loudly on the app's own `error-message`. The live shape, `expires_in_secs`
 * in `(60, 3600]` before and after the forced refresh, is case M's to witness
 * once the android venue runs it.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentLaunchTokenTest {

    @Test
    fun withNoLiveClientTheLaunchTokenKeyIsPresentAndJsonNull() {
        val state = TestAgent.serializeState(AppState(), mock(SecureStorage::class.java), null)

        assertTrue("the key must be published, not absent", state.has("launch_token"))
        assertTrue(
            "no client ⇒ JSON null, the reader's refusal: ${state.get("launch_token")}",
            state.isNull("launch_token"),
        )
    }

    @Test
    fun launchRefreshTokenWithNoLiveClientIsRefusedLoudly() {
        val appState = AppState()
        runBlocking {
            TestAgent.processCommand(
                "launch_refresh_token",
                JSONObject().put("action", "launch_refresh_token"),
                appState,
                mock(SecureStorage::class.java),
                null,
            )
        }

        val refusal = appState.messages.refusedAgentCommand.value
        assertTrue(
            "must name the command and why: $refusal",
            refusal?.contains("launch_refresh_token") == true &&
                refusal.contains("no held bearer to refresh"),
        )
    }
}
