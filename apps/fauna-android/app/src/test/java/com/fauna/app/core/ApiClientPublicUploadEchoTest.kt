package com.fauna.app.core

import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.conversations.ConversationsManagerHost
import com.fauna.app.core.events.EventDraftsHost
import com.fauna.app.p2pshare.OfflineShareHost
import com.fauna.app.testing.FaunaRobolectricTestRunner
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.runTest
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito
import org.robolectric.annotation.Config

/**
 * [ApiClient.uploadPublicPostBlob] signs nothing itself, but its caller
 * (`ProfileEditVM`) signs a profile image over the hash it returns, so the hash
 * must be the one the client computed over the bytes it sent, never the nest's
 * echo (`security.md` § Signed-envelope shape, the client-computed media hash).
 * The nest is a wire fake: an application interceptor answers every request
 * with `{"hash": <echo>}`. The shared check is `verifyBlobUploadEcho`
 * (`libs/fauna-ffi/src/media_upload.rs`); this pins that android calls it.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ApiClientPublicUploadEchoTest {

    private val bytes = "a public profile picture".toByteArray()

    private fun apiClient(echo: () -> String): ApiClient {
        val fake = OkHttpClient.Builder()
            .addInterceptor(Interceptor { chain ->
                Response.Builder()
                    .request(chain.request())
                    .protocol(Protocol.HTTP_1_1)
                    .code(200)
                    .message("OK")
                    .body("""{"hash":"${echo()}"}""".toResponseBody("application/json".toMediaType()))
                    .build()
            })
            .build()
        val api = ApiClient(
            fake,
            ApplicationProvider.getApplicationContext(),
            Mockito.mock(SessionAccount::class.java),
            Mockito.mock(ConversationsManagerHost::class.java),
            Mockito.mock(EventDraftsHost::class.java),
            Mockito.mock(AccountStores::class.java),
            Mockito.mock(CriticalAlertsHost::class.java),
            Mockito.mock(OfflineShareHost::class.java),
            Mockito.mock(NotificationHelper::class.java),
            Mockito.mock(com.fauna.app.core.push.PushHost::class.java),
        )
        api.nodeUrl = "https://nest.test"
        // A live session without `authenticate`: that seats a client and starts
        // the auto-renew cadence, which reaches real FFI on a double. The blob
        // POST only needs `ensureAuthenticated` to find an unexpired token.
        seed(api, "token", "tok")
        seed(api, "tokenExpiresAt", Long.MAX_VALUE)
        return api
    }

    private fun seed(api: ApiClient, field: String, value: Any) {
        ApiClient::class.java.getDeclaredField(field).apply { isAccessible = true }.set(api, value)
    }

    @Test
    fun `an echo naming another blob makes the public upload throw`() =
        runTest(UnconfinedTestDispatcher()) {
            val api = apiClient { "ab".repeat(32) }
            try {
                api.uploadPublicPostBlob(bytes)
                fail("a nest echoing another blob's hash must be refused")
            } catch (e: ApiException) {
                throw e
            } catch (e: Exception) {
                assertTrue("got: $e", e.toString().contains("blob upload hash mismatch"))
            }
        }

    @Test
    fun `a matching echo returns the locally computed hash`() =
        runTest(UnconfinedTestDispatcher()) {
            // Learn the local hash from the refusal that names it, then answer with it.
            var echo = "ab".repeat(32)
            val api = apiClient { echo }
            val local = try {
                api.uploadPublicPostBlob(bytes)
                fail("setup: the first echo must mismatch")
                ""
            } catch (e: Exception) {
                Regex("local ([0-9a-f]{64})").find(e.toString())!!.groupValues[1]
            }
            echo = local.uppercase()
            assertEquals(local, api.uploadPublicPostBlob(bytes))
        }
}
