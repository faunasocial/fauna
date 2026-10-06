package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
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
                    machine = m
                    snapshot.value = m.snapshot()
                    m.refresh()
                    snapshot.value = m.snapshot()
                    return@launch
                }
                delay(HYDRATE_RETRY_MS)
            }
        }
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
