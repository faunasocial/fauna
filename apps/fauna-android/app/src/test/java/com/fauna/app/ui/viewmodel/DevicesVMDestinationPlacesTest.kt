package com.fauna.app.ui.viewmodel

import android.content.Context
import com.fauna.app.R
import com.fauna.app.core.ApiClient
import com.fauna.app.p2pshare.OfflineShareHost
import com.fauna.app.core.ResolveService
import com.fauna.app.core.SecureStorage
import com.fauna.ffi.FfiFolderDestinationPlace
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
 * `DevicesVM`'s per-folder *Destination places* state
 * (`folder-destination-row`/-detach-button/-attach-select/-attach-button —
 * `docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage;
 * android is the fourth app leg, following tui/linux/web). Covers the
 * platform-glue half this VM owns: on-expand loading
 * ([DevicesVM.setFolderExpanded] / [DevicesVM.loadFolderDestinationPlaces])
 * and the attach/detach gestures, which always repaint from the mutation's
 * own re-read rather than an optimistic flip (mirrors [DevicesVMTest]'s
 * device-activity coverage and linux's `attach_folder_destination` /
 * `detach_folder_destination`).
 *
 * FFI-free (mocks [ApiClient] itself, mirroring [DevicesVMTest] /
 * [BackupsVMTest]).
 *
 * The android e2e leg (`test_folder_destination_places.py`) is
 * emulator-host-gated (android emulator) and not runnable from this
 * session — this VM test plus `FoldersContentTest`'s rendering coverage is
 * the verification ceiling here, same caveat [DevicesVMTest] already
 * carries for device activity.
 */
@ExperimentalCoroutinesApi
class DevicesVMDestinationPlacesTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private fun place(
        destinationId: String,
        label: String,
        attached: Boolean,
        folderSet: String? = null,
    ) = FfiFolderDestinationPlace(
        destinationId = destinationId,
        label = label,
        attached = attached,
        folderSet = folderSet,
    )

    private data class Fixture(
        val vm: DevicesVM,
        val api: ApiClient,
    )

    private fun makeVm(): Fixture {
        val api = mock(ApiClient::class.java)
        val resolveService = mock(ResolveService::class.java)
        val secureStorage = mock(SecureStorage::class.java)
        val appContext = mock(Context::class.java)
        whenever(appContext.getString(R.string.devices_error_folder_destination))
            .thenReturn("Failed to change the folder's destination places: {message}")
        // Stubbed BEFORE construction: `DevicesVM.init` collects both the moment
        // the VM is built (mirrors DevicesVMTest's makeVm()). `reconnectTick` is
        // unstubbed-null otherwise, and `init`'s collector NPEs on construction —
        // added alongside `folderChangedTick` when DevicesVM adopted the shared
        // classifier's reconnect sweep (`transport.md` § Which surfaces a push
        // invalidates).
        val tick = MutableSharedFlow<Unit>(replay = 0, extraBufferCapacity = 1)
        whenever(api.folderChangedTick).thenReturn(tick)
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        whenever(api.storeChangedTick).thenReturn(MutableSharedFlow())
        val vm = DevicesVM(api, resolveService, secureStorage, appContext, OfflineShareHost(appContext))
        return Fixture(vm, api)
    }

    @Test
    fun expandingASetLoadsItsDestinationPlaces() = runTest {
        val (vm, api) = makeVm()
        val offsite = place("dest-1", "Offsite", attached = true, folderSet = "__folder/aa/7")
        whenever(api.folderDestinationsList(7L)).thenReturn(listOf(offsite))

        vm.setFolderExpanded("photos", 7L, true)

        assertEquals(listOf(offsite), vm.folderDestinationPlaces.value["photos"])
    }

    @Test
    fun expandingReReadsEveryTimeNotJustTheFirst() = runTest {
        // Matches web's `loadDestinationPlaces` firing on every toggle-to-expand
        // (DevicesVMTest's device-activity twin of this test).
        val (vm, api) = makeVm()
        val before = place("dest-1", "Offsite", attached = false)
        val after = place("dest-1", "Offsite", attached = true, folderSet = "__folder/aa/7")
        whenever(api.folderDestinationsList(7L))
            .thenReturn(listOf(before))
            .thenReturn(listOf(after))

        vm.setFolderExpanded("photos", 7L, true)
        vm.setFolderExpanded("photos", 7L, false)
        vm.setFolderExpanded("photos", 7L, true)

        assertEquals(listOf(after), vm.folderDestinationPlaces.value["photos"])
    }

    @Test
    fun aFailedReadDegradesToAnEmptyListNotAPageError() = runTest {
        val (vm, api) = makeVm()
        whenever(api.folderDestinationsList(7L)).thenThrow(RuntimeException("boom"))

        vm.setFolderExpanded("photos", 7L, true)

        assertEquals(emptyList<FfiFolderDestinationPlace>(), vm.folderDestinationPlaces.value["photos"])
        // A read failure is not a page error — same posture as device activity.
        assertNull(vm.sharingError.value)
    }

    @Test
    fun attachRepaintsFromTheMutationsOwnReReadNeverAnOptimisticFlip() = runTest {
        val (vm, api) = makeVm()
        val attached = place("dest-1", "Offsite", attached = true, folderSet = "__folder/aa/7")
        whenever(api.folderDestinationAttach(7L, "dest-1")).thenReturn(listOf(attached))

        vm.attachFolderDestination("photos", 7L, "dest-1")

        assertEquals(listOf(attached), vm.folderDestinationPlaces.value["photos"])
        assertNull(vm.sharingError.value)
    }

    @Test
    fun attachFailureSetsSharingErrorAndLeavesPlacesUntouched() = runTest {
        val (vm, api) = makeVm()
        whenever(api.folderDestinationAttach(7L, "dest-1")).thenThrow(RuntimeException("nest unreachable"))

        vm.attachFolderDestination("photos", 7L, "dest-1")

        assertEquals(
            "Failed to change the folder's destination places: nest unreachable",
            vm.sharingError.value,
        )
        assertNull(vm.folderDestinationPlaces.value["photos"])
    }

    @Test
    fun detachPassesTheRowsOwnFolderSetNeverReDerivingIt() = runTest {
        val (vm, api) = makeVm()
        val attachedRow = place("dest-1", "Offsite", attached = true, folderSet = "__folder/aa/7")
        whenever(api.folderDestinationDetach(7L, "dest-1", "__folder/aa/7")).thenReturn(emptyList())

        vm.detachFolderDestination("photos", 7L, attachedRow)

        assertEquals(emptyList<FfiFolderDestinationPlace>(), vm.folderDestinationPlaces.value["photos"])
        assertNull(vm.sharingError.value)
    }

    @Test
    fun detachFailureSetsSharingError() = runTest {
        val (vm, api) = makeVm()
        val attachedRow = place("dest-1", "Offsite", attached = true, folderSet = "__folder/aa/7")
        whenever(api.folderDestinationDetach(7L, "dest-1", "__folder/aa/7"))
            .thenThrow(RuntimeException("boom"))

        vm.detachFolderDestination("photos", 7L, attachedRow)

        assertEquals(
            "Failed to change the folder's destination places: boom",
            vm.sharingError.value,
        )
    }
}
