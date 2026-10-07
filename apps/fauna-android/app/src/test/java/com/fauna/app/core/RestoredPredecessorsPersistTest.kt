package com.fauna.app.core

import com.fauna.ffi.FfiAccountRegistry
import com.fauna.ffi.onboarding.RestoredPredecessorSeed
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The phrase-restore handoff's predecessor persist (`identity-succession.md`
 * § Seed escrow → *Restore path*): the seeds a restore recovered reach the
 * registry linked to the restored identity, NAMED explicitly — an add-account
 * restore persists before the switch that activates it. Over the REAL shared
 * registry (`just android-host-test`).
 */
class RestoredPredecessorsPersistTest {

    @Test
    fun theSeedsAreLinkedToTheNamedRestoredIdentity() {
        val registry = FfiAccountRegistry(LogicalSecretStore(MemoryBackend()))
        val predecessor = FfiAccountRegistry(LogicalSecretStore(MemoryBackend()))
            .confirmIdentity(PREDECESSOR_SECRET, false)
        val live = registry.confirmIdentity(LIVE_SECRET, false)
        // An add-account restore: the restored identity is registered but NOT
        // active — the link must name it, not whatever is active.
        val restored = registry.addAccount(RESTORED_SECRET, null, null)
        assertEquals(live, registry.active())

        persistRestoredPredecessors(
            registry,
            restored,
            listOf(RestoredPredecessorSeed(actorIdHex = predecessor, seedHex = PREDECESSOR_SECRET)),
        )

        assertEquals(listOf(predecessor), registry.predecessorsOf(restored))
        assertTrue(registry.predecessorsOf(live).isEmpty())
    }

    /** An ordinary onboarding recovered nothing — the persist is a no-op. */
    @Test
    fun nothingRecoveredWritesNothing() {
        val registry = FfiAccountRegistry(LogicalSecretStore(MemoryBackend()))
        val restored = registry.confirmIdentity(RESTORED_SECRET, false)
        persistRestoredPredecessors(registry, restored, emptyList())
        assertTrue(registry.predecessorsOf(restored).isEmpty())
    }

    private companion object {
        const val LIVE_SECRET =
            "2120201f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403"
        const val RESTORED_SECRET =
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20"
        const val PREDECESSOR_SECRET =
            "a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebf"
    }
}
