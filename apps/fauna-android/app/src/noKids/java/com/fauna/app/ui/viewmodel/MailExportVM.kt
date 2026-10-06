package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.launch
import uniffi.fauna_mail.ExportFormat
import uniffi.fauna_client_mail_settings.ExportSessionState
import uniffi.fauna_client_mail_settings.ExportStatus
import uniffi.fauna_client_mail_settings.ExportStep
import uniffi.fauna_client_mail_settings.MailExportAction
import uniffi.fauna_client_mail_settings.MailExportMachine
import uniffi.fauna_client_mail_settings.MailExportSnapshot
import javax.inject.Inject

/**
 * Renders the shared `MailExportMachine` (libs/fauna-client-mail-settings, over
 * UniFFI) for the per-account `mail-export` wizard — Format → Scope → Confirm →
 * Progress → Done. Per priority #2 this view-model holds **no** export logic
 * (the job model, format catalog, sealed-blob delivery, § Download flow); it
 * owns the machine, mirrors its snapshot, and dispatches actions. Leads: tui
 * (apps/fauna-tui/src/settings/mail_export.rs), linux
 * (apps/fauna-linux/src/settings/mail_export.rs) and the FaunaKit `MailExportVM`.
 *
 * ## This view-model drives the export
 *
 * The machine is built with key custody ([ApiClient.buildMailExportMachine]),
 * so this class does the three things custody obliges:
 *
 * 1. **It spawns [MailExportMachine.runExport]** after a Start or Resume whose
 *    *post-dispatch* snapshot reads `RUNNING` — never after a rejected one.
 *    Custody and the spawn land together: custody alone would open a session
 *    nothing drives, a Progress screen stuck at zero holding one of the user's
 *    concurrency slots. ([MailImportVM]'s `runImport` spawn is the twin.)
 * 2. **It repaints Progress on a tick** while the loop mutates the machine's
 *    snapshot. The tick only re-reads the snapshot.
 * 3. **Download runs § Download flow** (`MailExportAction.Download`) into
 *    [ApiClient.mailExportSaveDir], and a press that really saved an archive
 *    announces its path on [savedArchives] — the screen hands that file to the
 *    share sheet, and the Done summary names where it went.
 *
 * The actor handle names the archive's root directory and the saved file; it
 * may arrive after the machine is built or change with the user, so it is read
 * at the gesture (`setActorHandle` before Start / Resume / Download, an empty
 * one never pushed), not only at construction.
 */
@HiltViewModel
class MailExportVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val machine: MailExportMachine? = api.buildMailExportMachine()

    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)
    val errorMessage = MutableStateFlow<String?>(null)

    private val _savedArchives = MutableSharedFlow<String>(extraBufferCapacity = 1)

    /** The path of each archive a Download press really saved — one event per
     *  successful press, never for a refused one (which leaves no file). */
    val savedArchives: SharedFlow<String> = _savedArchives

    /** Guards against a second drive loop when Resume is pressed while one is
     *  still running (linux's `ticking` cell). Main-thread confined. */
    private var driving = false

    init {
        hydrate()
    }

    private fun hydrate() {
        val m = machine ?: return
        viewModelScope.launch {
            try {
                m.hydrate()
            } catch (_: Exception) {
                // One attempt: the transport already waits out a socket that has not landed yet
                // (NestClient::request_inner), and the machine records any real failure
                // in its own snapshot, which the publish below surfaces.
            }
            publish(m)
        }
    }

    fun selectFormat(format: ExportFormat) = dispatch(MailExportAction.SelectFormat(format = format))
    fun toggleMailbox(mailbox: String) = dispatch(MailExportAction.ToggleMailbox(mailbox = mailbox))
    fun setDateFrom(value: String) = dispatch(MailExportAction.SetDateFrom(value = value))
    fun setDateTo(value: String) = dispatch(MailExportAction.SetDateTo(value = value))
    fun setStripHeaders(on: Boolean) = dispatch(MailExportAction.SetStripHeaders(on = on))
    fun next() = dispatch(MailExportAction.Next)
    fun back() = dispatch(MailExportAction.Back)
    fun start() = dispatch(MailExportAction.Start)
    fun pause() = dispatch(MailExportAction.Pause)
    fun resume() = dispatch(MailExportAction.Resume)
    fun cancel() = dispatch(MailExportAction.Cancel)
    fun discard() = dispatch(MailExportAction.Discard)
    fun download() = dispatch(MailExportAction.Download)

    private fun dispatch(action: MailExportAction) {
        val m = machine ?: return
        val wantsRun = action is MailExportAction.Start || action is MailExportAction.Resume
        val isDownload = action is MailExportAction.Download
        viewModelScope.launch {
            if (wantsRun || isDownload) {
                val handle = api.mailExportActorHandle()
                if (handle.isNotEmpty()) m.setActorHandle(handle)
            }
            var rejected = false
            try {
                m.dispatch(action)
            } catch (e: Exception) {
                errorMessage.value = e.message
                rejected = true
            }
            val snap = publish(m)
            // The caller's obligation (class docs): spawn the drive loop once,
            // and only when the session really came back RUNNING — never on a
            // rejected Start/Resume.
            if (wantsRun && snap.sessionState == ExportSessionState.RUNNING) {
                launchExportLoop(m)
            }
            // `savedArchivePath` outlives the press that set it, so it alone
            // does not say THIS press saved anything — the absence of a
            // refusal does.
            if (isDownload && !rejected && snap.error == null && snap.savedArchivePath.isNotEmpty()) {
                _savedArchives.emit(snap.savedArchivePath)
            }
        }
    }

    /**
     * Run the drive loop to completion, republishing as it goes — the same
     * shape as [MailImportVM]'s `launchImportLoop`: `runExport` mutates the
     * machine's own snapshot in the background, so the loop is awaited in its
     * own coroutine and the snapshot is republished on a cadence until it stops.
     */
    private fun launchExportLoop(m: MailExportMachine) {
        if (driving) return
        driving = true
        viewModelScope.launch {
            val ticker = launch {
                while (true) {
                    delay(PROGRESS_TICK_MS)
                    publish(m)
                }
            }
            try {
                m.runExport()
            } catch (e: Exception) {
                errorMessage.value = e.message
            } finally {
                ticker.cancel()
                driving = false
                publish(m)
            }
        }
    }

    private fun publish(m: MailExportMachine): MailExportSnapshot {
        val snap = m.snapshot()
        snapshot.value = snap
        errorMessage.value = snap.error
        return snap
    }

    private companion object {
        /** Same cadence as the import twin ([MailImportVM]). */
        const val PROGRESS_TICK_MS = 400L

        val EMPTY_SNAPSHOT = MailExportSnapshot(
            step = ExportStep.FORMAT,
            format = ExportFormat.MBOX,
            mailboxes = emptyList(),
            dateFrom = "",
            dateTo = "",
            stripHeaders = false,
            sessionState = null,
            exportedCount = 0u,
            skippedCount = 0u,
            erroredCount = 0u,
            totalCount = 0u,
            mailboxProgress = emptyList(),
            errorLog = emptyList(),
            blobBytes = null,
            downloadUrl = "",
            savedArchivePath = "",
            status = ExportStatus.IDLE,
            error = null,
        )
    }
}
