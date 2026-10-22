package com.fauna.app.age

import android.content.Context
import com.fauna.ffi.FfiPlayChangeStatus
import com.fauna.ffi.FfiStoreConsentInput
import com.fauna.ffi.FfiStoreConsentLockState
import com.fauna.ffi.FfiStoreConsentRound
import com.fauna.ffi.FfiStoreConsentState
import com.fauna.ffi.FfiStoreConsentVerdict
import com.fauna.ffi.FfiStoreSignal
import com.fauna.ffi.FfiUtcDate
import java.time.LocalDate
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.mockito.Mockito.mock

/**
 * The android store-consent round's wiring (`account-age-band.md` § The account
 * age band → the store-law developer duties bullet, item (3)) on a plain JVM,
 * over its three seams. The verdict itself is the shared round's, pinned at
 * tier_1 in `fauna_client_family::store_consent`; what this pins is the glue's
 * duties — the stored record goes IN, the whole returned record is written
 * back after EVERY round (the first-run baseline included), a failed Play read
 * is no signal, and the date crosses as UTC — plus one call through the real
 * shared face to prove the bindings carry it.
 */
class StoreConsentRoundTest {
    private val context: Context = mock(Context::class.java)
    private val today = FfiUtcDate(2027u, 1u, 2u)

    private class MemoryStateStore(var state: FfiStoreConsentState = FfiStoreConsentState(null, null)) :
        StoreConsentStateStore {
        val saves = mutableListOf<FfiStoreConsentState>()
        override fun load() = state
        override fun save(state: FfiStoreConsentState) {
            saves += state
            this.state = state
        }
    }

    private fun round(verdict: FfiStoreConsentVerdict, persist: FfiStoreConsentState) =
        FfiStoreConsentRound(verdict = verdict, persist = persist, message = null)

    @Test
    fun the_stored_record_goes_in_and_the_whole_returned_record_comes_back() {
        val store = MemoryStateStore(FfiStoreConsentState(consentedThrough = 3u, lastRating = 4))
        val seen = mutableListOf<FfiStoreConsentInput>()
        val signal = FfiStoreSignal.Play(status = FfiPlayChangeStatus.APPROVED, approvalDate = null)
        StoreConsentRound.run(signal, store, today) {
            seen += it
            round(FfiStoreConsentVerdict.Clear, FfiStoreConsentState(consentedThrough = 5u, lastRating = 6))
        }
        assertEquals(listOf(FfiStoreConsentInput(signal, FfiStoreConsentState(3u, 4), today)), seen)
        assertEquals(listOf(FfiStoreConsentState(5u, 6)), store.saves)
    }

    @Test
    fun a_round_with_no_gate_still_writes_the_baseline() {
        val store = MemoryStateStore()
        StoreConsentRound.run(null, store, today) {
            round(FfiStoreConsentVerdict.NoGate, FfiStoreConsentState(consentedThrough = 0u, lastRating = null))
        }
        assertEquals(listOf(FfiStoreConsentState(0u, null)), store.saves)
    }

    @Test
    fun a_failing_play_read_is_no_signal() = runBlocking {
        val warnings = mutableListOf<String>()
        val signals = object : StoreConsentSignals {
            override suspend fun playSignal(context: Context): FfiStoreSignal? =
                throw IllegalStateException("no Play Store on this device")
        }
        assertNull(StoreConsentRound.readSignal(signals, context) { warnings += it })
        assertEquals(1, warnings.size)
    }

    @Test
    fun the_date_crosses_as_its_calendar_fields() {
        assertEquals(FfiUtcDate(2026u, 10u, 19u), StoreConsentRound.utcDate(LocalDate.of(2026, 10, 19)))
    }

    /** Through the real shared face: a Play PENDING locks, with no ask on android and the Play hint last. */
    @Test
    fun the_shared_face_locks_on_a_play_pending() {
        val store = MemoryStateStore()
        val r = StoreConsentRound.run(
            FfiStoreSignal.Play(status = FfiPlayChangeStatus.PENDING, approvalDate = null),
            store,
            today,
        )
        assertEquals(
            FfiStoreConsentVerdict.Locked(entries = emptyList(), state = FfiStoreConsentLockState.PENDING, canAsk = false),
            r.verdict,
        )
        assertEquals("family.store_consent_lock.play_hint", r.message!!.last().key)
        assertEquals(listOf(r.persist), store.saves)
    }
}
