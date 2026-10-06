package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.ImportSessionState
import uniffi.fauna_client_mail_settings.ImportSourceKind
import uniffi.fauna_client_mail_settings.ImportStatus
import uniffi.fauna_client_mail_settings.ImportStep
import uniffi.fauna_client_mail_settings.ImportTlsMode
import uniffi.fauna_client_mail_settings.MailImportAction
import uniffi.fauna_client_mail_settings.MailImportMachine
import uniffi.fauna_client_mail_settings.MailImportSnapshot
import uniffi.fauna_client_mail_settings.connectActions
import uniffi.fauna_client_mail_settings.scopeNextActions
import javax.inject.Inject

/**
 * Renders the shared `MailImportMachine` (libs/fauna-client-mail-settings, over
 * UniFFI) for the per-account `mail-import` wizard — Source → Scope → Confirm →
 * Progress → Done. Per priority #2 this view-model holds **no** import logic
 * (the FSM, the provider presets, the fetch-drive loop); it owns the machine,
 * mirrors its snapshot, and dispatches actions. Leads: tui
 * (apps/fauna-tui/src/settings/mail_import.rs) and linux
 * (apps/fauna-linux/src/settings/mail_import.rs).
 *
 * ## Unlike [MailExportVM], the backend is REAL
 *
 * Every import RPC shipped 2026-07-08 and both machine seams are real — the nest
 * half wraps `MailImportClient`, the source half opens a live IMAP session over
 * TLS against the foreign server. So Connect/Start/Pause/Resume/Cancel drive a
 * genuine `import_sessions` row, and everything reaching the error banner is a
 * real nest or source answer, never an unbuilt-backend explanation.
 *
 * ## The one obligation the shared machine puts on its caller
 *
 * [MailImportMachine.runImport] is the fetch-drive loop that actually moves
 * messages, and the machine deliberately does NOT self-spawn it: each app starts
 * it in its own runtime, once per successful Start/Resume. [dispatch] does that
 * here, gated on the post-dispatch snapshot really reporting a `RUNNING`
 * session, so a rejected Start never launches a loop. Without this the Progress
 * screen would sit at zero while the session was genuinely open.
 *
 * ## Draft buffers live in the SCREEN, not here
 *
 * Every Source/Scope text field is a page-local draft the whole form commits at
 * the transition, as ONE ordered multi-action dispatch ([connect], [scopeNext])
 * built by the shared `connectActions`/`scopeNextActions`
 * (`uniffi.fauna_client_mail_settings`, over `libs/fauna-client-mail-settings/
 * src/import.rs`) — this VM only locates its own raw field values, same
 * division of labor as every other bindings-consuming leg (windows). A `Set*`
 * per keystroke would be one FFI hop per character, and on the leads it also
 * races the Connect tap badly enough to log in with a truncated password.
 */
