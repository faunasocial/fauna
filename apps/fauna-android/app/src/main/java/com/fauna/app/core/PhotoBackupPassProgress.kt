package com.fauna.app.core

import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * What a photo-backup pass publishes about itself: the per-pass countdown, the pass
 * size (so a position can be read), and the two cycle counters. Apple's twins are
 * `PhotoBackupEngine`'s `pendingCount`, `passProgress`, `passesStarted` and
 * `passesCompleted`; `fauna_e2e_agent::PHOTO_BACKUP_KEY` documents what the counters
 * are for.
 *
 * **The countdown describes ONE in-flight pass and nothing else.** It is set to the
 * pass's item count before the first item, decremented once per item however that
 * item leaves the loop ([itemLeft], called from a `finally`), and cleared on every
 * exit path ([endPass]). The old shape — a `+=` into a total that was never reset —
 * made the "{n} pending" row survive a completed pass and grow over a session.
 *
 * `passesStarted` is bumped synchronously by [beginPass], before the pass's first
 * suspension point (convention 14's "initiated synchronously" corollary), and a
 * trailing re-run is its own pass and counts as one.
 */
class PhotoBackupPassProgress {
    private val _pendingCount = MutableStateFlow(0)

    /** Items in the in-flight pass not yet left behind; 0 whenever no pass runs. */
    val pendingCount: StateFlow<Int> = _pendingCount

    private val _passTotal = MutableStateFlow(0)

    /** Items in the in-flight pass; 0 until the pass has counted them. */
    val passTotal: StateFlow<Int> = _passTotal

    private val started = AtomicInteger(0)
    private val completed = AtomicInteger(0)

    val passesStarted: Int get() = started.get()
    val passesCompleted: Int get() = completed.get()

    /** How far the in-flight pass has got — the "3" of "Syncing 3 of 5". */
    val processed: Int get() = (_passTotal.value - _pendingCount.value).coerceAtLeast(0)

    fun beginPass(total: Int) {
        started.incrementAndGet()
        _passTotal.value = total
        _pendingCount.value = total
    }

    /** The pass has counted its items (the scan answers after [beginPass]). */
    fun setTotal(total: Int) {
        _passTotal.value = total
        _pendingCount.value = total
    }

    /** One item has left the loop, by whichever path. */
    fun itemLeft() {
        _pendingCount.value = (_pendingCount.value - 1).coerceAtLeast(0)
    }

    /** End of a pass, however it ended: clear the countdown, bump the completion counter. */
    fun endPass() {
        _pendingCount.value = 0
        _passTotal.value = 0
        completed.incrementAndGet()
    }
}
