package com.fauna.app.core

import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiReportForm
import com.fauna.ffi.FfiReportSent
import com.fauna.ffi.FfiReportSubject
import com.fauna.ffi.FfiReportTarget
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText

/**
 * [ReportSheetStore]'s sequencing (`moderation.md` § User-initiated reporting →
 * *App surface*): submit → `knocks_block(author)` when ticked → `hide_reported(id)`
 * → the stored list into the render inputs. Every wording and gate decision is
 * shared Rust's (`reportSheetView`, `abuseReportSubmit`), so the store is tested
 * FFI-free against a mocked [ApiClient] — the same shape as web's
 * `ReportHost.svelte` and apple's `ReportSheetStoreTests`.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ReportSheetStoreTest {

    private val author = "ab".repeat(32)
    private val ack = LocalizedText("moderation.report.sent_local", mapOf("nest" to "home"))

    private fun post(cid: String = "bafycid") = FfiReportTarget(
        subject = FfiReportSubject.Post(cid = cid),
        sealed = false,
        author = author,
        plaintext = "the words",
    )

    private fun sent() = FfiReportSent(reportId = "r1", routedTo = listOf("home"), acknowledgement = ack)

    private class Harness(val api: ApiClient, val hidden: MutableList<List<String>>, val store: ReportSheetStore)

    private fun harness(): Harness {
        val api = mock(ApiClient::class.java)
        val hidden = mutableListOf<List<String>>()
        return Harness(api, hidden, ReportSheetStore(api) { hidden += it })
    }

    @Test
    fun `open starts from an empty draft with no stale acknowledgement`() {
        val h = harness()
        h.store.open(post())
        h.store.edit { it.copy(reason = "spam", note = "x") }

        h.store.open(post("another"))

        val s = h.store.state.value
        assertEquals(FfiReportSubject.Post("another"), s.target?.subject)
        assertEquals(ReportSheetStore.EMPTY_FORM, s.form)
        assertNull(s.status)
        assertNull(s.error)
    }

    @Test
    fun `a landed send hides the subject and paints the acknowledgement outside the closed sheet`() = runBlocking {
        val h = harness()
        val target = post()
        h.store.open(target)
        h.store.edit { it.copy(reason = "spam") }
        val form = h.store.state.value.form
        whenever(h.api.abuseReportSubmit(target, form)).thenReturn(sent())
        whenever(h.api.hideReported("bafycid")).thenReturn(listOf("bafycid"))

        h.store.submit()

        val s = h.store.state.value
        assertNull("the sheet closes after a landed send", s.target)
        assertEquals(ack, s.status)
        assertNull(s.followupError)
        assertEquals(listOf(listOf("bafycid")), h.hidden)
        // The block is chained only when ticked.
        verify(h.api, never()).blockKnock("", author)
    }

    @Test
    fun `a ticked block chains knocks_block with the author`() = runBlocking {
        val h = harness()
        val target = post()
        h.store.open(target)
        h.store.edit { it.copy(reason = "harassment", blockAuthor = true) }
        val form = h.store.state.value.form
        whenever(h.api.abuseReportSubmit(target, form)).thenReturn(sent())
        whenever(h.api.hideReported("bafycid")).thenReturn(listOf("bafycid"))

        h.store.submit()

        verify(h.api).blockKnock("", author)
        assertNull(h.store.state.value.followupError)
    }

    @Test
    fun `a failed block lands beside the acknowledgement and the subject is still hidden`() = runBlocking {
        val h = harness()
        val target = post()
        h.store.open(target)
        h.store.edit { it.copy(reason = "spam", blockAuthor = true) }
        val form = h.store.state.value.form
        whenever(h.api.abuseReportSubmit(target, form)).thenReturn(sent())
        whenever(h.api.blockKnock("", author)).thenThrow(RuntimeException("nest unreachable"))
        whenever(h.api.hideReported("bafycid")).thenReturn(listOf("bafycid"))

        h.store.submit()

        val s = h.store.state.value
        assertEquals("the report itself landed", ack, s.status)
        assertEquals("block: nest unreachable", s.followupError)
        assertEquals("the hide still ran", listOf(listOf("bafycid")), h.hidden)
        assertNull("a failed follow-up is not a failed send", s.error)
    }

    @Test
    fun `an unknown subject kind is not hidden`() = runBlocking {
        val h = harness()
        val target = FfiReportTarget(
            subject = FfiReportSubject.Unknown(cbor = byteArrayOf(1)),
            sealed = false,
            author = null,
            plaintext = null,
        )
        h.store.open(target)
        val form = h.store.state.value.form
        whenever(h.api.abuseReportSubmit(target, form)).thenReturn(sent())

        h.store.submit()

        assertEquals(emptyList<List<String>>(), h.hidden)
        verify(h.api, never()).hideReported("")
        Unit
    }

    @Test
    fun `cancel closes the sheet and reset drops the whole draft`() {
        val h = harness()
        h.store.open(post())
        h.store.edit { it.copy(reason = "spam") }

        h.store.cancel()
        assertNull(h.store.state.value.target)
        assertEquals("a cancel keeps the draft's form", "spam", h.store.state.value.form.reason)

        h.store.reset()
        assertEquals(ReportSheetStore(h.api) {}.state.value, h.store.state.value)
        assertNotNull(h.store.state.value.form)
    }

    @Test
    fun `the hide keys on the subject's own id per kind`() {
        assertEquals("c", ReportSheetStore.subjectId(FfiReportSubject.Post("c")))
        // A message hides on the plane record DIGEST (the record cid half), never the channel.
        assertEquals("digest", ReportSheetStore.subjectId(FfiReportSubject.Message("chan", "digest")))
        assertEquals("actor", ReportSheetStore.subjectId(FfiReportSubject.Actor("actor")))
        assertNull(ReportSheetStore.subjectId(FfiReportSubject.Unknown(byteArrayOf())))
    }

    @Test
    fun `an empty form equals the draft a fresh sheet starts from`() {
        assertEquals(FfiReportForm(reason = null, note = "", includeText = false, blockAuthor = false), ReportSheetStore.EMPTY_FORM)
    }
}
