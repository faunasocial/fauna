package com.fauna.app.age

import android.content.Context
import com.fauna.ffi.FfiStoreSignal

/**
 * The store-consent round's Play half — **the excised twin**, compiled into the
 * `foss` build type beside the inert [StoreAgeGlue], naming no
 * `com.google.android.play` symbol. With no Play Store there is no store
 * parent to consent: no signal, so the shared round answers `NoGate` and a
 * `foss` build never shows the lock — absent by data
 * (`account-age-band.md` § The account age band → the store-law developer
 * duties bullet, item (3)).
 */
object StoreConsentGlue : StoreConsentSignals {
    @Suppress("UNUSED_PARAMETER")
    override suspend fun playSignal(context: Context): FfiStoreSignal? = null
}
