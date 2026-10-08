package com.fauna.app.core

import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiSpamPreferences
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config

/**
 * The reporter-side hide list in [ContentPolicyStore] (`moderation.md` §
 * Corollary — block also hides): read on the refresh trigger, **keep-on-failure**
 * like the guardian half (a failed read leaves the last-known list in force), and
 * replaced wholesale by a `hideReported` reply. The decision "is this item hidden"
 * is shared Rust's `contentRenderForItem`; this pins only the store's custody of
 * the list. Latency-independence (testing.md convention 14): the failed-read case
 * anchors on a causal barrier — the same refresh's prefs half lands changed
 * thresholds — before the list is inspected.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ContentPolicyStoreReportedTest {

    private companion object {
        const val BUDGET_MS = 10_000L
    }

    private fun prefs(spam: Int) = FfiSpamPreferences(spamThreshold = spam.toUShort(), phishingThreshold = 100u)

    /** The two read outcomes a test flips AFTER construction (see ContentPolicyStoreTest's `ApiState`). */
    private class ApiState(var hidden: Result<List<String>>, var prefs: Result<FfiSpamPreferences>)

    private fun make(state: ApiState): ContentPolicyStore = runBlocking {
        val api = mock(ApiClient::class.java)
        val accountStores = mock(AccountStores::class.java)
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        whenever(api.familyStatus()).thenAnswer { throw RuntimeException("no status in this test") }
        whenever(api.getSpamPreferences()).thenAnswer { state.prefs.getOrThrow() }
        whenever(api.loadHiddenContent()).thenAnswer { state.hidden.getOrThrow() }
        ContentPolicyStore(api, accountStores)
    }

    @Test
    fun `a refresh loads the reporter-side hide list`() = runBlocking {
        val store = make(ApiState(Result.success(listOf("post-1", "actor-9")), Result.success(prefs(500))))

        val after = withTimeout(BUDGET_MS) { store.inputs.first { it.hiddenContent.isNotEmpty() } }

        assertEquals(listOf("post-1", "actor-9"), after.hiddenContent)
    }

    @Test
    fun `a failed hide read keeps the last-known list`() = runBlocking {
        val state = ApiState(Result.success(listOf("post-1")), Result.success(prefs(500)))
        val store = make(state)
        withTimeout(BUDGET_MS) { store.inputs.first { it.hiddenContent == listOf("post-1") } }

        state.hidden = Result.failure(RuntimeException("nest unreachable"))
        state.prefs = Result.success(prefs(700))
        store.refresh()
        val after = withTimeout(BUDGET_MS) { store.inputs.first { it.ownSpamPermille == 700.toUShort() } }

        assertEquals("a failed read must not clear a reported item's hide", listOf("post-1"), after.hiddenContent)
    }

    @Test
    fun `a hideReported reply replaces the list wholesale`() = runBlocking {
        val store = make(ApiState(Result.success(listOf("post-1")), Result.success(prefs(500))))
        withTimeout(BUDGET_MS) { store.inputs.first { it.hiddenContent == listOf("post-1") } }

        store.setHiddenContent(listOf("post-1", "post-2"))

        assertEquals(listOf("post-1", "post-2"), store.inputs.value.hiddenContent)
    }

    @Test
    fun `nothing reported short-circuits without the shared call`() {
        // FFI-free by construction: an empty list (or an item with no report
        // identity) answers false before `contentRenderForItem` is reached, so a
        // default-constructed inputs works in the VM-free harnesses.
        val inputs = ContentPolicyInputs()
        assertFalse(inputs.isReported("post-1", "author"))
        assertFalse(ContentPolicyInputs(hiddenContent = listOf("x")).isReported(null, null))
    }
}
