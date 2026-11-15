package com.fauna.app.payments

import com.fauna.app.core.ApiClient

/**
 * The sold post's *Change price…* glue — **the excised half**
 * (`dynamic-features.md` § The App-Store escape hatch). Compiled into the
 * build types with a feed and no payments (`storeSafe`, `storeNoPayments` —
 * the two that take `src/noKidsNoPayments/`) in place of the `src/payments/`
 * twin, and signature-for-signature identical to it. It cannot live in
 * `src/noPayments/`: the `kids` build type compiles that directory too, and
 * its bindings generate no `uniffi.fauna_feed`; every caller sits in
 * `src/noKids/`, so `kids` takes no twin at all (the `PostTipsGlue` posture).
 *
 * **Inert, not silently-dropped commands:** [SALE_PRICE_BUILT] is false and
 * [postOffersSalePrice] answers false, so the verb and its dialog are never
 * rendered and nothing can reach the other two. No runtime flag, hidden
 * setting or agent command re-enables them.
 */

/** Excised — this flavor never offers *Change price…*. */
const val SALE_PRICE_BUILT: Boolean = false

/** Excised — no post offers the verb in this flavor. */
@Suppress("UNUSED_PARAMETER")
fun postOffersSalePrice(post: uniffi.fauna_feed.PostSummary): Boolean = false

/** Excised — the dialog is not rendered in this flavor. */
@Suppress("UNUSED_PARAMETER")
suspend fun ApiClient.salePriceTierLoad(tierName: String): SalePriceTier =
    SalePriceTier(hint = "", asking = "", raw = null)

/** Excised — the dialog is not rendered in this flavor. */
@Suppress("UNUSED_PARAMETER")
suspend fun ApiClient.salePriceSave(tier: SalePriceTier, hint: String, asking: String): SalePriceSaveResult =
    SalePriceSaveResult.Closed
