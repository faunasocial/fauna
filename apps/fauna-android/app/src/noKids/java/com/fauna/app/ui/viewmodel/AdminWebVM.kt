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

/**
 * Drives the admin `admin-web` page (`web-content-hosting.md` § Admin apex
 * hosting): the deployment apex-actor designation — which actor's `web` content
 * serves at `https://<domain>/`, "none" clearing it to the built-in info page.
 * The direct analogue of the per-domain catch-all mail actor ([AdminDnsVM] /
 * `admin-dns-domain-catch-all-select`): an Admin-class designation over the shared
 * `fauna.web.{get,set}_apex_actor` kinds. The actor list is every account on
 * the nest (`fauna_client_admin::users_list_all`, admin.md § 2 → *Which
 * accounts a picker offers*). Per priority #2 the VM holds no web logic — the shared
 * `web_apex_url` projection builds the info URL. Non-optimistic: the picker
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

    /** The `https://<domain>/` URL the apex serves at (the info line). */
    val apexUrl = MutableStateFlow("")
    val errorMessage = MutableStateFlow<String?>(null)

    init { hydrate() }

    /** Fetch the current designation + the actor list, then publish. */
    private fun hydrate() {
        val w = web ?: return
        viewModelScope.launch {
            // Kept: two sequential RPCs (getApexActor + adminUsersListAll),
            // not a single NestClient RPC (transport.md § Request lifecycle
            // step 3's note).
            repeat(HYDRATE_ATTEMPTS) {
                try {
                    val current = w.getApexActor()
                    val users = actorOptions(api.adminUsersListAll())
                    val domain = runCatching { api.nestSetupStatus().domain }
                        .onFailure { ShellLog.w("AdminWebVM", "setup status fetch failed: ${it.message}") }
                        .getOrDefault("")
                    currentActorHex.value = current?.let { HexUtil.bytesToHex(it) }
                    actors.value = users
                    apexUrl.value = runCatching { com.fauna.ffi.webApexUrl(domain) }.getOrDefault("")
                    return@launch
                } catch (_: Exception) {
                    delay(HYDRATE_RETRY_MS)
                }
            }
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
