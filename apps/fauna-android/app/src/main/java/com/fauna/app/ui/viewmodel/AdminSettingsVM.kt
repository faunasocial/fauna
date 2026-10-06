package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiAdminMembershipTier
import com.fauna.ffi.FfiAdminTier
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * View-model for the `admin-settings` page — nav-labelled **"Tiers"** after the
 * per-page-services redesign (admin.md § 3 Settings / § Admin IA redesign): tier
 * *definitions* with in-place cap editing. The factory-reset danger zone moved
 * to the new `admin-nest` page ([AdminNestVM]) — the read-only storage-mode
 * indicator that also lived there is retired outright (storage-modes.md: every
 * nest is sealed from first boot, so there's no mode left to indicate). Drives
 * the shared `fauna.admin.tiers.*` WS-RPC kinds through [ApiClient]; never the
 * deleted `/admin/api` HTTP twins.
 *
 * Mirrors the Linux reference (`apps/fauna-linux/src/views/admin.rs`
 * `build_settings_page` / `update_tiers`) and the Windows `AdminSettingsViewModel`.
 */
@HiltViewModel
class AdminSettingsVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    /** Tier definitions backing the indexed `admin-settings-tier-item` rows. */
    private val _tiers = MutableStateFlow<List<FfiAdminTier>>(emptyList())
    val tiers: StateFlow<List<FfiAdminTier>> = _tiers

    /**
     * The admin's own subscription tier names (`fauna.subscriptions.tiers.list`)
     * — the row set for the `admin-settings-membership-item` link editor
     * (monetization.md § Pillar 4). Empty is the normal out-of-the-box state
     * (no subscription tiers minted yet), not an error.
     */
    private val _ownMembershipTierNames = MutableStateFlow<List<String>>(emptyList())
    val ownMembershipTierNames: StateFlow<List<String>> = _ownMembershipTierNames

    /** Which of the rows above already carry a designation, and what it links to. */
    private val _membershipTiers = MutableStateFlow<List<FfiAdminMembershipTier>>(emptyList())
    val membershipTiers: StateFlow<List<FfiAdminMembershipTier>> = _membershipTiers

    /** Page-scoped action error surface. */
    private val _actionError = MutableStateFlow<String?>(null)
    val actionError: StateFlow<String?> = _actionError

    fun refresh() {
        viewModelScope.launch {
            _actionError.value = null
            try {
                _tiers.value = api.adminTiersList()
                _ownMembershipTierNames.value = api.subscriptionTiersList().map { it.name }
                _membershipTiers.value = api.adminMembershipTiersList()
            } catch (e: Exception) {
                _actionError.value = e.message
            }
        }
    }

    /**
     * Persist a tier's caps via `fauna.admin.tiers.update`, then refetch so the
     * row re-renders from persisted state (`name` identifies the row). The caps
     * are raw i64 already parsed by the row via the shared `parse_cap` validator
     * (an empty/unparseable field falls back to the persisted value, never zeroes).
     */
    fun saveTier(
        name: String,
        maxInboxBytes: Long,
        maxStorageBytes: Long,
        maxDevices: Long,
        maxBlobSize: Long,
        maxFeeds: Long,
    ) {
        viewModelScope.launch {
            _actionError.value = null
            try {
                api.adminTiersUpdate(name, maxInboxBytes, maxStorageBytes, maxDevices, maxBlobSize, maxFeeds)
                _tiers.value = api.adminTiersList()
            } catch (e: Exception) {
                _actionError.value = e.message
            }
        }
    }

    /**
     * Designate/re-point a membership tier via `fauna.admin.membership_tiers.set`
     * (an upsert), then refetch so the row re-renders from persisted state
     * (mirrors [saveTier]). [lapseTier] always rides an explicit selection —
     * the row never relies on the wire's omit-means-default.
     */
    fun saveMembershipTier(tierName: String, adminTier: String, lapseTier: String) {
        viewModelScope.launch {
            _actionError.value = null
            try {
                api.adminMembershipTiersSet(tierName, adminTier, lapseTier)
                _membershipTiers.value = api.adminMembershipTiersList()
            } catch (e: Exception) {
                _actionError.value = e.message
            }
        }
    }

    /**
     * Drop a membership designation via `fauna.admin.membership_tiers.clear`;
     * the subscription tier itself survives, reverting to undesignated.
     */
    fun clearMembershipTier(tierName: String) {
        viewModelScope.launch {
            _actionError.value = null
            try {
                api.adminMembershipTiersClear(tierName)
                _membershipTiers.value = api.adminMembershipTiersList()
            } catch (e: Exception) {
                _actionError.value = e.message
            }
        }
    }
}
