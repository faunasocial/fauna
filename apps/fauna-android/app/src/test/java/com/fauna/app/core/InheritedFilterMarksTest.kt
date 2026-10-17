package com.fauna.app.core

import com.fauna.app.testing.FaunaRobolectricTestRunner
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.ArgumentMatchers.anyLong
import org.mockito.Mockito.inOrder
import org.mockito.Mockito.mock
import org.mockito.Mockito.mockingDetails
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config

/**
 * [InheritedFilterMarks] — the one cache android's two filter-mark surfaces
 * read (`succession-aftermath.md` § Adjudicating what the aftermath carries
 * across, the email-filter plane): reads go through the fauna-ffi façade,
 * a failed read keeps the previous marks, Keep and Removed drive the shared
 * doors and re-read, and an account switch drops the marks.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class InheritedFilterMarksTest {

    /** The read outcome, mutable after construction (no re-stubbing). */
    private class Reads(var next: Result<List<Long>>)

    private fun make(initial: List<Long>): Triple<InheritedFilterMarks, ApiClient, Reads> = runBlocking {
        val api = mock(ApiClient::class.java)
        val reads = Reads(Result.success(initial))
        whenever(api.filterMarksList()).thenAnswer { reads.next.getOrThrow() }
        val store = InheritedFilterMarks(api, mock(AccountStores::class.java))
        store.reload()
        Triple(store, api, reads)
    }

    @Test
    fun `reload reads the open marks through the façade`() {
        val (store, _, _) = make(listOf(3L, 5L))
        assertEquals(listOf(3L, 5L), store.marks.value)
    }

    @Test
    fun `a failed read keeps the previous marks`() = runBlocking<Unit> {
        val (store, _, reads) = make(listOf(3L))
        reads.next = Result.failure(RuntimeException("account store not ready"))
        store.reload()
        assertEquals(listOf(3L), store.marks.value)
    }

    @Test
    fun `keep records the verdict through the shared door and re-reads`() = runBlocking<Unit> {
        val (store, api, reads) = make(listOf(3L, 5L))
        whenever(api.filterMarkKeep(3L)).thenAnswer { reads.next = Result.success(listOf(5L)); true }
        store.keep(3L)
        verify(api).filterMarkKeep(3L)
        assertEquals(listOf(5L), store.marks.value)
    }

    @Test
    fun `a failed keep is thrown to the caller and still re-reads`() {
        val (store, api, reads) = make(listOf(3L))
        runBlocking {
            whenever(api.filterMarkKeep(3L)).thenAnswer {
                reads.next = Result.success(listOf(3L, 9L))
                throw RuntimeException("ledger write refused")
            }
        }
        assertThrows(RuntimeException::class.java) { runBlocking { store.keep(3L) } }
        assertEquals(listOf(3L, 9L), store.marks.value)
    }

    @Test
    fun `recordRemoved records Removed for a marked rule and re-reads`() = runBlocking<Unit> {
        val (store, api, reads) = make(listOf(3L))
        whenever(api.filterMarkRemoved(3L)).thenAnswer { reads.next = Result.success(emptyList()); true }
        store.recordRemoved(3L)
        val order = inOrder(api)
        order.verify(api).filterMarkRemoved(3L)
        order.verify(api).filterMarksList()
        assertEquals(emptyList<Long>(), store.marks.value)
    }

    @Test
    fun `recordRemoved is a no-op for a rule that carries no mark`() = runBlocking<Unit> {
        val (store, api, _) = make(listOf(3L))
        store.recordRemoved(4L)
        verify(api, never()).filterMarkRemoved(anyLong())
    }

    @Test
    fun `an account switch drops the marks`() = runBlocking<Unit> {
        val api = mock(ApiClient::class.java)
        val accountStores = mock(AccountStores::class.java)
        whenever(api.filterMarksList()).thenReturn(listOf(3L))
        val store = InheritedFilterMarks(api, accountStores)
        store.reload()
        // The closer straight from Mockito's recorded invocation (not an
        // ArgumentCaptor: its null NPEs on registerCloser's non-null lambda
        // param — FamilyNotifyStoreTest's note).
        val registerCall = mockingDetails(accountStores).invocations
            .single { it.method.name == "registerCloser" }
        assertEquals("inherited-filter-marks", registerCall.arguments[0])
        @Suppress("UNCHECKED_CAST")
        (registerCall.arguments[1] as () -> Unit).invoke()
        assertEquals(emptyList<Long>(), store.marks.value)
    }
}
