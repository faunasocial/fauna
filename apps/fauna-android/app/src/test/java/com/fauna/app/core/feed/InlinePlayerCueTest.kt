package com.fauna.app.core.feed

import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiCueTracker
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_feed.CueRow
import uniffi.fauna_feed.LeaveModel

/**
 * The android leg of the media arm of `watch-complete` (`engagement-cues.md` § Cue
 * vocabulary & derivation): what the inline player notes on the shared tracker
 * (`notePlayed`) reaches the observation. Robolectric cannot decode video, so the
 * player's readings are fed as `MediaPlayer` would report them; the peak semantics and
 * the derivation over the observation are pinned in shared Rust (`libs/fauna-feed`).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class InlinePlayerCueTest {

    @Test
    fun `a video played to the end reaches the observation as its played fraction`() {
        val post = "played-observed"
        val tracker = FfiCueTracker(LeaveModel.HOLD_UNMEASURED)
        fun sample(atMs: Long) = tracker.sample(
            listOf(CueRow(postId = post, top = 0.0, height = 400.0, isMedia = true, mediaPlayedPm = null)),
            listOf(post),
            0.0,
            1_000.0,
            atMs.toULong(),
            atMs.toULong(),
        )

        tracker.notePlayed(post, 2_500.0, 5_000.0)
        sample(0)
        // Completion lands between two ticks: the whole play.
        tracker.notePlayed(post, 5_000.0, 5_000.0)
        sample(250)

        val left = tracker.drainAll(1_000u)
        assertEquals(1, left.size)
        assertEquals(1000u, left[0].mediaPlayedPm)
    }

    @Test
    fun `an engaged player holds its media row tracked below the viewport, a paused one does not`() {
        fun exposuresOf(engagedThrough: Int): Int {
            val post = "engaged-$engagedThrough"
            val tracker = FfiCueTracker(LeaveModel.HOLD_UNMEASURED)
            // The row sits far below a 1000-px viewport: no visible fraction at all.
            fun sample(atMs: Long) = tracker.sample(
                listOf(CueRow(postId = post, top = 5_000.0, height = 400.0, isMedia = true, mediaPlayedPm = null)),
                listOf(post),
                0.0,
                1_000.0,
                atMs.toULong(),
                atMs.toULong(),
            )
            // `engagedThrough` samples are taken while the player is engaged (the tap).
            tracker.notePlaying(post, true)
            repeat(engagedThrough) { sample(it * 250L) }
            // The pause / end / error / teardown edge.
            tracker.notePlaying(post, false)
            return tracker.drainAll(10_000u).size
        }

        assertEquals("engaged, off-screen: the row is an exposure", 1, exposuresOf(3))
        assertEquals("never sampled while engaged: no exposure", 0, exposuresOf(0))
    }
}
