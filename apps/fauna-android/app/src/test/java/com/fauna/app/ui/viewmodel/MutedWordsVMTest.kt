package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.times
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import uniffi.fauna_client_config.MutedWordsSnapshot

/**
 * The store-change notice on the Muted words page
 * (account-runtime.md § Multi-instance concurrency → *A runtime's own pump is
 * a source of the notice too*, part 4): an OPEN page re-runs its own load when
 * [ApiClient.storeChangedTick] fires, so another device's word appears with no
 * re-visit. FFI-free — [ApiClient] is mocked, as in [DevicesVMTest], and the
 * tick is stubbed before construction because `init` collects it.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class MutedWordsVMTest {

    /** `common.needs_nest`, as the shared crate words it (`fauna_i18n::strings::common::NEEDS_NEST`). */
    private val NEEDS_NEST = "Needs a connection to your nest"

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    @Test
    fun aStoreChangeTickReRunsTheOpenPagesLoad() = runTest {
        val api = mock(ApiClient::class.java)
        val tick = MutableSharedFlow<Unit>(replay = 0, extraBufferCapacity = 1)
        whenever(api.storeChangedTick).thenReturn(tick)
        // Two distinguishable answers without a term: the record's `loaded`
        // bit. The mount's read answers the first, the notice's the second.
        whenever(api.mutedKeywordsList())
            .thenReturn(MutedWordsSnapshot(keywords = emptyList(), loaded = false))
            .thenReturn(MutedWordsSnapshot(keywords = emptyList(), loaded = true))

        val vm = MutedWordsVM(api)
        assertFalse(vm.page.value.loaded)
        verify(api, times(1)).mutedKeywordsList()

        tick.tryEmit(Unit)

        assertTrue(
            "the notice re-ran the load and published its answer",
            vm.page.value.loaded,
        )
        verify(api, times(2)).mutedKeywordsList()
    }

    // The first-listing gate's refusal (`common.needs_nest`) reaches the load as
    // `FfiException.General(msg)`; the page says why on `error-message`, and no
    // empty state paints (`loaded` stays false). Needs the host FFI library for
    // the shared `refusedAsNotReady` predicate (account-client-lifecycle.md
    // § The first listing, clauses (4)/(5)).
    @Test
    fun aRefusedLoadShowsTheReasonAndNoEmptyState() = runTest {
        val api = mock(ApiClient::class.java)
        whenever(api.storeChangedTick).thenReturn(MutableSharedFlow())
        whenever(api.mutedKeywordsList())
            .thenAnswer { throw FfiException.General(msg = NEEDS_NEST) }

        val vm = MutedWordsVM(api)
        testScheduler.advanceUntilIdle()

        assertEquals(NEEDS_NEST, vm.errorMessage.value)
        assertFalse(vm.page.value.loaded)
    }

    @Test
    fun anyOtherFailedLoadStaysQuiet() = runTest {
        val api = mock(ApiClient::class.java)
        whenever(api.storeChangedTick).thenReturn(MutableSharedFlow())
        whenever(api.mutedKeywordsList())
            .thenAnswer { throw FfiException.General(msg = "connection reset") }

        val vm = MutedWordsVM(api)
        testScheduler.advanceUntilIdle()

        assertNull(vm.errorMessage.value)
    }

    @Test
    fun noTickNoSecondRead() = runTest {
        val api = mock(ApiClient::class.java)
        whenever(api.storeChangedTick).thenReturn(MutableSharedFlow())
        whenever(api.mutedKeywordsList())
            .thenReturn(MutedWordsSnapshot(keywords = emptyList(), loaded = true))

        MutedWordsVM(api)

        verify(api, times(1)).mutedKeywordsList()
    }
}
