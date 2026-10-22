package com.fauna.app.age

import android.content.Context
import com.fauna.ffi.FfiPlayChangeStatus
import com.fauna.ffi.FfiStoreSignal
import com.google.android.play.agesignals.AgeSignalsManagerFactory
import com.google.android.play.agesignals.AgeSignalsRequest
import com.google.android.play.agesignals.model.SignificantChangeStatus
import java.time.ZoneOffset

/**
 * The store-consent round's Play half — **the real twin**, compiled into the
 * Play-distributed build types beside [StoreAgeGlue] (`account-age-band.md` §
 * The account age band → the store-law developer duties bullet, item (6)).
 * One `checkAgeSignals` read, no access request: the launch and foreground
 * re-check never shows the user a Play prompt, and a person Play shares
 * nothing about reads as no signal, which never gates.
 *
 * Play reports `significantChangeStatus` (`APPROVED` / `PENDING` /
 * `DECLINED`) and `significantChangeApprovalDate` (the effective-from date
 * the parent approved through, 00:00 UTC) — mapped here without judgement;
 * the shared round decides.
 */
object StoreConsentGlue : StoreConsentSignals {
    override suspend fun playSignal(context: Context): FfiStoreSignal? {
        val manager = AgeSignalsManagerFactory.create(context.applicationContext)
        val result = manager.checkAgeSignals(AgeSignalsRequest.builder().build()).await()
        val status = when (result.significantChangeStatus()) {
            SignificantChangeStatus.APPROVED -> FfiPlayChangeStatus.APPROVED
            SignificantChangeStatus.PENDING -> FfiPlayChangeStatus.PENDING
            SignificantChangeStatus.DECLINED -> FfiPlayChangeStatus.DECLINED
            else -> null
        }
        val approved = result.significantChangeApprovalDate()?.let {
            StoreConsentRound.utcDate(it.toInstant().atZone(ZoneOffset.UTC).toLocalDate())
        }
        return FfiStoreSignal.Play(status = status, approvalDate = approved)
    }
}
