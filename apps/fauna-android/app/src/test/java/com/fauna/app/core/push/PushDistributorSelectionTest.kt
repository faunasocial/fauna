package com.fauna.app.core.push

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * [pickDistributor], the UnifiedPush-first rule (`android.md` § Push
 * Notifications): a distributor the user installed always wins over this
 * app's own embedded one, which is taken only when no other is installed.
 * The embedded distributor lists the app's own package among the installed
 * distributors, so [OWN] in `installed` stands for "embedded present".
 */
class PushDistributorSelectionTest {
    @Test
    fun noDistributorAtAllPicksNothing() {
        assertNull(pickDistributor(saved = null, installed = emptyList(), ownPackage = OWN))
    }

    @Test
    fun embeddedAloneIsTheFallback() {
        assertEquals(OWN, pickDistributor(saved = null, installed = listOf(OWN), ownPackage = OWN))
    }

    @Test
    fun aUserDistributorWinsOverTheEmbeddedOne() {
        assertEquals(NTFY, pickDistributor(saved = null, installed = listOf(OWN, NTFY), ownPackage = OWN))
    }

    @Test
    fun aUserDistributorWithoutTheEmbeddedOneIsPicked() {
        assertEquals(NTFY, pickDistributor(saved = null, installed = listOf(NTFY), ownPackage = OWN))
    }

    @Test
    fun aSavedUserDistributorStillInstalledIsKept() {
        assertEquals(NEXTPUSH, pickDistributor(saved = NEXTPUSH, installed = listOf(OWN, NTFY, NEXTPUSH), ownPackage = OWN))
    }

    @Test
    fun aSavedEmbeddedPickYieldsToAUserDistributorInstalledSince() {
        assertEquals(NTFY, pickDistributor(saved = OWN, installed = listOf(OWN, NTFY), ownPackage = OWN))
    }

    @Test
    fun aSavedDistributorNoLongerInstalledFallsThrough() {
        assertEquals(OWN, pickDistributor(saved = NTFY, installed = listOf(OWN), ownPackage = OWN))
    }

    private companion object {
        const val OWN = "social.fauna.fauna"
        const val NTFY = "io.heckel.ntfy"
        const val NEXTPUSH = "org.unifiedpush.distributor.nextpush"
    }
}
