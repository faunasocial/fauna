package com.fauna.app.core.feed

import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiCueTracker
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_feed.CueRow
import uniffi.fauna_feed.LeaveModel

/**
 * The android leg of the media arm of `watch-complete` (`engagement-cues.md` § Cue
 * vocabulary & derivation): what the inline player notes reaches the shared tracker's
 * observation through the probe's take. Robolectric cannot decode video, so the
 * player's readings are fed as `MediaPlayer` would report them; the derivation over
 * the observation is pinned in shared Rust (`libs/fauna-feed`).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class PlaybackPeaksTest {

    @Test
    fun `the probe takes the peak since its last take, through the shared clamp`() {
        val post = "peaks-take"
        assertNull("nothing played yet", PlaybackPeaks.take(post))
        PlaybackPeaks.note(post, 1_000, -1)
        assertNull("a duration MediaPlayer does not know yet is no reading", PlaybackPeaks.take(post))

        PlaybackPeaks.note(post, 3_000, 5_000)
        PlaybackPeaks.note(post, 1_000, 5_000)
        assertNull("another post played nothing", PlaybackPeaks.take("peaks-other"))
        assertEquals("the peak, not the latest", 600u, PlaybackPeaks.take(post))
        assertNull("taken once", PlaybackPeaks.take(post))

        PlaybackPeaks.note(post, 5_200, 5_000)
        assertEquals("clamped to the end", 1000u, PlaybackPeaks.take(post))
    }

    @Test
    fun `a video played to the end reaches the observation as its played fraction`() {
        val post = "peaks-observed"
        val tracker = FfiCueTracker(LeaveModel.HOLD_UNMEASURED)
        fun sample(atMs: Long) = tracker.sample(
            listOf(
                CueRow(
                    postId = post,
                    top = 0.0,
                    height = 400.0,
                    isMedia = true,
                    mediaPlayedPm = PlaybackPeaks.take(post),
                ),
            ),
            listOf(post),
            0.0,
            1_000.0,
            atMs.toULong(),
            atMs.toULong(),
        )

        PlaybackPeaks.note(post, 2_500, 5_000)
        sample(0)
        // Completion lands between two ticks: the whole play.
        PlaybackPeaks.note(post, 5_000, 5_000)
        sample(250)
        // Nothing played since: the next sample takes no reading.
        sample(500)

        val left = tracker.drainAll(1_000u)
        assertEquals(1, left.size)
        assertEquals(1000u, left[0].mediaPlayedPm)
    }
}
