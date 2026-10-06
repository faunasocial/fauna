package com.fauna.app.ui.screen.feed

import android.os.SystemClock
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberUpdatedState
import com.fauna.app.ui.viewmodel.FeedVM
import com.fauna.ffi.FfiCueTracker
import com.fauna.ffi.cueSampleIntervalMs
import kotlinx.coroutines.delay
import uniffi.fauna_feed.CueObservation
import uniffi.fauna_feed.CueRow
import uniffi.fauna_feed.LeaveModel
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
 * Android renders feed media as still images only (no ExoPlayer/media3
 * dependency), so `mediaPlayedPm` is always `null` here: the media
 * watch-complete gate can never fire on this client — only the dwell-derived
 * non-media gate and the skip gate can (a video-capable client exercises the
 * playback path).
 */

/**
 * Wire the cue observer onto the feed post list. Call once from the Feed screen,
 * inside the composition that owns [listState].
 *
 * Lifecycle: the sampling loop lives in a [LaunchedEffect], so it starts when
 * the feed list enters the composition and is cancelled when it leaves (nav to
 * detail/compose, tab switch, screen teardown). Cancellation drains every
 * tracked card — off-screen is off-viewport.
 */
@Composable
fun CueViewportObserver(listState: LazyListState, posts: List<PostSummary>, vm: FeedVM) {
    // The observer reads the LATEST post list on every sample without
    // restarting the loop — a feed reload must not reset accumulated dwell.
    val currentPosts by rememberUpdatedState(posts)

    LaunchedEffect(listState) {
        // Compose's LazyColumn is virtualized: a card scrolled well away is
        // DISPOSED and simply absent from `visibleItemsInfo` while its post is
        // still loaded, so absence from a non-empty read is real leave-evidence.
        // linux/windows/apple use the opposite model for their eager containers.
        val tracker = FfiCueTracker(LeaveModel.ABSENCE_IS_LEAVE)
        val intervalMs = cueSampleIntervalMs().toLong()
        try {
            while (true) {
                delay(intervalMs)
                val layout = listState.layoutInfo
                val posts = currentPosts
                val mediaById = posts.associate { it.postId to it.hasMedia }
                // postId ← the row's own key (never an index-into-list read,
                // which mid-update attributes one post's dwell to another).
                val rows = layout.visibleItemsInfo.mapNotNull { item ->
                    val postId = item.key as? String ?: return@mapNotNull null
                    CueRow(
                        postId = postId,
                        top = item.offset.toDouble(),
                        // A not-yet-measured item reports its non-positive size
                        // as-is; what that MEANS is the tracker's call, not the
                        // probe's (it holds the row rather than flushing it).
                        height = item.size.toDouble(),
                        isMedia = mediaById[postId] ?: false,
                        mediaPlayedPm = null,
                    )
                }
                emit(
                    tracker.sample(
                        rows,
                        posts.map { it.postId },
                        layout.viewportStartOffset.toDouble(),
                        layout.viewportEndOffset.toDouble(),
                        // Dwell credit reads the MONOTONIC clock: a user
                        // changing the date, or an NTP step, must never inflate
                        // how long they were shown something.
                        SystemClock.elapsedRealtime().toULong(),
                        System.currentTimeMillis().toULong(),
                    ),
                    vm,
                )
            }
        } finally {
            emit(tracker.drainAll(System.currentTimeMillis().toULong()), vm)
        }
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
    for (obs in left) {
        vm.recordObservation(
            contentId = obs.contentId,
            isMedia = obs.isMedia,
            mediaPlayedPm = obs.mediaPlayedPm,
            dwellMsAtSkipVisibility = obs.dwellMsAtSkipVisibility,
            dwellMsAtLongVisibility = obs.dwellMsAtLongVisibility,
            observedAtMs = obs.observedAtMs,
        )
    }
}
