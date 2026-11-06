package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.feed.FeedManagerHost
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.fauna_archive_import_machine.ArchiveImportAction
import uniffi.fauna_archive_import_machine.ArchiveImportMachine
import uniffi.fauna_archive_import_machine.ArchiveImportSnapshot
import uniffi.fauna_archive_import_machine.ArchiveSourceKind
import uniffi.fauna_archive_import_machine.AudienceMode
import uniffi.fauna_archive_import_machine.RunState
import java.lang.ref.WeakReference
import javax.inject.Inject

/**
 * Renders the shared `ArchiveImportMachine` (libs/fauna-archive-import-machine,
 * over UniFFI) for the user-facing `archive-import` wizard — Source → Archive →
 * Scope → Confirm → Progress → Done (`docs/goal/behavior/archive-import.md`
 * § The wizard and its machine). Per priority #2 this view-model holds **no**
 * import logic: the machine owns the FSM, the parser, the folder writes and the
 * authoring; this VM builds it, mirrors its snapshot and dispatches its
 * actions. Leads: tui (`apps/fauna-tui/src/settings/archive_import.rs`), the
 * FaunaKit `ArchiveImportVM` and windows' `ArchiveImportViewModel`; the
 * [MailImportVM] shape.
 *
 * ## The one obligation the shared machine puts on its caller
 *
 * The machine never spawns its own drive loop: after a `Start`/`Resume` whose
 * post-dispatch snapshot really reads `RUNNING` — or a hydrate that finds a run
 * still running — [driveImport] runs `runImport` once, republishing on a tick
 * while it runs.
 *
 * ## Drafts live in the SCREEN
 *
 * The archive path and the two dates are page-local drafts committed as one
 * ordered dispatch at Open archive / Next ([openArchive], [scopeNext]) — never a
 * `Set*` per keystroke (tui's `open_actions` / `scope_next_actions`).
 */
