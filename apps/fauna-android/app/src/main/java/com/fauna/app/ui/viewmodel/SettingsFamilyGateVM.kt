package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.AccountStores
import com.fauna.app.core.ApiClient
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * Gates the in-Settings **`family-tab`** entry (`navigation.gated_tabs` —
 * family-safety.md § App surface): shown when `fauna.family.status` returns
 * any relationship (guardian or supervised) **or** a pending/incoming transfer
 * — the widened gate a proposed guardian with no other family relationship
 * still needs to reach the incoming-transfer prompt. Mirrors
 * [SettingsAdminGateVM]'s shape exactly (the established gated-tab idiom).
 *
 * Starts `false` unless the persisted last-known supervision snapshot names a
 * guardian (the restore path below); otherwise only a resolved check flips it.
 */
@HiltViewModel
class SettingsFamilyGateVM @Inject constructor(
    private val api: ApiClient,
    accountStores: AccountStores,
) : ViewModel() {

    val hasFamilyRelationship = MutableStateFlow(false)

    init {
        // A restored supervision opens the gate before/without a successful
        // read (family-safety.md § Content policy, clause 2): the ward must
        // always be able to see who supervises them and what the policy is —
        // the same `family-tab` gate tui's snapshot restore drives. The
        // guardian role has nothing persisted (the snapshot records only the
        // caller's own supervision), so a guardian's gate still waits for the
        // read, as before.
        if (accountStores.supervisionSnapshot() != null) hasFamilyRelationship.value = true
        viewModelScope.launch {
            try {
                val status = api.familyStatus()
                hasFamilyRelationship.value =
                    status.wards.isNotEmpty() || status.supervisedBy != null || status.incomingTransfers.isNotEmpty()
            } catch (_: Exception) {
                // No information: keep the restored/last-known gate rather than
                // hiding a supervised ward's Family entry on a reconnect blip.
            }
        }
    }
}
