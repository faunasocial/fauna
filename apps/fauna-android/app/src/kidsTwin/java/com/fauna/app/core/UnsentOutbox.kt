package com.fauna.app.core

import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.UnsentChange

/**
 * The device outbox's unsent-changes doors — **the excised half**, the kids
 * build types' twin of `src/noKids/…/UnsentOutbox.kt`. The kids `fauna-ffi`
 * flavor compiles the outbox's list, retry and discard out with `feed-manager`,
 * and the account page composes no unsent-changes section under
 * `BuildConfig.KIDS`, so nothing reaches these doors; each refuses by name
 * rather than answering as if the outbox were empty.
 */
object UnsentOutbox {
    /** Excised — see the file comment. */
    suspend fun unsentChanges(): List<UnsentChange> = throw excised()

    /** Excised — see the file comment. */
    @Suppress("UNUSED_PARAMETER")
    fun stateText(change: UnsentChange): LocalizedText = throw excised()

    /** Excised — see the file comment. */
    @Suppress("UNUSED_PARAMETER")
    fun reasonText(change: UnsentChange): LocalizedText? = throw excised()

    /** Excised — see the file comment. */
    @Suppress("UNUSED_PARAMETER")
    suspend fun retry(intentIdHex: String): Boolean = throw excised()

    /** Excised — see the file comment. */
    @Suppress("UNUSED_PARAMETER")
    suspend fun discard(intentIdHex: String): Boolean = throw excised()

    private fun excised() = IllegalStateException("the device outbox's unsent-changes list is not part of Fauna Kids")
}
