package com.fauna.app.ui.viewmodel

import com.fauna.app.core.AftermathProgressStore
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ContentPolicyStore
import com.fauna.app.core.FamilyNotifyStore
import com.fauna.app.core.InheritedFilterMarks
import com.fauna.app.core.SuccessionAftermathHost
import com.fauna.app.core.conversations.ConversationsManagerHost
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.times
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import uniffi.fauna_client_config.MutedWordsSnapshot
import uniffi.fauna_conversations.ConversationsSnapshot
import uniffi.fauna_core.MutedKeyword

/**
 * The store-change notice on an open conversation's muted-word list
 * (account-runtime.md § Multi-instance concurrency → *A runtime's own pump is
 * a source of the notice too*, part 4): once a detail has loaded the list,
 * [ApiClient.storeChangedTick] re-reads it, so a word muted on another device
 * collapses its bubbles with no re-open; a failed re-read keeps the list.
 * FFI-free, the [MutedWordsVMTest] shape.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ConversationsVMMutedKeywordsTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private fun snapshot(vararg words: String) = MutedWordsSnapshot(
        keywords = words.map { MutedKeyword(keyword = it, weight = 1L) },
        loaded = true,
    )

    private fun makeVm(api: ApiClient, tick: MutableSharedFlow<Unit>): ConversationsVM {
        // Stubbed BEFORE construction: `init` collects both.
        val host = mock(ConversationsManagerHost::class.java)
        whenever(host.snapshot).thenReturn(MutableStateFlow<ConversationsSnapshot?>(null))
        whenever(api.storeChangedTick).thenReturn(tick)
        return ConversationsVM(
            host,
            api,
            mock(ContentPolicyStore::class.java),
            mock(FamilyNotifyStore::class.java),
            SuccessionAftermathHost(mock(InheritedFilterMarks::class.java), mock(AftermathProgressStore::class.java)),
        )
    }

    @Test
    fun aStoreChangeTickReReadsAnOpenConversationsMutedWords() = runTest {
        val api = mock(ApiClient::class.java)
        val tick = MutableSharedFlow<Unit>(replay = 0, extraBufferCapacity = 1)
        whenever(api.mutedKeywordsList())
            .thenReturn(snapshot("spoiler"))
            .thenReturn(snapshot("spoiler", "finale"))
            .thenThrow(RuntimeException("store unreadable"))
        val vm = makeVm(api, tick)

        vm.loadMutedKeywords()
        assertEquals(listOf("spoiler"), vm.mutedKeywords.value.map { it.keyword })

        tick.tryEmit(Unit)
        assertEquals(listOf("spoiler", "finale"), vm.mutedKeywords.value.map { it.keyword })

        tick.tryEmit(Unit)
        verify(api, times(3)).mutedKeywordsList()
        assertEquals(
            "a failed re-read keeps the list",
            listOf("spoiler", "finale"),
            vm.mutedKeywords.value.map { it.keyword },
        )
    }

    @Test
    fun noDetailMountedNoRead() = runTest {
        val api = mock(ApiClient::class.java)
        val tick = MutableSharedFlow<Unit>(replay = 0, extraBufferCapacity = 1)
        makeVm(api, tick)

        tick.tryEmit(Unit)

        verify(api, never()).mutedKeywordsList()
    }
}