@HiltViewModel
class ArchiveImportVM @Inject constructor(
    private val api: ApiClient,
    private val secureStorage: SecureStorage,
    private val feedHost: FeedManagerHost,
) : ViewModel() {

    /** `null` until the machine is built and read once. */
    val snapshot = MutableStateFlow<ArchiveImportSnapshot?>(null)
    val errorMessage = MutableStateFlow<String?>(null)

    /** The glue refused to build the machine (no device id, or a bearer-only
     *  session with no identity key to sign the imported posts with): the
     *  page states it and offers no wizard that could not commit. */
    val unavailable = MutableStateFlow(false)

    private var machine: ArchiveImportMachine? = null

    /** Guards against a second drive loop while one runs. */
    private var driving = false

    init {
        viewModelScope.launch { build() }
    }

    private suspend fun build() {
        val deviceIdHex = secureStorage.resolveDeviceId()
        if (deviceIdHex == null) {
            unavailable.value = true
            return
        }
        val built = try {
            withContext(Dispatchers.IO) { api.buildArchiveImportMachine(deviceIdHex) }
        } catch (e: Exception) {
            unavailable.value = true
            errorMessage.value = e.message
            return
        } ?: run {
            unavailable.value = true
            return
        }
        machine = built
        current = WeakReference(this)
        publish(built)
        try {
            built.hydrate()
        } catch (_: Exception) {
            // The refusal is on the snapshot's own `error`, published below.
        }
        val snap = publish(built)
        if (snap.runState == RunState.RUNNING) driveImport(built)
    }

    fun selectSource(kind: ArchiveSourceKind) = dispatch(ArchiveImportAction.SelectSource(kind = kind))

    /** Commit the path draft, then open it — one ordered dispatch. */
    fun openArchive(path: String) =
        dispatch(ArchiveImportAction.SetArchivePath(value = path.trim()), ArchiveImportAction.OpenArchive)

    fun next() = dispatch(ArchiveImportAction.Next)
    fun back() = dispatch(ArchiveImportAction.Back)

    fun toggleCategory(token: String, selected: Boolean) =
        dispatch(ArchiveImportAction.ToggleCategory(token = token, selected = selected))

    fun setAudienceMode(mode: AudienceMode) = dispatch(ArchiveImportAction.SetAudienceMode(mode = mode))

    /** Scope → Confirm: both date drafts committed, then the advance — the
     *  machine refuses a malformed date or a reversed range here. */
    fun scopeNext(dateFrom: String, dateTo: String) = dispatch(
        ArchiveImportAction.SetDateFrom(value = dateFrom.trim()),
        ArchiveImportAction.SetDateTo(value = dateTo.trim()),
        ArchiveImportAction.Next,
    )

    fun start() = dispatch(ArchiveImportAction.Start)
    fun pause() = dispatch(ArchiveImportAction.Pause)
    fun resume() = dispatch(ArchiveImportAction.Resume)
    fun cancel() = dispatch(ArchiveImportAction.Cancel)
    fun applyProfile() = dispatch(ArchiveImportAction.ApplyArchiveProfile)

    /**
     * View imported — put the shared feed manager on the imported-posts
     * selection for the archive's platform (`feed.md` § State & data shape →
     * *The imported-posts selection*; the token is the platform's lowercase
     * protocol name, `facebook` | `instagram`), then [onSelected] navigates to
     * the Feed. With no snapshot to name a platform it opens the feed as it
     * stands.
     */
    fun viewImported(onSelected: () -> Unit) {
        val token = snapshot.value?.sourceKind?.name?.lowercase()
        val m = feedHost.manager()
        if (token == null || m == null) {
            onSelected()
            return
        }
        viewModelScope.launch {
            m.selectImported(token)
            onSelected()
        }
    }

    /**
     * Dispatch [actions] IN ORDER, awaiting each before the one re-read and
     * stopping at the first refusal (each refusal is on the snapshot's own
     * `error`); only an accepted Start/Resume arms the drive loop.
     */
    private fun dispatch(vararg actions: ArchiveImportAction) {
        val m = machine ?: return
        val wantsRun = actions.any { it is ArchiveImportAction.Start || it is ArchiveImportAction.Resume }
        viewModelScope.launch {
            for (action in actions) {
                try {
                    m.dispatch(action)
                } catch (_: Exception) {
                    break
                }
            }
            val snap = publish(m)
            if (wantsRun && snap.runState == RunState.RUNNING) driveImport(m)
        }
    }

    private fun driveImport(m: ArchiveImportMachine) {
        if (driving) return
        driving = true
        // Phase two (`archive-import.md` § Linking and merging): hand the
        // login's archive links to the machine before its run is spawned, so a
        // run that completes sets the Done step's link counts and runs the
        // announce round itself — nothing is glued onto the run here (tui's
        // `LinkRound`). No links (no conversations session) → a plain run.
        api.currentArchiveLinks()?.let { m.attachLinks(it) }
        viewModelScope.launch {
            val ticker = launch {
                while (true) {
                    delay(PROGRESS_TICK_MS)
                    publish(m)
                }
            }
            try {
                m.runImport()
            } catch (e: Exception) {
                errorMessage.value = e.message
            } finally {
                ticker.cancel()
                driving = false
                publish(m)
            }
        }
    }

    private fun publish(m: ArchiveImportMachine): ArchiveImportSnapshot {
        val snap = m.snapshot()
        snapshot.value = snap
        errorMessage.value = snap.error
        return snap
    }

    /** The machine the mounted page holds, for the debug test agent's
     *  `archive_import_pause_after` / `archive_import_hold_after` arms. */
    internal fun machineOrNull(): ArchiveImportMachine? = machine

    override fun onCleared() {
        if (current?.get() === this) current = null
        super.onCleared()
    }

    companion object {
        /** The Progress step's repaint cadence — the mail-import page's tick. */
        private const val PROGRESS_TICK_MS = 400L

        /** The VM of the page on the back stack now; `null` while no page holds
         *  a machine (the agent arms refuse then — convention 11). */
        @Volatile
        var current: WeakReference<ArchiveImportVM>? = null
            private set
    }
}
