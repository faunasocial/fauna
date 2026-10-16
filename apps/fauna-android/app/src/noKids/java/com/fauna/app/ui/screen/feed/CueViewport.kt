package com.fauna.app.ui.screen.feed

import android.os.SystemClock
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberUpdatedState
import com.fauna.app.core.feed.PlaybackPeaks
import com.fauna.app.ui.viewmodel.FeedVM
import com.fauna.ffi.FfiCueTracker
import com.fauna.ffi.cueSampleIntervalMs
import kotlinx.coroutines.delay
import uniffi.fauna_feed.CueObservation
import uniffi.fauna_feed.CueRow
import uniffi.fauna_feed.PostSummary

/**
 * Engagement-cue viewport observer for the Feed post list
 * (`docs/goal/behavior/engagement-cues.md` § Cue vocabulary & derivation).
 *
 * **This file is a geometry probe and nothing else.** It reads where each post
 * card sits, ticks, and hands the readings over; every piece of bookkeeping
 * above that — visibility bucketing, dwell credit and its stall cap, the
 * hold-vs-leave policy, the single-sample noise floor, `isMedia` stamping,
 * observation assembly — belongs to the shared `fauna_feed::CueTracker`, reached
 * here through [FfiCueTracker], and all derivation below that to
 * `fauna_feed::CueEngine`. No threshold, no cadence and no arithmetic is written
 * down here.
 *
 * *(Until 2026-07-29 this file also held that bookkeeping, as did linux, windows
 * and apple — four hand-written copies of the same arithmetic. Android's had
 * drifted onto the wall clock for dwell credit, so a date change or an NTP step
 * inflated dwell here and on windows but not on linux or apple. The boundary
 * moved one layer down; this is what is left.)*
 *
 * Playback progress comes from the inline player ([FeedPostVideo]): a media
 * row's `mediaPlayedPm` is what [PlaybackPeaks.take] hands over for its post
 * (the peak since the last sample), so a video played to the end derives the
 * media watch-complete.
 */

/**
 * Wire the cue observer onto the feed post list. Call once from the Feed screen,
 * inside the composition that owns [listState].
 *
 * Lifecycle: the sampling loop lives in a [LaunchedEffect], so it starts when
 * the feed list enters the composition and is cancelled when it leaves (nav to
 * detail/compose, tab switch, screen teardown). Cancellation drains every
 * tracked card — off-screen is off-viewport. The tracker itself is NOT the
 * mount's: it is the auth's one tracker ([FeedVM.cueTracker]), so what it holds
 * outlives a trip off the page.
 */
@Composable
fun CueViewportObserver(listState: LazyListState, posts: List<PostSummary>, vm: FeedVM) {
    // The observer reads the LATEST post list on every sample without
    // restarting the loop — a feed reload must not reset accumulated dwell.
    val currentPosts by rememberUpdatedState(posts)

    LaunchedEffect(listState) {
        val capture = CueCapture(listState, { currentPosts }, vm)
        CueCapture.active = capture
        val intervalMs = cueSampleIntervalMs().toLong()
        try {
            while (true) {
                delay(intervalMs)
                emit(capture.sample(), vm)
            }
        } finally {
            if (CueCapture.active === capture) CueCapture.active = null
            capture.lastTracker?.let { emit(it.drainAll(System.currentTimeMillis().toULong()), vm) }
        }
    }
}

/**
 * One mounted Feed list's geometry probe over the auth's tracker. The sampling
 * loop above drives it; the e2e cue-clock commands (debug `TestAgent`) reach the
 * mounted one through [active] to land each step with a sample of their own —
 * linux's `feed::viewport::test_clock` and web's `activeCueCapture()` twin.
 * Main thread only, like every `LazyListState` read.
 */
class CueCapture internal constructor(
    val listState: LazyListState,
    private val posts: () -> List<PostSummary>,
    private val vm: FeedVM,
) {
    /** The tracker the last sample fed — the one the unmount drains. */
    var lastTracker: FfiCueTracker? = null
        private set

    /** The auth's tracker, or null before the feed manager exists. */
    fun tracker(): FfiCueTracker? = vm.cueTracker()

    /**
     * One probe read: every visible row's geometry handed to the shared
     * tracker; returns the exposures it says have finished.
     */
    fun sample(): List<CueObservation> {
        val tracker = vm.cueTracker() ?: return emptyList()
        lastTracker = tracker
        val layout = listState.layoutInfo
        val posts = posts()
        val mediaById = posts.associate { it.postId to it.playsMedia }
        // postId ← the row's own key (never an index-into-list read,
        // which mid-update attributes one post's dwell to another).
        val rows = layout.visibleItemsInfo.mapNotNull { item ->
            val postId = item.key as? String ?: return@mapNotNull null
            val isMedia = mediaById[postId] ?: false
            CueRow(
                postId = postId,
                top = item.offset.toDouble(),
                // A not-yet-measured item reports its non-positive size
                // as-is; what that MEANS is the tracker's call, not the
                // probe's (it holds the row rather than flushing it).
                height = item.size.toDouble(),
                isMedia = isMedia,
                mediaPlayedPm = if (isMedia) PlaybackPeaks.take(postId) else null,
            )
        }
        return tracker.sample(
            rows,
            posts.map { it.postId },
            layout.viewportStartOffset.toDouble(),
            layout.viewportEndOffset.toDouble(),
            // Dwell credit reads the MONOTONIC clock: a user
            // changing the date, or an NTP step, must never inflate
            // how long they were shown something.
            SystemClock.elapsedRealtime().toULong(),
            System.currentTimeMillis().toULong(),
        )
    }

    /**
     * Fold [left] into the engine and resolve once every record is done — the
     * Layer-A train and any put included — so an e2e step's ack means nothing
     * more is on its way (web's barrier shape; android has no quit door a test
     * can wait out).
     */
    suspend fun record(left: List<CueObservation>) {
        for (obs in left) vm.recordObservationNow(obs)
    }

    companion object {
        /** The mounted Feed list's capture, if any (main thread). */
        @Volatile
        var active: CueCapture? = null
            internal set
    }
}

/**
 * Hand the finished exposures to the shared engine
 * (`FeedManager::record_observation`); a failure surfaces on the page
 * `error-message` via the view-model — never a silent log. The tracker has
 * already applied the single-sample noise floor, so everything here is a real
 * exposure.
 */
private fun emit(left: List<CueObservation>, vm: FeedVM) {
    // Relayed whole: the tracker stamped how each exposure ended, and the
    // shell has no honest way of its own to know it.
    for (obs in left) vm.recordObservation(obs)
}
