package com.fauna.app.ui.viewmodel

import android.content.Context
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ResolveService
import com.fauna.app.core.SecureStorage
import com.fauna.app.p2pshare.OfflineShareHost
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.mockito.ArgumentMatchers.anyString
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.times
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import uniffi.fauna_devices_machine.FolderSummary

/**
 * `DevicesVM`'s `folder-on-demand-toggle` ("Show in Files") half —
 * `on-demand-files-android.md` § Android SAF DocumentsProvider binding (set-level,
 * default ON, remembered), *Auto-appear default-ON* (ON on an owner row is the
 * enrol gesture: the shared `ensure_place`, before the re-plan) and § Shared
 * sets on a capability host, decision 3 (a member's row is hide-only: it
 * writes no place). FFI-free: [ApiClient] is mocked and the presence surface
 * is [FakeOnDemandToggle].
 *
 * The android arms of `test_folder_on_demand_toggle.py` are wired but have no
 * venue to run in yet (no android e2e run has been recorded — testing.md
 * § Default app and nest mode); this test, `FoldersContentTest`'s rendering
 * cases and the shared Rust store/plan tests are the verification today.
 */
@ExperimentalCoroutinesApi
class DevicesVMOnDemandTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val deviceId = "de".repeat(32)

    private fun folder(name: String, id: Long, role: String? = null) = FolderSummary(
        id = id,
        name = name,
        retentionPolicy = null,
        cachedSnapshotCount = 0L,
        cachedTotalBytes = 0L,
        cachedLastSnapshotAt = null,
        includePaths = null,
        includePathsUnreadable = false,
        excludePathsUnreadable = false,
        excludePaths = null,
        mlsGroupId = if (role == "member") "cd".repeat(16) else null,
        role = role,
        access = null,
        ownerHandle = null,
        ownerDisplay = "",
        seatForked = false,
        webdavEnabled = false,
        conflictPolicy = null,
        webPaywallTier = null,
        homeNestUrl = null,
        nestSnapshots = null,
        nestSnapshotQuietSecs = null,
        versionRetentionMaxVersions = 0u,
        versionRetentionMaxAgeDays = 0u,
        audience = "private",
        websiteEnabled = false,
        residency = "",
    )

    private class Fixture(val vm: DevicesVM, val api: ApiClient, val toggle: FakeOnDemandToggle, val log: MutableList<String>)

    private fun makeVm(stronger: Set<String> = emptySet(), failSetShown: Boolean = false): Fixture {
        val api = mock(ApiClient::class.java)
        val secureStorage = mock(SecureStorage::class.java)
        whenever(secureStorage.resolveDeviceId()).thenReturn(deviceId)
        val appContext = mock(Context::class.java)
        whenever(appContext.getString(com.fauna.app.R.string.devices_error_use_other_version))
            .thenReturn("Failed to use the other version: {message}")
        whenever(api.folderChangedTick).thenReturn(MutableSharedFlow())
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        whenever(api.devicesStaleTick).thenReturn(MutableSharedFlow())
        whenever(api.storeChangedTick).thenReturn(MutableSharedFlow())
        val log = mutableListOf<String>()
        val toggle = FakeOnDemandToggle(stronger, failSetShown, log)
        val vm = DevicesVM(
            api, mock(ResolveService::class.java), secureStorage, appContext, OfflineShareHost(appContext), toggle, NoSharePlaneReadings,
        )
        return Fixture(vm, api, toggle, log)
    }

    @Test
    fun everyRowReadsDefaultOnAndAWatchedDirectoryMarksTheStrongerPresence() = runTest {
        val f = makeVm(stronger = setOf("local:2"))
        val own = folder("photos", 1L)
        val fed = folder("camera", 2L)
        val shared = folder("docs", 3L, role = "member")

        f.vm.loadOnDemand(listOf(own, fed, shared))

        val rows = f.vm.onDemand.value
        assertEquals(3, rows.size)
        assertTrue(rows.getValue(OnDemandRowKey.of(own)).shown)
        assertFalse(rows.getValue(OnDemandRowKey.of(own)).stronger)
        assertTrue(rows.getValue(OnDemandRowKey.of(fed)).stronger)
        assertTrue(rows.getValue(OnDemandRowKey.of(shared)).shown)
    }

    @Test
    fun aFlipIsRememberedAcrossReEntry() = runTest {
        val f = makeVm()
        val own = folder("photos", 1L)
        f.vm.loadOnDemand(listOf(own))

        f.vm.setFolderOnDemand(own, false)
        // Re-entry: a fresh read from the store, not the view state.
        f.vm.loadOnDemand(listOf(own))

        assertFalse(f.vm.onDemand.value.getValue(OnDemandRowKey.of(own)).shown)
    }

    @Test
    fun ownerRowOnEnrolsThisDeviceBeforeTheReplan() = runTest {
        val f = makeVm()
        val own = folder("photos", 1L)
        whenever(f.api.ensureFolderPlace("photos", deviceId)).thenAnswer { f.log += "ensure photos"; true }
        f.vm.loadOnDemand(listOf(own))

        f.vm.setFolderOnDemand(own, false)
        f.vm.setFolderOnDemand(own, true)

        val id = "local%3A1@" + "aa".repeat(32)
        assertEquals(
            listOf("setShown $id false", "replan", "setShown $id true", "ensure photos", "replan"),
            f.log,
        )
        verify(f.api, times(1)).ensureFolderPlace("photos", deviceId)
    }

    @Test
    fun anEnrolRepaintsTheRowsPlacesAndAFoundPlaceReadsNothing() = runTest {
        val f = makeVm()
        val own = folder("photos", 1L)
        val seat = com.fauna.ffi.FfiFolderMember(deviceId, "this phone", true, true, true)
        whenever(f.api.folderPlaces("photos")).thenReturn(listOf(seat))
        whenever(f.api.ensureFolderPlace("photos", deviceId)).thenReturn(true, false)
        f.vm.loadOnDemand(listOf(own))

        // The enrol wrote this device's place: the roster is re-read at once.
        f.vm.setFolderOnDemand(own, false)
        f.vm.setFolderOnDemand(own, true)
        verify(f.api, times(1)).folderPlaces("photos")
        assertEquals(listOf(seat), f.vm.folderPlaces.value["photos"])

        // A second ON finds the place and writes nothing: no re-read.
        f.vm.setFolderOnDemand(own, false)
        f.vm.setFolderOnDemand(own, true)
        verify(f.api, times(1)).folderPlaces("photos")
    }

    @Test
    fun memberRowIsHideOnlyAndNeverWritesAPlace() = runTest {
        val f = makeVm()
        val shared = folder("docs", 3L, role = "member")
        f.vm.loadOnDemand(listOf(shared))

        f.vm.setFolderOnDemand(shared, false)
        f.vm.setFolderOnDemand(shared, true)

        verify(f.api, never()).ensureFolderPlace(anyString(), anyString())
        assertEquals(2, f.log.count { it == "replan" })
        assertTrue(f.vm.onDemand.value.getValue(OnDemandRowKey.of(shared)).shown)
    }

    @Test
    fun theProvidersParkReachesTheRowOnLoadAndAfterEachReconcile() = runTest {
        // `file-sync.md` § Multi-writer shared sets → *Revocation on a capability
        // host*: the fold runs on the folder-list refresh and on the presence
        // reconcile — the host writes its mark at its own edge, which no push
        // announces, so the page watches rather than waits to be told.
        val f = makeVm()
        val shared = folder("docs", 3L, role = "member")
        f.vm.loadOnDemand(listOf(shared))
        assertFalse(f.vm.onDemand.value.getValue(OnDemandRowKey.of(shared)).parked)

        f.toggle.parked += "local:3"
        f.toggle.reconcile()
        assertTrue(f.vm.onDemand.value.getValue(OnDemandRowKey.of(shared)).parked)

        // A re-grant clears the host's mark; the next reconcile clears the row.
        f.toggle.parked -= "local:3"
        f.toggle.reconcile()
        assertFalse(f.vm.onDemand.value.getValue(OnDemandRowKey.of(shared)).parked)
    }

    @Test
    fun aHideFlipKeepsTheRowsParkReading() = runTest {
        val f = makeVm()
        val shared = folder("docs", 3L, role = "member")
        f.toggle.parked += "local:3"
        f.vm.loadOnDemand(listOf(shared))

        f.vm.setFolderOnDemand(shared, false)

        assertTrue(f.vm.onDemand.value.getValue(OnDemandRowKey.of(shared)).parked)
    }

    @Test
    fun anUnwritableStoreRevertsTheFlipAndReplansNothing() = runTest {
        val f = makeVm(failSetShown = true)
        val own = folder("photos", 1L)
        f.vm.loadOnDemand(listOf(own))

        f.vm.setFolderOnDemand(own, false)

        assertTrue(f.vm.onDemand.value.getValue(OnDemandRowKey.of(own)).shown)
        assertFalse(f.log.contains("replan"))
        verify(f.api, never()).ensureFolderPlace(anyString(), anyString())
    }

    // ── The group-scope row (`group-set.md` § Build design — the group set,
    // decision 11): a phone's seat is its on-demand replica, keyed by the
    // scope's `group:` ref, hide-only like a member's row. ──

    private val groupRef = "group:" + "07".repeat(32)

    @Test
    fun aGroupScopeRowReadsDefaultOnUnderItsGroupRef() = runTest {
        val f = makeVm()

        f.vm.loadGroupOnDemand(listOf(groupRef))

        val row = f.vm.groupOnDemand.value.getValue(groupRef)
        assertTrue(row.shown)
        assertFalse(row.stronger)
        assertEquals(f.toggle.identifyRef(groupRef).scopedId, row.scopedId)
    }

    @Test
    fun aGroupScopeHideIsHideOnlyAndRemembered() = runTest {
        val f = makeVm()
        f.vm.loadGroupOnDemand(listOf(groupRef))
        val scoped = f.vm.groupOnDemand.value.getValue(groupRef).scopedId

        f.vm.setGroupOnDemand(groupRef, false)
        f.vm.loadGroupOnDemand(listOf(groupRef))

        assertFalse(f.vm.groupOnDemand.value.getValue(groupRef).shown)
        assertEquals(listOf("setShown $scoped false", "replan"), f.log)
        // The membership is the seat: no place is read or written.
        verify(f.api, never()).ensureFolderPlace(anyString(), anyString())
    }

    @Test
    fun aReconcileReReadsTheGroupScopeRows() = runTest {
        val f = makeVm()
        f.vm.loadGroupOnDemand(listOf(groupRef))
        assertFalse(f.vm.groupOnDemand.value.getValue(groupRef).parked)

        f.toggle.parked += groupRef
        f.toggle.reconcile()

        assertTrue(f.vm.groupOnDemand.value.getValue(groupRef).parked)
    }

    // ── A group set's review rows (`group-set.md` § Build design — the group
    // set, decision 9): read in-process on a poll, restored on the set's host ──

    private fun retained(id: Long, fileInfo: String) = uniffi.fauna_devices_machine.ConflictSummary(
        id = id,
        folder = "Shared folder 07",
        deviceId = "cd".repeat(32),
        path = "notes.txt",
        conflictType = "content",
        details = null,
        createdAt = 0L,
        candidates = emptyList(),
        resolvedAt = 1L,
        resolution = "auto",
        winningManifestHash = null,
        hasOtherVersion = true,
        fileInfo = fileInfo,
    )

    @Test
    fun theReviewPollReadsEachSetUnderItsLabelAndKeepsRowsOverAFailedRead() = runTest {
        val f = makeVm()
        f.toggle.reviewRows[groupRef] = listOf(retained(9L, "Shared folder 07: notes.txt"))
        backgroundScope.launch { f.vm.pollGroupReviewRows(listOf(groupRef to "Shared folder 07")) }
        runCurrent()

        assertEquals(listOf(GroupReviewRow(groupRef, retained(9L, "Shared folder 07: notes.txt"))), f.vm.groupReviewRows.value)
        assertTrue("review $groupRef Shared folder 07" in f.log)

        // A busy DB never loses a review row.
        f.toggle.reviewReadFails += groupRef
        advanceTimeBy(GROUP_REVIEW_POLL_MS + 1)
        assertEquals(1, f.vm.groupReviewRows.value.size)
    }

    @Test
    fun aRestoreIsKeyedBySetAndRowAndTheUsedRowGoes() = runTest {
        val f = makeVm()
        f.toggle.reviewRows[groupRef] = listOf(retained(9L, "Shared folder 07: notes.txt"))
        backgroundScope.launch { f.vm.pollGroupReviewRows(listOf(groupRef to "Shared folder 07")) }
        runCurrent()

        f.vm.useRetainedVersion(groupRef, 9L)

        assertTrue("restore $groupRef 9" in f.log)
        assertTrue(f.vm.groupReviewRows.value.isEmpty())
        assertEquals(null, f.vm.sharingError.value)
    }

    @Test
    fun aRefusedRestoreLandsOnTheErrorSlotAndTheRowStays() = runTest {
        val f = makeVm()
        f.toggle.reviewRows[groupRef] = listOf(retained(9L, "Shared folder 07: notes.txt"))
        f.toggle.restoreRefusal = "the kept version is gone"
        backgroundScope.launch { f.vm.pollGroupReviewRows(listOf(groupRef to "Shared folder 07")) }
        runCurrent()

        f.vm.useRetainedVersion(groupRef, 9L)

        assertEquals("Failed to use the other version: the kept version is gone", f.vm.sharingError.value)
        assertEquals(1, f.vm.groupReviewRows.value.size)
    }
}
