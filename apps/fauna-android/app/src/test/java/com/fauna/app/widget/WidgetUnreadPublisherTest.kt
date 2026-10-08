package com.fauna.app.widget

import com.fauna.app.testing.FaunaRobolectricTestRunner
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import java.io.File

/**
 * The headless half of the android home-screen widget's witness
 * (`apps/android.md` § App Widgets): what the app publishes for the widget, and
 * what the widget is asked to paint. The number itself is the shared
 * `ConversationsManager.unreadTotal()` fold, pinned in Rust; the e2e witness
 * (`test_home_screen_widget*.py`, marked `android`) reads the published file
 * from outside the app. Robolectric only for `org.json`.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class WidgetUnreadPublisherTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private lateinit var dir: File
    private val rendered = mutableListOf<Int>()
    private var clock = 1_000L

    private fun publisher() = WidgetUnreadPublisher(
        UnreadSnapshotStore(dir),
        render = { rendered += it },
        dispatcher = Dispatchers.Unconfined,
        now = { clock },
    )

    private fun snapshot(): JSONObject? =
        File(dir, UnreadSnapshotStore.FILE_NAME).takeIf { it.exists() }?.let { JSONObject(it.readText()) }

    @Before
    fun setUp() {
        dir = File(tmp.root, "widget")
    }

    @Test
    fun publishWritesTheCountForTheWidgetAndPaintsIt() {
        publisher().publish(3)

        val snap = snapshot()!!
        assertEquals(3, snap.getInt("count"))
        assertEquals("the apple snapshot's field set", true, snap.has("updatedAt"))
        assertEquals(listOf(3), rendered)
        assertEquals(3, UnreadSnapshotStore(dir).load())
    }

    @Test
    fun anUnchangedCountIsNeitherRewrittenNorRepainted() {
        // The observer fires on every manager change, a compose keystroke
        // included; only a moved count is worth a write and a widget update.
        val p = publisher()
        p.publish(2)
        clock = 2_000L
        p.publish(2)
        // A second instance over the same file agrees: the file is the memory.
        publisher().publish(2)

        assertEquals(listOf(2), rendered)
        p.publish(5)
        assertEquals(listOf(2, 5), rendered)
    }

    @Test
    fun clearForgetsTheCountSoTheNextAccountsEqualCountStillPaints() {
        // A switch between two accounts with the same unread total: the
        // incoming account's first tick must repaint, not read as "unchanged".
        val p = publisher()
        p.publish(4)
        p.clear()

        assertNull(snapshot())
        assertEquals(listOf(4, 0), rendered)
        p.publish(4)
        assertEquals(listOf(4, 0, 4), rendered)
    }

    @Test
    fun rerenderPaintsTheLastPublishedCountAndZeroBeforeAnyPublish() = runBlocking {
        val p = publisher()
        p.rerender()
        p.publish(7)
        p.rerender()

        assertEquals(listOf(0, 7, 7), rendered)
    }

    @Test
    fun aRefreshPassWithNoSessionIsCountedAndReportsNoCount() = runBlocking {
        val before = WidgetRefreshPasses.snapshot()
        WidgetRefreshPasses.run { null }
        val after = WidgetRefreshPasses.snapshot()

        assertEquals(before.started + 1, after.started)
        assertEquals(before.completed + 1, after.completed)
        assertNull(after.lastPassCount)

        WidgetRefreshPasses.run { 6 }
        assertEquals(6, WidgetRefreshPasses.snapshot().lastPassCount)
    }

    @Test
    fun aThrowingPassStillCompletesSoTheBarrierNeverHangs() = runBlocking {
        val before = WidgetRefreshPasses.snapshot()
        WidgetRefreshPasses.run { error("rails down") }
        val after = WidgetRefreshPasses.snapshot()

        assertEquals(before.completed + 1, after.completed)
        assertNull(after.lastPassCount)
        assertFalse(File(dir, UnreadSnapshotStore.FILE_NAME).exists())
    }
}
