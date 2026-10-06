package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiSubscribeReply
import com.fauna.ffi.FfiTierItem
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * The profile Tiers-tab **OTHER** (subscriber-browse) state — subscriptions
 * Slice B item 7 (`monetization.md` § Pillar 1; `profile.md` § Layout & flow).
 * When viewing another actor's profile, [load] reads their offered tiers
 * (`SubscriptionsClient::offers_list` — the OTHER analogue of the bearer-keyed
 * `tiers_list`) + the viewer's status (`status_get`); [subscribe] subscribes to
 * a per-row tier. Pure glue over the shared FFI (priority #2/#3); lifts the linux
 * lead (`apps/fauna-linux/src/views/profile/offers.rs`). Observer-free: a manual
 * re-read after an Approved subscribe; errors ride [errorMessage] to the page's
 * global banner.
 */
@HiltViewModel
class ProfileOffersVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val _offers = MutableStateFlow<List<FfiTierItem>>(emptyList())
    val offers: StateFlow<List<FfiTierItem>> = _offers.asStateFlow()

    /** The viewer's held tier name for this author (from `status_get`), or `null`. */
    private val _statusTier = MutableStateFlow<String?>(null)
    val statusTier: StateFlow<String?> = _statusTier.asStateFlow()

    /** Tiers the viewer just subscribed to that resolved `Queued` (transient "pending"). */
    private val _pendingTiers = MutableStateFlow<Set<String>>(emptySet())
    val pendingTiers: StateFlow<Set<String>> = _pendingTiers.asStateFlow()

    private val _working = MutableStateFlow(false)
    val working: StateFlow<Boolean> = _working.asStateFlow()

    val errorMessage = MutableStateFlow<String?>(null)

    /** Read the author's offered tiers + the viewer's status. The free "followers"
     * tier is excluded — it is followed via the header `profile-follow-button`. */
    fun load(authorIdHex: String) {
        viewModelScope.launch {
            errorMessage.value = null
            try {
                // A failed status read is non-fatal — every row just renders "not subscribed".
                _statusTier.value = runCatching { api.subscriptionStatus(authorIdHex).tier }.getOrNull()
                _offers.value = api.subscriptionOffersList(authorIdHex)
                    .filter { it.name != FOLLOWERS_TIER }
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            }
        }
    }

    /** Subscribe to [tier]; `Approved` re-reads (status flips to active), `Queued`
     * marks the tier pending (encrypted mode — the author confirms later). */
    fun subscribe(authorIdHex: String, tier: String) {
        viewModelScope.launch {
            errorMessage.value = null
            _working.value = true
            try {
                when (api.subscriptionSubscribe(authorIdHex, tier)) {
                    is FfiSubscribeReply.Approved -> {
                        _pendingTiers.value = _pendingTiers.value - tier
                        load(authorIdHex)
                    }
                    is FfiSubscribeReply.Queued ->
                        _pendingTiers.value = _pendingTiers.value + tier
                }
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            } finally {
                _working.value = false
            }
        }
    }

    companion object {
        /** The free "followers" tier — followed via the header `profile-follow-button`,
         * so it is not shown as a per-row paid offer (`profile.md` § Layout & flow). */
        const val FOLLOWERS_TIER = "followers"
    }
}
