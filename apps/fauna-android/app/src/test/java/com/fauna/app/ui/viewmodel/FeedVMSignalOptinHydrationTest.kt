package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.app.core.ContentPolicyStore
import com.fauna.app.core.FamilyNotifyStore
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.WebPublishStore
import com.fauna.app.core.feed.FeedManagerHost
import com.fauna.ffi.FfiFeedManager
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.times
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever

/**
 * The Layer-B producer contributes only when the manager's cached opt-in is true,
 * and a fresh manager starts with that cache empty — so feed entry must re-read
 * the persisted opt-in beside the cue rollup, before the user ever opens
 * Personalization (`docs/goal/behavior/engagement-cues.md` § Layer B). linux, tui,
 * web and apple already do; android's feed entry did not. A failed read is
 * non-fatal: it leaves sharing off and never reaches `error-message`.
 */
@ExperimentalCoroutinesApi
class FeedVMSignalOptinHydrationTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private fun vmOver(manager: FfiFeedManager): FeedVM {
        val host = mock(FeedManagerHost::class.java)
        whenever(host.manager()).thenReturn(manager)
        val api = mock(ApiClient::class.java)
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        whenever(api.storeChangedTick).thenReturn(MutableSharedFlow())
        return FeedVM(
            host,
            api,
            mock(SecureStorage::class.java),
            mock(ContentPolicyStore::class.java),
            mock(FamilyNotifyStore::class.java),
            mock(WebPublishStore::class.java),
        )
    }

    @Test
    fun enteringTheFeedHydratesTheSignalOptinBesideTheCueRollup() {
        val manager = mock(FfiFeedManager::class.java)
        val vm = vmOver(manager)

        vm.start()

        runBlocking {
            verify(manager, times(1)).hydrateCues()
            verify(manager, times(1)).hydrateSignalOptin()
        }
    }

    @Test
    fun aFailedOptinReadIsNonFatalAndStaysOffTheCueErrorLine() {
        val manager = mock(FfiFeedManager::class.java)
        runBlocking { whenever(manager.hydrateSignalOptin()).thenThrow(RuntimeException("boom")) }
        val vm = vmOver(manager)

        vm.start()

        assertTrue(
            "a failed opt-in read must not surface on error-message",
            vm.cueError.value == null,
        )
    }
}
