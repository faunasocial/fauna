package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ConsentHandoff
import com.fauna.app.core.SessionAccount
import com.fauna.app.core.ShellLog
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.fauna_client_connected_apps.ConnectedAppsMachine
import uniffi.fauna_client_connected_apps.ConnectedAppsObserver
import uniffi.fauna_client_connected_apps.ConnectedAppsSnapshot
import javax.inject.Inject

/**
 * Drives the "Connected apps" Settings sub-page ([ConnectedAppsScreen];
 * `docs/goal/ui/connected-apps.md`; ui.yaml page `connected-apps`). A thin proxy
 * over the shared `ConnectedAppsMachine` (`libs/fauna-client-connected-apps`,
 * over UniFFI): the roster's composition, the scope words, the class badge key,
 * *lasts-until* and **which verb revokes a row** are all the machine's — this VM
 * never picks a revoke verb, a row's `key` is opaque here. It owns the machine,
 * mirrors its snapshot, forwards each gesture, and holds only the page-local
 * drafts a gesture needs (the code field, the armed revoke, the revealed mail
 * secrets). Reference painter: tui
 * `apps/fauna-tui/src/settings/connected_apps.rs`.
 *
 * **The mail app passwords are rows of this roster.** The machine takes the
 * session's Mail & Calendar machine and reads, revokes and reveals them through
 * it; that machine's `credential_management_reachable` gate is only known after
 * its own `hydrate()`, so [visit] hydrates a fresh one before it builds the
 * connected-apps machine over it.
 *
 * **A visit starts unread** (`connected-apps.md` § Errors & edge cases): rows
 * are nest state read on every open, so [visit] builds a fresh machine, which
 * paints neither rows nor the empty state until its own read has returned —
 * never the previous visit's list while the fresh one is in flight — and takes
 * every revealed secret off the screen.
 *
 * **The same-device handoff lands here.** A `fauna://consent` route staged in
 * [ConsentHandoff] is opened by the visit once its machine has read the roster
 * (`openStagedHandoff`), so the route's card appears in the requests tray.
 */