@HiltViewModel
class MailImportVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val machine: MailImportMachine? = api.buildMailImportMachine()

    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)
    val errorMessage = MutableStateFlow<String?>(null)

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

    fun selectSourceKind(kind: ImportSourceKind) =
        dispatch(MailImportAction.SelectSourceKind(kind = kind))

    fun setTlsMode(mode: ImportTlsMode) = dispatch(MailImportAction.SetTlsMode(mode = mode))

    fun toggleMailbox(mailbox: String) = dispatch(MailImportAction.ToggleMailbox(mailbox = mailbox))

    /**
     * Step 1→2: commit whichever Source fields [kind] actually shows, then
     * Connect — one ordered dispatch, so the source can never be dialled with a
     * half-committed form. The sequence itself (which fields [kind] shows, the
     * host/port trim + parse, the port-omitted-when-unparseable rule) is the
     * shared `connect_actions` (`libs/fauna-client-mail-settings/src/import.rs`)
     * — this VM only locates its own raw field values, same division of labor
     * as every other bindings-consuming leg (windows).
     */
    fun connect(
        kind: ImportSourceKind,
        host: String,
        port: String,
        username: String,
        password: String,
    ) {
        dispatch(*connectActions(kind, host, port, username, password).toTypedArray())
    }

    /**
     * Step 2→3: commit both Scope drafts, then advance — the shared
     * `scope_next_actions` (same crate), which owns the MB→bytes conversion and
     * the unparseable/zero-buffer → the machine's own default fallback.
     * `maxSizeMb` is in MB, the field's own unit; the shared function takes the
     * raw string, not a pre-converted byte count.
     */
    fun scopeNext(dateFrom: String, maxSizeMb: String) {
        dispatch(*scopeNextActions(dateFrom, maxSizeMb).toTypedArray())
    }

    fun back() = dispatch(MailImportAction.Back)
    fun start() = dispatch(MailImportAction.Start)
    fun pause() = dispatch(MailImportAction.Pause)
    fun resume() = dispatch(MailImportAction.Resume)
    fun cancel() = dispatch(MailImportAction.Cancel)

    /**
     * Dispatch `actions` IN ORDER, awaiting each before the single re-read, so a
     * Scope→Confirm advance can never observe a half-committed scope — and
     * STOPPING at the first rejection (mirrors windows' `DispatchOneAsync`):
     * pressing on with a half-applied form after e.g. a rejected `SetHost`
     * could still fire the trailing `Connect`/`Next`. Each failure surfaces on
     * the snapshot's own `error`, which [publish] mirrors.
     */
    private fun dispatch(vararg actions: MailImportAction) {
        val m = machine ?: return
        val shouldRun = actions.any {
            it is MailImportAction.Start || it is MailImportAction.Resume
        }
        viewModelScope.launch {
            for (action in actions) {
                try {
                    m.dispatch(action)
                } catch (e: Exception) {
                    errorMessage.value = e.message
                    break
                }
            }
            val snap = publish(m)
            // The caller's obligation (class docs): spawn the fetch-drive loop
            // once, and only when the session really came back RUNNING — never
            // on a rejected Start/Resume.
            if (shouldRun && snap.sessionState == ImportSessionState.RUNNING) {
                launchImportLoop(m)
            }
        }
    }

    /**
     * Run the fetch-drive loop to completion, republishing as it goes.
     *
     * `runImport` mutates the machine's own snapshot in the background, so the
     * screen needs something to repaint it — the leads use a periodic tick for
     * exactly this. Here the loop is awaited in its own coroutine and the
     * snapshot is republished on a cadence until it stops, which is the same
     * idea with no timer to cancel.
     */
    private fun launchImportLoop(m: MailImportMachine) {
        viewModelScope.launch {
            val ticker = launch {
                while (true) {
                    kotlinx.coroutines.delay(PROGRESS_TICK_MS)
                    publish(m)
                }
            }
            try {
                m.runImport()
            } catch (e: Exception) {
                errorMessage.value = e.message
            } finally {
                ticker.cancel()
                publish(m)
            }
        }
    }

    private fun publish(m: MailImportMachine): MailImportSnapshot {
        val snap = m.snapshot()
        snapshot.value = snap
        errorMessage.value = snap.error
        return snap
    }

    private companion object {
        const val PROGRESS_TICK_MS = 400L
        const val DEFAULT_MAX_SIZE_BYTES = 52428800uL // 50 MiB — the machine's own default

        val EMPTY_SNAPSHOT = MailImportSnapshot(
            step = ImportStep.SOURCE,
            sourceKind = ImportSourceKind.GMAIL,
            host = "",
            port = 993u,
            tlsMode = ImportTlsMode.IMPLICIT,
            username = "",
            password = "",
            mailboxes = emptyList(),
            dateFrom = "",
            maxSizeBytes = DEFAULT_MAX_SIZE_BYTES,
            sessionState = null,
            importedCount = 0u,
            skippedCount = 0u,
            erroredCount = 0u,
            totalCount = 0u,
            errorLog = emptyList(),
            status = ImportStatus.IDLE,
            error = null,
        )
    }
}
