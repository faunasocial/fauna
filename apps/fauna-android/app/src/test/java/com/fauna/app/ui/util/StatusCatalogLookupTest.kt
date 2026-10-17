package com.fauna.app.ui.util

import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

/**
 * The catalog handed to the shared `statusText` projection (`ui/status.md`
 * § State & data shape): a known key resolves to its generated android string
 * resource with the `{name}` tokens intact (the shared projection fills them),
 * and an unknown key is `null` so the projection falls back to the key itself.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class StatusCatalogLookupTest {
    private val lookup get() = StatusCatalogLookup(RuntimeEnvironment.getApplication())

    @Test
    fun aKnownKeyResolvesToItsTemplateWithTheTokensIntact() {
        assertEquals("{files} files, {bytes}", lookup.lookup("status.sync.pending_summary"))
        assertEquals("Never", lookup.lookup("common.never"))
    }

    @Test
    fun anUnknownKeyIsNullNotBlank() {
        assertNull(lookup.lookup("status.no.such.key"))
    }
}