@HiltViewModel
class ConnectedAppsVM @Inject constructor(
    private val api: ApiClient,
    private val sessionAccount: SessionAccount,
) : ViewModel() {

    /** The last snapshot painted; null until the machine is built. */
    val snapshot = MutableStateFlow<ConnectedAppsSnapshot?>(null)

    /** The *Connect an app* field's draft. */
    val code = MutableStateFlow("")

    /** The row key whose inline revoke confirm is open. */
    val revokeArmed = MutableStateFlow<String?>(null)

    /**
     * The mail app-password secrets currently shown, by row key. Empty by
     * default: the secret is never in the snapshot, so a row shows one only
     * after the user asks and the on-demand read resolves. Keyed by row key,
     * never by index, so a roster that re-orders under a fresh snapshot cannot
     * show one password's secret against another row.
     */
    val revealed = MutableStateFlow<Map<String, String>>(emptyMap())

    private var machine: ConnectedAppsMachine? = null

    /** Bumped by every [visit]; a visit's late work checks it is still current. */
    private var visitGeneration = 0

    private val observer = object : ConnectedAppsObserver {
        override fun onChanged() {
            machine?.let { snapshot.value = it.snapshot() }
        }
    }

    /**
     * Start a visit: drop the page-local drafts and the last visit's snapshot,
     * build a fresh machine over a freshly hydrated mail machine, and read the
     * roster. The nest socket may not be up yet, so the build retries like the
     * sibling Settings pages' hydrate loops.
     */
    fun visit() {
        code.value = ""
        revokeArmed.value = null
        revealed.value = emptyMap()
        snapshot.value = null
        machine = null
        val generation = ++visitGeneration
        viewModelScope.launch {
            repeat(HYDRATE_ATTEMPTS) {
                val mail = api.buildMailSettingsMachine()
                if (mail != null) {
                    // One attempt: the machine records a real failure in its own
                    // snapshot, and a roster without mail rows beats no roster.
                    runCatching { mail.hydrate() }
                        .onFailure { ShellLog.w("ConnectedAppsVM", "mail hydrate failed: ${it.message}") }
                }
                val m = api.buildConnectedAppsMachine(observer, mail)
                if (m != null) {
                    if (generation != visitGeneration) return@launch
                    machine = m
                    snapshot.value = m.snapshot()
                    m.refresh()
                    snapshot.value = m.snapshot()
                    openStagedHandoff(m, generation)
                    return@launch
                }
                delay(HYDRATE_RETRY_MS)
            }
        }
    }

    /**
     * Open the staged `fauna://consent/<request_uri>` route, if any, through the
     * shared machine ([ConsentHandoff]; `android.md` § Navigation → *In-app
     * routes*), and clear it only after the open has returned — a waiter (the e2e
     * `open_route` command) then knows the card is painted. A miss is the
     * machine's own `connected_apps.error_handoff_expired` message. A newer
     * visit owns the page once it starts: the request then stays staged, so ITS
     * open (a fresh machine) reveals the card and clears it.
     */
    private suspend fun openStagedHandoff(m: ConnectedAppsMachine, generation: Int) {
        val requestUri = ConsentHandoff.shared.pending.value ?: return
        m.openHandoff(requestUri)
        if (generation != visitGeneration) return
        snapshot.value = m.snapshot()
        ConsentHandoff.shared.finishOpen(requestUri)
    }

    /** The bare handle `resolve_mua_username` substitutes into a mail row's login. */
    fun resolveUsername(muaUsername: String): String =
        runCatching {
            com.fauna.ffi.resolveMuaUsername(muaUsername, (sessionAccount.handle ?: "").substringBefore("@"))
        }.getOrDefault(muaUsername)

    fun setCode(value: String) {
        code.value = value
    }

    fun submitCode() {
        val typed = code.value.trim()
        if (typed.isEmpty()) return
        run { it.submitCode(typed) }
        code.value = ""
    }

    fun resolveRequest(consentIdHex: String, approved: Boolean) =
        run { it.resolveRequest(consentIdHex, approved) }

    fun blockRequest(consentIdHex: String) = run { it.blockRequest(consentIdHex) }

    fun unblock(clientId: String) = run { it.unblock(clientId) }

    /** The account's external-apps switch (`atproto-external-apps-enable`): the
     *  machine writes and re-reads, and the toggle paints the re-read. */
    fun setExternalAppsEnabled(enabled: Boolean) = run { it.setExternalAppsEnabled(enabled) }

    fun armRevoke(key: String) {
        revokeArmed.value = key
    }

    fun cancelRevoke() {
        revokeArmed.value = null
    }

    fun confirmRevoke(key: String) {
        revokeArmed.value = null
        run { it.revoke(key) }
        revealed.update { it - key }
    }

    /** Show the row's mail secret, or hide it if shown. */
    fun toggleReveal(key: String) {
        if (revealed.value.containsKey(key)) {
            revealed.update { it - key }
            return
        }
        viewModelScope.launch {
            val secret = readSecret(key) ?: return@launch
            revealed.update { it + (key to secret) }
        }
    }

    /**
     * Read a mail secret on demand for the clipboard, without painting it:
     * copy is independent of the reveal toggle, so the secret reaches the
     * clipboard without being drawn on a screen someone else can read.
     */
    suspend fun readSecret(key: String): String? =
        runCatching { machine?.revealSecret(key) }
            .onFailure { ShellLog.w("ConnectedAppsVM", "reveal secret failed: ${it.message}") }
            .getOrNull()

    override fun onCleared() {
        revealed.value = emptyMap()
        super.onCleared()
    }

    private fun run(gesture: suspend (ConnectedAppsMachine) -> Unit) {
        val m = machine ?: return
        viewModelScope.launch {
            gesture(m)
            snapshot.value = m.snapshot()
        }
    }

    private companion object {
        const val HYDRATE_ATTEMPTS = 10
        const val HYDRATE_RETRY_MS = 500L
    }
}
