package com.fauna.app.core

import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.conversations.ConversationsManagerHost
import com.fauna.app.core.events.EventDraftsHost
import com.fauna.app.p2pshare.OfflineShareHost
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiBearerToken
import com.fauna.ffi.FfiNestClient
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.runTest
import okhttp3.OkHttpClient
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito
import org.robolectric.annotation.Config

/**
 * [ApiClient.authenticate] at the **identity change**: `account-scoping.md`
 * § The scoping taxonomy → the in-memory corollary checks every post-await
 * write against the current actor, "at the write, not at the launch".
 * `authenticate` has two awaits: the bearer mint, and `connect()` on the client
 * it just seated. Neither caller serializes against the switch.
 * `ensureAuthenticated()` refreshes an expired token from any RPC site, so
 * [ApiClient.clearAuth] can land inside either await. When it does, the
 * outgoing actor's call must write nothing: no client for the next actor to
 * ride, and no pumps or account runtime started on a client the teardown has
 * already taken. (Apple's `sameActorSince()` in `ensureNestConnected` is the
 * twin.)
 *
 * The two FFI entry points are swapped through the class's test seams: the mint
 * is held open by a gate, and the client is a Mockito double that never dials.
 * The double records every call. It refuses `uniffiCloneHandle`, so a
 * regression that hands it to a real FFI function fails here instead of passing
 * a zero handle into Rust. No wall clock: the gates order everything, and every
 * negative assert is checked after the call under test has returned, when
 * everything it does synchronously has already happened.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ApiClientActorGenerationTest {

    private val secretA = "11".repeat(32)
    private val secretB = "22".repeat(32)

    private fun apiClient() = ApiClient(
        OkHttpClient(),
        ApplicationProvider.getApplicationContext(),
        Mockito.mock(SessionAccount::class.java),
        Mockito.mock(ConversationsManagerHost::class.java),
        Mockito.mock(EventDraftsHost::class.java),
        Mockito.mock(AccountStores::class.java),
        Mockito.mock(CriticalAlertsHost::class.java),
        Mockito.mock(OfflineShareHost::class.java),
        Mockito.mock(NotificationHelper::class.java),
    )

    private fun bearer() = FfiBearerToken("tok", "0123456789abcdef", ULong.MAX_VALUE / 2000uL)

    /** A client double recording every method called on it. It answers `null`
     *  to everything except `uniffiCloneHandle`, which it refuses (see the
     *  class doc), and runs [onConnect] as the body of `connect()`. */
    private class FakeClient(onConnect: () -> Unit) {
        val calls = mutableListOf<String>()
        val client: FfiNestClient = Mockito.mock(FfiNestClient::class.java) { inv ->
            val name = inv.method.name
            synchronized(calls) { calls += name }
            when (name) {
                "uniffiCloneHandle" -> throw AssertionError("a fake client reached a real FFI call")
                "connect" -> { onConnect(); Unit }
                else -> null
            }
        }
        fun called(): List<String> = synchronized(calls) { calls.toList() }
    }

    /** Everything [ApiClient] starts on a client only AFTER its `connect()`
     *  returns: the four subscription pumps and the account runtime. */
    private val postConnectCalls = setOf(
        "subscribeReconnects",
        "subscribePushes",
        "subscribeKnocks",
        "subscribeConnectionState",
        "startAccountRuntime",
    )

    @Test
    fun `a clear landing during the mint seats no client for the outgoing actor`() =
        runTest(UnconfinedTestDispatcher()) {
            val api = apiClient()
            val built = mutableListOf<String>()
            val mintA = CompletableDeferred<FfiBearerToken>()
            val entered = CompletableDeferred<Unit>()
            api.bearerMinter = { _, _ -> entered.complete(Unit); mintA.await() }
            api.nestClientFactory = { _, secretBytes ->
                built += HexUtil.bytesToHex(secretBytes)
                throw IllegalStateException("stop at the build")
            }

            // A's token-expiry refresh is suspended in the mint…
            var outcomeA: Throwable? = null
            val refreshA = launch {
                try { api.authenticate(secretA) } catch (t: Throwable) { outcomeA = t }
            }
            entered.await()
            // …when the switch tears A down, and the mint then answers.
            api.clearAuth()
            mintA.complete(bearer())
            refreshA.join()

            assertTrue(
                "A's refresh must end as superseded, not seat a client: $outcomeA",
                outcomeA is CancellationException,
            )
            assertEquals("no client may be built from A's secret after the clear", emptyList<String>(), built)

            // B then connects and builds its own client. Nothing of A's is in
            // the seat for B's early return to reuse.
            api.bearerMinter = { _, _ -> bearer() }
            try { api.authenticate(secretB) } catch (_: IllegalStateException) {}
            assertEquals(listOf(secretB), built)
        }

    @Test
    fun `a clear landing during connect starts nothing on the torn-down client`() =
        runTest(UnconfinedTestDispatcher()) {
            val api = apiClient()
            api.bearerMinter = { _, _ -> bearer() }
            // The switch lands while A's client is inside connect().
            val fake = FakeClient(onConnect = { api.clearAuth() })
            api.nestClientFactory = { _, _ -> fake.client }

            var outcome: Throwable? = null
            try { api.authenticate(secretA) } catch (t: Throwable) { outcome = t }

            assertTrue(
                "A's authenticate must end as superseded once connect() returns into B's session: $outcome",
                outcome is CancellationException,
            )
            val calls = fake.called()
            assertTrue(
                "the clear must have taken the seated client into its teardown: $calls",
                "releaseAccountScopedStores" in calls,
            )
            assertEquals(
                "no pump or account runtime may start on the torn-down client",
                emptyList<String>(),
                calls.filter { it in postConnectCalls },
            )
        }
}

