package com.fauna.app.ui.viewmodel

import android.content.Context
import com.fauna.app.core.ApiClient
import com.fauna.app.p2pshare.OfflineShareHost
import com.fauna.app.core.ResolveService
import com.fauna.app.core.SecureStorage
import com.fauna.ffi.FfiFolderDevice
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.times
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever

/**
 * `DevicesVM`'s per-set device-activity state (`folder-device-activity-item`/
 * -label/-count — file-sync.md § Implementation status today; the fourth of six
 * per-app legs, following web/tui/linux). Covers the platform-glue half this VM
 * owns: on-expand loading ([DevicesVM.setFolderExpanded] /
 * [DevicesVM.loadFolderDeviceActivity]) and the push-driven LIVE UPDATE on
 * `fauna.sync.changed` ([DevicesVM.init]'s `folderChangedTick` collector) — the
 * entire point of the feature per the doc: a client with no push arm wired to
 * this section never converges at any timeout.
 *
 * FFI-free (mocks [ApiClient] itself, mirroring [BackupsVMTest]), so this runs
 * without host-JNA. Two coroutine-test pieces, each solving a different
 * problem: `Dispatchers.setMain(UnconfinedTestDispatcher())` mirrors
 * [AppLaunchVMTest] — `DevicesVM.init` collects `api.folderChangedTick` on
 * `viewModelScope` (`Dispatchers.Main.immediate`) the moment the VM is
 * constructed, which a plain JVM test has no Main loop for; Unconfined makes
 * every tick's re-fetch resolve synchronously so assertions need no
 * `advanceUntilIdle()`. Each test body is `runTest { … }` (mirrors
 * [BackupsVMTest]) purely because [ApiClient.folderDevices] is `suspend` —
 * Kotlin requires a coroutine to stub/verify a suspend call at all, regardless
 * of which dispatcher actually runs it.
 *
 * The android e2e leg (`test_folder_device_activity_reflects_recorded_changes`,
 * `tests/e2e-unified/tests/test_folders.py`) is emulator-host-gated (android
 * emulator) and not runnable from this session — this VM test plus
 * `FoldersContentTest`'s rendering coverage is the verification ceiling here.
 */
