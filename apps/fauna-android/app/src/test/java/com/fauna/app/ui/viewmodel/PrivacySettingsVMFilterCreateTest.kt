package com.fauna.app.ui.viewmodel

import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.AccountStores
import com.fauna.app.core.ApiClient
import com.fauna.app.core.InheritedFilterMarks
import com.fauna.app.core.SecureStorage
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiEmailFilter
import com.fauna.ffi.FfiEmailFilterAction
import com.fauna.ffi.FfiEmailFilterRule
import com.fauna.ffi.FfiFilterActionInputs
import com.fauna.ffi.FfiFilterRuleInputs
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.clearInvocations
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config

/**
 * [PrivacySettingsVM.createFilter] through the shared `OfflineQueued` composer
 * (`account-offline-mutation.md` § The offline-mutation contract): with no nest
 * the create is queued by the FFI door (`filtersCreateOrQueue`) and nothing is
 * re-read or listed — the rule has no server id until the drain; connected, it
 * is sent and the list is re-read as before.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class PrivacySettingsVMFilterCreateTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val context = ApplicationProvider.getApplicationContext<android.app.Application>()

    private val form = FilterForm(
        name = "Move newsletters",
        rule = FfiFilterRuleInputs(kind = "SenderDomain", value = "news.example"),
        action = FfiFilterActionInputs(
            kind = "Allow",
            rejectReason = "",
            forwardAddress = "",
            keepLocalCopy = false,
        ),
        priority = "",
        continueOnMatch = false,
    )

    /** The VM with `createEmailFilterOrQueue` answering [queued] for exactly
     *  the form's encoded values. */
    private fun make(queued: Boolean): Pair<PrivacySettingsVM, ApiClient> = runBlocking {
        val api = mock(ApiClient::class.java)
        whenever(api.filterMarksList()).thenReturn(emptyList())
        whenever(api.listEmailFilters()).thenReturn(emptyList<FfiEmailFilter>())
        whenever(
            api.createEmailFilterOrQueue(
                "Move newsletters",
                listOf(FfiEmailFilterRule.SenderDomain(domain = "news.example")),
                "all",
                FfiEmailFilterAction.Allow,
                0,
                false,
            ),
        ).thenReturn(queued)
        val store = InheritedFilterMarks(api, mock(AccountStores::class.java))
        store.reload()
        val vm = PrivacySettingsVM(api, mock(SecureStorage::class.java), store, context)
        clearInvocations(api)
        vm to api
    }

    @Test
    fun aFilterCreateWithNoNestIsQueuedAndListsNothing() = runBlocking<Unit> {
        val (vm, api) = make(queued = true)
        vm.createFilter(form)
        verify(api, never()).listEmailFilters()
        assertEquals(emptyList<FfiEmailFilter>(), vm.emailFilters.value)
        assertNull(vm.emailFilterError.value)
        assertFalse(vm.creatingFilter.value)
    }

    @Test
    fun aFilterCreateWithTheNestConnectedIsSentAndReListed() = runBlocking<Unit> {
        val (vm, api) = make(queued = false)
        vm.createFilter(form)
        verify(api).listEmailFilters()
        assertNull(vm.emailFilterError.value)
        assertFalse(vm.creatingFilter.value)
    }
}
