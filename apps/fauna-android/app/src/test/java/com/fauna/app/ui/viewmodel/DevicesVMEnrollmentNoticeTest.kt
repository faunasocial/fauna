package com.fauna.app.ui.viewmodel

import android.content.Context
import com.fauna.app.core.ApiClient
import com.fauna.app.p2pshare.OfflineShareHost
import com.fauna.app.core.ResolveService
import com.fauna.app.core.SecureStorage
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.`when` as whenever

/**
 * `DevicesVM`'s standing device-cap-refusal notice
 * (`docs/goal/ui/devices.md` § Errors & edge cases; the four-app UniFFI
 * trickle-down off `FfiNestClient::account_enrollment_notice`, android's leg. Covers the
 * platform-glue half this VM owns: [DevicesVM.loadEnrollmentNotice] rides
 * [DevicesVM.start] beside [DevicesVM.loadCustodyFacet] (same edge), and a
 * failed read keeps whatever notice already stood rather than flickering it
 * off — only a genuine fresh `null` clears it, mirroring
 * [DevicesVM.loadCustodyFacet]'s null-fold. The precedence over a roster
 * gesture's own error, and the re-paint through the shared `error-message`
 * banner, live in [DevicesScreen]'s combined `LaunchedEffect` and are NOT
 * covered here — this VM never renders, it only holds state (the notice
 * never enters the stateless `DevicesRosterContent` at all).
 *
 * FFI-free (mocks [ApiClient] itself, mirroring [DevicesVMTest] /
 * [DevicesVMDestinationPlacesTest]).
 *
 * The android e2e leg (`test_device_cap_refusal.py --app android`) is
 * emulator-host-gated (android emulator) and not runnable from this
 * session — this VM test is the verification ceiling here, same caveat
 * every other android row in this area carries.
 */
@ExperimentalCoroutinesApi
class DevicesVMEnrollmentNoticeTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private data class Fixture(val vm: DevicesVM, val api: ApiClient)

    private fun makeVm(): Fixture {
        val api = mock(ApiClient::class.java)
        val resolveService = mock(ResolveService::class.java)
        val secureStorage = mock(SecureStorage::class.java)
        val appContext = mock(Context::class.java)
        // Stubbed BEFORE construction: `DevicesVM.init` collects both the moment
        // the VM is built (mirrors DevicesVMTest's / DevicesVMDestinationPlacesTest's
        // makeVm()) — unstubbed either is null and `init`'s collector NPEs.
        whenever(api.folderChangedTick).thenReturn(MutableSharedFlow())
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        whenever(api.storeChangedTick).thenReturn(MutableSharedFlow())
        val vm = DevicesVM(api, resolveService, secureStorage, appContext, OfflineShareHost(appContext))
        return Fixture(vm, api)
    }

    @Test
    fun aStandingRefusalIsRead() = runTest {
        val (vm, api) = makeVm()
        whenever(api.accountEnrollmentNotice()).thenReturn("This account's device limit has been reached.")

        vm.loadEnrollmentNotice()

        assertEquals("This account's device limit has been reached.", vm.enrollmentNotice.value)
    }

    @Test
    fun noRefusalReadsAsNull() = runTest {
        val (vm, api) = makeVm()
        whenever(api.accountEnrollmentNotice()).thenReturn(null)

        vm.loadEnrollmentNotice()

        assertNull(vm.enrollmentNotice.value)
    }

    @Test
    fun aFreshNullReadClearsAStandingNotice() = runTest {
        // "only a later read returning None does" clear it (devices.md's linux
        // reference note) — a genuine cleared-refusal read, not a repaint,
        // brings the notice down.
        val (vm, api) = makeVm()
        whenever(api.accountEnrollmentNotice()).thenReturn("This account's device limit has been reached.")
        vm.loadEnrollmentNotice()
        assertEquals("This account's device limit has been reached.", vm.enrollmentNotice.value)

        whenever(api.accountEnrollmentNotice()).thenReturn(null)
        vm.loadEnrollmentNotice()

        assertNull(vm.enrollmentNotice.value)
    }

    @Test
    fun aFailedReadKeepsTheStandingNoticeRatherThanFlickeringItOff() = runTest {
        val (vm, api) = makeVm()
        whenever(api.accountEnrollmentNotice())
            .thenReturn("This account's device limit has been reached.")
            .thenThrow(RuntimeException("transient FFI hiccup"))
        vm.loadEnrollmentNotice()
        assertEquals("This account's device limit has been reached.", vm.enrollmentNotice.value)

        vm.loadEnrollmentNotice()

        assertEquals("This account's device limit has been reached.", vm.enrollmentNotice.value)
    }
}
