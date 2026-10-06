package com.fauna.app.testing

import com.fauna.app.core.ActorScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.AppState
import com.fauna.app.core.SecureStorage
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.After
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.ArgumentMatchers.anyString
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config

/**
 * Pins the reconnect step `applySessionPatch`/`reset`/`logout` owe the LIVE
 * [ApiClient] — the gap this test locks in: `TestAgent` never referenced `ApiClient`
 * at all, so a `set_state` `session` patch (the SAME mechanism every
 * `HttpBridgeDriver` login uses — `conftest.py::_login_app_as`,
 * `test_second_login_live_clients.py::_login_as`) only ever updated the
 * Compose-observed [AppState]/[SecureStorage], never the real WS-RPC
 * connection. `nestClient` is a plain `var`, built ONLY inside
 * `ApiClient.ensureNestConnected` (called only from `authenticate()`, called
 * only from `AppLaunchVM.connectActiveSession()`, called only from
 * `FaunaNavHost`'s onboarding `LaunchedEffect` — a branch a direct
 * `isOnboarding = false` assignment never enters). So a cold app's FIRST
 * test-agent login never actually connected (no android e2e test has ever
 * run against a real device — `--client android` is host-emulator-gated
 * fleet-wide, which is exactly why this went uncaught), and a same-process
 * actor switch left the live client bound to the PREVIOUS actor while
 * `appState`/`storage` reported the new one — the same "half-applied
 * session patch" shape as linux/web's `403 Forbidden` regression
 * ([[set-state-does-not-reauth-running-app]]), on a different mechanism.
 *
 * `ApiClient` is mocked (not built for real): its `authenticate()` mints a
 * bearer over a real WS-RPC pre-identity call, which this Robolectric suite
 * must not attempt. The assertion here is purely "was the reconnect step
 * invoked with the right arguments", which is exactly what a mock proves —
 * mirrors `AppLaunchVMTest`'s `mock(ApiClient::class.java)` convention.
 *
 * The teardown half of that reconnect (`reset`/`logout`/`applySessionPatch`'s
 * pre-seed drop) has since moved off a bare `apiClient?.clearAuth()` onto the
 * canonical [ActorScope.dropActorScopedState] every other teardown site calls
 * (`account-scoping.md:1543` — "all six teardown sites plus the test agent's
 * three arms call it"). `TestAgent.actorScope` is populated only inside
 * [TestAgent.start], which this suite never calls, so without
 * `setActorScopeForTest` it stays `null` and the drop is a silent safe-call
 * no-op — the cases below mock [ActorScope] directly and verify the drop,
 * same as [com.fauna.app.core.ActorScopeTest].
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentSessionReconnectTest {

    @After
    fun tearDown() {
        // TestAgent is a process-wide object singleton; a stale mock set here
        // must not leak into a later test in the same Robolectric class run.
        TestAgent.setApiClientForTest(null)
        TestAgent.setActorScopeForTest(null)
    }

    private fun sessionPatchCommand(
        secretHex: String? = "abcdef",
        nodeUrl: String? = "https://nest.example",
    ): JSONObject =
        JSONObject().apply {
            put("action", "patch")
            put(
                "state",
                JSONObject().apply {
                    put(
                        "session",
                        JSONObject().apply {
                            put("authenticated", true)
                            secretHex?.let { put("secret_hex", it) }
                            nodeUrl?.let { put("node_url", it) }
                            put("handle", "e2e-user")
                            put("actor_id", "aa".repeat(32))
                            put("device_id", "test-device")
                        },
                    )
                },
            )
        }

    @Test
    fun authenticatedSessionPatchReconnectsTheLiveApiClient() {
        val apiClient = mock(ApiClient::class.java)
        TestAgent.setApiClientForTest(apiClient)
        val actorScope = mock(ActorScope::class.java)
        TestAgent.setActorScopeForTest(actorScope)
        val storage = mock(SecureStorage::class.java)
        whenever(storage.secretHex).thenReturn("abcdef")
        whenever(storage.nestUrl).thenReturn("https://nest.example")

        runBlocking {
            TestAgent.processCommand("patch", sessionPatchCommand(), AppState(), storage, null)

            // The forcing function this pin exists for: pre-fix, NOTHING here ever
            // called into ApiClient at all — verify both halves of the teardown +
            // rebuild the real account-switch flow performs. The teardown half
            // routes through the canonical ActorScope drop, not a direct
            // `apiClient.clearAuth()`; `authenticate` is `suspend`, so its
            // verification needs a coroutine same as the call.
            verify(actorScope).dropActorScopedState()
            verify(apiClient).authenticate("abcdef")
        }
    }

    @Test
    fun sessionPatchWithNoStoredCredentialsDoesNotReconnect() {
        val apiClient = mock(ApiClient::class.java)
        TestAgent.setApiClientForTest(apiClient)
        // A bare mock's secretHex/nestUrl getters return null — nothing to
        // reconnect with (e.g. an `authenticated` flag with no identity yet).
        val storage = mock(SecureStorage::class.java)

        runBlocking {
            TestAgent.processCommand(
                "patch",
                sessionPatchCommand(secretHex = null, nodeUrl = null),
                AppState(), storage, null,
            )

            verify(apiClient, never()).authenticate(anyString())
        }
    }

    @Test
    fun resetTearsDownTheLiveApiClient() {
        val apiClient = mock(ApiClient::class.java)
        TestAgent.setApiClientForTest(apiClient)
        val actorScope = mock(ActorScope::class.java)
        TestAgent.setActorScopeForTest(actorScope)
        val storage = mock(SecureStorage::class.java)

        runBlocking {
            TestAgent.processCommand(
                "reset",
                JSONObject().apply { put("action", "reset") },
                AppState(), storage, null,
            )
        }

        // Mirrors production sign-out (AccountSettingsVM.signOut ->
        // ActorScope.dropActorScopedState()): the per-test boundary must not
        // leave a stale WS-RPC session running.
        verify(actorScope).dropActorScopedState()
    }

    @Test
    fun logoutTearsDownTheLiveApiClient() {
        val apiClient = mock(ApiClient::class.java)
        TestAgent.setApiClientForTest(apiClient)
        val actorScope = mock(ActorScope::class.java)
        TestAgent.setActorScopeForTest(actorScope)
        val storage = mock(SecureStorage::class.java)

        runBlocking {
            TestAgent.processCommand(
                "logout",
                JSONObject().apply { put("action", "logout") },
                AppState(), storage, null,
            )
        }

        verify(actorScope).dropActorScopedState()
    }
}
