package com.fauna.app.core

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The per-pass countdown and cycle counters (`ui/folders.md` § Photo backup — the
 * progress surface names a position driven by a per-pass countdown). Mirrors
 * apple's `pendingCount` / `passProgress` / `passesStarted` / `passesCompleted`.
 */
class PhotoBackupPassProgressTest {

    @Test
    fun countsDownFromThePassSizeAndPositionCountsUp() {
        val p = PhotoBackupPassProgress()
        p.beginPass(5)
        assertEquals(5, p.pendingCount.value)
        assertEquals(5, p.passTotal.value)
        assertEquals(0, p.processed)
        p.itemLeft()
        p.itemLeft()
        assertEquals(3, p.pendingCount.value)
        assertEquals(2, p.processed)
    }

    @Test
    fun endPassClearsTheCountdownOnEveryExitPath() {
        val p = PhotoBackupPassProgress()
        p.beginPass(4)
        p.itemLeft()
        p.endPass()
        assertEquals(0, p.pendingCount.value)
        assertEquals(0, p.passTotal.value)
    }

    @Test
    fun countdownNeverGoesNegative() {
        val p = PhotoBackupPassProgress()
        p.beginPass(1)
        p.itemLeft()
        p.itemLeft()
        assertEquals(0, p.pendingCount.value)
    }

    @Test
    fun aSecondPassDescribesItselfNotTheSumOfBoth() {
        val p = PhotoBackupPassProgress()
        p.beginPass(3)
        repeat(3) { p.itemLeft() }
        p.endPass()
        p.beginPass(2)
        assertEquals(2, p.pendingCount.value)
        assertEquals(2, p.passTotal.value)
    }

    @Test
    fun cycleCountersBumpOncePerPassAndStartLeadsCompleted() {
        val p = PhotoBackupPassProgress()
        assertEquals(0, p.passesStarted)
        assertEquals(0, p.passesCompleted)
        p.beginPass(1)
        assertEquals(1, p.passesStarted)
        assertEquals(0, p.passesCompleted)
        p.endPass()
        assertEquals(1, p.passesCompleted)
        p.beginPass(0)
        p.endPass()
        assertEquals(2, p.passesStarted)
        assertEquals(2, p.passesCompleted)
    }
}
