package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.inOrder
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.times
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import uniffi.fauna_client_mail_settings.ExportSessionState
import uniffi.fauna_client_mail_settings.ExportStatus
import uniffi.fauna_client_mail_settings.ExportStep
import uniffi.fauna_client_mail_settings.MailExportAction
import uniffi.fauna_client_mail_settings.MailExportMachine
import uniffi.fauna_client_mail_settings.MailExportSnapshot
import uniffi.fauna_mail.ExportFormat

/**
 * The three things key custody obliges [MailExportVM] to do (`mail-export.md`
 * § Implementation status today, the android paragraph) — each one a mechanism,
 * so each one is pinned here with no device: the drive loop is spawned after a
 * Start/Resume that really landed `RUNNING` and never after a rejected one; the
 * actor handle is refreshed before Start / Resume / Download and an empty one
 * is never pushed; and a Download announces the saved archive only when the
 * machine really saved one. FFI-free (mocks [ApiClient] and the machine,
 * mirroring [MailEnableGlueVMTest]).
 */
@ExperimentalCoroutinesApi
class MailExportVMTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private fun snap(
        step: ExportStep = ExportStep.CONFIRM,
        sessionState: ExportSessionState? = null,
        savedArchivePath: String = "",
        error: String? = null,
    ) = MailExportSnapshot(
        step = step,
        format = ExportFormat.MBOX,
        mailboxes = emptyList(),
        dateFrom = "",
        dateTo = "",
        stripHeaders = false,
        sessionState = sessionState,
        exportedCount = 0u,
        skippedCount = 0u,
        erroredCount = 0u,
        totalCount = 0u,
        mailboxProgress = emptyList(),
        errorLog = emptyList(),
        blobBytes = null,
        downloadUrl = "",
        savedArchivePath = savedArchivePath,
        status = ExportStatus.IDLE,
        error = error,
    )

    private data class Fixture(val vm: MailExportVM, val api: ApiClient, val machine: MailExportMachine)

    private fun makeVm(handle: String = "alice", snapshot: MailExportSnapshot = snap()): Fixture {
        val api = mock(ApiClient::class.java)
        val machine = mock(MailExportMachine::class.java)
        whenever(machine.snapshot()).thenReturn(snapshot)
        whenever(api.buildMailExportMachine()).thenReturn(machine)
        whenever(api.mailExportActorHandle()).thenReturn(handle)
        return Fixture(MailExportVM(api), api, machine)
    }

    @Test
    fun startThatLandsRunning_setsTheHandleThenSpawnsTheDriveLoopOnce() = runTest {
        val (vm, _, machine) = makeVm(
            snapshot = snap(step = ExportStep.PROGRESS, sessionState = ExportSessionState.RUNNING),
        )

        vm.start()

        val order = inOrder(machine)
        order.verify(machine).setActorHandle("alice")
        order.verify(machine).dispatch(MailExportAction.Start)
        order.verify(machine).runExport()
        verify(machine, times(1)).runExport()
    }

    @Test
    fun resumeThatLandsRunning_spawnsTheDriveLoop() = runTest {
        val (vm, _, machine) = makeVm(
            snapshot = snap(step = ExportStep.PROGRESS, sessionState = ExportSessionState.RUNNING),
        )

        vm.resume()

        verify(machine).setActorHandle("alice")
        verify(machine).runExport()
    }

    @Test
    fun rejectedStart_neverSpawnsTheDriveLoop() = runTest {
        // A refused Start leaves no session: the post-dispatch snapshot is not RUNNING.
        val (vm, _, machine) = makeVm(snapshot = snap(error = "refused"))

        vm.start()

        verify(machine).dispatch(MailExportAction.Start)
        verify(machine, never()).runExport()
        assertEquals("refused", vm.errorMessage.value)
    }

    @Test
    fun pause_neverSpawnsTheDriveLoop_evenWhileTheSessionReadsRunning() = runTest {
        val (vm, _, machine) = makeVm(
            snapshot = snap(step = ExportStep.PROGRESS, sessionState = ExportSessionState.RUNNING),
        )

        vm.pause()

        verify(machine, never()).runExport()
        verify(machine, never()).setActorHandle("alice")
    }

    @Test
    fun anEmptyHandleIsNeverPushed() = runTest {
        val (vm, _, machine) = makeVm(
            handle = "",
            snapshot = snap(step = ExportStep.PROGRESS, sessionState = ExportSessionState.RUNNING),
        )

        vm.start()

        verify(machine, never()).setActorHandle("")
        verify(machine).runExport()
    }

    @Test
    fun download_setsTheHandle_dispatchesDownload_andAnnouncesTheSavedArchive() = runTest {
        val (vm, _, machine) = makeVm(
            snapshot = snap(step = ExportStep.DONE, savedArchivePath = "/cache/fauna/a.zip.zst"),
        )
        val announced = mutableListOf<String>()
        val collector = launch(UnconfinedTestDispatcher(testScheduler)) {
            vm.savedArchives.collect { announced += it }
        }

        vm.download()

        val order = inOrder(machine)
        order.verify(machine).setActorHandle("alice")
        order.verify(machine).dispatch(MailExportAction.Download)
        assertEquals(listOf("/cache/fauna/a.zip.zst"), announced)
        verify(machine, never()).runExport()
        collector.cancel()
    }

    @Test
    fun refusedDownload_announcesNothing() = runTest {
        // A refused archive leaves no file; an earlier download's path may still
        // sit on the snapshot, and it must not be handed on as this press's result.
        val (vm, _, machine) = makeVm(
            snapshot = snap(
                step = ExportStep.DONE,
                savedArchivePath = "/cache/fauna/earlier.zip.zst",
                error = "the archive was not terminated",
            ),
        )
        val announced = mutableListOf<String>()
        val collector = launch(UnconfinedTestDispatcher(testScheduler)) {
            vm.savedArchives.collect { announced += it }
        }

        vm.download()

        verify(machine).dispatch(MailExportAction.Download)
        assertEquals(emptyList<String>(), announced)
        collector.cancel()
    }
}
