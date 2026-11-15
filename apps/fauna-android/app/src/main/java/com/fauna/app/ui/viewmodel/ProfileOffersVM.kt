package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.payments.ClaimRedeemResult
import com.fauna.app.payments.paymentsClaimsRedeemAcrossNests
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
 * (`SubscriptionsClient::offers_list_across_nests` with the page's actor-home
 * slot — the OTHER analogue of the bearer-keyed `tiers_list`, relayed to the
 * author's home when it is another nest; `subscriptions.md` § Pillar 1 → *The
 * offers browse across nests*) + the viewer's status (`status_get`, this nest);
 * [subscribe] subscribes to a per-row tier across nests at the same home, and
 * [payTo] is that row's payment link (subscribe first, then the checkout URL).
 * Pure glue over the shared FFI (priority #2/#3); lifts the linux
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

    /**
     * `subscription-claim-redeem-input` — a code of THIS creator, redeemed at the
     * page's actor home. A local buffer committed only on
     * `subscription-claim-redeem-button`, cleared only when the nest takes the
     * code (a refused paste stays to correct), and per open: a code typed for one
     * creator never rides to the next (lifts tui's `ProfileState::redeem_input`).
     */
    private val _redeemInput = MutableStateFlow("")
    val redeemInput: StateFlow<String> = _redeemInput.asStateFlow()

    /** The actor the buffer was typed for — a different one is a fresh open. */
    private var openedFor: String? = null

    /** The redeem call, a seam only so the VM tests need no live nest:
     *  `(code, authorIdHex, authorHomeUrl)`. */
    internal var redeem: suspend (String, String?, String?) -> ClaimRedeemResult =
        { code, author, home -> api.paymentsClaimsRedeemAcrossNests(code, author, home) }

    fun setRedeemInput(value: String) {
        _redeemInput.value = value
    }

    /** Bumped by every [load]: an answer whose load is no longer the latest (the
     * header read moved the home slot and a re-read started) is dropped. */
    private var loadSeq = 0

    /** Read the author's offered tiers from [home] (the page's actor-home slot,
     * `null` = this nest) + the viewer's status. The free "followers" tier is
     * excluded — it is followed via the header `profile-follow-button`. */
    fun load(authorIdHex: String, home: String?) {
        if (openedFor != authorIdHex) {
            openedFor = authorIdHex
            _redeemInput.value = ""
        }
        val seq = ++loadSeq
        viewModelScope.launch {
            errorMessage.value = null
            try {
                // A failed status read is non-fatal — every row just renders "not subscribed".
                val status = runCatching { api.subscriptionStatus(authorIdHex).tier }.getOrNull()
                val offers = api.subscriptionOffersList(authorIdHex, home)
                    .filter { it.name != FOLLOWERS_TIER }
                if (seq != loadSeq) return@launch
                _statusTier.value = status
                _offers.value = offers
            } catch (e: Exception) {
                if (seq != loadSeq) return@launch
                errorMessage.value = e.message ?: "error"
            }
        }
    }

    /** Subscribe to [tier] across nests at [home] — the page's actor-home slot,
     * the value the header follow button passes (`federated-follow.md`
     * § Decision 2; `null` = this nest). The reply lands on the row via
     * [applyReply]. */
    fun subscribe(authorIdHex: String, tier: String, home: String?) {
        viewModelScope.launch {
            errorMessage.value = null
            _working.value = true
            try {
                applyReply(authorIdHex, tier, home, api.subscriptionSubscribe(authorIdHex, tier, home))
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            } finally {
                _working.value = false
            }
        }
    }

    /**
     * The payment link (`subscription-offer-payment-link`): the subscribe to the
     * tier the row sells goes FIRST, across nests at [home], then [open]
     * receives the checkout URL the shared rule composes from [paymentUrl], so the
     * external checkout is bound to the buyer's actor (`monetization.md` § Pillar 3
     * → *Buyer↔actor binding (Q4)*, the Q13 rules (1) and (2); tui's
     * `Action::OpenPaymentLink`, linux `offers.rs::pay_to`). The reply lands on the
     * row exactly as the Subscribe button's does; `null` opens nothing; a refused
     * subscribe is the page error and opens nothing. The caller has already
     * refused an unsafe URL (nothing sent then) and passes the app's one opener
     * ([com.fauna.app.core.UrlOpener]) as [open].
     */
    fun payTo(
        authorIdHex: String,
        tier: String,
        paymentUrl: String,
        home: String?,
        open: (String) -> Unit,
    ) {
        viewModelScope.launch {
            errorMessage.value = null
            _working.value = true
            try {
                val checkout = api.subscriptionSubscribeAndCheckout(authorIdHex, tier, paymentUrl, home)
                if (checkout != null) {
                    applyReply(authorIdHex, tier, home, checkout.reply)
                    open(checkout.url)
                }
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            } finally {
                _working.value = false
            }
        }
    }

    /**
     * `subscription-claim-redeem-button` on the OTHER Tiers tab — redeem the typed
     * code of THIS creator at [authorHomeUrl], the page's actor-home slot exactly
     * as the follow button carries it (`PaymentsClient::claims_redeem_across_nests`;
     * `monetization.md` § Pillar 3 → *Which nest an in-app redeem binds on* →
     * *The doors*). A blank buffer is a no-op, never a nest round-trip. On success
     * the spent code leaves the input, offers + status are re-read, and a queued
     * grant marks its tier pending (the [subscribe] fold); a refusal (the typed
     * `fauna.payments.claim_*` / `peer_nest_outdated` errors) rides [errorMessage]
     * to the page's `error-message` and keeps the code to correct.
     */
    fun redeemClaim(authorIdHex: String, authorHomeUrl: String?) {
        val code = _redeemInput.value.trim()
        if (code.isEmpty()) return
        viewModelScope.launch {
            errorMessage.value = null
            _working.value = true
            try {
                val reply = redeem(code, authorIdHex, authorHomeUrl)
                _redeemInput.value = ""
                _pendingTiers.value = if (reply.queued) {
                    _pendingTiers.value + reply.tier
                } else {
                    _pendingTiers.value - reply.tier
                }
                load(authorIdHex, authorHomeUrl)
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            } finally {
                _working.value = false
            }
        }
    }

    /** A subscribe's reply on its row: `Approved` re-reads (status flips to
     * active), `Queued` marks the tier pending (encrypted mode — the author
     * confirms later). */
    private fun applyReply(authorIdHex: String, tier: String, home: String?, reply: FfiSubscribeReply) {
        when (reply) {
            is FfiSubscribeReply.Approved -> {
                _pendingTiers.value = _pendingTiers.value - tier
                load(authorIdHex, home)
            }
            is FfiSubscribeReply.Queued ->
                _pendingTiers.value = _pendingTiers.value + tier
        }
    }

    companion object {
        /** The free "followers" tier — followed via the header `profile-follow-button`,
         * so it is not shown as a per-row paid offer (`profile.md` § Layout & flow). */
        const val FOLLOWERS_TIER = "followers"
    }
}
