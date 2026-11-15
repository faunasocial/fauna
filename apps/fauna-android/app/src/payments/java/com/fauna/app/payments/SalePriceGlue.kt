package com.fauna.app.payments

import com.fauna.app.core.ApiClient

/**
 * The sold post's *Change price…* glue — **the built half**
 * (`monetization.md` § The asking price → *Re-pricing a sold post*). The money
 * plane's, so it rides the same `src/payments` / `src/noPayments` split as
 * [PaymentsGlue.kt]; the store-safe build types compile the twin in
 * `src/noPayments/` instead.
 *
 * Pure glue over the shared `FfiSubscriptionsClient.salePriceSave`, which runs
 * the shared plan and its verbs, so no app decides what to send.
 */

/** Whether this build offers *Change price…* at all. */
const val SALE_PRICE_BUILT: Boolean = true

/** `PostSummary::is_sold_post` — the verb's presence rule, shared with the
 *  Nostr advertisement verb. */
fun postOffersSalePrice(post: uniffi.fauna_feed.PostSummary): Boolean =
    uniffi.fauna_feed.postIsSoldPost(post)

/** The author's own selling tier by name (`tiers.list`), with the dialog's
 *  two fields pre-filled from it. Throws when the read fails or no tier has
 *  that name — the caller shows the load error. */
suspend fun ApiClient.salePriceTierLoad(tierName: String): SalePriceTier {
    val tier = subscriptionTiersList().firstOrNull { it.name == tierName }
        ?: throw IllegalStateException("no tier named $tierName")
    return SalePriceTier(
        hint = tier.priceHint ?: "",
        asking = tier.askingPriceSats?.toString() ?: "",
        raw = tier,
    )
}

/** Save the dialog's two fields through the shared plan. */
suspend fun ApiClient.salePriceSave(tier: SalePriceTier, hint: String, asking: String): SalePriceSaveResult {
    val stored = tier.raw as com.fauna.ffi.FfiTierItem
    return when (val r = nestRpc().subscriptions().salePriceSave(stored, hint, asking)) {
        is com.fauna.ffi.FfiSalePriceSave.Refused -> SalePriceSaveResult.Refused(r.i18nKey)
        is com.fauna.ffi.FfiSalePriceSave.Saved, com.fauna.ffi.FfiSalePriceSave.Unchanged -> SalePriceSaveResult.Closed
    }
}
