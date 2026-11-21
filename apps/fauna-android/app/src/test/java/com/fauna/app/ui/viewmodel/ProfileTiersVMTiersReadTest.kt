package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.app.payments.PaymentsSiteOrigin
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiNestClient
import com.fauna.ffi.FfiPaymentsClient
import com.fauna.ffi.FfiTierItem
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.robolectric.annotation.Config
import uniffi.fauna_core.ReadState
import org.mockito.Mockito.`when` as whenever

/**
 * The profile Tiers tab's tier-read state ([ProfileTiersVM.tiersRead]) — the
 * read the two controls that need a tier are painted from (`profile.md`
 * § Errors & edge cases, *A tier before what sells it*; tui's
 * `AuthorState::tiers_read`): `PENDING` until the tab's first read answers,
 * `FAILED` only when it failed before ever answering, and an answered list
 * stands through a later failure. The reads are a mocked [ApiClient]; the
 * payments reads go through `nestRpc().payments()`, mocked the same way.
 */
@ExperimentalCoroutinesApi
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ProfileTiersVMTiersReadTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private fun tier(name: String) = FfiTierItem(
        name = name,
        rank = 1u,
        description = null,
        priceHint = null,
        askingPriceSats = null,
        paymentUrl = null,
        autoApprove = false,
        createdAt = 0uL,
        unlocksPost = null,
    )

    private suspend fun makeVm(): Pair<ProfileTiersVM, ApiClient> {
        val api = mock(ApiClient::class.java)
        val nest = mock(FfiNestClient::class.java)
        val payments = mock(FfiPaymentsClient::class.java)
        whenever(api.nestRpc()).thenReturn(nest)
        whenever(nest.payments()).thenReturn(payments)
        whenever(payments.providersList()).thenReturn(emptyList())
        whenever(payments.claimsList()).thenReturn(emptyList())
        whenever(api.subscriptionRequestsList()).thenReturn(emptyList())
        whenever(api.subscriptionSubscribersList("gold")).thenReturn(emptyList())
        val origin = mock(PaymentsSiteOrigin::class.java)
        whenever(origin.origin).thenReturn(MutableStateFlow(null))
        return ProfileTiersVM(api, origin) to api
    }

    @Test
    fun theReadIsPendingUntilItFirstAnswers() = runTest {
        val (vm, _) = makeVm()
        assertEquals(ReadState.PENDING, vm.tiersRead.value)
    }

    @Test
    fun anAnsweredReadIsResolved_evenWithNoTiers() = runTest {
        val (vm, api) = makeVm()
        whenever(api.subscriptionTiersList()).thenReturn(emptyList())

        vm.refreshAll()

        assertEquals(ReadState.RESOLVED, vm.tiersRead.value)
        assertEquals(emptyList<FfiTierItem>(), vm.tiers.value)
    }

    @Test
    fun aReadThatFailsBeforeEverAnsweringIsFailed() = runTest {
        val (vm, api) = makeVm()
        whenever(api.subscriptionTiersList()).thenThrow(RuntimeException("nest away"))

        vm.refreshAll()

        assertEquals(ReadState.FAILED, vm.tiersRead.value)
    }

    @Test
    fun aLaterFailureLeavesAnAnsweredListStanding() = runTest {
        val (vm, api) = makeVm()
        whenever(api.subscriptionTiersList())
            .thenReturn(listOf(tier("gold")))
            .thenThrow(RuntimeException("nest away"))
        vm.refreshAll()
        assertEquals(ReadState.RESOLVED, vm.tiersRead.value)

        vm.refreshAll()

        assertEquals(ReadState.RESOLVED, vm.tiersRead.value)
        assertEquals(listOf("gold"), vm.tiers.value.map { it.name })
    }

    @Test
    fun aFailedReadThatLaterAnswersIsResolved() = runTest {
        val (vm, api) = makeVm()
        whenever(api.subscriptionTiersList())
            .thenThrow(RuntimeException("nest away"))
            .thenReturn(listOf(tier("gold")))
        vm.refreshAll()
        assertEquals(ReadState.FAILED, vm.tiersRead.value)

        vm.refreshAll()

        assertEquals(ReadState.RESOLVED, vm.tiersRead.value)
    }
}
