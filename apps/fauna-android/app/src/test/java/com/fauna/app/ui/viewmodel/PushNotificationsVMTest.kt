package com.fauna.app.ui.viewmodel

import com.fauna.app.core.push.PushHost
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
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
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever

/**
 * [PushNotificationsVM]: the toggle reads the stored bit after every attempt,
 * never the tap — a failed enable settles back off with its line, a refused
 * permission registers nothing (`settings.md` § Push notifications).
 */
@OptIn(ExperimentalCoroutinesApi::class)
class PushNotificationsVMTest {
    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val host: PushHost = mock(PushHost::class.java)

    @Test
    fun aSuccessfulEnableReadsTheStoredBitOn() = runTest {
        whenever(host.optedIn()).thenReturn(false)
        val vm = PushNotificationsVM(host)
        whenever(host.enable()).thenReturn(null)
        whenever(host.optedIn()).thenReturn(true)
        vm.setOptIn(true)
        assertTrue(vm.optedIn.value)
        assertNull(vm.line.value)
        assertFalse(vm.busy.value)
    }

    @Test
    fun noDistributorSettlesBackOffAndSaysWhy() = runTest {
        whenever(host.optedIn()).thenReturn(false)
        whenever(host.enable()).thenReturn(PushHost.EnableFailure.NoDistributor)
        val vm = PushNotificationsVM(host)
        vm.setOptIn(true)
        assertFalse(vm.optedIn.value)
        assertEquals(PushNotificationsVM.Line.NO_DISTRIBUTOR, vm.line.value)
    }

    @Test
    fun aFailedRegistrationSaysTheUpdateFailed() = runTest {
        whenever(host.optedIn()).thenReturn(false)
        whenever(host.enable()).thenReturn(PushHost.EnableFailure.Failed("timeout"))
        val vm = PushNotificationsVM(host)
        vm.setOptIn(true)
        assertFalse(vm.optedIn.value)
        assertEquals(PushNotificationsVM.Line.UPDATE_FAILED, vm.line.value)
    }

    @Test
    fun offDisablesAndReadsTheBitBack() = runTest {
        whenever(host.optedIn()).thenReturn(true)
        val vm = PushNotificationsVM(host)
        whenever(host.optedIn()).thenReturn(false)
        vm.setOptIn(false)
        verify(host).disable()
        assertFalse(vm.optedIn.value)
        assertNull(vm.line.value)
    }

    @Test
    fun aRefusedPermissionRegistersNothing() = runTest {
        whenever(host.optedIn()).thenReturn(false)
        val vm = PushNotificationsVM(host)
        vm.permissionRefused()
        assertFalse(vm.optedIn.value)
        assertEquals(PushNotificationsVM.Line.PERMISSION_DENIED, vm.line.value)
    }
}
