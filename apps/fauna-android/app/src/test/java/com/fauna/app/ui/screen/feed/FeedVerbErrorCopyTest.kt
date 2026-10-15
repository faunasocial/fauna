package com.fauna.app.ui.screen.feed

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * A rejected feed verb's banner copy (`restricted-posts.md` ruling 6): a refusal the
 * shared recognizer names paints the `feed.reference_restricted` string, any other
 * failure keeps its own text. The recognizer is injected (the real one is the native
 * `feedRefusalI18nKey`, pinned in shared Rust); windows and tui pin the same shape.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class FeedVerbErrorCopyTest {
    private val context: Context = ApplicationProvider.getApplicationContext()
    private val refusal = "a refused reply"
    private val recognizer: (String) -> String? =
        { if (it == refusal) "feed.reference_restricted" else null }

    @Test
    fun aRestrictedReplyRefusalPaintsTheSharedString() {
        assertEquals(
            context.getString(R.string.feed_reference_restricted),
            verbErrorCopy(context, refusal, recognizer),
        )
    }

    @Test
    fun anyOtherFailureKeepsItsOwnText() {
        assertEquals("nest unreachable", verbErrorCopy(context, "nest unreachable", recognizer))
    }
}
