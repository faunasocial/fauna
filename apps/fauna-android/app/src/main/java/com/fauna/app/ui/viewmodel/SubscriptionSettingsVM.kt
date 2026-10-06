package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.payments.paymentsClaimsRedeem
import com.fauna.ffi.FfiMineSubscription
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * The consumer-side **`subscription-settings`** page state (subscriptions Slice
 * B — `monetization.md` § Pillar 1). Pure glue over the shared
 * `SubscriptionsClient.mine_list` / `unsubscribe` FFI (priority #2/#3, no logic
 * here); lifts the linux lead (`apps/fauna-linux/src/settings/subscriptions.rs`).
 *
 * Observer-free, like the profile Tiers tab: a manual re-read on mount and after
 * every unsubscribe (no client-side caching — `feed.md` § Architectural rules).
 * Unsubscribe in encrypted mode returns `Queued` (the row stays until the author
 * commits the removal), so the row does **not** vanish on click — the re-read
 * reflects the nest's state. Errors ride [errorMessage] to the page's global
 * banner (the Screen wraps it into `LocalAppMessages`).
 */
@HiltViewModel
class SubscriptionSettingsVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val _subscriptions = MutableStateFlow<List<FfiMineSubscription>>(emptyList())
    val subscriptions: StateFlow<List<FfiMineSubscription>> = _subscriptions.asStateFlow()

    /** True while a read/unsubscribe round-trip is in flight (disables controls). */
    private val _working = MutableStateFlow(false)
    val working: StateFlow<Boolean> = _working.asStateFlow()

    val errorMessage = MutableStateFlow<String?>(null)

    /** Re-read the caller's subscriptions (mount + on becoming visible). */
    fun refresh() {
        viewModelScope.launch {
            errorMessage.value = null
            try {
                _subscriptions.value = api.subscriptionMineList()
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            }
        }
    }

    /** Unsubscribe from [authorId], then re-read so the list reflects nest state. */
    fun unsubscribe(authorId: ByteArray) {
        viewModelScope.launch {
            errorMessage.value = null
            _working.value = true
            try {
                api.subscriptionUnsubscribe(authorId)
                _subscriptions.value = api.subscriptionMineList()
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            } finally {
                _working.value = false
            }
        }
    }

    /** True right after a successful redeem — clears the input field once. */
    val claimRedeemed = MutableStateFlow(false)

    /**
     * Redeem a post-payment claim code (`monetization.md` § Pillar 3 Q4 — the
     * universal fallback binding): binds the entitlement to this actor, then
     * re-reads so the queued grant renders exactly like a queued subscribe (a
     * "pending" row). Typed `fauna.payments.claim_*` errors ride
     * [errorMessage] to the global banner.
     */
    fun redeemClaim(code: String) {
        val trimmed = code.trim()
        if (trimmed.isEmpty()) return
        viewModelScope.launch {
            errorMessage.value = null
            _working.value = true
            try {
                api.paymentsClaimsRedeem(trimmed)
                claimRedeemed.value = true
                _subscriptions.value = api.subscriptionMineList()
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            } finally {
                _working.value = false
            }
        }
    }
}
