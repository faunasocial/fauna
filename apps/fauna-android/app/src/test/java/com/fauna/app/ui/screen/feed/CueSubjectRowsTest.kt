package com.fauna.app.ui.screen.feed

import androidx.compose.foundation.lazy.LazyListItemInfo
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * A placeholder is no exposure (`engagement-cues.md` § Cue vocabulary &
 * derivation): the android probe hands the shared tracker only the rows whose
 * card shows the post — a region, nest, guardian-block, muted-word or collapse
 * placeholder painted under the same key is left out, as linux's
 * `CUE_SUBJECT_CLASS` and web's `data-cue-post` leave it out.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class CueSubjectRowsTest {

    private fun row(rowKey: Any, top: Int, height: Int = 400): LazyListItemInfo =
        object : LazyListItemInfo {
            override val index: Int = top
            override val key: Any = rowKey
            override val offset: Int = top
            override val size: Int = height
            override val contentType: Any? = null
        }

    @Test
    fun `a row whose card shows a placeholder is not measured as its post`() {
        val items = listOf(row("shown", 0), row("muted", 400), row("video", 800))
        val subjects = setOf("shown", "video")

        val rows = cueRowsOf(items, mapOf("video" to true), subjects::contains)

        assertEquals(listOf("shown", "video"), rows.map { it.postId })
        assertEquals(listOf(0.0, 800.0), rows.map { it.top })
        assertEquals(listOf(false, true), rows.map { it.isMedia })
    }

    @Test
    fun `a row keyed by anything but a post id is never a cue row`() {
        val rows = cueRowsOf(listOf(row(7, 0), row("p", 400)), emptyMap()) { true }
        assertEquals(listOf("p"), rows.map { it.postId })
    }

    @Test
    fun `a post stays a subject until every card showing it has left`() {
        val id = "cue-subject-refcount"
        CueSubjects.mark(id)
        CueSubjects.mark(id)
        CueSubjects.unmark(id)
        assertTrue("one card still shows the post", CueSubjects.contains(id))
        CueSubjects.unmark(id)
        assertFalse("no card shows the post any more", CueSubjects.contains(id))
        // An unmatched leave is harmless.
        CueSubjects.unmark(id)
        assertFalse(CueSubjects.contains(id))
    }
}
