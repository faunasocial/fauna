package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.HexUtil
import com.fauna.app.core.ShellLog
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.ReadState

/**
 * Drives the admin `admin-web` page (`web-content-hosting.md` § Admin apex
 * hosting): the deployment apex-actor designation — which actor's `web` content
 * serves at `https://<domain>/`, "none" clearing it to the built-in info page.
 * The direct analogue of the per-domain catch-all mail actor ([AdminDnsVM] /
 * `admin-dns-domain-catch-all-select`): an Admin-class designation over the shared
 * `fauna.web.{get,set}_apex_actor` kinds. The actor list is every account on
 * the nest (`fauna_client_admin::users_list_all`, admin.md § 2 → *Which
 * accounts a picker offers*). Per priority #2 the VM holds no web logic — the shared
 * `web_apex_info_text` answer builds the info line (where the apex serves, or
 * why there is no address — never `https:///`), and the picker paints from the
 * read's own answer (`read_possibility`: blocked before the read lands and after
 * a failed apex read). A failed account or serving-domain read reaches
 * `error-message` rather than leaving a silently empty picker (admin.md § 7
 * Web). Non-optimistic: the picker
 * reflects only the nest-echoed designation. Linux lead:
 * apps/fauna-linux/src/settings/admin_web.rs.
 */
@HiltViewModel
class AdminWebVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val web = api.webClient()

    /** The currently-designated apex actor (hex), or null = info page. */
    val currentActorHex = MutableStateFlow<String?>(null)

    /** The pickable actors — every account on the nest (`fauna_client_admin::users_list_all`). */
    val actors = MutableStateFlow<List<ActorOption>>(emptyList())

    /** The `admin-web-apex-info` line (`web_apex_info_text`); null before the read. */
    val apexInfo = MutableStateFlow<LocalizedText?>(null)

    /** Where the apex-designation read stands — the picker's answer reads it. */
    val readState = MutableStateFlow(ReadState.PENDING)
    val errorMessage = MutableStateFlow<String?>(null)

    /** A return by Back to this page reads it again (`AdminEntryReread`). */
    val entryReread = AdminEntryReread(::hydrate)

    init {
        hydrate()
        // A read that has not answered is read again when the nest connection
        // comes back (`docs/goal/behavior/admin.md` § Errors & edge cases,
        // *Controls wait for their read*; tui's `reconnect_reread_op`): only
        // while the page's own apex read is unresolved.
        viewModelScope.rereadOnReconnect(
            api.reconnectTick,
            answered = { readState.value == ReadState.RESOLVED },
            reread = ::hydrate,
        )
    }

    /** Fetch the current designation + the actor list, then publish. */
    private fun hydrate() {
        val w = web ?: return
        viewModelScope.launch {
            // Kept: two sequential RPCs (getApexActor + adminUsersListAll),
            // not a single NestClient RPC (transport.md § Request lifecycle
            // step 3's note).
            var lastError: Exception? = null
            repeat(HYDRATE_ATTEMPTS) {
                val current = try {
                    w.getApexActor()
                } catch (e: Exception) {
                    lastError = e
                    delay(HYDRATE_RETRY_MS)
                    return@repeat
                }
                // The account list and the serving domain degrade rather than
                // blank the page — the designation still shows — but neither
                // failure is silent (`docs/goal/ui/README.md` § Offer only what
                // is possible, rule 7).
                val failures = mutableListOf<String>()
                val users = try {
                    actorOptions(api.adminUsersListAll())
                } catch (e: Exception) {
                    failures += "load accounts: ${e.message}"
                    emptyList()
                }
                // The nest's own `web_serving_domain` (admin.md § 7), never the
                // setup-status/handle domain.
                val domain = try {
                    w.servingDomain()
                } catch (e: Exception) {
                    ShellLog.w("AdminWebVM", "serving-domain fetch failed: ${e.message}")
                    failures += "load web serving domain: ${e.message}"
                    ""
                }
                currentActorHex.value = current?.let { HexUtil.bytesToHex(it) }
                actors.value = users
                apexInfo.value = runCatching { com.fauna.ffi.webApexInfoText(domain) }.getOrNull()
                readState.value = ReadState.RESOLVED
                if (failures.isNotEmpty()) errorMessage.value = failures.joinToString("; ")
                return@launch
            }
            readState.value = ReadState.FAILED
            errorMessage.value = lastError?.message
        }
    }

    /** Designate (`actorIdHex` non-null) or clear ("none") the apex actor, then
     *  re-render from the nest's echoed designation. */
    fun setApex(actorIdHex: String?) {
        val w = web ?: return
        viewModelScope.launch {
            try {
                val confirmed = w.setApexActor(actorIdHex?.let { HexUtil.hexToBytes(it) })
                currentActorHex.value = confirmed?.let { HexUtil.bytesToHex(it) }
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    private companion object {
        const val HYDRATE_ATTEMPTS = 10
        const val HYDRATE_RETRY_MS = 500L
    }
}
