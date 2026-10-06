package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.payments.ClaimItem
import com.fauna.app.payments.ProviderItem
import com.fauna.app.payments.paymentsClaimsList
import com.fauna.app.payments.paymentsClaimsMint
import com.fauna.app.payments.paymentsKnownKinds
import com.fauna.app.payments.paymentsProvidersList
import com.fauna.app.payments.paymentsProvidersRemove
import com.fauna.app.payments.paymentsProvidersSet
import com.fauna.app.payments.paymentsWebhookUrl
import com.fauna.ffi.FfiPendingRequest
import com.fauna.ffi.FfiSubscriberEntry
import com.fauna.ffi.FfiTierItem
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * The profile Tiers-tab **SELF** author-management state (subscriptions Slice A —
 * `monetization.md` § Pillar 1 + `profile.md`). Pure glue over the shared
 * `subscriptions_*` FFI (priority #2/#3, no logic here); lifts the linux lead
 * (`apps/fauna-linux/src/views/profile/tiers.rs`). Three sections:
 *
 * - **§1 My tiers** ([tiers]): `subscriptionTiersList`; create
 *   ([createTier], encrypted-mode mint), edit ([updateTier], thin), delete
 *   ([deleteTier]).
 * - **§2 Pending requests** ([requests]): `subscriptionRequestsList`; [approve]
 *   runs the transparent mint+upload (showing [approving]); [reject] is thin.
 * - **§3 Subscribers** ([subscribers]): the [selectedTier]'s roster; [remove]
 *   rotates + re-mints.
 * - **§4 Payment providers** ([providers] — `monetization.md` § Pillar 3):
 *   `paymentsProvidersList`; [setProvider] upserts (kind + webhook secret +
 *   entitled tier), [removeProvider] deletes. The nest owns validation.
 *   [webhookUrl] is a pure local passthrough (no round-trip) for the §4 form's
 *   live URL preview.
 * - **§5 Manual claim codes** ([claims]): `paymentsClaimsList`; [mintClaim]
 *   mints a `"manual"`-provider code for one tier (no expiry).
 *
 * Observer-free: a manual re-read after every mutation (no client-side caching —
 * `feed.md` § Architectural rules). Errors ride [errorMessage] to the page's
 * global banner (the Section wraps it into `LocalAppMessages`).
 */
@HiltViewModel
class ProfileTiersVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val _tiers = MutableStateFlow<List<FfiTierItem>>(emptyList())
    val tiers: StateFlow<List<FfiTierItem>> = _tiers.asStateFlow()

    private val _requests = MutableStateFlow<List<FfiPendingRequest>>(emptyList())
    val requests: StateFlow<List<FfiPendingRequest>> = _requests.asStateFlow()

    private val _subscribers = MutableStateFlow<List<FfiSubscriberEntry>>(emptyList())
    val subscribers: StateFlow<List<FfiSubscriberEntry>> = _subscribers.asStateFlow()

    // §4/§5 are typed on the app-owned seam rows, not the FFI records: a
    // store-safe build has no `FfiProviderItem`/`FfiClaimItem` at all, and this
    // shared view model must compile in that flavor
    // (`com.fauna.app.payments.PaymentsRows` owns the why). Both stay empty
    // there — the excised `paymentsProvidersList`/`paymentsClaimsList` twins
    // return `emptyList()`, and the sections that would render them are gated.
    private val _providers = MutableStateFlow<List<ProviderItem>>(emptyList())
    val providers: StateFlow<List<ProviderItem>> = _providers.asStateFlow()

    private val _claims = MutableStateFlow<List<ClaimItem>>(emptyList())
    val claims: StateFlow<List<ClaimItem>> = _claims.asStateFlow()

    /** Index into [tiers] selecting which tier's roster §3 shows (preserved by name). */
    private val _selectedTier = MutableStateFlow(0)
    val selectedTier: StateFlow<Int> = _selectedTier.asStateFlow()

    /** True while an approve mint+upload is in flight (`subscription-request-busy`). */
    private val _approving = MutableStateFlow(false)
    val approving: StateFlow<Boolean> = _approving.asStateFlow()

    /** True while any read/mutation round-trip is in flight (disables controls). */
    private val _working = MutableStateFlow(false)
    val working: StateFlow<Boolean> = _working.asStateFlow()

    val errorMessage = MutableStateFlow<String?>(null)

    /** Read §1 + §2, repopulate the §3 picker, read the selected tier's roster. */
    fun refreshAll() = mutate { /* read-only — reload() does the work */ }

    fun createTier(
        name: String,
        rank: UInt,
        description: String?,
        priceHint: String?,
        paymentUrl: String?,
        autoApprove: Boolean,
        askingPriceSats: ULong? = null,
    ) = mutate {
        api.subscriptionCreateTier(name, rank, description, priceHint, paymentUrl, autoApprove, askingPriceSats)
    }

    fun updateTier(
        name: String,
        rank: UInt?,
        description: String?,
        priceHint: String?,
        paymentUrl: String?,
        autoApprove: Boolean?,
        askingPriceSats: ULong? = null,
    ) = mutate {
        api.subscriptionUpdateTier(name, rank, description, priceHint, paymentUrl, autoApprove, askingPriceSats)
    }

    fun deleteTier(name: String) = mutate { api.subscriptionDeleteTier(name) }

    fun reject(requestId: Long) = mutate { api.subscriptionRejectRequest(requestId) }

    fun remove(tierName: String, subscriberId: ByteArray) =
        mutate { api.subscriptionRemoveSubscriber(tierName, subscriberId) }

    /** §4 — the registered provider kinds the form's kind select enumerates. */
    fun knownProviderKinds(): List<String> = api.paymentsKnownKinds()

    /** §4 add — upsert one provider config (nest-side validation; typed errors). */
    fun setProvider(kind: String, webhookSecret: String, tier: String) =
        mutate { api.paymentsProvidersSet(kind, webhookSecret, tier) }

    /** §4 remove. */
    fun removeProvider(kind: String) = mutate { api.paymentsProvidersRemove(kind) }

    /** §4 — the live webhook-URL preview for the form's current kind selection. */
    fun webhookUrl(kind: String): String = api.paymentsWebhookUrl(kind)

    /** §5 mint — a manual claim code for [tier]. */
    fun mintClaim(tier: String) = mutate { api.paymentsClaimsMint(tier) }

    /** §2 approve — special-cased: shows [approving] for the mint's duration. */
    fun approve(request: FfiPendingRequest) {
        viewModelScope.launch {
            errorMessage.value = null
            _approving.value = true
            _working.value = true
            try {
                api.subscriptionApproveRequest(request)
                reload()
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            } finally {
                _approving.value = false
                _working.value = false
            }
        }
    }

    /** §3 — switch the selected tier and re-read its roster. */
    fun selectTier(index: Int) {
        viewModelScope.launch {
            _selectedTier.value = index
            try {
                refreshRoster()
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            }
        }
    }

    /** Run a mutation (or a bare read), then re-read §1/§2/§3 and surface failures. */
    private fun mutate(block: suspend () -> Unit) {
        viewModelScope.launch {
            errorMessage.value = null
            _working.value = true
            try {
                block()
                reload()
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            } finally {
                _working.value = false
            }
        }
    }

    /** Read tiers + requests + providers + claims, preserve the §3 selection by
     *  name, read its roster. */
    private suspend fun reload() {
        val tiers = api.subscriptionTiersList()
        val requests = api.subscriptionRequestsList()
        _providers.value = api.paymentsProvidersList()
        _claims.value = api.paymentsClaimsList()
        val prevName = _tiers.value.getOrNull(_selectedTier.value)?.name
        _tiers.value = tiers
        _requests.value = requests
        val restored = tiers.indexOfFirst { it.name == prevName }.takeIf { it >= 0 } ?: 0
        _selectedTier.value = restored
        refreshRoster()
    }

    /** Re-read §3 for the currently-selected tier (empty when there are no tiers). */
    private suspend fun refreshRoster() {
        val name = _tiers.value.getOrNull(_selectedTier.value)?.name
        _subscribers.value = if (name == null) emptyList() else api.subscriptionSubscribersList(name)
    }
}
