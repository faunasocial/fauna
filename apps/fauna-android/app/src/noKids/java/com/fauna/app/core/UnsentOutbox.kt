package com.fauna.app.core

import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.UnsentChange

/**
 * The device outbox's unsent-changes doors (`account-offline-mutation.md`
 * § The offline-mutation contract → *The unsent-changes surface*) — **the built
 * half**, compiled by every build type but the two kids ones. The free UniFFI
 * functions behind it ride `fauna-ffi`'s `feed-manager` gate, which the kids
 * flavor excises, so `src/main` reaches them only through this object; the
 * kids twin (`src/kidsTwin/`) refuses every door, and the account page does not
 * compose the section under `BuildConfig.KIDS` (the iOS twin's `#if !FAUNA_KIDS`
 * in `UnsentChangesSection.swift`).
 */
object UnsentOutbox {
    /** The undrained intents, in the shared model's order. */
    suspend fun unsentChanges(): List<UnsentChange> = com.fauna.ffi.unsentChanges()

    /** The shared state line of one row. */
    fun stateText(change: UnsentChange): LocalizedText = com.fauna.ffi.unsentChangeStateText(change)

    /** The shared reason of one row that says why it was not sent, else null. */
    fun reasonText(change: UnsentChange): LocalizedText? = com.fauna.ffi.unsentChangeReasonText(change)

    /** Re-arm a not-sent change through the outbox's own door. */
    suspend fun retry(intentIdHex: String): Boolean = com.fauna.ffi.retryUnsentChange(intentIdHex)

    /** Delete a not-sent change through the outbox's own door. */
    suspend fun discard(intentIdHex: String): Boolean = com.fauna.ffi.discardUnsentChange(intentIdHex)
}
