package com.fauna.app.core.search

import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiSearchManager
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Test
import org.mockito.ArgumentMatchers
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.times
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import uniffi.fauna_client_search.SearchSnapshot

/**
 * `ArgumentMatchers.any()` returns a Java platform-typed `null`; passed
 * directly as an argument to a Kotlin non-null parameter (every FFI-facing
 * signature in this codebase), the compiler inserts a caller-side
 * `Intrinsics.checkNotNullExpressionValue` on the `any()` sub-expression
 * itself and throws before Mockito ever sees the call. The unchecked cast
 * breaks that direct platform-type flow (mockito-core carries no Kotlin-safe
 * `any()`, unlike the separate mockito-kotlin artifact this project doesn't
 * depend on).
 */
private fun <T> any(): T {
    ArgumentMatchers.any<T>()
    @Suppress("UNCHECKED_CAST")
    return null as T
}

/**
 * `SearchManagerHost` — the observer→snapshot bridge + local-index attach
 * retry for the shared `FfiSearchManager` (docs/goal/ui/search.md § State &
 * data shape, § Implementation status today). FFI-free (mocks `ApiClient` and
 * `FfiSearchManager` themselves, per `BackupsVMTest`'s pattern), so it runs
 * without host-JNA.
 *
 * The manager's own query/filter/paging/merge decisions are pinned by
 * `libs/fauna-ffi/src/search_manager.rs`'s own tests; what is left for this
 * host is the bookkeeping apple's `SearchVM.indexAttached` pioneered: a
 * `false` attach attempt must be retryable, a `true` one must not re-fire,
 * and a manager-identity change (sign-out→sign-in) must reset the flag.
 */
class SearchManagerHostTest {

    private fun blankSnapshot() =
        SearchSnapshot(
            query = "",
            typeFilter = "all",
            results = emptyList(),
            inFlight = false,
            noResults = false,
            hasMore = false,
            error = null,
        )

    @Test
    fun manager_returnsNullWhenApiHasNoConnection() {
        val api = mock(ApiClient::class.java)
        whenever(api.searchManager(any())).thenReturn(null)
        val host = SearchManagerHost(api)

        assertNull(host.manager())
    }

    @Test
    fun manager_reusesTheSameInstanceAcrossCalls() {
        val api = mock(ApiClient::class.java)
        val mgr = mock(FfiSearchManager::class.java)
        whenever(mgr.snapshot()).thenReturn(blankSnapshot())
        whenever(api.searchManager(any())).thenReturn(mgr)
        val host = SearchManagerHost(api)

        val first = host.manager()
        val second = host.manager()

        assertSame(first, second)
    }

    @Test
    fun attachLocalIndexIfNeeded_isANoOpBeforeAnyManagerIsBuilt() = runTest {
        val api = mock(ApiClient::class.java)
        val host = SearchManagerHost(api)

        host.attachLocalIndexIfNeeded()

        verify(api, never()).attachLocalSearchIndex(any())
    }

    @Test
    fun attachLocalIndexIfNeeded_retriesAfterAFalseReturn() = runTest {
        val api = mock(ApiClient::class.java)
        val mgr = mock(FfiSearchManager::class.java)
        whenever(mgr.snapshot()).thenReturn(blankSnapshot())
        whenever(api.searchManager(any())).thenReturn(mgr)
        whenever(api.attachLocalSearchIndex(mgr)).thenReturn(false)
        val host = SearchManagerHost(api)
        host.manager()

        host.attachLocalIndexIfNeeded()
        host.attachLocalIndexIfNeeded()

        // A normal state (no session yet, or this actor has no mail) must stay
        // retryable — a false attempt must not be treated as settled.
        verify(api, times(2)).attachLocalSearchIndex(mgr)
    }

    @Test
    fun attachLocalIndexIfNeeded_stopsRetryingOnceItSucceeds() = runTest {
        val api = mock(ApiClient::class.java)
        val mgr = mock(FfiSearchManager::class.java)
        whenever(mgr.snapshot()).thenReturn(blankSnapshot())
        whenever(api.searchManager(any())).thenReturn(mgr)
        whenever(api.attachLocalSearchIndex(mgr)).thenReturn(true)
        val host = SearchManagerHost(api)
        host.manager()

        host.attachLocalIndexIfNeeded()
        host.attachLocalIndexIfNeeded()

        verify(api, times(1)).attachLocalSearchIndex(mgr)
    }

    @Test
    fun aFreshManagerInstanceResetsTheAttachRetryFlag() = runTest {
        val api = mock(ApiClient::class.java)
        val oldMgr = mock(FfiSearchManager::class.java)
        whenever(oldMgr.snapshot()).thenReturn(blankSnapshot())
        whenever(api.searchManager(any())).thenReturn(oldMgr)
        whenever(api.attachLocalSearchIndex(oldMgr)).thenReturn(true)
        val host = SearchManagerHost(api)
        host.manager()
        host.attachLocalIndexIfNeeded()

        // Sign-out -> sign-in: ApiClient hands back a brand-new manager
        // instance over the fresh connection.
        val newMgr = mock(FfiSearchManager::class.java)
        whenever(newMgr.snapshot()).thenReturn(blankSnapshot())
        whenever(api.searchManager(any())).thenReturn(newMgr)
        whenever(api.attachLocalSearchIndex(newMgr)).thenReturn(true)
        host.manager()

        host.attachLocalIndexIfNeeded()

        // The new actor's arm must be attached for real, not skipped because
        // the OLD manager's flag was still set — and the old manager's own
        // (already-succeeded) attach call is not repeated.
        verify(api, times(1)).attachLocalSearchIndex(newMgr)
        verify(api, times(1)).attachLocalSearchIndex(oldMgr)
    }
}
