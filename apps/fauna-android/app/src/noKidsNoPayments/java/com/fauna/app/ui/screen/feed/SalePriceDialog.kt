package com.fauna.app.ui.screen.feed

import androidx.compose.runtime.Composable
import com.fauna.app.ui.viewmodel.FeedVM

/**
 * The sold post's *Change price...* render — **the excised half**
 * (`dynamic-features.md` § What "completely compiled away" means, item 1).
 * Compiled into the build types with a feed and no payments (`storeSafe`,
 * `storeNoPayments` — the two that take `src/noKidsNoPayments/`) in place of
 * the `src/payments/` twin, and signature-for-signature identical to it. It
 * paints nothing and names no `feed-post-sale-price-*` id, so the store
 * artifact's no-payments scan finds none; it cannot live in `src/noPayments/`
 * because `kids` compiles that directory and has no [FeedVM] (every caller
 * sits in `src/noKids/` — the `SalePriceGlue` posture).
 */

/** Excised — this flavor paints no *Change price...* entry. */
@Suppress("UNUSED_PARAMETER")
@Composable
internal fun SalePriceMenuItem(onChangePrice: (() -> Unit)?, closeMenu: () -> Unit) = Unit

/** Excised — this flavor paints no *Change price...* dialog. */
@Suppress("UNUSED_PARAMETER")
@Composable
internal fun SalePriceDialog(
    vm: FeedVM,
    tierName: String,
    advertised: Boolean,
    onDismiss: () -> Unit,
) = Unit
