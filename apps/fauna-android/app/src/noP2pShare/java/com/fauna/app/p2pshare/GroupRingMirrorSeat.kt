package com.fauna.app.p2pshare

/**
 * The group-ring mirror seat — **the excised half** (`dynamic-features.md`
 * § Platform-family surface excision), compiled into `storeSafe` and `kids` in
 * place of the `src/p2pShare/` twin. Signature-identical to it and naming no
 * ring FFI symbol: those bindings export no `installGroupRingMirror`, and a
 * build with no ceremony holds no group set to mirror.
 */
object GroupRingMirrorSeat {
    /** Excised — no group set exists in this flavor, so there is no ring to mirror. */
    @Suppress("UNUSED_PARAMETER")
    fun install(actorId: ByteArray, stateDir: String) = Unit
}
