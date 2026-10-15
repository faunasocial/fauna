package com.fauna.app.ui.viewmodel

import com.fauna.ffi.FfiAuthoredRow
import com.fauna.ffi.FfiEditorCell
import com.fauna.ffi.FfiEditorVerdict
import com.fauna.ffi.FfiPolicyEditorInterface
import com.fauna.ffi.FfiPolicyEditorView
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.fauna_core.LocalizedText

/**
 * [FeatureAuthoringHost] over a fake editor — no Android, no native library.
 * The host is a thin shell (the draft and every rule are the shared editor's),
 * so these pin only what it owns: when state changes, which verdict becomes
 * which line, and that a write re-reads the host.
 *
 * `Dispatchers.Unconfined` runs each launch eagerly; the fakes never suspend,
 * so the assertions are deterministic without a test scheduler.
 */
class FeatureAuthoringHostTest {

    private class FakeEditor(
        var currentOn: Boolean = true,
        var cellText: String = "5 GB",
        val saveVerdict: () -> FfiEditorVerdict = { FfiEditorVerdict(status = LocalizedText("features.editor_saved", emptyMap()), invalid = null) },
        val removeVerdict: () -> FfiEditorVerdict = { FfiEditorVerdict(status = LocalizedText("features.editor_removed", emptyMap()), invalid = null) },
    ) : FfiPolicyEditorInterface {
        var saves = 0
        var removes = 0

        override fun view() = FfiPolicyEditorView(
            feature = "payments",
            tier = "self",
            title = LocalizedText("features.editor_title_self", emptyMap()),
            on = currentOn,
            cells = if (currentOn) {
                listOf(FfiEditorCell(LocalizedText("l", emptyMap()), cellText, null, null))
            } else {
                emptyList()
            },
            offNote = null,
            canRemove = true,
            hint = LocalizedText("features.editor_hint", emptyMap()),
        )

        override fun setOn(on: Boolean) {
            currentOn = on
        }

        override fun setCell(index: UInt, text: String) {
            cellText = text
        }

        override suspend fun save(): FfiEditorVerdict {
            saves++
            return saveVerdict()
        }

        override suspend fun remove(): FfiEditorVerdict {
            removes++
            return removeVerdict()
        }
    }

    private fun row(feature: String) = FfiAuthoredRow(
        feature = feature,
        name = LocalizedText("n", emptyMap()),
        summary = LocalizedText("s", emptyMap()),
        hasDocument = false,
        unreadable = false,
    )

    private class Fixture(
        val editor: FakeEditor = FakeEditor(),
        var rows: () -> List<FfiAuthoredRow> = { emptyList() },
    ) {
        var reads = 0
        var written = 0
        val host = FeatureAuthoringHost(
            scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined),
            tier = "self",
            readRows = { reads++; rows() },
            openEditor = { _, _ -> editor },
            // The key plus its args, so a test can see both.
            resolve = { t -> t.key + t.args.entries.joinToString("") { "|${it.key}=${it.value}" } },
            onWritten = { written++ },
        )
    }

    @Test
    fun stateIsEmptyUntilTheAuthoredReadAnswers() {
        val f = Fixture()
        assertNull(f.host.state.value.rows)
        f.host.refresh()
        assertEquals(emptyList<FfiAuthoredRow>(), f.host.state.value.rows)
    }

    @Test
    fun aFailedReadLeavesNoRowsAndAWordedReason() {
        val f = Fixture(rows = { throw IllegalStateException("boom") })
        f.host.refresh()
        assertNull(f.host.state.value.rows)
        assertEquals("features.editor_load_failed|error=boom", f.host.state.value.readError)
    }

    @Test
    fun openSeedsTheViewAndCancelClosesWithoutTouchingTheDocument() {
        val f = Fixture()
        f.host.open("payments")
        assertEquals("payments", f.host.state.value.openFeature)
        assertNotNull(f.host.state.value.view)
        f.host.cancel()
        assertNull(f.host.state.value.openFeature)
        assertNull(f.host.state.value.view)
        assertEquals(0, f.editor.saves + f.editor.removes)
    }

    @Test
    fun keystrokesAndTheOnOffChoiceRepaintFromTheSharedView() {
        val f = Fixture()
        f.host.open("payments")
        f.host.setCell(0, "7 GB")
        assertEquals("7 GB", f.host.state.value.view!!.cells[0].text)
        f.host.setOn(false)
        assertEquals(false, f.host.state.value.view!!.on)
        assertEquals(emptyList<FfiEditorCell>(), f.host.state.value.view!!.cells)
    }

    @Test
    fun aSavedWriteShowsItsStatusRereadsTheHostAndNotifiesTheOwner() {
        val f = Fixture()
        f.host.open("payments")
        val readsBefore = f.reads
        f.host.save()
        assertEquals(1, f.editor.saves)
        assertEquals("features.editor_saved", f.host.state.value.status)
        assertNull(f.host.state.value.error)
        assertEquals(readsBefore + 1, f.reads)
        assertEquals(1, f.written)
        assertEquals(false, f.host.state.value.working)
    }

    @Test
    fun removeSendsTheRemovalNotASave() {
        val f = Fixture()
        f.host.open("payments")
        f.host.remove()
        assertEquals(1, f.editor.removes)
        assertEquals(0, f.editor.saves)
        assertEquals("features.editor_removed", f.host.state.value.status)
    }

    @Test
    fun anInvalidCellLandsOnTheErrorLineAndDispatchesNothingElse() {
        val invalid = LocalizedText("features.editor_invalid_size", mapOf("value" to "x"))
        val f = Fixture(editor = FakeEditor(saveVerdict = { FfiEditorVerdict(status = null, invalid = invalid) }))
        f.host.open("payments")
        val readsBefore = f.reads
        f.host.save()
        assertEquals("features.editor_invalid_size|value=x", f.host.state.value.error)
        assertNull(f.host.state.value.status)
        assertEquals(readsBefore, f.reads)
        assertEquals(0, f.written)
        assertEquals(false, f.host.state.value.working)
    }

    @Test
    fun aTransportFailureLandsOnTheErrorLineAndReleasesTheGuard() {
        val f = Fixture(editor = FakeEditor(saveVerdict = { throw IllegalStateException("offline") }))
        f.host.open("payments")
        f.host.save()
        assertEquals("features.editor_save_failed|error=offline", f.host.state.value.error)
        assertEquals(false, f.host.state.value.working)
        assertEquals(0, f.written)
    }

    @Test
    fun writingWithNoOpenEditorDoesNothing() {
        val f = Fixture()
        f.host.save()
        f.host.remove()
        assertEquals(0, f.editor.saves + f.editor.removes)
    }
}
