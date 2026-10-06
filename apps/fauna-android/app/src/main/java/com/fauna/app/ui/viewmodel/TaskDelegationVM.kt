package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.R
import com.fauna.app.core.ApiClient
import com.fauna.app.core.SecureStorage
import com.fauna.ffi.FfiHeavyTaskCapability
import com.fauna.ffi.FfiPinOption
import com.fauna.ffi.FfiTaskDelegationRow
import com.fauna.ffi.FfiTaskDelegationView
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_devices_machine.DevicesObserver
import javax.inject.Inject

/**
 * Drives the "Task delegation" Settings sub-page ([TaskDelegationScreen]) — the
 * per-task-kind runner + assignment surface (`docs/goal/behavior/participants.md`
 * § Task delegation). A thin proxy over the shared `FfiTaskDelegationView`: the
 * shared layer composes the user's pins (`fauna.state.delegation`) with the live
 * advisory lease (`fauna.delegation.observe`) into rows and orchestrates the pin
 * write through the plane's read-modify-write; this VM only holds the
 * latest rows as a `StateFlow` and re-`load()`s after every reload / pin write.
 * No delegation policy here (priority #1/#2) — the lift is render-only. Mirrors
 * apple's `TaskDelegationVM`; reference render: linux
 * `apps/fauna-linux/src/settings/task_delegation.rs`.
 *
 * **android passes [FfiHeavyTaskCapability.VIEWER_ONLY]** — a phone is always
 * battery-mobile and never runs a heavy task kind (participants.md § The
 * assignment picker), so this client's picker legitimately offers only
 * "Automatic" plus a pin made elsewhere, rendered so it stays escapable. Never
 * add a "This device" option here; `setAssignment` would reject it anyway
 * (`resolve_pin` → `NotPinnable`).
 */
@HiltViewModel
class TaskDelegationVM @Inject constructor(
    @ApplicationContext private val context: Context,
    private val api: ApiClient,
    private val secureStorage: SecureStorage,
) : ViewModel() {

    init {
        // The store-change notice: a pin set on another device re-reads the
        // open page ([ApiClient.storeChangedTick]).
        viewModelScope.launch { api.storeChangedTick.collect { load() } }
    }

    /** Latest rows, in `LIVE_TASK_KINDS` order. Empty until the first load. */
    val rows = MutableStateFlow<List<FfiTaskDelegationRow>>(emptyList())

    /** `device_id` hex → display label, resolved from the shared Devices roster
     *  — a runner / pinned participant's name is inherently client-side state
     *  the shared view-model deliberately does not bake in (`RunnerStatus.Other`
     *  / `PinOption.Other` carry only the ref). */
    val deviceLabels = MutableStateFlow<Map<String, String>>(emptyMap())

    /** Page-level error surface (`error-message`). */
    val errorMessage = MutableStateFlow<String?>(null)

    private var view: FfiTaskDelegationView? = null

    /**
     * Build the surface if not already built, then load the rows. Call again on
     * every page visit ([TaskDelegationScreen]'s `LaunchedEffect(Unit)`) — the
     * runner column is **live** advisory-lease state (a peer claims the lease; a
     * desktop unplugs and yields), so a build-once hydrate would go stale.
     * Kept: `v.load()` composes a config fetch+unseal with a second
     * `fauna.delegation.observe` RPC for the live lease, not a single
     * NestClient RPC (transport.md § Request lifecycle step 3's note).
     */
    fun load() {
        val deviceIdHex = secureStorage.deviceId
        if (deviceIdHex == null) {
            errorMessage.value = context.getString(R.string.task_delegation_error_device_id)
            return
        }
        viewModelScope.launch {
            repeat(HYDRATE_ATTEMPTS) {
                val v = view ?: api.buildTaskDelegationView(deviceIdHex, FfiHeavyTaskCapability.VIEWER_ONLY)
                if (v != null) {
                    view = v
                    try {
                        rows.value = v.load()
                        errorMessage.value = null
                        loadDeviceLabels()
                        return@launch
                    } catch (e: Exception) {
                        errorMessage.value = e.message
                    }
                }
                delay(HYDRATE_RETRY_MS)
            }
        }
    }

    /**
     * Persist a pin change for `taskKind`, then reload so the runner column +
     * picker reflect authoritative state. On failure the error shows and the
     * rows are left as-is (the write is a CAS read-modify-write — a failure
     * changed nothing).
     */
    fun setAssignment(taskKind: String, option: FfiPinOption) {
        val v = view ?: return
        viewModelScope.launch {
            try {
                v.setAssignment(taskKind, option)
            } catch (e: Exception) {
                errorMessage.value = e.message
                return@launch
            }
            load()
        }
    }

    /** The `device_id` hex → label map from the shared Devices roster, for
     *  naming a runner / pinned device. Any read failure yields an empty map —
     *  the caller then falls back to a short-hex abbreviation rather than
     *  showing nothing. Mirrors linux's `device_labels` / apple's
     *  `loadDeviceLabels`. */
    private suspend fun loadDeviceLabels() {
        val observer = object : DevicesObserver {
            override fun onChanged() {}
        }
        val machine = api.buildDevicesMachine(observer)
        if (machine == null) {
            deviceLabels.value = emptyMap()
            return
        }
        try {
            machine.refresh()
            deviceLabels.value = machine.snapshot().devices.associate { it.deviceId to it.label }
        } catch (_: Exception) {
            deviceLabels.value = emptyMap()
        }
    }

    private companion object {
        const val HYDRATE_ATTEMPTS = 10
        const val HYDRATE_RETRY_MS = 500L
    }
}
