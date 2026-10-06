package com.fauna.app.core

import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiWebClient
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.mockingDetails
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config
import uniffi.fauna_client_web.MintedPaywallLink
import uniffi.fauna_client_web.PaywallTarget
import uniffi.fauna_client_web.WebDomainRow

/**
 * [WebPublishStore]'s shared state for the own-post web-publishing verbs
 * (`web-content-hosting.md` § Published-post management) — the origin BOTH
 * `WebSettingsVM` (the `web-settings` Published-posts section) and `FeedVM`
 * (the feed ⋯-menu) read and write, so the two surfaces can never disagree
 * about a creator's address (mirrors linux's thread-local cache / web's
 * `$lib/web-publish` store). Same shape as [ContentPolicyStoreTest]: mock
 * [ApiClient] + its [FfiWebClient], drive the store's public surface, assert
 * on its [kotlinx.coroutines.flow.StateFlow]s.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class WebPublishStoreTest {

    private fun makeStore(): Triple<WebPublishStore, ApiClient, FfiWebClient> {
        val api = mock(ApiClient::class.java)
        val secureStorage = mock(SecureStorage::class.java)
        val accountStores = mock(AccountStores::class.java)
        val web = mock(FfiWebClient::class.java)
        whenever(api.webClient()).thenReturn(web)
        whenever(secureStorage.handle).thenReturn("alice@example.com")
        val store = WebPublishStore(api, secureStorage, accountStores)
        return Triple(store, api, web)
    }

    // ── applyWebPage / ensureOrigin ─────────────────────────────────────────

    @Test
    fun `applyWebPage resolves origin from an enabled subdomain`() {
        val (store, _, _) = makeStore()
        store.applyWebPage(
            domains = emptyList(),
            subdomainEnabled = true,
            handle = "alice",
            servingDomain = "example.com",
        )
        assertEquals("https://alice.example.com/", store.origin.value)
        assertNull(store.disabledReason.value)
    }

    @Test
    fun `applyWebPage reports a disabled reason when hosting is off`() {
        val (store, _, _) = makeStore()
        store.applyWebPage(
            domains = emptyList(),
            subdomainEnabled = false,
            handle = "alice",
            servingDomain = "example.com",
        )
        assertNull("no origin while the opt-in is off", store.origin.value)
        assertTrue(
            "the reason must explain the dead affordance, not leave it silent",
            store.disabledReason.value != null,
        )
    }

    @Test
    fun `ensureOrigin hydrates from the web client once and is a no-op after`() = runBlocking {
        val (store, api, web) = makeStore()
        whenever(web.servingDomain()).thenReturn("example.com")
        whenever(web.getSubdomainEnabled()).thenReturn(true)
        whenever(web.domainGet()).thenReturn(emptyList<WebDomainRow>())

        assertNull("unresolved until a caller asks", store.origin.value)
        store.ensureOrigin()
        // Poll: ensureOrigin's coroutine runs on Dispatchers.IO, not this
        // thread — a bounded poll is the causal barrier proving it landed.
        val deadline = System.currentTimeMillis() + 10_000
        while (store.origin.value == null && System.currentTimeMillis() < deadline) {
            Thread.sleep(20)
        }
        assertEquals("https://alice.example.com/", store.origin.value)

        // A second call must not re-hydrate (no-op once resolved) — reset the
        // mock's answer to something detectably different and confirm it
        // never lands.
        whenever(web.servingDomain()).thenReturn("other.example.com")
        store.ensureOrigin()
        Thread.sleep(200)
        assertEquals(
            "already-resolved origin must not be clobbered by a redundant call",
            "https://alice.example.com/",
            store.origin.value,
        )
    }

    // ── publish / unpublish ──────────────────────────────────────────────

    @Test
    fun `publish calls publishSet with the decoded post id`() = runBlocking {
        val (store, _, web) = makeStore()
        val postIdHex = "aa".repeat(32)
        whenever(web.publishSet(HexUtil.hexToBytes(postIdHex), null)).thenReturn("aa-page")

        store.publish(postIdHex)

        mockingDetails(web).invocations.single { it.method.name == "publishSet" }
        assertNull("a successful publish records no error", store.error.value)
    }

    @Test
    fun `a failed publish records the publish error`() = runBlocking {
        val (store, _, web) = makeStore()
        val postIdHex = "bb".repeat(32)
        whenever(web.publishSet(HexUtil.hexToBytes(postIdHex), null))
            .thenThrow(RuntimeException("nest unreachable"))

        store.publish(postIdHex)

        val error = store.error.value
        assertEquals("publish", error?.first)
        assertEquals("nest unreachable", error?.second)
    }

    @Test
    fun `unpublish calls publishUnset with the decoded post id`() = runBlocking {
        val (store, _, web) = makeStore()
        val postIdHex = "cc".repeat(32)
        whenever(web.publishUnset(HexUtil.hexToBytes(postIdHex))).thenReturn(true)

        store.unpublish(postIdHex)

        mockingDetails(web).invocations.single { it.method.name == "publishUnset" }
        assertNull("a successful unpublish records no error", store.error.value)
    }

    @Test
    fun `a failed unpublish records the unpublish error`() = runBlocking {
        val (store, _, web) = makeStore()
        val postIdHex = "dd".repeat(32)
        whenever(web.publishUnset(HexUtil.hexToBytes(postIdHex)))
            .thenThrow(RuntimeException("nest unreachable"))

        store.unpublish(postIdHex)

        val error = store.error.value
        assertEquals("unpublish", error?.first)
    }

    // ── copyWebLink / copyPaywallLink ────────────────────────────────────

    @Test
    fun `copyWebLink is a local no-round-trip composition once origin is known`() {
        val (store, _, web) = makeStore()
        store.applyWebPage(emptyList(), subdomainEnabled = true, handle = "alice", servingDomain = "example.com")

        store.copyWebLink("ee".repeat(32), "my-page")

        val copied = store.copied.value
        assertEquals("ee".repeat(32), copied?.first)
        assertEquals("web", copied?.second)
        assertEquals("https://alice.example.com/post/my-page.html", copied?.third)
        // No round trip: the web client is never asked anything for this verb.
        assertTrue(mockingDetails(web).invocations.isEmpty())
    }

    @Test
    fun `copyWebLink no-ops with no resolved origin`() {
        val (store, _, _) = makeStore()
        store.copyWebLink("ff".repeat(32), "my-page")
        assertNull("nothing to copy without an origin", store.copied.value)
    }

    @Test
    fun `copyPaywallLink mints a fresh token and composes the tokened url`() = runBlocking {
        val (store, _, web) = makeStore()
        store.applyWebPage(emptyList(), subdomainEnabled = true, handle = "alice", servingDomain = "example.com")
        whenever(web.paywallMintToken(PaywallTarget.PostSlug("gated-page")))
            .thenReturn(MintedPaywallLink(token = "tok123", expires = 9_999_999_999UL, path = "post/gated-page.html"))

        store.copyPaywallLink("11".repeat(32), "gated-page")

        val deadline = System.currentTimeMillis() + 10_000
        while (store.copied.value == null && System.currentTimeMillis() < deadline) {
            Thread.sleep(20)
        }
        val copied = store.copied.value
        assertEquals("11".repeat(32), copied?.first)
        assertEquals("paywall", copied?.second)
        assertEquals("https://alice.example.com/post/gated-page.html?token=tok123", copied?.third)
    }

    @Test
    fun `a failed paywall mint records the paywall error, not a stale copied value`() = runBlocking {
        val (store, _, web) = makeStore()
        store.applyWebPage(emptyList(), subdomainEnabled = true, handle = "alice", servingDomain = "example.com")
        whenever(web.paywallMintToken(PaywallTarget.PostSlug("gated-page")))
            .thenThrow(RuntimeException("mint failed"))

        store.copyPaywallLink("22".repeat(32), "gated-page")

        val deadline = System.currentTimeMillis() + 10_000
        while (store.error.value == null && System.currentTimeMillis() < deadline) {
            Thread.sleep(20)
        }
        assertEquals("paywall", store.error.value?.first)
        assertNull("a failed mint must not leave a stale copied link", store.copied.value)
    }

    // ── account-switch reset ─────────────────────────────────────────────

    /** **A read spawned for the outgoing actor must not paint the incoming
     *  actor's origin** (`account-scoping.md` § The scoping taxonomy → the
     *  in-memory corollary):
     *  [ensureOrigin] holds no cancellation handle over its coroutine, so a
     *  read already inside the FFI call when the closer runs still lands —
     *  and without the [WebPublishStore.generation] guard it would apply the
     *  DEPARTING actor's origin to whatever screen the INCOMING actor now has
     *  open.
     *
     *  Red-verify by dropping the `generation == mine` checks from
     *  [ensureOrigin]: `store.origin.value` reads the stale
     *  `https://alice.example.com/` instead of staying `null`. */
    @Test
    fun `a stale hydrate landing after an account switch does not paint the new actor's origin`() = runBlocking {
        val api = mock(ApiClient::class.java)
        val secureStorage = mock(SecureStorage::class.java)
        val accountStores = mock(AccountStores::class.java)
        val web = mock(FfiWebClient::class.java)
        whenever(api.webClient()).thenReturn(web)
        whenever(secureStorage.handle).thenReturn("alice@example.com")
        val store = WebPublishStore(api, secureStorage, accountStores)

        // Blocks the read INSIDE the FFI call, so the test can land the
        // account switch while `ensureOrigin`'s coroutine is genuinely in
        // flight — not merely scheduled.
        val releaseRead = java.util.concurrent.CountDownLatch(1)
        whenever(web.servingDomain()).thenAnswer {
            releaseRead.await(10, java.util.concurrent.TimeUnit.SECONDS)
            "example.com"
        }
        whenever(web.getSubdomainEnabled()).thenReturn(true)
        whenever(web.domainGet()).thenReturn(emptyList<WebDomainRow>())

        store.ensureOrigin()

        // The account switch lands while the read above is still blocked.
        val registerCall = mockingDetails(accountStores).invocations
            .single { it.method.name == "registerCloser" }
        @Suppress("UNCHECKED_CAST")
        val closer = registerCall.arguments[1] as () -> Unit
        closer.invoke()

        // Now let the outgoing actor's read finish.
        releaseRead.countDown()

        // Bounded poll for the wrong-actor write this test guards against —
        // it must never arrive, so a poll that times out withOUT seeing it is
        // the passing case (the `ensureOrigin hydrates...` test above polls
        // FOR the write; this one polls against it).
        val deadline = System.currentTimeMillis() + 2_000
        while (store.origin.value == null && System.currentTimeMillis() < deadline) {
            Thread.sleep(20)
        }
        assertNull(
            "a read spawned for the outgoing actor must not paint the incoming actor's origin",
            store.origin.value,
        )
    }

    @Test
    fun `the registered account closer clears all four state halves`() {
        val api = mock(ApiClient::class.java)
        val secureStorage = mock(SecureStorage::class.java)
        val accountStores = mock(AccountStores::class.java)
        val store = WebPublishStore(api, secureStorage, accountStores)
        store.applyWebPage(emptyList(), subdomainEnabled = true, handle = "alice", servingDomain = "example.com")
        assertTrue("precondition: origin is resolved before the switch", store.origin.value != null)

        val registerCall = mockingDetails(accountStores).invocations
            .single { it.method.name == "registerCloser" }
        assertEquals("web-publish", registerCall.arguments[0])
        @Suppress("UNCHECKED_CAST")
        val closer = registerCall.arguments[1] as () -> Unit

        closer.invoke()

        assertNull("origin must not leak across an account switch", store.origin.value)
        assertNull(store.disabledReason.value)
        assertNull(store.copied.value)
        assertNull(store.error.value)
    }
}
