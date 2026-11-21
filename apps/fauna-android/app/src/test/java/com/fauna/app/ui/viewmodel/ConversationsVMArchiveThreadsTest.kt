package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.app.core.ContentPolicyStore
import com.fauna.app.core.FamilyNotifyStore
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
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import uniffi.fauna_conversations.ConversationsSession

/**
 * The `Archive` rail's re-read (`archive-import.md` § What each category
 * becomes; `conversations-archive-rail`): a visit to the conversations page
 * asks the live session to read the imported archives' message threads again.
 * The rows then arrive in the shared snapshot already read-only (no send
 * capability), so the re-read is the whole app-side half. FFI-free: the
 * session and [ApiClient] are mocks, so this runs without host-JNA.
 */
@ExperimentalCoroutinesApi
class ConversationsVMArchiveThreadsTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private fun vm(host: ConversationsManagerHost, api: ApiClient): ConversationsVM {
        whenever(host.snapshot).thenReturn(MutableStateFlow(null))
        val aftermath = mock(SuccessionAftermathHost::class.java)
        whenever(aftermath.configStageSettled).thenReturn(MutableSharedFlow())
        return ConversationsVM(
            host,
            api,
            mock(ContentPolicyStore::class.java),
            mock(FamilyNotifyStore::class.java),
            aftermath,
        )
    }

    @Test
    fun visit_rereads_the_archive_threads_over_the_live_session() = runTest {
        val session = mock(ConversationsSession::class.java)
        val host = mock(ConversationsManagerHost::class.java)
        whenever(host.session).thenReturn(session)
        val api = mock(ApiClient::class.java)

        vm(host, api).refreshArchiveThreads()

        verify(api).refreshArchiveThreads(session)
    }

    @Test
    fun no_session_is_a_no_op() = runTest {
        val host = mock(ConversationsManagerHost::class.java)
        whenever(host.session).thenReturn(null)
        val api = mock(ApiClient::class.java)
        // Built before `verify(...)`: a `mock(...)` call inside the verify's
        // argument list ends the verification early (UnfinishedVerification).
        val anySession = mock(ConversationsSession::class.java)

        vm(host, api).refreshArchiveThreads()

        verify(api, never()).refreshArchiveThreads(anySession)
    }
}
