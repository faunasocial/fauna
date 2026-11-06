package com.fauna.app.ui.viewmodel

import android.util.Log
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiAccountRegistry
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * Gates the in-Settings **`admin-tab`** entry on the shared `am-i-admin` result
 * (`admin.md` § Navigation model — the single gated admin-shell entry, shown only
 * when the gate passes; on mobile it is the idiomatic in-Settings entry). Reads
 * the same `fauna.account.am_i_admin` WS-RPC the admin shell itself checks
 * (`api.checkIsAdmin()`), mirroring the other apps' shared `isAdmin` flag
 * (web's `userIsAdmin`, apple's `AppState.isAdmin`).
 *
 * **Fail-closed:** `isAdmin` starts `false` and only flips true once the check
 * resolves, so a non-admin (or an unresolved socket) never sees the admin entry —
 * the "non-admins do not see admin entries" invariant.
 *
 * This `am-i-admin = true` observation is also the **admin auto-default** hook
 * (long-term-store.md § Multi-account evolution → *Per-account re-auth*): an
 * admin identity gets its require-confirm-to-activate flag turned ON unless the
 * user ever touched its toggle — the android twin of apple's nav-gate
 * `FaunaAccounts.autoEnableRequireConfirmForActiveAdmin()`.
 */
@HiltViewModel
class SettingsAdminGateVM @Inject constructor(
    private val api: ApiClient,
    private val registry: FfiAccountRegistry,
) : ViewModel() {

    val isAdmin = MutableStateFlow(false)

    init {
        viewModelScope.launch { readGate() }
        // Reconnect re-read (`transport.md` `admin_gate`): a login read that found no
        // nest failed closed, and only a re-read shows the entry. A failed read moves
        // nothing; the auto-default fires on the false→true transition only.
        viewModelScope.launch {
            api.reconnectTick.collect { readGate() }
        }
    }

    private suspend fun readGate() {
        try {
            val (gate, becameAdmin) = applyAdminGateRead(isAdmin.value, api.readAdminGate())
            isAdmin.value = gate
            if (becameAdmin) autoEnableRequireConfirmForActiveAdmin()
        } catch (_: Exception) {
            // Fail-closed: leave the entry as it was if the check can't complete.
        }
    }

    /** Flip the ACTIVE account's require-confirm flag ON at the admin observation,
     *  unless the user ever touched its toggle (`require_confirm_user_set` pins an
     *  explicit choice — the registry enforces the OFF-sticks rule; this call is
     *  idempotent and never turns the flag off). Best-effort: a failure must never
     *  break the admin gate. */
    private fun autoEnableRequireConfirmForActiveAdmin() {
        try {
            registry.active()?.let { actorId ->
                if (registry.autoEnableRequireConfirm(actorId)) {
                    Log.i(
                        "SettingsAdminGate",
                        "[admin-auto-default] require_confirm_to_activate auto-enabled for $actorId",
                    )
                }
            }
        } catch (e: Exception) {
            Log.w("SettingsAdminGate", "[admin-auto-default] autoEnableRequireConfirm failed", e)
        }
    }
}

/**
 * The admin gate's transition rule (`transport.md` `admin_gate`; tui's `became_admin`,
 * linux's `admin_gate_passed`): a FAILED read (`null`) moves nothing, and
 * `becameAdmin` is true only on the false→true transition.
 */
internal fun applyAdminGateRead(prev: Boolean, read: Boolean?): Pair<Boolean, Boolean> =
    if (read == null) prev to false else read to (read && !prev)
