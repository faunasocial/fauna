package com.fauna.app.ui.viewmodel

import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.AccountStores
import com.fauna.app.core.ApiClient
import com.fauna.app.core.InheritedFilterMarks
import com.fauna.app.core.SecureStorage
import com.fauna.app.testing.FaunaRobolectricTestRunner
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.ArgumentMatchers.anyLong
import org.mockito.Mockito.inOrder
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config

/**
 * [PrivacySettingsVM]'s half of the post-succession filter review
 * (`succession-aftermath.md` § Adjudicating what the aftermath carries across,
 * the email-filter plane): Keep rides the shared `filter_mark_keep` door and
 * surfaces a failure; `filter-delete` is the Remove half, so a marked rule's
 * `filter_mark_removed` is recorded strictly AFTER the delete landed — and
 * never when the delete failed.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class PrivacySettingsVMFilterMarksTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val context = ApplicationProvider.getApplicationContext<android.app.Application>()

    /** The marks the next read returns — mutable so an answer can move it
     *  without re-stubbing the mock mid-invocation. */
    private var nextMarks: List<Long> = emptyList()

    private fun make(marks: List<Long>): Pair<PrivacySettingsVM, ApiClient> = runBlocking {
        val api = mock(ApiClient::class.java)
        nextMarks = marks
        whenever(api.filterMarksList()).thenAnswer { nextMarks }
        whenever(api.listEmailFilters()).thenReturn(emptyList())
        val store = InheritedFilterMarks(api, mock(AccountStores::class.java))
        store.reload()
        PrivacySettingsVM(api, mock(SecureStorage::class.java), store, context) to api
    }

    @Test
    fun deletingAMarkedRuleRecordsRemovedAfterTheDelete() = runBlocking<Unit> {
        val (vm, api) = make(listOf(4L))
        vm.deleteFilter(4L)
        val order = inOrder(api)
        order.verify(api).deleteEmailFilter(4L)
        order.verify(api).filterMarkRemoved(4L)
    }

    @Test
    fun aFailedDeleteRecordsNoVerdict() = runBlocking<Unit> {
        val (vm, api) = make(listOf(4L))
        whenever(api.deleteEmailFilter(4L)).thenThrow(RuntimeException("nest unreachable"))
        vm.deleteFilter(4L)
        verify(api, never()).filterMarkRemoved(anyLong())
        assertNotNull(vm.emailFilterError.value)
    }

    @Test
    fun keepRidesTheSharedDoorAndClearsTheMark() = runBlocking<Unit> {
        val (vm, api) = make(listOf(4L))
        whenever(api.filterMarkKeep(4L)).thenAnswer { nextMarks = emptyList(); true }
        vm.keepFilterMark(4L)
        verify(api).filterMarkKeep(4L)
        assertEquals(emptyList<Long>(), vm.filterMarks.value)
    }

    @Test
    fun aFailedKeepIsSurfaced() = runBlocking<Unit> {
        val (vm, api) = make(listOf(4L))
        whenever(api.filterMarkKeep(4L)).thenThrow(RuntimeException("ledger write refused"))
        vm.keepFilterMark(4L)
        assertEquals("ledger write refused", vm.emailFilterError.value)
        assertEquals(listOf(4L), vm.filterMarks.value)
    }
}
