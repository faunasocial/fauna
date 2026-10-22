package com.fauna.app.age

import android.content.Context
import com.fauna.ffi.FfiStoreConsentInput
import com.fauna.ffi.FfiStoreConsentRound
import com.fauna.ffi.FfiStoreConsentState
import com.fauna.ffi.FfiStoreSignal
import com.fauna.ffi.FfiUtcDate
import java.time.LocalDate
import java.time.ZoneOffset

/**
 * The android **store-consent round** (`account-age-band.md` § The account age
 * band → the store-law developer duties bullet, items (3) and (6)): at launch
 * and on every return to the foreground, read Play Age Signals'
 * `significantChangeStatus` / `significantChangeApprovalDate`, hand them with
 * today's UTC date and the device-local state to the one shared call
 * (`fauna_ffi::store_consent_round`), and write the state it returns back —
 * after EVERY round, since it carries the first-run baseline whatever the
 * signal says.
 *
 * **Nothing here decides anything.** Whether the person is covered, whether a
 * change is pending, and what the lock says are all the shared round's. This
 * glue is three seams — [StoreConsentSignals] (the Play half: the real
 * `src/storeAge` twin on the Play-distributed build types, the inert
 * `src/noStoreAge` twin on `foss`, which answers no signal and so never
 * gates), [StoreConsentStateStore] (the device-local record) and the round
 * itself — so it runs on a plain JVM (the FaunaKit `StoreAgeClaimRound`
 * shape).
 *
 * The record is about the person the store knows on this device, never the
 * Fauna account: it is not per-account, and signing out leaves it.
 */

/** The Play half of the round. */
interface StoreConsentSignals {
    /**
     * Play's significant-change answer for the person on this device, or null
     * when there is none to read (no Play, the read failed, or a `foss` build).
     * Null never gates. Asks nothing of the user: no access prompt is shown at
     * launch.
     */
    suspend fun playSignal(context: Context): FfiStoreSignal?
}

/** The device-local `{ consented_through, last_rating }` record, both null on first run. */
interface StoreConsentStateStore {
    fun load(): FfiStoreConsentState
    fun save(state: FfiStoreConsentState)
}

/** The record in the app's own device-local preferences. */
class PrefsStoreConsentStateStore(context: Context) : StoreConsentStateStore {
    private val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    override fun load(): FfiStoreConsentState = FfiStoreConsentState(
        consentedThrough = if (prefs.contains(KEY_THROUGH)) prefs.getLong(KEY_THROUGH, 0).toUInt() else null,
        lastRating = if (prefs.contains(KEY_RATING)) prefs.getLong(KEY_RATING, 0) else null,
    )

    override fun save(state: FfiStoreConsentState) {
        prefs.edit().apply {
            state.consentedThrough?.let { putLong(KEY_THROUGH, it.toLong()) } ?: remove(KEY_THROUGH)
            state.lastRating?.let { putLong(KEY_RATING, it) } ?: remove(KEY_RATING)
        }.apply()
    }

    private companion object {
        const val PREFS = "store_consent"
        const val KEY_THROUGH = "consented_through"
        const val KEY_RATING = "last_rating"
    }
}

object StoreConsentRound {
    /**
     * One round. [signal] is the store's answer already read (the caller reads
     * it through [StoreConsentSignals], or the debug agent stands in for it);
     * [round] is the shared call, a seam for the JVM test.
     */
    fun run(
        signal: FfiStoreSignal?,
        stateStore: StoreConsentStateStore,
        today: FfiUtcDate = todayUtc(),
        round: (FfiStoreConsentInput) -> FfiStoreConsentRound = { com.fauna.ffi.storeConsentRound(it) },
    ): FfiStoreConsentRound {
        val result = round(FfiStoreConsentInput(signal = signal, stored = stateStore.load(), today = today))
        stateStore.save(result.persist)
        return result
    }

    /** Read the Play half best-effort: a throwing read is no signal, which never gates. */
    suspend fun readSignal(
        signals: StoreConsentSignals,
        context: Context,
        onWarn: (String) -> Unit = {},
    ): FfiStoreSignal? = runCatching { signals.playSignal(context) }
        .getOrElse { e ->
            onWarn("store significant-change signal unavailable, no gate: $e")
            null
        }

    fun todayUtc(): FfiUtcDate = utcDate(LocalDate.now(ZoneOffset.UTC))

    fun utcDate(d: LocalDate): FfiUtcDate =
        FfiUtcDate(year = d.year.toUShort(), month = d.monthValue.toUByte(), day = d.dayOfMonth.toUByte())
}
