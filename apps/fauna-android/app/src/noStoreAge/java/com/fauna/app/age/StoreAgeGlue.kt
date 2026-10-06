package com.fauna.app.age

import android.app.Activity

/**
 * The store-age arm's Play half — **the excised twin**, compiled into the
 * `foss` build type (the F-Droid / direct-download artifact,
 * `installers/android.md` § Release channels) in place of the real
 * `src/storeAge/` twin. Signature-for-signature identical to it and naming
 * **no `com.google.android.play` symbol at all**: a `foss` build declares no
 * Play dependency, so this file failing to compile against the real twin's
 * imports is the excision working (the `src/noPayments/` posture).
 *
 * Inert, not silently dropped: with no Play Store there is no age signal to
 * ask for, so the admission carries no claim and the nest records provenance
 * `none` — the honest partial coverage `family-safety.md` § The account age
 * band (D5) accepts. `integrityToken` is unreachable (a null range ends the
 * round) and says so if it ever is.
 */
object StoreAgeGlue : StoreAgeSignals {
    @Suppress("UNUSED_PARAMETER")
    override suspend fun ageRange(activity: Activity): StoreAgeRange? = null

    @Suppress("UNUSED_PARAMETER")
    override suspend fun integrityToken(activity: Activity, nonceB64Url: String): String =
        throw IllegalStateException("store-age attestation is excised from this build")
}
