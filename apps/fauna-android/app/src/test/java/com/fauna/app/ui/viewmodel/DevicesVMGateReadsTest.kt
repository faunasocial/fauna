package com.fauna.app.ui.viewmodel

import android.content.Context
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ResolveService
import com.fauna.app.core.SecureStorage
import com.fauna.app.p2pshare.OfflineShareHost
import com.fauna.ffi.FfiTierItem
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
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import uniffi.fauna_core.ReadState
import org.mockito.Mockito.`when` as whenever

/**
 * The Folders page's two gate reads (`ui/folders.md` § Errors & edge cases,
 * *Own-tier and mail gates*; tui's `webdav_read` / `own_tiers_read`): the
 * mail-custody read behind `folder-webdav-toggle` and the own-tier read behind
 * `folder-paywall-tier-select` each carry where they stand — `PENDING` until
 * answered, `FAILED` on error, `RESOLVED` on an answer — so the shared answer
 * the controls are painted from can tell an unanswered read from "no MSEK" /
 * "no tier". FFI-free: a mocked [ApiClient], as [DevicesVMEnrollmentNoticeTest].
 */
@ExperimentalCoroutinesApi
class DevicesVMGateReadsTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private fun makeVm(): Pair<DevicesVM, ApiClient> {
        val api = mock(ApiClient::class.java)
        val appContext = mock(Context::class.java)
        whenever(api.folderChangedTick).thenReturn(MutableSharedFlow())
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        whenever(api.devicesStaleTick).thenReturn(MutableSharedFlow())
        whenever(api.storeChangedTick).thenReturn(MutableSharedFlow())
        val vm = DevicesVM(
            api,
            mock(ResolveService::class.java),
            mock(SecureStorage::class.java),
            appContext,
            OfflineShareHost(appContext),
            FakeOnDemandToggle(),
            NoSharePlaneReadings,
        )
        return vm to api
    }

    private fun tier(name: String, rank: UInt) = FfiTierItem(
        name = name,
        rank = rank,
        description = null,
        priceHint = null,
        askingPriceSats = null,
        paymentUrl = null,
        autoApprove = false,
        createdAt = 0uL,
        unlocksPost = null,
    )

    @Test
    fun bothReadsArePendingBeforeTheyAnswer() {
        val (vm, _) = makeVm()
        assertEquals(ReadState.PENDING, vm.webdavRead.value)
        assertEquals(ReadState.PENDING, vm.ownTiersRead.value)
    }

    @Test
    fun anAnsweredCustodyReadIsResolved_withItsAnswer() = runTest {
        val (vm, api) = makeVm()
        whenever(api.canServeWebdav()).thenReturn(false)

        vm.refreshWebdavCapability()

        assertEquals(ReadState.RESOLVED, vm.webdavRead.value)
        assertFalse(vm.canServeWebdav.value)
    }

    @Test
    fun aFailedCustodyReadIsFailed_neverAnAnsweredNo() = runTest {
        val (vm, api) = makeVm()
        whenever(api.canServeWebdav()).thenReturn(true).thenThrow(RuntimeException("nest away"))
        vm.refreshWebdavCapability()
        assertTrue(vm.canServeWebdav.value)

        vm.refreshWebdavCapability()

        assertEquals(ReadState.FAILED, vm.webdavRead.value)
        assertFalse(vm.canServeWebdav.value)
    }

    @Test
    fun anAnsweredTierReadIsResolved_evenWithNoTiers() = runTest {
        val (vm, api) = makeVm()
        whenever(api.subscriptionTiersList()).thenReturn(emptyList())

        vm.refreshOwnTiers()

        assertEquals(ReadState.RESOLVED, vm.ownTiersRead.value)
        assertEquals(emptyList<String>(), vm.ownTiers.value)
    }

    @Test
    fun anAnsweredTierReadListsTheNamesByRank() = runTest {
        val (vm, api) = makeVm()
        whenever(api.subscriptionTiersList()).thenReturn(listOf(tier("gold", 2u), tier("silver", 1u)))

        vm.refreshOwnTiers()

        assertEquals(ReadState.RESOLVED, vm.ownTiersRead.value)
        assertEquals(listOf("silver", "gold"), vm.ownTiers.value)
    }

    @Test
    fun aFailedTierReadIsFailed_neverNoTier() = runTest {
        val (vm, api) = makeVm()
        whenever(api.subscriptionTiersList()).thenThrow(RuntimeException("nest away"))

        vm.refreshOwnTiers()

        assertEquals(ReadState.FAILED, vm.ownTiersRead.value)
        assertEquals(emptyList<String>(), vm.ownTiers.value)
    }
}
