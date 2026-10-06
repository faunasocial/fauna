package com.fauna.app.age

import android.app.Activity
import com.google.android.gms.tasks.Task
import com.google.android.play.agesignals.AgeSignalsAccessRequest
import com.google.android.play.agesignals.AgeSignalsManagerFactory
import com.google.android.play.agesignals.AgeSignalsRequest
import com.google.android.play.agesignals.model.AgeSignalsStatus
import com.google.android.play.core.integrity.IntegrityManagerFactory
import com.google.android.play.core.integrity.IntegrityTokenRequest
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

/**
 * The store-age arm's Play half — **the real twin**, compiled into the
 * Play-distributed build types (`debug`, `release`, `storeSafe`) in place of
 * the inert `src/noStoreAge/` twin the `foss` build type takes
 * (`installers/android.md` § Release channels; `family-safety.md` § The
 * account age band).
 *
 * This file is the complete list of android code that may name a
 * `com.google.android.play` symbol: the two client libraries are proprietary
 * Google IPC shims to the Play Store app, so they live only in the artifacts
 * Play itself distributes — an F-Droid or direct-download build carries no
 * Play code at all and simply makes no claim (its admissions record
 * provenance `none`, the honest partial coverage D5 accepts). What Google
 * sees: one explicit age-signals question and one integrity verdict, both at
 * admission time only, over a nonce that is a SHA-256 digest — never the band
 * or the actor id.
 */
object StoreAgeGlue : StoreAgeSignals {
    override suspend fun ageRange(activity: Activity): StoreAgeRange? {
        val manager = AgeSignalsManagerFactory.create(activity.applicationContext)
        // Step 1 — access. Play answers only for users whose account (or
        // whose parent, or whose region) shares the range; it may show its own
        // consent prompt, which is why the Activity is needed.
        val access = manager
            .requestAgeSignalsAccess(AgeSignalsAccessRequest.builder().setActivity(activity).build())
            .await()
        if (access.ageSignalsStatus() != AgeSignalsStatus.SHARED) return null
        // Step 2 — the range itself. Both bounds absent = nothing shared
        // (e.g. a verified adult sharing no range).
        val result = manager.checkAgeSignals(AgeSignalsRequest.builder().build()).await()
        val lower: Int? = result.ageLower()
        val upper: Int? = result.ageUpper()
        if (lower == null && upper == null) return null
        return StoreAgeRange(lower?.toUInt(), upper?.toUInt())
    }

    override suspend fun integrityToken(activity: Activity, nonceB64Url: String): String {
        // A CLASSIC request (the nest opens its verdict locally — the Play
        // Integrity plumbing ruling, family-safety.md). Play-distributed apps
        // need no cloud project number: the Console link supplies it.
        val manager = IntegrityManagerFactory.create(activity.applicationContext)
        val response = manager
            .requestIntegrityToken(IntegrityTokenRequest.builder().setNonce(nonceB64Url).build())
            .await()
        return response.token()
    }
}

/** Play's `Task` as a suspending call, without pulling the coroutines-play-services artifact. */
private suspend fun <T> Task<T>.await(): T = suspendCancellableCoroutine { cont ->
    addOnSuccessListener { cont.resume(it) }
    addOnFailureListener { cont.resumeWithException(it) }
    addOnCanceledListener { cont.cancel() }
}
