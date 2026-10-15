package com.fauna.app.core.feed

import com.fauna.ffi.cuePlayedPerMille

/**
 * What the feed's inline players have played, for the engagement-cue probe
 * (`engagement-cues.md` § Cue vocabulary & derivation, the media arm of
 * `watch-complete`): each post's highest played per-mille since the probe last
 * took it — the contract on the shared `CueRow::media_played_pm`.
 *
 * A player [note]s every position it reads (polled on the shared sampling tick
 * while it plays) and its completion; the probe [take]s the value for the post's
 * row each sample, clearing it, so a video finished once never credits a second
 * exposure when the reader scrolls back to it. The per-mille clamp is the shared
 * Rust one (`cuePlayedPerMille`), never written down here.
 */
object PlaybackPeaks {
    private val unread = HashMap<String, UInt>()

    /** A player's reading, both in milliseconds (`MediaPlayer`'s unit). A
     *  duration not yet known (`MediaPlayer` reports -1 or 0) is no reading. */
    @Synchronized
    fun note(postId: String, positionMs: Int, durationMs: Int) {
        val pm = cuePlayedPerMille(positionMs.toDouble(), durationMs.toDouble()) ?: return
        unread[postId] = maxOf(pm, unread[postId] ?: 0u)
    }

    /** The peak played on [postId] since the last take, cleared by the take;
     *  `null` when nothing played since. */
    @Synchronized
    fun take(postId: String): UInt? = unread.remove(postId)
}