@ExperimentalCoroutinesApi
class DevicesVMTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private fun device(label: String, changeCount: Long, deviceId: String = "aa".repeat(32)) =
        FfiFolderDevice(
            deviceId = deviceId,
            label = label,
            lastChangeAt = 0L,
            changeCount = changeCount,
        )

    private data class Fixture(
        val vm: DevicesVM,
        val api: ApiClient,
        val tick: MutableSharedFlow<Unit>,
        val reconnectTick: MutableSharedFlow<Unit>,
        val storeChangedTick: MutableSharedFlow<Unit>,
    )

    private fun makeVm(): Fixture {
        val api = mock(ApiClient::class.java)
        val resolveService = mock(ResolveService::class.java)
        val secureStorage = mock(SecureStorage::class.java)
        val appContext = mock(Context::class.java)
        // Stubbed BEFORE construction: `DevicesVM.init` collects both the moment
        // the VM is built (mirrors AppLaunchVMTest's `observer.snapshot` stub).
        // `reconnectTick` joined `folderChangedTick` here when DevicesVM adopted
        // the shared classifier's reconnect sweep (`transport.md` § Which
        // surfaces a push invalidates) — unstubbed it is null, and `init`'s
        // collector NPEs on construction.
        val tick = MutableSharedFlow<Unit>(replay = 0, extraBufferCapacity = 1)
        val reconnectTick = MutableSharedFlow<Unit>(replay = 0, extraBufferCapacity = 1)
        whenever(api.folderChangedTick).thenReturn(tick)
        whenever(api.reconnectTick).thenReturn(reconnectTick)
        // The store-change notice (`ApiClient.storeChangedTick`), collected in
        // `init` beside the two above.
        val storeChangedTick = MutableSharedFlow<Unit>(replay = 0, extraBufferCapacity = 1)
        whenever(api.storeChangedTick).thenReturn(storeChangedTick)
        val vm = DevicesVM(api, resolveService, secureStorage, appContext, OfflineShareHost(appContext))
        return Fixture(vm, api, tick, reconnectTick, storeChangedTick)
    }

    @Test
    fun expandingASetLoadsItsDeviceActivity() = runTest {
        val (vm, api, _) = makeVm()
        whenever(api.folderDevices("photos")).thenReturn(listOf(device("laptop", 3)))

        vm.setFolderExpanded("photos", 1L, true)

        assertEquals(listOf(device("laptop", 3)), vm.folderDeviceActivity.value["photos"])
    }

    @Test
    fun expandingReReadsEveryTimeNotJustTheFirst() = runTest {
        // Matches web's `loadDeviceActivity` firing on every toggle-to-expand, not
        // just the first — a stale count on re-expand is exactly what this feature
        // exists to prevent (distinct from `loadFolderActors`'s
        // undefined-check-gated eager load).
        val (vm, api, _) = makeVm()
        whenever(api.folderDevices("photos"))
            .thenReturn(listOf(device("laptop", 1)))
            .thenReturn(listOf(device("laptop", 2)))

        vm.setFolderExpanded("photos", 1L, true)
        vm.setFolderExpanded("photos", 1L, false)
        vm.setFolderExpanded("photos", 1L, true)

        assertEquals(listOf(device("laptop", 2)), vm.folderDeviceActivity.value["photos"])
        verify(api, times(2)).folderDevices("photos")
    }

    @Test
    fun aFailedReadDegradesToAnEmptyListNotAPageError() = runTest {
        val (vm, api, _) = makeVm()
        whenever(api.folderDevices("photos")).thenThrow(RuntimeException("boom"))

        vm.setFolderExpanded("photos", 1L, true)

        assertEquals(emptyList<FfiFolderDevice>(), vm.folderDeviceActivity.value["photos"])
    }

    // ── The live-update half: fauna.sync.changed re-fetches EXPANDED rows only ──

    @Test
    fun aPushTickReReadsTheExpandedRow() = runTest {
        val (vm, api, tick) = makeVm()
        whenever(api.folderDevices("photos"))
            .thenReturn(listOf(device("laptop", 1)))
            .thenReturn(listOf(device("laptop", 2)))
        vm.setFolderExpanded("photos", 1L, true)
        assertEquals(1L, vm.folderDeviceActivity.value["photos"]?.single()?.changeCount)

        // The real production trigger: `fauna.sync.changed` landing on the ONE
        // authenticated socket (ApiClient.kt's `FfiPushEvent.SyncChanged` arm) —
        // simulated here by emitting straight onto the tick this VM collects.
        tick.tryEmit(Unit)

        assertEquals(2L, vm.folderDeviceActivity.value["photos"]?.single()?.changeCount)
    }

    @Test
    fun aPushTickDoesNotReadASetThatWasNeverExpanded() = runTest {
        // Never touching (or blanking) a row the push has nothing to do with —
        // file-sync.md's tui/linux bullets both call this out explicitly.
        val (vm, api, tick) = makeVm()

        tick.tryEmit(Unit)

        verify(api, never()).folderDevices("photos")
        assertTrue(vm.folderDeviceActivity.value["photos"].isNullOrEmpty())
    }

    // ── The reconnect backstop: on_reconnect() stales media too ──────────────

    @Test
    fun aReconnectTickAlsoReReadsTheExpandedRow() = runTest {
        // This VM had NO reconnect arm before adopting the shared classifier
        // (`transport.md` § Which surfaces a push invalidates) — a push dropped
        // across a socket gap left device activity stale forever, the same
        // class of gap the seam's own audit found on web's Events page. Proves
        // the fix: `reconnectTick` alone (no `folderChangedTick` push) re-reads
        // an expanded row exactly like the push tick does.
        val (vm, api, _, reconnectTick) = makeVm()
        whenever(api.folderDevices("photos"))
            .thenReturn(listOf(device("laptop", 1)))
            .thenReturn(listOf(device("laptop", 2)))
        vm.setFolderExpanded("photos", 1L, true)
        assertEquals(1L, vm.folderDeviceActivity.value["photos"]?.single()?.changeCount)

        reconnectTick.tryEmit(Unit)

        assertEquals(2L, vm.folderDeviceActivity.value["photos"]?.single()?.changeCount)
    }

    // ── The store-change notice: store reads only, never a ceremony drive ────

    @Test
    fun aStoreChangeTickReReadsTheStoreBackedHalvesAndNothingElse() = runTest {
        // account-runtime.md § Multi-instance concurrency → *A runtime's own
        // pump is a source of the notice too*: the notice re-reads what the
        // store backs — here the enrollment notice — and must not become a
        // custody ceremony drive (the facet's load drives before it folds), nor
        // re-read an expanded row's device activity, which is nest state.
        val (vm, api, _, _, storeChangedTick) = makeVm()
        whenever(api.folderDevices("photos")).thenReturn(listOf(device("laptop", 1)))
        vm.setFolderExpanded("photos", 1L, true)

        storeChangedTick.tryEmit(Unit)

        verify(api, times(1)).accountEnrollmentNotice()
        verify(api, never()).custodyDrive()
        verify(api, never()).custodyFacetLoad()
        verify(api, times(1)).folderDevices("photos")
    }

    @Test
    fun collapsingStopsFurtherPushReadsForThatRow() = runTest {
        // A collapsed row's device-activity read is never wastefully re-fired —
        // android's twin of linux's `folder_row_is_expanded` gate.
        val (vm, api, tick) = makeVm()
        whenever(api.folderDevices("photos")).thenReturn(listOf(device("laptop", 1)))
        vm.setFolderExpanded("photos", 1L, true)
        vm.setFolderExpanded("photos", 1L, false)

        tick.tryEmit(Unit)
        tick.tryEmit(Unit)

        // Exactly the one read from the original expand — none from either tick.
        verify(api, times(1)).folderDevices("photos")
    }

    @Test
    fun aPushTickReReadsOnlyTheExpandedSetAmongSeveral() = runTest {
        val (vm, api, tick) = makeVm()
        whenever(api.folderDevices("photos")).thenReturn(listOf(device("laptop", 1)))
        whenever(api.folderDevices("docs")).thenReturn(listOf(device("phone", 5)))
        // "docs" is loaded once (e.g. a prior expand) but is NOT currently
        // expanded — collapsing it must stop it riding the tick, same as
        // collapsingStopsFurtherPushReadsForThatRow above.
        vm.setFolderExpanded("docs", 2L, true)
        vm.setFolderExpanded("docs", 2L, false)
        vm.setFolderExpanded("photos", 1L, true)

        tick.tryEmit(Unit)

        verify(api, times(1)).folderDevices("docs")
        verify(api, times(2)).folderDevices("photos")
    }
}
