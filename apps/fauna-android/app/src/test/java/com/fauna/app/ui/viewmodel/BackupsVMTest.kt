package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.app.core.SecureStorage
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.mockito.ArgumentMatchers.anyInt
import org.mockito.ArgumentMatchers.anyString
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever

/**
 * `BackupsVM.downloadSnapshotFile` (`snapshot-file-download-button[i]`,
 * backup-restore.md § 3): the FFI-consume glue between the Compose screen and
 * `ApiClient.downloadSnapshotFileBytes`. FFI-free (mocks `ApiClient` itself),
 * so it runs without host-JNA.
 *
 * The list/detail state itself is the shared `BackupsMachine`'s and is covered
 * by its own 41 tier_1 tests in `libs/fauna-backups-machine`; what is left for
 * this VM is the platform glue above.
 */
class BackupsVMTest {

    private fun makeVm(): Triple<BackupsVM, ApiClient, SecureStorage> {
        val api = mock(ApiClient::class.java)
        val storage = mock(SecureStorage::class.java)
        val vm = BackupsVM(api, storage)
        return Triple(vm, api, storage)
    }

    @Test
    fun noDeviceId_returnsNullWithoutCallingApi() = runTest {
        val (vm, api, storage) = makeVm()
        whenever(storage.deviceId).thenReturn(null)

        val result = vm.downloadSnapshotFile(1, "docs/a.txt")

        assertNull(result)
        verify(api, never()).downloadSnapshotFileBytes(anyString(), anyInt(), anyString())
    }

    @Test
    fun success_returnsBytesAndLeavesErrorClear() = runTest {
        val (vm, api, storage) = makeVm()
        whenever(storage.deviceId).thenReturn("aabbcc")
        val bytes = byteArrayOf(1, 2, 3)
        whenever(api.downloadSnapshotFileBytes("aabbcc", 7, "docs/a.txt")).thenReturn(bytes)

        val result = vm.downloadSnapshotFile(7, "docs/a.txt")

        assertEquals(bytes, result)
        assertNull(vm.downloadError.value)
    }

    @Test
    fun apiFailure_returnsNullAndSetsDownloadError() = runTest {
        val (vm, api, storage) = makeVm()
        whenever(storage.deviceId).thenReturn("aabbcc")
        whenever(api.downloadSnapshotFileBytes(anyString(), anyInt(), anyString()))
            .thenThrow(RuntimeException("boom"))

        val result = vm.downloadSnapshotFile(7, "docs/a.txt")

        assertNull(result)
        assertEquals("boom", vm.downloadError.value)
    }

    /**
     * A gesture dispatched before the nest connection exists must be a no-op,
     * not a crash: `ensureMachine()` returns null until `buildBackupsMachine`
     * can hand one back, and every gesture guards on it. Without this the page
     * would take the app down on any tap made while the socket is still coming
     * up — the state the screen renders on its very first frame.
     */
    @Test
    fun gesturesBeforeTheMachineExists_areNoOps() = runTest {
        // The `ApiClient` mock answers `buildBackupsMachine` with null by
        // default, which IS the not-yet-connected case — no stubbing needed.
        val (vm, _, _) = makeVm()

        vm.selectFolder("photos")
        vm.createSnapshot()
        vm.deleteSnapshot(1)
        vm.undeleteSnapshot(1)
        vm.openSnapshot(1)
        vm.closeSnapshotDetail()
        vm.prunePreview()
        vm.pruneExecute()
        vm.cancelPrunePreview()
        vm.checkIntegrity()
        vm.deleteSnapshotImmediate(1, "1", "ack")

        assertNull(vm.snapshot.value)
    }

    /**
     * The immediate-delete confirm predicate is the MACHINE's, and with no
     * machine it must answer **false** — never a hopeful `true` that would arm
     * an irreversible owner-only delete against nothing. This is the fail-safe
     * direction of Architectural rule 4's "never re-derive the predicate".
     */
    @Test
    fun immediateDeleteEnabled_isFalseWithoutAMachine() {
        val (vm, _, _) = makeVm()
        assertEquals(false, vm.immediateDeleteEnabled("7", "7", "ack"))
    }

    /** The modal's open/close flag is the VM's own state; the RENDER path (the
     *  observer) is what closes it on a landed delete, so opening and cancelling
     *  must be independent of the machine existing. */
    @Test
    fun immediateDeleteTarget_opensAndCancels() {
        val (vm, _, _) = makeVm()
        assertNull(vm.immediateDeleteTargetId.value)
        vm.openImmediateDelete(42)
        assertEquals(42L, vm.immediateDeleteTargetId.value)
        vm.cancelImmediateDelete()
        assertNull(vm.immediateDeleteTargetId.value)
    }
}
