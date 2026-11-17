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
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import uniffi.fauna_feed.FeedComposeState
import uniffi.fauna_feed.FeedSnapshot
import uniffi.fauna_feed.TrainVerb

/**
 * A refused training gesture (`feed-post-more-like-this` / `-less-like-this`)
 * is the shared manager's own page error: `train_post`/`untrain_post` stamp
 * `feed.error_train` on `FeedSnapshot.error` (`libs/fauna-feed`'s
 * `report_train`, pinned there), which the feed screen already paints on
 * `error-message`. The VM therefore runs the gesture against the manager and
 * bridges NOTHING of the returned refusal onto a channel of its own — a
 * second paint would race or shadow the snapshot's line (the shape tui and
 * linux keep; `docs/goal/behavior/topic-factors.md` § Goal).
 */
@ExperimentalCoroutinesApi
class FeedVMTrainRefusalTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val postId = "post-1"
    private val factor = "topic:cats"

    private class Harness(val vm: FeedVM, val manager: FfiFeedManager)

    private fun harness(): Harness {
        val manager = mock(FfiFeedManager::class.java)
        val snapshot = mock(FeedSnapshot::class.java)
        whenever(snapshot.compose).thenReturn(
            FeedComposeState(
                text = "",
                tags = "",
                attachedFile = null,
                gateTier = null,
                gatePreview = "",
                sell = null,
                gateRoom = null,
                error = null,
                submitting = false,
            ),
        )
        whenever(manager.snapshot()).thenReturn(snapshot)
        runBlocking {
            whenever(manager.trainPost(postId, factor, TrainVerb.MORE_LIKE_THIS))
                .thenAnswer { throw com.fauna.ffi.FfiException.General("the model could not be opened") }
            whenever(manager.untrainPost(postId, factor))
                .thenAnswer { throw com.fauna.ffi.FfiException.General("the model could not be opened") }
        }
        val host = mock(FeedManagerHost::class.java)
        whenever(host.manager()).thenReturn(manager)
        val api = mock(ApiClient::class.java)
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        whenever(api.storeChangedTick).thenReturn(MutableSharedFlow())
        val store = mock(ContentPolicyStore::class.java)
        whenever(store.spamMarked).thenReturn(MutableStateFlow(emptyList()))
        val vm = FeedVM(
            host,
            api,
            mock(SecureStorage::class.java),
            store,
            mock(FamilyNotifyStore::class.java),
            mock(WebPublishStore::class.java),
        )
        return Harness(vm, manager)
    }

    private fun assertNoVmChannelCarriesIt(vm: FeedVM) {
        assertNull(vm.cueError.value)
        assertNull(vm.deleteError.value)
        assertNull(vm.markSpamError.value)
        assertNull(vm.buyUnlockError.value)
    }

    @Test
    fun aRefusedTrainRunsOnTheManagerAndIsLeftToTheSnapshotError() {
        val h = harness()

        h.vm.dispatchTrain(postId, factor, TrainVerb.MORE_LIKE_THIS, alreadyMarked = false)

        runBlocking {
            verify(h.manager).trainPost(postId, factor, TrainVerb.MORE_LIKE_THIS)
            verify(h.manager, never()).untrainPost(postId, factor)
        }
        assertNoVmChannelCarriesIt(h.vm)
    }

    @Test
    fun aRefusedUntrainOfTheMarkedVerbIsLeftToTheSnapshotErrorToo() {
        val h = harness()

        h.vm.dispatchTrain(postId, factor, TrainVerb.MORE_LIKE_THIS, alreadyMarked = true)

        runBlocking { verify(h.manager).untrainPost(postId, factor) }
        assertNoVmChannelCarriesIt(h.vm)
    }
}
