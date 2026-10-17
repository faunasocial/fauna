package com.fauna.app.ui.viewmodel

import android.content.Context
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ResolveService
import com.fauna.app.core.SecureStorage
import com.fauna.app.p2pshare.OfflineShareHost
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
        whenever(api.folderChangedTick).thenReturn(MutableSharedFlow())
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        whenever(api.storeChangedTick).thenReturn(MutableSharedFlow())
        val log = mutableListOf<String>()
        val toggle = FakeOnDemandToggle(stronger, failSetShown, log)
        val vm = DevicesVM(
            api, mock(ResolveService::class.java), secureStorage, appContext, OfflineShareHost(appContext), toggle,
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
    fun anUnwritableStoreRevertsTheFlipAndReplansNothing() = runTest {
        val f = makeVm(failSetShown = true)
        val own = folder("photos", 1L)
        f.vm.loadOnDemand(listOf(own))

        f.vm.setFolderOnDemand(own, false)

        assertTrue(f.vm.onDemand.value.getValue(OnDemandRowKey.of(own)).shown)
        assertFalse(f.log.contains("replan"))
        verify(f.api, never()).ensureFolderPlace(anyString(), anyString())
    }
}
