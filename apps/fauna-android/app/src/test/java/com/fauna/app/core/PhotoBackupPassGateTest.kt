package com.fauna.app.core

import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The photo-backup single-flight gate's state machine, driven directly — the
 * engine's own pass needs MediaStore, a nest host and Hilt, so the gate is the one
 * seam where the coalescing LOGIC is unit-testable. Cases transfer from apple's
 * `PhotoBackupPassGateTests.swift`; the wiring is witnessed by the e2e (no android
 * e2e venue exists yet).
 */
class PhotoBackupPassGateTest {

    @Test
    fun firstClaimWinsAndASecondIsRefused() {
        val gate = PhotoBackupPassGate()
        assertTrue(gate.claim())
        assertFalse(gate.claim())
        assertTrue(gate.isRunning)
    }

    @Test
    fun finishWithNoRequestOpensTheGate() {
        val gate = PhotoBackupPassGate()
        assertTrue(gate.claim())
        assertFalse(gate.finish())
        assertFalse(gate.isRunning)
        assertTrue(gate.claim())
    }

    @Test
    fun aRefusedClaimIsRememberedAsOneTrailingPass() {
        val gate = PhotoBackupPassGate()
        assertTrue(gate.claim())
        assertFalse(gate.claim())
        // The caller of the first claim owns the trailing pass.
        assertTrue(gate.finish())
        // ... and the gate is NOT open between the pass and its re-run.
        assertTrue(gate.isRunning)
        assertFalse(gate.claim())
        assertTrue(gate.finish())
        assertFalse(gate.finish())
        assertFalse(gate.isRunning)
    }

    @Test
    fun aBurstOfRefusalsCollapsesToExactlyOneTrailingPass() {
        val gate = PhotoBackupPassGate()
        assertTrue(gate.claim())
        repeat(25) { assertFalse(gate.claim()) }
        assertTrue(gate.finish())
        assertFalse(gate.finish())
    }

    @Test
    fun releaseDiscardsARememberedRequestAndOpensTheGate() {
        val gate = PhotoBackupPassGate()
        assertTrue(gate.claim())
        assertFalse(gate.claim())
        gate.release()
        assertFalse(gate.isRunning)
        // The discarded request left nothing behind: a fresh claim wins and
        // finishes with no trailing pass.
        assertTrue(gate.claim())
        assertFalse(gate.finish())
    }

    @Test
    fun exactlyOneWinnerAmongConcurrentClaims() {
        val workers = 16
        val gate = PhotoBackupPassGate()
        val pool = Executors.newFixedThreadPool(workers)
        val start = CountDownLatch(1)
        val done = CountDownLatch(workers)
        val winners = AtomicInteger(0)
        repeat(workers) {
            pool.execute {
                start.await()
                if (gate.claim()) winners.incrementAndGet()
                done.countDown()
            }
        }
        start.countDown()
        done.await()
        pool.shutdown()
        assertEquals(1, winners.get())
        // Every loser asked for a re-run; they collapse to one.
        assertTrue(gate.finish())
        assertFalse(gate.finish())
    }
}
